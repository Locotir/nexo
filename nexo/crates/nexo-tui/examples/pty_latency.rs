//! Fixture for tests/pty_latency.py; runs the production TUI loop without a provider.
use nexo_core::agent::{channels, AgentEvent};
use nexo_core::message::Message;
use nexo_core::permission::PermissionMode;
use nexo_tui::TuiOptions;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let blocks: usize = std::env::var("PTY_BLOCKS")?.parse()?;
    let animations = std::env::var("PTY_ANIMATIONS")? == "1";
    let (handle, ends) = channels();
    let cwd = std::env::current_dir()?;
    let events = ends.events.clone();
    events
        .send(AgentEvent::Ready {
            session_id: "pty-fixture".into(),
            provider: "mock".into(),
            model: "test".into(),
            cwd: cwd.clone(),
            permission_mode: PermissionMode::Ask,
        })
        .unwrap();
    for i in 0..blocks {
        let text = if i + 1 == blocks {
            "PTY_FIXTURE_READY https://example.com/pty-mouse".to_owned()
        } else {
            format!("history message {i}")
        };
        events.send(AgentEvent::History { index: i, message: Message::user(text) }).unwrap();
    }
    events
        .send(AgentEvent::Notice {
            level: nexo_core::agent::Level::Info,
            text: "PTY_FIXTURE_READY".into(),
            problem: None,
        })
        .unwrap();
    tokio::spawn(async move {
        // Keep the input receiver and sender alive while continuously producing events.
        let _ends = ends;
        events.send(AgentEvent::TurnStart { turn: 1 }).unwrap();
        loop {
            if events.send(AgentEvent::TextDelta { text: ".".into() }).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    nexo_tui::run(
        handle,
        TuiOptions {
            cwd,
            provider: "mock".into(),
            model: "test".into(),
            animations,
            hints: false,
            complete_words: false,
            ..Default::default()
        },
    )
    .await
}
