//! The hint in the empty draft: queued messages, a key tip, and what happens to it on narrow screens and
//! when the person types.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app_with(hints: bool) -> App {
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
        hints,
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

fn app() -> App {
    app_with(true)
}

fn rows(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    (0..buf.area.height).map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()).collect()
}

fn has(rows: &[String], s: &str) -> bool {
    rows.iter().any(|r| r.contains(s))
}

const SIZES: [(u16, u16); 3] = [(80, 24), (40, 12), (24, 10)];

#[test]
fn waiting_requests_say_how_to_take_one_back_and_the_short_form_fits_narrow_screens() {
    for (w, h) in SIZES {
        let mut a = app();
        a.queued_msgs.push("fix the build".into());
        a.queued = 1;
        let r = rows(&mut a, w, h);
        let full = has(&r, "↑ edit queued messages · Enter sends now");
        let short = has(&r, "↑ edit queued");
        assert!(short, "{w}x{h}: {r:#?}");
        assert_eq!(full, w >= 43, "{w}x{h}: the long form only whole");
        let row = r.iter().find(|l| l.starts_with('›')).unwrap();
        assert!(!row.contains('…'), "{w}x{h}: nothing cut with an ellipsis in the box: {r:#?}");
    }
}

#[test]
fn a_key_tip_comes_after_three_turns_whole_or_not_at_all() {
    for (w, h) in SIZES {
        let mut a = app();
        assert!(has(&rows(&mut a, w, h), "ask anything"));
        a.turns_ended = 2;
        assert!(!has(&rows(&mut a, w, h), "tip:"), "{w}x{h}: not yet");
        a.turns_ended = 3;
        let r = rows(&mut a, w, h);
        if has(&r, "tip:") {
            // whole: the sentence ends with its last word
            assert!(r.iter().any(|l| l.contains("tip: ") && !l.contains('…')), "{w}x{h}: {r:#?}");
        } else {
            assert!(has(&r, "ask anything"), "{w}x{h}: falls back to the plain line: {r:#?}");
        }
    }
    // on a wide screen it does show
    let mut a = app();
    a.turns_ended = 3;
    assert!(has(&rows(&mut a, 100, 30), "tip: "));
}

#[test]
fn typing_replaces_the_hint_and_the_draft_is_never_covered() {
    for (w, h) in SIZES {
        let mut a = app();
        a.turns_ended = 3;
        a.queued_msgs.push("x".into());
        a.queued = 1;
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let r = rows(&mut a, w, h);
        assert!(r.iter().any(|l| l.contains("› hi")), "{w}x{h}: {r:#?}");
        assert!(!has(&r, "edit queued messages") && !has(&r, "tip:") && !has(&r, "ask anything"), "{w}x{h}: {r:#?}");
    }
}

#[test]
fn busy_and_hints_off_say_nothing_extra() {
    let mut a = app();
    a.turns_ended = 3;
    a.busy = true;
    let r = rows(&mut a, 100, 30);
    assert!(has(&r, "joins the turn") && !has(&r, "tip:"), "{r:#?}");

    let mut a = app_with(false);
    a.turns_ended = 3;
    a.queued_msgs.push("x".into());
    a.queued = 1;
    let r = rows(&mut a, 100, 30);
    assert!(!has(&r, "tip:") && !has(&r, "edit queued messages"), "{r:#?}");
}

#[test]
fn a_tip_is_said_at_most_three_times() {
    let mut a = app();
    let mut seen = 0;
    let mut last = String::new();
    for turn in 3..60 {
        a.turns_ended = turn;
        // drawn twice in a turn: counted once
        rows(&mut a, 100, 30);
        let r = rows(&mut a, 100, 30);
        if let Some(l) = r.iter().find(|l| l.contains("tip: ")) {
            if l.contains("searches the conversation") {
                seen += 1;
                last = l.clone();
            }
        }
    }
    assert_eq!(seen, 3, "{last}");
}

#[test]
fn using_a_key_ends_its_tip() {
    let mut a = app();
    a.hint_state.used_action(nexo_tui::keymap::Action::Search);
    for turn in 3..60 {
        a.turns_ended = turn;
        assert!(!has(&rows(&mut a, 100, 30), "searches the conversation"));
    }
}
