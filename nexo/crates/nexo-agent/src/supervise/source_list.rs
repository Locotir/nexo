//! The external agents list adapter: runs a program that prints the running sessions of another tool as a JSON
//! array (`id`, `sessionId`, `name`, `cwd`, `pid`, `state`) and turns each entry into observations under the
//! session's key, so it joins what the JSONL log adapter reads. When the program is not installed the adapter
//! turns itself off without a word. It only runs the program and reads its output.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use super::source::{one_line, Source, SourceKind, EXTERNAL};
use super::{ObsKind, Observation, Reported, Secs};

/// The program gets this long to answer.
const PATIENCE: Duration = Duration::from_secs(3);

pub struct ListSource {
    program: String,
    args: Vec<String>,
    every: Secs,
    last: Option<Secs>,
    off: bool,
    /// The sessions the last good answer listed.
    known: HashSet<String>,
}

impl ListSource {
    pub fn new(program: impl Into<String>, args: Vec<String>, every: Secs) -> ListSource {
        ListSource { program: program.into(), args, every, last: None, off: false, known: HashSet::new() }
    }

    /// False once the program turned out not to exist.
    pub fn available(&self) -> bool {
        !self.off
    }

    fn run(&mut self) -> Option<String> {
        let spawned = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::PermissionDenied {
                    self.off = true;
                }
                return None;
            }
        };
        let mut stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stdout.read_to_string(&mut s);
            let _ = tx.send(s);
        });
        match rx.recv_timeout(PATIENCE) {
            Ok(s) => {
                let _ = child.wait();
                Some(s)
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        }
    }
}

impl Source for ListSource {
    fn id(&self) -> &str {
        "agents-list"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::External
    }

    fn program(&self) -> String {
        Path::new(&self.program)
            .file_stem()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .map_or_else(|| EXTERNAL.to_string(), str::to_string)
    }

    fn poll(&mut self, now: Secs) -> Vec<Observation> {
        if self.off || self.last.is_some_and(|t| now - t < self.every) {
            return Vec::new();
        }
        self.last = Some(now);
        // a program that failed, timed out or printed something else says nothing about who is gone
        let Some(text) = self.run() else { return Vec::new() };
        let Some((mut obs, keys)) = parse_all(&text, now, alive) else { return Vec::new() };
        // a session that was listed and no longer is has ended
        let mut gone: Vec<&String> = self.known.iter().filter(|k| !keys.contains(*k)).collect();
        gone.sort();
        obs.extend(gone.into_iter().map(|k| Observation { agent_key: k.clone(), at: now, kind: ObsKind::Dead }));
        self.known = keys.into_iter().collect();
        obs
    }
}

/// Whether a process still exists. Unknown (None) where it cannot be told.
fn alive(pid: u64) -> Option<bool> {
    let proc = std::path::Path::new("/proc");
    if !proc.join("self").exists() {
        return None;
    }
    Some(proc.join(pid.to_string()).exists())
}

/// The observations of a list. `alive` answers whether a pid still runs.
pub fn parse(text: &str, now: Secs, alive: impl Fn(u64) -> Option<bool>) -> Vec<Observation> {
    parse_all(text, now, alive).map(|(obs, _)| obs).unwrap_or_default()
}

/// The observations of a list and the keys it names; None when the text is not a list at all.
fn parse_all(text: &str, now: Secs, alive: impl Fn(u64) -> Option<bool>) -> Option<(Vec<Observation>, Vec<String>)> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(text.trim()) else { return None };
    let mut out = Vec::new();
    let mut keys = Vec::new();
    for item in items {
        let get = |k: &str| item.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(key) = get("sessionId").or_else(|| get("id")) else { continue };
        keys.push(key.to_string());
        let mk = |kind: ObsKind| Observation { agent_key: key.to_string(), at: now, kind };
        out.push(mk(ObsKind::Meta {
            name: get("name").map(|n| one_line(n, 60)).filter(|n| !n.is_empty()),
            cwd: get("cwd").map(str::to_string),
            permission_mode: None,
        }));
        let state = match get("state") {
            Some("working") => Some(Reported::Working),
            Some("blocked") => Some(Reported::Blocked),
            Some("done") => Some(Reported::Done),
            _ => None,
        };
        if let Some(s) = state {
            out.push(mk(ObsKind::Reported(s)));
        }
        let gone = item.get("pid").and_then(Value::as_u64).and_then(&alive) == Some(false);
        if gone && state != Some(Reported::Done) {
            out.push(mk(ObsKind::Dead));
        }
    }
    Some((out, keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_the_command_name() {
        assert_eq!(ListSource::new("/usr/bin/othertool", vec![], 10).program(), "othertool");
        assert_eq!(ListSource::new("", vec![], 10).program(), "external");
    }

    const LIST: &str = r#"[
      {"id":"a1","sessionId":"s-1","name":"build","kind":"bg","pid":11,"cwd":"/work/app","startedAt":1,"state":"working","status":"busy"},
      {"id":"a2","sessionId":"s-2","name":"","pid":12,"cwd":"/work/lib","state":"blocked","status":"idle"},
      {"id":"a3","name":"old","pid":13,"state":"done"},
      {"nothing":true}
    ]"#;

    #[test]
    fn entries_become_meta_and_reported_state_under_the_session_key() {
        let obs = parse(LIST, 100, |p| Some(p != 13));
        let of = |k: &str| obs.iter().filter(|o| o.agent_key == k).map(|o| o.kind.clone()).collect::<Vec<_>>();
        assert_eq!(
            of("s-1"),
            vec![
                ObsKind::Meta { name: Some("build".into()), cwd: Some("/work/app".into()), permission_mode: None },
                ObsKind::Reported(Reported::Working),
            ]
        );
        assert!(of("s-2").contains(&ObsKind::Reported(Reported::Blocked)));
        // no sessionId: the id keys it; done and gone is Done, not Dead
        assert!(of("a3").contains(&ObsKind::Reported(Reported::Done)));
        assert!(!of("a3").contains(&ObsKind::Dead));
        assert_eq!(obs.iter().filter(|o| o.agent_key.is_empty()).count(), 0);
    }

    #[test]
    fn a_vanished_process_is_dead() {
        let obs = parse(r#"[{"sessionId":"s","pid":5,"state":"working"}]"#, 1, |_| Some(false));
        assert!(obs.iter().any(|o| o.kind == ObsKind::Dead));
        let obs = parse(r#"[{"sessionId":"s","pid":5,"state":"working"}]"#, 1, |_| None);
        assert!(!obs.iter().any(|o| o.kind == ObsKind::Dead), "unknown is not dead");
    }

    #[test]
    fn garbage_gives_nothing() {
        assert!(parse("not json", 1, |_| None).is_empty());
        assert!(parse("{}", 1, |_| None).is_empty());
    }

    #[test]
    fn a_list_that_is_not_a_list_is_not_a_session_count() {
        assert!(parse_all("oops", 1, |_| None).is_none());
        assert!(parse_all("[]", 1, |_| None).is_some_and(|(o, k)| o.is_empty() && k.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn a_session_the_list_stops_naming_is_dead_and_a_bad_answer_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("list.json");
        std::fs::write(&f, r#"[{"sessionId":"a","state":"working"},{"sessionId":"b"}]"#).unwrap();
        let mut s = ListSource::new("cat", vec![f.display().to_string()], 0);
        assert!(s.poll(10).iter().all(|o| o.kind != ObsKind::Dead));
        std::fs::write(&f, "not a list").unwrap();
        assert!(s.poll(20).is_empty(), "a bad answer says nothing about who is gone");
        std::fs::write(&f, r#"[{"sessionId":"b"}]"#).unwrap();
        let obs = s.poll(30);
        let dead: Vec<_> = obs.iter().filter(|o| o.kind == ObsKind::Dead).map(|o| o.agent_key.clone()).collect();
        assert_eq!(dead, vec!["a".to_string()]);
        assert!(s.poll(40).iter().all(|o| o.kind != ObsKind::Dead), "said once");
    }

    #[test]
    fn a_missing_program_turns_the_adapter_off_without_a_sound() {
        let mut s = ListSource::new("nexo-no-such-program-xyz", vec![], 10);
        assert!(s.available());
        assert!(s.poll(1000).is_empty());
        assert!(!s.available());
        assert!(s.poll(2000).is_empty());
    }
}
