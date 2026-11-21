//! The local audit log: one JSON line per tool call and per model request, appended to a file on
//! this machine. Nothing is sent anywhere. It is off unless `[audit] enabled = true`.
//!
//! Lines are handed to a background thread through an unbounded channel, so a slow disk never holds
//! a turn. The thread appends each line with a single write; the file is never rewritten.
//!
//! A tool line: `ts, kind="tool", session, cwd, user, provider, model, tool, args, decision, by,
//! status, exit_code?, duration_ms`. A request line: `ts, kind="request", session, cwd, user,
//! provider, model, call` (main, sub, reader, compact, ...), `tokens_in, tokens_out, tokens_cached,
//! cost_usd`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;

use nexo_core::config::AuditConfig;
use nexo_core::stream::Usage;
use serde_json::{json, Value};

use crate::scrub::{scrub, touches_env_file};

/// The longest args summary kept in a line.
const SUMMARY_CHARS: usize = 200;

/// The time as the log writes it: UTC, to the second.
pub fn stamp() -> String {
    let f = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    time::OffsetDateTime::now_utc().format(&f).unwrap_or_default()
}

/// Who runs nexo, as the operating system says.
fn user_name() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "unknown".into())
}

/// Where the log goes: the configured file, else `<dir>/YYYY-MM.jsonl` by the line's month (`dir` is [`default_dir`]).
fn target(cfg: &AuditConfig, dir: &Path, ts: &str) -> PathBuf {
    match configured(cfg) {
        Some(p) => expand(p),
        None => dir.join(format!("{}.jsonl", ts.get(..7).unwrap_or("unknown"))),
    }
}

pub fn default_dir() -> PathBuf {
    nexo_core::paths::data_dir().join("audit")
}

/// Opens the log for appending, making its folder. The audit log holds the commands and arguments of every tool call,
/// so when it lives in the data folder (`private`) the folder is `0700` and the file `0600`, an existing one included:
/// logs written before this had the umask rights. A file the admin pointed `path` at keeps whatever rights its owner
/// gave it; only a file made new gets the private mode.
///
/// Closing an old folder or file is hardening, not a condition of writing: when the rights cannot be changed (a folder
/// or log owned by another user, a read-only mount) the log is still written, as it was before, and one warning says so.
fn open_log(path: &Path, private: bool) -> std::io::Result<std::fs::File> {
    use nexo_core::fsmode::{create_private_dir, private_file, tighten_dir, tighten_file};
    let warn = |what: &Path, e: std::io::Error| {
        tracing::warn!("audit log: could not make {} private: {e}", what.display());
    };
    if let Some(dir) = path.parent() {
        if private {
            create_private_dir(dir)?;
            if let Err(e) = tighten_dir(dir) {
                warn(dir, e);
            }
        } else {
            std::fs::create_dir_all(dir)?;
        }
    }
    let file = private_file().create(true).append(true).open(path)?;
    if private {
        if let Err(e) = tighten_file(path) {
            warn(path, e);
        }
    }
    Ok(file)
}

/// The file the admin set in `[audit] path`, if any; a blank one counts as none.
fn configured(cfg: &AuditConfig) -> Option<&str> {
    cfg.path.as_deref().filter(|p| !p.trim().is_empty())
}

/// Whether the log lives in nexo's own data folder, which is the only place nexo sets rights on (see [`open_log`]).
fn own_folder(cfg: &AuditConfig) -> bool {
    configured(cfg).is_none()
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        Some(rest) => dirs_home().join(rest),
        None => PathBuf::from(p),
    }
}

fn dirs_home() -> PathBuf {
    nexo_core::paths::home_dir().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// What the person asked for the call: allowed, denied, or asked (the person was asked and said yes).
#[derive(Debug, Clone, Copy)]
pub struct Why {
    pub decision: &'static str,
    /// Who decided: `rule` (the mode and the rules), `hook`, or `user`.
    pub by: &'static str,
}

impl Why {
    pub const RULE: Why = Why { decision: "allowed", by: "rule" };
    pub fn denied(by: &'static str) -> Why {
        Why { decision: "denied", by }
    }
}

/// A finished tool call.
pub struct ToolRecord<'a> {
    pub session: &'a str,
    pub cwd: &'a Path,
    pub provider: &'a str,
    pub model: &'a str,
    pub tool: &'a str,
    pub args: &'a Value,
    /// What `Tool::summary` says of the call.
    pub summary: &'a str,
    pub why: Why,
    /// `ok`, `error` or `denied`.
    pub status: &'a str,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

/// A paid model request.
pub struct RequestRecord<'a> {
    pub session: &'a str,
    pub cwd: &'a Path,
    pub provider: &'a str,
    pub model: &'a str,
    pub call: &'a str,
    pub usage: Usage,
    pub cost_usd: f64,
}

struct Inner {
    user: String,
    tx: Mutex<Option<Sender<String>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

/// The writer of the log. Cheap to clone; the last clone dropped waits for the thread to finish.
#[derive(Clone)]
pub struct Audit {
    inner: std::sync::Arc<Inner>,
}

/// Every writer made in this process, so `close_all` can finish them before the process exits.
static LIVE: Mutex<Vec<std::sync::Weak<Inner>>> = Mutex::new(Vec::new());

/// Writes what every audit log has queued and ends its thread. `std::process::exit` runs no
/// destructors, so a writer a task still holds would lose its last lines; call this before it.
/// A line sent after it is dropped.
pub fn close_all() {
    let live = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    for inner in live.iter().filter_map(std::sync::Weak::upgrade) {
        inner.close();
    }
}

impl Inner {
    fn close(&self) {
        // closing the channel ends the thread once it has written what is queued
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.close();
    }
}

impl Audit {
    /// The writer when `[audit] enabled`; none (and nothing written, not even a directory) otherwise.
    pub fn from_config(cfg: &AuditConfig) -> Option<Audit> {
        Self::start(cfg, default_dir())
    }

    /// [`Self::from_config`] with the folder of the month files given, so a test does not need the data folder.
    fn start(cfg: &AuditConfig, dir: PathBuf) -> Option<Audit> {
        if !cfg.enabled {
            return None;
        }
        let (tx, rx) = channel::<String>();
        let cfg = cfg.clone();
        let thread = std::thread::Builder::new()
            .name("nexo-audit".into())
            .spawn(move || {
                let mut open: Option<(PathBuf, std::fs::File)> = None;
                let mut warned = false;
                // a log in the data folder is the user's alone; one the admin pointed somewhere else is left as set
                let private = own_folder(&cfg);
                for line in rx {
                    let ts = serde_json::from_str::<Value>(&line)
                        .ok()
                        .and_then(|v| v.get("ts").and_then(Value::as_str).map(String::from))
                        .unwrap_or_default();
                    let path = target(&cfg, &dir, &ts);
                    if open.as_ref().map(|(p, _)| p != &path).unwrap_or(true) {
                        let file = open_log(&path, private);
                        match file {
                            Ok(f) => open = Some((path.clone(), f)),
                            Err(e) => {
                                if !warned {
                                    tracing::warn!("audit log {} not writable: {e}", path.display());
                                    warned = true;
                                }
                                continue;
                            }
                        }
                    }
                    if let Some((_, f)) = open.as_mut() {
                        // one write per line, so lines of two nexo processes do not mix
                        let mut buf = line.into_bytes();
                        buf.push(b'\n');
                        let _ = f.write_all(&buf);
                    }
                }
            })
            .ok()?;
        let inner = std::sync::Arc::new(Inner {
            user: user_name(),
            tx: Mutex::new(Some(tx)),
            thread: Mutex::new(Some(thread)),
        });
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|w| w.strong_count() > 0);
        live.push(std::sync::Arc::downgrade(&inner));
        Some(Audit { inner })
    }

    fn send(&self, v: Value) {
        if let Some(tx) = self.inner.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(v.to_string());
        }
    }

    pub fn tool(&self, r: ToolRecord<'_>) {
        // a call on a .env file keeps its tool name but not its arguments
        let args = if touches_env_file(r.args) {
            "[redacted]".to_string()
        } else {
            let s = scrub(r.summary);
            let mut t: String = s.chars().take(SUMMARY_CHARS).collect();
            if s.chars().count() > SUMMARY_CHARS {
                t.push_str("...");
            }
            t
        };
        let mut v = json!({
            "ts": stamp(),
            "kind": "tool",
            "session": r.session,
            "cwd": r.cwd.display().to_string(),
            "user": self.inner.user,
            "provider": r.provider,
            "model": r.model,
            "tool": r.tool,
            "args": args,
            "decision": r.why.decision,
            "by": r.why.by,
            "status": r.status,
            "duration_ms": r.duration_ms,
        });
        if let Some(c) = r.exit_code {
            v["exit_code"] = json!(c);
        }
        self.send(v);
    }

    pub fn request(&self, r: RequestRecord<'_>) {
        self.send(json!({
            "ts": stamp(),
            "kind": "request",
            "session": r.session,
            "cwd": r.cwd.display().to_string(),
            "user": self.inner.user,
            "provider": r.provider,
            "model": r.model,
            "call": r.call,
            "tokens_in": r.usage.input + r.usage.cache_write,
            "tokens_out": r.usage.output,
            "tokens_cached": r.usage.cache_read,
            "cost_usd": r.cost_usd,
        }));
    }
}

/// How a report groups the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    User,
    Model,
    Day,
}

impl By {
    pub fn parse(s: &str) -> Option<By> {
        match s {
            "user" => Some(By::User),
            "model" => Some(By::Model),
            "day" => Some(By::Day),
            _ => None,
        }
    }
}

/// One row of a report.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Row {
    pub key: String,
    pub calls: u64,
    pub requests: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub tokens_cached: u64,
    pub cost_usd: f64,
}

/// A cutoff such as `7d`, `12h`, `30m` or a date `2026-11-01`, as a log time stamp.
pub fn since_stamp(s: &str, now: time::OffsetDateTime) -> Result<String, String> {
    let f = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    let s = s.trim();
    let bad = || format!("cannot read --since {s:?}; use 7d, 12h, 30m or a date like 2026-11-01");
    if let Some(unit) = s.chars().last().filter(|c| matches!(c, 'd' | 'h' | 'm' | 'w')) {
        let n: i64 = s[..s.len() - 1].parse().map_err(|_| bad())?;
        let secs = n * match unit {
            'w' => 604_800,
            'd' => 86_400,
            'h' => 3_600,
            _ => 60,
        };
        return (now - time::Duration::seconds(secs)).format(&f).map_err(|_| bad());
    }
    if s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.bytes().filter(u8::is_ascii_digit).count() == 8
    {
        return Ok(format!("{s}T00:00:00Z"));
    }
    Err(bad())
}

/// Sums the lines of `files` newer than `since` (a stamp) into rows by `by`, sorted by cost.
/// Lines that do not parse are skipped. Tool calls count under the model that was in use.
pub fn report(files: &[PathBuf], since: Option<&str>, by: By) -> Vec<Row> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let ts = v.get("ts").and_then(Value::as_str).unwrap_or("");
            if since.is_some_and(|s| ts < s) {
                continue;
            }
            let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("unknown").to_string();
            let key = match by {
                By::User => get("user"),
                By::Model => format!("{}/{}", get("provider"), get("model")),
                By::Day => ts.get(..10).unwrap_or("unknown").to_string(),
            };
            let row = rows.entry(key.clone()).or_insert_with(|| Row { key, ..Row::default() });
            let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            match v.get("kind").and_then(Value::as_str) {
                Some("tool") => row.calls += 1,
                Some("request") => {
                    row.requests += 1;
                    row.tokens_in += n("tokens_in");
                    row.tokens_out += n("tokens_out");
                    row.tokens_cached += n("tokens_cached");
                    row.cost_usd += v.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                }
                _ => {}
            }
        }
    }
    let mut rows: Vec<Row> = rows.into_values().collect();
    if by == By::Day {
        rows.sort_by(|a, b| a.key.cmp(&b.key));
    } else {
        rows.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd).then(a.key.cmp(&b.key)));
    }
    rows
}

/// The log files to read: the configured file, else every month file in the default folder.
pub fn files(cfg: &AuditConfig) -> Vec<PathBuf> {
    if let Some(p) = configured(cfg) {
        return vec![expand(p)];
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(default_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn the_default_audit_log_and_its_folder_are_the_users_alone() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("audit/2026-11.jsonl");
        drop(open_log(&path, true).unwrap());
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn an_audit_log_made_with_the_umask_is_tightened_when_it_is_opened() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("audit");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("2026-11.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(open_log(&path, true).unwrap());
        assert_eq!((mode(&dir), mode(&path)), (0o700, 0o600));
        // the lines already in it stay
        assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_log_the_admin_pointed_elsewhere_keeps_the_rights_its_owner_set() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = d.path().join("shared.jsonl");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        drop(open_log(&path, false).unwrap());
        assert_eq!((mode(d.path()), mode(&path)), (0o755, 0o640));
    }

    /// One request line through a writer started the way `from_config` starts it, finished before it returns.
    #[cfg(unix)]
    fn write_one_line(cfg: &AuditConfig, dir: PathBuf) {
        let a = Audit::start(cfg, dir).unwrap();
        let cwd = Path::new("/");
        a.request(RequestRecord {
            session: "s",
            cwd,
            provider: "p",
            model: "m",
            call: "main",
            usage: Usage::default(),
            cost_usd: 0.0,
        });
        // the last clone dropped waits for the thread, so the line and the rights are in place after it
        drop(a);
    }

    #[cfg(unix)]
    #[test]
    fn a_writer_without_a_path_closes_the_data_folder_and_one_with_a_path_leaves_it() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        // no path, or a blank one: the month file goes in the folder nexo owns
        for (i, path) in [None, Some("  ".to_string())].into_iter().enumerate() {
            let dir = d.path().join(format!("audit{i}"));
            write_one_line(&AuditConfig { enabled: true, path }, dir.clone());
            let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
            assert_eq!(files.len(), 1, "{files:?}");
            assert_eq!((mode(&dir), mode(&files[0])), (0o700, 0o600));
        }
        // a path of the admin's: the folder and a file that exist keep their rights, and the line is appended
        let shared = d.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = shared.join("audit.jsonl");
        std::fs::write(&file, b"").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let cfg = AuditConfig { enabled: true, path: Some(file.display().to_string()) };
        write_one_line(&cfg, d.path().join("never-made"));
        assert_eq!((mode(&shared), mode(&file)), (0o755, 0o640));
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        assert!(!d.path().join("never-made").exists());
    }

    #[test]
    fn close_all_writes_the_queued_lines_of_a_writer_still_held() {
        // close_all closes every writer in the process, including those in parallel tests.
        // Exercise it in a separate test process so it cannot close their writers.
        const CHILD: &str = "NEXO_AUDIT_CLOSE_ALL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "audit::tests::close_all_writes_the_queued_lines_of_a_writer_still_held"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "isolated close_all test failed: {status}");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.jsonl");
        let cfg = AuditConfig { enabled: true, path: Some(f.display().to_string()) };
        let a = Audit::from_config(&cfg).unwrap();
        // a task the process never drops: `exit` would not run its destructor
        let held = a.clone();
        let usage = Usage::default();
        let r =
            |call| RequestRecord { session: "s", cwd: d.path(), provider: "p", model: "m", call, usage, cost_usd: 0.0 };
        a.request(r("main"));
        drop(a);
        close_all();
        let text = std::fs::read_to_string(&f).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        // after it, a line is dropped, not left half written
        held.request(r("late"));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text);
    }
    #[test]
    fn since_reads_spans_and_dates() {
        let now = time::macros::datetime!(2026-11-07 12:00:00 UTC);
        assert_eq!(since_stamp("7d", now).unwrap(), "2026-10-31T12:00:00Z");
        assert_eq!(since_stamp("2h", now).unwrap(), "2026-11-07T10:00:00Z");
        assert_eq!(since_stamp("2026-11-01", now).unwrap(), "2026-11-01T00:00:00Z");
        assert!(since_stamp("soon", now).is_err());
    }

    #[test]
    fn report_sums_by_key() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.jsonl");
        let lines = [
            json!({"ts":"2026-11-06T10:00:00Z","kind":"tool","user":"ana","provider":"p","model":"m"}),
            json!({"ts":"2026-11-06T10:00:01Z","kind":"request","user":"ana","provider":"p","model":"m","tokens_in":10,"tokens_out":5,"tokens_cached":2,"cost_usd":0.5}),
            json!({"ts":"2026-11-07T10:00:00Z","kind":"request","user":"bob","provider":"p","model":"n","tokens_in":1,"tokens_out":1,"tokens_cached":0,"cost_usd":0.25}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\nnot json\n")).collect();
        std::fs::write(&f, text).unwrap();
        let rows = report(std::slice::from_ref(&f), None, By::User);
        assert_eq!((rows[0].key.as_str(), rows[0].calls, rows[0].requests, rows[0].cost_usd), ("ana", 1, 1, 0.5));
        let days = report(std::slice::from_ref(&f), None, By::Day);
        assert_eq!(days.len(), 2);
        let late = report(&[f], Some("2026-11-07T00:00:00Z"), By::Model);
        assert_eq!(late.len(), 1);
        assert_eq!(late[0].key, "p/n");
    }
}
