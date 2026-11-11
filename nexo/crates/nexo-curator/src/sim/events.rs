//! What a view loses: forgotten words, blind and denied results, and the events they make.

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use serde_json::{json, Value};

use super::input::{said_words, text_of};
use super::price::tokens_of;
use crate::distinctive_words;

/// The words of `reply` that `history` holds but neither `view` nor `system` does, and how many of its
/// words `history` holds. The name of a tool the reply calls is not one of them: it comes from the tool
/// list sent with every request, so a view that holds no earlier call of that tool has forgotten nothing.
pub fn forgotten(reply: &Value, history: &[Value], view: &[Value], system: &str) -> (Vec<String>, usize) {
    let mut seen = HashSet::new();
    for m in view {
        seen.extend(words_of(m).iter().cloned());
    }
    seen.extend(distinctive_words(system));
    let mut held = HashSet::new();
    for m in history {
        held.extend(words_of(m).iter().cloned());
    }
    let copied: Vec<String> = said_words(reply).into_iter().filter(|w| held.contains(w)).collect();
    let n = copied.len();
    (copied.into_iter().filter(|w| !seen.contains(w)).collect(), n)
}

/// The distinctive words of a message. Each request sends the same history again, so the sets are kept
/// by the hash of the text, which makes a long session linear in its size instead of quadratic.
fn words_of(m: &Value) -> Rc<HashSet<String>> {
    thread_local! {
        static MEMO: RefCell<HashMap<u64, Rc<HashSet<String>>>> = RefCell::new(HashMap::new());
    }
    let text = text_of(m);
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    let key = h.finish();
    if let Some(w) = MEMO.with(|c| c.borrow().get(&key).cloned()) {
        return w;
    }
    let w: Rc<HashSet<String>> = Rc::new(distinctive_words(&text).into_iter().collect());
    MEMO.with(|c| c.borrow_mut().insert(key, w.clone()));
    w
}

/// Whether `m` is the agent writing: an assistant message with text, or a `note` call.
pub fn wrote(m: &Value) -> bool {
    m["role"] == "assistant"
        && match &m["content"] {
            Value::String(s) => !s.trim().is_empty(),
            Value::Array(blocks) => blocks.iter().any(|b| {
                (b["type"] == "text" && b["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
                    || (b["type"] == "tool_use" && b["name"] == "note")
            }),
            _ => false,
        }
}

/// Where the latest request starts in `msgs`: the person's last message with text.
pub fn latest_request(msgs: &[Value]) -> usize {
    let asked = |m: &Value| {
        m["role"] == "user"
            && match &m["content"] {
                Value::String(s) => !s.trim().is_empty(),
                Value::Array(blocks) => blocks.iter().any(|b| b["type"] == "text"),
                _ => false,
            }
    };
    msgs.iter().rposition(asked).unwrap_or(0)
}

/// A result the view leaves out: the message holding it, the tokens of the result, and the tokens of it
/// the view lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lack {
    pub mi: usize,
    pub block: usize,
    pub tokens: i64,
    pub missing: i64,
}

/// The results `blind` counts, one by one: every result of the latest request the agent has not written
/// about yet (`missing` is the whole result when the view keeps under half of its segments, else 0).
pub fn blind_parts(history: &[Value], view: &[Value]) -> Vec<Lack> {
    const SMALL_RESULT: i64 = 100;
    let shown: HashSet<String> = view.iter().flat_map(segments).collect();
    let start = latest_request(history);
    let mut out = Vec::new();
    for (k, m) in history.iter().enumerate().skip(start) {
        let Some(Value::Array(blocks)) = m.get("content") else { continue };
        if history[k + 1..].iter().any(wrote) {
            continue;
        }
        for (bi, b) in blocks.iter().enumerate().filter(|(_, b)| b["type"] == "tool_result") {
            let one = json!({"role": "user", "content": [b]});
            let tokens = tokens_of(&one);
            if tokens < SMALL_RESULT {
                continue;
            }
            let segs = segments(&one);
            let out_of_view = 2 * segs.iter().filter(|s| shown.contains(*s)).count() < segs.len();
            out.push(Lack { mi: k, block: bi, tokens, missing: if out_of_view { tokens } else { 0 } });
        }
    }
    out
}

/// The tokens of results of the latest request in `history` (after the person's last message) that the agent
/// has not written about yet (no text and no `note` call in a later assistant message) that `view` leaves out (it keeps under half of their segments), and the tokens
/// of all such results. The agent is still reading those: what it writes next draws on them, and a reply
/// that paraphrases them shows no copied word for `forgotten` to catch. Results under `SMALL_RESULT` tokens
/// are left out of both counts, as the engine leaves them in place.
pub fn blind(history: &[Value], view: &[Value]) -> (i64, i64) {
    blind_parts(history, view).iter().fold((0, 0), |(m, o), l| (m + l.missing, o + l.tokens))
}

/// The results `denied` counts, one by one: every result in the last message of the request, with the
/// tokens of it the view's last message lacks (`tokens * lacking segments / segments`).
pub fn denied_parts(history: &[Value], view: &[Value]) -> Vec<Lack> {
    const SMALL_RESULT: i64 = 100;
    let (Some(last), Some(sent)) = (history.last(), view.last()) else { return Vec::new() };
    let Some(Value::Array(blocks)) = last.get("content") else { return Vec::new() };
    let here: HashSet<String> = segments(sent).into_iter().collect();
    let mut out = Vec::new();
    for (bi, b) in blocks.iter().enumerate().filter(|(_, b)| b["type"] == "tool_result") {
        let one = json!({"role": "user", "content": [b]});
        let tokens = tokens_of(&one);
        let segs = segments(&one);
        if tokens < SMALL_RESULT || segs.is_empty() {
            continue;
        }
        let lacks = segs.iter().filter(|s| !here.contains(*s)).count() as i64;
        out.push(Lack { mi: history.len() - 1, block: bi, tokens, missing: tokens * lacks / segs.len() as i64 });
    }
    out
}

/// The tokens of the tool results the agent has just asked for (the last message of the request) that the
/// view does not send in their place, as the share of their segments the view's last message lacks, and the
/// tokens of all such results. What the agent just asked for is what it means to read in this step: a pointer
/// to a copy elsewhere, or a note that it was left out, holds the facts but not the reading it asked for, and
/// `blind` misses it, as it looks for the lines anywhere in the view.
pub fn denied(history: &[Value], view: &[Value]) -> (i64, i64) {
    denied_parts(history, view).iter().fold((0, 0), |(m, o), l| (m + l.missing, o + l.tokens))
}

/// The pieces a view can keep or drop, at the finest grain a curator cuts: the lines of a message's text,
/// long lines split every `SEGMENT` bytes.
pub const SEGMENT: usize = 200;

pub fn segments(m: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for line in text_of(m).lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut rest = line;
        while !rest.is_empty() {
            let mut cut = rest.len().min(SEGMENT);
            while !rest.is_char_boundary(cut) {
                cut += 1;
            }
            out.push(rest[..cut].to_string());
            rest = &rest[cut..];
        }
    }
    out
}

/// The tool that produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Read,
    Bash,
    Other,
    /// Not a tool result: text the person or the agent wrote.
    Text,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Read => "read",
            Tool::Bash => "bash",
            Tool::Other => "other",
            Tool::Text => "text",
        }
    }

    /// The family of a tool by its name, whatever the host calls it (`read`, `read_file`, `Read`, `bash`,
    /// `run_command`, ...).
    pub fn of(name: &str) -> Tool {
        match name.to_ascii_lowercase().as_str() {
            "read" | "read_file" | "view" | "cat" | "open" => Tool::Read,
            "bash" | "run_command" | "shell" | "sh" | "exec" | "execute_command" | "powershell" => Tool::Bash,
            _ => Tool::Other,
        }
    }
}

/// How a view lacks something the recorded trajectory depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The next recorded reply copies words only the cut message held.
    Cited,
    /// The agent just asked for this result and the view did not send it in its place.
    Denied,
    /// A result the agent has not written about yet, left out.
    Blind,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Cited => "cited",
            Kind::Denied => "denied",
            Kind::Blind => "blind",
        }
    }
}

/// A place where the view lacks content, grouped by the message that held it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The request (1-based, as the replay numbers them) whose view lacks the content.
    pub t: usize,
    /// The message of the history that held it.
    pub mi: usize,
    pub kind: Kind,
    /// Tokens of the cut message (`cited`), or of the result the view lacks (`denied`, `blind`).
    pub s_e: i64,
    pub tool: Tool,
    /// Requests left after `t`.
    pub s_rest: usize,
    /// Forgotten words the event holds (`cited`), else 0.
    pub words: usize,
}

/// The tool that made the result block `block` of message `mi`: the name of the `tool_use` with its id,
/// looked for in the messages before it. `Text` when the message holds no result.
fn tool_of(history: &[Value], mi: usize, block: Option<usize>) -> Tool {
    let Some(Value::Array(blocks)) = history.get(mi).and_then(|m| m.get("content")) else { return Tool::Text };
    let results = || blocks.iter().enumerate().filter(|(_, b)| b["type"] == "tool_result");
    let Some((_, b)) = block.and_then(|k| results().find(|(i, _)| *i == k)).or_else(|| results().next()) else {
        return Tool::Text;
    };
    let Some(id) = b["tool_use_id"].as_str() else { return Tool::Other };
    for m in history[..mi].iter().rev() {
        let Some(Value::Array(bs)) = m.get("content") else { continue };
        if let Some(u) = bs.iter().find(|u| u["type"] == "tool_use" && u["id"].as_str() == Some(id)) {
            return Tool::of(u["name"].as_str().unwrap_or(""));
        }
    }
    Tool::Other
}

/// The block of message `mi` whose text holds `word`, if it is a result.
fn block_with(history: &[Value], mi: usize, word: &str) -> Option<usize> {
    let Some(Value::Array(blocks)) = history.get(mi).and_then(|m| m.get("content")) else { return None };
    blocks.iter().position(|b| b["type"] == "tool_result" && text_of(&json!({"content": [b]})).contains(word))
}

/// Groups the forgotten `words` of a reply by the message of `history` that held them: the latest message
/// whose text holds the word and for which `fate` says no copy of it is sent (`holds`), else the latest
/// that holds it, else message 0. The pairs come in message order.
pub fn group_cited(
    history: &[Value],
    words: &[String],
    mut fate: impl FnMut(usize, &str) -> bool,
) -> Vec<(usize, Vec<String>)> {
    let texts: Vec<String> = history.iter().map(text_of).collect();
    let mut by: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
    for w in words {
        let holders: Vec<usize> = (0..texts.len()).rev().filter(|&k| texts[k].contains(w.as_str())).collect();
        let mi = holders.iter().copied().find(|&k| fate(k, w)).or_else(|| holders.first().copied()).unwrap_or(0);
        by.entry(mi).or_default().push(w.clone());
    }
    by.into_iter().collect()
}

/// The events of request `t` (1-based) of `s` requests: the `cited` ones from the words the recorded reply
/// copied that the view lacks (`lost`, grouped by `cited`), then the `denied` and `blind` ones.
pub fn events_of(t: usize, s: usize, history: &[Value], view: &[Value], cited: &[(usize, Vec<String>)]) -> Vec<Event> {
    let rest = s.saturating_sub(t);
    let mut out = Vec::new();
    for (mi, words) in cited {
        let tokens = history.get(*mi).map_or(0, tokens_of);
        let tool = tool_of(history, *mi, block_with(history, *mi, &words[0]));
        out.push(Event { t, mi: *mi, kind: Kind::Cited, s_e: tokens, tool, s_rest: rest, words: words.len() });
    }
    for (kind, parts) in [(Kind::Denied, denied_parts(history, view)), (Kind::Blind, blind_parts(history, view))] {
        let mut by: std::collections::BTreeMap<usize, (i64, i64, usize)> = Default::default();
        for l in parts.iter().filter(|l| l.missing > 0) {
            let e = by.entry(l.mi).or_insert((0, 0, l.block));
            e.0 += l.missing;
            if l.missing > e.1 {
                e.1 = l.missing;
                e.2 = l.block;
            }
        }
        for (mi, (missing, _, block)) in by {
            let tool = tool_of(history, mi, Some(block));
            out.push(Event { t, mi, kind, s_e: missing, tool, s_rest: rest, words: 0 });
        }
    }
    out
}

/// The extra input tokens (miss, hit) over the whole session that fetching one event adds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Foot {
    /// With that fetch only.
    pub alone: (i64, i64),
    /// On top of the fetches of every event before it.
    pub joint: (i64, i64),
    /// With every event before it fetched, the closed loop no longer meets this one (the fetches put the
    /// content back, so the curator cuts nothing more there): it adds no step.
    pub gone: bool,
}

/// The result of one open-loop pass of a session through the curator.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub requests: usize,
    pub events: Vec<Event>,
    /// Replies with a recorded reply after them.
    pub replies: usize,
    /// Forgotten words, a word once per reply it is forgotten in.
    pub forgotten: usize,
    /// Words the replies took from the history.
    pub copied: usize,
    pub miss: i64,
    pub hit: i64,
    /// Output tokens the transcript records (0 for a fixture).
    pub out: i64,
    /// Per `cited` or `denied` event, in order: what fetching it costs in input tokens, the re-cuts after it
    /// included (`behave::probe`). Empty until probed.
    pub feet: Vec<Foot>,
    /// Events the closed loop meets that the open loop does not (a fetch makes the curator cut elsewhere later), with the
    /// extra input tokens (miss, hit) fetching all of them adds on top of the fetches of the events above (`behave::probe`).
    pub appear: (usize, (i64, i64)),
    pub blind: (i64, i64),
    pub denied: (i64, i64),
    pub input_cost: f64,
    /// Input and output.
    pub cost: f64,
    /// Per request: the tokens of the view it was sent (system prompt included).
    pub views: Vec<i64>,
    /// Per request: the tokens the cache read.
    pub hits: Vec<i64>,
}

/// A curator and a prefix cache stepped one request at a time, with the totals so far. `run` is a loop over
/// it; the behaviour layer's Monte Carlo drives it with histories it builds itself.
pub struct Stepper {
    cur: crate::Curator,
    cache: super::price::Cache,
    pub run: Run,
}

impl Stepper {
    pub fn new(spec: &str, life: f64, slots: usize) -> Result<Stepper, String> {
        let mut cur = crate::Curator::new(spec).map_err(|e| format!("bad spec: {e}"))?;
        cur.window(0);
        Ok(Stepper { cur, cache: super::price::Cache::new(life, slots), run: Run::default() })
    }

    /// Sends one request and prices its view. With the `reply` the agent gave, also checks what the view
    /// lacks of it and returns the events, numbered `t` of `s` (1-based). `out` is the tokens the model wrote.
    pub fn send(
        &mut self,
        q: &super::input::Request,
        messages: &[Value],
        reply: Option<&Value>,
        t: usize,
        s: usize,
        out: u64,
    ) -> Result<Vec<Event>, String> {
        let cur = &mut self.cur;
        let r = &mut self.run;
        if let Some(tools) = &q.tools {
            cur.tools(tools);
        }
        let shown = cur.view(q.system_tokens, messages, q.now).map_err(|e| format!("view: {e:?}"))?;
        let mut cited = Vec::new();
        if let Some(said) = reply {
            let (lost, held) = forgotten(said, messages, &shown.view, &q.system);
            r.copied += held;
            r.replies += 1;
            r.forgotten += lost.len();
            if !lost.is_empty() {
                cited = group_cited(messages, &lost, |k, w| cur.fate(k, w).is_some());
            }
        }
        let events = events_of(t, s, messages, &shown.view, &cited);
        let (m, o) = blind(messages, &shown.view);
        r.blind = (r.blind.0 + m, r.blind.1 + o);
        let (m, o) = denied(messages, &shown.view);
        r.denied = (r.denied.0 + m, r.denied.1 + o);
        let hit = self.cache.send(&shown.view, q.system_tokens as i64, q.now);
        // a transcript records what the model wrote, and the provider cached it with the prompt
        if q.out_tokens.is_some() {
            if let Some(said) = reply {
                self.cache.extend_last(said);
            }
        }
        let all = shown.view.iter().map(tokens_of).sum::<i64>() + q.system_tokens as i64;
        r.hit += hit;
        r.miss += all - hit;
        r.out += out as i64;
        r.requests += 1;
        r.views.push(all);
        r.hits.push(hit);
        cur.cached(hit as u64, all as u64);
        Ok(events)
    }

    /// Closes the run: the costs at `prices`.
    pub fn finish(mut self, prices: super::price::Prices) -> Run {
        self.run.input_cost = prices.dollars(self.run.miss, self.run.hit);
        self.run.cost = self.run.input_cost + prices.output(self.run.out);
        self.run
    }
}

/// Feeds every request to a curator with `spec`, prices the views with a prefix cache (`life` seconds,
/// `slots` requests) and collects the events. The input cost is the replay's.
pub fn run(
    requests: &[super::input::Request],
    spec: &str,
    prices: super::price::Prices,
    life: f64,
    slots: usize,
) -> Result<Run, String> {
    use super::input::reply_to;
    let mut st = Stepper::new(spec, life, slots)?;
    for (i, q) in requests.iter().enumerate() {
        let ev = st.send(q, &q.messages, reply_to(requests, i), i + 1, requests.len(), q.out_tokens.unwrap_or(0))?;
        st.run.events.extend(ev);
    }
    Ok(st.finish(prices))
}
