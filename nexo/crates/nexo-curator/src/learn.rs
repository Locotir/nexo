//! Features of a chunk at a step and the learned heads that score them.

use std::cell::RefCell;
use std::sync::LazyLock;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::hindsight::{set_of, words, Interner, Set, Sym, Tracer, UKind};
use crate::mechanical::LINENO;
use crate::msg::{get_str, key_of};
use crate::py::{self, bisect_left, bisect_right, Re};
use crate::tools::{classify, Kind};

const DECAY: f64 = 0.5;
const ACTR_FLOOR: f64 = -4.0;
const MAX_MENTIONS: usize = 30;
const RECENT: usize = 2;
const MAX_CHUNK_LINES: usize = 30;
pub const RARE_IDF: f64 = 1.5;
/// What a word the current step does not ask about is worth, next to 1 for one it asks about (of the same
/// rarity): what tells nothing of the step is kept only for what is left of the room.
pub const UNASKED: f64 = 0.02;

static ERROR: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"(error|exception|traceback|failed|failure|fatal|panic|denied|not found)", py::I));
static TOKEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^\W_]+", 0));

static STOP: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    "the and for with that this from have has are was were not but you your can will would should could there their \
     they them then than what when where which while into onto about after before also just only some such each other \
     more most very here these those been being does did done its our out over under please now let use using need \
     needs want like make sure okay yes all any how why who \
     que los las del por con para una uno unos unas como pero más mas esto esta este eso esa ese hay muy son ser está \
     están estan fue han has hace hacer puede puedes tiene tienen todo toda todos todas cuando donde porque sobre entre \
     sin también tambien solo sólo nos les ella ellos algo bien así asi luego qué cómo mis tus sus"
        .split_whitespace()
        .collect()
});

fn hump_split(tok: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<char> = None;
    for (i, c) in tok.char_indices() {
        if let Some(p) = prev {
            if p.is_ascii_lowercase() && c.is_ascii_uppercase() {
                out.push(&tok[start..i]);
                start = i;
            }
        }
        prev = Some(c);
    }
    out.push(&tok[start..]);
    out
}

/// Topic words of a text (``learn.terms``).
pub fn terms(text: &str, int: &mut Interner) -> Set {
    let mut out = Vec::new();
    for tok in TOKEN.findall(text) {
        let parts = if py::str_islower(tok) { vec![tok] } else { hump_split(tok) };
        for part in parts {
            if py::len(part) < 3 || py::str_isdigit(part) {
                continue;
            }
            let low = py::lower(part);
            if !STOP.contains(low.as_ref()) {
                out.push(int.intern(py::head(&low, 5)));
            }
        }
    }
    out.extend(words(text, int));
    set_of(out)
}

const NOT_REQUEST: [&str; 5] = ["<system-reminder>", "<local-command", "<command-", "Caveat:", "[Request interrupted"];

/// User text written by the person.
pub fn is_request(text: &str) -> bool {
    !py::strip(text).is_empty() && !NOT_REQUEST.iter().any(|p| py::lstrip(text).starts_with(p))
}

pub fn split_paragraphs(text: &str) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        let blank = py::strip(&LINENO.sub_n(line, "", 1)).is_empty();
        let n = i + 1 - start;
        if (blank && n >= 4) || n >= MAX_CHUNK_LINES {
            out.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < lines.len() {
        if !out.is_empty() && lines.len() - start < 3 {
            let l = out.len() - 1;
            out[l].1 = lines.len();
        } else {
            out.push((start, lines.len()));
        }
    }
    out
}

#[derive(Clone)]
pub struct Chunk {
    pub start: i64,
    pub end: i64,
    pub words: Set,
    pub tokens: i64,
    pub position: f64,
    pub error: bool,
    pub terms: Set,
}

fn unit_chunks(tr: &Tracer, i: usize, int: &mut Interner) -> Vec<Chunk> {
    let u = &tr.units[i];
    let text = tr.unit_text(i);
    if u.kind != UKind::Result {
        return vec![Chunk {
            start: -1,
            end: -1,
            words: u.words.clone(),
            tokens: u.saving,
            position: 0.0,
            error: false,
            terms: terms(&text, int),
        }];
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let ranges = split_paragraphs(&text);
    let n = ranges.len();
    ranges
        .iter()
        .enumerate()
        .map(|(k, &(a, b))| {
            let part = lines[a..b].join("\n");
            Chunk {
                start: a as i64,
                end: b as i64,
                words: words(&part, int),
                tokens: (py::len(&part) / 4 + 1) as i64,
                position: k as f64 / (n.max(2) - 1) as f64,
                error: ERROR.is_match(&part),
                terms: terms(&part, int),
            }
        })
        .collect()
}

/// A call's or result's tool category and path, and whether the result is an error.
#[derive(Clone)]
pub struct Info {
    pub kind: Kind,
    pub path: Option<String>,
    pub error: bool,
}

pub struct Ctx {
    pub t: usize,
    recent: FxHashSet<Sym>,
    request: FxHashSet<Sym>,
    answer: FxHashSet<Sym>,
    n: f64,
    novelty: f64,
}

struct Act {
    t: usize,
    k: usize,
    base: FxHashMap<Sym, Option<f64>>,
    spread: FxHashMap<Sym, f64>,
}

/// A write that names a file, with the names written next to it.
type CoWrite = (usize, std::sync::Arc<Vec<Sym>>);

#[derive(Default)]
pub struct Featurizer {
    pub info: FxHashMap<usize, Info>,
    calls: FxHashMap<String, (Kind, Option<String>)>,
    pub chunks: Vec<Vec<Chunk>>,
    pub terms: Vec<Set>,
    pub by_msg: Vec<Vec<usize>>,
    pub requests: Vec<usize>,
    answers: Vec<usize>,
    novelty: FxHashMap<usize, f64>,
    echoed: FxHashMap<Sym, Vec<usize>>,
    mentions: FxHashMap<Sym, Vec<usize>>,
    cowrites: FxHashMap<Sym, Vec<CoWrite>>,
    amsg: Option<(usize, Vec<Sym>)>,
    reads: FxHashMap<String, Vec<usize>>,
    writes: FxHashMap<String, Vec<usize>>,
    seen: FxHashSet<Sym>,
    df: FxHashMap<Sym, u32>,
    df_pos: usize,
    pub ctx: Option<Ctx>,
    act: RefCell<Option<Act>>,
}

pub static NO_UNITS: Vec<usize> = Vec::new();

impl Featurizer {
    pub fn by_msg(&self, mi: usize) -> &[usize] {
        self.by_msg.get(mi).map_or(&NO_UNITS[..], |v| &v[..])
    }

    /// Takes in the units the trace gained; returns their range.
    pub fn extend(&mut self, tr: &Tracer, int: &mut Interner) -> std::ops::Range<usize> {
        let start = self.terms.len();
        while self.by_msg.len() < tr.msgs.len() {
            self.by_msg.push(Vec::new());
        }
        for i in start..tr.units.len() {
            let u = &tr.units[i];
            if matches!(u.kind, UKind::Call | UKind::Result) {
                let b = tr.block(i);
                if u.kind == UKind::Call {
                    let input = b.get("input").filter(|v| crate::msg::truthy(Some(v)));
                    let (kind, path, _) = classify(get_str(b, "name"), input);
                    let path = path.filter(|p| !p.is_empty());
                    self.info.insert(i, Info { kind, path: path.clone(), error: false });
                    self.calls.insert(key_of(b.get("id")), (kind, path.clone()));
                    if let (Some(p), Kind::Write) = (&path, kind) {
                        self.writes.entry(p.clone()).or_default().push(u.msg);
                    }
                } else if let Some((kind, path)) = self.calls.get(&key_of(b.get("tool_use_id"))).cloned() {
                    let error = crate::msg::truthy(b.get("is_error"));
                    if let (Some(p), Kind::Read) = (&path, kind) {
                        self.reads.entry(p.clone()).or_default().push(u.msg);
                    }
                    self.info.insert(i, Info { kind, path, error });
                }
            }
            let cs = unit_chunks(tr, i, int);
            let ts = if u.kind == UKind::Result {
                set_of(cs.iter().flat_map(|c| c.terms.iter().copied()).collect())
            } else {
                cs[0].terms.clone()
            };
            self.chunks.push(cs);
            self.by_msg[u.msg].push(i);
            if u.kind == UKind::User && is_request(&tr.unit_text(i)) && self.requests.last() != Some(&u.msg) {
                self.requests.push(u.msg);
                let new = ts.iter().filter(|w| !self.seen.contains(w)).count();
                self.novelty.insert(u.msg, new as f64 / ts.len().max(1) as f64);
            }
            if u.kind == UKind::Text && self.answers.last() != Some(&u.msg) {
                self.answers.push(u.msg);
            }
            if matches!(u.kind, UKind::Text | UKind::Call) {
                for &w in &ts {
                    let lst = self.echoed.entry(w).or_default();
                    if lst.last() != Some(&u.msg) {
                        lst.push(u.msg);
                    }
                }
                if self.amsg.as_ref().is_some_and(|a| a.0 != u.msg) {
                    self.flush(int);
                }
                let a = self.amsg.get_or_insert_with(|| (u.msg, Vec::new()));
                a.1.extend(u.words.iter().copied());
            }
            self.seen.extend(ts.iter().copied());
            self.terms.push(ts);
        }
        self.flush(int);
        start..tr.units.len()
    }

    fn flush(&mut self, int: &Interner) {
        let Some((msg, ws)) = self.amsg.take() else { return };
        let mut ws = set_of(ws);
        ws.sort_by(|a, b| int.get(*a).cmp(int.get(*b)));
        ws.truncate(60);
        let shared = std::sync::Arc::new(ws.clone());
        for &w in &ws {
            self.mentions.entry(w).or_default().push(msg);
            if ws.len() > 1 {
                self.cowrites.entry(w).or_default().push((msg, shared.clone()));
            }
        }
    }

    fn terms_at(&self, tr: &Tracer, msg: usize, kind: UKind) -> FxHashSet<Sym> {
        let mut out = FxHashSet::default();
        for &i in self.by_msg(msg) {
            if tr.units[i].kind == kind {
                out.extend(self.terms[i].iter().copied());
            }
        }
        out
    }

    /// The context of step ``t`` (kept until another step is asked).
    pub fn context(&mut self, tr: &Tracer, t: usize) {
        if self.ctx.as_ref().is_some_and(|c| c.t == t) {
            return;
        }
        let units = &tr.units;
        if self.df_pos > 0 && units[self.df_pos - 1].msg >= t {
            self.df.clear();
            self.df_pos = 0;
        }
        while self.df_pos < units.len() && units[self.df_pos].msg < t {
            for &w in &self.terms[self.df_pos] {
                *self.df.entry(w).or_insert(0) += 1;
            }
            self.df_pos += 1;
        }
        let mut recent = FxHashSet::default();
        for m in t.saturating_sub(RECENT)..t {
            for &i in self.by_msg(m) {
                recent.extend(self.terms[i].iter().copied());
            }
        }
        let k = bisect_left(&self.requests, &t);
        let req = if k > 0 { Some(self.requests[k - 1]) } else { None };
        let request = req.map(|r| self.terms_at(tr, r, UKind::User)).unwrap_or_default();
        let k = bisect_left(&self.answers, &t);
        let answer = if k > 0 { self.terms_at(tr, self.answers[k - 1], UKind::Text) } else { FxHashSet::default() };
        let novelty = req.and_then(|r| self.novelty.get(&r).copied()).unwrap_or(0.0);
        self.ctx = Some(Ctx { t, recent, request, answer, n: self.df_pos.max(1) as f64, novelty });
    }

    /// What knowing ``w`` is worth at the step of the current context: its rarity, times 1 if the request,
    /// the last reply or the latest messages name it and [`UNASKED`] if not.
    pub fn worth(&self, w: Sym) -> f64 {
        let ctx = self.ctx.as_ref().expect("context before worth");
        let need = ctx.request.contains(&w) || ctx.recent.contains(&w) || ctx.answer.contains(&w);
        self.idf(ctx, w) * if need { 1.0 } else { UNASKED }
    }

    fn idf(&self, ctx: &Ctx, w: Sym) -> f64 {
        (1.0 + ctx.n / (1.0 + *self.df.get(&w).unwrap_or(&0) as f64)).ln()
    }

    fn steps_between(tr: &Tracer, a: i64, b: usize) -> usize {
        let hi = bisect_left(&tr.steps, &b);
        let lo = if a < 0 { 0 } else { bisect_right(&tr.steps, &(a as usize)) };
        hi.saturating_sub(lo)
    }

    fn echo(&self, tr: &Tracer, c: &Chunk, ctx: &Ctx, created: usize) -> (f64, i64) {
        let (mut strength, mut last) = (0.0, -1i64);
        for &w in &c.terms {
            let Some(ms) = self.echoed.get(&w) else { continue };
            let k = bisect_left(ms, &ctx.t);
            if k > 0 && ms[k - 1] > created {
                let r = self.idf(ctx, w);
                if r >= RARE_IDF {
                    strength += r;
                    last = last.max(ms[k - 1] as i64);
                }
            }
        }
        let _ = tr;
        (strength, last)
    }

    /// The feature vector of chunk ``j`` of unit ``i`` in the current context.
    pub fn vector(&self, tr: &Tracer, i: usize, j: usize) -> Vec<f64> {
        let ctx = self.ctx.as_ref().expect("context before vector");
        let c = &self.chunks[i][j];
        let t = ctx.t;
        let u = &tr.units[i];
        let info = self.info.get(&i);
        let cat = info.map(|x| x.kind);
        let path = info.and_then(|x| x.path.as_ref());
        let err = info.is_some_and(|x| x.error);
        let created = Self::steps_between(tr, u.msg as i64, t);
        let npast = bisect_left(&u.uses, &t);
        let since = if npast > 0 { Self::steps_between(tr, u.uses[npast - 1] as i64, t) } else { created };
        let between = |v: Option<&Vec<usize>>| v.is_some_and(|v| v.iter().any(|&w| u.msg < w && w < t));
        let stale = u.kind == UKind::Result && path.is_some_and(|p| between(self.writes.get(p)));
        let superseded = u.kind == UKind::Result && path.is_some_and(|p| between(self.reads.get(p)));
        let used =
            if c.start < 0 { npast > 0 } else { c.words.iter().any(|w| u.word_first.get(w).copied().unwrap_or(t) < t) };
        let sum_in =
            |s: &FxHashSet<Sym>| -> f64 { c.terms.iter().filter(|w| s.contains(w)).map(|&w| self.idf(ctx, w)).sum() };
        let rarest = c
            .terms
            .iter()
            .filter(|w| ctx.request.contains(w) || ctx.recent.contains(w) || ctx.answer.contains(w))
            .map(|&w| self.idf(ctx, w))
            .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
            .unwrap_or(0.0);
        let (strength, last) = self.echo(tr, c, ctx, u.msg);
        let since_echo = if last >= 0 { Self::steps_between(tr, last, t) } else { created };
        let reqs_since = bisect_left(&self.requests, &t) as i64 - bisect_right(&self.requests, &u.msg) as i64;
        let f = |b: bool| if b { 1.0 } else { 0.0 };
        let l1 = |x: f64| x.ln_1p();
        let (actr, assoc) = self.memory(tr, c, t);
        vec![
            f(u.kind == UKind::Result),
            f(u.kind == UKind::Call),
            f(u.kind == UKind::Thinking),
            f(cat == Some(Kind::Read)),
            f(cat == Some(Kind::Write)),
            f(cat == Some(Kind::Exec)),
            f(cat == Some(Kind::Search)),
            f(!matches!(cat, Some(Kind::Read | Kind::Write | Kind::Exec | Kind::Search))),
            f(err),
            l1(u.tokens as f64),
            l1(created as f64),
            l1(npast as f64),
            l1(since as f64),
            f(stale),
            f(superseded),
            l1(reqs_since as f64),
            l1(if c.start >= 0 { (c.end - c.start) as f64 } else { 0.0 }),
            l1(c.words.len() as f64),
            c.position,
            c.tokens as f64 / u.tokens.max(1) as f64,
            f(used),
            f(c.error),
            l1(sum_in(&ctx.request)),
            l1(sum_in(&ctx.recent)),
            l1(sum_in(&ctx.answer)),
            rarest,
            l1(strength),
            l1(since_echo as f64),
            ctx.novelty,
            actr,
            assoc,
        ]
    }

    fn memory(&self, tr: &Tracer, c: &Chunk, t: usize) -> (f64, f64) {
        let mut act = self.act.borrow_mut();
        if act.as_ref().map_or(true, |a| a.t != t) {
            *act = Some(self.activation(tr, t));
        }
        let Some(a) = act.as_mut() else { return (ACTR_FLOOR, 0.0) };
        let mut best: Option<f64> = None;
        let mut spread: Option<f64> = None;
        for &w in &c.words {
            let k = a.k;
            let v = *a.base.entry(w).or_insert_with(|| self.base(tr, w, t, k));
            if let Some(v) = v {
                best = Some(best.map_or(v, |b| b.max(v)));
            }
            if let Some(&s) = a.spread.get(&w) {
                spread = Some(spread.map_or(s, |b| b.max(s)));
            }
        }
        (best.unwrap_or(ACTR_FLOOR), spread.unwrap_or(0.0))
    }

    fn activation(&self, tr: &Tracer, t: usize) -> Act {
        let k = bisect_left(&tr.steps, &t);
        let mut cues: FxHashSet<Sym> = FxHashSet::default();
        if k > 0 {
            for &i in self.by_msg(tr.steps[k - 1]) {
                if matches!(tr.units[i].kind, UKind::Text | UKind::Call) {
                    cues.extend(tr.units[i].words.iter().copied());
                }
            }
        }
        let kr = bisect_left(&self.requests, &t);
        if kr > 0 {
            for &i in self.by_msg(self.requests[kr - 1]) {
                cues.extend(tr.units[i].words.iter().copied());
            }
        }
        let mut spread: FxHashMap<Sym, f64> = FxHashMap::default();
        for c in cues {
            let Some(ev) = self.cowrites.get(&c) else { continue };
            let j = ev.partition_point(|e| e.0 < t);
            if j == 0 {
                continue;
            }
            let share = 1.0 / j as f64;
            for (_, ws) in &ev[j.saturating_sub(MAX_MENTIONS)..j] {
                for &w in ws.iter() {
                    if w != c {
                        *spread.entry(w).or_insert(0.0) += share;
                    }
                }
            }
        }
        Act { t, k, base: FxHashMap::default(), spread }
    }

    fn base(&self, tr: &Tracer, w: Sym, t: usize, k: usize) -> Option<f64> {
        let ms = self.mentions.get(&w)?;
        let j = bisect_left(ms, &t);
        if j == 0 {
            return None;
        }
        let s: f64 = ms[j.saturating_sub(MAX_MENTIONS)..j]
            .iter()
            .map(|m| (k as f64 - bisect_left(&tr.steps, m) as f64 + 0.5).powf(-DECAY))
            .sum();
        Some(s.ln())
    }
}

/// Additive logistic model over binned features.
pub struct Head {
    edges: Vec<Vec<f64>>,
    w: Vec<Vec<f64>>,
    b: f64,
}

impl Head {
    fn from(v: &Value) -> Option<Head> {
        let vecs = |k: &str| -> Option<Vec<Vec<f64>>> {
            v.get(k)?.as_array()?.iter().map(|e| e.as_array()?.iter().map(Value::as_f64).collect()).collect()
        };
        Some(Head { edges: vecs("edges")?, w: vecs("w")?, b: v.get("b")?.as_f64()? })
    }

    pub fn prob(&self, x: &[f64]) -> f64 {
        let mut z = self.b;
        for ((e, wf), &v) in self.edges.iter().zip(&self.w).zip(x) {
            let k = e.partition_point(|&a| a <= v);
            z += wf.get(k).copied().unwrap_or(0.0);
        }
        1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
    }
}

pub struct Model {
    pub keep: Head,
    pub turn: Option<Head>,
    pub link: Option<Head>,
    turn_share: f64,
    pub link_share: Option<f64>,
}

fn correct(p: f64, pi: f64) -> f64 {
    p * pi / (p * pi + (1.0 - p) * (1.0 - pi))
}

impl Model {
    pub fn prob(&self, x: &[f64]) -> f64 {
        self.keep.prob(x)
    }

    pub fn turn_prob(&self, x: &[f64]) -> f64 {
        match &self.turn {
            Some(h) => correct(h.prob(x), self.turn_share),
            None => self.prob(x),
        }
    }
}

/// The model shipped with the crate.
pub static MODEL: LazyLock<Model> = LazyLock::new(|| {
    let d: Value = serde_json::from_str(crate::MODEL_JSON).expect("bundled model");
    let on = d.get("trained_on");
    let share = |k: &str| on.and_then(|o| o.get(k)).and_then(Value::as_f64);
    let head = |k: &str| d.get(k).and_then(Head::from);
    Model {
        keep: head("keep").expect("bundled model has a keep head"),
        turn: head("turn"),
        link: head("link"),
        turn_share: share("turn_share").unwrap_or(0.5),
        link_share: share("link_share"),
    }
});

pub fn correct_share(p: f64, pi: f64) -> f64 {
    correct(p, pi)
}
