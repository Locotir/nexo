//! The curator of one conversation: the view it returns for each model call.

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::engine::{Added, Engine};
use crate::mechanical::MARK;
use crate::params::params;
use crate::{json, msg, Memory, State};

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
    pub memory: Memory,
    /// Messages this call put in the view: new ones and ones brought back.
    pub added: usize,
    /// Of those, the ones that were in the conversation already and had been left out: what the curator
    /// moved, as against what the conversation itself appended.
    pub back: usize,
    /// Messages this call took out of the view or cut.
    pub removed: usize,
    /// What the view carries beyond the conversation and what it holds back, by name and size.
    pub curated: Vec<Added>,
}

/// The curator of one conversation.
pub struct Curator {
    pub(crate) spec: String,
    pub(crate) eng: Engine,
    pub(crate) head_tokens: u64,
    /// Tokens the model takes in a call, less its reply (0: no limit).
    pub(crate) window: u64,
    /// The model calls left, this one included, when the host knows them (`Curator::calls_left`).
    pub(crate) left: Option<u64>,
    /// What a cached input token costs against an uncached one, from the provider's prices (`None`: the
    /// spec's `ratio`).
    pub(crate) price_ratio: Option<f64>,
    /// Each message's state in the last view (`whole`, `cut`, `out`).
    pub(crate) last: Vec<State>,
    /// The tools the host offers (their names), when it said.
    pub(crate) tools: Option<Vec<String>>,
    /// For each position of the last view, a hash of the text the window paged there (`None`: not paged).
    pub(crate) paged: Vec<Option<u64>>,
}

impl Curator {
    /// A curator with the settings of ``spec`` (`key=value` pairs separated by `;`, over [`D_SPEC`]).
    pub fn new(spec: &str) -> Result<Curator, String> {
        Ok(Curator {
            spec: spec.to_string(),
            eng: Engine::new(params(spec)?),
            head_tokens: 0,
            window: 0,
            left: None,
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
        self.eng.set_left(self.left);
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
        let (added, back, removed) = self.activity(&memory);
        let curated = self.eng.added();
        Ok(Reply { view, tokens, stats: self.eng.stats.clone(), memory, added, back, removed, curated })
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

    /// The model calls the next view and the ones after it are sent for, that one included, when the host
    /// knows them: a sub-agent's rounds left, a session's steps left (`None`: not known, the default). A cut is
    /// then priced over those calls only, so one the run ends before paying back is not made; the window
    /// still cuts whatever is left.
    pub fn calls_left(&mut self, calls: Option<u64>) {
        self.left = calls;
    }

    /// How many messages this view put in (new or back), how many of those were back (they were in the
    /// last view's history and it left them out), and how many it took out (left out or cut).
    fn activity(&mut self, memory: &Memory) -> (usize, usize, usize) {
        let (mut added, mut back, mut removed) = (0, 0, 0);
        let known = self.last.len();
        let mut count = |i: usize, s: State, was: State| {
            if s != State::Out && was == State::Out {
                added += 1;
                back += usize::from(i < known);
            } else if s == State::Out && was != State::Out || s == State::Cut && was == State::Whole {
                removed += 1;
            }
        };
        match self.eng.memory_changed() {
            // the map went on from the one this last read: only the cells it changed can count
            Some((before, changed)) if before == known => {
                self.last.resize(memory.cells.len(), State::Out);
                for &(i, s) in changed {
                    count(i, s, self.last[i]);
                    self.last[i] = s;
                }
            }
            _ => {
                for (i, c) in memory.cells.iter().enumerate() {
                    count(i, c.state, self.last.get(i).copied().unwrap_or_default());
                }
                self.last = memory.cells.iter().map(|c| c.state).collect();
            }
        }
        (added, back, removed)
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
                json!({"ok": true, "view": r.view, "tokens": r.tokens, "stats": stats, "memory": r.memory.to_value()})
            }
            Err(e) => json!({"ok": false, "error": e}),
        }
    }
}

/// Pages the longest texts of a view until ``over`` tokens are gone: each keeps its head and tail and
/// says which lines were left out and why. Only for what the window cannot take. The messages it changed.
pub(crate) fn page(view: &mut [Value], over: i64) -> Vec<usize> {
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
pub(crate) fn longest(v: &Value, path: &mut Vec<usize>, f: &mut dyn FnMut(&[usize], usize)) {
    walk(v, None, path, f)
}

pub(crate) fn walk(v: &Value, key: Option<&str>, path: &mut Vec<usize>, f: &mut dyn FnMut(&[usize], usize)) {
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
pub(crate) fn at<'a>(v: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    let Some((&k, rest)) = path.split_first() else { return Some(v) };
    match v {
        Value::Array(a) => at(a.get_mut(k)?, rest),
        Value::Object(o) => at(o.values_mut().nth(k)?, rest),
        _ => None,
    }
}
