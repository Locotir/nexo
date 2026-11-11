//! A scripted agent behind the interface, for a look without a model:
//! `cargo run -p nexo-tui --example demo`.

use std::path::PathBuf;
use std::time::Duration;

use nexo_core::agent::{channels, AgentCommand, AgentEvent, GoalState, Level, TurnEnd};
use nexo_core::message::Message;
use nexo_core::permission::{PermissionMode, PermissionRequest};
use nexo_core::pricing::Pricing;
use nexo_core::stream::Usage;
use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay, ToolKind, ToolOutput};
use nexo_tui::TuiOptions;

const REPLY: &str = "I looked at the failing test and the cause is in `parse_weight`: it treats **tonnes** as kilograms.\n\n\
## What I will do\n\n1. read the test\n2. fix the conversion\n3. run the suite\n\n\
```rust\nfn parse_weight(s: &str) -> f64 {\n    let (n, unit) = split(s);\n    match unit {\n        \"t\" => n * 1000.0,\n        _ => n,\n    }\n}\n```\n\n\
| unit | factor |\n|---|---|\n| kg | 1 |\n| t | 1000 |\n\n> The test file also checks the error message, so I keep its wording.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (handle, mut ends) = channels();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let events = ends.events.clone();
    let cwd2 = cwd.clone();
    tokio::spawn(async move {
        let _ = events.send(AgentEvent::Ready {
            session_id: "demo-session".into(),
            provider: "mock".into(),
            model: "scripted".into(),
            cwd: cwd2,
            permission_mode: PermissionMode::Ask,
        });
        let mut pricing = Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 };
        let limits = |pricing: Pricing, source: &str| AgentEvent::ModelLimits {
            provider: "mock".into(),
            model: "scripted".into(),
            pricing: Some(pricing),
            context_window: Some(128_000),
            max_output: Some(8192),
            source: source.into(),
        };
        let _ = events.send(limits(pricing, "preset"));
        let _ = events.send(AgentEvent::Goal {
            text: Some("make the weight tests pass".into()),
            round: 0,
            max: 3,
            state: Default::default(),
        });
        // the banner shows for a moment, then the person's first message arrives
        tokio::time::sleep(Duration::from_millis(1500)).await;
        // the history as the agent would keep it: the person's messages and the replies
        let mut history: Vec<Message> = vec![Message::user("fix the failing test in @tests/tasks.rs")];
        let _ = events.send(AgentEvent::History { index: 0, message: history[0].clone() });
        if play(&events, &mut ends.commands).await {
            history.push(Message::assistant(REPLY));
        }
        let mut forks = 0;
        let mut goal_round = 0;
        let mut tree: Vec<(usize, String, String)> = vec![(0, "demo-session".into(), "fix the failing test".into())];
        loop {
            match ends.commands.recv().await {
                Some(AgentCommand::Submit { text, .. }) => {
                    history.push(Message::user(text));
                    let _ = events.send(AgentEvent::History {
                        index: history.len() - 1,
                        message: history.last().cloned().unwrap_or_else(|| Message::user("")),
                    });
                    if play(&events, &mut ends.commands).await {
                        history.push(Message::assistant(REPLY));
                    }
                }
                Some(AgentCommand::Rewind { index }) => {
                    history.truncate(index);
                    let _ = events.send(AgentEvent::HistoryCleared);
                    for (i, m) in history.iter().enumerate() {
                        let _ = events.send(AgentEvent::History { index: i, message: m.clone() });
                    }
                    let _ = events.send(AgentEvent::Notice {
                        level: Level::Info,
                        text: format!("rewound to {} messages", history.len()),
                    });
                }
                Some(AgentCommand::Resume { id }) => {
                    history = vec![
                        Message::user("add a tonnes unit to parse_weight"),
                        Message::assistant("Done: `t` now multiplies by 1000; the tests pass."),
                    ];
                    let _ = events.send(AgentEvent::HistoryCleared);
                    let _ =
                        events.send(AgentEvent::SessionChanged { session_id: id, title: "add a tonnes unit".into() });
                    for (i, m) in history.iter().enumerate() {
                        let _ = events.send(AgentEvent::History { index: i, message: m.clone() });
                    }
                }
                Some(AgentCommand::Fork { index }) => {
                    let keep = index.unwrap_or(history.len()).min(history.len());
                    history.truncate(keep);
                    forks += 1;
                    let id = format!("demo-fork-{forks}");
                    tree.push((1, id.clone(), "fix the failing test (fork)".to_string()));
                    let _ = events.send(AgentEvent::HistoryCleared);
                    let _ = events.send(AgentEvent::SessionChanged {
                        session_id: id.clone(),
                        title: "fix the failing test (fork)".into(),
                    });
                    for (i, m) in history.iter().enumerate() {
                        let _ = events.send(AgentEvent::History { index: i, message: m.clone() });
                    }
                    let _ = events.send(AgentEvent::Notice {
                        level: Level::Info,
                        text: format!("forked demo-session into {id} (/tree to jump back)"),
                    });
                }
                Some(AgentCommand::ShowTree) => {
                    let _ = events.send(AgentEvent::SessionTree { entries: tree.clone() });
                }
                Some(AgentCommand::Export { path, .. }) => {
                    let where_ = path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.nexo/sessions/demo-session.md".into());
                    let _ =
                        events.send(AgentEvent::Notice { level: Level::Info, text: format!("exported to {where_}") });
                }
                Some(AgentCommand::Clear) => {
                    history.clear();
                    let _ = events.send(AgentEvent::HistoryCleared);
                    let _ = events
                        .send(AgentEvent::SessionChanged { session_id: "demo-session-2".into(), title: String::new() });
                }
                Some(AgentCommand::Shell { command }) => {
                    let out = nexo_core::shell::shell().command(&command).output();
                    let (output, code) = match out {
                        Ok(o) => (
                            format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
                            o.status.code(),
                        ),
                        Err(e) => (e.to_string(), None),
                    };
                    let _ = events.send(AgentEvent::ShellOutput { command, output, exit_code: code });
                }
                Some(AgentCommand::SetModel { provider, model }) => {
                    let _ = events
                        .send(AgentEvent::ModelChanged { provider: provider.unwrap_or_else(|| "mock".into()), model });
                }
                Some(AgentCommand::SetPermissionMode(mode)) => {
                    let _ = events.send(AgentEvent::PermissionModeChanged { mode });
                }
                Some(AgentCommand::ShowModel) => {
                    let _ = events.send(limits(pricing, "preset"));
                }
                Some(AgentCommand::SetGoal { text }) => {
                    // scripted runs (NEXO_DEMO_GOAL): "/goal next round" and "/goal goal met" move the goal along
                    let scripted = std::env::var_os("NEXO_DEMO_GOAL").is_some();
                    let kept = Some("make the weight tests pass".to_string());
                    let (text, round, state) = match text.as_deref() {
                        Some("next round") if scripted => (kept, goal_round + 1, GoalState::Active),
                        Some("goal met") if scripted => (kept, goal_round, GoalState::Done),
                        _ => (text, 0, GoalState::Active),
                    };
                    goal_round = round;
                    let _ = events.send(AgentEvent::Goal { text, round, max: 3, state });
                }
                Some(AgentCommand::SetPricing { pricing: p }) => {
                    pricing = p;
                    let _ = events.send(limits(pricing, "config"));
                }
                Some(AgentCommand::ShowPrompt) => {
                    let _ = events.send(AgentEvent::SystemPrompt {
                        text: "You are nexo, a coding agent working in a terminal.\n\nWorking directory: .".into(),
                    });
                }
                Some(AgentCommand::Compact) => {
                    let _ = events.send(AgentEvent::Notice {
                        level: Level::Info,
                        text: "compacted: 4 messages became a summary".into(),
                    });
                }
                Some(AgentCommand::Shutdown) | None => {
                    let _ = events.send(AgentEvent::Shutdown);
                    break;
                }
                _ => {}
            }
        }
    });
    let opts = TuiOptions {
        cwd,
        provider: "mock".into(),
        model: "scripted".into(),
        models: vec![
            ("mock".into(), "scripted".into()),
            ("mock".into(), "fast".into()),
            ("deepseek".into(), "deepseek-chat".into()),
        ],
        sessions: vec![
            ("20261027-101500-7f3a".into(), "fix the failing test".into()),
            ("20261026-180002-c0de".into(), "add a tonnes unit".into()),
        ],
        // DEMO_STILL=1 (or NEXO_DEMO_STILL=1) runs with the animations off, to compare the settled frame
        animations: std::env::var_os("DEMO_STILL").is_none() && std::env::var_os("NEXO_DEMO_STILL").is_none(),
        ..Default::default()
    };
    nexo_tui::run(handle, opts).await
}

/// One scripted turn; true when it ran to its reply.
async fn play(events: &nexo_core::agent::EventTx, commands: &mut nexo_core::agent::CommandRx) -> bool {
    let _ = events.send(AgentEvent::TurnStart { turn: 1 });
    let _ = events.send(AgentEvent::AssistantStart);
    for w in "Let me check which assertion fails and why the tonnes case is off by a thousand.".split(' ') {
        let _ = events.send(AgentEvent::ReasoningDelta { text: format!("{w} ") });
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let todo = |done: usize| ToolDisplay::Todo {
        items: ["read the failing test", "fix the tonnes conversion", "run the suite"]
            .iter()
            .enumerate()
            .map(|(i, t)| TodoItem {
                text: t.to_string(),
                status: if i < done {
                    TodoStatus::Done
                } else if i == done {
                    TodoStatus::InProgress
                } else {
                    TodoStatus::Pending
                },
            })
            .collect(),
    };
    let _ = events.send(AgentEvent::ToolCall {
        id: "c0".into(),
        name: "todo".into(),
        args: serde_json::json!({"items": ["read the failing test", "fix the tonnes conversion", "run the suite"]}),
        summary: "3 tasks".into(),
    });
    let _ = events.send(AgentEvent::ToolStart { id: "c0".into() });
    let _ = events.send(AgentEvent::ToolDone {
        id: "c0".into(),
        output: ToolOutput::ok("3 tasks").with_display(todo(0)),
        elapsed_ms: 1,
    });
    let _ = events.send(AgentEvent::ToolCall {
        id: "c1".into(),
        name: "read".into(),
        args: serde_json::json!({"path": "tests/tasks.rs"}),
        summary: "tests/tasks.rs".into(),
    });
    let _ = events.send(AgentEvent::ToolStart { id: "c1".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let text = "use crate::parse_weight;\n\n#[test]\nfn tonnes() {\n    assert_eq!(parse_weight(\"2t\"), 2000.0);\n}\n";
    let _ = events.send(AgentEvent::ToolDone {
        id: "c1".into(),
        output: ToolOutput::ok(text).with_display(ToolDisplay::File {
            path: "tests/tasks.rs".into(),
            text: text.into(),
            start_line: 1,
        }),
        elapsed_ms: 310,
    });
    let _ = events.send(AgentEvent::ToolCall {
        id: "c2".into(),
        name: "edit".into(),
        args: serde_json::json!({"path": "src/weight.rs"}),
        summary: "src/weight.rs".into(),
    });
    let _ = events.send(AgentEvent::ToolStart { id: "c2".into() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let old = "fn parse_weight(s: &str) -> f64 {\n    let (n, unit) = split(s);\n    match unit {\n        \"t\" => n,\n        _ => n,\n    }\n}\n";
    let new = "fn parse_weight(s: &str) -> f64 {\n    let (n, unit) = split(s);\n    match unit {\n        \"t\" => n * 1000.0,\n        _ => n,\n    }\n}\n";
    let _ = events.send(AgentEvent::ToolDone {
        id: "c2".into(),
        output: ToolOutput::ok("edited src/weight.rs").with_display(ToolDisplay::Diff {
            path: "src/weight.rs".into(),
            old: old.into(),
            new: new.into(),
            start_line: 1,
        }),
        elapsed_ms: 4,
    });
    let _ = events.send(AgentEvent::ToolCall {
        id: "c4".into(),
        name: "todo".into(),
        args: serde_json::json!({}),
        summary: "3 tasks".into(),
    });
    let _ = events.send(AgentEvent::ToolStart { id: "c4".into() });
    let _ = events.send(AgentEvent::ToolDone {
        id: "c4".into(),
        output: ToolOutput::ok("3 tasks").with_display(todo(1)),
        elapsed_ms: 1,
    });
    // a permission ask for the command
    let _ = events.send(AgentEvent::ToolCall {
        id: "c3".into(),
        name: "bash".into(),
        args: serde_json::json!({"command": "cargo test"}),
        summary: "cargo test".into(),
    });
    let _ = events.send(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            id: "c3".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: "cargo test".into(),
            detail: ToolDisplay::Shell {
                command: "cargo test".into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec!["bash(cargo test*)".into(), "bash(cargo *)".into(), "bash".into()],
        },
    });
    let allowed;
    loop {
        match commands.recv().await {
            Some(AgentCommand::Permission { id, decision }) if id == "c3" => {
                allowed = !matches!(decision, nexo_core::permission::PermissionDecision::Deny { .. });
                let _ = events.send(AgentEvent::PermissionResolved { id, decision });
                break;
            }
            Some(AgentCommand::Interrupt | AgentCommand::InterruptRun) => {
                let _ = events.send(AgentEvent::TurnEnd { reason: TurnEnd::Interrupted });
                return false;
            }
            Some(AgentCommand::Shutdown) | None => {
                let _ = events.send(AgentEvent::Shutdown);
                return false;
            }
            Some(AgentCommand::Submit { .. }) => {
                let _ = events.send(AgentEvent::Queued { pending: 1 });
            }
            _ => {}
        }
    }
    if allowed {
        let _ = events.send(AgentEvent::ToolStart { id: "c3".into() });
        let mut out = String::new();
        for i in 1..=300 {
            let line = format!("test case_{i:03} ... ok");
            out.push_str(&line);
            out.push('\n');
            if i % 10 == 0 {
                let _ = events.send(AgentEvent::ToolProgress { id: "c3".into(), line });
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        }
        out.push_str("test result: ok. 300 passed\n");
        let _ = events.send(AgentEvent::ToolDone {
            id: "c3".into(),
            output: ToolOutput::ok(out.clone()).with_display(ToolDisplay::Shell {
                command: "cargo test".into(),
                stdout: out,
                stderr: String::new(),
                exit_code: Some(0),
            }),
            elapsed_ms: 2300,
        });
    } else {
        let _ = events.send(AgentEvent::ToolDone {
            id: "c3".into(),
            output: ToolOutput::error("Permission denied by the user"),
            elapsed_ms: 0,
        });
    }
    for w in REPLY.split_inclusive(' ') {
        let _ = events.send(AgentEvent::TextDelta { text: w.to_string() });
        tokio::time::sleep(Duration::from_millis(12)).await;
    }
    let _ = events.send(AgentEvent::AssistantDone { message: Message::assistant(REPLY) });
    let _ = events.send(AgentEvent::Usage {
        turn: Usage { input: 1840, output: 412, cache_read: 12_300, ..Default::default() },
        call: Usage { input: 1840, output: 412, cache_read: 12_300, ..Default::default() },
        session: Usage { input: 1840, output: 412, cache_read: 12_300, ..Default::default() },
        cost_usd: 0.0042,
    });
    let _ = events.send(AgentEvent::Context {
        view_tokens: 7420,
        history_tokens: 14_900,
        history_bytes: 59_600,
        view_bytes: 29_680,
        messages_in_view: 5,
        messages_total: 8,
        policy: "curator".into(),
        note: Some("2 messages left the view".into()),
        label: "curator +2 in -1 out".into(),
        detail: Some("curator: this call sends 7.4k tokens of the 14.9k tokens kept (50% less)".into()),
    });
    let _ = events.send(AgentEvent::Notice {
        level: Level::Info,
        text: "the curator kept the view at 7.4k tokens (history 14.9k)".into(),
    });
    let _ = events.send(AgentEvent::Queued { pending: 0 });
    let _ = events.send(AgentEvent::TurnEnd { reason: TurnEnd::Done });
    true
}
