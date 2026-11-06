//! Replays a recorded conversation through the curator, offline, and shows what it keeps and cuts.
//!
//! `cargo run --release -p nexo-curator --example replay -- FILE [--spec SPEC] [--every N] [--quiet] [--window N] [--save N FILE]
//!  [--life S] [--gap S] [--pause N:S]`
//!
//! `FILE` is either a golden fixture (`tests/fixtures/*.jsonl`, lines `{"now", "req", "reply"}`) or a
//! `nexo bench` transcript of the plain arm (`realm-plain-0.jsonl`: each line holds the `messages` the model
//! was sent at that step and the `system` prompt). The curator sees the same conversation the model saw,
//! one request per model call, with the settings of `--spec` (over the defaults). For each request it prints
//! the messages and tokens of the raw history, the messages and tokens of the view, the share kept, whether
//! the view ends in a nudge, and what changed in the stats. At the end it prints the anchor the model would
//! read (rules, notes, pins, ledger) and every message the view sends rebuilt rather than as it was.
//!
//! Two measures help weigh a change before a live run:
//! - forgotten: the distinctive words (names, paths, long numbers) of each reply that the whole history
//!   held but the view the curator built for that request did not. The agent wrote them from material the
//!   curator had left out, so a view that loses more of them risks more wrong answers or reads again.
//! - blind: the tokens of results of the latest request the agent had not yet written about (no text, no `note` since it read
//!   them) that the view left out, as a share of all such tokens, summed over the requests. The agent
//!   writes its next text from them; when it paraphrases, `forgotten` sees no copied word, so a view can
//!   drop what is still being read and look lossless.
//! - denied: the tokens of the results the agent has just asked for that the view does not send in their place
//!   (a pointer to a copy above, a note that they were left out), as a share of all such tokens. `blind` finds
//!   the lines anywhere in the view; this asks whether the agent got the reading it asked for.
//! - repeated result lines: the tokens of result lines that an earlier result in the same request already
//!   holds (lines of 8 characters or more), summed over the requests, for the view and the raw history.
//! - efficiency: how much of the most a view can save it does save, priced with a prefix cache. 0 % is
//!   plain (the whole history every time), 100 % is a floor no view that forgets nothing can go under: at
//!   each request the lines of the request and, for each distinctive word a later reply copies, the smallest
//!   line (cut every 200 bytes) that holds it, every token at the hit price and each line a miss the first
//!   time it is sent. The floor ignores that the cache is a prefix, so it can be out of reach.
//!
//! Costs are priced with a prefix cache: what a request sends unchanged at its start, message by message
//! and then block by block within the first message that changed, is a hit. Without `--life` the cache holds
//! only the last request (a local server with one slot); with `--life S` it holds every request sent in the
//! last `S` seconds, and a request reads the most any of them starts with. `--gap S` spaces the requests
//! `S` seconds apart instead of the times the file holds. `--pause N:S` adds a pause of `S` seconds after every `N`
//! requests, as when the person comes back to a session later. The provider's answer (what it read, of all) goes
//! back to the curator after each request, as a host does.
//!
//! `--window N` gives the curator a window of N tokens (system prompt and tools included), as a small local
//! model has. `--save N FILE` writes the view of request N (the messages, as JSON) to FILE.
//!
//! `--bank FILE...` reads sessions of the corpus (`examples/corpus.rs`) and prints one JSON line for each:
//! the whole history, the agent's own requests (with its compaction) and the curator's view, side by side.
//! It skips the slower measures, so that hundreds of long sessions fit in memory and time.
//! Its `errs` counts failed calls that fail again on the same entity for the same cause, and whether the first
//! failure was still in the view the repeat was made from.
//!
//! The replay shows what the curator keeps and cuts at each step of a conversation that already happened.
//! It cannot show how the model would have reacted to a smaller view (whether it calls `note`, whether it
//! repeats a read): that needs the live benchmark, `nexo bench`.

use std::collections::{HashMap, HashSet};

use nexo_curator::{distinctive_words, Curator};
use serde_json::{json, Value};

/// One request as the agent made it: the conversation so far and the system prompt.
struct Request {
    now: f64,
    system_tokens: u64,
    /// The system prompt's text, when the file holds it.
    system: String,
    messages: Vec<Value>,
    /// The tools the host offered (their names), when the file says.
    tools: Option<Vec<String>>,
}

/// A rough token count of a message (4 characters per token, as the engine estimates).
fn tokens_of(v: &Value) -> i64 {
    let text = match v.get("content") {
        Some(Value::String(s)) => s.len(),
        Some(c) => c.to_string().len(),
        None => v.to_string().len(),
    };
    (text as i64 + 3) / 4
}

/// The content blocks of a message: a text given as a string is one text block, as the API reads it.
fn blocks_of(m: &Value) -> Vec<Value> {
    match m.get("content") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        _ => Vec::new(),
    }
}

/// The tokens of `now` a prefix cache holds after `prev` was sent: the messages sent unchanged at its
/// start and, of the first one that changed, the blocks it still starts with. The cache matches the
/// tokens the request starts with, not whole messages (a provider stores them in units of some tens of
/// tokens), so a message that only grew keeps what it had.
fn cached(prev: &[Value], now: &[Value]) -> i64 {
    let same = now.iter().zip(prev).take_while(|(a, b)| a == b).count();
    let hit: i64 = now[..same].iter().map(tokens_of).sum();
    hit + match (now.get(same), prev.get(same)) {
        (Some(a), Some(b)) => grown(a, b),
        _ => 0,
    }
}

/// A provider's prefix cache over time: the requests sent in the last `life` seconds, each with when it
/// was sent (`life` 0: only the last request, as a local server with one slot).
struct Cache {
    life: f64,
    sent: Vec<(Vec<Value>, f64)>,
}

impl Cache {
    /// Sends `now` (after a system prompt of `system` tokens) at second `at`: the tokens it reads from the
    /// cache, the most any request still held starts it with.
    fn send(&mut self, now: &[Value], system: i64, at: f64) -> i64 {
        let life = self.life;
        if life > 0.0 {
            self.sent.retain(|(_, t)| at - t <= life);
        } else if self.sent.len() > 1 {
            self.sent.drain(..self.sent.len() - 1);
        }
        let hit = self.sent.iter().map(|(v, _)| cached(v, now)).max().map_or(0, |h| h + system);
        self.sent.push((now.to_vec(), at));
        hit
    }
}

/// The tokens of `now` a prefix cache holds when `before` was sent in its place: the blocks both start
/// with, when they come from the same side.
fn grown(now: &Value, before: &Value) -> i64 {
    let (x, y) = (blocks_of(now), blocks_of(before));
    let kept = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
    if now.get("role") == before.get("role") && kept > 0 {
        tokens_of(&json!({ "content": x[..kept] }))
    } else {
        0
    }
}

/// The text of a message, flattened, for printing.
fn text_of(v: &Value) -> String {
    let s = match v.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b["text"].as_str().unwrap_or("").to_string(),
                Some("tool_use") => {
                    let input = match &b["input"] {
                        Value::Object(m) => m
                            .values()
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    };
                    format!("{}({input})", b["name"].as_str().unwrap_or("?"))
                }
                Some("tool_result") => match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(inner) => {
                        inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                    }
                    other => other.to_string(),
                },
                _ => b.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    s
}

/// The requests of a golden fixture: each line's `req`, a full view or a delta on the previous one.
fn fixture_requests(text: &str) -> Vec<Request> {
    let mut msgs: Vec<Value> = Vec::new();
    let mut system = String::new();
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).expect("a JSON line");
        let req = &rec["req"];
        if req.get("op").and_then(Value::as_str) == Some("reset") {
            msgs.clear();
            continue;
        }
        if let Some(s) = req.get("system").and_then(Value::as_str) {
            system = s.to_string();
        }
        if let Some(new) = req.get("new") {
            let base = req["base"].as_u64().unwrap_or(0) as usize;
            msgs.truncate(base);
            msgs.extend(new.as_array().cloned().unwrap_or_default());
        } else if let Some(all) = req.get("messages").and_then(Value::as_array) {
            msgs = all.clone();
        } else {
            continue;
        }
        out.push(Request {
            now: rec["now"].as_f64().unwrap_or(0.0),
            system_tokens: req["system_tokens"].as_u64().unwrap_or(0),
            system: system.clone(),
            messages: msgs.clone(),
            tools: names(&req["tools"]),
        });
    }
    out
}

/// The names in a list of tools (names or `{"name": ...}`), if `v` is one.
fn names(v: &Value) -> Option<Vec<String>> {
    let list = v.as_array()?;
    Some(list.iter().filter_map(|t| t.as_str().or_else(|| t["name"].as_str())).map(str::to_string).collect())
}

/// The requests of a `nexo bench` transcript: the `messages` of each step, as the model was sent them.
fn transcript_requests(text: &str) -> Vec<Request> {
    let mut out = Vec::new();
    for (i, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: Value = serde_json::from_str(line).expect("a JSON line");
        let Some(msgs) = rec["messages"].as_array() else { continue };
        let head = rec["system"].as_str().map_or(0, str::len) + rec["tools"].to_string().len();
        out.push(Request {
            now: 1_750_000_000.0 + 2.0 * i as f64,
            system_tokens: (head as u64).div_ceil(4),
            system: rec["system"].as_str().unwrap_or("").to_string(),
            messages: msgs.clone(),
            tools: names(&rec["tools"]),
        });
    }
    out
}

/// The reply the agent gave to request `i`: the assistant message the next request adds after it, when
/// the next request continues the same conversation.
fn reply_to(requests: &[Request], i: usize) -> Option<&Value> {
    let (now, next) = (&requests[i].messages, &requests.get(i + 1)?.messages);
    let m = next.get(now.len())?;
    (next[..now.len()] == now[..] && m["role"] == "assistant").then_some(m)
}

/// The words of `reply` that `history` holds but neither `view` nor `system` does, and how many of its
/// words `history` holds.
fn forgotten(reply: &Value, history: &[Value], view: &[Value], system: &str) -> (Vec<String>, usize) {
    let words_of =
        |msgs: &[Value]| -> HashSet<String> { msgs.iter().flat_map(|m| distinctive_words(&text_of(m))).collect() };
    let mut seen = words_of(view);
    seen.extend(distinctive_words(system));
    let held = words_of(history);
    let copied: Vec<String> = distinctive_words(&text_of(reply)).into_iter().filter(|w| held.contains(w)).collect();
    let n = copied.len();
    (copied.into_iter().filter(|w| !seen.contains(w)).collect(), n)
}

/// Whether `m` is the agent writing: an assistant message with text, or a `note` call.
fn wrote(m: &Value) -> bool {
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
fn latest_request(msgs: &[Value]) -> usize {
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

/// The tokens of results of the latest request in `history` (after the person's last message) that the agent
/// has not written about yet (no text and no `note` call in a later assistant message) that `view` leaves out (it keeps under half of their segments), and the tokens
/// of all such results. The agent is still reading those: what it writes next draws on them, and a reply
/// that paraphrases them shows no copied word for `forgotten` to catch. Results under `SMALL_RESULT` tokens
/// are left out of both counts, as the engine leaves them in place.
fn blind(history: &[Value], view: &[Value]) -> (i64, i64) {
    const SMALL_RESULT: i64 = 100;
    let shown: HashSet<String> = view.iter().flat_map(segments).collect();
    let start = latest_request(history);
    let (mut missing, mut open) = (0, 0);
    for (k, m) in history.iter().enumerate().skip(start) {
        let Some(Value::Array(blocks)) = m.get("content") else { continue };
        if history[k + 1..].iter().any(wrote) {
            continue;
        }
        for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
            let one = json!({"role": "user", "content": [b]});
            let tokens = tokens_of(&one);
            if tokens < SMALL_RESULT {
                continue;
            }
            open += tokens;
            let segs = segments(&one);
            if 2 * segs.iter().filter(|s| shown.contains(*s)).count() < segs.len() {
                missing += tokens;
            }
        }
    }
    (missing, open)
}

/// The tokens of the tool results the agent has just asked for (the last message of the request) that the
/// view does not send in their place, as the share of their segments the view's last message lacks, and the
/// tokens of all such results. What the agent just asked for is what it means to read in this step: a pointer
/// to a copy elsewhere, or a note that it was left out, holds the facts but not the reading it asked for, and
/// `blind` misses it, as it looks for the lines anywhere in the view.
fn denied(history: &[Value], view: &[Value]) -> (i64, i64) {
    const SMALL_RESULT: i64 = 100;
    let (Some(last), Some(sent)) = (history.last(), view.last()) else { return (0, 0) };
    let Some(Value::Array(blocks)) = last.get("content") else { return (0, 0) };
    let here: HashSet<String> = segments(sent).into_iter().collect();
    let (mut missing, mut open) = (0, 0);
    for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
        let one = json!({"role": "user", "content": [b]});
        let tokens = tokens_of(&one);
        let segs = segments(&one);
        if tokens < SMALL_RESULT || segs.is_empty() {
            continue;
        }
        open += tokens;
        let lacks = segs.iter().filter(|s| !here.contains(*s)).count() as i64;
        missing += tokens * lacks / segs.len() as i64;
    }
    (missing, open)
}

/// The pieces a view can keep or drop, at the finest grain a curator cuts: the lines of a message's text,
/// long lines split every `SEGMENT` bytes.
const SEGMENT: usize = 200;

fn segments(m: &Value) -> Vec<String> {
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

/// The least input cost (miss, hit tokens) any view can have that forgets nothing a reply copies: at each
/// request it sends the lines of the latest request and, for each distinctive word a reply from this
/// request on copies, the smallest segment holding it. Every token sent costs at least a cache hit, and a
/// segment is a miss the first time it is sent, whatever the order and wherever it sits. A view that cuts
/// inside a message still keeps whole segments around what it keeps, so none goes under this.
///
/// With `once`, each new message is also sent whole in the request that first holds it: the model sees every
/// result it asked for at least once. Replies that reason over what they read (a number worked out from a
/// listing) copy few of its words, so without this the floor leaves the material out.
fn bound(requests: &[Request], once: bool) -> (i64, i64) {
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| {
            reply_to(requests, i).map(|m| distinctive_words(&text_of(m)).into_iter().collect()).unwrap_or_default()
        })
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    // per message: its segments, with their distinctive words and tokens
    let mut pieces: HashMap<String, Vec<(String, Vec<String>, i64)>> = HashMap::new();
    let (mut sent, mut miss, mut hit) = (HashSet::<String>::new(), 0i64, 0i64);
    for (i, r) in requests.iter().enumerate() {
        let keys: Vec<String> = r.messages.iter().map(Value::to_string).collect();
        for (k, m) in keys.iter().zip(&r.messages) {
            pieces.entry(k.clone()).or_insert_with(|| {
                segments(m)
                    .into_iter()
                    .map(|s| {
                        let (w, t) = (distinctive_words(&s), (s.len() as i64 + 3) / 4);
                        (s, w, t)
                    })
                    .collect()
            });
        }
        let mut need: HashMap<&str, i64> = HashMap::new();
        if let Some(q) = (0..keys.len()).rev().find(|&j| r.messages[j]["role"] == "user" && !has_result(&r.messages[j]))
        {
            need.extend(pieces[&keys[q]].iter().map(|(s, _, t)| (s.as_str(), *t)));
        }
        if once {
            let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len());
            for k in keys.iter().skip(new) {
                need.extend(pieces[k].iter().map(|(s, _, t)| (s.as_str(), *t)));
            }
        }
        let mut smallest: HashMap<&str, (&str, i64)> = HashMap::new();
        for k in &keys {
            for (s, ws, t) in &pieces[k] {
                for w in ws.iter().filter(|w| ahead[i].contains(*w)) {
                    let e = smallest.entry(w).or_insert((s, *t));
                    if *t < e.1 {
                        *e = (s, *t);
                    }
                }
            }
        }
        need.extend(smallest.into_values());
        hit += r.system_tokens as i64;
        for (s, t) in need {
            if sent.insert(s.to_string()) {
                miss += t;
            } else {
                hit += t;
            }
        }
        if i == 0 {
            (miss, hit) = (miss + r.system_tokens as i64, hit - r.system_tokens as i64);
        }
    }
    (miss, hit)
}

/// The input cost (miss, hit tokens) of an oracle that knows the replies to come and cuts whole results, as
/// a curator does: a result message seen before (not new in this request) and holding no distinctive word a
/// reply from this request on copies may go, as a short stub. It is priced with the same prefix cache as the
/// views, so each cut breaks the cache behind it. With `batched` it cuts the results that may go only when
/// what they cost over the requests left pays for the cache it breaks (`X · R > 9 · S`: `X` tokens cut, `R`
/// requests left, `S` tokens sent from the first cut on, a miss costing ten hits); else it cuts each as soon
/// as it may. A rule that drops whole results and forgets nothing goes much under the better of the two only
/// by knowing the future better: it is the reachable mark, where `bound` is the unreachable one. With
/// `chunk` 1 it keeps, of each result it cuts, the lines that hold a word a reply from this request on copies
/// (any seen result may then go), so only what is never used again leaves: the finest cut a curator could
/// make, still paying the cache it breaks. With a larger `chunk` it keeps or drops runs of that many lines
/// whole (a run stays when one of its lines is used later); 0 keeps nothing of a result it cuts.
fn oracle(requests: &[Request], batched: bool, chunk: usize, digested: bool) -> (i64, i64, Vec<(i64, i64)>) {
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| {
            reply_to(requests, i).map(|m| distinctive_words(&text_of(m)).into_iter().collect()).unwrap_or_default()
        })
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    let mut words: HashMap<String, Vec<String>> = HashMap::new();
    let mut cut: HashMap<String, Value> = HashMap::new();
    let (mut prev, mut miss, mut hit, mut per) = (Vec::<Value>::new(), 0i64, 0i64, Vec::new());
    for (i, r) in requests.iter().enumerate() {
        let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len());
        let keys: Vec<String> = r.messages.iter().map(Value::to_string).collect();
        let start = latest_request(&r.messages);
        let may: Vec<usize> = (0..new.min(keys.len()))
            .filter(|&j| has_result(&r.messages[j]) && !cut.contains_key(&keys[j]))
            .filter(|&j| {
                !digested
                    || j < start
                    || r.messages[j + 1..].iter().any(wrote)
                    || repeats(&r.messages[..j], &r.messages[j])
            })
            .filter(|&j| {
                chunk > 0 || {
                    let ws =
                        words.entry(keys[j].clone()).or_insert_with(|| distinctive_words(&text_of(&r.messages[j])));
                    !ws.iter().any(|w| ahead[i].contains(w))
                }
            })
            .collect();
        if let Some(&first) = may.first() {
            // each result block keeps its call id, as the API asks of any view; only its text shrinks
            let stub = |j: usize| {
                let m = &r.messages[j];
                // a result whose lines an earlier one holds is left out whole: the earlier copy stays
                let again = digested && repeats(&r.messages[..j], m);
                let shrink = |text: String| {
                    let kept: Vec<&str> = if chunk > 0 && !again {
                        // runs of `chunk` lines stay or go whole: a run stays when one of its lines is used later
                        let all: Vec<&str> = text.lines().collect();
                        all.chunks(chunk)
                            .filter(|c| c.iter().any(|l| distinctive_words(l).iter().any(|w| ahead[i].contains(w))))
                            .flatten()
                            .copied()
                            .collect()
                    } else {
                        Vec::new()
                    };
                    if kept.is_empty() {
                        "[out]".to_string()
                    } else {
                        kept.join("\n")
                    }
                };
                let mut m = m.clone();
                if let Some(Value::Array(blocks)) = m.get_mut("content") {
                    for b in blocks.iter_mut().filter(|b| b["type"] == "tool_result") {
                        let text = match &b["content"] {
                            Value::String(s) => s.clone(),
                            Value::Array(inner) => {
                                inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                            }
                            _ => String::new(),
                        };
                        b["content"] = Value::String(shrink(text));
                    }
                }
                m
            };
            let stubs: Vec<(usize, Value)> = may.iter().map(|&j| (j, stub(j))).collect();
            let x: i64 = stubs.iter().map(|(j, v)| tokens_of(&r.messages[*j]) - tokens_of(v)).sum();
            let s: i64 = (first..keys.len())
                .map(|j| cut.get(&keys[j]).map_or_else(|| tokens_of(&r.messages[j]), tokens_of))
                .sum();
            if !batched || x * (requests.len() - i) as i64 > 9 * s {
                cut.extend(stubs.into_iter().map(|(j, v)| (keys[j].clone(), v)));
            }
        }
        let view: Vec<Value> =
            r.messages.iter().zip(&keys).map(|(m, k)| cut.get(k).cloned().unwrap_or_else(|| m.clone())).collect();
        let h = cached(&prev, &view) + if i > 0 { r.system_tokens as i64 } else { 0 };
        let all = view.iter().map(tokens_of).sum::<i64>() + r.system_tokens as i64;
        hit += h;
        miss += all - h;
        per.push((all - h, h));
        prev = view;
    }
    (miss, hit, per)
}

/// Whether every result line of `m` (8 characters or more, trimmed) is already in a result of `before`:
/// leaving `m` out loses nothing while that earlier result stays.
fn repeats(before: &[Value], m: &Value) -> bool {
    let lines = |m: &Value| -> Vec<String> {
        let Some(Value::Array(blocks)) = m.get("content") else { return Vec::new() };
        blocks
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .flat_map(|b| {
                let text = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(inner) => {
                        inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")
                    }
                    _ => String::new(),
                };
                text.lines().map(str::trim).filter(|l| l.len() >= 8).map(str::to_string).collect::<Vec<_>>()
            })
            .collect()
    };
    let mine = lines(m);
    if mine.is_empty() {
        return false;
    }
    let seen: HashSet<String> = before.iter().flat_map(lines).collect();
    mine.iter().all(|l| seen.contains(l))
}

/// Whether a message carries tool results.
fn has_result(m: &Value) -> bool {
    m["content"].as_array().is_some_and(|b| b.iter().any(|x| x["type"] == "tool_result"))
}

/// The signals a curator could read at each request about each run of 8 result lines seen before, with
/// whether a reply from that request on copies one of its distinctive words (the run is used later), one
/// tab-separated row per run and request: label, age in requests, share of its words in the open request,
/// in the agent's last three messages, in anything the agent wrote since it arrived, share of digits in
/// it, whether it came in the open request, its tokens, and keys to the run's text and to the text it is
/// weighed against (the open request and the agent's last three messages), both in the returned texts.
/// A study of what predicts use, not a rule.
fn chunk_rows(requests: &[Request]) -> (Vec<String>, Vec<(u64, String)>) {
    let key = |s: &str| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    let mut texts: HashMap<u64, String> = HashMap::new();
    let said: Vec<HashSet<String>> = (0..requests.len())
        .map(|i| {
            reply_to(requests, i).map(|m| distinctive_words(&text_of(m)).into_iter().collect()).unwrap_or_default()
        })
        .collect();
    let mut ahead: Vec<HashSet<String>> = vec![HashSet::new(); requests.len() + 1];
    for i in (0..requests.len()).rev() {
        ahead[i] = ahead[i + 1].union(&said[i]).cloned().collect();
    }
    let words = |s: &str| -> HashSet<String> { distinctive_words(s).into_iter().collect() };
    let share = |ws: &HashSet<String>, of: &HashSet<String>| -> f64 {
        if ws.is_empty() {
            0.0
        } else {
            ws.iter().filter(|w| of.contains(*w)).count() as f64 / ws.len() as f64
        }
    };
    let mut born: HashMap<String, usize> = HashMap::new();
    let mut rows = Vec::new();
    for (i, r) in requests.iter().enumerate() {
        let new = i.checked_sub(1).map_or(0, |p| requests[p].messages.len()).min(r.messages.len());
        let start = latest_request(&r.messages);
        let asked = words(&text_of(&r.messages[start]));
        let mine: Vec<usize> = (0..r.messages.len()).filter(|&j| r.messages[j]["role"] == "assistant").collect();
        let recent: HashSet<String> =
            mine.iter().rev().take(3).flat_map(|&j| distinctive_words(&text_of(&r.messages[j]))).collect();
        let context: String = std::iter::once(start)
            .chain(mine.iter().rev().take(3).copied())
            .map(|j| text_of(&r.messages[j]))
            .collect::<Vec<_>>()
            .join("\n");
        let ck = key(&context);
        texts.entry(ck).or_insert(context);
        for (j, m) in r.messages.iter().enumerate() {
            let born_at = *born.entry(m.to_string()).or_insert(i);
            if j >= new || !has_result(m) {
                continue;
            }
            let since: HashSet<String> =
                mine.iter().filter(|&&k| k > j).flat_map(|&k| distinctive_words(&text_of(&r.messages[k]))).collect();
            let text = text_of(m);
            let lines: Vec<&str> = text.lines().collect();
            for c in lines.chunks(8) {
                let run = c.join("\n");
                let ws = words(&run);
                if ws.is_empty() {
                    continue;
                }
                let used = ws.iter().any(|w| ahead[i].contains(w));
                let digits = run.chars().filter(char::is_ascii_digit).count() as f64 / run.len().max(1) as f64;
                rows.push(format!(
                    "{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{ck}",
                    used as u8,
                    i - born_at,
                    share(&ws, &asked),
                    share(&ws, &recent),
                    share(&ws, &since),
                    digits,
                    (j >= start) as u8,
                    run.len().div_ceil(4),
                    {
                        let rk = key(&run);
                        texts.entry(rk).or_insert_with(|| run.clone());
                        rk
                    }
                ));
            }
        }
    }
    (rows, texts.into_iter().collect())
}

/// The input cost of a view sequence at 0.28 $ per million missed tokens and 0.028 $ per million hits.
fn dollars(miss: i64, hit: i64) -> f64 {
    (miss as f64 * 0.28 + hit as f64 * 0.028) / 1e6
}

/// The tokens of result lines in `msgs` that an earlier result in `msgs` already holds (lines of 8
/// characters or more, compared trimmed).
fn repeated_tokens(msgs: &[Value]) -> i64 {
    let mut seen: HashSet<String> = HashSet::new();
    let mut n = 0;
    for m in msgs {
        let Some(Value::Array(blocks)) = m.get("content") else { continue };
        for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
            let text = match &b["content"] {
                Value::String(s) => s.clone(),
                Value::Array(inner) => inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
                _ => continue,
            };
            let mut mine = Vec::new();
            for line in text.lines().map(str::trim).filter(|l| l.len() >= 8) {
                if seen.contains(line) {
                    n += (line.len() as i64 + 3) / 4;
                } else {
                    mine.push(line.to_string());
                }
            }
            seen.extend(mine);
        }
    }
    n
}

/// How many messages of a list hold each distinctive word.
#[derive(Default)]
struct Bag(HashMap<String, u32>);

impl Bag {
    fn add(&mut self, words: &[String]) {
        for w in words {
            *self.0.entry(w.clone()).or_insert(0) += 1;
        }
    }

    fn sub(&mut self, words: &[String]) {
        for w in words {
            if let Some(n) = self.0.get_mut(w) {
                *n -= 1;
                if *n == 0 {
                    self.0.remove(w);
                }
            }
        }
    }

    fn has(&self, w: &str) -> bool {
        self.0.contains_key(w)
    }
}

/// The messages one way of sending a session sent last, with the tokens and distinctive words of each,
/// kept up to date by what changed at their end (the past a request keeps is not looked at again).
#[derive(Default)]
struct Sending {
    msgs: Vec<Value>,
    tokens: Vec<i64>,
    words: Vec<Vec<String>>,
    bag: Bag,
}

impl Sending {
    /// Keeps the first `base` messages and adds `new`; returns the tokens a prefix cache holds.
    fn extend(&mut self, base: usize, new: Vec<Value>) -> i64 {
        let base = base.min(self.msgs.len());
        let mut hit: i64 = self.tokens[..base].iter().sum();
        if let (Some(before), Some(now)) = (self.msgs.get(base), new.first()) {
            hit += grown(now, before);
        }
        self.cut(base);
        for m in new {
            self.push(m);
        }
        hit
    }

    /// Sends `now` in place of the last messages; returns the tokens a prefix cache holds, and whether the
    /// start of `now` differs from what was sent before (the past was rewritten).
    fn send(&mut self, now: Vec<Value>) -> (i64, bool) {
        let hit = cached(&self.msgs, &now);
        let same = now.iter().zip(&self.msgs).take_while(|(a, b)| a == b).count();
        let rewrote = same < self.msgs.len();
        self.cut(same);
        for m in now.into_iter().skip(same) {
            self.push(m);
        }
        (hit, rewrote)
    }

    fn cut(&mut self, keep: usize) {
        for w in self.words.drain(keep.min(self.msgs.len())..) {
            self.bag.sub(&w);
        }
        self.msgs.truncate(keep);
        self.tokens.truncate(keep);
    }

    fn push(&mut self, m: Value) {
        let words = distinctive_words(&text_of(&m));
        self.bag.add(&words);
        self.words.push(words);
        self.tokens.push(tokens_of(&m));
        self.msgs.push(m);
    }

    fn total(&self) -> i64 {
        self.tokens.iter().sum()
    }
}

/// One way of sending a session, summed over its requests.
#[derive(Default)]
struct Way {
    miss: i64,
    hit: i64,
    /// The most tokens one request sent.
    peak: i64,
    /// Distinctive words the replies took from the history that this way had not sent with the request.
    lost: usize,
    /// Replies with at least one such word.
    lost_replies: usize,
    /// Requests that rewrote what an earlier one had sent (a compaction, a cut).
    rewrites: usize,
}

impl Way {
    fn count(&mut self, all: i64, hit: i64) {
        self.miss += all - hit;
        self.hit += hit;
        self.peak = self.peak.max(all);
    }

    fn json(&self) -> Value {
        json!({
            "usd": dollars(self.miss, self.hit), "miss": self.miss, "hit": self.hit, "peak": self.peak,
            "lost": self.lost, "lost_replies": self.lost_replies, "rewrites": self.rewrites,
        })
    }
}

/// What a failed tool call says went wrong, from the first lines of its output; `fail` when no clear cause.
fn error_kind(out: &str, is_error: bool) -> Option<&'static str> {
    use std::sync::OnceLock;
    static KINDS: OnceLock<Vec<(&'static str, regex::Regex)>> = OnceLock::new();
    static ANY: OnceLock<regex::Regex> = OnceLock::new();
    let kinds = KINDS.get_or_init(|| {
        [
            ("glob", r"no matches found"),
            ("notool", r"command not found|is not recognized as|: not found\b|no such command|unknown command"),
            (
                "readonly",
                r"permission denied|read-only file system|operation not permitted|access is denied|EACCES|EROFS",
            ),
            ("missing", r"no such file|does not exist|cannot find|can't open|not a directory|ENOENT|file not found"),
            ("blocked", r"\bblocked\b|denied by|not allowed|was rejected"),
            ("timeout", r"timed out|timeout"),
        ]
        .into_iter()
        .map(|(k, p)| (k, regex::RegexBuilder::new(p).case_insensitive(true).build().unwrap()))
        .collect()
    });
    let any = ANY.get_or_init(|| {
        regex::RegexBuilder::new(
            r"exit code [1-9]|exit status [1-9]|traceback \(most recent|\berror[:\[]|\bfatal:|panicked at",
        )
        .case_insensitive(true)
        .build()
        .unwrap()
    });
    let head: String = out.chars().take(400).collect();
    if let Some((k, _)) = kinds.iter().find(|(_, r)| r.is_match(&head)) {
        return Some(k);
    }
    (is_error || any.is_match(&head)).then_some("fail")
}

/// The thing a call acts on: the path it names, else for a command the path its error names, the
/// program it says is missing, or the program it runs.
fn entity_of(name: &str, input: &Value, out: &str, kind: &str) -> String {
    for k in ["file_path", "path", "notebook_path"] {
        if let Some(p) = input[k].as_str() {
            return p.to_string();
        }
    }
    let Some(cmd) = input["command"].as_str() else {
        let key = input.as_object().and_then(|m| m.values().find_map(Value::as_str)).unwrap_or("");
        return format!("{name}:{}", key.chars().take(80).collect::<String>());
    };
    use std::sync::OnceLock;
    static PATH: OnceLock<regex::Regex> = OnceLock::new();
    static TOOL: OnceLock<regex::Regex> = OnceLock::new();
    let head: String = out.chars().take(400).collect();
    if matches!(kind, "missing" | "readonly") {
        let re = PATH.get_or_init(|| regex::Regex::new(r#"(?:^|[\s'"`:(])((?:~|\.{1,2})?/[^\s'"`:,)]+)"#).unwrap());
        if let Some(c) = re.captures(&head) {
            return c[1].to_string();
        }
    }
    if kind == "notool" {
        let re = TOOL.get_or_init(|| {
            regex::Regex::new(
                r"command not found: (\S+)|(\S+): command not found|(\S+): not found|'(\S+)' is not recognized",
            )
            .unwrap()
        });
        if let Some(c) = re.captures(&head) {
            if let Some(m) = c.iter().skip(1).flatten().next() {
                return format!("$ {}", m.as_str());
            }
        }
    }
    if kind == "glob" {
        return "$ zsh glob".into();
    }
    let skip = ["sudo", "pkexec", "nice", "timeout", "env", "time", "command", "exec", "nohup"];
    for seg in cmd.split(['\n', ';', '|', '&']) {
        let seg = seg.trim();
        if seg.is_empty() || seg.starts_with("cd ") || seg == "cd" {
            continue;
        }
        let prog = seg.split_whitespace().find(|t| {
            !t.contains('=') && !skip.contains(t) && !t.starts_with('-') && !t.chars().all(|c| c.is_ascii_digit())
        });
        if let Some(p) = prog {
            return format!("$ {}", p.rsplit('/').next().unwrap_or(p));
        }
    }
    format!("$ {}", cmd.chars().take(40).collect::<String>())
}

/// A failure the session met on one entity, as a fact the curator would keep.
struct Fact {
    kind: &'static str,
    /// The first line of the failed output: how the view is searched for it.
    head: String,
    call: usize,
}

/// Repeated failures on the same entity in one session, and whether the first was still in the context
/// the model had when it repeated the call (`Fable Q4`).
#[derive(Default)]
struct Errs {
    calls: usize,
    errors: usize,
    facts: HashMap<String, Fact>,
    /// call id -> the tool, its input and the call's number in the session
    pending: HashMap<String, (String, Value, usize)>,
    repeats: usize,
    in_view: usize,
    in_sent: usize,
    labeled: usize,
    gap: [usize; 3],
    /// Repeats 30 or more calls after the first, with the first out of the view.
    late_out: usize,
    /// A call on an entity with a live fact that then worked: the fact, shown, would have misled.
    stale_by: std::collections::BTreeMap<&'static str, usize>,
    /// Facts of a clear cause (not `fail`) alive at each request, summed.
    live_labeled: usize,
    /// Calls on an entity with a live fact (where a footer would have shown it).
    shown: usize,
    /// Facts alive at each request, summed (a fact is about 20 tokens).
    live: usize,
    by_kind: std::collections::BTreeMap<&'static str, usize>,
    /// Machine facts (a missing program, the shell's glob, a blocked call), first seen in this session.
    machine: Vec<String>,
}

fn has_line(s: &Sending, line: &str) -> bool {
    !line.is_empty() && s.msgs.iter().any(|m| text_of(m).contains(line))
}

impl Errs {
    /// The new messages of a request: calls of the reply (made from `view`, `sent`), results after them.
    fn see(&mut self, new: &[Value], view: &Sending, sent: Option<&Sending>) {
        for m in new {
            for b in blocks_of(m) {
                if b["type"] == "tool_use" {
                    self.calls += 1;
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    let name = b["name"].as_str().unwrap_or("").to_string();
                    self.pending.insert(id, (name, b["input"].clone(), self.calls));
                } else if b["type"] == "tool_result" {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    let Some((name, input, call)) = self.pending.remove(id) else { continue };
                    let out = text_of(&json!({"content": [b.clone()]}));
                    let kind = error_kind(&out, b["is_error"] == true);
                    let entity = entity_of(&name, &input, &out, kind.unwrap_or(""));
                    // the fact alive when the call was made, and whether its line was in what the model saw
                    if let Some(f) = self.facts.get(&entity) {
                        self.shown += 1;
                        let (in_view, in_sent) = (has_line(view, &f.head), sent.is_some_and(|s| has_line(s, &f.head)));
                        match kind {
                            Some(k) if k == f.kind => {
                                self.repeats += 1;
                                self.in_view += usize::from(in_view);
                                self.in_sent += usize::from(in_sent);
                                self.labeled += usize::from(k != "fail");
                                let gap = call - f.call;
                                self.gap[if gap < 5 {
                                    0
                                } else if gap < 30 {
                                    1
                                } else {
                                    2
                                }] += 1;
                                self.late_out += usize::from(gap >= 30 && !in_view);
                                *self.by_kind.entry(k).or_default() += 1;
                            }
                            None => *self.stale_by.entry(f.kind).or_default() += 1,
                            _ => {}
                        }
                    }
                    match kind {
                        Some(k) => {
                            self.errors += 1;
                            let head = out
                                .lines()
                                .map(str::trim)
                                .find(|l| l.len() >= 12)
                                .unwrap_or("")
                                .chars()
                                .take(120)
                                .collect();
                            let fact = format!("{k} {entity}");
                            if matches!(k, "notool" | "glob" | "blocked") && !self.machine.contains(&fact) {
                                self.machine.push(fact);
                            }
                            self.facts.insert(entity, Fact { kind: k, head, call });
                        }
                        None => {
                            self.facts.remove(&entity);
                        }
                    }
                }
            }
        }
        self.live += self.facts.len();
        self.live_labeled += self.facts.values().filter(|f| f.kind != "fail").count();
    }

    fn json(&self) -> Value {
        json!({
            "calls": self.calls, "errors": self.errors, "repeats": self.repeats, "in_view": self.in_view,
            "in_sent": self.in_sent, "labeled": self.labeled, "gap": self.gap, "late_out": self.late_out,
            "stale_by": self.stale_by, "live_labeled": self.live_labeled, "shown": self.shown, "live": self.live, "by_kind": self.by_kind, "machine": self.machine,
        })
    }
}

/// `--oracle`: the floor an offline policy sets under the view. It knows the replies to come (the words
/// each reply copies from the history) and at each request keeps, within the tokens the curator's view
/// spent on that request, the messages of the request itself, the smallest message holding each word
/// the next reply copies (small ones first when they do not all fit), then the messages with the nearest
/// use ahead, the small ones first (Belady's rule weighed by size); a message never copied from again
/// goes. A reply loses a word only when what it needs does not fit. The views are priced with the same
/// prefix cache, so each change behind the start breaks it: the cost says what the floor would pay, the
/// floor is on what is lost. A history the host rewound is indexed by its final shape.
fn belady(
    records: &[(usize, Vec<Value>)],
    said: &[Option<Vec<String>>],
    budgets: &[i64],
    system: &HashSet<String>,
    system_tokens: i64,
) -> Way {
    let mut fin = Sending::default();
    let mut len_at = Vec::with_capacity(records.len());
    for (base, new) in records {
        fin.extend(*base, new.clone());
        len_at.push(fin.msgs.len());
    }
    let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, ws) in fin.words.iter().enumerate() {
        for w in ws {
            holders.entry(w).or_default().push(j);
        }
    }
    // per message, the requests whose reply copies one of its words, in order
    let mut uses: Vec<Vec<usize>> = vec![Vec::new(); fin.msgs.len()];
    for (r, ws) in said.iter().enumerate() {
        for w in ws.iter().flatten() {
            for &j in holders.get(w.as_str()).map_or(&[][..], Vec::as_slice) {
                if j < len_at[r] && uses[j].last() != Some(&r) {
                    uses[j].push(r);
                }
            }
        }
    }
    let (mut whole, mut view, mut way) = (Sending::default(), Sending::default(), Way::default());
    for (r, (base, new)) in records.iter().enumerate() {
        if let Some(ws) = r.checked_sub(1).and_then(|p| said[p].as_ref()) {
            let lost = ws.iter().filter(|w| !view.bag.has(w) && !system.contains(*w)).count();
            way.lost += lost;
            way.lost_replies += usize::from(lost > 0);
        }
        whole.extend(*base, new.clone());
        let n = whole.msgs.len();
        let base = (*base).min(n);
        let mut keep: Vec<bool> = (0..n).map(|j| j >= base).collect();
        let mut total: i64 = whole.tokens[base..].iter().sum();
        let budget = budgets.get(r).copied().unwrap_or(0).max(total);
        let mut needed: Vec<usize> = said
            .get(r)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|w| {
                holders.get(w.as_str())?.iter().filter(|&&j| j < n).min_by_key(|&&j| (fin.tokens[j], j)).copied()
            })
            .collect();
        needed.sort_unstable();
        needed.dedup();
        needed.sort_by_key(|&j| fin.tokens[j]);
        for j in needed {
            if !keep[j] && total + whole.tokens[j] <= budget {
                keep[j] = true;
                total += whole.tokens[j];
            }
        }
        let mut ahead: Vec<(i64, usize)> = (0..base)
            .filter(|&j| !keep[j])
            .filter_map(|j| {
                let next = *uses[j].iter().find(|&&u| u > r)?;
                Some(((next - r) as i64 * whole.tokens[j].max(1), j))
            })
            .collect();
        ahead.sort_unstable();
        for (_, j) in ahead {
            if total + whole.tokens[j] <= budget {
                keep[j] = true;
                total += whole.tokens[j];
            }
        }
        let msgs: Vec<Value> = (0..n).filter(|&j| keep[j]).map(|j| whole.msgs[j].clone()).collect();
        let (hit, rewrote) = view.send(msgs);
        way.rewrites += usize::from(rewrote);
        let head = if r > 0 { system_tokens } else { 0 };
        way.count(view.total() + system_tokens, hit + head);
    }
    way
}

/// `--bank`: one session of the corpus (`examples/corpus.rs`) sent three ways, as one JSON line: the whole
/// history each time (no curator), the requests as the agent sent them, with its own compaction (when the
/// corpus wrote them, in `sent/` next to the file), and the curator's view. For each way: the input cost
/// with a prefix cache, the largest request, the requests that rewrote the past, and the distinctive words
/// the replies took from the history that the way had not sent. Memory and time grow with the session,
/// not with its square: each request is applied to what the one before left, as the file gives it.
/// With `oracle`, a fourth way: the offline floor of `belady`, at the tokens the curator's view spent.
fn bank(path: &str, spec: &str, recall: bool, oracle: bool, dump: Option<&str>) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file = std::path::Path::new(path);
    let sent_text = file
        .parent()
        .zip(file.file_name())
        .and_then(|(dir, name)| std::fs::read_to_string(dir.join("sent").join(name)).ok());
    let mut sent_lines = sent_text.as_deref().map(|t| t.lines().filter(|l| !l.trim().is_empty()));
    let mut cur = Curator::new(spec).map_err(|e| format!("bad spec: {e}"))?;
    // `--dump DIR`: the features of what the view holds at 40 requests of the session, and what each unit
    // was used by, for a fit of the need heads outside the replay
    if let Some(dir) = dump {
        let n = text.lines().filter(|l| !l.trim().is_empty()).count();
        let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("session");
        cur.dump_to(&format!("{dir}/{name}.tsv"), n.div_ceil(40)).map_err(|e| format!("dump: {e}"))?;
    }
    // `--recall`: the host offers the `recall` tool, as nexo does by default
    let offer = |mut t: Vec<String>| {
        if recall && !t.iter().any(|n| n == "recall") {
            t.push("recall".into());
        }
        t
    };
    cur.tools(&offer(vec!["note".into()]));
    let (mut whole, mut sent, mut view) = (Sending::default(), Sending::default(), Sending::default());
    let (mut plain, mut agent, mut curated) = (Way::default(), Way::default(), Way::default());
    let (mut system, mut system_tokens) = (HashSet::<String>::new(), 0i64);
    let (mut requests, mut replies, mut copied) = (0usize, 0usize, 0usize);
    let mut owned: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    // the same, by the rule and the kind of the message (user, assistant, result) that held the word last
    let mut owned_by: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let parse = |line: &str| -> Result<(usize, Vec<Value>, Value), String> {
        let mut rec: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let base = rec["req"]["base"].as_u64().unwrap_or(0) as usize;
        let new = match rec["req"]["new"].take() {
            Value::Array(a) => a,
            _ => Vec::new(),
        };
        Ok((base, new, rec))
    };
    // G12: result texts (200 bytes or more) the history held before, to size the re-reads that follow a compaction
    let (mut seen, mut reread_after, mut reread_before, mut new_after) = (HashSet::<String>::new(), 0i64, 0i64, 0i64);
    let mut errs = Errs::default();
    // for the oracle: each request as given, the words the reply to each copied, and the view's tokens
    let (mut records, mut said_at, mut budgets) =
        (Vec::<(usize, Vec<Value>)>::new(), Vec::<Option<Vec<String>>>::new(), Vec::<i64>::new());
    let started = std::time::Instant::now();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (base, new, rec) = parse(line)?;
        if oracle {
            records.push((base, new.clone()));
        }
        errs.see(&new, &view, sent_text.is_some().then_some(&sent));
        let req = &rec["req"];
        if requests == 0 {
            system_tokens = req["system_tokens"].as_i64().unwrap_or(0);
            system.extend(distinctive_words(req["system"].as_str().unwrap_or("")));
            if let Some(t) = names(&req["tools"]) {
                cur.tools(&offer(t));
            }
        }
        // the reply to the request before, when this one goes on from it
        if requests > 0 && base == whole.msgs.len() && new.first().is_some_and(|m| m["role"] == "assistant") {
            let said: Vec<String> =
                distinctive_words(&text_of(&new[0])).into_iter().filter(|w| whole.bag.has(w)).collect();
            replies += 1;
            copied += said.len();
            for (way, sending) in [(&mut agent, &sent), (&mut curated, &view)] {
                let lost = said.iter().filter(|w| !sending.bag.has(w) && !system.contains(*w)).count();
                way.lost += lost;
                if std::ptr::eq(sending, &view) {
                    for w in said.iter().filter(|w| !sending.bag.has(w) && !system.contains(*w)) {
                        let mut best: Option<((u64, &str), usize)> = None;
                        for (mi, ws) in whole.words.iter().enumerate() {
                            if ws.iter().any(|x| x == w) {
                                if let Some(f) = cur.fate(mi, w) {
                                    best = Some(best.map_or((f, mi), |b| b.max((f, mi))));
                                }
                            }
                        }
                        let rule = best.map_or("unowned", |b| b.0 .1);
                        *owned.entry(rule.to_string()).or_default() += 1;
                        let kind = best.map_or("none", |(_, mi)| match &whole.msgs[mi] {
                            m if has_result(m) => "result",
                            m if m["role"] == "user" => "user",
                            _ => "assistant",
                        });
                        *owned_by.entry(format!("{rule}/{kind}")).or_default() += 1;
                    }
                }
                way.lost_replies += usize::from(lost > 0);
            }
            if let Some(last) = said_at.last_mut() {
                *last = Some(said);
            }
        }
        let head = if requests > 0 { system_tokens } else { 0 };
        let compacted = agent.rewrites > 0;
        for m in &new {
            for b in blocks_of(m).iter().filter(|b| b["type"] == "tool_result") {
                let t = text_of(&json!({"content": [b.clone()]}));
                if t.len() < 200 {
                    continue;
                }
                let tok = (t.len() as i64 + 3) / 4;
                let dup = !seen.insert(t);
                if compacted {
                    new_after += tok;
                    reread_after += if dup { tok } else { 0 };
                } else if dup {
                    reread_before += tok;
                }
            }
        }
        let hit = whole.extend(base, new);
        plain.count(whole.total() + system_tokens, hit + head);
        if let Some(line) = sent_lines.as_mut().and_then(Iterator::next) {
            let (base, new, _) = parse(line)?;
            agent.rewrites += usize::from(base < sent.msgs.len());
            let hit = sent.extend(base, new);
            agent.count(sent.total() + system_tokens, hit + head);
        }
        let now = rec["now"].as_f64().unwrap_or(0.0);
        let reply = cur.view(system_tokens as u64, &whole.msgs, now).map_err(|e| format!("view: {e}"))?;
        let (hit, rewrote) = view.send(reply.view);
        curated.rewrites += usize::from(rewrote);
        curated.count(view.total() + system_tokens, hit + head);
        said_at.push(None);
        budgets.push(view.total());
        requests += 1;
    }
    cur.dump_end();
    let floor = oracle.then(|| belady(&records, &said_at, &budgets, &system, system_tokens));
    Ok(json!({
        "file": file.file_name().and_then(|n| n.to_str()).unwrap_or(path),
        "requests": requests, "messages": whole.msgs.len(), "replies": replies, "copied": copied,
        "reread": {"before": reread_before, "after": reread_after, "new_after": new_after},
        "secs": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "plain": plain.json(),
        "sent": if sent_text.is_some() { agent.json() } else { Value::Null },
        "view": curated.json(),
        "oracle": floor.map_or(Value::Null, |w| w.json()),
        "errs": errs.json(),
        "own": {"lost": owned, "lost_by": owned_by, "saved": cur.savings().into_iter().collect::<std::collections::BTreeMap<_, _>>()},
    }))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--bank") {
        let mut spec = String::new();
        let mut files = Vec::new();
        let (mut recall, mut oracle, mut dump) = (false, false, None);
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--bank" => {}
                "--recall" => recall = true,
                "--oracle" => oracle = true,
                "--dump" => dump = it.next().cloned(),
                "--spec" => spec = it.next().cloned().unwrap_or_default(),
                _ => files.push(a.clone()),
            }
        }
        for f in files {
            let line =
                bank(&f, &spec, recall, oracle, dump.as_deref()).unwrap_or_else(|e| json!({"file": f, "error": e}));
            println!("{line}");
        }
        return;
    }
    let mut path = None;
    let mut spec = String::new();
    let mut every = 1usize;
    let mut quiet = false;
    let mut window = 0u64;
    let mut save: Option<(usize, String)> = None;
    let (mut life, mut gap, mut pause): (f64, Option<f64>, Option<(usize, f64)>) = (0.0, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--spec" => spec = it.next().cloned().unwrap_or_default(),
            "--every" => every = it.next().and_then(|n| n.parse().ok()).unwrap_or(1).max(1),
            "--quiet" => quiet = true,
            "--window" => window = it.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--save" => {
                let n = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                save = it.next().map(|f| (n, f.clone()));
            }
            "--life" => life = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0),
            "--gap" => gap = it.next().and_then(|n| n.parse().ok()),
            "--pause" => {
                pause = it
                    .next()
                    .and_then(|p| p.split_once(':'))
                    .and_then(|(n, s)| Some((n.parse::<usize>().ok()?.max(1), s.parse().ok()?)))
            }
            _ => path = Some(a.clone()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: replay FILE.jsonl [--spec SPEC] [--every N] [--quiet] [--window N] [--save N FILE] [--life S] [--gap S] [--pause N:S]");
        std::process::exit(2);
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    let first: Value =
        text.lines().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default();
    let requests = if first.get("req").is_some() { fixture_requests(&text) } else { transcript_requests(&text) };
    if requests.is_empty() {
        eprintln!("{path}: no requests found (expected a golden fixture or a nexo bench transcript)");
        std::process::exit(1);
    }
    let mut cur = Curator::new(&spec).unwrap_or_else(|e| {
        eprintln!("bad spec: {e}");
        std::process::exit(2);
    });
    cur.window(window);
    println!("{} requests from {path}", requests.len());
    println!(" req  msgs  raw≈tok  view  viewtok  kept  nudge stats changed");
    let mut prev_stats: Vec<(String, i64)> = Vec::new();
    let mut last_view: Vec<Value> = Vec::new();
    let mut last_msgs: Vec<Value> = Vec::new();
    let (mut sum_raw, mut sum_view) = (0i64, 0i64);
    // a prefix cache, message by message: what a view it holds sent unchanged at the start is a hit
    let (mut prev_view, mut sum_miss, mut sum_hit, mut per_view) = (Vec::<Value>::new(), 0i64, 0i64, Vec::new());
    let (mut cache, mut plain_cache) = (Cache { life, sent: Vec::new() }, Cache { life, sent: Vec::new() });
    let (mut replies, mut lost_replies, mut lost_words, mut lost_first) = (0, 0, 0, Vec::<String>::new());
    let mut copied_words = 0;
    let (mut blind_tokens, mut open_tokens) = (0i64, 0i64);
    let (mut denied_tokens, mut asked_tokens) = (0i64, 0i64);
    let (mut rep_view, mut rep_raw) = (0i64, 0i64);
    // the shapes of the views sent so far: a view the model was already sent comes back the same way
    let (mut shapes, mut revisits, mut first_revisits) = (HashSet::<String>::new(), 0usize, Vec::<String>::new());
    // the same cache with the whole history sent each time: the plain policy
    let (mut plain_miss, mut plain_hit) = (0i64, 0i64);
    // requests whose view changed a message the previous view had sent: each one pays the rest of the view again
    let mut rewrites = 0usize;
    for (i, r) in requests.iter().enumerate() {
        if let Some(t) = &r.tools {
            cur.tools(t);
        }
        let at = gap.map_or(r.now, |g| g * i as f64) + pause.map_or(0.0, |(n, s)| (i / n) as f64 * s);
        let reply = cur.view(r.system_tokens, &r.messages, at).expect("a view");
        if let Some((n, file)) = &save {
            if *n == i + 1 {
                std::fs::write(file, Value::Array(reply.view.clone()).to_string()).expect("the view is written");
            }
        }
        if let Some(said) = reply_to(&requests, i) {
            let (lost, held) = forgotten(said, &r.messages, &reply.view, &r.system);
            copied_words += held;
            replies += 1;
            if !lost.is_empty() {
                lost_replies += 1;
                lost_words += lost.len();
                // REPLAY_LOST=all lists every forgotten word, not just the first ones
                let all = std::env::var("REPLAY_LOST").is_ok_and(|v| v == "all");
                let room = if all { usize::MAX } else { 12usize.saturating_sub(lost_first.len()).min(3) };
                lost_first.extend(lost.iter().take(room).map(|w| format!("r{} {w}", i + 1)));
            }
        }
        let (missing, open) = blind(&r.messages, &reply.view);
        // REPLAY_BLIND=1 names each request that left out a result the agent had not yet written about
        if missing > 0 && std::env::var("REPLAY_BLIND").is_ok_and(|v| v == "1") {
            println!("r{} blind {missing} of {open} tokens", i + 1);
        }
        blind_tokens += missing;
        open_tokens += open;
        let (missing, open) = denied(&r.messages, &reply.view);
        denied_tokens += missing;
        asked_tokens += open;
        if !shapes.insert(shape(&reply.view)) {
            revisits += 1;
            if first_revisits.len() < 6 {
                first_revisits.push(format!("r{}", i + 1));
            }
        }
        rep_view += repeated_tokens(&reply.view);
        rep_raw += repeated_tokens(&r.messages);
        let raw: i64 = r.messages.iter().map(tokens_of).sum();
        sum_raw += raw;
        sum_view += reply.tokens;
        let hit = cache.send(&reply.view, r.system_tokens as i64, at);
        sum_hit += hit;
        // counted as the plain cost is, so that a view equal to the history costs the same
        let all = reply.view.iter().map(tokens_of).sum::<i64>() + r.system_tokens as i64;
        sum_miss += all - hit;
        per_view.push((all - hit, hit));
        // what the provider answers: the tokens it read from its cache, of all
        cur.cached(hit as u64, all as u64);
        rewrites +=
            usize::from(reply.view.iter().zip(&prev_view).take_while(|(a, b)| a == b).count() < prev_view.len());
        prev_view = reply.view.clone();
        let hit = plain_cache.send(&r.messages, r.system_tokens as i64, at);
        plain_hit += hit;
        plain_miss += raw + r.system_tokens as i64 - hit;
        // REPLAY_MISS=1 prints the tokens each request misses in the cache, this view and plain
        if std::env::var("REPLAY_MISS").is_ok_and(|v| v == "1") {
            println!("r{} miss view {} plain {}", i + 1, per_view[i].0, raw + r.system_tokens as i64 - hit);
        }
        let nudge = reply.view.last().map(text_of).is_some_and(|t| t.contains("[curator] Call the `note` tool"));
        let stats: Vec<(String, i64)> = reply.stats.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let mut changed = Vec::new();
        for (k, v) in &stats {
            let before = prev_stats.iter().find(|(pk, _)| pk == k).map_or(0, |(_, pv)| *pv);
            if *v != before {
                changed.push(format!("{k} {before}→{v}"));
            }
        }
        prev_stats = stats;
        if !quiet && (i % every == 0 || i + 1 == requests.len() || !changed.is_empty()) {
            let kept = if raw > 0 { 100.0 * reply.tokens as f64 / raw as f64 } else { 100.0 };
            println!(
                "{:>4} {:>5} {:>8} {:>5} {:>8} {:>4.0}%  {:5} {}",
                i + 1,
                r.messages.len(),
                raw,
                reply.view.len(),
                reply.tokens,
                kept,
                if nudge { "yes" } else { "" },
                changed.join(", ")
            );
        }
        last_view = reply.view;
        last_msgs = r.messages.clone();
    }
    println!();
    println!(
        "total over all requests: raw≈{sum_raw} tokens, view {sum_view} tokens ({:.0}% kept)",
        if sum_raw > 0 { 100.0 * sum_view as f64 / sum_raw as f64 } else { 100.0 }
    );
    println!(
        "prefix cache: miss {sum_miss} tokens, hit {sum_hit} tokens, input cost {:.4} $ at 0.28/0.028 $ per M, {rewrites} rewrites",
        dollars(sum_miss, sum_hit)
    );
    let (low_miss, low_hit) = bound(&requests, false);
    let (once_miss, once_hit) = bound(&requests, true);
    let (plain, now, low) = (dollars(plain_miss, plain_hit), dollars(sum_miss, sum_hit), dollars(low_miss, low_hit));
    println!(
        "efficiency: {:.1}% of the most a view can save (plain {plain:.4} $, this view {now:.4} $, least with nothing forgotten {low:.4} $)",
        if plain > low { 100.0 * (plain - now) / (plain - low) } else { 100.0 }
    );
    for (chunk, digested, what) in [
        (0, false, "whole results"),
        (1, false, "results down to the lines used later"),
        (1, true, "results the agent has written about down to the lines used later"),
    ] {
        let (or, per) = [false, true]
            .map(|b| oracle(&requests, b, chunk, digested))
            .map(|(m, h, per)| (dollars(m, h), per))
            .into_iter()
            .fold((f64::MAX, Vec::new()), |a, b| if b.0 < a.0 { b } else { a });
        if digested && std::env::var("REPLAY_GAP").is_ok_and(|v| v == "1") {
            // the requests where this view costs most over the fair oracle, in millionths of a dollar
            let mut gaps: Vec<(usize, f64, f64)> = per_view
                .iter()
                .zip(&per)
                .enumerate()
                .map(|(i, (&(vm, vh), &(om, oh)))| (i + 1, 1e6 * dollars(vm, vh), 1e6 * dollars(om, oh)))
                .collect();
            gaps.sort_by(|a, b| (b.1 - b.2).total_cmp(&(a.1 - a.2)));
            for (i, v, o) in gaps.iter().take(12) {
                println!("r{i} gap {:.0} µ$ (view {v:.0}, fair oracle {o:.0})", v - o);
            }
        }
        println!(
            "oracle that cuts {what}: {or:.4} $; this view reaches {:.1}% of what it saves over plain",
            if plain > or { 100.0 * (plain - now) / (plain - or) } else { 100.0 }
        );
    }
    if std::env::var("REPLAY_CHUNKS").is_ok_and(|v| v == "1") {
        // how much of the saving is left when a cut keeps or drops whole runs of lines, not single lines
        for chunk in [2, 4, 8, 16, 32, 64] {
            let or = [false, true]
                .map(|b| {
                    let (m, h, _) = oracle(&requests, b, chunk, false);
                    dollars(m, h)
                })
                .into_iter()
                .fold(f64::MAX, f64::min);
            println!("oracle that cuts results down to the runs of {chunk} lines used later: {or:.4} $");
        }
    }
    if let Ok(out) = std::env::var("REPLAY_PREDICT") {
        // what predicts that a run of result lines is used later: rows for a study, appended to a file
        use std::io::Write;
        let (rows, texts) = chunk_rows(&requests);
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&out).expect("REPLAY_PREDICT file");
        for row in &rows {
            writeln!(f, "{row}").expect("REPLAY_PREDICT write");
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{out}.texts"))
            .expect("REPLAY_PREDICT texts file");
        for (k, t) in &texts {
            writeln!(f, "{}", json!({"k": k.to_string(), "text": t})).expect("REPLAY_PREDICT write");
        }
        println!("predict: {} rows to {out}, {} texts to {out}.texts", rows.len(), texts.len());
    }
    let once = dollars(once_miss, once_hit);
    println!(
        "efficiency, each new message seen once: {:.1}% (least {once:.4} $)",
        if plain > once { 100.0 * (plain - now) / (plain - once) } else { 100.0 }
    );
    println!(
        "forgotten: {lost_words} words in {lost_replies} of {replies} replies, of {copied_words} words the replies took from the history{}",
        if lost_first.is_empty() { String::new() } else { format!(" (first: {})", lost_first.join(", ")) }
    );
    let share = |part: i64, whole: i64| if whole > 0 { 100.0 * part as f64 / whole as f64 } else { 0.0 };
    println!(
        "blind: {blind_tokens} of {open_tokens} tokens of results not yet written about were left out ({:.1}%)",
        share(blind_tokens, open_tokens)
    );
    println!(
        "denied: {denied_tokens} of {asked_tokens} tokens of the results just asked for were not sent in their place ({:.1}%)",
        share(denied_tokens, asked_tokens)
    );
    println!(
        "revisits: {revisits} requests sent a view already sent before, call ids and the counts in curator lines aside{}",
        if first_revisits.is_empty() { String::new() } else { format!(" (first: {})", first_revisits.join(", ")) }
    );
    println!(
        "repeated result lines: view {rep_view} tokens ({:.0}% of the view), raw history {rep_raw} tokens ({:.0}%)",
        share(rep_view, sum_view),
        share(rep_raw, sum_raw)
    );
    println!(
        "final stats: {}",
        json!(prev_stats.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>())
    );

    // the last view: which messages go as they are, which are rebuilt, and the anchor
    let raw_index: HashMap<String, usize> = last_msgs.iter().enumerate().map(|(i, m)| (m.to_string(), i)).collect();
    let mut plain = 0;
    let mut rebuilt = Vec::new();
    for (j, v) in last_view.iter().enumerate() {
        if raw_index.contains_key(&v.to_string()) {
            plain += 1;
        } else {
            rebuilt.push((j, v));
        }
    }
    println!();
    println!(
        "last view: {} of {} messages sent as they are, {} rebuilt by the curator",
        plain,
        last_msgs.len(),
        rebuilt.len()
    );
    for (j, v) in &rebuilt {
        let t = text_of(v);
        let role = v["role"].as_str().unwrap_or("?");
        let one_line: String = t.chars().take(110).map(|c| if c == '\n' { ' ' } else { c }).collect();
        println!("  view[{j}] {role} {}tok: {one_line}", tokens_of(v));
    }
    if let Some(anchor) = last_view.first().filter(|v| !raw_index.contains_key(&v.to_string())) {
        println!();
        println!("===== anchor (the first message of the last view, rebuilt by the curator) =====");
        println!("{}", text_of(anchor));
    } else if let Some(first) = last_view.first() {
        // the anchor is folded into the first message: print the part that is not the original text
        let orig = last_msgs.first().map(text_of).unwrap_or_default();
        let now = text_of(first);
        if now.len() > orig.len() {
            println!();
            println!("===== added to the first message by the curator =====");
            println!("{}", &now[orig.len()..]);
        }
    }
}

/// A view with what a model cannot tell apart left out: call ids, and the numbers in the curator's own lines
/// (a count that went up by one). A model sent a view of the same shape as one before, at a fixed
/// temperature, takes the same step again; a history that only ever grows never does this.
fn shape(view: &[Value]) -> String {
    fn walk(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, _)| *k != "id" && *k != "tool_use_id")
                    .map(|(k, x)| (k.clone(), walk(x)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(walk).collect()),
            Value::String(s) if s.contains("[curator]") => Value::String(
                s.lines()
                    .map(|l| {
                        if l.contains("[curator]") {
                            l.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect()
                        } else {
                            l.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            x => x.clone(),
        }
    }
    view.iter().map(|m| walk(m).to_string()).collect::<Vec<_>>().join("\n")
}
