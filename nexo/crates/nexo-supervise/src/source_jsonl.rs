//! The JSONL log adapter: tails the session logs of an external program, one JSON record per line, in
//! `<root>/<project>/<session>.jsonl`. Read-only: it opens files for reading, keeps an offset per file
//! and returns only what is new.
//!
//! Bounds: at most [`CHUNK`] bytes per file per poll; a file seen for the first time is read from its
//! last [`CHUNK`] bytes; a line that is not finished yet is left for the next poll; a file that shrinks
//! is read again from the start and only what follows its last compaction mark is kept.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::events::{after_command, Ev, EvDetail, EvKind};
use super::source::{is_raw_json, one_line, tail, tool_name, tool_step, Source, SourceKind, EXTERNAL};
use super::{input_hash, ObsKind, Observation, Secs};

#[cfg(test)]
mod tests;

/// The most bytes read from one file in one poll.
pub const CHUNK: u64 = 256 * 1024;
/// How often the folder is listed for new files, in seconds.
const RESCAN: Secs = 5;
/// Characters of a reply kept as its tail.
const TAIL: usize = 300;
/// Observations kept while a shrunk file is read again.
const RESYNC_KEEP: usize = 4000;
const TOOL_KEEP: usize = 512;

#[derive(Default)]
struct FileState {
    key: String,
    offset: u64,
    /// Drop bytes up to the next newline (the read began inside a line).
    skip_line: bool,
    /// Tool calls waiting for their result: id to (name, hash of the input).
    tools: HashMap<String, (String, u64)>,
    /// Shell commands waiting for their result: id to command.
    cmds: HashMap<String, String>,
    cwd_sent: bool,
    /// Which file the path held when last looked at.
    id: Option<(u64, u64)>,
    /// Set while a shrunk file is read again: what is kept so far.
    resync: Option<Vec<Observation>>,
}

pub struct JsonlSource {
    root: PathBuf,
    window: Secs,
    files: HashMap<PathBuf, FileState>,
    scanned: Option<Secs>,
}

impl JsonlSource {
    /// Watch `root` for logs touched in the last `scan_hours`.
    pub fn new(root: impl Into<PathBuf>, scan_hours: u64) -> JsonlSource {
        JsonlSource { root: root.into(), window: scan_hours as Secs * 3600, files: HashMap::new(), scanned: None }
    }

    fn scan(&mut self, now: Secs) {
        // a log nobody touched for the whole window, or a file that is gone, is let go
        let window = self.window;
        self.files.retain(|p, _| std::fs::metadata(p).is_ok_and(|m| m.is_file() && now - mtime(&m) <= window));
        let Ok(dirs) = std::fs::read_dir(&self.root) else { return };
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") || self.files.contains_key(&path) {
                    continue;
                }
                let Ok(meta) = f.metadata() else { continue };
                if !meta.is_file() || now - mtime(&meta) > self.window {
                    continue;
                }
                let key = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let len = meta.len();
                let offset = len.saturating_sub(CHUNK);
                self.files.insert(
                    path,
                    FileState { key, offset, skip_line: offset > 0, id: file_id(&meta), ..Default::default() },
                );
            }
        }
    }
}

/// The program a log folder belongs to: `<home>/.<name>/projects` is `<name>`; any other place is not named.
fn program_of_root(root: &Path) -> String {
    let home = if root.file_name().and_then(|n| n.to_str()) == Some("projects") { root.parent() } else { None };
    home.and_then(|h| h.file_name())
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix('.'))
        .filter(|n| !n.is_empty())
        .map_or_else(|| EXTERNAL.to_string(), str::to_string)
}

fn mtime(meta: &std::fs::Metadata) -> Secs {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as Secs)
        .unwrap_or(0)
}

impl Source for JsonlSource {
    fn id(&self) -> &str {
        "jsonl-log"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::External
    }

    fn program(&self) -> String {
        program_of_root(&self.root)
    }

    fn poll(&mut self, now: Secs) -> Vec<Observation> {
        if !self.scanned.is_some_and(|t| now - t < RESCAN) {
            self.scan(now);
            self.scanned = Some(now);
        }
        let mut out = Vec::new();
        let mut gone = Vec::new();
        for (path, st) in self.files.iter_mut() {
            match read_new(path, st, now) {
                Some(obs) => out.extend(obs),
                None => gone.push(path.clone()),
            }
        }
        for p in gone {
            self.files.remove(&p);
        }
        out
    }
}

/// Which file a path holds, to tell a log that was replaced from one that grew. None where the platform
/// cannot say.
#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// New observations of one file; None when the file is gone.
fn read_new(path: &Path, st: &mut FileState, now: Secs) -> Option<Vec<Observation>> {
    // a look at the directory entry is enough to see that nothing changed; the file is opened only to read
    let meta = std::fs::metadata(path).ok()?;
    let len = meta.len();
    let id = file_id(&meta);
    let replaced = st.id.is_some() && id.is_some() && st.id != id;
    st.id = id;
    if len < st.offset || replaced {
        // shrunk or replaced (rotated): read again, from the last chunk only when the new file is big
        st.offset = len.saturating_sub(CHUNK);
        st.skip_line = st.offset > 0;
        st.cmds.clear();
        st.tools.clear();
        st.resync = Some(Vec::new());
    }
    if len == st.offset {
        return Some(st.resync.take().unwrap_or_default());
    }
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(st.offset)).ok()?;
    let mut buf = Vec::new();
    file.take(CHUNK).read_to_end(&mut buf).ok()?;
    let full = buf.len() as u64 == CHUNK;
    let mut start = 0;
    if st.skip_line {
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => {
                start = i + 1;
                st.skip_line = false;
            }
            None => {
                // a line longer than a chunk: skip it whole
                if full {
                    st.offset += buf.len() as u64;
                }
                return Some(Vec::new());
            }
        }
    }
    let end = match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) if i + 1 > start => i + 1,
        _ => {
            if start > 0 {
                // the cut line is behind us and the next one is not finished: do not read the cut one again
                st.offset += start as u64;
            } else if full {
                // no line end in a whole chunk: drop this line
                st.offset += buf.len() as u64;
                st.skip_line = true;
            }
            return Some(Vec::new());
        }
    };
    st.offset += end as u64;
    let mut fresh = Vec::new();
    for line in buf[start..end].split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let (obs, compact) = convert(&v, st, now);
        if compact && st.resync.is_some() {
            fresh.clear();
            if let Some(r) = st.resync.as_mut() {
                r.clear();
            }
        }
        fresh.extend(obs);
    }
    match st.resync.as_mut() {
        Some(keep) => {
            keep.extend(fresh);
            if keep.len() > RESYNC_KEEP {
                keep.drain(..keep.len() - RESYNC_KEEP);
            }
            if st.offset >= len {
                Some(st.resync.take().unwrap_or_default())
            } else {
                Some(Vec::new())
            }
        }
        None => Some(fresh),
    }
}

/// Characters of a message kept as the summary of a mail.
const MAIL_SUMMARY: usize = 60;

/// What a `SendMessage` call says it sends: to whom, a summary, how many bytes.
fn send_message(input: &Value) -> ObsKind {
    let to = input.get("to").and_then(Value::as_str).unwrap_or("").to_string();
    let body = input.get("message").or_else(|| input.get("text")).and_then(Value::as_str).unwrap_or("");
    let summary = match input.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        Some(s) => one_line(s, MAIL_SUMMARY),
        None => one_line(body, MAIL_SUMMARY),
    };
    ObsKind::MsgOut { to, mail: None, summary, bytes: body.len() }
}

/// Items done and in all, from the input of a `TodoWrite` call.
fn todo_progress(input: &Value) -> Option<EvDetail> {
    let items = input.get("todos")?.as_array()?;
    let done = items.iter().filter(|i| i.get("status").and_then(Value::as_str) == Some("completed")).count();
    Some(EvDetail::Progress {
        done: done.min(u16::MAX as usize) as u16,
        total: items.len().min(u16::MAX as usize) as u16,
    })
}

/// The text blocks of a user record (the whole content when it is a string).
fn user_texts(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// The text of a tool result (a string, or the text blocks of a list).
fn result_text(r: &Value) -> String {
    match r.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// The value of `attr="..."` in a tag, when the attribute stands alone (`from=` is not `from-name=`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!(" {name}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    Some(rest[..rest.find('"')?].to_string())
}

/// The messages of other sessions in a piece of text: who wrote (the name, else the id) and what.
fn cross_messages(text: &str) -> Vec<(String, String)> {
    const OPEN: &str = "<cross-session-message";
    const CLOSE: &str = "</cross-session-message>";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i..];
        let Some(end) = after.find('>') else { break };
        let tag = &after[..end];
        let from = attr(tag, "from-name").filter(|n| !n.is_empty()).or_else(|| attr(tag, "from")).unwrap_or_default();
        let body = &after[end + 1..];
        let (body, next) = match body.find(CLOSE) {
            Some(c) => (&body[..c], &body[c + CLOSE.len()..]),
            None => (body, ""),
        };
        out.push((from, body.trim().to_string()));
        rest = next;
    }
    out
}

/// Timestamp of a record in seconds, when it has one.
fn stamp(v: &Value) -> Option<Secs> {
    let s = v.get("timestamp")?.as_str()?;
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some(t.unix_timestamp())
}

/// The period of a cron line in seconds: the longest gap between two runs of its minute and hour lists.
pub fn cron_period(cron: &str) -> u64 {
    let f: Vec<&str> = cron.split_whitespace().collect();
    if f.len() < 2 {
        return 3600;
    }
    let gap = |field: &str, unit: u64, span: u64| -> Option<u64> {
        if field == "*" {
            return Some(unit);
        }
        if let Some(n) = field.strip_prefix("*/") {
            return n.parse::<u64>().ok().filter(|n| *n > 0).map(|n| n.saturating_mul(unit));
        }
        let mut v: Vec<u64> = field.split(',').filter_map(|x| x.parse::<u64>().ok()).filter(|x| *x < span).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        let first = v[0];
        let wrap = (first + span).saturating_sub(v[v.len() - 1]);
        let widest = v.windows(2).map(|w| w[1] - w[0]).chain(std::iter::once(wrap)).max().unwrap_or(span);
        Some(widest.saturating_mul(unit))
    };
    // minute, hour: when the hour is free the minutes decide, else the hours do
    let hour_free = f[1] == "*";
    if hour_free {
        gap(f[0], 60, 60).unwrap_or(3600)
    } else {
        gap(f[1], 3600, 24).unwrap_or(86400)
    }
}

/// One record as observations, and whether it is a compaction mark.
fn convert(v: &Value, st: &mut FileState, now: Secs) -> (Vec<Observation>, bool) {
    // a clock ahead of ours is not "the future": the record is from now at the latest
    let at = stamp(v).map_or(now, |t| t.min(now));
    let key = st.key.clone();
    let mk = |kind: ObsKind| Observation { agent_key: key.clone(), at, kind };
    let mut out = Vec::new();
    if !st.cwd_sent {
        if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
            st.cwd_sent = true;
            out.push(mk(ObsKind::Meta { name: None, cwd: Some(cwd.to_string()), permission_mode: None }));
        }
    }
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let side = v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
    let mut compact = false;
    match kind {
        "custom-title" | "agent-name" => {
            let name = v.get(if kind == "custom-title" { "customTitle" } else { "agentName" }).and_then(Value::as_str);
            if let Some(n) = name.filter(|n| !n.is_empty()) {
                out.push(mk(ObsKind::Meta { name: Some(one_line(n, 60)), cwd: None, permission_mode: None }));
            }
        }
        "permission-mode" => {
            if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
                out.push(mk(ObsKind::Meta { name: None, cwd: None, permission_mode: Some(m.to_string()) }));
            }
        }
        "assistant" if side => out.push(mk(ObsKind::Activity)),
        "assistant" => {
            let msg = v.get("message");
            let blocks = msg.and_then(|m| m.get("content")).and_then(Value::as_array);
            let mut last_text = String::new();
            let mut any = false;
            for b in blocks.into_iter().flatten() {
                match b.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let name = tool_name(b.get("name").and_then(Value::as_str).unwrap_or("tool"));
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        if let Some(id) = b.get("id").and_then(Value::as_str) {
                            if st.tools.len() >= TOOL_KEEP {
                                st.tools.clear();
                            }
                            st.tools.insert(id.to_string(), (name.clone(), input_hash(&input.to_string())));
                        }
                        if name == "SendMessage" {
                            out.push(mk(send_message(&input)));
                        }
                        if let Some(c) = input.get("command").and_then(Value::as_str) {
                            if let Some(id) = b.get("id").and_then(Value::as_str) {
                                if st.cmds.len() >= TOOL_KEEP {
                                    st.cmds.clear();
                                }
                                st.cmds.insert(id.to_string(), c.to_string());
                            }
                        }
                        if name == "TodoWrite" {
                            if let Some(p) = todo_progress(&input) {
                                out.push(mk(ObsKind::Ev(Ev::with(at, EvKind::Todo, p))));
                            }
                        }
                        if matches!(name.as_str(), "Agent" | "Task") {
                            out.push(mk(ObsKind::Ev(Ev::new(at, EvKind::SubSpawn))));
                        }
                        out.push(mk(ObsKind::Step { text: tool_step(&name, &input) }));
                        out.push(mk(ObsKind::ToolPending { name }));
                        any = true;
                    }
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            last_text = t.to_string();
                        }
                    }
                    _ => {}
                }
            }
            let stop = msg.and_then(|m| m.get("stop_reason")).and_then(Value::as_str);
            if stop == Some("end_turn") {
                if !last_text.is_empty() && !is_raw_json(&last_text) {
                    out.push(mk(ObsKind::Step { text: one_line(&last_text, super::source::STEP_CHARS) }));
                }
                let t = tail(&nexo_core::redact::redact(&last_text), TAIL);
                out.push(mk(ObsKind::TurnEnd { last_text_tail: t }));
            } else if !any {
                out.push(mk(ObsKind::Activity));
            }
        }
        "user" if side => out.push(mk(ObsKind::Activity)),
        "user" => {
            let content = v.get("message").and_then(|m| m.get("content"));
            let results: Vec<&Value> = content
                .and_then(Value::as_array)
                .map(|a| a.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")).collect())
                .unwrap_or_default();
            // text of the record, also beside tool results: another session may have written there
            let texts = user_texts(content);
            let mails: Vec<(String, String)> = texts.iter().flat_map(|t| cross_messages(t)).collect();
            for (from, body) in &mails {
                out.push(mk(ObsKind::MsgIn {
                    from: from.clone(),
                    mail: None,
                    summary: one_line(body, MAIL_SUMMARY),
                    held: false,
                }));
            }
            if !results.is_empty() {
                for r in results {
                    let id = r.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let (name, hash) = st.tools.remove(id).unwrap_or_else(|| ("tool".to_string(), 0));
                    let is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    if matches!(name.as_str(), "Agent" | "Task") {
                        out.push(mk(ObsKind::Ev(Ev::new(at, EvKind::SubDone))));
                    }
                    if let Some(cmd) = st.cmds.remove(id) {
                        if let Some(ev) = after_command(&cmd, &result_text(r), is_error, at) {
                            out.push(mk(ObsKind::Ev(ev)));
                        }
                    }
                    out.push(mk(ObsKind::ToolResult { name, input_hash: hash, is_error }));
                }
            } else if !mails.is_empty() {
                out.push(mk(ObsKind::Activity));
            } else {
                let meta = v.get("isMeta").and_then(Value::as_bool).unwrap_or(false);
                let origin = v.get("origin").and_then(|o| o.get("kind")).and_then(Value::as_str);
                let person = !meta && !origin.is_some_and(|o| o != "human");
                out.push(mk(if person { ObsKind::UserInput } else { ObsKind::Activity }));
            }
        }
        "system" => {
            let sub = v.get("subtype").and_then(Value::as_str).unwrap_or("");
            if sub == "compact_boundary" {
                compact = true;
                out.push(mk(ObsKind::Ev(Ev::new(at, EvKind::Compact))));
            }
            if sub == "scheduled_task_fire" {
                let cron = v.get("cron").and_then(Value::as_str).unwrap_or("");
                out.push(mk(ObsKind::Loop { cron_secs: cron_period(cron) }));
            }
            if v.get("level").and_then(Value::as_str) == Some("error") {
                let low = v.to_string().to_ascii_lowercase();
                if low.contains("rate limit")
                    || low.contains("rate_limit")
                    || low.contains("429")
                    || low.contains("overloaded")
                {
                    out.push(mk(ObsKind::Ev(Ev::new(at, EvKind::RateLimit))));
                }
                out.push(mk(ObsKind::SystemError));
            } else {
                out.push(mk(ObsKind::Activity));
            }
        }
        "attachment" | "progress" => out.push(mk(ObsKind::Activity)),
        _ => {}
    }
    (out, compact)
}
