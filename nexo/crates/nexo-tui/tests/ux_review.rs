//! Review fixes: no ghost after a paste, Ctrl+U, a scrolling help, new commands, typo hints.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_tui::modal::{Modal, PickItem, PickKind, Picker};
use nexo_tui::{draw_frame, App, TuiOptions};
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
        suggest: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
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

fn draw(a: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    nexo_tui::event::key(a, KeyEvent::new(code, mods));
}

fn run(a: &mut App, line: &str) {
    a.input.set_text(line);
    a.submit();
}

#[test]
fn ctrl_u_cuts_to_the_line_start_and_undo_brings_it_back() {
    let mut a = app();
    a.input.insert_paste("one\ntwo\nthree");
    key(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(a.input.text(), "one\ntwo\n");
    key(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(a.input.text(), "one\ntwo\nthree");
}

#[test]
fn the_ghost_waits_for_a_typed_single_line_draft() {
    let mut a = app();
    a.input.set_text("hello world");
    a.input.insert_paste("");
    a.input.insert_paste("hel");
    assert_eq!(a.input.word_before_cursor(), None, "none right after a paste");
    key(&mut a, KeyCode::Char('l'), KeyModifiers::NONE);
    assert!(a.input.word_before_cursor().is_some(), "a typed key brings it back");
    a.input.set_text("a\nhel");
    assert_eq!(a.input.word_before_cursor(), None, "none in a multi-line draft");
}

#[test]
fn the_help_scrolls_by_page_and_says_so() {
    let mut a = app();
    a.modal = Some(Modal::Help { scroll: 0 });
    let first = draw(&mut a, 80, 24);
    assert!(first.contains("PgUp/PgDn"), "{first}");
    key(&mut a, KeyCode::PageDown, KeyModifiers::NONE);
    assert!(matches!(a.modal, Some(Modal::Help { scroll }) if scroll > 0));
    assert_ne!(draw(&mut a, 80, 24), first);
    key(&mut a, KeyCode::Home, KeyModifiers::NONE);
    assert!(matches!(a.modal, Some(Modal::Help { scroll: 0 })));
    key(&mut a, KeyCode::End, KeyModifiers::NONE);
    assert!(matches!(a.modal, Some(Modal::Help { scroll }) if scroll > 0));
}

#[test]
fn a_mistyped_command_names_the_closest_one() {
    let mut a = app();
    run(&mut a, "/modle");
    let s = draw(&mut a, 120, 30);
    assert!(s.contains("unknown command /modle") && s.contains("did you mean /model"), "{s}");
}

#[test]
fn login_opens_the_setup() {
    let mut a = app();
    run(&mut a, "/login");
    assert!(matches!(a.modal, Some(Modal::Setup(_))));
}

#[test]
fn the_command_menu_opens_on_the_common_commands() {
    let mut a = app();
    key(&mut a, KeyCode::Char('/'), KeyModifiers::NONE);
    let s = draw(&mut a, 120, 34);
    let at = |w: &str| s.find(w).unwrap_or_else(|| panic!("{w} missing\n{s}"));
    assert!(at("/model") < at("/fork") || !s.contains("/fork"), "{s}");
    assert!(at("/permissions") > 0);
}

#[test]
fn a_lone_slash_goes_when_the_menu_is_closed_with_esc() {
    let mut a = app();
    key(&mut a, KeyCode::Char('/'), KeyModifiers::NONE);
    key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(a.input.text(), "");
}

#[test]
fn a_multi_line_paste_into_a_picker_filter_keeps_the_words_apart() {
    let mut a = app();
    a.modal = Some(Modal::Picker(Picker::new(
        PickKind::Model,
        "Model",
        vec![PickItem { label: "a b".into(), value: "1".into(), hint: String::new() }],
        None,
    )));
    nexo_tui::event::handle(&mut a, crossterm::event::Event::Paste("message 0\nmessage 1".into()));
    let s = draw(&mut a, 100, 30);
    assert!(s.contains("message 0 message 1"), "{s}");
}
