//! Turns and threads of a conversation, built message by message.

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::hindsight::{Sym, Tracer, UKind};
use crate::learn::{correct_share, is_request, Featurizer, Head};
use crate::msg::py_str;
use crate::textops;

const PRIOR_B: f64 = -2.0;
const PRIOR_W: [f64; 13] = [7.0, 0.0, 0.0, 1.5, 4.0, 1.5, 0.6, -0.4, -6.0, 0.0, 6.0, 0.0, -1.0];
const CARD_ASK: usize = 160;
const CARD_ANSWER: usize = 200;
const CARD_ACTIONS: usize = 6;

pub struct Turn {
    pub start: usize,
    request: FxHashSet<Sym>,
    terms: FxHashSet<Sym>,
    seen: FxHashSet<Sym>,
    tokens: i64,
    pub thread: usize,
    pub units: Vec<usize>,
}

pub fn starts_turn(tr: &Tracer, fz: &Featurizer, mi: usize) -> bool {
    if mi == 0 {
        return true;
    }
    if tr.msgs[mi].get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    // the model gets two user messages in a row as one (the provider joins them): the second goes on
    // the first one's request, as when a host adds a line after the tool results of a step
    if tr.msgs[mi - 1].get("role").and_then(Value::as_str) == Some("user") {
        return false;
    }
    let us = fz.by_msg(mi);
    !us.iter().any(|&i| tr.units[i].kind == UKind::Result)
        && us.iter().any(|&i| tr.units[i].kind == UKind::User && is_request(&tr.unit_text(i)))
}

/// `" ".join(text.split())`, cut to ``n`` characters.
pub fn line(text: &str, n: usize) -> String {
    let x = textops::squash_ws(text);
    if textops::len(&x) <= n {
        x
    } else {
        format!("{}…", textops::head(&x, n - 1))
    }
}

#[derive(Default)]
pub struct Threads {
    pub link: f64,
    pub turns: Vec<Turn>,
    pub members: Vec<Vec<usize>>,
    pub last: Vec<usize>,
    m: usize,
    df: FxHashMap<Sym, u32>,
    n: usize,
    names: FxHashSet<Sym>,
}

impl Threads {
    pub fn update(&mut self, tr: &Tracer, fz: &Featurizer, head: Option<&Head>, share: Option<f64>) {
        while self.m < tr.msgs.len() {
            let mi = self.m;
            self.m += 1;
            let us = fz.by_msg(mi);
            if starts_turn(tr, fz, mi) {
                self.open(tr, fz, mi, head, share);
            }
            let Some(t) = self.turns.last_mut() else { continue };
            for &i in us {
                t.terms.extend(fz.terms[i].iter().copied());
                t.seen.extend(tr.units[i].words.iter().copied());
                t.tokens += tr.units[i].tokens;
                t.units.push(i);
                for &w in &fz.terms[i] {
                    *self.df.entry(w).or_insert(0) += 1;
                }
                self.names.extend(tr.units[i].words.iter().copied());
            }
            self.n += us.len();
        }
    }

    fn open(&mut self, tr: &Tracer, fz: &Featurizer, mi: usize, head: Option<&Head>, share: Option<f64>) {
        let mut req = FxHashSet::default();
        let mut names = FxHashSet::default();
        for &i in fz.by_msg(mi) {
            if tr.units[i].kind == UKind::User && is_request(&tr.unit_text(i)) {
                req.extend(fz.terms[i].iter().copied());
                names.extend(tr.units[i].words.iter().copied());
            }
        }
        let n = self.turns.len();
        let turn = |thread| Turn {
            start: mi,
            request: req.clone(),
            terms: FxHashSet::default(),
            seen: FxHashSet::default(),
            tokens: 0,
            thread,
            units: Vec::new(),
        };
        if n == 0 {
            self.turns.push(turn(0));
            self.members.push(vec![0]);
            self.last.push(0);
            return;
        }
        let weight: FxHashMap<Sym, f64> = req
            .iter()
            .map(|&w| (w, (1.0 + self.n as f64 / (1.0 + *self.df.get(&w).unwrap_or(&0) as f64)).ln()))
            .collect();
        let mut total: f64 = weight.values().sum();
        if total == 0.0 {
            total = 1.0;
        }
        let mut joint = vec![0.0f64; self.members.len()];
        let fresh = names.iter().filter(|w| !self.names.contains(w)).count();
        for j in 0..n {
            let t = &self.turns[j];
            let mut s = 0.0;
            let mut top: Option<f64> = None;
            for w in &req {
                if t.terms.contains(w) {
                    let x = weight[w];
                    s += x;
                    top = Some(top.map_or(x, |m: f64| m.max(x)));
                }
            }
            let asked: f64 = req.iter().filter(|w| t.request.contains(w)).map(|w| weight[w]).sum();
            let prev = if j == n - 1 { 1.0 } else { 0.0 };
            let x = [
                s / total,
                s.ln_1p(),
                top.unwrap_or(0.0),
                asked / total,
                if names.is_empty() {
                    0.0
                } else {
                    names.iter().filter(|w| t.seen.contains(w)).count() as f64 / names.len() as f64
                },
                prev,
                if t.thread == self.turns[n - 1].thread { 1.0 } else { 0.0 },
                ((n - 1 - j) as f64).ln_1p(),
                fresh as f64 / (names.len() + 2) as f64,
                (req.len() as f64).ln_1p(),
                prev / (1 + req.len()) as f64,
                (t.tokens as f64).ln_1p(),
                if j == 0 { 1.0 } else { 0.0 },
            ];
            let p = match head {
                Some(h) => correct_share(h.prob(&x), share.filter(|s| *s != 0.0).unwrap_or(0.5)),
                None => {
                    let z = PRIOR_B + PRIOR_W.iter().zip(&x).map(|(w, v)| w * v).sum::<f64>();
                    1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
                }
            };
            let k = t.thread;
            joint[k] = joint[k].max(p);
        }
        let mut best = 0;
        for k in 1..joint.len() {
            let better = joint[k] > joint[best] || (joint[k] == joint[best] && self.last[k] > self.last[best]);
            if better {
                best = k;
            }
        }
        // a request that rides on the results of a step stays on the topic of that step: the call and its
        // results are sent together or not at all, whatever topic the view keeps
        let rides = fz.by_msg(mi).iter().any(|&i| tr.units[i].kind == UKind::Result);
        let th = if rides {
            self.turns[n - 1].thread
        } else if joint[best] >= self.link {
            best
        } else {
            self.members.len()
        };
        if th == self.members.len() {
            self.members.push(Vec::new());
            self.last.push(n);
        }
        self.members[th].push(n);
        self.last[th] = n;
        self.turns.push(turn(th));
    }

    /// For measurement: each turn's first message and the thread the split gave it.
    pub fn assignment(&self) -> Vec<(usize, usize)> {
        self.turns.iter().map(|t| (t.start, t.thread)).collect()
    }

    pub fn active(&self) -> usize {
        self.turns.last().map_or(0, |t| t.thread)
    }

    pub fn span(&self, j: usize, nmsgs: usize) -> (usize, usize) {
        let end = self.turns.get(j + 1).map_or(nmsgs, |t| t.start);
        (self.turns[j].start, end)
    }

    pub fn ask(&self, tr: &Tracer, fz: &Featurizer, j: usize) -> String {
        let mut parts = Vec::new();
        for &i in fz.by_msg(self.turns[j].start) {
            if tr.units[i].kind == UKind::User {
                let t = tr.unit_text(i);
                if is_request(&t) {
                    parts.push(t);
                }
            }
        }
        line(&parts.join(" "), CARD_ASK)
    }

    /// The threads other than the active one, the most recently active first.
    pub fn others(&self) -> Vec<usize> {
        let a = self.active();
        let mut o: Vec<usize> = (0..self.members.len()).filter(|&k| k != a).collect();
        o.sort_by_key(|&k| std::cmp::Reverse(self.last[k]));
        o
    }

    pub fn card(&self, tr: &Tracer, fz: &Featurizer, k: usize, fixes: &mut dyn FnMut(usize) -> String) -> String {
        let ms = &self.members[k];
        let (Some(&j0), Some(&jn)) = (ms.first(), ms.last()) else { return String::new() };
        let mut asked = format!("\"{}\"", self.ask(tr, fz, j0));
        if ms.len() > 1 {
            if ms.len() > 2 {
                asked += &format!(", … ({} more), latest", ms.len() - 2);
            } else {
                asked += ", then";
            }
            asked += &format!(" \"{}\"", self.ask(tr, fz, jn));
        }
        let mut actions: Vec<String> = Vec::new();
        let mut changed: Vec<String> = Vec::new();
        let mut answer = String::new();
        for &j in ms {
            for &i in &self.turns[j].units {
                match tr.units[i].kind {
                    UKind::Call => {
                        actions.push(action(tr, i));
                        if let Some(info) = fz.info.get(&i) {
                            if let (crate::tools::Kind::Write, Some(p)) = (info.kind, &info.path) {
                                if !changed.contains(p) {
                                    changed.push(p.clone());
                                }
                            }
                        }
                    }
                    UKind::Text => answer = tr.unit_text(i).into_owned(),
                    _ => {}
                }
            }
        }
        let mut out =
            format!("• messages {}–{}: asked {}", self.turns[j0].start + 1, self.span(jn, tr.msgs.len()).1, asked);
        if !actions.is_empty() {
            let shown = &actions[actions.len().saturating_sub(CARD_ACTIONS)..];
            out += &format!(". Did ({}): {}", actions.len(), shown.join("; "));
            if actions.len() > shown.len() {
                out += "; …";
            }
        }
        let why: String = ms.iter().map(|&j| fixes(j)).collect();
        if !changed.is_empty() && why.is_empty() {
            out += &format!(". Changed: {}", changed[changed.len().saturating_sub(8)..].join(", "));
        }
        if !answer.is_empty() {
            out += &format!(". Last answer: \"{}\"", line(&answer, CARD_ANSWER));
        }
        out + "." + &why
    }
}

/// How many of `n` items `others[:limit]` keeps: a negative limit leaves out that many at the end.
pub fn head_slice(n: usize, limit: i64) -> usize {
    if limit >= 0 {
        (limit as usize).min(n)
    } else {
        n.saturating_sub(limit.unsigned_abs() as usize)
    }
}

pub fn action(tr: &Tracer, i: usize) -> String {
    let b = tr.block(i);
    let what = match b.get("input") {
        Some(Value::Object(args)) => ["file_path", "path", "command", "pattern", "url", "query", "description"]
            .iter()
            .find_map(|k| args.get(*k).and_then(Value::as_str))
            .unwrap_or(""),
        _ => "",
    };
    let name = match b.get("name") {
        None => "?".to_string(),
        Some(v) => py_str(v),
    };
    textops::strip(&format!("{} {}", name, line(what, 70))).to_string()
}
