//! The dim `HH:MM` on each message and reply, and a reply's duration: on, off, toggled, too narrow.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_core::message::Message;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(timestamps: bool) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        timestamps,
        animations: false,
        ..Default::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261104-143200-ab12".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    // the time is injected: nothing here reads the clock
    a.fx.wall = Some("14:32".into());
    a.drop_banner();
    a
}

/// One question and its reply, the reply taking 64 seconds.
fn talk(a: &mut App) {
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.input.set_text("fix the test");
    a.submit();
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("fix the test") });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::TextDelta { text: "Done, the list is sorted now.".into() });
    a.fx.clock = Some(t0 + Duration::from_secs(64));
    a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("Done, the list is sorted now.") });
    a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
}

fn draw(a: &mut App, w: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter().find(|r| r.contains(needle)).unwrap_or_else(|| panic!("no row with {needle:?}: {rows:#?}"))
}

#[test]
fn on_a_message_and_a_reply_carry_the_time_and_the_reply_its_duration() {
    let mut a = app(true);
    talk(&mut a);
    let rows = draw(&mut a, 80);
    let user = row_with(&rows, "fix the test");
    assert!(user.trim_end().ends_with("14:32"), "{user:?}");
    let reply = row_with(&rows, "Done, the list is sorted now.");
    assert!(reply.trim_end().ends_with("14:32 · 1m04s"), "{reply:?}");
}

#[test]
fn off_nothing_is_drawn() {
    let mut a = app(false);
    talk(&mut a);
    let rows = draw(&mut a, 80);
    assert!(rows.iter().all(|r| !r.contains("14:32")), "{rows:#?}");
}

#[test]
fn the_toggle_shows_and_hides_it_at_once() {
    let mut a = app(true);
    talk(&mut a);
    assert!(draw(&mut a, 80).iter().any(|r| r.contains("14:32")));
    a.input.set_text("/timestamps");
    a.submit();
    assert!(draw(&mut a, 80).iter().all(|r| !r.contains("14:32")));
    a.input.set_text("/timestamps");
    a.submit();
    assert!(draw(&mut a, 80).iter().any(|r| r.contains("14:32 · 1m04s")));
}

#[test]
fn a_replayed_session_shows_nothing_and_a_narrow_line_drops_it() {
    let mut a = app(true);
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("old question") });
    a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("old answer") });
    assert!(draw(&mut a, 80).iter().all(|r| !r.contains("14:32")), "a replay has no saved time");

    let mut a = app(true);
    talk(&mut a);
    let rows = draw(&mut a, 40);
    let reply = row_with(&rows, "Done, the list is sorted now.");
    assert!(!reply.contains("14:32"), "no room, no stamp: {reply:?}");
    assert!(reply.contains("sorted now."), "{reply:?}");
}
