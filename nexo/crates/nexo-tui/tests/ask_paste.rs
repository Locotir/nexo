//! A paste into the question modal lands in the answer of one's own, on one line.

use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use nexo_core::permission::{Question, QuestionOption};
use nexo_tui::modal::{Ask, Modal};
use nexo_tui::ui::rows_of;
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
        suggest: false,
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

fn asking(multi: bool) -> App {
    let mut a = app();
    let options = (0..2).map(|i| QuestionOption { label: format!("option {i}"), description: String::new() }).collect();
    a.modal = Some(Modal::Question(Box::new(Ask::new(Question {
        id: "q".into(),
        header: "Approach".into(),
        question: "Which way?".into(),
        options,
        multi,
        plan: None,
    }))));
    a
}

fn screen(a: &mut App) -> String {
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw_frame(&mut t, a).unwrap();
    rows_of(t.backend().buffer()).join("\n")
}

fn other(a: &App) -> &str {
    match &a.modal {
        Some(Modal::Question(q)) => &q.other,
        _ => panic!("no question"),
    }
}

#[test]
fn a_paste_on_the_own_answer_row_shows_on_one_line() {
    for multi in [false, true] {
        let mut a = asking(multi);
        for _ in 0..2 {
            nexo_tui::event::handle(&mut a, Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        }
        nexo_tui::event::handle(&mut a, Event::Paste("pasted first\r\nsecond\tline\n".into()));
        assert_eq!(other(&a), "pasted first second line");
        assert!(screen(&mut a).contains("pasted first second line"), "{}", screen(&mut a));
        nexo_tui::event::handle(&mut a, Event::Key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE)));
        assert_eq!(other(&a), "pasted first second line!");
    }
}

#[test]
fn a_paste_on_an_option_row_is_dropped() {
    let mut a = asking(false);
    nexo_tui::event::handle(&mut a, Event::Paste("stray".into()));
    assert_eq!(other(&a), "");
}
