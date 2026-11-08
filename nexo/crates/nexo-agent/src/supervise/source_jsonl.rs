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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn the_program_is_named_by_the_hidden_folder_the_logs_sit_in() {
        let name = |p: &str| JsonlSource::new(p, 12).program();
        assert_eq!(name("/home/u/.othertool/projects"), "othertool");
        assert_eq!(name("/data/logs"), "external");
        assert_eq!(name("/home/u/projects"), "external");
    }

    fn append(path: &Path, lines: &[Value]) {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf, JsonlSource) {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("-work-app");
        std::fs::create_dir_all(&proj).unwrap();
        let log = proj.join("sess-1.jsonl");
        let src = JsonlSource::new(dir.path(), 12);
        (dir, log, src)
    }

    fn now() -> Secs {
        super::super::run::now_secs()
    }

    fn assistant_tool(id: &str, name: &str, input: Value, ts: &str) -> Value {
        json!({"type":"assistant","timestamp":ts,"cwd":"/work/app","message":{"stop_reason":"tool_use",
            "content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
    }

    fn kinds(obs: &[Observation]) -> Vec<ObsKind> {
        obs.iter().map(|o| o.kind.clone()).collect()
    }

    #[test]
    fn it_reads_what_grows_and_only_that() {
        let (_d, log, mut src) = setup();
        append(&log, &[json!({"type":"custom-title","customTitle":"Builder"})]);
        // the first poll reads what is there
        let first = src.poll(now());
        assert!(kinds(&first).iter().any(|k| matches!(k, ObsKind::Meta { name: Some(n), .. } if n == "Builder")));
        assert!(first.iter().all(|o| o.agent_key == "sess-1"));
        assert!(src.poll(now()).is_empty(), "nothing new, nothing returned");
        append(
            &log,
            &[
                json!({"type":"permission-mode","permissionMode":"bypassPermissions"}),
                assistant_tool("t1", "Bash", json!({"command":"cargo test"}), "2026-11-02T11:19:28.322Z"),
            ],
        );
        let k = kinds(&src.poll(now()));
        assert!(k.contains(&ObsKind::ToolPending { name: "Bash".into() }));
        assert!(k.contains(&ObsKind::Step { text: "Bash  cargo test".into() }));
        assert!(k
            .iter()
            .any(|k| matches!(k, ObsKind::Meta { permission_mode: Some(m), .. } if m == "bypassPermissions")));
        append(
            &log,
            &[json!({"type":"user","timestamp":"2026-11-02T11:19:40Z","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"boom"}]}})],
        );
        let k = kinds(&src.poll(now()));
        // a failed test run is an event of its own, then the result
        assert_eq!(k.len(), 2, "{k:?}");
        assert!(matches!(&k[0], ObsKind::Ev(e) if e.kind == EvKind::TestsFail));
        assert!(matches!(&k[1], ObsKind::ToolResult { name, is_error: true, .. } if name == "Bash"));
    }

    #[test]
    fn a_split_line_waits_for_its_end() {
        let (_d, log, mut src) = setup();
        append(&log, &[json!({"type":"progress","timestamp":"2026-11-02T11:00:00Z"})]);
        assert_eq!(src.poll(now()).len(), 1);
        let line = json!({"type":"user","timestamp":"2026-11-02T11:05:00Z","message":{"role":"user","content":"hi"}})
            .to_string();
        let (a, b) = line.split_at(20);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        write!(f, "{a}").unwrap();
        assert!(src.poll(now() + 6).is_empty(), "half a line is not read");
        writeln!(f, "{b}").unwrap();
        let obs = src.poll(now() + 6);
        assert_eq!(kinds(&obs), vec![ObsKind::UserInput]);
        assert_eq!(obs[0].at, 1_793_617_500);
    }

    #[test]
    fn a_shrunk_file_is_read_again_from_its_last_compaction() {
        let (_d, log, mut src) = setup();
        let old: Vec<Value> = (0..30)
            .map(|i| {
                assistant_tool(
                    &format!("o{i}"),
                    "Read",
                    json!({"file_path": format!("/old/{i}")}),
                    "2026-11-02T10:00:00Z",
                )
            })
            .collect();
        append(&log, &old);
        assert!(!src.poll(now()).is_empty());
        // rewritten shorter: a mark, then two new records
        std::fs::write(&log, "").unwrap();
        append(
            &log,
            &[
                assistant_tool("x", "Read", json!({"file_path":"/before-mark"}), "2026-11-02T11:00:00Z"),
                json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-11-02T11:01:00Z"}),
                assistant_tool("n1", "Edit", json!({"file_path":"/after-mark"}), "2026-11-02T11:02:00Z"),
            ],
        );
        // the shrink may need a second poll only when the new file is not shorter; here it is
        let obs = src.poll(now());
        let steps: Vec<String> = kinds(&obs)
            .into_iter()
            .filter_map(|k| if let ObsKind::Step { text } = k { Some(text) } else { None })
            .collect();
        assert_eq!(steps, vec!["Edit  /after-mark".to_string()], "{obs:?}");
    }

    #[test]
    fn sidechains_and_machine_text_are_activity_and_a_person_clears_the_turn() {
        let (_d, log, mut src) = setup();
        append(
            &log,
            &[
                json!({"type":"assistant","isSidechain":true,"timestamp":"2026-11-02T11:00:00Z","message":{"content":[]}}),
                json!({"type":"user","origin":{"kind":"task-notification"},"timestamp":"2026-11-02T11:00:01Z","message":{"content":"x"}}),
                json!({"type":"user","origin":{"kind":"human"},"timestamp":"2026-11-02T11:00:02Z","message":{"content":"hello"}}),
                json!({"type":"assistant","timestamp":"2026-11-02T11:00:03Z","message":{"stop_reason":"end_turn",
                    "content":[{"type":"text","text":"All done. Shall I continue?"}]}}),
            ],
        );
        let k = kinds(&src.poll(now()));
        assert_eq!(k[0], ObsKind::Activity);
        assert_eq!(k[1], ObsKind::Activity);
        assert_eq!(k[2], ObsKind::UserInput);
        assert!(k.contains(&ObsKind::TurnEnd { last_text_tail: "All done. Shall I continue?".into() }));
    }

    #[test]
    fn a_scheduled_task_marks_a_loop_and_error_records_count() {
        let (_d, log, mut src) = setup();
        append(
            &log,
            &[
                json!({"type":"system","subtype":"scheduled_task_fire","cron":"7,22,37,52 * * * *","timestamp":"2026-11-02T11:00:00Z"}),
                json!({"type":"system","subtype":"api_error","level":"error","timestamp":"2026-11-02T11:00:01Z"}),
            ],
        );
        let k = kinds(&src.poll(now()));
        assert!(k.contains(&ObsKind::Loop { cron_secs: 900 }));
        assert!(k.contains(&ObsKind::SystemError));
    }

    fn wrapper(from: &str, name: &str, body: &str) -> String {
        format!("<cross-session-message from=\"{from}\" from-name=\"{name}\" from-mode=\"ask\">{body}</cross-session-message>")
    }

    #[test]
    fn a_message_of_another_session_is_a_mail_also_beside_a_tool_result() {
        let (_d, log, mut src) = setup();
        append(&log, &[assistant_tool("t1", "Bash", json!({"command":"ls"}), "2026-11-02T11:00:00Z")]);
        src.poll(now());
        append(
            &log,
            &[
                // the wrapper sits next to the result of a tool in one record
                json!({"type":"user","timestamp":"2026-11-02T11:00:05Z","message":{"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"t1","content":"ok"},
                    {"type":"text","text": wrapper("pid-77", "api", "the build is red\nsecond line")}]}}),
                // and alone, as a string: not the person
                json!({"type":"user","timestamp":"2026-11-02T11:00:09Z","message":{"role":"user","content":
                    wrapper("pid-78", "web", "ping")}}),
            ],
        );
        let obs = src.poll(now());
        let k = kinds(&obs);
        assert!(
            k.contains(&ObsKind::MsgIn {
                from: "api".into(),
                mail: None,
                summary: "the build is red".into(),
                held: false
            }),
            "{k:?}"
        );
        assert!(k.contains(&ObsKind::MsgIn { from: "web".into(), mail: None, summary: "ping".into(), held: false }));
        assert!(
            k.iter().any(|k| matches!(k, ObsKind::ToolResult { name, .. } if name == "Bash")),
            "the result still counts"
        );
        assert!(!k.contains(&ObsKind::UserInput), "a mail is not the person");
    }

    #[test]
    fn a_send_message_call_is_a_mail_out_and_secrets_stay_out_of_its_summary() {
        let (_d, log, mut src) = setup();
        let key = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx");
        append(
            &log,
            &[assistant_tool(
                "t1",
                "SendMessage",
                json!({"to":"api","summary":format!("use {key} now"),"message":"x".repeat(300)}),
                "2026-11-02T11:00:00Z",
            )],
        );
        let k = kinds(&src.poll(now()));
        let out = k.iter().find_map(|k| {
            if let ObsKind::MsgOut { to, summary, bytes, .. } = k {
                Some((to, summary, bytes))
            } else {
                None
            }
        });
        let (to, summary, bytes) = out.expect("a mail out");
        assert_eq!((to.as_str(), *bytes), ("api", 300));
        assert!(!summary.contains(&key), "{summary}");
    }

    #[test]
    fn tests_commits_todo_and_compaction_become_events() {
        let (_d, log, mut src) = setup();
        let result = |id: &str, text: &str, err: bool, ts: &str| {
            json!({"type":"user","timestamp":ts,"message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":id,"is_error":err,"content":text}]}})
        };
        append(
            &log,
            &[
                assistant_tool("a", "Bash", json!({"command":"cargo test -p x"}), "2026-11-02T11:00:00Z"),
                result("a", "test result: ok. 12 passed; 0 failed", false, "2026-11-02T11:00:01Z"),
                assistant_tool("b", "Bash", json!({"command":"git commit -m wip"}), "2026-11-02T11:00:02Z"),
                result("b", "[main 1234] wip", false, "2026-11-02T11:00:03Z"),
                assistant_tool("c", "Bash", json!({"command":"pytest"}), "2026-11-02T11:00:04Z"),
                result("c", "3 failed, 4 passed", true, "2026-11-02T11:00:05Z"),
                assistant_tool(
                    "d",
                    "TodoWrite",
                    json!({"todos":[{"status":"completed"},{"status":"pending"},{"status":"completed"}]}),
                    "2026-11-02T11:00:06Z",
                ),
                json!({"type":"system","subtype":"compact_boundary","timestamp":"2026-11-02T11:00:07Z"}),
            ],
        );
        let evs: Vec<Ev> = src
            .poll(now())
            .into_iter()
            .filter_map(|o| if let ObsKind::Ev(e) = o.kind { Some(e) } else { None })
            .collect();
        let want = [
            (EvKind::TestsPass, EvDetail::Tests { pass: 12, fail: 0 }),
            (EvKind::GitCommit, EvDetail::None),
            (EvKind::TestsFail, EvDetail::Tests { pass: 4, fail: 3 }),
            (EvKind::Todo, EvDetail::Progress { done: 2, total: 3 }),
            (EvKind::Compact, EvDetail::None),
        ];
        let got: Vec<(EvKind, EvDetail)> = evs.into_iter().map(|e| (e.kind, e.detail)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn cron_periods() {
        assert_eq!(cron_period("*/10 * * * *"), 600);
        assert_eq!(cron_period("7,22,37,52 * * * *"), 900);
        assert_eq!(cron_period("41 0,5,10,15,20 * * *"), 5 * 3600);
        assert_eq!(cron_period("nonsense"), 3600);
    }

    #[test]
    fn old_logs_and_other_files_are_ignored_and_a_big_log_is_read_from_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("notes.txt"), "x").unwrap();
        let big = proj.join("big.jsonl");
        let mut body = String::new();
        for i in 0..6000 {
            body.push_str(&format!("{}\n", json!({"type":"progress","n":i,"pad":"x".repeat(60)})));
        }
        std::fs::write(&big, body).unwrap();
        let mut src = JsonlSource::new(dir.path(), 12);
        let obs = src.poll(now());
        assert!(!obs.is_empty() && obs.len() < 6000, "{}", obs.len());
        // far in the future the file is older than the window and is never picked up
        let mut late = JsonlSource::new(dir.path(), 12);
        assert!(late.poll(now() + 13 * 3600).is_empty());
    }

    fn steps(obs: &[Observation]) -> Vec<String> {
        kinds(obs).into_iter().filter_map(|k| if let ObsKind::Step { text } = k { Some(text) } else { None }).collect()
    }

    #[test]
    fn a_half_written_last_line_and_a_call_without_result_leave_the_state_as_it_was() {
        let (_d, log, mut src) = setup();
        append(&log, &[assistant_tool("t1", "Bash", json!({"command":"cargo build"}), "2026-11-02T11:00:00Z")]);
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        // the writer is in the middle of the next record, and of its string
        write!(f, "{{\"type\":\"user\",\"timestamp\":\"2026-11-02T11:00:09Z\",\"message\":{{\"content\":[{{\"type\":\"tool_res").unwrap();
        let k = kinds(&src.poll(now()));
        assert!(k.contains(&ObsKind::ToolPending { name: "Bash".into() }));
        assert!(!k.iter().any(|k| matches!(k, ObsKind::ToolResult { .. } | ObsKind::UserInput)), "{k:?}");
        // nothing more for as long as the line is open, however often we look
        for _ in 0..3 {
            assert!(src.poll(now() + 6).is_empty());
        }
        // a line that ends but is not JSON is skipped, the next one still reads
        writeln!(f, "ult\" this is broken").unwrap();
        append(&log, &[json!({"type":"progress","timestamp":"2026-11-02T11:00:10Z"})]);
        assert_eq!(kinds(&src.poll(now() + 6)), vec![ObsKind::Activity]);
    }

    #[test]
    fn a_log_replaced_by_a_longer_one_is_read_from_its_new_start() {
        let (d, log, mut src) = setup();
        append(&log, &[assistant_tool("a", "Read", json!({"file_path":"/old"}), "2026-11-02T11:00:00Z")]);
        assert_eq!(steps(&src.poll(now())), vec!["Read  /old".to_string()]);
        // a new file takes the path: larger than the old one was, so a length check alone would miss it
        let fresh = d.path().join("fresh.tmp");
        let lines: Vec<Value> = (0..5)
            .map(|i| {
                assistant_tool(
                    &format!("n{i}"),
                    "Edit",
                    json!({"file_path": format!("/new/{i}")}),
                    "2026-11-02T12:00:00Z",
                )
            })
            .collect();
        append(&fresh, &lines);
        std::fs::rename(&fresh, &log).unwrap();
        let got = steps(&src.poll(now()));
        assert_eq!(got.len(), 5, "{got:?}");
        assert_eq!(got[0], "Edit  /new/0");
    }

    #[test]
    fn a_rotated_big_log_is_read_from_its_end_not_whole() {
        let (d, log, mut src) = setup();
        append(&log, &[json!({"type":"progress"})]);
        src.poll(now());
        let fresh = d.path().join("fresh.tmp");
        let mut body = String::new();
        for i in 0..8000 {
            body.push_str(&format!(
                "{}\n",
                assistant_tool(
                    &format!("t{i}"),
                    "Read",
                    json!({"file_path": format!("/f/{i}")}),
                    "2026-11-02T12:00:00Z"
                )
            ));
        }
        assert!(body.len() as u64 > 2 * CHUNK);
        std::fs::write(&fresh, body).unwrap();
        std::fs::rename(&fresh, &log).unwrap();
        let got = steps(&src.poll(now()));
        assert!(!got.is_empty() && got.len() < 8000, "{}", got.len());
        assert_eq!(got.last().unwrap(), "Read  /f/7999");
    }

    #[test]
    fn a_record_dated_in_the_future_counts_as_now() {
        let (_d, log, mut src) = setup();
        append(&log, &[assistant_tool("t1", "Bash", json!({"command":"ls"}), "2099-01-01T00:00:00Z")]);
        let t = now();
        let obs = src.poll(t);
        assert!(!obs.is_empty() && obs.iter().all(|o| o.at <= t), "{obs:?}");
    }

    #[test]
    fn a_log_nobody_touched_for_the_whole_window_is_let_go() {
        let (_d, log, mut src) = setup();
        append(&log, &[json!({"type":"progress"})]);
        src.poll(now());
        assert_eq!(src.files.len(), 1);
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 3600);
        std::fs::File::options().write(true).open(&log).unwrap().set_modified(old).unwrap();
        src.poll(now() + 10);
        assert!(src.files.is_empty());
        // and a file that is deleted is let go too
        append(&log, &[json!({"type":"progress"})]);
        src.poll(now() + 20);
        assert_eq!(src.files.len(), 1);
        std::fs::remove_file(&log).unwrap();
        src.poll(now() + 21);
        assert!(src.files.is_empty());
    }

    #[test]
    fn what_a_step_shows_is_short_plain_and_free_of_secrets() {
        let (_d, log, mut src) = setup();
        let gh = format!("{}{}", "gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");
        let sk = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx");
        append(
            &log,
            &[
                assistant_tool(
                    "a",
                    "Bash",
                    json!({"command": format!("git clone https://{gh}@example.test/r.git")}),
                    "2026-11-02T11:00:00Z",
                ),
                assistant_tool(
                    "b",
                    "Bash",
                    json!({"command": "curl -H 'Authorization: Bearer abcdef0123456789xyz' https://example.test"}),
                    "2026-11-02T11:00:01Z",
                ),
                assistant_tool("c", "Bash", json!({"command": "{\"a\":1,\"b\":[2,3]}"}), "2026-11-02T11:00:02Z"),
                assistant_tool(
                    "d",
                    "Bash\u{7}\nweird",
                    json!({"command": format!("export KEY={sk}")}),
                    "2026-11-02T11:00:03Z",
                ),
                assistant_tool("e", "Read", json!({"file_path": "x".repeat(900)}), "2026-11-02T11:00:04Z"),
                json!({"type":"assistant","timestamp":"2026-11-02T11:00:05Z","message":{"stop_reason":"end_turn",
                    "content":[{"type":"text","text":format!("{{\"status\":\"ok\",\"token\":\"{sk}\"}}")}]}}),
            ],
        );
        let obs = src.poll(now());
        let got = steps(&obs);
        assert_eq!(got.len(), 5, "{got:?}");
        for s in &got {
            assert!(!s.contains(&gh) && !s.contains(&sk) && !s.contains("abcdef0123456789xyz"), "{s}");
            assert!(!s.chars().any(char::is_control), "{s:?}");
            assert!(s.chars().count() <= 200, "{}", s.chars().count());
        }
        assert_eq!(got[2], "Bash", "a JSON document is not a step: {:?}", got[2]);
        assert!(got[3].starts_with("Bash weird"), "{:?}", got[3]);
        // the end of the turn keeps no key, and no raw JSON as its step
        let tail = kinds(&obs).into_iter().find_map(|k| {
            if let ObsKind::TurnEnd { last_text_tail } = k {
                Some(last_text_tail)
            } else {
                None
            }
        });
        assert!(!tail.unwrap().contains(&sk));
    }

    #[test]
    fn a_secret_cut_by_the_tail_does_not_leave_half_a_key() {
        let (_d, log, mut src) = setup();
        let sk = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ");
        // the key starts 20 characters before the 300 character cut
        let text = format!("{} {sk}{}", "w".repeat(10), "z".repeat(TAIL - 20 - 8));
        append(
            &log,
            &[json!({"type":"assistant","timestamp":"2026-11-02T11:00:05Z","message":{"stop_reason":"end_turn",
                "content":[{"type":"text","text":text}]}})],
        );
        let tail = kinds(&src.poll(now()))
            .into_iter()
            .find_map(|k| if let ObsKind::TurnEnd { last_text_tail } = k { Some(last_text_tail) } else { None })
            .unwrap();
        assert!(!tail.contains("abcdefghij") && !tail.contains("k-abc"), "{tail}");
    }

    #[test]
    fn a_cron_line_made_of_nonsense_does_not_panic() {
        for c in [
            "5,70 * * * *",
            "99,1 * * * *",
            "* 30,40 * * *",
            "*/0 * * * *",
            "*/99999999999999999999 * * * *",
            ",, * * * *",
            "1,2,3,4,5,6,7,8,9 99999999999 * * *",
        ] {
            let _ = cron_period(c);
        }
    }

    #[test]
    fn many_sessions_and_a_huge_log_stay_cheap() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("p");
        std::fs::create_dir_all(&proj).unwrap();
        // 200 sessions with a few hundred records each
        for i in 0..200 {
            let lines: Vec<Value> = (0..150)
                .map(|n| {
                    assistant_tool(
                        &format!("t{n}"),
                        "Read",
                        json!({"file_path": format!("/s{i}/f{n}")}),
                        "2026-11-02T11:00:00Z",
                    )
                })
                .collect();
            append(&proj.join(format!("s{i}.jsonl")), &lines);
        }
        // and one log of 100 MB
        let big = proj.join("big.jsonl");
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&big).unwrap());
            let block: String = (0..400)
                .map(|n| {
                    format!(
                        "{}\n",
                        assistant_tool(
                            &format!("b{n}"),
                            "Edit",
                            json!({"file_path": format!("/big/{n}")}),
                            "2026-11-02T11:00:00Z"
                        )
                    )
                })
                .collect();
            let mut written = 0usize;
            while written < 100 * 1024 * 1024 {
                f.write_all(block.as_bytes()).unwrap();
                written += block.len();
            }
        }
        let mut src = JsonlSource::new(dir.path(), 12);
        let t = Instant::now();
        let first = src.poll(now());
        let first_took = t.elapsed();
        assert!(first.len() > 200 * 150, "{}", first.len());
        // nothing happens: a poll only looks at the files' sizes
        let t = Instant::now();
        for _ in 0..5 {
            assert!(src.poll(now()).is_empty());
        }
        let quiet = t.elapsed();
        // one line lands on the big log: the cost is that line, not the file
        append(&big, &[assistant_tool("last", "Edit", json!({"file_path":"/big/last"}), "2026-11-02T11:00:01Z")]);
        let t = Instant::now();
        let got = steps(&src.poll(now()));
        let tail_took = t.elapsed();
        assert_eq!(got, vec!["Edit  /big/last".to_string()]);
        // generous bounds, for a loaded machine and a build without optimisation
        assert!(first_took.as_secs_f64() < 30.0, "first {first_took:?}");
        assert!(quiet.as_secs_f64() < 2.0, "quiet {quiet:?}");
        assert!(tail_took.as_secs_f64() < 1.0, "tail {tail_took:?}");
    }
}
