//! Tool cards fold and unfold (Ctrl+O), each with its status, its duration and a one-line summary
//! when folded. Checked on the screen at 80x24 and 60x16.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_core::tool::ToolOutput;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 3,
        group_tools: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        vim: false,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let out = (1..=12).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
    for (id, summary, ok, ms) in [("c1", "bash: cargo test", true, 1500u64), ("c2", "bash: false", false, 40)] {
        a.handle_agent(AgentEvent::ToolCall {
            id: id.into(),
            name: "bash".into(),
            args: serde_json::json!({ "command": summary }),
            summary: summary.into(),
        });
        a.handle_agent(AgentEvent::ToolStart { id: id.into() });
        let o = if ok { ToolOutput::ok(out.clone()) } else { ToolOutput::error(out.clone()) };
        a.handle_agent(AgentEvent::ToolDone { id: id.into(), output: o, elapsed_ms: ms });
    }
    a
}

fn screen(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let rows = rows_of(t.backend().buffer());
    assert_eq!(rows.len(), h as usize);
    rows.join("\n")
}

/// The cards only: the hints row (flash message) and the status rule with the running timer
/// change on their own, so they are left out of the comparison.
fn cards(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().collect();
    let hints = rows.len().saturating_sub(2);
    rows.iter()
        .enumerate()
        .filter(|(i, l)| *i != hints && !l.starts_with('─'))
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n")
}

fn ctrl_o(a: &mut App) {
    event::key(a, KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
}

#[test]
fn cards_fold_and_unfold_with_status_duration_and_summary_at_two_sizes() {
    for (w, h) in [(80u16, 24u16), (60, 16)] {
        let mut a = app();
        let folded = cards(&screen(&mut a, w, h));
        assert!(folded.contains("cargo test"), "{w}x{h} summary lost:\n{folded}");
        assert!(folded.contains("1.5s"), "{w}x{h} duration lost:\n{folded}");
        assert!(folded.contains("✓") || folded.contains("ok"), "{w}x{h} ok state lost:\n{folded}");
        assert!(folded.contains("✗"), "{w}x{h} error state lost:\n{folded}");
        assert!(!folded.contains("line12"), "{w}x{h} the folded card shows the whole output:\n{folded}");
        // Ctrl+O toggles every body: whatever it does, twice brings the screen back
        ctrl_o(&mut a);
        let once = cards(&screen(&mut a, w, h));
        ctrl_o(&mut a);
        let twice = cards(&screen(&mut a, w, h));
        assert_ne!(folded, once, "{w}x{h} Ctrl+O changed nothing");
        assert!(once.contains("line12"), "{w}x{h} the opened card does not show its whole output:\n{once}");
        assert_eq!(folded, twice, "{w}x{h} Ctrl+O twice is not the start");
    }
}
