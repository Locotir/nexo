//! What the screen must never show, whatever arrives and whatever the size: raw control bytes, a frame
//! that panics, a cursor off the screen, a box left open. Each scenario is drawn at many sizes.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use nexo_core::agent::{AgentEvent, Level, TurnEnd};
use nexo_core::message::Message;
use nexo_core::stream::Usage;
use nexo_core::tool::{ToolDisplay, ToolOutput};
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(animations: bool) -> App {
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
        keys: Default::default(),
        notify_after: 15,
        animations,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        session_id: Some("20261101-120000-cd34".into()),
        sessions: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a.handle_agent(AgentEvent::Ready {
        session_id: "20261101-120000-cd34".into(),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        cwd: PathBuf::from("/tmp/project"),
        permission_mode: nexo_core::permission::PermissionMode::Ask,
    });
    a
}

/// Text that breaks naive renderers: tabs, carriage returns, colour escapes, bells, wide and joined
/// characters, combining marks, right-to-left text and a word longer than any screen.
const HOSTILE: &str =
    "tab\there\tand\tthere\r\nprogress 10%\rprogress 100%\n\x1b[31mred\x1b[0m \x1b[1;4mbold\x1b[0m \x07bell\n\
宽字符测试 漢字 かな カナ 한국어\n👨‍👩‍👧‍👦 family 🏳️‍🌈 flag 👍🏽 thumb\ne\u{301} n\u{303} combining\n\
مرحبا بالعالم rtl\nzero\u{200b}width\u{feff}marks\n";

fn long_word() -> String {
    "x".repeat(700)
}

fn tool(a: &mut App, id: &str, name: &str, summary: &str, out: ToolOutput) {
    a.handle_agent(AgentEvent::ToolCall {
        id: id.into(),
        name: name.into(),
        args: serde_json::json!({"command": summary}),
        summary: summary.into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: id.into() });
    a.handle_agent(AgentEvent::ToolDone { id: id.into(), output: out, elapsed_ms: 7 });
}

fn hostile(a: &mut App) {
    a.handle_agent(AgentEvent::History { index: 0, message: Message::user(format!("look at this\t{HOSTILE}")) });
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    a.handle_agent(AgentEvent::ReasoningDelta { text: format!("thinking {HOSTILE}") });
    a.handle_agent(AgentEvent::TextDelta { text: format!("Here:\n\n{HOSTILE}\n`{}`\n", long_word()) });
    let shell = ToolDisplay::Shell {
        command: "cargo test\t--color=always".into(),
        stdout: format!("\x1b[32mok\x1b[0m\t1\n{HOSTILE}{}\n", long_word()),
        stderr: "\x1b[33mwarning\x1b[0m: \r\x1b[2Kunused\n".into(),
        exit_code: Some(101),
    };
    tool(a, "s1", "bash", "cargo test\t--color=always", ToolOutput::ok(HOSTILE).with_display(shell));
    let file = ToolDisplay::File {
        path: "src/宽.rs".into(),
        text: format!("\tfn a() {{}}\n{HOSTILE}"),
        start_line: 9_999_990,
    };
    tool(a, "f1", "read", "src/宽.rs", ToolOutput::ok(HOSTILE).with_display(file));
    let diff = ToolDisplay::Diff {
        path: "src/\x1b[31mevil\x1b[0m.rs".into(),
        old: format!("\tlet a = 1;\r\n{HOSTILE}"),
        new: format!("\tlet a = 2;\r\n{}\n", long_word()),
        start_line: 1,
    };
    tool(a, "d1", "edit", "src/evil.rs", ToolOutput::ok("edited").with_display(diff));
    let list = ToolDisplay::List { items: vec![HOSTILE.into(), long_word(), String::new(), "\t".into()] };
    tool(a, "l1", "glob", "**/*", ToolOutput::ok("").with_display(list));
    a.handle_agent(AgentEvent::ToolCall {
        id: "p1".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        summary: "long".into(),
    });
    a.handle_agent(AgentEvent::ToolStart { id: "p1".into() });
    a.handle_agent(AgentEvent::ToolProgress { id: "p1".into(), line: format!("\x1b[2K\r{HOSTILE}") });
    a.handle_agent(AgentEvent::Notice { level: Level::Warn, text: format!("notice\t{HOSTILE}") });
    a.handle_agent(AgentEvent::Notice { level: Level::Error, text: long_word() });
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: u64::MAX / 4, output: 9_999_999_999, cache_read: 1, ..Default::default() },
        call: Usage { input: u64::MAX / 4, output: 9_999_999_999, cache_read: 1, ..Default::default() },
        session: Usage { input: u64::MAX / 4, output: 9_999_999_999, cache_read: 1, ..Default::default() },
        cost_usd: 123_456.789,
    });
    a.handle_agent(AgentEvent::Context {
        view_tokens: 9_000_000,
        history_tokens: 1,
        history_bytes: 0,
        view_bytes: 0,
        messages_in_view: 0,
        messages_total: 0,
        policy: "curator".into(),
        note: Some(HOSTILE.into()),
        label: format!("curator\t{HOSTILE}"),
        detail: None,
    });
}

/// Every cell holds printable text: no control byte reaches the terminal, where it would move the
/// cursor, ring, change colours or break the frame.
fn check(a: &mut App, w: u16, h: u16, what: &str) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..h {
        for x in 0..w {
            let s = buf[(x, y)].symbol();
            assert!(
                !s.chars().any(|c| c.is_control() || c == '\u{feff}' || c == '\u{200b}'),
                "{what} at {w}x{h}: control or invisible char {s:?} at ({x},{y})\n{}",
                rows_of(&buf).join("\n")
            );
        }
    }
    if let Ok(p) = t.get_cursor_position() {
        assert!(p.x < w.max(1) && p.y < h.max(1), "{what} at {w}x{h}: cursor at {p:?}");
    }
    let rows = rows_of(&buf);
    if std::env::var("NEXO_DUMP").is_ok() {
        eprintln!("==== {what} {w}x{h}\n{}", rows.join("\n"));
    }
    rows
}

const SIZES: &[(u16, u16)] =
    &[(1, 1), (2, 2), (5, 3), (10, 4), (16, 6), (24, 8), (40, 12), (60, 20), (80, 24), (120, 40), (200, 60)];

#[test]
fn hostile_text_never_reaches_the_terminal_raw_at_any_size() {
    let mut a = app(false);
    hostile(&mut a);
    for &(w, h) in SIZES {
        check(&mut a, w, h, "hostile");
    }
    // every block open, then every block folded
    a.fold_all();
    for &(w, h) in SIZES {
        check(&mut a, w, h, "folded");
    }
}

#[test]
fn hostile_text_in_every_overlay_and_modal() {
    use nexo_tui::commands::Command;
    let mut a = app(false);
    hostile(&mut a);
    for c in [Command::Context, Command::Help] {
        a.run_command(c);
        for &(w, h) in SIZES {
            check(&mut a, w, h, "modal");
        }
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
    // the palette and the search, typed into
    for open in [
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
    ] {
        nexo_tui::event::key(&mut a, open);
        for ch in "宽\tx👍🏽".chars() {
            nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        for &(w, h) in SIZES {
            check(&mut a, w, h, "palette/search");
        }
        nexo_tui::event::key(&mut a, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
}

#[test]
fn a_pasted_hostile_text_in_the_editor_stays_printable() {
    let mut a = app(false);
    nexo_tui::event::handle(&mut a, crossterm::event::Event::Paste(format!("{HOSTILE}{}", long_word())));
    for &(w, h) in SIZES {
        check(&mut a, w, h, "editor");
    }
}

/// A long run of mixed events, keys, mouse and resizes, with the animations on: nothing panics and
/// every frame keeps its invariants.
#[test]
fn a_storm_of_events_and_resizes_keeps_every_frame_whole() {
    let mut a = app(true);
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut rnd = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n.max(1)
    };
    let keys = [
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Char('x'),
        KeyCode::Char('宽'),
        KeyCode::Backspace,
        KeyCode::Left,
        KeyCode::Right,
    ];
    for step in 0..400u64 {
        match rnd(9) {
            0 => a.handle_agent(AgentEvent::TextDelta {
                text: format!("word{step} {} ", if step % 7 == 0 { HOSTILE } else { "" }),
            }),
            1 => tool(
                &mut a,
                &format!("t{step}"),
                "bash",
                "ls",
                ToolOutput::ok("a\nb").with_display(ToolDisplay::Shell {
                    command: "ls".into(),
                    stdout: (0..rnd(60)).map(|i| format!("line {i}\t\x1b[1mx\x1b[0m\n")).collect(),
                    stderr: String::new(),
                    exit_code: Some(rnd(3) as i32),
                }),
            ),
            2 => {
                a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("done") });
                a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
                a.handle_agent(AgentEvent::TurnStart { turn: step as u32 });
            }
            3 => {
                let k = keys[rnd(keys.len() as u64) as usize];
                let m = if rnd(4) == 0 { KeyModifiers::CONTROL } else { KeyModifiers::NONE };
                // ctrl+c/ctrl+d would quit; keep the session alive
                if !(m == KeyModifiers::CONTROL && matches!(k, KeyCode::Char('c') | KeyCode::Char('d'))) {
                    nexo_tui::event::key(&mut a, KeyEvent::new(k, m));
                }
            }
            4 => {
                let kind =
                    [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown, MouseEventKind::Moved][rnd(3) as usize];
                let ev = crossterm::event::MouseEvent {
                    kind,
                    column: rnd(200) as u16,
                    row: rnd(60) as u16,
                    modifiers: KeyModifiers::NONE,
                };
                nexo_tui::event::handle(&mut a, crossterm::event::Event::Mouse(ev));
            }
            5 => a.handle_agent(AgentEvent::ReasoningDelta { text: format!("hmm {step}\n") }),
            6 => a.handle_agent(AgentEvent::Context {
                view_tokens: rnd(200_000),
                history_tokens: rnd(400_000),
                history_bytes: 0,
                view_bytes: 0,
                messages_in_view: rnd(100) as _,
                messages_total: 100,
                policy: "curator".into(),
                note: None,
                label: "curator".into(),
                detail: None,
            }),
            7 => a.tick(),
            _ => a.handle_agent(AgentEvent::Notice { level: Level::Info, text: format!("note {step}") }),
        }
        let (w, h) = (1 + rnd(180) as u16, 1 + rnd(55) as u16);
        check(&mut a, w, h, &format!("storm step {step}"));
    }
}

#[test]
fn a_progress_bar_shows_its_last_state_and_the_bar_has_no_lone_separator() {
    let mut a = app(false);
    hostile(&mut a);
    let all = check(&mut a, 120, 60, "progress").join("\n");
    assert!(!all.contains("[31m") && !all.contains("[0m"), "no piece of a colour escape is left: {all}");
    // a diff shows the file as it is; everywhere else the line shows its last state
    let shown =
        all.lines().filter(|l| !l.contains(" - progress") && !l.contains(" + progress")).collect::<Vec<_>>().join("\n");
    assert!(shown.contains("progress 100%") && !shown.contains("progress 10%"), "{all}");
    for &(w, h) in SIZES.iter().filter(|s| s.0 >= 16) {
        let rows = check(&mut a, w, h, "bar");
        let bar = &rows[rows.len() - 1];
        assert!(!bar.contains("· ·") && !bar.contains("│ │") && !bar.trim_end().ends_with('·'), "{w}x{h}: {bar}");
    }
}

#[test]
fn under_the_minimum_size_a_clean_notice_replaces_the_screen() {
    let mut a = app(false);
    hostile(&mut a);
    let rows = check(&mut a, 20, 5, "tiny");
    assert!(rows.iter().any(|r| r.contains("window too small")), "{}", rows.join("\n"));
    assert!(rows.iter().any(|r| r.contains("has 20x5")), "{}", rows.join("\n"));
    // back to a usable size: the notice is gone
    let rows = check(&mut a, 80, 24, "usable");
    assert!(!rows.iter().any(|r| r.contains("window too small")));
}

#[test]
fn a_short_window_gives_the_keys_line_to_the_text() {
    let mut a = app(false);
    let tall = check(&mut a, 80, 24, "tall");
    let keys = &tall[tall.len() - 2];
    assert!(keys.contains("Enter send"), "{keys}");
    let short = check(&mut a, 80, 10, "short");
    assert!(!short.join("\n").contains("Enter send"), "{}", short.join("\n"));
}
