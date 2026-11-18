//! The "watch a value and alert" half of `nexo api`: `--select` (print only some values of a JSON answer), `--when`
//! (a condition on them), `--state` (the last picked values between runs), `--on-change` (a command for people).
//!
//! What this file keeps and prints is only what was picked, after `exchange::scrub_json` has hidden every secret field
//! and every echo of one. The state file holds the select list and the picked values, never a header or a body.
//! `--on-change` gets the values only through environment variables and stdin, never inside the command text.

use nexo_api::channel::Msg;
use nexo_api::exchange;
use nexo_api::select::{self, Path, Picked};
use nexo_api::when::{Moved, When};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Exit code of a `watch` whose condition came true (or whose selected values changed). The other codes of `watch`
/// and `check` are 0 ok, 1 a call failed or drift, 2 bad arguments, 3 an unreadable stream or an unusable map.
pub const EXIT_ALERT: i32 = 4;
/// How long an `--on-change` command may run before it is killed.
pub const ON_CHANGE_TIMEOUT: Duration = Duration::from_secs(60);
/// How many moved values an alert line names.
const ALERT_NAMES: usize = 5;

/// What `--select`, `--when`, `--on-change` and `--state` ask for.
pub struct Sel {
    /// The paths picked and compared: the `--select` ones, then the ones `--when` reads that were not named.
    pub paths: Vec<Path>,
    /// The texts of those paths: what the state file is checked against.
    pub names: Vec<String>,
    pub when: Option<When>,
    pub on_change: Option<String>,
    pub state: Option<PathBuf>,
}

/// One poll, worked out.
pub struct Step {
    pub picked: Picked,
    /// What the poll prints: the values on the first poll, then one line per change (nothing when none).
    pub text: String,
    /// Why a value could not be compared (to stderr).
    pub notes: Vec<String>,
    pub alert: Option<Alert>,
}

/// The alert of a poll: its one line, and the JSON that `--on-change` gets.
pub struct Alert {
    pub line: String,
    pub json: Value,
}

impl Sel {
    /// `None` when none of `--select`, `--when`, `--on-change` is given (the plain watch). An error is a usage error.
    pub fn build(
        select_args: &[String],
        when: Option<&str>,
        on_change: Option<&str>,
        state: Option<&std::path::Path>,
    ) -> Result<Option<Sel>, String> {
        if select_args.is_empty() && when.is_none() && on_change.is_none() {
            return Ok(None);
        }
        let mut paths = select::parse_list(select_args)?;
        let when = when.map(When::parse).transpose()?;
        if let Some(w) = &when {
            for p in w.paths() {
                if !paths.iter().any(|x| x.text == p.text) {
                    paths.push(p);
                }
            }
        }
        if paths.is_empty() {
            return Err("--on-change needs --select or --when: it runs when the picked values change".to_string());
        }
        if let Some(c) = on_change {
            if c.trim().is_empty() {
                return Err("--on-change is empty".to_string());
            }
        }
        let names = paths.iter().map(|p| p.text.clone()).collect();
        Ok(Some(Sel { paths, names, when, on_change: on_change.map(str::to_string), state: state.map(PathBuf::from) }))
    }

    /// What the state file holds, when there is one for this select list.
    pub fn load(&self) -> Option<Picked> {
        let path = self.state.as_ref()?;
        select::read_state(&std::fs::read_to_string(path).ok()?, &self.names)
    }

    /// Writes the state (owner only, through a temporary file and a rename).
    pub fn save(&self, picked: &Picked) -> Result<(), String> {
        let Some(path) = &self.state else { return Ok(()) };
        crate::api_call::write_private(path, &select::state_text(&self.names, picked))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Works out one poll of `doc` (already scrubbed) against the picks of the last one.
    pub fn step(&self, doc: &Value, prev: Option<&Picked>) -> Step {
        let picked = select::pick(doc, &self.paths);
        let diffs = prev.map(|p| select::changes(p, &picked)).unwrap_or_default();
        let mut text = String::new();
        match prev {
            None => text.push_str(&select::lines(&picked)),
            Some(_) => {
                for (path, was, now) in &diffs {
                    text.push_str(&format!("~ {path}: {} -> {}\n", select::show(was), select::show(now)));
                }
            }
        }
        let (fired, notes, moved) = match &self.when {
            Some(w) => {
                let (hit, notes) = w.eval(doc, prev);
                (hit, notes, w.moved(&picked, prev))
            }
            None => {
                let moved: Vec<Moved> = diffs
                    .iter()
                    .map(|(p, was, now)| Moved { path: p.clone(), now: now.clone(), prev: was.clone() })
                    .collect();
                (prev.is_some() && !moved.is_empty(), Vec::new(), moved)
            }
        };
        let alert = fired.then(|| self.alert(&moved));
        Step { picked, text, notes, alert }
    }

    fn alert(&self, moved: &[Moved]) -> Alert {
        let mut names: Vec<String> = moved
            .iter()
            .take(ALERT_NAMES)
            .map(|m| {
                let was = if m.prev.is_some() { format!(" (was {})", select::show(&m.prev)) } else { String::new() };
                format!("{}={}{was}", m.path, select::show(&m.now))
            })
            .collect();
        if moved.len() > ALERT_NAMES {
            names.push(format!("... {} more", moved.len() - ALERT_NAMES));
        }
        let cause = match &self.when {
            Some(w) => format!("when {}", w.text),
            None => "the selected values changed".to_string(),
        };
        let line = format!("alert: {} | {cause}", names.join(", "));
        let values: Vec<Value> =
            moved.iter().map(|m| json!({ "path": m.path, "now": m.now, "prev": m.prev })).collect();
        let json = json!({ "alert": true, "when": self.when.as_ref().map(|w| w.text.clone()), "line": line, "values": values });
        Alert { line, json }
    }
}

/// Runs the `--on-change` command through the platform shell and says how it ended. The alert and the picked values
/// reach it as `NEXO_API_ALERT` (JSON), `NEXO_API_PATHS` (JSON object, path to value) and on stdin (the alert JSON, one
/// line); the command text itself is never touched. It is killed after [`ON_CHANGE_TIMEOUT`].
pub fn run_on_change(cmd: &str, alert: &Value, picked: &Picked) -> String {
    run_on_change_within(cmd, alert, picked, ON_CHANGE_TIMEOUT)
}

pub fn run_on_change_within(cmd: &str, alert: &Value, picked: &Picked, limit: Duration) -> String {
    let alert_text = alert.to_string();
    let mut c = shell(cmd);
    c.env("NEXO_API_ALERT", &alert_text)
        .env("NEXO_API_PATHS", select::object(picked).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return format!("on-change: could not run the command: {e}"),
    };
    if let Some(mut stdin) = child.stdin.take() {
        // a command that never reads must not hold the write (and the timeout) back
        let line = format!("{alert_text}\n");
        std::thread::spawn(move || {
            let _ = stdin.write_all(line.as_bytes());
        });
    }
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.code() {
                    Some(code) => format!("on-change: exit {code}"),
                    None => "on-change: stopped by a signal".to_string(),
                };
            }
            Ok(None) => {
                if started.elapsed() >= limit {
                    let _ = child.kill();
                    let _ = child.wait();
                    return format!("on-change: killed after {}s (timeout)", limit.as_secs());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return format!("on-change: could not wait for the command: {e}"),
        }
    }
}

#[cfg(windows)]
fn shell(cmd: &str) -> std::process::Command {
    let mut c = std::process::Command::new("cmd");
    c.arg("/C").arg(cmd);
    c
}

#[cfg(not(windows))]
fn shell(cmd: &str) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.arg("-c").arg(cmd);
    c
}

/// What `call --select` prints: `path<TAB>value` lines, or with `as_json` an object. Unless `reveal`, secrets in the
/// answer are hidden first, as everywhere else.
pub fn select_text(body: &str, select_args: &[String], as_json: bool, reveal: bool) -> Result<String, String> {
    let paths = select::parse_list(select_args)?;
    let mut doc: Value = serde_json::from_str(body)
        .map_err(|_| "the answer is not JSON, so --select has nothing to pick".to_string())?;
    if !reveal {
        exchange::scrub_json(&mut doc);
    }
    let picked = select::pick(&doc, &paths);
    if as_json {
        Ok(format!("{}\n", serde_json::to_string_pretty(&select::object(&picked)).unwrap_or_default()))
    } else {
        Ok(select::lines(&picked))
    }
}

/// The `--on-change` run a channel read leaves to its caller (it blocks, so it must not run on the async path).
pub struct Pending {
    /// Where in the text the result line goes: right after the alert line.
    at: usize,
    alert: Value,
    picked: Picked,
}

/// `watch --select ...` over a live channel (event stream or WebSocket): the messages the server sent, in order, each
/// read as JSON; a message with none of the picked values is skipped, the others are polls. Returns what to print, the
/// exit code ([`EXIT_ALERT`] at the first alert) and the `--on-change` command still to run, if any.
pub fn channel_poll(sel: &Sel, msgs: &[Msg]) -> (String, i32, Option<Pending>) {
    let mut prev = sel.load();
    let mut out = String::new();
    let mut ticks = 0usize;
    let mut code = 0;
    let mut pending = None;
    let mut last: Option<Picked> = None;
    for m in msgs.iter().filter(|m| !m.send && m.opcode == 1) {
        let Ok(mut doc) = serde_json::from_str::<Value>(m.text.trim()) else { continue };
        exchange::scrub_json(&mut doc);
        if select::pick(&doc, &sel.paths).iter().all(|(_, v)| v.is_none()) {
            continue;
        }
        ticks += 1;
        let step = sel.step(&doc, prev.as_ref());
        out.push_str(&step.text);
        for n in &step.notes {
            out.push_str(&format!("note: {n}\n"));
        }
        if let Some(a) = &step.alert {
            out.push_str(&format!("{}\n", a.line));
            if sel.on_change.is_some() {
                pending = Some(Pending { at: out.len(), alert: a.json.clone(), picked: step.picked.clone() });
            }
            code = EXIT_ALERT;
        }
        prev = Some(step.picked.clone());
        last = Some(step.picked);
        if code != 0 {
            break;
        }
    }
    if ticks == 0 {
        out.push_str("no message had the selected values\n");
    }
    if let Some(p) = last {
        if let Err(e) = sel.save(&p) {
            out.push_str(&format!("note: {e}\n"));
        }
    }
    (out, code, pending)
}

/// [`channel_poll`], then the `--on-change` command on a blocking thread, its result line placed after the alert.
pub async fn channel_report(sel: &Sel, msgs: &[Msg]) -> (String, i32) {
    let (mut out, code, pending) = channel_poll(sel, msgs);
    if let (Some(cmd), Some(p)) = (sel.on_change.clone(), pending) {
        let at = p.at;
        let (alert, picked) = (p.alert, p.picked);
        let r = tokio::task::spawn_blocking(move || run_on_change(&cmd, &alert, &picked))
            .await
            .unwrap_or_else(|e| format!("on-change: {e}"));
        out.insert_str(at, &format!("{r}\n"));
    }
    (out, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(select: &[&str], when: Option<&str>) -> Sel {
        let args: Vec<String> = select.iter().map(|s| s.to_string()).collect();
        Sel::build(&args, when, None, None).unwrap().unwrap()
    }

    #[test]
    fn select_on_a_call_answer_prints_lines_or_an_object() {
        let body = r#"{"price": 12.5, "items": [{"price": 3}, {"price": 4}], "access_token": "tkAAAA1111bbbbCCCC"}"#;
        let args = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let t = select_text(body, &args(&["$.price,$.items[*].price", "$.nope"]), false, false).unwrap();
        assert_eq!(t, "$.price\t12.5\n$.items[0].price\t3\n$.items[1].price\t4\n$.nope\t(none)\n");
        let j = select_text(body, &args(&["$.price", "$.nope"]), true, false).unwrap();
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v, json!({"$.price": 12.5, "$.nope": null}));
        let s = select_text(body, &args(&["$.access_token"]), false, false).unwrap();
        assert_eq!(s, "$.access_token\t\"<secret>\"\n");
        assert!(select_text("not json", &args(&["$.a"]), false, false).is_err());
        assert!(select_text(body, &args(&["price"]), false, false).is_err());
    }

    #[test]
    fn only_the_selected_values_decide_a_change() {
        let s = sel(&["$.price"], None);
        let first = s.step(&json!({"price": 10, "views": 1}), None);
        assert_eq!(first.text, "$.price\t10\n");
        assert!(first.alert.is_none());
        let same = s.step(&json!({"price": 10, "views": 2}), Some(&first.picked));
        assert!(same.text.is_empty() && same.alert.is_none());
        let moved = s.step(&json!({"price": 9, "views": 2}), Some(&first.picked));
        assert_eq!(moved.text, "~ $.price: 10 -> 9\n");
        let a = moved.alert.unwrap();
        assert_eq!(a.line, "alert: $.price=9 (was 10) | the selected values changed");
    }

    #[test]
    fn when_decides_the_alert_and_the_line_names_the_values() {
        let s = sel(&[], Some("$.price < prev"));
        assert_eq!(s.names, ["$.price"]);
        let first = s.step(&json!({"price": 99}), None);
        assert!(first.alert.is_none());
        let up = s.step(&json!({"price": 120}), Some(&first.picked));
        assert!(up.alert.is_none() && up.text == "~ $.price: 99 -> 120\n");
        let drop = s.step(&json!({"price": 89}), Some(&first.picked));
        let a = drop.alert.unwrap();
        assert_eq!(a.line, "alert: $.price=89 (was 99) | when $.price < prev");
        assert_eq!(a.json["values"][0], json!({"path": "$.price", "now": 89, "prev": 99}));
        assert_eq!(a.json["when"], "$.price < prev");
    }

    #[test]
    fn the_exit_code_is_not_one_watch_or_check_already_use() {
        // watch: 0, 1 (a call failed, drift), 2 (usage), 3 (stream); check: 0 to 3
        assert!(EXIT_ALERT > 3);
        assert_eq!(EXIT_ALERT, 4);
    }

    #[test]
    fn the_state_round_trips_through_the_file_and_holds_no_secret() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.json");
        let args = vec!["$.price".to_string(), "$.access_token".to_string()];
        let s = Sel::build(&args, None, None, Some(&file)).unwrap().unwrap();
        assert_eq!(s.load(), None);
        let mut doc = json!({"price": 5, "access_token": "tkAAAA1111bbbbCCCC", "other": "tkAAAA1111bbbbCCCC"});
        exchange::scrub_json(&mut doc);
        let step = s.step(&doc, None);
        s.save(&step.picked).unwrap();
        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert!(!on_disk.contains("tkAAAA1111bbbbCCCC") && on_disk.contains("<secret>"), "{on_disk}");
        assert!(!on_disk.contains("other"), "{on_disk}");
        assert!(!std::path::Path::new(&format!("{}.tmp", file.display())).exists());
        assert_eq!(s.load(), Some(step.picked.clone()));
        // a one-shot run: the second run sees the first run's values
        let again = s.step(&json!({"price": 4, "access_token": "x"}), s.load().as_ref());
        assert!(again.text.contains("~ $.price: 5 -> 4"), "{}", again.text);
        // another select list does not read this state
        let other = Sel::build(&["$.price".to_string()], None, None, Some(&file)).unwrap().unwrap();
        assert_eq!(other.load(), None);
    }

    #[test]
    fn build_refuses_what_it_cannot_run() {
        assert!(Sel::build(&[], None, None, None).unwrap().is_none());
        assert!(Sel::build(&[], None, Some("echo hi"), None).is_err());
        assert!(Sel::build(&[], Some("$.a <"), None, None).is_err());
        assert!(Sel::build(&["nope".to_string()], None, None, None).is_err());
        assert!(Sel::build(&["$.a".to_string()], None, Some("  "), None).is_err());
    }

    #[test]
    fn a_channel_with_select_compares_only_the_picked_values() {
        let msg = |t: &str| Msg { send: false, time: None, opcode: 1, text: t.to_string(), event: None };
        let s = sel(&["$.price"], Some("$.price < prev"));
        let msgs = [
            msg(r#"{"type":"hello"}"#),
            msg(r#"{"type":"tick","price":10,"volume":1}"#),
            msg(r#"{"type":"tick","price":10,"volume":2}"#),
            msg(r#"{"type":"tick","price":8,"volume":3}"#),
            msg(r#"{"type":"tick","price":1,"volume":4}"#),
        ];
        let (text, code, _) = channel_poll(&s, &msgs);
        assert_eq!(code, EXIT_ALERT, "{text}");
        assert_eq!(text, "$.price\t10\n~ $.price: 10 -> 8\nalert: $.price=8 (was 10) | when $.price < prev\n");
        let (none, code, _) = channel_poll(&s, &msgs[..1]);
        assert_eq!(code, 0);
        assert_eq!(none, "no message had the selected values\n");
    }

    #[cfg(unix)]
    #[test]
    fn on_change_gets_values_by_environment_and_stdin_only() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.txt");
        let alert = json!({"alert": true, "line": "x; touch /tmp/never"});
        let picked: Picked = vec![("$.price".to_string(), Some(json!(7)))];
        let cmd =
            format!("printf '%s|%s|' \"$NEXO_API_PATHS\" \"$NEXO_API_ALERT\" > '{0}'; cat >> '{0}'", out.display());
        assert_eq!(run_on_change(&cmd, &alert, &picked), "on-change: exit 0");
        let got = std::fs::read_to_string(&out).unwrap();
        assert!(got.starts_with("{\"$.price\":7}|{") && got.contains("touch /tmp/never"), "{got}");
        assert!(got.trim_end().ends_with('}'), "{got}");
        assert_eq!(run_on_change("exit 3", &alert, &picked), "on-change: exit 3");
        let slow = run_on_change_within("sleep 5", &alert, &picked, Duration::from_millis(200));
        assert!(slow.contains("killed") && slow.contains("timeout"), "{slow}");
    }
}
