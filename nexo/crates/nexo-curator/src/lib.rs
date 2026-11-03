//! The context curator: decides, call by call, which parts of a conversation the model sees.
//!
//! [`Curator`] is the goal engine run on the live path: it takes the whole conversation on every
//! call, keeps what it learnt of the earlier messages and returns the view to send, the tokens of that view and what it did.

mod engine;
mod flat;
mod goal;
mod hindsight;
mod json;
mod learn;
mod ledger;
mod mechanical;
mod msg;
mod py;
mod rules;
mod telegraph;
mod tools;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use engine::{Engine, Params};
use mechanical::MARK;

/// Names any model knows without reading them in the conversation, one per line.
pub const GENERAL_WORDS: &str = include_str!("../data/general.txt");
/// The curator's learnt model (JSON), bundled into the binary.
pub const MODEL_JSON: &str = include_str!("../data/model.json");

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
/// Every step stays in the history, a call made over and over too (`loops=0`): a view of the same shape as
/// one sent before (call ids and the counts in curator lines aside) is never sent again, since at a fixed
/// temperature the model takes the same step again. Folding such a run kept the agent in it for 22 steps
/// on the golden session. The output the agent has just asked for comes whole (`fresh=1`); its repeated runs go
/// as pointers from the next request on, so no pointer sends it looking for the lines. An output the agent
/// has not written about yet (no text and no note since) leaves only as an exact copy of a later one, never
/// on a guess, for its latest 32000 tokens (`still=32000`): a new request does not mean the agent is done with
/// what it was reading. Without it, the golden session lost the reads of a request left unanswered as the
/// next one came in, and the agent read the same file again for 180 steps. Past the budget a guess may take
/// the oldest, so a session where the agent never writes still has a bounded view.
pub const D_SPEC: &str =
    "threshold=0.02;head=turn;recall=0;link=0.5;rule=rent;birth=squeeze;eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;protect=12;directives=0;nudge=0;close=2;ahead=0.25;margin=2.25;dedup=3;live=1;lead=1;aim=1;follow=1;brief=1;digest=1;hold=1;loops=0;asked=3;fresh=1;still=32000;sup=1;named=1;mission=1;apart=8000;check=4000;every=4;ratio=0.1;least=0.0005;floor=0.6;cap=0;cut=age;cards=8;settle=0;topic=0;noted=0;sticky=0;recite=0;task=0;reruns=0;stall=0;stale=1";

const INTS: &[&str] = &[
    "protect",
    "check",
    "cap",
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
];
const STRS: &[&str] = &["rule", "birth", "stub", "head", "leave", "spill", "outcome", "cut"];
const FLOATS: &[&str] =
    &["threshold", "ratio", "margin", "floor", "least", "link", "recall", "early", "trust", "ttl", "lerr", "ahead"];

#[derive(Clone, Debug, PartialEq)]
enum Val {
    I(i64),
    F(f64),
    S(String),
}

/// The `key=value` pairs of a spec, in order. Only `;` separates them: a value may hold a comma
/// (`birth=squeeze,telegraph`).
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
    ("recall", "0"),
    ("early", "0"),
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
    // a cap given without a view after the rewrite leaves no view: the cap alone decides
    if given.contains_key("cap") && !given.contains_key("eoq") {
        kv.insert("eoq".into(), "0".into());
    }
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
    if i("cap")? > 0 && floor == 0.0 && eoq == 0 {
        return Err("cap needs a floor above 0".into());
    }
    let birth: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for p in s("birth")?.replace(',', "+").split('+').map(str::trim).filter(|p| !p.is_empty()) {
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
        cap: i("cap")?,
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
        settle: i("settle")?,
        dedup: i("dedup")?,
        cut_need: cut == "need",
        cut_copies: cut == "copies",
        cut_cover: cut == "cover",
        close: i("close")?,
        ahead: f("ahead")?,
        sup: i("sup")?,
        topic: i("topic")?,
        apart: i("apart")?,
        noted: i("noted")?,
        named: i("named")?,
        mission: i("mission")?,
        sticky: i("sticky")?,
        live: i("live")?,
        lead: i("lead")?,
        aim: i("aim")?,
        follow: i("follow")?,
        brief: i("brief")?,
        digest: i("digest")?,
        hold: i("hold")?,
        still: i("still")?,
        loops: i("loops")?,
        asked: i("asked")?,
        fresh: i("fresh")?,
        recite: i("recite")?,
        task: i("task")?,
        reruns: i("reruns")?,
        stall: i("stall")?,
        stale: i("stale")?,
    })
}

/// The distinctive words of a text (names, paths, long numbers): the ones the curator follows from
/// the material that holds them to the replies that copy them.
pub fn distinctive_words(text: &str) -> Vec<String> {
    let mut int = hindsight::Interner::default();
    hindsight::words(text, &mut int).into_iter().map(|w| int.get(w).to_string()).collect()
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
}

/// The curator of one conversation.
pub struct Curator {
    spec: String,
    eng: Engine,
    head_tokens: u64,
    /// Tokens the model takes in a call, less its reply (0: no limit).
    window: u64,
    /// Each message's state in the last view (`whole`, `cut`, `out`).
    last: Vec<u8>,
    /// The tools the host offers (their names), when it said.
    tools: Option<Vec<String>>,
}

impl Curator {
    /// A curator with the settings of ``spec`` (`key=value` pairs separated by `;`, over [`D_SPEC`]).
    pub fn new(spec: &str) -> Result<Curator, String> {
        Ok(Curator {
            spec: spec.to_string(),
            eng: Engine::new(params(spec)?),
            head_tokens: 0,
            window: 0,
            last: Vec::new(),
            tools: None,
        })
    }

    /// Whether the host adds the curator's directives to the system prompt (`directives=0`: no, so that
    /// a comparison with another policy measures only what the curator does to the view).
    pub fn directives(&self) -> bool {
        params(&self.spec).is_ok_and(|p| p.directives != 0)
    }

    /// Forgets the conversation.
    pub fn reset(&mut self) {
        if let Ok(p) = params(&self.spec) {
            self.eng = Engine::new(p);
            self.last.clear();
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
    pub fn view(&mut self, system_tokens: u64, messages: &[Value], _now: f64) -> Result<Reply, String> {
        self.head_tokens = system_tokens;
        // the view takes what the window leaves after the system prompt and tools, never under a quarter
        let budget = match self.window {
            0 => 0,
            w => w.saturating_sub(system_tokens).max(w / 4) as i64,
        };
        self.eng.set_budget(budget);
        self.eng.sync(messages);
        let mut view = self.eng.view();
        let mut tokens = self.eng.tokens();
        if budget > 0 && tokens > budget && page(&mut view, tokens - budget) {
            tokens = view.iter().map(json::est).sum();
        }
        let memory = self.eng.memory();
        let (added, removed) = self.activity(&memory);
        Ok(Reply { view, tokens, stats: self.eng.stats.clone(), memory, added, removed })
    }

    /// G0: the rule (and request) that took out the last place message `mi` held `word`, if none sends it.
    pub fn fate(&self, mi: usize, word: &str) -> Option<(u64, &'static str)> {
        self.eng.fate(mi, word)
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
/// says which lines were left out and why. Only for what the window cannot take. Whether anything changed.
fn page(view: &mut [Value], over: i64) -> bool {
    let mut left = over;
    let mut changed = false;
    // a few passes: one text past the window is the common case
    for _ in 0..8 {
        if left <= 0 {
            break;
        }
        let mut best: Option<(usize, Vec<usize>, usize)> = None;
        for (i, m) in view.iter().enumerate() {
            longest(m, &mut Vec::new(), &mut |path, n| {
                if best.as_ref().map_or(true, |b| n > b.2) {
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
        let keep = (n as i64 - left * 4 - 400).max(n as i64 / 8).max(800) as usize;
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
        let paged = if head < tail && total > 2 {
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
        changed = true;
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
        assert_eq!(refused("recall=0.5"), "recall is not supported by the built-in curator");
        assert_eq!(refused("cap=9000;floor=0"), "cap needs a floor above 0");
        // the values these settings take still run, written as they were
        assert!(params(" rule=rent ; recall=0;trust=1.0;margin=1_000;; ").is_ok());
        assert!(params("cap=0;floor=0;eoq=0").is_ok(), "no cap needs no floor");
    }

    #[test]
    fn comma_is_not_a_pair_separator() {
        let p = params("birth=squeeze,telegraph;eoq=9000").unwrap();
        assert_eq!((p.birth.as_slice(), p.eoq), (&["squeeze".to_string(), "telegraph".to_string()][..], 9000));
        assert_eq!(refused("eoq=1,ledger=2"), "eoq takes a whole number of 0 or more, not \"1,ledger=2\"");
        // a cap without a view after the rewrite takes none
        assert_eq!(params("cap=9000").unwrap().eoq, 0);
    }
}
