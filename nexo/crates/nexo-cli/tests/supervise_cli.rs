//! `nexo supervise hook ...` as a program: it prints `{}` and exits 0 when no supervisor answers, passes a
//! supervisor's answer through when one does, and `install --print` writes nothing.
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nexo_agent::supervise::{NexoSource, Options, Panel, Policy, Setup};

fn run_hook(kind: &str, sock: &std::path::Path, input: &str) -> (String, i32, Duration) {
    let t = Instant::now();
    let mut c = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(["supervise", "hook", kind])
        .env("NEXO_SUPERVISOR_SOCK", sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).trim().to_string(), out.status.code().unwrap_or(-1), t.elapsed())
}

#[test]
fn with_no_supervisor_the_hook_prints_nothing_special_and_exits_fast() {
    let d = tempfile::tempdir().unwrap();
    let (out, code, took) =
        run_hook("stop", &d.path().join("none.sock"), r#"{"session_id":"s","stop_hook_active":false}"#);
    assert_eq!((out.as_str(), code), ("{}", 0));
    assert!(took < Duration::from_secs(2), "{took:?}");
    // bad input and an unknown kind fail open too
    assert_eq!(run_hook("stop", &d.path().join("none.sock"), "not json").0, "{}");
}

#[test]
fn a_running_supervisor_answers_through_the_program() {
    let d = tempfile::tempdir().unwrap();
    let sock = d.path().join("s.sock");
    let setup = Setup { logs_root: None, scan_hours: 12, list_cmd: None, list_every: 10, poll_every_ms: 100 };
    let opts = Options { socket: Some(sock.clone()), dir: Some(d.path().join("st")), must_listen: true };
    let panel = Panel::start_with(&setup, Policy::default(), NexoSource::default(), &opts).unwrap();
    panel.nudge("sess-a", "Check the build output first.");
    let (out, code, _) = run_hook("stop", &sock, r#"{"session_id":"sess-a","stop_hook_active":false}"#);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], "block");
    assert_eq!(v["reason"], "Check the build output first.");
    let (out, _, _) = run_hook(
        "permission",
        &sock,
        r#"{"session_id":"sess-a","tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#,
    );
    assert_eq!(out, "{}");
}

#[test]
fn install_print_shows_the_hooks_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_nexo"))
        .args(["supervise", "install", "--print"])
        .env("HOME", home.path())
        .env("NEXO_CONFIG_DIR", home.path().join("cfg"))
        .env("NEXO_DATA_DIR", home.path().join("data"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    for (event, kind) in [("Stop", "stop"), ("PermissionRequest", "permission"), ("Notification", "notify")] {
        let cmd = v["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with(&format!("supervise hook {kind}")), "{cmd}");
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0, "nothing was written");
}
