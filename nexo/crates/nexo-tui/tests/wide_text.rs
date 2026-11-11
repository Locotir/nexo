//! Text that is wider than it looks, or narrower: CJK, flags, emoji joined with joiners, letters with
//! combining marks, a long word, a tab. One key moves or deletes one visible character, nothing is
//! drawn past the right edge, and what a window shows has no joiner or escape that terminals draw in
//! different widths. A paste into a window is taken in one step, a resize keeps what was typed.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::AgentEvent;
use nexo_core::message::Message;
use nexo_core::permission::PermissionRequest;
use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay, ToolKind, ToolOutput};
use nexo_tui::modal::{Modal, PickItem, PickKind, Picker};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

fn app() -> (App, mpsc::UnboundedReceiver<nexo_core::agent::AgentCommand>) {
    // the captures must not depend on the other tools' folders in this machine's home
    std::env::set_var("NEXO_COMPAT_HOME", "");
    let (tx, rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/プロジェクト"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: false,
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
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261102-100000-ef56".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/プロジェクト"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    (a, rx)
}

fn key(a: &mut App, code: KeyCode, mods: KeyModifiers) {
    nexo_tui::event::handle(a, Event::Key(KeyEvent::new(code, mods)));
}

fn type_str(a: &mut App, s: &str) {
    for c in s.chars() {
        key(a, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

/// The screen as rows of text. A wide character always has its second cell on screen, and a dialog
/// titled `title` keeps its right edge on every row (`╮` and `│` in one column).
fn screen_with(a: &mut App, w: u16, h: u16, title: &str) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    let rows = rows_of(&buf);
    for y in 0..h {
        let mut x = 0;
        while x < w {
            let n = UnicodeWidthStr::width(buf[(x, y)].symbol());
            assert!(
                x + (n as u16).max(1) <= w,
                "a wide character cut by the edge at {x},{y} of {w}x{h}: {:?}",
                rows[y as usize]
            );
            x += (n as u16).max(1);
        }
    }
    let cell = |x: u16, y: u16| buf[(x, y)].symbol().to_string();
    let top = (0..h).find(|&y| rows[y as usize].contains(&format!("╭ {title}")));
    if let Some(y) = top {
        let left = (0..w).find(|&x| cell(x, y) == "╭").unwrap();
        let right = (left..w).rev().find(|&x| cell(x, y) == "╮").expect("the dialog's top edge is closed");
        for y2 in y + 1..h {
            if cell(left, y2) == "╰" {
                assert_eq!(cell(right, y2), "╯");
                break;
            }
            assert!(
                cell(left, y2) == "│" && matches!(cell(right, y2).as_str(), "│" | "┃"),
                "row {y2} at {w}x{h}: {:?}",
                rows[y2 as usize]
            );
        }
    }
    rows
}

fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
    screen_with(a, w, h, "\u{0}")
}

const TEXT: &str = "日本語のテキスト 中文字符 👨\u{200d}👩\u{200d}👧 🇪🇸 ✅ e\u{301} ệ مرحبا שלום a\tb";
const FLAG: &str = "\u{1F1EA}\u{1F1F8}";
const E_ACUTE: &str = "e\u{301}";
const VIET: &str = "e\u{323}\u{302}";

#[test]
fn one_arrow_key_crosses_one_visible_character() {
    for what in [FLAG, E_ACUTE, VIET, "日", "👨\u{200d}"] {
        let (mut a, _rx) = app();
        type_str(&mut a, &format!("a{what}b"));
        let n = what.chars().count();
        key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(a.input.editor.cursor().1, 1 + n, "{what:?}: before b");
        key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(a.input.editor.cursor().1, 1, "{what:?}: one Left crossed all of it");
        key(&mut a, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(a.input.editor.cursor().1, 1 + n, "{what:?}: one Right crossed all of it");
    }
}

#[test]
fn backspace_and_delete_take_a_whole_visible_character() {
    for what in [FLAG, E_ACUTE, VIET] {
        let (mut a, _rx) = app();
        type_str(&mut a, &format!("a{what}b"));
        key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        key(&mut a, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(a.input.text(), "ab", "{what:?}: Backspace");
        let (mut a, _rx) = app();
        type_str(&mut a, &format!("a{what}b"));
        for _ in 0..2 {
            key(&mut a, KeyCode::Left, KeyModifiers::NONE);
        }
        key(&mut a, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(a.input.text(), "ab", "{what:?}: Delete");
    }
    // Ctrl+W takes the word, marks and all
    let (mut a, _rx) = app();
    type_str(&mut a, &format!("one tw{E_ACUTE}{E_ACUTE}"));
    key(&mut a, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert_eq!(a.input.text(), "one ");
}

#[test]
fn the_draft_wraps_inside_the_box_at_every_width() {
    for w in [24u16, 40, 60, 80] {
        let (mut a, _rx) = app();
        type_str(&mut a, TEXT);
        type_str(&mut a, &"x".repeat(300));
        let rows = screen(&mut a, w, 24);
        assert!(rows.last().is_some_and(|r| r.contains("nexo")), "the status bar is still there at {w}");
    }
}

#[test]
fn a_reply_with_wide_text_stays_inside_the_screen() {
    for w in [30u16, 60, 80] {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user(TEXT) });
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::TextDelta { text: format!("{TEXT} {}\n", "y".repeat(300)) });
        a.handle_agent(AgentEvent::ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "echo"}),
            summary: format!("echo {TEXT}"),
        });
        a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
        a.handle_agent(AgentEvent::ToolDone {
            id: "c1".into(),
            output: ToolOutput::ok(format!("{TEXT}\n{}", "z".repeat(300))).with_display(ToolDisplay::Shell {
                command: "echo".into(),
                stdout: format!("{TEXT}\n{}", "z".repeat(300)),
                stderr: String::new(),
                exit_code: Some(0),
            }),
            elapsed_ms: 3,
        });
        screen(&mut a, w, 24);
    }
}

fn ask(a: &mut App, summary: &str) {
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            agent: None,
            id: "p1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: summary.into(),
            detail: ToolDisplay::Shell {
                command: summary.into(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            suggested_rules: vec![format!("bash({summary})")],
        },
    });
}

#[test]
fn what_a_window_shows_has_no_joiner_in_it() {
    let joined = "echo 👨\u{200d}👩\u{200d}👧 \u{1F3FD}ok";
    let (mut a, _rx) = app();
    ask(&mut a, joined);
    for w in [40u16, 80] {
        let rows = screen_with(&mut a, w, 24, "permission");
        assert!(rows.iter().all(|r| !r.contains('\u{200d}') && !r.contains('\u{1F3FD}')), "{rows:#?}");
    }
    // the to-do strip and its window
    let (mut a, _rx) = app();
    a.opts.todo_strip = true;
    a.handle_agent(AgentEvent::ToolCall {
        id: "t1".into(),
        name: "todo".into(),
        args: serde_json::json!({"items": [{"text": joined, "status": "in_progress"}, {"text": "日本語\t中文"}]}),
        summary: String::new(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "t1".into() });
    a.handle_agent(AgentEvent::ToolDone { id: "t1".into(), output: ToolOutput::ok("ok"), elapsed_ms: 1 });
    // the list itself comes with the tool's result, not with the call: set it as the app would
    a.todo = vec![
        TodoItem { text: joined.into(), status: TodoStatus::InProgress },
        TodoItem { text: "日本語\t中文".into(), status: TodoStatus::Pending },
    ];
    for w in [40u16, 80] {
        let rows = screen(&mut a, w, 24);
        assert!(rows.iter().any(|r| r.contains("echo")), "the strip shows the item: {rows:#?}");
        assert!(rows.iter().all(|r| !r.contains('\u{200d}') && !r.contains('\t')), "{rows:#?}");
    }
    a.open_todo();
    for w in [40u16, 80] {
        let rows = screen(&mut a, w, 24);
        assert!(rows.iter().any(|r| r.contains("echo")), "the window shows the item: {rows:#?}");
        assert!(rows.iter().all(|r| !r.contains('\u{200d}') && !r.contains('\t')), "{rows:#?}");
    }
}

#[test]
fn a_picker_title_and_a_status_bar_with_wide_names_fit() {
    let (mut a, _rx) = app();
    let items = vec![PickItem {
        label: "プロジェクト 日本語のテキスト 👨\u{200d}👩\u{200d}👧 e\u{301}".repeat(4),
        value: "s1".into(),
        hint: "中文字符".repeat(20),
    }];
    a.modal = Some(Modal::Picker(Picker::new(PickKind::Session, "プロジェクト 中文字符", items, None)));
    for w in [30u16, 60, 80] {
        screen(&mut a, w, 12);
    }
    // the status bar carries the folder name
    let (mut a, _rx) = app();
    for w in [20u16, 40, 60, 80, 200] {
        screen(&mut a, w, 24);
    }
}

#[test]
fn a_huge_paste_into_a_picker_is_one_step_and_a_short_filter() {
    let (mut a, _rx) = app();
    let items = vec![PickItem { label: "one".into(), value: "s1".into(), hint: String::new() }];
    a.modal = Some(Modal::Picker(Picker::new(PickKind::Session, "Sessions", items, None)));
    let big = "abcdefghi\n\t ".repeat(20_000);
    let t = Instant::now();
    nexo_tui::event::handle(&mut a, Event::Paste(big));
    assert!(t.elapsed().as_millis() < 1000, "took {:?}", t.elapsed());
    let Some(Modal::Picker(p)) = &a.modal else { panic!("the window closed") };
    assert!(p.query.chars().count() <= 256 && p.query.starts_with("abcdefghi "), "{:?}", p.query);
    assert!(!p.query.contains(['\n', '\t']));
}

#[test]
fn a_paste_is_whole_and_clean_and_a_resize_keeps_the_draft() {
    let (mut a, mut rx) = app();
    type_str(&mut a, "x ");
    let pasted = "tab\there\r\ncrlf\rcr\x1b[31mred\x1b[0m end\n日本語 👨\u{200d}👩\u{200d}👧\nl3\nl4\nl5\n";
    nexo_tui::event::handle(&mut a, Event::Paste(pasted.into()));
    assert!(rx.try_recv().is_err(), "the line ends of a paste send nothing");
    assert_eq!(a.input.text(), "x [Pasted text #1 +7 lines]");
    let sent = a.input.full_text();
    assert!(!sent.contains(['\x1b', '\r', '\t', '\u{200d}']), "{sent:?}");
    assert!(sent.contains("tab    here\ncrlf\ncrred end\n日本語 "), "{sent:?}");
    // Backspace at the end of the chip takes it whole
    let mut b = a;
    for (w, h) in [(120u16, 40u16), (60, 15), (200, 50), (80, 24), (20, 5), (80, 24)] {
        let rows = screen(&mut b, w, h);
        assert_eq!(rows.len(), h as usize);
    }
    assert!(screen(&mut b, 80, 24).iter().any(|r| r.contains("[Pasted text #1 +7 lines]")));
    key(&mut b, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(b.input.text(), "x ");
}

#[test]
fn a_dialog_stays_inside_the_screen_through_every_resize() {
    let (mut a, _rx) = app();
    ask(&mut a, &format!("echo {TEXT}"));
    for (w, h) in [(120u16, 40u16), (60, 15), (200, 50), (80, 24), (20, 5), (80, 24), (1, 1), (80, 24)] {
        let rows = screen_with(&mut a, w, h, "permission");
        assert_eq!(rows.len(), h as usize);
    }
    let rows = screen(&mut a, 80, 24);
    assert!(rows.iter().any(|r| r.contains("[y] allow")), "{rows:#?}");
    let _ = PathBuf::new();
}
