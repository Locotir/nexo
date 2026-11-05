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

use super::source::{one_line, tail, tool_step, Source, SourceKind, EXTERNAL};
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
    cwd_sent: bool,
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
                self.files.insert(path, FileState { key, offset, skip_line: offset > 0, ..Default::default() });
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

/// New observations of one file; None when the file is gone.
fn read_new(path: &Path, st: &mut FileState, now: Secs) -> Option<Vec<Observation>> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < st.offset {
        st.offset = 0;
        st.skip_line = false;
        st.tools.clear();
        st.resync = Some(Vec::new());
    }
    if len == st.offset {
        return Some(st.resync.take().unwrap_or_default());
    }
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
            if full {
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
            return n.parse::<u64>().ok().filter(|n| *n > 0).map(|n| n * unit);
        }
        let mut v: Vec<u64> = field.split(',').filter_map(|x| x.parse().ok()).collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        let first = v[0];
        let wrap = first + span - v[v.len() - 1];
        let widest = v.windows(2).map(|w| w[1] - w[0]).chain(std::iter::once(wrap)).max().unwrap_or(span);
        Some(widest * unit)
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
    let at = stamp(v).unwrap_or(now);
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
                out.push(mk(ObsKind::Meta { name: Some(n.to_string()), cwd: None, permission_mode: None }));
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
                        let name = b.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        if let Some(id) = b.get("id").and_then(Value::as_str) {
                            if st.tools.len() >= TOOL_KEEP {
                                st.tools.clear();
                            }
                            st.tools.insert(id.to_string(), (name.clone(), input_hash(&input.to_string())));
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
                if !last_text.is_empty() {
                    out.push(mk(ObsKind::Step { text: one_line(&last_text, super::source::STEP_CHARS) }));
                }
                let t = nexo_core::redact::redact(&tail(&last_text, TAIL)).into_owned();
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
            if !results.is_empty() {
                for r in results {
                    let id = r.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    let (name, hash) = st.tools.remove(id).unwrap_or_else(|| ("tool".to_string(), 0));
                    let is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    out.push(mk(ObsKind::ToolResult { name, input_hash: hash, is_error }));
                }
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
            }
            if sub == "scheduled_task_fire" {
                let cron = v.get("cron").and_then(Value::as_str).unwrap_or("");
                out.push(mk(ObsKind::Loop { cron_secs: cron_period(cron) }));
            }
            if v.get("level").and_then(Value::as_str) == Some("error") {
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
        assert_eq!(k.len(), 1);
        assert!(matches!(&k[0], ObsKind::ToolResult { name, is_error: true, .. } if name == "Bash"));
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
}
