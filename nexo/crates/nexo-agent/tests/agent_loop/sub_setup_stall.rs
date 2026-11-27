//! Three sub-agents starting from one message, against a long conversation.
//!
//! A probe on the same runtime ticks every 5 ms. The longest gap between ticks
//! is how long the runtime workers were busy with the setup (the stall the TUI
//! sees, since it draws on those workers). Printed, not asserted: the number is
//! what a change to that setup is judged by.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nexo_core::agent::AgentCommand;
use nexo_core::permission::PermissionMode;
use nexo_core::stream::{FinishReason, StreamEvent};

use super::*;

const TICK_MS: u64 = 5;
const MESSAGES: usize = 300;
const TOOL_BYTES: usize = 4000;

fn long_history(n: usize) -> Vec<Message> {
    let body = "x".repeat(TOOL_BYTES);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (id, name) = (format!("c{i}"), if i % 2 == 0 { "read" } else { "grep" });
        out.push(Message::assistant(format!("looking at item {i}")));
        out.push(Message::new(
            Role::Assistant,
            vec![Part::ToolCall { id: id.clone(), name: name.into(), args: json!({"path": format!("f{i}.rs")}) }],
        ));
        out.push(Message::tool_results(vec![Part::ToolResult {
            call_id: id,
            name: Some(name.into()),
            content: vec![Part::text(format!("{name} f{i}.rs\n{body}"))],
            is_error: false,
        }]));
    }
    out.push(Message::user("three look at the files above"));
    out
}

/// The longest a 5 ms probe on this runtime went unanswered, in ms.
/// `mark` bumps when a phase starts: the tick that crosses the bump is dropped, so a
/// stall before the phase is not counted in it.
fn watch_gaps(stop: Arc<AtomicBool>, worst: Arc<AtomicU64>, mark: Arc<AtomicU64>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = Instant::now();
        let mut seen = mark.load(Ordering::Relaxed);
        let step = Duration::from_millis(TICK_MS);
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(step).await;
            let now = mark.load(Ordering::Relaxed);
            if now != seen {
                seen = now;
                last = Instant::now();
                continue;
            }
            let gap = last.elapsed().as_millis() as u64;
            worst.fetch_max(gap, Ordering::Relaxed);
            last = Instant::now();
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn three_sub_agent_setups_leave_the_runtime_a_gap() {
    let d = test_tmpdir().unwrap();
    // the history is a real session: the stall is the resume, which reads it all back in
    let seed = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut seeding = start(seed, config(PermissionMode::Auto), None, d.path()).await;
    let AgentEvent::Ready { session_id: id, .. } = seeding.next().await else { panic!("no Ready") };
    seeding.send(AgentCommand::Submit { text: "seed".into(), attachments: vec![] });
    seeding.turn().await;
    seeding.stop().await;
    let mut session = nexo_agent::Session::open(&id).unwrap();
    for m in long_history(MESSAGES) {
        session.append(&m).unwrap();
    }

    let three = vec![
        StreamEvent::ToolCallStart { id: "t1".into(), name: "task".into() },
        StreamEvent::ToolCallEnd { id: "t1".into(), name: "task".into(), args: json!({"prompt": "look at f1.rs"}) },
        StreamEvent::ToolCallStart { id: "t2".into(), name: "task".into() },
        StreamEvent::ToolCallEnd { id: "t2".into(), name: "task".into(), args: json!({"prompt": "look at f2.rs"}) },
        StreamEvent::ToolCallStart { id: "t3".into(), name: "task".into() },
        StreamEvent::ToolCallEnd { id: "t3".into(), name: "task".into(), args: json!({"prompt": "look at f3.rs"}) },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![
        Reply::Events(three),
        Reply::Events(text("report one")),
        Reply::Events(text("report two")),
        Reply::Events(text("report three")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "curator".into();
    cfg.agent.subagent_brief = 3000;
    cfg.agent.parallel_tasks = true;
    let stop = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(AtomicU64::new(0));
    let mark = Arc::new(AtomicU64::new(0));
    let probe = watch_gaps(stop.clone(), worst.clone(), mark.clone());

    let mut r = start(client, cfg, Some(id), d.path()).await;
    // Ready comes before the history replay. Waiting it out on this runtime is itself the stall the
    // resume costs, and the three setups run after it. The replay of this history takes longer than
    // the usual event wait, so this one waits longer.
    let _ = r.until_within(|e| matches!(e, AgentEvent::Usage { .. }), Duration::from_secs(120)).await;
    let after_resume = worst.load(Ordering::Relaxed);

    r.next().await;
    // The gap that matters is the three setups, not the resume that preceded them.
    worst.store(0, Ordering::Relaxed);
    mark.fetch_add(1, Ordering::Relaxed);
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    // the three setups run, then each sub-agent's first model call; the turn ends after
    let _ = r.turn().await;

    stop.store(true, Ordering::Relaxed);
    probe.await.unwrap();
    let setup = worst.load(Ordering::Relaxed);
    eprintln!(
        "sub-setup stall: max probe gap {setup} ms (resume {after_resume} ms, tick {TICK_MS} ms, {MESSAGES} messages)"
    );
}
