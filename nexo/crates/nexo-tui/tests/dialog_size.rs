//! A dialog keeps the size it took when it opened: moving the selection (or typing, or
//! switching a tab) never stretches or shrinks the box. It is worked out again only when the
//! terminal is resized or the dialog is replaced.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::supervise::{AgentRow, SourceKind, State};
use nexo_core::agent::AgentEvent;
use nexo_core::permission::{PermissionRequest, Question, QuestionOption};
use nexo_core::tool::{ToolDisplay, ToolKind};
use nexo_tui::agents_view::AgentsView;
use nexo_tui::modal::setup::Setup;
use nexo_tui::modal::{Ask, Modal, PickItem, PickKind, Picker};
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use tokio::sync::mpsc;

const NOW: i64 = 1_000_000;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: (0..12).map(|i| ("deepseek".to_string(), format!("model-{i}-with-a-long-name"))).collect(),
        theme: "dark".into(),
        animations: false,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn press(a: &mut App, code: KeyCode) {
    event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn draw(t: &mut Terminal<TestBackend>, a: &mut App) {
    draw_frame(t, a).unwrap();
}

/// The rectangle of the dialog (or the palette) drawn in the last frame.
fn rect(a: &App) -> Rect {
    if a.palette.is_some() {
        a.dialog_size.palette.drawn
    } else {
        a.dialog_size.modal.drawn
    }
}

/// Opens with `open`, draws, presses `keys` one by one drawing after each, and checks that the
/// rectangle never changes.
fn stays(name: &str, w: u16, h: u16, mut a: App, keys: &[KeyCode]) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw(&mut t, &mut a);
    let first = rect(&a);
    assert!(first.width >= 5 && first.height >= 3, "{name} {w}x{h}: nothing drawn ({first:?})");
    for (i, k) in keys.iter().enumerate() {
        press(&mut a, *k);
        draw(&mut t, &mut a);
        assert_eq!(rect(&a), first, "{name} {w}x{h}: the box changed after key {i} ({k:?})");
    }
}

fn downs(n: usize) -> Vec<KeyCode> {
    vec![KeyCode::Down; n]
}

fn picker() -> Modal {
    let items = (0..30)
        .map(|i| PickItem {
            label: if i % 3 == 0 {
                format!("provider/model-{i}-with-a-very-long-name-indeed-{i}")
            } else {
                format!("m{i}")
            },
            value: i.to_string(),
            hint: if i % 2 == 0 { "64k ctx · $0.28 in · $0.42 out · vision · tools".into() } else { "8k".into() },
        })
        .collect();
    Modal::Picker(Picker::new(PickKind::Model, "Model", items, None))
}

fn question(plan: bool) -> Modal {
    let options = (0..5)
        .map(|i| QuestionOption {
            label: format!("option {i}"),
            description: "a long description that wraps over more than one line of the dialog ".repeat(i),
        })
        .collect();
    Modal::Question(Box::new(Ask::new(Question {
        id: "q".into(),
        header: "Approach".into(),
        question: "Which way should the change go?".into(),
        options,
        multi: false,
        plan: plan.then(|| "# Plan\n\n1. first\n2. second\n3. third\n".repeat(6)),
    })))
}

fn row(key: &str, name: &str, state: State) -> AgentRow {
    AgentRow {
        key: key.into(),
        name: name.into(),
        source: SourceKind::External,
        program: "agent".into(),
        state,
        since: NOW - 60,
        last_step: Some("Edit  src/parser.rs".into()),
        idle_secs: Some(30),
        cost_usd: None,
        model: None,
        cwd: Some(format!("/home/dev/{name}")),
        permission_mode: Some("default".into()),
        parent: None,
        paused: false,
        last_action: None,
        ribbon: vec![],
        spark: vec![],
        todo: None,
        tests: None,
        subs: vec![],
        unread: 0,
        held: 0,
        retry_secs: None,
    }
}

const SIZES: [(u16, u16); 4] = [(60, 20), (100, 30), (160, 50), (200, 60)];

#[test]
fn a_picker_keeps_its_size_while_the_selection_moves_and_the_filter_changes() {
    for (w, h) in SIZES {
        let mut a = app();
        a.modal = Some(picker());
        let mut keys = downs(12);
        keys.extend([KeyCode::Char('m'), KeyCode::Char('9'), KeyCode::Backspace, KeyCode::Backspace]);
        keys.extend(downs(5));
        keys.extend([KeyCode::Up, KeyCode::PageDown, KeyCode::End]);
        stays("picker", w, h, a, &keys);
    }
}

#[test]
fn a_permission_keeps_its_size_while_it_scrolls() {
    for (w, h) in SIZES {
        let mut a = app();
        a.handle_agent(AgentEvent::PermissionAsk {
            request: PermissionRequest {
                id: "p1".into(),
                tool: "bash".into(),
                kind: ToolKind::Exec,
                summary: "cargo test".into(),
                detail: ToolDisplay::Shell {
                    command: "cargo test --workspace --all-features -- --nocapture ".repeat(8),
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: None,
                },
                suggested_rules: vec!["bash(cargo test*)".into(), "bash(cargo *)".into(), "bash".into()],
            },
        });
        stays("permission", w, h, a, &downs(8));
    }
}

#[test]
fn a_question_keeps_its_size_through_every_option_and_the_typed_answer() {
    for plan in [false, true] {
        for (w, h) in SIZES {
            let mut a = app();
            a.modal = Some(question(plan));
            let mut keys = downs(5);
            // the row for one's own answer takes the keys: type a long line, then leave it
            keys.push(KeyCode::Enter);
            keys.extend(
                "a long answer of my own that wraps over several lines of the dialog ".chars().map(KeyCode::Char),
            );
            keys.extend([KeyCode::Esc, KeyCode::Up, KeyCode::Up]);
            stays(if plan { "plan" } else { "question" }, w, h, a, &keys);
        }
    }
}

#[test]
fn the_help_keeps_its_size_while_it_scrolls() {
    for (w, h) in SIZES {
        let mut a = app();
        a.modal = Some(Modal::Help { scroll: 0 });
        stays("help", w, h, a, &downs(10));
    }
}

#[test]
fn the_model_setup_keeps_its_size_through_its_list() {
    for (w, h) in SIZES {
        let mut a = app();
        let models: Vec<(String, String)> = (0..12).map(|i| ("deepseek".to_string(), format!("model-{i}"))).collect();
        a.modal = Some(Modal::Setup(Box::new(Setup::new(vec![], models, "deepseek/model-0"))));
        let mut keys = downs(8);
        keys.extend([KeyCode::Up, KeyCode::Up]);
        stays("setup", w, h, a, &keys);
    }
}

#[test]
fn the_usage_window_keeps_its_size_through_its_tabs_and_scroll() {
    for (w, h) in SIZES {
        let mut a = app();
        a.modal = Some(Modal::Info(Box::new(nexo_tui::overlay::Info::new(0).still(true))));
        let mut keys = downs(4);
        keys.extend([KeyCode::Tab, KeyCode::Down, KeyCode::Tab, KeyCode::Down, KeyCode::BackTab, KeyCode::Up]);
        stays("info", w, h, a, &keys);
    }
}

#[test]
fn the_panel_keeps_its_size_while_the_selection_moves_and_sessions_come() {
    for (w, h) in SIZES {
        let mut a = app();
        let rows: Vec<AgentRow> =
            (0..4).map(|i| row(&format!("s-{i}"), &format!("task-{i}"), State::Working)).collect();
        a.modal = Some(Modal::Agents(Box::new(AgentsView::with_rows(rows, NOW, true))));
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        draw(&mut t, &mut a);
        let first = rect(&a);
        assert!(first.width >= 20, "panel {w}x{h}: {first:?}");
        for i in 0..6 {
            press(&mut a, KeyCode::Down);
            if i == 2 {
                // sessions appear while it is open: the window does not grow for them
                if let Some(Modal::Agents(v)) = &mut a.modal {
                    let more: Vec<AgentRow> =
                        (0..9).map(|i| row(&format!("n-{i}"), &format!("late-{i}"), State::Working)).collect();
                    v.set_rows(more);
                }
            }
            draw(&mut t, &mut a);
            assert_eq!(rect(&a), first, "panel {w}x{h}: changed after key {i}");
        }
    }
}

#[test]
fn the_palette_keeps_its_size_while_the_selection_moves() {
    for (w, h) in SIZES {
        for text in ["/", "/m", "@"] {
            let mut a = app();
            a.input.set_text(text);
            a.refresh_palette();
            if text == "@" {
                a.palette = Some(nexo_tui::palette::Palette::files(
                    "",
                    1,
                    (0..40).map(|i| format!("crates/nexo-tui/src/very/deep/folder-{i}/file-{i}.rs")).collect(),
                ));
            }
            assert!(a.palette.is_some(), "{text}: no palette");
            let mut keys = downs(15);
            keys.extend([KeyCode::Up, KeyCode::Up, KeyCode::PageDown, KeyCode::PageUp]);
            stays(&format!("palette {text}"), w, h, a, &keys);
        }
    }
}

#[test]
fn a_resize_works_the_size_out_again_and_a_replaced_dialog_starts_afresh() {
    let mut a = app();
    a.modal = Some(picker());
    let mut small = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut small, &mut a);
    let on_small = rect(&a);
    let mut big = Terminal::new(TestBackend::new(160, 50)).unwrap();
    draw(&mut big, &mut a);
    let on_big = rect(&a);
    assert!(on_big.width > on_small.width && on_big.height > on_small.height, "{on_small:?} -> {on_big:?}");
    // the same dialog, shrunk again: it fits the screen
    draw(&mut small, &mut a);
    assert_eq!(rect(&a), on_small);
    // another dialog in its place: its own size
    a.modal = Some(Modal::Picker(Picker::new(
        PickKind::Model,
        "Other",
        vec![PickItem { label: "one".into(), value: "1".into(), hint: String::new() }],
        None,
    )));
    draw(&mut big, &mut a);
    assert!(rect(&a).height < on_big.height, "{:?} vs {on_big:?}", rect(&a));
}

#[test]
fn an_edit_permission_names_the_file_and_shows_old_and_new_at_80x24() {
    let mut a = app();
    a.handle_agent(AgentEvent::PermissionAsk {
        request: PermissionRequest {
            id: "p2".into(),
            tool: "edit".into(),
            kind: ToolKind::Write,
            summary: "edit src/lib.rs".into(),
            detail: ToolDisplay::List {
                items: [
                    "src/lib.rs (6 changes)",
                    "- let a = 1;",
                    "+ let a = 2;",
                    "- fn old()",
                    "+ fn new()",
                    "+4 more",
                ]
                .map(String::from)
                .to_vec(),
            },
            suggested_rules: vec!["edit".into()],
        },
    });
    let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    let buf = t.backend().buffer().clone();
    let rows: Vec<String> =
        (0..24u16).map(|y| (0..80u16).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect();
    let all = rows.join("\n");
    for want in ["src/lib.rs (6 changes)", "- let a = 1;", "+ let a = 2;", "- fn old()", "+ fn new()", "+4 more"] {
        assert!(all.contains(want), "{want:?} missing\n{all}");
    }
    // the old line is red, the new one green, the count dim: three different colours
    let at = |text: &str| {
        let y = rows.iter().position(|r| r.contains(text)).unwrap() as u16;
        let x = rows[y as usize].chars().position(|c| c == text.chars().next().unwrap()).unwrap() as u16;
        buf[(x, y)].fg
    };
    let (del, add, more) = (at("- let a"), at("+ let a"), at("+4 more"));
    assert!(del != add && add != more && del != more, "{del:?} {add:?} {more:?}");
}
