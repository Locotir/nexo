//! Enter on an @ picker with no match sends the line; Tab stays a no-op.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_tui::{event, App, TuiOptions};
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, rx) = mpsc::unbounded_channel();
    std::mem::forget(rx);
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
        complete_words: false,
        suggest: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations: false,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        todo_strip: false,
        vim: false,
        prompt: None,
        session_id: Some("20261102-100000-ef56".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a
}

fn press(a: &mut App, code: KeyCode) {
    event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

#[test]
fn enter_on_an_empty_files_picker_sends_the_line() {
    let mut a = app();
    type_str(&mut a, "mail bob @zzqqxx");
    assert!(a.palette.as_ref().is_some_and(|p| p.current().is_none()));
    press(&mut a, KeyCode::Enter);
    assert!(a.palette.is_none());
    assert_eq!(a.input.full_text(), "", "the first Enter must send the line");
}

#[test]
fn tab_on_an_empty_files_picker_does_nothing() {
    let mut a = app();
    type_str(&mut a, "mail bob @zzqqxx");
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.input.full_text(), "mail bob @zzqqxx");
}
