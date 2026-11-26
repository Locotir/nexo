//! Fixture for tests/pty_latency.py; runs the production TUI loop without a provider.
use nexo_core::agent::{channels, AgentEvent};
use nexo_core::message::Message;
use nexo_core::permission::PermissionMode;
use nexo_tui::TuiOptions;
use std::time::{Duration, Instant};

fn context(view: u64, history: u64, blocks: usize, label: &str) -> AgentEvent {
    AgentEvent::Context {
        view_tokens: view,
        history_tokens: history,
        history_bytes: history * 4,
        view_bytes: view * 4,
        messages_in_view: blocks,
        messages_total: blocks,
        policy: "curator".into(),
        note: None,
        label: label.into(),
        detail: None,
    }
}

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
    events
        .send(AgentEvent::Effort {
            model: "test".into(),
            current: Some("high".into()),
            levels: vec!["low".into(), "medium".into(), "high".into()],
        })
        .unwrap();
    events.send(context(4000, 8000, blocks, "curator")).unwrap();
    tokio::spawn(async move {
        // Keep the input receiver and sender alive while continuously producing events.
        let _ends = ends;
        events.send(AgentEvent::TurnStart { turn: 1 }).unwrap();
        let mut next_context = Instant::now() + Duration::from_secs(1);
        let (mut view, mut history) = (4000, 8000);
        let mut positive = true;
        loop {
            if Instant::now() >= next_context {
                // Both views grow, so the separate map-cut animation stays out of the sample.
                history += 700;
                view += if positive { 1300 } else { 100 };
                let label = if positive { "curator +600 tok" } else { "curator -600 tok" };
                if events.send(context(view, history, blocks, label)).is_err() {
                    break;
                }
                positive = !positive;
                next_context = Instant::now() + Duration::from_secs(2);
            }
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
            status_line: vec!["curator".into(), "effort".into()],
            hints: false,
            complete_words: false,
            ..Default::default()
        },
    )
    .await
}
