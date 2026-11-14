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
    r.send(AgentCommand::SetGoal { text: Some("restate the plan".into()) });
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
