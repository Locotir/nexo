//! `mono` (which `NO_COLOR` selects) paints nothing: every cell of every screen keeps the
//! terminal's own colours, and the screen is told apart with bold, dim and reverse only.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_core::message::Message;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(theme: &str) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: theme.into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-100000-ef56".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user("fix the failing test") });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: "the test expects a sorted list".into() });
    a.handle_agent(AgentEvent::TextDelta {
        text: "I will **read** it.\n\n- the input is `3 1 2`\n\n```rust\nfn main() {}\n```\n".into(),
    });
    a
}

fn colours(theme: &str, w: u16, h: u16) -> Vec<(u16, u16, Color, Color)> {
    let mut a = app(theme);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let buf = t.backend().buffer();
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = &buf[(x, y)];
            out.push((x, y, c.fg, c.bg));
        }
    }
    out
}

#[test]
fn mono_leaves_every_cell_to_the_terminals_colours_at_every_size() {
    for (w, h) in [(40, 12), (80, 24), (200, 60)] {
        for (x, y, fg, bg) in colours("mono", w, h) {
            assert_eq!((fg, bg), (Color::Reset, Color::Reset), "{w}x{h} cell {x},{y}");
        }
    }
}

#[test]
fn the_coloured_theme_does_paint_so_the_test_above_means_something() {
    let painted =
        colours("dark", 80, 24).iter().filter(|(_, _, fg, bg)| *fg != Color::Reset || *bg != Color::Reset).count();
    assert!(painted > 100, "{painted}");
}
