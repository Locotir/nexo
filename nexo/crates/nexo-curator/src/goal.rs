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
    /// What the request that opened the turn asked, as a card names it (`Threads::ask`).
    ask: String,
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
    /// For each thread, how likely the latest request belongs to it (the classifier's joint); 0 when the
    /// request rides on a step or the thread is new.
    pub joint: Vec<f64>,
    /// Topic changes so far, and those that went back to a topic seen before (`switches`).
    sw: usize,
    back: usize,
    m: usize,
    df: FxHashMap<Sym, u32>,
    n: usize,
    names: FxHashSet<Sym>,
    /// The closed turns each word is in, by what the turn said (`terms`), asked (`request`) and held (`seen`),
    /// the turns in order: a new request is scored against these, not against every turn for each word.
    in_terms: FxHashMap<Sym, Vec<u32>>,
    in_request: FxHashMap<Sym, Vec<u32>>,
    in_seen: FxHashMap<Sym, Vec<u32>>,
    /// How many turns are in those indexes.
    indexed: usize,
}

/// What a new request shares with one turn.
#[derive(Clone, Default)]
struct Overlap {
    terms: f64,
    top: Option<f64>,
    asked: Option<f64>,
    seen: usize,
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

    /// Moves the words of closed turn `j` into the indexes (a closed turn takes no more).
    fn index_turn(&mut self, j: usize) {
        let t = &mut self.turns[j];
        let at = u32::try_from(j).expect("fewer than 2^32 turns");
        for w in std::mem::take(&mut t.terms) {
            self.in_terms.entry(w).or_default().push(at);
        }
        for w in std::mem::take(&mut t.request) {
            self.in_request.entry(w).or_default().push(at);
        }
        for w in std::mem::take(&mut t.seen) {
            self.in_seen.entry(w).or_default().push(at);
        }
        self.indexed = j + 1;
    }

    fn open(&mut self, tr: &Tracer, fz: &Featurizer, mi: usize, head: Option<&Head>, share: Option<f64>) {
        let mut req = FxHashSet::default();
        let mut names = FxHashSet::default();
        let mut asked = Vec::new();
        for &i in fz.by_msg(mi) {
            if tr.units[i].kind == UKind::User {
                let text = tr.unit_text(i);
                if is_request(&text) {
                    req.extend(fz.terms[i].iter().copied());
                    names.extend(tr.units[i].words.iter().copied());
                    asked.push(text);
                }
            }
        }
        let ask = line(&asked.join(" "), CARD_ASK);
        let n = self.turns.len();
        let turn = |thread| Turn {
            start: mi,
            request: req.clone(),
            terms: FxHashSet::default(),
            seen: FxHashSet::default(),
            tokens: 0,
            thread,
            units: Vec::new(),
            ask: ask.clone(),
        };
        if n == 0 {
            self.turns.push(turn(0));
            self.members.push(vec![0]);
            self.last.push(0);
            self.joint = vec![0.0];
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
        // the turns before this request are closed: their words go to the indexes, and the request is scored
        // against the turns it shares a word with; the others have nothing in common with it
        while self.indexed < n {
            self.index_turn(self.indexed);
        }
        // the sum of no weights, as a turn that shares no word asked for it
        let none: f64 = std::iter::empty::<f64>().sum();
        let mut shared = vec![Overlap::default(); n];
        for w in &req {
            let x = weight[w];
            for &j in self.in_terms.get(w).into_iter().flatten() {
                let o = &mut shared[j as usize];
                o.terms += x;
                o.top = Some(o.top.map_or(x, |m: f64| m.max(x)));
            }
            for &j in self.in_request.get(w).into_iter().flatten() {
                let o = &mut shared[j as usize];
                o.asked = Some(o.asked.unwrap_or(none) + x);
            }
        }
        for w in &names {
            for &j in self.in_seen.get(w).into_iter().flatten() {
                shared[j as usize].seen += 1;
            }
        }
        for (j, (t, o)) in self.turns.iter().zip(&shared).enumerate() {
            let (s, top, asked) = (o.terms, o.top, o.asked.unwrap_or(none));
            let prev = if j == n - 1 { 1.0 } else { 0.0 };
            let x = [
                s / total,
                s.ln_1p(),
                top.unwrap_or(0.0),
                asked / total,
                if names.is_empty() { 0.0 } else { o.seen as f64 / names.len() as f64 },
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
        let fresh_thread = th == self.members.len();
        if th != self.turns[n - 1].thread {
            self.sw += 1;
            self.back += usize::from(!fresh_thread);
        }
        if th == self.members.len() {
            self.members.push(Vec::new());
            self.last.push(n);
        }
        self.members[th].push(n);
        self.last[th] = n;
        self.joint = if rides { Vec::new() } else { joint };
        self.joint.resize(self.members.len(), 0.0);
        self.turns.push(turn(th));
    }

    /// For measurement: each turn's first message and the thread the split gave it.
    pub fn assignment(&self) -> Vec<(usize, usize)> {
        self.turns.iter().map(|t| (t.start, t.thread)).collect()
    }

    /// The thread of the turn that holds message `mi`.
    pub fn thread_at(&self, mi: usize) -> usize {
        let i = self.turns.partition_point(|t| t.start <= mi);
        i.checked_sub(1).map_or(0, |i| self.turns[i].thread)
    }

    /// How many times the person changed topic so far, and how many of those went back to a topic seen before.
    pub fn switches(&self) -> (usize, usize) {
        (self.sw, self.back)
    }

    pub fn active(&self) -> usize {
        self.turns.last().map_or(0, |t| t.thread)
    }

    pub fn span(&self, j: usize, nmsgs: usize) -> (usize, usize) {
        let end = self.turns.get(j + 1).map_or(nmsgs, |t| t.start);
        (self.turns[j].start, end)
    }

    /// What the request that opened turn `j` asked, as a card names it.
    pub fn ask(&self, _tr: &Tracer, _fz: &Featurizer, j: usize) -> String {
        self.turns[j].ask.clone()
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
