//! `nexo --wire` (or `NEXO_WIRE=<dir>`): every request to a model, as it went on the wire, kept on disk with its
//! answer, and a report that follows the session live. Off unless asked for.
//!
//! One folder per run, `<dir>/<date>-<time>-<pid>/`:
//! - `NNNN-request.json`: the body exactly as sent (pretty printed), with the URL and the headers, keys hidden;
//! - `NNNN-response.sse`: the answer's bytes as they came (or the error text);
//! - `NNNN-reply.txt`: the text the model wrote, taken from the stream;
//! - `events.jsonl`: one line per request, answer and harness event (the context view, the window fit, warnings);
//! - `report.html`: the whole run at a glance, each request split into its parts by size; it reloads itself.
//!
//! What goes in these files is the conversation itself: they are written for the owner only (0600, folder 0700 on
//! Unix). Header values that carry a key and `key`/`token` query parameters are never written.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use futures::StreamExt;
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::{json, Value};

use crate::http::BodyStream;

/// The environment variable that turns the recording on: the folder the runs go in.
pub const ENV: &str = "NEXO_WIRE";
/// Where runs go when `--wire` names no folder: `~/.nexo/wire`.
pub fn default_dir() -> PathBuf {
    nexo_core::paths::data_dir().join("wire")
}

/// The longest preview of a part kept in the report.
const PREVIEW: usize = 160;
/// The most parts of one request listed in the report; the largest ones are kept past that.
const MAX_PARTS: usize = 400;
/// How many of the newest requests the report lists part by part.
const REPORT_PARTS_OF: usize = 80;

struct Recorder {
    dir: PathBuf,
    start: Instant,
    seq: u32,
    timeline: Vec<Item>,
}

/// One line of the report's timeline.
#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Item {
    Exchange(Box<Summary>),
    Note { ms: u64, at: String, kind: String, data: Value },
}

/// A request and, once it came, its answer.
#[derive(Serialize, Clone, Default)]
struct Summary {
    n: u32,
    ms: u64,
    at: String,
    url: String,
    model: String,
    protocol: String,
    request_bytes: u64,
    /// characters / 4: the same rough count nexo's own estimate starts from
    estimated_tokens: u64,
    max_tokens: Option<u64>,
    system_bytes: u64,
    tools: usize,
    tools_bytes: u64,
    images: usize,
    parts: Vec<PartSize>,
    /// parts left out of `parts` (past `MAX_PARTS`, smallest first)
    parts_hidden: usize,
    status: Option<u16>,
    first_byte_ms: Option<u64>,
    total_ms: Option<u64>,
    response_bytes: u64,
    usage: Value,
    stop: Option<String>,
    error: Option<String>,
    done: bool,
}

/// One message of a request with what it takes.
#[derive(Serialize, Clone)]
struct PartSize {
    /// its place in the request's message list, from 1 (as nexo's own notices count)
    index: usize,
    role: String,
    kinds: Vec<String>,
    bytes: u64,
    tokens: u64,
    images: usize,
    preview: String,
}

fn recorder() -> Option<&'static Mutex<Recorder>> {
    static REC: OnceLock<Option<Mutex<Recorder>>> = OnceLock::new();
    REC.get_or_init(|| {
        let root = std::env::var_os(ENV).filter(|p| !p.is_empty()).map(PathBuf::from)?;
        match open_run(&root) {
            Ok(dir) => Some(Mutex::new(Recorder { dir, start: Instant::now(), seq: 0, timeline: Vec::new() })),
            Err(e) => {
                tracing::warn!("wire: cannot record in {}: {e}", root.display());
                None
            }
        }
    })
    .as_ref()
}

fn open_run(root: &Path) -> std::io::Result<PathBuf> {
    let stamp = now_local().format(time::macros::format_description!("[year][month][day]-[hour][minute][second]"));
    let dir = root.join(format!("{}-{}", stamp.unwrap_or_else(|_| "run".into()), std::process::id()));
    std::fs::create_dir_all(&dir)?;
    private_dir(root);
    private_dir(&dir);
    // `latest` names the newest run for `nexo wire`
    let _ = std::fs::write(root.join("latest"), dir.to_string_lossy().as_bytes());
    let rec = Recorder { dir: dir.clone(), start: Instant::now(), seq: 0, timeline: Vec::new() };
    write_report(&rec);
    Ok(dir)
}

/// Whether requests are being recorded.
pub fn on() -> bool {
    recorder().is_some()
}

/// The folder of this run's recording, when on.
pub fn run_dir() -> Option<PathBuf> {
    Some(recorder()?.lock().ok()?.dir.clone())
}

/// The newest run under `root` (what `latest` names), if it is still there.
pub fn latest(root: &Path) -> Option<PathBuf> {
    let dir = PathBuf::from(std::fs::read_to_string(root.join("latest")).ok()?.trim());
    dir.is_dir().then_some(dir)
}

/// A harness event on the timeline: what the context policy sent, how the request was fitted, a warning.
pub fn note(kind: &str, data: Value) {
    let Some(rec) = recorder() else { return };
    let Ok(mut r) = rec.lock() else { return };
    let item = Item::Note { ms: r.start.elapsed().as_millis() as u64, at: clock(), kind: kind.to_string(), data };
    append_event(&r.dir, &item);
    r.timeline.push(item);
    write_report(&r);
}

/// The recording of one request: made before it is sent, it keeps its answer as the stream is read.
pub(crate) struct Exchange {
    n: u32,
    dir: PathBuf,
    sent: Instant,
    raw: Vec<u8>,
    status: Option<u16>,
    first_byte: Option<u64>,
    finished: bool,
}

/// Records a request about to go; `None` when the recording is off.
pub(crate) fn begin(url: &str, headers: &HeaderMap, body: &Value) -> Option<Exchange> {
    let rec = recorder()?;
    let mut r = rec.lock().ok()?;
    r.seq += 1;
    let n = r.seq;
    let shown_url = redact_url(url);
    let file = json!({
        "url": shown_url,
        "method": "POST",
        "headers": redact_headers(headers),
        "body": body,
    });
    write_private(&r.dir.join(format!("{n:04}-request.json")), &serde_json::to_vec_pretty(&file).unwrap_or_default());
    let mut s = summarize(body, &shown_url);
    s.n = n;
    s.ms = r.start.elapsed().as_millis() as u64;
    s.at = clock();
    let item = Item::Exchange(Box::new(s));
    append_event(&r.dir, &item);
    r.timeline.push(item);
    write_report(&r);
    Some(Exchange {
        n,
        dir: r.dir.clone(),
        sent: Instant::now(),
        raw: Vec::new(),
        status: None,
        first_byte: None,
        finished: false,
    })
}

impl Exchange {
    /// The status of the answer, as soon as its headers came.
    pub(crate) fn status(&mut self, status: u16) {
        self.status = Some(status);
        self.first_byte = Some(self.sent.elapsed().as_millis() as u64);
    }

    /// The request failed before or instead of a stream: the error is its answer.
    pub(crate) fn fail(mut self, error: &str) {
        self.raw.extend_from_slice(error.as_bytes());
        self.finish(Some(error.to_string()));
    }

    /// `body`, kept as it is read; the answer is written when the stream ends or is dropped.
    pub(crate) fn tee(self, body: BodyStream) -> BodyStream {
        Box::pin(futures::stream::unfold((body, Some(self)), |(mut body, mut ex)| async move {
            match body.next().await {
                Some(chunk) => {
                    if let (Some(x), Ok(bytes)) = (ex.as_mut(), &chunk) {
                        x.raw.extend_from_slice(bytes);
                    }
                    if let (Err(e), Some(x)) = (&chunk, ex.take()) {
                        x.finish(Some(e.clone()));
                    }
                    Some((chunk, (body, ex)))
                }
                None => {
                    if let Some(x) = ex.take() {
                        x.finish(None);
                    }
                    None
                }
            }
        }))
    }

    fn finish(mut self, error: Option<String>) {
        self.finished = true;
        self.write(error, false);
    }

    /// `dropped`: the reader let go of the stream before its end, which nexo does once the answer said it stopped
    fn write(&mut self, error: Option<String>, dropped: bool) {
        let n = self.n;
        write_private(&self.dir.join(format!("{n:04}-response.sse")), &self.raw);
        let text = String::from_utf8_lossy(&self.raw);
        let (reply, usage, stop) = read_stream(&text);
        let error =
            error.or_else(|| (dropped && stop.is_none()).then(|| "stream ended early (interrupted)".to_string()));
        if !reply.is_empty() {
            write_private(&self.dir.join(format!("{n:04}-reply.txt")), reply.as_bytes());
        }
        let Some(rec) = recorder() else { return };
        let Ok(mut r) = rec.lock() else { return };
        let total = self.sent.elapsed().as_millis() as u64;
        let mut line = None;
        if let Some(Item::Exchange(s)) =
            r.timeline.iter_mut().rev().find(|i| matches!(i, Item::Exchange(s) if s.n == n))
        {
            s.status = self.status;
            s.first_byte_ms = self.first_byte;
            s.total_ms = Some(total);
            s.response_bytes = self.raw.len() as u64;
            s.usage = usage;
            s.stop = stop;
            s.error = error;
            s.done = true;
            line = Some(json!({"type": "response", "n": n, "status": s.status, "total_ms": total,
                "response_bytes": s.response_bytes, "usage": s.usage, "stop": s.stop, "error": s.error}));
        }
        if let Some(l) = line {
            append_line(&r.dir, &l);
        }
        write_report(&r);
    }
}

impl Drop for Exchange {
    /// A stream let go of before its end is still written, as far as it came: an error only when the answer never
    /// said it stopped (the person interrupted, a retry).
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.write(None, true);
        }
    }
}

// ---------------------------------------------------------------- what a request holds

/// The parts of a request by size, whatever the protocol: Anthropic and OpenAI `messages`, Responses `input`,
/// Gemini `contents`.
fn summarize(body: &Value, url: &str) -> Summary {
    let size = |v: &Value| serde_json::to_string(v).map_or(0, |s| s.len() as u64);
    let protocol = if body.get("contents").is_some() {
        "gemini"
    } else if body.get("input").is_some() {
        "openai-responses"
    } else if body.get("system").is_some() || body.get("anthropic_version").is_some() || url.contains("/messages") {
        "anthropic"
    } else {
        "openai-chat"
    };
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| url.split("/models/").nth(1).and_then(|m| m.split(':').next()).map(str::to_string))
        .unwrap_or_default();
    let messages = ["messages", "input", "contents"].iter().find_map(|k| body.get(*k)).and_then(Value::as_array);
    let mut system_bytes =
        ["system", "instructions", "systemInstruction"].iter().filter_map(|k| body.get(*k)).map(size).sum::<u64>();
    let mut parts: Vec<PartSize> = messages
        .map(|ms| {
            ms.iter()
                .enumerate()
                .map(|(i, m)| {
                    let bytes = size(m);
                    let role =
                        m.get("role").or_else(|| m.get("type")).and_then(Value::as_str).unwrap_or("?").to_string();
                    let mut kinds = Vec::new();
                    collect_kinds(m, &mut kinds, 0);
                    PartSize {
                        index: i + 1,
                        role,
                        kinds,
                        bytes,
                        tokens: bytes / 4,
                        images: count_images(m),
                        preview: preview(m),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // OpenAI chat carries the system prompt as the first message
    if protocol == "openai-chat" {
        system_bytes +=
            parts.iter().filter(|p| p.role == "system" || p.role == "developer").map(|p| p.bytes).sum::<u64>();
    }
    let parts_hidden = parts.len().saturating_sub(MAX_PARTS);
    if parts_hidden > 0 {
        let mut by_size: Vec<usize> = (0..parts.len()).collect();
        by_size.sort_by_key(|i| std::cmp::Reverse(parts[*i].bytes));
        let keep: std::collections::HashSet<usize> = by_size.into_iter().take(MAX_PARTS).collect();
        parts = parts.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, p)| p).collect();
    }
    let tools = body.get("tools").and_then(Value::as_array);
    let request_bytes = size(body);
    Summary {
        url: url.to_string(),
        model,
        protocol: protocol.to_string(),
        request_bytes,
        estimated_tokens: request_bytes / 4,
        max_tokens: ["max_tokens", "max_output_tokens", "max_completion_tokens"]
            .iter()
            .find_map(|k| body.get(*k))
            .or_else(|| body.pointer("/generationConfig/maxOutputTokens"))
            .and_then(Value::as_u64),
        system_bytes,
        tools: tools.map_or(0, Vec::len),
        tools_bytes: tools.map_or(0, |t| t.iter().map(size).sum()),
        images: count_images(body),
        parts,
        parts_hidden,
        usage: Value::Null,
        ..Default::default()
    }
}

/// The `type`s of the blocks a message holds, each once, in order (`text`, `tool_use`, `image`, ...).
fn collect_kinds(v: &Value, out: &mut Vec<String>, depth: usize) {
    if depth > 4 {
        return;
    }
    match v {
        Value::Object(o) => {
            if depth > 0 {
                if let Some(t) = o.get("type").and_then(Value::as_str) {
                    if !out.iter().any(|k| k == t) {
                        out.push(t.to_string());
                    }
                }
                // Gemini names a part by its one key
                for k in ["functionCall", "functionResponse", "inlineData", "text"] {
                    if o.contains_key(k) && o.get("type").is_none() && !out.iter().any(|x| x == k) {
                        out.push(k.to_string());
                    }
                }
            }
            for (k, x) in o {
                if matches!(k.as_str(), "content" | "parts" | "output" | "input") || x.is_array() {
                    collect_kinds(x, out, depth + 1);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_kinds(x, out, depth + 1)),
        _ => {}
    }
}

fn count_images(v: &Value) -> usize {
    match v {
        Value::Object(o) => {
            let here = matches!(o.get("type").and_then(Value::as_str), Some("image" | "image_url" | "input_image"))
                || o.contains_key("inlineData");
            usize::from(here) + o.values().map(count_images).sum::<usize>()
        }
        Value::Array(a) => a.iter().map(count_images).sum(),
        _ => 0,
    }
}

/// The first words a message holds: its first text, else what it calls, else its shape.
fn preview(m: &Value) -> String {
    fn first_text(v: &Value, depth: usize) -> Option<String> {
        if depth > 5 {
            return None;
        }
        match v {
            Value::String(s) if depth > 0 && !s.trim().is_empty() => Some(s.clone()),
            Value::Object(o) => {
                for k in ["text", "content", "output", "thinking"] {
                    if let Some(t) = o.get(k).and_then(|x| first_text(x, depth + 1)) {
                        return Some(t);
                    }
                }
                if let Some(name) = o.get("name").and_then(Value::as_str) {
                    return Some(format!(
                        "{name}({})",
                        o.get("input").or_else(|| o.get("arguments")).map_or(String::new(), |a| a.to_string())
                    ));
                }
                o.values().filter(|x| x.is_array() || x.is_object()).find_map(|x| first_text(x, depth + 1))
            }
            Value::Array(a) => a.iter().find_map(|x| first_text(x, depth + 1)),
            _ => None,
        }
    }
    let t = first_text(m, 0).unwrap_or_default();
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > PREVIEW {
        format!("{}…", one.chars().take(PREVIEW).collect::<String>())
    } else {
        one
    }
}

/// From a streamed answer: the text the model wrote, its usage as the provider counted it, and why it stopped.
fn read_stream(raw: &str) -> (String, Value, Option<String>) {
    let mut reply = String::new();
    let mut usage = serde_json::Map::new();
    let mut stop = None;
    let events: Vec<Value> = raw
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != "[DONE]")
        .filter_map(|d| serde_json::from_str(d).ok())
        .collect();
    // an answer that is not a stream (an error body, a plain JSON reply)
    let events = if events.is_empty() { serde_json::from_str::<Value>(raw).ok().into_iter().collect() } else { events };
    for e in &events {
        // Anthropic `content_block_delta`, Responses `response.output_text.delta`, OpenAI chat, Gemini
        if let Some(t) = e.pointer("/delta/text").and_then(Value::as_str) {
            reply.push_str(t);
        } else if e.get("type").and_then(Value::as_str) == Some("response.output_text.delta") {
            reply.push_str(e.get("delta").and_then(Value::as_str).unwrap_or(""));
        } else if let Some(t) = e.pointer("/choices/0/delta/content").and_then(Value::as_str) {
            reply.push_str(t);
        } else if let Some(ps) = e.pointer("/candidates/0/content/parts").and_then(Value::as_array) {
            for p in ps {
                if p.get("thought").and_then(Value::as_bool) != Some(true) {
                    reply.push_str(p.get("text").and_then(Value::as_str).unwrap_or(""));
                }
            }
        }
        for u in ["/usage", "/message/usage", "/response/usage", "/usageMetadata"].iter().filter_map(|p| e.pointer(p)) {
            if let Some(o) = u.as_object() {
                for (k, v) in o {
                    if !v.is_null() {
                        usage.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        for p in [
            "/delta/stop_reason",
            "/choices/0/finish_reason",
            "/candidates/0/finishReason",
            "/response/status",
            "/error/message",
        ] {
            if let Some(s) = e.pointer(p).and_then(Value::as_str) {
                stop = Some(s.to_string());
            }
        }
    }
    (reply, Value::Object(usage), stop)
}

// ---------------------------------------------------------------- keys never written

const HIDDEN: &str = "‹hidden›";

fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["auth", "key", "token", "secret", "cookie", "session", "signature", "password"].iter().any(|w| n.contains(w))
}

fn redact_headers(h: &HeaderMap) -> Value {
    let mut out = serde_json::Map::new();
    for (k, v) in h {
        let value =
            if secret_name(k.as_str()) { HIDDEN.to_string() } else { v.to_str().unwrap_or("‹binary›").to_string() };
        out.insert(k.as_str().to_string(), Value::String(value));
    }
    Value::Object(out)
}

fn redact_url(url: &str) -> String {
    // `scheme://user:password@host`: the user info goes whole
    let url = match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            match rest[..end].rfind('@') {
                Some(at) => format!("{scheme}://{HIDDEN}@{}", &rest[at + 1..]),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    };
    let Some((base, query)) = url.split_once('?') else { return url };
    let q: Vec<String> = query
        .split('&')
        .map(|p| match p.split_once('=') {
            Some((k, _)) if secret_name(k) => format!("{k}={HIDDEN}"),
            _ => p.to_string(),
        })
        .collect();
    format!("{base}?{}", q.join("&"))
}

// ---------------------------------------------------------------- files

fn clock() -> String {
    now_local()
        .format(time::macros::format_description!("[hour]:[minute]:[second].[subsecond digits:3]"))
        .unwrap_or_default()
}

fn now_local() -> time::OffsetDateTime {
    let utc = time::OffsetDateTime::from(SystemTime::now());
    time::UtcOffset::current_local_offset().map_or(utc, |o| utc.to_offset(o))
}

fn private_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn open_private(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

fn write_private(path: &Path, bytes: &[u8]) {
    if let Err(e) = open_private(path, false).and_then(|mut f| f.write_all(bytes)) {
        tracing::warn!("wire: cannot write {}: {e}", path.display());
    }
}

fn append_line(dir: &Path, v: &Value) {
    if let Ok(mut f) = open_private(&dir.join("events.jsonl"), true) {
        let _ = writeln!(f, "{v}");
    }
}

fn append_event(dir: &Path, item: &Item) {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    // the timeline keeps the parts for the report; the event line keeps what a script greps for
    if let Some(o) = v.as_object_mut() {
        o.remove("parts");
    }
    if matches!(item, Item::Exchange(_)) {
        v["type"] = "request".into();
    }
    append_line(dir, &v);
}

/// The report, written whole to a side file and moved in place: a reload never reads half of it.
fn write_report(r: &Recorder) {
    // a long session would rewrite tens of megabytes on every event: the parts of the newest requests are what
    // a person opens, older ones keep their numbers and their raw file
    let exchanges = r.timeline.iter().filter(|i| matches!(i, Item::Exchange(_))).count();
    let mut seen = 0;
    let timeline: Vec<Item> = r
        .timeline
        .iter()
        .map(|i| match i {
            Item::Exchange(s) => {
                seen += 1;
                if exchanges - seen < REPORT_PARTS_OF {
                    i.clone()
                } else {
                    let mut s = s.clone();
                    s.parts_hidden += s.parts.len();
                    s.parts.clear();
                    Item::Exchange(s)
                }
            }
            note => note.clone(),
        })
        .collect();
    let data = serde_json::to_string(&timeline).unwrap_or_else(|_| "[]".into());
    let html = crate::wire_report::page(&r.dir.to_string_lossy(), &data);
    let tmp = r.dir.join(".report.html.tmp");
    write_private(&tmp, html.as_bytes());
    let _ = std::fs::rename(&tmp, r.dir.join("report.html"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_headers_and_query_are_never_written() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer sk-live-123".parse().unwrap());
        h.insert("x-api-key", "sk-ant-456".parse().unwrap());
        h.insert("x-goog-api-key", "AIza789".parse().unwrap());
        h.insert("anthropic-version", "2023-06-01".parse().unwrap());
        let v = redact_headers(&h).to_string();
        for secret in ["sk-live-123", "sk-ant-456", "AIza789"] {
            assert!(!v.contains(secret), "{v}");
        }
        assert!(v.contains("2023-06-01"), "{v}");
        let u = redact_url("https://g.example/v1/models/m:streamGenerateContent?alt=sse&key=AIzaSECRET");
        assert_eq!(u, format!("https://g.example/v1/models/m:streamGenerateContent?alt=sse&key={HIDDEN}"));
        let u = redact_url("http://user:hunter2@127.0.0.1:9/v1/chat?key=zzz9");
        assert!(!u.contains("hunter2") && !u.contains("zzz9"), "{u}");
        assert!(u.contains("127.0.0.1:9/v1/chat"), "{u}");
        assert_eq!(redact_url("https://api.example/v1/messages"), "https://api.example/v1/messages");
    }

    #[test]
    fn an_anthropic_request_is_split_into_its_parts_with_the_largest_reply_named() {
        let body = json!({
            "model": "claude-x",
            "max_tokens": 256,
            "system": [{"type": "text", "text": "You are nexo."}],
            "tools": [{"name": "read", "input_schema": {}}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello there"},
                    {"type": "image", "source": {"type": "base64", "data": "AAAA"}}]},
                {"role": "assistant", "content": [{"type": "text", "text": "x".repeat(4000)},
                    {"type": "tool_use", "id": "c1", "name": "read", "input": {"path": "a.rs"}}]},
            ]
        });
        let s = summarize(&body, "https://api.example/v1/messages");
        assert_eq!(s.protocol, "anthropic");
        assert_eq!(s.model, "claude-x");
        assert_eq!(s.max_tokens, Some(256));
        assert_eq!(s.tools, 1);
        assert_eq!(s.images, 1);
        assert_eq!(s.parts.len(), 2);
        assert_eq!(s.parts[0].kinds, ["text", "image"]);
        assert_eq!(s.parts[0].preview, "hello there");
        assert_eq!(s.parts[1].role, "assistant");
        assert!(s.parts[1].kinds.contains(&"tool_use".to_string()));
        assert!(s.parts[1].tokens > 1000);
        assert!(s.system_bytes > 0);
    }

    #[test]
    fn gemini_and_responses_bodies_are_read_too() {
        let g = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}, {"inlineData": {"data": "AA"}}]}],
            "systemInstruction": {"parts": [{"text": "sys"}]}, "generationConfig": {"maxOutputTokens": 99}});
        let s = summarize(&g, "https://g.example/v1beta/models/gemini-9:streamGenerateContent?alt=sse");
        assert_eq!(
            (s.protocol.as_str(), s.model.as_str(), s.max_tokens, s.images),
            ("gemini", "gemini-9", Some(99), 1)
        );
        assert!(s.parts[0].kinds.contains(&"inlineData".to_string()), "{:?}", s.parts[0].kinds);
        let r = json!({"model": "gpt-x", "instructions": "sys", "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "q"}]},
            {"type": "function_call", "name": "bash", "arguments": "{\"cmd\":\"ls\"}"}]});
        let s = summarize(&r, "https://o.example/v1/responses");
        assert_eq!(s.protocol, "openai-responses");
        assert_eq!(s.parts[1].role, "function_call");
        assert!(s.parts[1].preview.starts_with("bash("), "{}", s.parts[1].preview);
    }

    #[test]
    fn a_stream_gives_back_its_text_usage_and_stop() {
        let anthropic = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":120,\"output_tokens\":1}}}\n\n\
            data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n\
            data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n\
            data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n";
        let (text, usage, stop) = read_stream(anthropic);
        assert_eq!(text, "Hello");
        assert_eq!(usage["input_tokens"], 120);
        assert_eq!(usage["output_tokens"], 7);
        assert_eq!(stop.as_deref(), Some("end_turn"));
        let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5}}\n\ndata: [DONE]\n\n";
        let (text, usage, stop) = read_stream(chat);
        assert_eq!((text.as_str(), usage["prompt_tokens"].as_u64(), stop.as_deref()), ("ok", Some(5), Some("stop")));
        let (_, _, stop) = read_stream("{\"error\":{\"message\":\"prompt is too long\"}}");
        assert_eq!(stop.as_deref(), Some("prompt is too long"));
    }

    #[test]
    fn a_huge_request_keeps_its_largest_parts_in_the_report() {
        let messages: Vec<Value> = (0..MAX_PARTS + 50)
            .map(|i| json!({"role": "user", "content": "y".repeat(if i == 3 { 9000 } else { i % 7 })}))
            .collect();
        let s = summarize(&json!({"model": "m", "messages": messages}), "https://x/v1/chat/completions");
        assert_eq!(s.parts.len(), MAX_PARTS);
        assert_eq!(s.parts_hidden, 50);
        assert!(s.parts.iter().any(|p| p.index == 4 && p.bytes > 9000));
        // the parts stay in request order
        assert!(s.parts.windows(2).all(|w| w[0].index < w[1].index));
    }
}
