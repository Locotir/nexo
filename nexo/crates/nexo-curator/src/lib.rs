//! The context curator: decides, call by call, which parts of a conversation the model sees.
//!
//! [`Curator`] is the goal engine run on the live path: it takes the whole conversation on every
//! call, keeps what it learnt of the earlier messages and returns the view to send, the tokens of that view and what it did.

#[cfg(feature = "sim")]
pub mod sim;

mod engine;
mod evidence;
mod flat;
mod goal;
mod hindsight;
#[cfg(test)]
mod hindsight_labels;
mod json;
mod learn;
mod ledger;
mod mechanical;
mod msg;
mod rules;
mod standing;
mod textops;
mod tools;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

pub use engine::{keywords, label, Added, AddedKind, AddedState};
use engine::{Engine, Params};
use mechanical::MARK;

/// Names any model knows without reading them in the conversation, one per line.
pub const GENERAL_WORDS: &str = include_str!("../data/general.txt");
/// The curator's learnt model (JSON), bundled into the binary.
pub const MODEL_JSON: &str = include_str!("../data/model.json");
/// The names of the features the need heads see, in the order `Curator::dump_to` writes them, and how many
/// of them the older heads see (for fits outside the curator).
pub use learn::{K_OLD, NAMES as NEED_FEATURES};

/// The settings every spec starts from (what is given replaces them). The view is pure (no directives in the
/// system prompt, no requests for notes): an answered request keeps only its reply, outputs nothing drew on go
/// idle, repeated lines and repeated calls go once, and what the agent has not yet written about, or read of a
/// file the request names, stays. Measured on recorded checkpoints of the golden session: as many answers as
/// the whole history (+0.68 of 13) at 62 % of what it sends. A cut (idle steps, unused outputs, the cap)
/// is made only when what it saves is 2.25 times what rewriting the view behind it costs (`margin=2.25`): a cut
/// that guesses the future wrong pays the rewrite and, if the step comes back, its price without the cache.
/// While the thread works through the files its first message lists, what was read of one no request has
/// taken up yet stays (`mission=1`): the task named it, and cutting it would only bring it back without
/// the cache. An output an earlier call gave, line for line, goes as one line that points to it (and says
/// nothing changed when the same call gave it; `asked=3`): it is decided as it arrives, so it breaks no cache, and the copy above stays whole.
/// Every step stays in the history, a call made over and over too: a view of the same shape as
/// one sent before (call ids and the counts in curator lines aside) is never sent again, since at a fixed
/// temperature the model takes the same step again. Folding such a run kept the agent in it for 22 steps
/// on the golden session. The output the agent has just asked for comes whole (`fresh=1`); its repeated runs go
/// as pointers from the next request on, so no pointer sends it looking for the lines. An output the agent
/// has not written about yet (no text and no note since) leaves only as an exact copy of a later one, never
/// on a guess, for its latest 32000 tokens (`still=32000`): a new request does not mean the agent is done with
/// what it was reading. Without it, the golden session lost the reads of a request left unanswered as the
/// next one came in, and the agent read the same file again for 180 steps. Past the budget a guess may take
/// the oldest, so a session where the agent never writes still has a bounded view.
/// The working window: the view aims at `target=10000` tokens (the priced cap above runs with that budget), is
/// cut whatever the cache costs once it passes twice the target (or `cap=40000` when that comes first) down to
/// the target, in one rewrite (the oldest results that are not sticky leave first), and never
/// passes `hard=80000` (then the oldest sticky results leave too, as pointers). Sticky, by rule and with no
/// model: the request, the agent's own text and calls, the latest `file now` of each path, the last `check:`,
/// the latest whole read of each path and what the agent has read and not yet written about. `still` is
/// bounded by `target` minus the system prompt and the sticky tokens (at least 4000; flat with no `cap`).
pub const D_SPEC: &str =
    "threshold=0.02;head=turn;recall=1;link=0.5;rule=rent;birth=squeeze;eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;protect=12;directives=0;nudge=0;close=2;ahead=0.25;margin=2.25;dedup=3;live=1;lead=1;aim=1;follow=1;brief=1;digest=1;hold=1;asked=3;fresh=1;still=32000;sup=1;named=16000;mission=1;apart=8000;check=4000;every=4;ratio=0.1;least=0.0005;floor=0.6;cap=40000;target=10000;hard=80000;cut=age;cards=8;stale=1;pockets=0;life=3600";

const INTS: &[&str] = &[
    "protect",
    "check",
    "cap",
    "target",
    "hard",
    "every",
    "cards",
    "eoq",
    "ledger",
    "gwin",
    "jit",
    "notes",
    "rules",
    "pin",
    "nudge",
    "directives",
    "recall",
    "settle",
    "dedup",
    "close",
    "sup",
    "topic",
    "apart",
    "noted",
    "named",
    "mission",
    "sticky",
    "live",
    "lead",
    "aim",
    "follow",
    "brief",
    "digest",
    "hold",
    "still",
    "loops",
    "asked",
    "fresh",
    "recite",
    "task",
    "reruns",
    "stall",
    "stale",
    "pockets",
];
const STRS: &[&str] = &["rule", "birth", "stub", "head", "leave", "spill", "outcome", "cut"];
const FLOATS: &[&str] =
    &["threshold", "ratio", "margin", "floor", "least", "link", "early", "trust", "ttl", "lerr", "ahead", "life"];

#[derive(Clone, Debug, PartialEq)]
enum Val {
    I(i64),
    F(f64),
    S(String),
}

/// The `key=value` pairs of a spec, in order. Only `;` separates them: a value may hold a comma.
fn parse_params(spec: &str) -> Result<IndexMap<String, String>, String> {
    let mut out = IndexMap::new();
    for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once('=') else {
            return Err(format!("{part:?} is not key=value"));
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(out)
}

/// A value of the type its key takes; a number is never negative and always finite.
fn typed(k: &str, v: &str) -> Result<Val, String> {
    if STRS.contains(&k) {
        return Ok(Val::S(v.to_string()));
    }
    let digits = v.replace('_', "");
    if INTS.contains(&k) {
        match digits.parse::<i64>() {
            Ok(x) if x >= 0 => Ok(Val::I(x)),
            _ => Err(format!("{k} takes a whole number of 0 or more, not {v:?}")),
        }
    } else {
        match digits.parse::<f64>() {
            Ok(x) if x.is_finite() && x >= 0.0 => Ok(Val::F(x)),
            _ => Err(format!("{k} takes a number of 0 or more, not {v:?}")),
        }
    }
}

/// Settings kept so older specs still read, with the one value the built-in curator runs.
const ONLY: &[(&str, &str)] = &[
    ("rule", "rent"),
    ("stub", "plain"),
    ("leave", "oldest"),
    ("spill", ""),
    ("early", "0"),
    ("settle", "0"),
    ("topic", "0"),
    ("noted", "0"),
    ("sticky", "0"),
    ("loops", "0"),
    ("recite", "0"),
    ("task", "0"),
    ("reruns", "0"),
    ("stall", "0"),
    ("trust", "1"),
    ("ttl", "0"),
];

/// The engine a spec describes, or why it cannot run here.
fn params(spec: &str) -> Result<Params, String> {
    let given = parse_params(spec)?;
    if given.contains_key("model") {
        return Err("model is not supported by the built-in curator".into());
    }
    let mut kv = parse_params(D_SPEC)?;
    kv.extend(given);
    let mut vals: IndexMap<String, Val> = IndexMap::new();
    for (k, v) in &kv {
        if !INTS.contains(&k.as_str()) && !STRS.contains(&k.as_str()) && !FLOATS.contains(&k.as_str()) {
            return Err(format!("unknown setting {k:?}"));
        }
        vals.insert(k.clone(), typed(k, v)?);
    }
    let unsupported = |k: &str| Err(format!("{k} is not supported by the built-in curator"));
    for k in ["outcome", "gwin", "lerr", "jit"] {
        if vals.contains_key(k) {
            return unsupported(k);
        }
    }
    for (k, only) in ONLY {
        if vals.get(*k).is_some_and(|v| *v != typed(k, only).expect("a value it takes")) {
            return unsupported(k);
        }
    }
    // every value has its default in D_SPEC
    let missing = |k: &str| format!("{k} has no default");
    let f = |k: &str| match vals.get(k) {
        Some(Val::F(x)) => Ok(*x),
        _ => Err(missing(k)),
    };
    let i = |k: &str| match vals.get(k) {
        Some(Val::I(x)) => Ok(*x),
        _ => Err(missing(k)),
    };
    let s = |k: &str| match vals.get(k) {
        Some(Val::S(x)) => Ok(x.clone()),
        _ => Err(missing(k)),
    };
    let cut = s("cut")?;
    if !["age", "need", "copies", "cover"].contains(&cut.as_str()) {
        return unsupported("cut");
    }
    let head = s("head")?;
    if head != "turn" && head != "keep" {
        return unsupported("head");
    }
    let (floor, eoq) = (f("floor")?, i("eoq")?);
    if i("target")? > 0 && floor == 0.0 && eoq == 0 {
        return Err("target needs a floor above 0".into());
    }
    let (target, cap, hard) = (i("target")?, i("cap")?, i("hard")?);
    if cap > 0 && target > cap {
        return Err("target is above cap".into());
    }
    if hard > 0 && cap > hard {
        return Err("cap is above hard".into());
    }
    let birth: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for p in s("birth")?.replace(',', "+").split('+').map(str::trim).filter(|p| !p.is_empty()) {
            // a stage the curator does not know would take the place of the default and run nothing
            if !["squeeze"].contains(&p) {
                return Err(format!("birth has no stage {p:?}"));
            }
            if !v.iter().any(|x| x == p) {
                v.push(p.to_string());
            }
        }
        v
    };
    Ok(Params {
        threshold: f("threshold")?,
        protect: i("protect")?,
        check: i("check")?,
        ratio: f("ratio")?,
        margin: f("margin")?,
        floor,
        every: i("every")?,
        least: f("least")?,
        target,
        cap,
        hard,
        turn_head: head == "turn",
        eoq,
        ledger: i("ledger")?,
        link: f("link")?,
        cards: i("cards")?,
        birth,
        notes: i("notes")?,
        rules: i("rules")?,
        pin: i("pin")?,
        nudge: i("nudge")?,
        directives: i("directives")?,
        recall: i("recall")?,
        dedup: i("dedup")?,
        cut_need: cut == "need",
        cut_copies: cut == "copies",
        cut_cover: cut == "cover",
        close: i("close")?,
        ahead: f("ahead")?,
        sup: i("sup")?,
        apart: i("apart")?,
        named: i("named")?,
        mission: i("mission")?,
        live: i("live")?,
        lead: i("lead")?,
        aim: i("aim")?,
        follow: i("follow")?,
        brief: i("brief")?,
        digest: i("digest")?,
        hold: i("hold")?,
        still: i("still")?,
        asked: i("asked")?,
        fresh: i("fresh")?,
        stale: i("stale")?,
        pockets: i("pockets")?,
        life: f("life")?,
    })
}

/// The distinctive words of a text (names, paths, long numbers): the ones the curator follows from
/// the material that holds them to the replies that copy them.
pub fn distinctive_words(text: &str) -> Vec<String> {
    let mut int = hindsight::Interner::default();
    hindsight::words(text, &mut int).into_iter().map(|w| int.get(w).to_string()).collect()
}

/// What a distinctive word is, for a report on what was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordKind {
    /// Only digits and the signs of a number (`4373.25`, `1,372`, `-8500.0`).
    Number,
    /// An identifier, path, version or hash: it has a `/`, `\`, `_`, `-`, a dot inside, or a digit among
    /// letters (`tools/audit.py`, `SPEC_EXAMPLES`, `sulko_dolde1`, `40114b`).
    Identifier,
    /// Anything else: letters only (a plain name).
    Other,
}

/// The kind of a word from [`distinctive_words`]. Mechanical, no dictionary: the rule of the H4 notes
/// (a digit, slash or inner dot makes a word private; a number is split off) without the word list.
pub fn word_kind(word: &str) -> WordKind {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    if w.chars().any(|c| c.is_ascii_digit())
        && w.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
        && !w.contains("--")
    {
        return WordKind::Number;
    }
    // a hash: hex digits only, long enough not to be a word
    if w.len() >= 6 && w.chars().all(|c| c.is_ascii_hexdigit()) {
        return WordKind::Identifier;
    }
    let joined = w.chars().any(|c| matches!(c, '/' | '\\' | '_' | '-' | '.') || c.is_ascii_digit());
    if joined {
        WordKind::Identifier
    } else {
        WordKind::Other
    }
}

/// What the replies forgot, by kind and by reply. The plain total (`words`) is the number the earlier
/// tables use; the rest says what the loss is made of and whether one reply holds most of it.
#[derive(Debug, Default, Clone)]
pub struct LostTally {
    /// Forgotten words in all, a word once per reply it is forgotten in.
    pub words: usize,
    pub numbers: usize,
    pub identifiers: usize,
    pub other: usize,
    /// Distinct forgotten words over the whole conversation: one forgotten in many replies counts once.
    distinct: std::collections::HashSet<String>,
    /// The reply with the most forgotten words: its label and count.
    pub largest: Option<(usize, usize)>,
    /// Replies that forgot something.
    pub replies: usize,
}

impl LostTally {
    /// Counts the words forgotten in reply number `reply`.
    pub fn add(&mut self, reply: usize, lost: &[String]) {
        if lost.is_empty() {
            return;
        }
        self.replies += 1;
        self.words += lost.len();
        for w in lost {
            match word_kind(w) {
                WordKind::Number => self.numbers += 1,
                WordKind::Identifier => self.identifiers += 1,
                WordKind::Other => self.other += 1,
            }
            self.distinct.insert(w.clone());
        }
        if self.largest.is_none_or(|(_, n)| lost.len() > n) {
            self.largest = Some((reply, lost.len()));
        }
    }

    /// Distinct forgotten words over the conversation.
    pub fn distinct(&self) -> usize {
        self.distinct.len()
    }

    /// Forgotten words outside the largest reply: what the loss is when one table does not dominate.
    pub fn without_largest(&self) -> usize {
        self.words - self.largest.map_or(0, |(_, n)| n)
    }

    /// The share, in percent, of all forgotten words that the largest reply holds.
    pub fn share_of_largest(&self) -> f64 {
        match self.largest {
            Some((_, n)) if self.words > 0 => 100.0 * n as f64 / self.words as f64,
            _ => 0.0,
        }
    }
}

/// What one call of the curator returns.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The messages to send.
    pub view: Vec<Value>,
    /// Their tokens (estimated).
    pub tokens: i64,
    /// What the curator did so far, by kind.
    pub stats: IndexMap<&'static str, i64>,
    /// Every message, by zone, and whether the view takes it.
    pub memory: Value,
    /// Messages this call put in the view: new ones and ones brought back.
    pub added: usize,
    /// Messages this call took out of the view or cut.
    pub removed: usize,
    /// What the view carries beyond the conversation and what it holds back, by name and size.
    pub curated: Vec<Added>,
}

/// The curator of one conversation.
pub struct Curator {
    spec: String,
    eng: Engine,
    head_tokens: u64,
    /// Tokens the model takes in a call, less its reply (0: no limit).
    window: u64,
    /// What a cached input token costs against an uncached one, from the provider's prices (`None`: the
    /// spec's `ratio`).
    price_ratio: Option<f64>,
    /// Each message's state in the last view (`whole`, `cut`, `out`).
    last: Vec<u8>,
    /// The tools the host offers (their names), when it said.
    tools: Option<Vec<String>>,
    /// For each position of the last view, a hash of the text the window paged there (`None`: not paged).
    paged: Vec<Option<u64>>,
}

impl Curator {
    /// A curator with the settings of ``spec`` (`key=value` pairs separated by `;`, over [`D_SPEC`]).
    pub fn new(spec: &str) -> Result<Curator, String> {
        Ok(Curator {
            spec: spec.to_string(),
            eng: Engine::new(params(spec)?),
            head_tokens: 0,
            window: 0,
            price_ratio: None,
            last: Vec::new(),
            tools: None,
            paged: Vec::new(),
        })
    }

    /// Whether the host adds the curator's directives to the system prompt (`directives=0`: no, so that
    /// a comparison with another policy measures only what the curator does to the view).
    pub fn directives(&self) -> bool {
        params(&self.spec).is_ok_and(|p| p.directives != 0)
    }

    /// Whether the host offers the model the `recall` tool (`recall=1`, the default): a message the view
    /// left out comes back as it was when the model asks for it, so what a cut guessed wrong is not lost.
    pub fn recall(&self) -> bool {
        params(&self.spec).is_ok_and(|p| p.recall != 0)
    }

    /// Forgets the conversation.
    pub fn reset(&mut self) {
        if let Ok(p) = params(&self.spec) {
            self.eng = Engine::new(p);
            if let Some(r) = self.price_ratio {
                self.eng.set_ratio(r);
            }
            self.last.clear();
            self.paged.clear();
            if let Some(t) = &self.tools {
                self.eng.tools(t);
            }
        }
    }

    /// The tools the host offers the model (their names). The curator asks for notes through the `note`
    /// tool when there is one, and for lines in the reply when there is not.
    pub fn tools(&mut self, names: &[String]) {
        self.eng.tools(names);
        self.tools = Some(names.to_vec());
    }

    /// The tokens of the system prompt and tools last given.
    pub fn head_tokens(&self) -> u64 {
        self.head_tokens
    }

    /// The view for the model call that follows ``messages`` (the whole conversation so far).
    pub fn view(&mut self, system_tokens: u64, messages: &[Value], now: f64) -> Result<Reply, String> {
        self.head_tokens = system_tokens;
        self.eng.head = system_tokens as i64;
        self.eng.now = now;
        // the view takes what the window leaves after the system prompt and tools: a floor above that would
        // only move the overflow to the provider (a small local window under many tools); 0 means no limit,
        // so the least is one token
        let budget = match self.window {
            0 => 0,
            w => w.saturating_sub(system_tokens).max(1) as i64,
        };
        self.eng.set_budget(budget);
        let missed = self.eng.stats.get("miss").copied().unwrap_or(0);
        self.eng.sync(messages);
        let mut view = self.eng.view();
        let mut tokens = self.eng.tokens();
        let mut paged = Vec::new();
        if budget > 0 && tokens > budget {
            paged = page(&mut view, tokens - budget);
            if !paged.is_empty() {
                tokens = view.iter().map(json::est).sum();
            }
        }
        // a text paged otherwise than in the last request (or no longer paged) is sent otherwise: the
        // provider's cache misses from there, past what the engine counted for its own changes
        let now: Vec<Option<u64>> = (0..view.len())
            .map(|i| {
                paged.contains(&i).then(|| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&view[i].to_string(), &mut h);
                    std::hash::Hasher::finish(&h)
                })
            })
            .collect();
        let differs = |p: usize| now.get(p).copied().flatten() != self.paged.get(p).copied().flatten();
        if let Some(p) = (0..now.len().max(self.paged.len())).find(|&p| differs(p)).filter(|&p| p < view.len()) {
            let behind: i64 = view[p..].iter().map(json::est).sum();
            // a history the host rewrote starts the engine's stats over
            let total = self.eng.stats.get("miss").copied().unwrap_or(0);
            let counted = if total < missed { total } else { total - missed };
            *self.eng.stats.entry("miss_page").or_insert(0) += behind;
            *self.eng.stats.entry("miss").or_insert(0) += (behind - counted).max(0);
        }
        self.paged = now;
        let memory = self.eng.memory();
        let (added, removed) = self.activity(&memory);
        let curated = self.eng.added();
        Ok(Reply { view, tokens, stats: self.eng.stats.clone(), memory, added, removed, curated })
    }

    /// The model's prices per million input tokens: uncached and read from the cache. The view is then
    /// weighed at the ratio of the two when it is under the spec's `ratio` (0.02 for a cache that costs 2 %),
    /// unless the spec sets `ratio`. Replayed over 656 recorded sessions at a 2 % cache: 4.9 % less input
    /// cost and 21 % fewer forgotten words than weighing it at 0.1. A dearer cache (0.25), or none, keeps the
    /// spec's ratio: weighed at its price it cut 2.8 % (13 % with none) of the cost but forgot 6 % (20 %)
    /// more words. No prices (a local model) keep it too.
    pub fn prices(&mut self, input: f64, cache_read: f64) {
        let Ok(p) = params(&self.spec) else { return };
        let set = self.spec.split(';').any(|kv| kv.split('=').next().is_some_and(|k| k.trim() == "ratio"));
        let cheap =
            (!set && input > 0.0 && cache_read > 0.0).then(|| (cache_read / input).max(0.01)).filter(|r| *r < p.ratio);
        if self.price_ratio != cheap {
            self.price_ratio = cheap;
            self.eng.set_ratio(cheap.unwrap_or(p.ratio));
        }
    }

    /// What the provider said of the call made with the last view: the input tokens it read from its cache
    /// and all the input tokens. With `pockets`, how long the provider keeps a view is learned from it.
    pub fn cached(&mut self, read: u64, input: u64) {
        self.eng.learn(self.head_tokens, read, input);
    }

    /// G0: the rule (and request) that took out the last place message `mi` held `word`, if none sends it.
    pub fn fate(&self, mi: usize, word: &str) -> Option<(u64, &'static str)> {
        self.eng.fate(mi, word)
    }

    /// For measurement: the features of what the view holds, every `stride` requests, and at the end what
    /// each unit was used by, to `path` (`Engine::dump_to`).
    pub fn dump_to(&mut self, path: &str, stride: usize) -> std::io::Result<()> {
        self.eng.dump_to(path, stride)
    }

    /// Ends the dump of `dump_to`.
    pub fn dump_end(&mut self) {
        self.eng.dump_end();
    }

    /// For measurement: the tokens of the sticky set (`engine/window.rs`) at the last request.
    pub fn sticky_tokens(&self) -> i64 {
        self.eng.sticky_tokens()
    }

    /// For measurement: the (message, block) of each tool result a window cut took parts of.
    pub fn cut_blocks(&self) -> Vec<(usize, usize)> {
        self.eng.cut_blocks()
    }

    /// G0: token-requests each rule saved so far.
    pub fn savings(&self) -> Vec<(&'static str, i64)> {
        self.eng.savings()
    }

    /// For measurement: the topic split of a whole conversation, (first message, thread) of each turn.
    pub fn threads(&mut self, messages: &[Value]) -> Vec<(usize, usize)> {
        self.eng.sync(messages);
        self.eng.turn_threads()
    }

    /// The tokens the model takes in one call, less its reply (0: no limit). The view never passes it.
    pub fn window(&mut self, tokens: u64) {
        self.window = tokens;
    }

    /// How many messages this view put in (new or back) and took out (left out or cut) against the last.
    fn activity(&mut self, memory: &Value) -> (usize, usize) {
        const OUT: u8 = 0;
        const CUT: u8 = 1;
        const WHOLE: u8 = 2;
        let code = |x: &Value| match x["state"].as_str().unwrap_or("out") {
            "out" => OUT,
            "cut" => CUT,
            "whole" => WHOLE,
            _ => 3,
        };
        let now: Vec<u8> = memory["cells"].as_array().map(|c| c.iter().map(code).collect()).unwrap_or_default();
        let (mut added, mut removed) = (0, 0);
        for (i, &s) in now.iter().enumerate() {
            let was = self.last.get(i).copied().unwrap_or(OUT);
            if s != OUT && was == OUT {
                added += 1;
            } else if s == OUT && was != OUT || s == CUT && was == WHOLE {
                removed += 1;
            }
        }
        self.last = now;
        (added, removed)
    }

    /// One request of the JSON-lines protocol the golden fixtures record (`{"op": "view" | "reset", ...}`)
    /// and its reply.
    pub fn answer(&mut self, req: &Value, now: f64) -> Value {
        let op = req.get("op");
        if op.and_then(Value::as_str) == Some("reset") {
            let spec = req.get("spec").and_then(Value::as_str).unwrap_or("");
            let model = req.get("model").and_then(Value::as_str).unwrap_or("");
            if !model.is_empty() {
                return json!({"ok": false, "error": "model is not supported by the built-in curator"});
            }
            return match Curator::new(spec) {
                Ok(c) => {
                    *self = c;
                    json!({"ok": true})
                }
                Err(e) => json!({"ok": false, "error": e}),
            };
        }
        if op.and_then(Value::as_str) != Some("view") {
            let shown = op.map_or("None".to_string(), msg::py_repr);
            return json!({"ok": false, "error": format!("unknown op {shown}")});
        }
        let Some(Value::Array(msgs)) = req.get("messages") else {
            return json!({"ok": false, "error": "messages must be a list"});
        };
        if let Some(Value::Array(ts)) = req.get("tools") {
            let names: Vec<String> = ts.iter().filter_map(Value::as_str).map(str::to_string).collect();
            self.tools(&names);
        }
        let st = match req.get("system_tokens") {
            None | Some(Value::Null) => 0,
            Some(v) => match v.as_f64() {
                Some(x) if x >= 0.0 => x as u64,
                Some(_) => 0,
                None => match v.as_str().map(str::trim).map(str::parse::<u64>) {
                    Some(Ok(x)) => x,
                    _ => {
                        return json!({"ok": false, "error": format!(
                            "system_tokens takes a whole number, not {}", msg::py_repr(v))})
                    }
                },
            },
        };
        match self.view(st, msgs, now) {
            Ok(r) => {
                let stats: Map<String, Value> = r.stats.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
                json!({"ok": true, "view": r.view, "tokens": r.tokens, "stats": stats, "memory": r.memory})
            }
            Err(e) => json!({"ok": false, "error": e}),
        }
    }
}

/// Pages the longest texts of a view until ``over`` tokens are gone: each keeps its head and tail and
/// says which lines were left out and why. Only for what the window cannot take. The messages it changed.
fn page(view: &mut [Value], over: i64) -> Vec<usize> {
    let mut left = over;
    let mut changed = Vec::new();
    // a few passes: one text past the window is the common case
    for _ in 0..8 {
        if left <= 0 {
            break;
        }
        let mut best: Option<(usize, Vec<usize>, usize)> = None;
        for (i, m) in view.iter().enumerate() {
            longest(m, &mut Vec::new(), &mut |path, n| {
                if best.as_ref().is_none_or(|b| n > b.2) {
                    best = Some((i, path.to_vec(), n));
                }
            });
        }
        let Some((i, path, n)) = best else { break };
        // under ~2k characters there is nothing worth paging
        if n < 2000 {
            break;
        }
        let Some(Value::String(s)) = at(&mut view[i], &path) else { break };
        // what is kept steps down by eighths of the text, not with the overflow: the overflow changes with
        // every request, and a text cut otherwise each time is read again without the cache each time
        let (need, mut keep) = (n as i64 - left * 4 - 400, n as i64);
        while keep > need && keep > n as i64 / 8 {
            keep = keep * 7 / 8;
        }
        let keep = keep.max(n as i64 / 8).max(800) as usize;
        // lines keep their own endings (CRLF stays CRLF)
        let nl = if s.contains("\r\n") { "\r\n" } else { "\n" };
        let lines: Vec<&str> = s.split_inclusive('\n').collect();
        let total = lines.len();
        let (mut head, mut tail, mut size) = (0usize, total, 0usize);
        while head < tail {
            let take = if head <= total - tail { lines[head].len() } else { lines[tail - 1].len() };
            if size + take > keep {
                break;
            }
            size += take;
            if head <= total - tail {
                head += 1;
            } else {
                tail -= 1;
            }
        }
        // the line form only when it keeps a line: a first line past what is kept goes by characters
        let paged = if head < tail && total > 2 && (head > 0 || tail < total) {
            format!(
                "{}{MARK} lines {}-{} of {} left out here: the whole text does not fit the model's window. Ask for them by line number if they are needed.{nl}{}",
                lines[..head].concat(),
                head + 1,
                tail,
                total,
                lines[tail..].concat()
            )
        } else {
            // one long line: keep its ends by characters
            let chars: Vec<char> = s.chars().collect();
            let h = keep / 2;
            let t = chars.len().saturating_sub(keep - h);
            if t <= h {
                break;
            }
            format!(
                "{}\n{MARK} characters {}-{} of {} left out here: the whole text does not fit the model's window.\n{}",
                chars[..h].iter().collect::<String>(),
                h + 1,
                t,
                chars.len(),
                chars[t..].iter().collect::<String>()
            )
        };
        let saved = (s.chars().count() as i64 - paged.chars().count() as i64) / 4;
        if saved <= 0 {
            break;
        }
        *s = paged;
        left -= saved;
        if !changed.contains(&i) {
            changed.push(i);
        }
    }
    changed
}

/// Calls ``f`` with the path and length (characters) of every string inside ``v``.
/// Only text the model reads as text: the ``text`` and ``content`` strings. Images, thinking, signatures,
/// ids and tool inputs are never visited, cutting them would make the request invalid.
fn longest(v: &Value, path: &mut Vec<usize>, f: &mut dyn FnMut(&[usize], usize)) {
    walk(v, None, path, f)
}

fn walk(v: &Value, key: Option<&str>, path: &mut Vec<usize>, f: &mut dyn FnMut(&[usize], usize)) {
    match v {
        Value::String(s) => {
            if matches!(key, Some("text") | Some("content")) {
                f(path, s.chars().count())
            }
        }
        Value::Array(a) => {
            for (k, x) in a.iter().enumerate() {
                path.push(k);
                walk(x, key, path, f);
                path.pop();
            }
        }
        Value::Object(o) => {
            let kind = o.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(kind, "image" | "document" | "thinking" | "redacted_thinking" | "tool_use" | "server_tool_use")
            {
                return;
            }
            for (k, (name, x)) in o.iter().enumerate() {
                path.push(k);
                walk(x, Some(name.as_str()), path, f);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The value at ``path`` (indices into arrays and objects, in order).
fn at<'a>(v: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    let Some((&k, rest)) = path.split_first() else { return Some(v) };
    match v {
        Value::Array(a) => at(a.get_mut(k)?, rest),
        Value::Object(o) => at(o.values_mut().nth(k)?, rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forgotten_word_is_a_number_an_identifier_or_other() {
        for n in ["4373.25", "1372", "10.0", "-8500.0", "1,372"] {
            assert_eq!(word_kind(n), WordKind::Number, "{n}");
        }
        for id in ["tools/audit.py", "SPEC_EXAMPLES", "sulko_dolde1", "40114b", "deadbeef", "1.2.3a", "c1-200", "a.out"]
        {
            assert_eq!(word_kind(id), WordKind::Identifier, "{id}");
        }
        for o in ["Ledgerpour", "Cargo", "fixture"] {
            assert_eq!(word_kind(o), WordKind::Other, "{o}");
        }
    }

    #[test]
    fn the_tally_splits_by_kind_and_shows_how_much_one_reply_holds() {
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut t = LostTally::default();
        t.add(1, &w(&["a/b.rs", "Zorbal"]));
        t.add(2, &[]);
        let table: Vec<String> = (0..8).map(|i| format!("{i}00.5")).chain(["a/b.rs".to_string()]).collect();
        t.add(3, &table);
        assert_eq!((t.words, t.replies), (11, 2));
        assert_eq!((t.numbers, t.identifiers, t.other), (8, 2, 1));
        assert_eq!(t.words, t.numbers + t.identifiers + t.other);
        assert_eq!(t.distinct(), 10, "a/b.rs forgotten in two replies counts once");
        assert_eq!(t.largest, Some((3, 9)));
        assert_eq!(t.without_largest(), 2);
        assert!((t.share_of_largest() - 100.0 * 9.0 / 11.0).abs() < 1e-9);
        assert_eq!(LostTally::default().share_of_largest(), 0.0);
    }

    #[test]
    fn a_cache_cheaper_than_the_specs_ratio_is_weighed_at_its_price() {
        let mut c = Curator::new("").unwrap();
        c.prices(0.28, 0.028);
        assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
        c.prices(0.3, 0.006);
        assert!((c.eng.ratio() - 0.02).abs() < 1e-9);
        c.reset();
        assert!((c.eng.ratio() - 0.02).abs() < 1e-9, "a reset keeps the prices");
        // a dearer cache, or none, after a switch of model: back to the spec's ratio
        c.prices(0.2, 0.05);
        assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
        c.prices(0.3, 0.006);
        c.prices(1.0, 0.0);
        assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
        // a local model or unknown prices: the spec's ratio
        let mut c = Curator::new("").unwrap();
        c.prices(0.0, 0.0);
        assert!((c.eng.ratio() - 0.1).abs() < 1e-9);
        let mut c = Curator::new("ratio=0.3").unwrap();
        c.prices(0.3, 0.006);
        assert!((c.eng.ratio() - 0.3).abs() < 1e-9);
    }

    fn refused(spec: &str) -> String {
        params(spec).err().unwrap_or_else(|| panic!("{spec:?} ran"))
    }

    #[test]
    fn every_value_has_its_default_in_d_spec_and_the_docs_show_it() {
        assert!(params("").is_ok());
        let d = parse_params(D_SPEC).unwrap();
        let only: Vec<&str> = ONLY.iter().map(|(k, _)| *k).collect();
        let unsupported = ["outcome", "gwin", "lerr", "jit"];
        for k in INTS.iter().chain(FLOATS).chain(STRS) {
            assert!(d.contains_key(*k) || only.contains(k) || unsupported.contains(k), "{k} has no default in D_SPEC");
        }
        let doc =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../docs/how-it-works.md")).unwrap();
        assert!(doc.contains(D_SPEC), "docs/how-it-works.md shows another default spec");
    }

    #[test]
    fn bad_values_are_refused() {
        assert_eq!(refused("eoq=-1"), "eoq takes a whole number of 0 or more, not \"-1\"");
        assert_eq!(refused("protect=lots"), "protect takes a whole number of 0 or more, not \"lots\"");
        for v in ["nan", "inf", "-0.5", "1e999", ""] {
            assert!(refused(&format!("margin={v}")).starts_with("margin takes a number of 0 or more"), "{v}");
        }
        assert_eq!(refused("eoq"), "\"eoq\" is not key=value");
        // settings no engine reads any more are unknown, not taken and ignored
        for k in ["budget", "horizon", "shortlist", "miss", "terms", "rank", "cold"] {
            assert_eq!(refused(&format!("{k}=1")), format!("unknown setting {k:?}"));
        }
        assert_eq!(refused("rule=lru"), "rule is not supported by the built-in curator");
        assert_eq!(refused("target=9000;eoq=0;floor=0"), "target needs a floor above 0");
        assert_eq!(refused("target=9000;cap=5000"), "target is above cap");
        assert_eq!(refused("cap=90000;hard=80000"), "cap is above hard");
        assert_eq!(refused("birth=squeez"), "birth has no stage \"squeez\"");
        assert_eq!(refused("birth=squeeze+telegraf"), "birth has no stage \"telegraf\"");
        assert!(params("birth=").is_ok_and(|p| p.birth.is_empty()), "an empty birth runs no stage");
        // the values these settings take still run, written as they were
        assert!(params(" rule=rent ; recall=0;trust=1.0;margin=1_000;; ").is_ok());
        assert!(params("target=0;cap=0;floor=0;eoq=0").is_ok(), "no target needs no floor");
    }

    /// Replays every fixture: a view that sends a message of the previous view otherwise (or no longer)
    /// counts the tokens from that message on under `miss`, the latest message sent too: an output sent
    /// whole as the latest one goes as a pointer from the next request on (`miss_fresh`).
    #[test]
    fn every_rewrite_of_a_sent_message_counts_its_miss() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let mut names: Vec<_> =
            std::fs::read_dir(dir).unwrap().filter_map(|e| Some(e.ok()?.path())).filter(|p| p.is_file()).collect();
        names.sort();
        let mut bad = Vec::new();
        for path in names {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut cur = Curator::new("").unwrap();
            let (mut msgs, mut prev, mut missed): (Vec<Value>, Vec<Value>, i64) = (Vec::new(), Vec::new(), 0);
            for (n, line) in text.lines().enumerate() {
                let rec: Value = serde_json::from_str(line).unwrap();
                let req = &rec["req"];
                let Some(new) = req.get("new") else {
                    cur.answer(req, 0.0);
                    (msgs, prev, missed) = (Vec::new(), Vec::new(), 0);
                    continue;
                };
                let base = req["base"].as_u64().unwrap() as usize;
                // an edited history is a change of the host's, not of the view
                let edited = base < msgs.len();
                msgs.truncate(base);
                msgs.extend(new.as_array().unwrap().iter().cloned());
                let send = json!({"op": "view", "system_tokens": req.get("system_tokens").cloned().unwrap_or(Value::Null), "messages": msgs});
                let got = cur.answer(&send, 0.0);
                let Some(Value::Array(view)) = got.get("view") else { continue };
                let now = got["stats"].get("miss").and_then(Value::as_i64).unwrap_or(0);
                if !edited {
                    if let Some(p) = (0..prev.len()).find(|&p| view.get(p) != Some(&prev[p])) {
                        // (a message that joined the one before it in the view changes from its first block that differs)
                        let behind: i64 = match view.get(p) {
                            Some(v) => {
                                let (new, old) = (msg::blocks(v.get("content")), msg::blocks(prev[p].get("content")));
                                let b = (0..new.len().min(old.len())).find(|&b| new[b] != old[b]).unwrap_or(0);
                                new[b..].iter().map(json::est).sum::<i64>()
                                    + view[p + 1..].iter().map(json::est).sum::<i64>()
                            }
                            None => 0,
                        };
                        if ((now - missed) as f64) < 0.9 * behind as f64 - 50.0 {
                            bad.push(format!(
                                "{} line {n}: changed at {p}, {} missed of {behind}",
                                path.display(),
                                now - missed
                            ));
                        }
                    }
                }
                (prev, missed) = (view.clone(), now);
            }
        }
        assert!(bad.is_empty(), "{} uncounted:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn a_text_paged_again_keeps_its_cut_while_the_overflow_grows_a_little() {
        let long: String = (0..4000).map(|k| format!("line {k} of the pasted log\n")).collect();
        let paged = |over: i64| {
            let mut view = vec![json!({"role": "user", "content": long.clone()})];
            assert!(!page(&mut view, over).is_empty());
            view
        };
        // the next request adds a few hundred tokens past the window: the paged text is sent as before
        assert_eq!(paged(10_000), paged(10_300));
        assert_eq!(paged(10_000), paged(10_050));
    }

    #[test]
    fn a_first_line_longer_than_what_is_kept_is_paged_by_characters() {
        let long = format!("{}\nsecond line\nthird line", "x".repeat(200_000));
        let mut view = vec![json!({"role": "user", "content": long})];
        assert!(!page(&mut view, 20_000).is_empty());
        let text = view[0]["content"].as_str().unwrap();
        assert!(text.contains("characters") && text.contains("third line"), "{text}");
        assert!(text.len() > 100_000, "{}", text.len());
    }

    #[test]
    fn comma_is_not_a_pair_separator() {
        let p = params("birth=squeeze,squeeze;eoq=9000").unwrap();
        assert_eq!((p.birth.as_slice(), p.eoq), (&["squeeze".to_string()][..], 9000));
        assert_eq!(refused("birth=squeeze,telegraph"), "birth has no stage \"telegraph\"");
        assert_eq!(refused("eoq=1,ledger=2"), "eoq takes a whole number of 0 or more, not \"1,ledger=2\"");
        // a cap without a view after the rewrite takes none
        assert_eq!(params("target=9000;cap=9000").unwrap().cap, 9000);
    }

    /// A text the window pages, sent paged otherwise in the next request, is a miss from where it is.
    #[test]
    fn a_text_paged_otherwise_than_in_the_last_request_counts_its_miss() {
        let paste: String = (0..2000).map(|k| format!("row {k:05} value {} ok\n", k * 7)).collect();
        let mut msgs = vec![json!({"role": "user", "content": format!("Why does this log stop?\n{paste}")})];
        let mut cur = Curator::new("").unwrap();
        cur.window(9000);
        let miss = |r: &Reply| r.stats.get("miss").copied().unwrap_or(0);
        let first = cur.view(0, &msgs, 0.0).unwrap();
        let mut prev = (first.view.clone(), miss(&first));
        let mut changed = 0;
        for k in 0..6 {
            msgs.push(
                json!({"role": "assistant", "content": format!("Step {k}. {}", "Reading the rows again. ".repeat(80))}),
            );
            msgs.push(json!({"role": "user", "content": format!("Go on with part {k}.")}));
            let r = cur.view(0, &msgs, 0.0).unwrap();
            if let Some(p) = (0..prev.0.len()).find(|&p| r.view.get(p) != Some(&prev.0[p])) {
                changed += 1;
                let behind: i64 = r.view[p..].iter().map(json::est).sum();
                assert!(
                    (miss(&r) - prev.1) as f64 >= 0.9 * behind as f64 - 50.0,
                    "request {k}: changed at {p}, {} of {behind}",
                    miss(&r) - prev.1
                );
            }
            prev = (r.view.clone(), miss(&r));
        }
        assert!(changed > 0, "the paged text never changed");
    }
}
