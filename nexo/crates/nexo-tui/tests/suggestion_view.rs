//! A suggested next message shows alone in the empty input, where the placeholder would be:
//! the placeholder is not drawn over it.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
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
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        session_id: Some("20261106-100000-ab12".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261106-100000-ab12".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a
}

fn screen(a: &mut App) -> String {
    draw(a).0
}

/// The screen as text, and the colour of the first cell of `word` on it.
fn draw(a: &mut App) -> (String, impl Fn(&str) -> Color) {
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    let text = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    let colour = move |word: &str| {
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains(word)).expect(word);
        let x = row[..row.find(word).unwrap()].chars().count();
        buf[(x as u16, y as u16)].fg
    };
    (text, colour)
}

#[test]
fn a_suggestion_takes_the_placeholder_row_and_the_placeholder_comes_back_without_it() {
    let mut a = app();
    let (s, colour) = draw(&mut a);
    assert!(s.contains("ask anything"));
    let placeholder = colour("ask anything");
    a.handle_agent(AgentEvent::Suggestion { text: "Now implement the TODO: parse args.".into() });
    let s = screen(&mut a);
    assert!(s.contains("Now implement the TODO: parse args."), "{s}");
    assert!(!s.contains("ask anything"), "{s}");
    assert_eq!(draw(&mut a).1("Now implement"), placeholder, "drawn in the dim of the placeholder");
    a.suggestion = None;
    assert!(screen(&mut a).contains("ask anything"));
}
