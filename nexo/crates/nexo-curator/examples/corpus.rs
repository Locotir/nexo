//! Turns recorded agent sessions into replay files (`replay FILE`), one per session, so that the curator
//! can be measured offline on real work, many sessions at a time, for free.
//!
//! `cargo run --release -p nexo-curator --example corpus -- OUT_DIR [--min N] [--max-mb M] SRC...`
//!
//! `SRC` is a session log or a folder searched for `*.jsonl`. Two shapes of log are read:
//! - request logs: lines `api-request` (an id, the request it builds on (`base`), how many of its messages
//!   it keeps (`keep`) and the hashes of the messages it adds (`tail`)) and `api-request-blob` (a message by
//!   its hash). The requests of the thread that made most of them are the session; each is one request
//!   of the replay. The system prompt is not in them; the system's own lines (role `system`: reminders,
//!   notices) join the person's turn. A message the log names but never wrote down is taken from the
//!   same file's transcript, by the same hash; a session that still lacks one is skipped.
//! - item logs: lines `response_item` (a message, a tool call, a tool output) after a `session_meta`. Each
//!   run of the model's items is one model call. A tool output is cut to what the model was sent (its first
//!   and last lines): the log keeps it whole.
//!
//! Each session gives `OUT_DIR/NAME.jsonl`: the whole history at each request, never compacted (lines
//! `{"now", "req"}` as in the golden fixtures: the `base` first messages of the previous request kept and
//! `new` added). A request log also gives `OUT_DIR/sent/NAME.jsonl`, the requests as they were sent, with
//! the agent's own compaction. A compaction is found by its shape: a request that drops messages of the
//! one before it. The latest messages it still holds as they were are found at the end of the history,
//! and what comes before them in the request (the summary) is not history; with no such messages, the
//! summary is the message that says the session continues from an earlier one.
//!
//! Reasoning blocks are left out (a provider drops them from later requests, and models without them
//! never send them), and images are replaced by a marker. Sessions with fewer than `--min` requests
//! (default 3) or logs over `--max-mb` megabytes (default 64) are skipped.
//!
//! The logs hold private work: the output stays on this machine, and only aggregate numbers leave it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// The messages of a session, each stored once and named by its index.
#[derive(Default)]
struct Store {
    msgs: Vec<Value>,
    by_key: HashMap<Vec<u32>, u32>,
}

impl Store {
    /// The id of the message named `key`, made by `make` the first time.
    fn id(&mut self, key: Vec<u32>, make: impl FnOnce() -> Value) -> u32 {
        if let Some(&id) = self.by_key.get(&key) {
            return id;
        }
        let id = self.msgs.len() as u32;
        self.msgs.push(make());
        self.by_key.insert(key, id);
        id
    }
}

/// What a request log holds under a hash: a message (whether the model wrote it, and its blocks), a
/// message with nothing left once its reasoning is dropped, or nothing: a hash a request names that the
/// log never wrote down.
enum Blob {
    Msg(bool, Vec<Value>),
    Empty,
    Missing,
}

/// The blobs of a request log, by hash.
#[derive(Default)]
struct Blobs {
    index: HashMap<String, u32>,
    list: Vec<Blob>,
}

impl Blobs {
    fn ix(&mut self, hash: &str) -> u32 {
        if let Some(&i) = self.index.get(hash) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(Blob::Missing);
        self.index.insert(hash.to_string(), i);
        i
    }
}

/// One model call: when it was made and the messages it was sent (as ids in the store).
struct Call {
    now: f64,
    ids: Vec<u32>,
}

/// A session ready to write: its calls with the whole history and, for a request log, as sent.
struct Session {
    store: Store,
    system: String,
    whole: Vec<Call>,
    sent: Vec<Call>,
}

/// Seconds since 1970 of an ISO 8601 time in UTC (`2026-10-30T12:34:56.789Z`).
fn epoch(ts: &str) -> Option<f64> {
    let num = |a: usize, b: usize| ts.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let frac = match ts.get(19..) {
        Some(r) if r.starts_with('.') => {
            let digits: String = r[1..].chars().take_while(char::is_ascii_digit).collect();
            format!("0.{digits}").parse::<f64>().unwrap_or(0.0)
        }
        _ => 0.0,
    };
    // days from the civil date
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((mo + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3_600 + mi * 60 + s) as f64 + frac)
}

/// A content block as the replay reads it: reasoning dropped, images replaced, fields the view does not
/// need (cache marks, citations, callers) left out.
fn clean_part(p: &Value) -> Option<Value> {
    let kind = p.get("type").and_then(Value::as_str)?;
    Some(match kind {
        "thinking" | "redacted_thinking" | "reasoning" => return None,
        "image" | "document" | "input_image" => json!({"type": "text", "text": "[image]"}),
        "text" | "input_text" | "output_text" => json!({"type": "text", "text": p.get("text")?.as_str()?}),
        "tool_use" | "server_tool_use" => json!({
            "type": "tool_use",
            "id": p.get("id").cloned().unwrap_or(Value::Null),
            "name": p.get("name").cloned().unwrap_or(Value::Null),
            "input": p.get("input").cloned().unwrap_or(json!({})),
        }),
        "tool_result" => {
            let content = match p.get("content") {
                Some(Value::Array(inner)) => Value::Array(inner.iter().filter_map(clean_part).collect()),
                Some(c) => c.clone(),
                None => Value::String(String::new()),
            };
            let mut out = json!({"type": "tool_result", "tool_use_id": p.get("tool_use_id").cloned().unwrap_or(Value::Null), "content": content});
            if p.get("is_error").and_then(Value::as_bool) == Some(true) {
                out["is_error"] = Value::Bool(true);
            }
            out
        }
        _ => {
            let mut q = p.clone();
            if let Some(o) = q.as_object_mut() {
                o.remove("cache_control");
                o.remove("citations");
                o.remove("caller");
            }
            q
        }
    })
}

/// The blocks of a message as the replay reads them (a text content is one text block).
fn blocks(m: &Value) -> Vec<Value> {
    match m.get("content") {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(parts)) => parts.iter().filter_map(clean_part).collect(),
        _ => Vec::new(),
    }
}

/// The text of a message's text blocks (or of its string content).
fn text_of(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            a.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// How many messages `a` and `b` start with in common.
fn common(a: &[u32], b: &[u32]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// The longest run at the end of `hist` that `sent` holds whole, as (where it starts in `sent`, length).
fn tail_in(hist: &[u32], sent: &[u32]) -> (usize, usize) {
    let Some(&last) = hist.last() else { return (0, 0) };
    let mut best = (0, 0);
    for q in (0..sent.len()).filter(|&q| sent[q] == last) {
        let mut n = 1;
        while n <= q && n < hist.len() && sent[q - n] == hist[hist.len() - 1 - n] {
            n += 1;
        }
        if n > best.1 {
            best = (q + 1 - n, n);
        }
    }
    best
}

/// The whole history behind each request as sent: what a compaction dropped is kept, and what it added
/// (a summary) is not history.
fn whole_history(sent: &[Call], store: &Store) -> Vec<Call> {
    // the history a compaction took out of the requests, and how many first messages of the request are
    // not history (the summary)
    let (mut archive, mut skip): (Vec<u32>, usize) = (Vec::new(), 0);
    let mut prev: &[u32] = &[];
    let mut out = Vec::with_capacity(sent.len());
    for c in sent {
        let s = &c.ids[..];
        let kept = common(s, prev);
        if prev.is_empty() || (kept >= skip && kept + 1 >= prev.len()) {
            // the request goes on from the one before (its last message may have changed)
        } else {
            let mut hist = archive.clone();
            hist.extend_from_slice(&prev[skip.min(prev.len())..]);
            // the latest messages the request still holds as they were; the very last may have changed
            let (mut at, mut n, mut drop) = (0, 0, 0);
            for d in 0..2.min(hist.len()) {
                let (a, m) = tail_in(&hist[..hist.len() - d], s);
                if m > n {
                    (at, n, drop) = (a, m, d);
                }
            }
            if n > 0 {
                archive = hist[..hist.len() - drop - n].to_vec();
                skip = at;
            } else if let Some(k) = s
                .iter()
                .position(|&id| text_of(&store.msgs[id as usize]).contains("continued from a previous conversation"))
            {
                archive = hist;
                skip = k + 1;
            } else {
                // another conversation in the same log
                archive.clear();
                skip = 0;
            }
        }
        let mut ids = archive.clone();
        ids.extend_from_slice(&s[skip.min(s.len())..]);
        out.push(Call { now: c.now, ids });
        prev = s;
    }
    out
}

/// SHA-256 of `data`, as 64 hex digits.
fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64).wrapping_mul(8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for (k, wi) in K.iter().zip(w.iter()) {
            let [a, b, c, d, e, f, g, hh] = v;
            let t1 = hh
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(*k)
                .wrapping_add(*wi);
            let t2 =
                (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// `v` with the keys of each object in order: the form a request log hashes to name a message.
fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&m[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        x => x.clone(),
    }
}

/// Puts `m` in the blob of its hash, if the log named that blob but never wrote it down.
fn put(m: Value, blobs: &mut Blobs) {
    let hash = sha256(serde_json::to_string(&sorted(&m)).unwrap_or_default().as_bytes());
    if let Some(&i) = blobs.index.get(&hash) {
        if matches!(blobs.list[i as usize], Blob::Missing) {
            let parts = blocks(&m);
            blobs.list[i as usize] =
                if parts.is_empty() { Blob::Empty } else { Blob::Msg(m["role"] == "assistant", parts) };
        }
    }
}

/// The longest run of the person's events [`fill`] joins, as a request joins the results of calls made
/// together.
const JOIN: usize = 24;

/// Puts each stretch of two or more events in `run` as one message of the person.
fn put_joined(run: &mut Vec<Vec<Value>>, blobs: &mut Blobs) {
    for i in 0..run.len() {
        let mut parts: Vec<Value> = run[i].clone();
        for next in run.iter().take((i + JOIN).min(run.len())).skip(i + 1) {
            parts.extend(next.iter().cloned());
            put(json!({"role": "user", "content": parts.clone()}), blobs);
        }
    }
    run.clear();
}

/// Fills the blobs a request log names but never wrote down with the messages its transcript holds under
/// the same hash: a message of the person or of a tool, the blocks of one model message (the transcript
/// keeps them apart, under the id they share), or the results of calls made together (the transcript keeps
/// one event per result, the request sends them as one message).
fn fill(path: &Path, blobs: &mut Blobs) -> std::io::Result<()> {
    let file = BufReader::new(std::fs::File::open(path)?);
    let (mut model, mut at): (Vec<Vec<Value>>, HashMap<String, usize>) = (Vec::new(), HashMap::new());
    let mut run: Vec<Vec<Value>> = Vec::new();
    for line in file.lines() {
        let line = line?;
        if line.contains("\"api-request") || !line.contains("\"message\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let Some(m) = v.get("message").filter(|_| kind == "user" || kind == "assistant") else { continue };
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        if kind == "assistant" {
            put_joined(&mut run, blobs);
            if let (Some(id), Some(parts)) = (m.get("id").and_then(Value::as_str), content.as_array()) {
                let k = *at.entry(id.to_string()).or_insert_with(|| {
                    model.push(Vec::new());
                    model.len() - 1
                });
                model[k].extend(parts.iter().cloned());
            }
        } else {
            run.push(match &content {
                Value::String(s) => vec![json!({"type": "text", "text": s})],
                Value::Array(a) => a.clone(),
                _ => Vec::new(),
            });
        }
        put(json!({"role": m.get("role").cloned().unwrap_or(Value::Null), "content": content}), blobs);
    }
    put_joined(&mut run, blobs);
    for parts in model {
        put(json!({"role": "assistant", "content": parts}), blobs);
    }
    Ok(())
}

/// A request log: the requests of its busiest thread, rebuilt from their bases and blobs, and how many
/// messages they name that the log never wrote down.
///
/// The system's own lines in the list (reminders, notices) are sent inside the person's turn, so they join
/// it, and a run of messages from the same side is one message, as a provider reads it. The system prompt
/// itself is not in the log.
fn request_log(path: &Path) -> std::io::Result<Option<(Session, usize)>> {
    let file = BufReader::new(std::fs::File::open(path)?);
    let mut blobs = Blobs::default();
    // each request: its id, base, keep, tail, source, time
    type Req = (String, Option<String>, usize, Vec<u32>, String, f64);
    let mut reqs: Vec<Req> = Vec::new();
    for line in file.lines() {
        let line = line?;
        if !line.contains("\"api-request") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("api-request-blob") => {
                let Some(hash) = v.get("hash").and_then(Value::as_str) else { continue };
                let m = v.get("message").cloned().unwrap_or(Value::Null);
                let model = m.get("role").and_then(Value::as_str) == Some("assistant");
                let parts = blocks(&m);
                let i = blobs.ix(hash) as usize;
                blobs.list[i] = if parts.is_empty() { Blob::Empty } else { Blob::Msg(model, parts) };
            }
            Some("api-request") => {
                let id = v.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let base = v.get("base").and_then(Value::as_str).map(str::to_string);
                let keep = v.get("keep").and_then(Value::as_u64).unwrap_or(0) as usize;
                let tail = v
                    .get("tail")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(|h| blobs.ix(h)).collect())
                    .unwrap_or_default();
                let source = v.get("querySource").and_then(Value::as_str).unwrap_or_default().to_string();
                let now = v.get("timestamp").and_then(Value::as_str).and_then(epoch).unwrap_or(0.0);
                reqs.push((id, base, keep, tail, source, now));
            }
            _ => {}
        }
    }
    if reqs.is_empty() {
        return Ok(None);
    }
    if blobs.list.iter().any(|b| matches!(b, Blob::Missing)) {
        fill(path, &mut blobs)?;
    }
    let mut count: HashMap<&str, usize> = HashMap::new();
    for r in &reqs {
        *count.entry(r.4.as_str()).or_default() += 1;
    }
    let main = count.iter().max_by_key(|(s, n)| (**n, std::cmp::Reverse(**s))).map(|(s, _)| s.to_string()).unwrap();
    // each request as the blobs it sent, to rebuild the ones built on it
    let mut full: HashMap<String, Vec<u32>> = HashMap::new();
    let (mut store, mut sent, mut missing) = (Store::default(), Vec::new(), std::collections::HashSet::new());
    let mut named = std::collections::HashSet::new();
    for (id, base, keep, tail, source, now) in &reqs {
        let mut list: Vec<u32> = match base {
            Some(b) => match full.get(b) {
                Some(h) if h.len() >= *keep => h[..*keep].to_vec(),
                _ => continue,
            },
            None if *keep == 0 => Vec::new(),
            None => continue,
        };
        list.extend(tail.iter().copied());
        if *source == main {
            // the runs of blobs from one side, each one message
            let mut runs: Vec<(bool, Vec<u32>)> = Vec::new();
            for &b in &list {
                named.insert(b);
                match &blobs.list[b as usize] {
                    Blob::Missing => {
                        missing.insert(b);
                    }
                    Blob::Empty => {}
                    Blob::Msg(model, _) => match runs.last_mut() {
                        Some((side, run)) if side == model => run.push(b),
                        _ => runs.push((*model, vec![b])),
                    },
                }
            }
            let ids = runs
                .into_iter()
                .map(|(model, run)| {
                    let make = || {
                        let content: Vec<Value> = run
                            .iter()
                            .flat_map(|&b| match &blobs.list[b as usize] {
                                Blob::Msg(_, parts) => parts.clone(),
                                _ => Vec::new(),
                            })
                            .collect();
                        json!({"role": if model { "assistant" } else { "user" }, "content": content})
                    };
                    store.id(run.clone(), make)
                })
                .collect();
            sent.push(Call { now: *now, ids });
        }
        full.insert(id.clone(), list);
    }
    let whole = whole_history(&sent, &store);
    if !missing.is_empty() && std::env::var("CORPUS_HOLES").is_ok() {
        eprintln!("holes {} of {} {}", missing.len(), named.len(), path.display());
    }
    Ok(Some((Session { store, system: String::new(), whole, sent }, missing.len())))
}

/// The most lines and bytes of a tool output an item log's model was sent.
const SENT_LINES: usize = 256;
const SENT_BYTES: usize = 10 * 1024;

/// The end of the longest start of `s` within `n` bytes, or the start of its shortest end within `n` bytes.
fn within(s: &str, n: usize, from_end: bool) -> usize {
    let mut i = if from_end { s.len().saturating_sub(n) } else { n.min(s.len()) };
    while !s.is_char_boundary(i) {
        i = if from_end { i + 1 } else { i - 1 };
    }
    i
}

/// A tool output as an item log's model was sent it. The log keeps the whole output; the model got its
/// first and last lines, at most `SENT_LINES` lines and `SENT_BYTES` bytes, half from each end. Measured
/// on the token counts the same logs record: outputs of megabytes added one to three thousand tokens to
/// the next request.
fn as_sent(out: String) -> String {
    let n = out.split('\n').count();
    if n <= SENT_LINES && out.len() <= SENT_BYTES {
        return out;
    }
    let (head, tail) = if n > SENT_LINES {
        let cut = out.match_indices('\n').nth(SENT_LINES / 2 - 1).map_or(out.len(), |(i, _)| i);
        let from = out.rmatch_indices('\n').nth(SENT_LINES / 2 - 1).map_or(0, |(i, _)| i + 1);
        (&out[..cut], &out[from..])
    } else {
        (&out[..], &out[..])
    };
    let head = &head[..within(head, SENT_BYTES / 2, false)];
    let tail = &tail[within(tail, SENT_BYTES / 2, true)..];
    let kept = head.split('\n').count() + tail.split('\n').count();
    format!("{head}\n[... {} of {n} lines left out ...]\n{tail}", n.saturating_sub(kept))
}

/// An item log: one call for each run of the model's items, with the history before it.
fn item_log(path: &Path) -> std::io::Result<Option<Session>> {
    let file = BufReader::new(std::fs::File::open(path)?);
    let mut store = Store::default();
    let mut system = String::new();
    // the messages so far, the one being built (role, blocks) and the calls
    let (mut done, mut open): (Vec<u32>, Option<(&'static str, Vec<Value>)>) = (Vec::new(), None);
    let mut whole = Vec::new();
    let mut n = 0u32;
    let mut close = |open: &mut Option<(&'static str, Vec<Value>)>, done: &mut Vec<u32>, store: &mut Store| {
        if let Some((role, blocks)) = open.take() {
            let kept: Vec<Value> = blocks
                .into_iter()
                .filter(|b| b["type"] != "text" || b["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
                .collect();
            if !kept.is_empty() {
                n += 1;
                done.push(store.id(vec![n], || json!({"role": role, "content": kept})));
            }
        }
    };
    let mut seen_meta = false;
    for line in file.lines() {
        let line = line?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let p = v.get("payload").cloned().unwrap_or(Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                seen_meta = true;
                let base = p.pointer("/base_instructions/text").or_else(|| p.get("instructions"));
                if let Some(s) = base.and_then(Value::as_str) {
                    system = s.to_string();
                }
                continue;
            }
            Some("response_item") => {}
            _ => continue,
        }
        let now = v.get("timestamp").and_then(Value::as_str).and_then(epoch).unwrap_or(0.0);
        let kind = p.get("type").and_then(Value::as_str).unwrap_or_default();
        let role = p.get("role").and_then(Value::as_str).unwrap_or_default();
        let (side, block): (&'static str, Option<Value>) = match (kind, role) {
            ("message", "developer") | ("message", "system") => {
                system.push('\n');
                system.push_str(&text_of(&json!({"content": p.get("content").cloned().unwrap_or(Value::Null)})));
                continue;
            }
            ("message", "assistant") => ("assistant", Some(json!({"type": "text", "text": text_of(&p)}))),
            ("message", _) => ("user", Some(json!({"type": "text", "text": text_of(&p)}))),
            ("reasoning", _) => ("assistant", None),
            ("function_call", _) | ("custom_tool_call", _) | ("local_shell_call", _) => {
                let input = match (p.get("arguments"), p.get("input"), p.get("action")) {
                    (Some(Value::String(a)), _, _) => {
                        serde_json::from_str(a).unwrap_or_else(|_| json!({"arguments": a}))
                    }
                    (_, Some(Value::String(i)), _) => json!({"input": i}),
                    (_, _, Some(a)) => a.clone(),
                    _ => json!({}),
                };
                let id = p.get("call_id").or_else(|| p.get("id")).cloned().unwrap_or(Value::Null);
                let name = p.get("name").and_then(Value::as_str).unwrap_or("shell");
                ("assistant", Some(json!({"type": "tool_use", "id": id, "name": name, "input": input})))
            }
            ("function_call_output", _) | ("custom_tool_call_output", _) | ("local_shell_call_output", _) => {
                let out = match p.get("output") {
                    Some(Value::String(s)) => s.clone(),
                    Some(o) => o.get("content").and_then(Value::as_str).map_or_else(|| o.to_string(), str::to_string),
                    None => String::new(),
                };
                let id = p.get("call_id").cloned().unwrap_or(Value::Null);
                ("user", Some(json!({"type": "tool_result", "tool_use_id": id, "content": as_sent(out)})))
            }
            _ => continue,
        };
        if open.as_ref().map(|o| o.0) != Some(side) {
            close(&mut open, &mut done, &mut store);
            if side == "assistant" {
                whole.push(Call { now, ids: done.clone() });
            }
            open = Some((side, Vec::new()));
        }
        if let (Some(b), Some(o)) = (block, open.as_mut()) {
            o.1.push(b);
        }
    }
    if !seen_meta {
        return Ok(None);
    }
    Ok(Some(Session { store, system, whole, sent: Vec::new() }))
}

/// Writes `calls` as replay lines: each request as the messages it keeps of the one before and those it adds.
fn write(path: &Path, s: &Session, calls: &[Call]) -> std::io::Result<()> {
    let mut tools: Vec<String> = Vec::new();
    for m in &s.store.msgs {
        for p in m.get("content").and_then(Value::as_array).into_iter().flatten() {
            if let Some(name) = p.get("name").and_then(Value::as_str).filter(|_| p["type"] == "tool_use") {
                if !tools.iter().any(|t| t == name) {
                    tools.push(name.to_string());
                }
            }
        }
    }
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    let system_tokens = nexo_core::tokens::text_tokens(&s.system);
    let mut prev: &[u32] = &[];
    for (i, c) in calls.iter().enumerate() {
        let base = common(&c.ids, prev);
        let new: Vec<&Value> = c.ids[base..].iter().map(|&id| &s.store.msgs[id as usize]).collect();
        let mut req =
            json!({"op": "view", "system_tokens": system_tokens, "base": base, "new": new, "total": c.ids.len()});
        if i == 0 {
            req["tools"] = json!(tools);
            req["system"] = json!(s.system);
        }
        writeln!(w, "{}", json!({"now": c.now, "req": req}))?;
        prev = &c.ids;
    }
    w.flush()
}

/// The `*.jsonl` files under `p` (or `p` itself).
fn logs(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut all: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        all.sort();
        for q in all {
            logs(&q, out);
        }
    } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
        out.push(p.to_path_buf());
    }
}

/// A short name for the session of `p`, unique among the logs read.
fn name_of(p: &Path, kind: char) -> String {
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("session");
    let stem: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let tail = stem.len().saturating_sub(36);
    format!("{kind}-{}", &stem[tail..])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut out_dir, mut srcs, mut min, mut max_mb) = (None, Vec::new(), 3usize, 64u64);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--min" => min = it.next().and_then(|n| n.parse().ok()).unwrap_or(min),
            "--max-mb" => max_mb = it.next().and_then(|n| n.parse().ok()).unwrap_or(max_mb),
            _ if out_dir.is_none() => out_dir = Some(PathBuf::from(a)),
            _ => srcs.push(PathBuf::from(a)),
        }
    }
    let Some(out_dir) = out_dir.filter(|_| !srcs.is_empty()) else {
        eprintln!("usage: corpus OUT_DIR [--min N] [--max-mb M] SRC...");
        std::process::exit(2);
    };
    std::fs::create_dir_all(out_dir.join("sent")).expect("the output folder is made");
    let mut files = Vec::new();
    for s in &srcs {
        logs(s, &mut files);
    }
    let (mut wrote, mut short, mut big, mut other, mut holes) = (0, 0, 0, 0, 0);
    for f in &files {
        if std::fs::metadata(f).map_or(true, |m| m.len() > max_mb * 1_000_000) {
            big += 1;
            continue;
        }
        let (session, kind) = match request_log(f) {
            Ok(Some((_, gaps))) if gaps > 0 => {
                holes += 1;
                continue;
            }
            Ok(Some((s, _))) => (s, 'a'),
            _ => match item_log(f) {
                Ok(Some(s)) => (s, 'x'),
                _ => {
                    other += 1;
                    continue;
                }
            },
        };
        if session.whole.len() < min {
            short += 1;
            continue;
        }
        let name = name_of(f, kind);
        write(&out_dir.join(format!("{name}.jsonl")), &session, &session.whole).expect("a replay file is written");
        if !session.sent.is_empty() {
            write(&out_dir.join("sent").join(format!("{name}.jsonl")), &session, &session.sent)
                .expect("a replay file is written");
        }
        wrote += 1;
    }
    println!(
        "{wrote} sessions written to {}, {short} shorter than {min} requests, {big} logs over {max_mb} MB, {holes} with messages the log never wrote down, {other} of another shape",
        out_dir.display()
    );
}
