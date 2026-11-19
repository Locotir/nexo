//! The person sees that a request was taken from the moment it is, not from the first model call.

use super::*;
use std::time::Instant;

/// Every event up to `stop`, `Preparing` included (`Running::next` passes over it).
async fn raw_until(r: &mut Running, stop: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    loop {
        let ev = r.raw().await;
        let hit = stop(&ev);
        out.push(ev);
        if hit {
            return out;
        }
    }
}

fn preparing(e: &AgentEvent) -> Option<&str> {
    match e {
        AgentEvent::Preparing { what } => Some(what.as_str()),
        _ => None,
    }
}

/// A slow `user_prompt_submit` hook (one second) holds the start of the turn: the request is acknowledged
/// before it, and the echo and the turn come after.
#[cfg(unix)]
#[tokio::test]
async fn a_request_is_acknowledged_before_its_slow_start() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hello there"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("user_prompt_submit", None, "sleep 1"));
    let mut r = start(client, cfg, None, d.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    let sent = Instant::now();
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let before = raw_until(&mut r, |e| preparing(e).is_some()).await;
    let took = sent.elapsed();
    assert_eq!(before.last().and_then(preparing), Some("preparing the request"));
    assert!(took < Duration::from_millis(800), "acknowledged after {took:?}, behind the hook");
    assert!(!before.iter().any(|e| matches!(e, AgentEvent::History { .. })), "acknowledged before the echo");
    let evs = raw_until(&mut r, |e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let echo = evs.iter().position(|e| matches!(e, AgentEvent::History { .. })).expect("the echo");
    let start = evs.iter().position(|e| matches!(e, AgentEvent::TurnStart { .. })).expect("the turn");
    assert!(echo < start);
    // the request is out, and until the reply's first word the wait is the model's
    let model = evs.iter().position(|e| preparing(e) == Some("waiting for the model")).expect("the model's wait");
    let delta = evs.iter().position(|e| matches!(e, AgentEvent::TextDelta { .. })).unwrap();
    assert!(start < model && model < delta);
    r.stop().await;
}

/// While a goal's turn runs the project's check before its first call, the loop still takes commands: a
/// message sent then is queued at once and said to be, never left without an answer.
#[cfg(unix)]
#[tokio::test]
async fn a_message_sent_while_a_goal_turn_is_prepared_is_queued_at_once() {
    if std::process::Command::new("pytest").arg("--version").output().is_err() {
        eprintln!("pytest is not installed: skipped");
        return;
    }
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("pyproject.toml"), "[project]\nname = \"slow\"\n").unwrap();
    std::fs::write(d.path().join("test_slow.py"), "import time\n\ndef test_slow():\n    time.sleep(2)\n").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("on it\nGOAL DONE")), Reply::Events(text("noted"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.check_at_start = true;
    let mut r = start(client, cfg, None, d.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make test_slow pass".into()) });
    let evs = raw_until(&mut r, |e| preparing(e).is_some_and(|w| w.starts_with("checking the project"))).await;
    let steps: Vec<&str> = evs.iter().filter_map(preparing).collect();
    assert_eq!(steps, vec!["starting the goal", "checking the project (pytest)"]);
    let sent = Instant::now();
    r.send(AgentCommand::Submit { text: "sigue con la misión".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    let took = sent.elapsed();
    assert!(took < Duration::from_millis(800), "queued after {took:?}: the loop waited for the check");
    assert!(matches!(evs.last(), Some(AgentEvent::Queued { pending: 1 })));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::History { .. })), "the goal's request is not built yet");
    r.stop().await;
}

/// A Cargo crate with a failing test: its check always has something to say.
fn failing_crate(dir: &std::path::Path, test_sleep_secs: u64) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
        .unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        format!(
            "pub fn add(a: i32, b: i32) -> i32 {{\n    a - b\n}}\n\n#[test]\nfn adds() {{\n    \
             std::thread::sleep(std::time::Duration::from_secs({test_sleep_secs}));\n    assert_eq!(add(2, 3), 5);\n}}\n"
        ),
    )
    .unwrap();
    prebuild_crate(dir);
}

/// Whether the first request of a fresh session at `dir` carries the project's check after `words`.
async fn start_check_comes(dir: &std::path::Path, words: &str) -> bool {
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.check_at_start = true;
    let mut r = start(client.clone(), cfg, None, dir).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: words.into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let sent = client.requests()[0].messages[0].text();
    sent.contains("\ncheck: cargo test")
}

/// The check at the start needs a cause: code named in the message (a source file, an identifier the project
/// defines, the check's own command) and a project of at most 200 source files.
#[cfg(unix)]
#[tokio::test]
async fn the_start_check_runs_only_on_code_evidence_in_a_small_tree() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    failing_crate(d.path(), 0);
    assert!(!start_check_comes(d.path(), "hi").await);
    assert!(start_check_comes(d.path(), "fix `add`").await);
    assert!(start_check_comes(d.path(), "`cargo test` fails").await);
    for i in 0..201 {
        std::fs::write(d.path().join(format!("src/g{i:03}.rs")), format!("pub fn g{i}() {{}}\n")).unwrap();
    }
    assert!(!start_check_comes(d.path(), "fix `add`").await);
}

/// A check that was too slow at the start is not run at a start again, and it does not stop the check after a
/// write: that one has its own record.
#[cfg(unix)]
#[tokio::test]
async fn a_start_check_over_its_cap_leaves_the_post_write_check_on() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    failing_crate(d.path(), 5);
    let edit = json!({"path": "src/lib.rs", "old_string": "a - b", "new_string": "a + b"});
    let client = Scripted::new(vec![Reply::Events(call("e1", "edit", edit)), Reply::Events(text("done"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.check_at_start = true;
    cfg.tools.post_write_check = true;
    cfg.tools.post_write_check_secs = 1;
    let set = tools().with(Arc::new(nexo_tools::edit::Edit));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "fix `add` in src/lib.rs".into(), attachments: vec![] });
    // the post-write check waits for the crate's slow test, so the turn takes longer than a plain one
    loop {
        let ev = r.raw_within(Duration::from_secs(90)).await;
        if matches!(ev, AgentEvent::TurnEnd { .. }) {
            break;
        }
    }
    r.stop().await;
    let reqs = client.requests();
    // the start check ran, and the cap killed it: without that the post-write asserts below prove nothing
    let first = reqs[0].messages[0].text();
    assert!(first.contains("killed at the 1 s cap"), "{first}");
    let result: String = reqs[1]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| if let Part::ToolResult { content, .. } = p { Some(text_of_parts(content)) } else { None })
        .collect();
    assert!(result.contains("\ncheck: cargo test"), "{result}");
    assert!(!result.contains("| skipped |"), "the start check's slowness is not the post-write check's: {result}");
    // a real outcome of the check (an exit or the cap's kill), not a note that it was left out
    assert!(result.contains("| exit "), "{result}");
}
