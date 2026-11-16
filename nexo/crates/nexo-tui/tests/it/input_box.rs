//! The input box: a long draft keeps its place, a paste goes where the keys go, a picked history entry leaves the draft recoverable.

use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
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
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn screen(a: &mut App) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(60, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer())
}

fn press(a: &mut App, code: KeyCode, m: KeyModifiers) {
    event::handle(a, Event::Key(KeyEvent::new(code, m)));
}

#[test]
fn a_long_draft_keeps_its_rows_while_the_cursor_moves_inside_them() {
    let mut a = app();
    let draft: String = (1..=20).map(|n| format!("row number {n:02}")).collect::<Vec<_>>().join("\n");
    a.input.set_text(&draft);
    screen(&mut a);
    let top = a.editor_top;
    assert!(top > 0, "the end of a long draft is in sight");
    press(&mut a, KeyCode::Up, KeyModifiers::NONE);
    screen(&mut a);
    assert_eq!(a.editor_top, top, "Up inside the window does not slide the text");
    for _ in 0..30 {
        press(&mut a, KeyCode::Up, KeyModifiers::NONE);
    }
    screen(&mut a);
    assert_eq!(a.editor_top, 0, "past the window it scrolls to the cursor");
}

#[test]
fn a_paste_goes_to_the_search_bar_while_it_has_focus() {
    let mut a = app();
    a.open_search();
    event::handle(&mut a, Event::Paste("foo\rbar\r\nbaz".into()));
    assert_eq!(a.search.as_ref().unwrap().query, "foo bar baz");
    assert!(a.input.is_empty(), "the draft stays empty");
    a.search.as_mut().unwrap().focused = false;
    event::handle(&mut a, Event::Paste("kept".into()));
    assert_eq!(a.input.text(), "kept", "an unfocused bar leaves the paste to the draft");
}

#[test]
fn a_picked_history_entry_leaves_the_draft_one_step_back() {
    let mut a = app();
    a.input.push_history("old request");
    a.input.set_text("my unsent draft");
    press(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(a.input.text(), "old request");
    assert!(a.input.history().iter().any(|h| h == "my unsent draft"), "the draft went to the history");
}
