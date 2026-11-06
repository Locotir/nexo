//! Copying from the transcript takes the content of the box the drag started in (a code block, a
//! tool's output, a diff, a file view), joins the rows the layout wrapped, and leaves the
//! decoration behind. A drag that starts in prose crosses boxes whole; Alt or Ctrl drag is free.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use nexo_core::tool::{ToolDisplay, ToolOutput};
use nexo_tui::render::blocks::{Block, ToolStatus};
use nexo_tui::{App, TuiOptions};
use ratatui::layout::Rect;
use tokio::sync::mpsc;

fn app() -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![],
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
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        session_id: None,
        sessions: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn reply(a: &mut App, text: &str) {
    a.push(Block::Assistant { text: text.into(), streaming: false, at: None, started: None, took_ms: None });
}

fn tool(a: &mut App, name: &str, output: ToolOutput) {
    a.push(Block::Tool {
        id: format!("t{}", a.blocks.len()),
        name: name.into(),
        summary: format!("{name} x"),
        args: serde_json::Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(output),
        started: None,
        elapsed_ms: None,
        folded: false,
    });
}

fn shown(a: &mut App, w: u16, h: u16) {
    a.transcript_area = Rect::new(0, 0, w, h);
    a.layout(w as usize);
    a.settle(h as usize);
}

/// The absolute line whose text contains `needle`.
fn find(a: &mut App, needle: &str) -> usize {
    (0..a.total_lines)
        .find(|l| a.line_text(*l).is_some_and(|t| t.contains(needle)))
        .unwrap_or_else(|| panic!("no line with {needle:?}"))
}

fn row(a: &App, line: usize) -> u16 {
    (line - a.view_top()) as u16
}

/// Press at `(col, line)`, drag to `(col2, line2)` and read what would be copied.
fn drag(a: &mut App, from: (u16, usize), to: (u16, usize)) -> String {
    a.press_at(from.0, row(a, from.1));
    a.drag_to(to.0, row(a, to.1));
    a.selection_text().unwrap_or_default()
}

const CODE: &str = "Before text\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nAfter text";

#[test]
fn drag_in_fence_past_its_end_copies_only_code() {
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let (first, after) = (find(&mut a, "fn main"), find(&mut a, "After"));
    let got = drag(&mut a, (0, first), (30, after));
    assert_eq!(got, "fn main() {\n    let x = 1;\n}");
    for bad in ["rust", "▎", "Before", "After"] {
        assert!(!got.contains(bad), "{got}");
    }
}

#[test]
fn drag_above_the_fence_stays_at_its_start() {
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let (mid, before) = (find(&mut a, "let x"), find(&mut a, "Before"));
    let got = drag(&mut a, (40, mid), (0, before));
    assert_eq!(got, "fn main() {\n    let x = 1;");
}

#[test]
fn drag_from_prose_across_a_fence_copies_prose_and_code() {
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let (before, after) = (find(&mut a, "Before"), find(&mut a, "After"));
    let got = drag(&mut a, (0, before), (40, after));
    let rows: Vec<&str> = got.lines().collect();
    assert_eq!(rows[0], "Before text");
    assert!(!got.contains('─'), "the rule over the code is screen, not text: {got}");
    assert!(got.contains("fn main() {\n    let x = 1;\n}"), "{got}");
    assert!(!got.contains('▎'), "{got}");
    assert_eq!(*rows.last().unwrap(), "After text");
}

#[test]
fn wrapped_prose_copies_as_one_line() {
    let mut a = app();
    let text = "alpha  beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi";
    reply(&mut a, text);
    shown(&mut a, 40, 20);
    let first = find(&mut a, "alpha");
    let end = a.total_lines - 2;
    assert!(end > first);
    let got = drag(&mut a, (0, first), (39, end));
    assert_eq!(got, text);
}

#[test]
fn tool_output_region_clamps() {
    let mut a = app();
    let body = (1..=6).map(|i| format!("out{i}")).collect::<Vec<_>>().join("\n");
    tool(
        &mut a,
        "bash",
        ToolOutput {
            content: body.clone(),
            is_error: false,
            display: ToolDisplay::Shell {
                command: "echo hi".into(),
                stdout: body,
                stderr: String::new(),
                exit_code: Some(0),
            },
        },
    );
    shown(&mut a, 60, 30);
    let (cmd, o2, o6) = (find(&mut a, "$ echo hi"), find(&mut a, "out2"), find(&mut a, "out6"));
    let up = drag(&mut a, (30, o2), (0, 0));
    assert_eq!(up, "out1\nout2");
    let sel = a.selection.clone().unwrap();
    let (s, e) = sel.range();
    assert!(s.line > cmd && e.line <= o6, "{s:?} {e:?}");
    a.selection = None;
    let past = a.total_lines + 3;
    let down = drag(&mut a, (0, o2), (5, past));
    assert_eq!(down, "out2\nout3\nout4\nout5\nout6");
}

#[test]
fn diff_copy_keeps_signs_drops_numbers() {
    let mut a = app();
    tool(
        &mut a,
        "edit",
        ToolOutput {
            content: "ok".into(),
            is_error: false,
            display: ToolDisplay::Diff {
                path: "a.txt".into(),
                old: "one\ntwo\nthree\n".into(),
                new: "one\n2\nthree\n".into(),
                start_line: 1,
            },
        },
    );
    shown(&mut a, 60, 30);
    let (del, last) = (find(&mut a, "- two"), a.total_lines - 1);
    let got = drag(&mut a, (0, del), (50, last));
    assert_eq!(got, "- two\n+ 2\n  three");
    assert!(!got.contains('│'));
}

#[test]
fn file_view_copy_uses_source_for_truncated_lines() {
    let mut a = app();
    let long = "let a_very_long_binding_name_that_does_not_fit = compute(first_argument, second_argument);";
    let text = format!("fn f() {{\n    {long}\n}}\n");
    tool(
        &mut a,
        "read",
        ToolOutput {
            content: text.clone(),
            is_error: false,
            display: ToolDisplay::File { path: "a.rs".into(), text, start_line: 1 },
        },
    );
    shown(&mut a, 40, 30);
    let (first, last) = (find(&mut a, "fn f"), a.total_lines - 1);
    assert!(a.line_text(first + 1).unwrap().contains('…'), "the screen cuts the line");
    let got = drag(&mut a, (0, first), (39, last));
    assert_eq!(got, format!("fn f() {{\n    {long}\n}}"));
}

#[test]
fn triple_click_copies_whole_code_block() {
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let mid = find(&mut a, "let x");
    for _ in 0..3 {
        a.press_at(12, row(&a, mid));
    }
    assert_eq!(a.selection_text().unwrap(), "fn main() {\n    let x = 1;\n}");
}

#[test]
fn double_click_word_stays_in_its_region() {
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let mid = find(&mut a, "let x");
    let line = a.line_text(mid).unwrap();
    let col = line.find("let").unwrap();
    let col = line[..col].chars().count() as u16 + 1;
    a.press_at(col, row(&a, mid));
    a.release_at(col, row(&a, mid));
    a.press_at(col, row(&a, mid));
    assert_eq!(a.selection_text().unwrap(), "let");
    // the rail of the box is not a word
    a.selection = None;
    a.press_at(2, row(&a, mid));
    a.release_at(2, row(&a, mid));
    a.press_at(2, row(&a, mid));
    assert!(a.selection.is_none());
}

#[test]
fn triple_click_in_prose_copies_logical_line() {
    let mut a = app();
    let text = "alpha  beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi";
    reply(&mut a, text);
    shown(&mut a, 40, 20);
    let second = find(&mut a, "alpha") + 1;
    for _ in 0..3 {
        a.press_at(5, row(&a, second));
    }
    assert_eq!(a.selection_text().unwrap(), text);
}

fn mouse(a: &mut App, kind: MouseEventKind, x: u16, line: usize, mods: KeyModifiers) {
    let y = row(a, line);
    nexo_tui::event::mouse(a, MouseEvent { kind, column: x, row: y, modifiers: mods });
}

#[test]
fn alt_and_ctrl_drag_are_free() {
    for mods in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
        let mut a = app();
        reply(&mut a, CODE);
        shown(&mut a, 60, 30);
        let (first, after) = (find(&mut a, "fn main"), find(&mut a, "After"));
        mouse(&mut a, MouseEventKind::Down(MouseButton::Left), 0, first, mods);
        mouse(&mut a, MouseEventKind::Drag(MouseButton::Left), 30, after, mods);
        let got = a.selection_text().unwrap();
        assert!(got.contains("fn main") && got.contains("After text"), "{mods:?}: {got}");
        assert!(a.selection.as_ref().unwrap().free);
    }
    // without a modifier the same drag stays in the box
    let mut a = app();
    reply(&mut a, CODE);
    shown(&mut a, 60, 30);
    let (first, after) = (find(&mut a, "fn main"), find(&mut a, "After"));
    mouse(&mut a, MouseEventKind::Down(MouseButton::Left), 0, first, KeyModifiers::NONE);
    mouse(&mut a, MouseEventKind::Drag(MouseButton::Left), 30, after, KeyModifiers::NONE);
    assert!(!a.selection_text().unwrap().contains("After"));
}

#[test]
fn folded_group_members_are_not_selectable() {
    let mut a = app();
    for n in 0..2 {
        tool(&mut a, "bash", ToolOutput::ok(format!("hidden{n}\nmore")));
    }
    shown(&mut a, 60, 30);
    let members: Vec<usize> = (0..a.blocks.len()).filter(|i| matches!(a.blocks[*i], Block::Tool { .. })).collect();
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|i| a.hidden(*i)));
    for line in 0..a.total_lines {
        assert!(a.region_at(line).is_none());
        assert!(!a.line_text(line).unwrap_or_default().contains("hidden"));
    }
}

#[test]
fn dragging_in_a_long_session_stays_fast() {
    let mut a = app();
    for i in 0..3000 {
        reply(&mut a, &format!("answer {i}\n\n```rust\nfn f{i}() {{}}\n```\n"));
    }
    shown(&mut a, 80, 40);
    a.scroll_to_end();
    a.settle(40);
    let line = find_last(&mut a, "fn f2999");
    let t = Instant::now();
    a.press_at(0, row(&a, line));
    for i in 0..200u16 {
        a.drag_to(i % 60, (i % 30) + 2);
    }
    assert!(a.selection_text().is_some());
    assert!(t.elapsed().as_millis() < 1500, "{:?}", t.elapsed());
}

fn find_last(a: &mut App, needle: &str) -> usize {
    (0..a.total_lines)
        .rev()
        .find(|l| a.line_text(*l).is_some_and(|t| t.contains(needle)))
        .unwrap_or_else(|| panic!("no line with {needle:?}"))
}

#[test]
fn fence_with_tabs_copies_its_tabs() {
    let mut a = app();
    reply(&mut a, "Before\n\n```js\nif (a) {\n\treturn [1,\n\t\t2];\n}\n```\n\nAfter");
    shown(&mut a, 60, 30);
    let (first, after) = (find(&mut a, "if (a)"), find(&mut a, "After"));
    let got = drag(&mut a, (0, first), (30, after));
    assert_eq!(got, "if (a) {\n\treturn [1,\n\t\t2];\n}");
}

#[test]
fn file_view_partial_selection_with_tabs_is_aligned() {
    let mut a = app();
    let text = "fn f() {\n\tlet x = 1;\n\t\tlet yy = 22;\n}\n".to_string();
    tool(
        &mut a,
        "read",
        ToolOutput {
            content: text.clone(),
            is_error: false,
            display: ToolDisplay::File { path: "a.rs".into(), text, start_line: 1 },
        },
    );
    shown(&mut a, 60, 30);
    let l = find(&mut a, "let x");
    let t = a.line_text(l).unwrap();
    let c = nexo_tui::select::width_of(&t[..t.find("let").unwrap()]) as u16;
    let got = drag(&mut a, (c, l), (c + 4, l));
    assert_eq!(got, "let x");
}

#[test]
fn table_copies_cells_not_the_screen() {
    let mut a = app();
    reply(&mut a, "Intro\n\n| a | b |\n|---|---|\n| 1 | dos |\n\nFin");
    shown(&mut a, 60, 30);
    let (first, fin) = (find(&mut a, "dos") - 2, find(&mut a, "Fin"));
    let got = drag(&mut a, (3, first), (20, fin - 1));
    assert_eq!(got, "a\tb\n1\tdos");
}

#[test]
fn quick_click_then_drag_is_a_plain_drag() {
    let mut a = app();
    reply(&mut a, "alpha\n\nbeta\n\ngamma");
    shown(&mut a, 60, 30);
    let (l0, l2) = (find(&mut a, "alpha"), find(&mut a, "gamma"));
    a.press_at(4, row(&a, l0));
    a.release_at(4, row(&a, l0));
    a.press_at(4, row(&a, l0));
    a.drag_to(8, row(&a, l2));
    let got = a.selection_text().unwrap_or_default();
    assert!(got.contains("beta") && got.contains("gamma"), "{got:?}");
}

#[test]
fn copy_has_no_trailing_newline() {
    let mut a = app();
    reply(&mut a, "alpha\n\nbeta");
    shown(&mut a, 60, 30);
    let l0 = find(&mut a, "alpha");
    let got = drag(&mut a, (0, l0), (30, l0 + 1));
    assert!(!got.ends_with('\n'), "{got:?}");
}

#[test]
fn drag_from_the_rail_space_copies_no_leading_space() {
    let mut a = app();
    reply(&mut a, "The handler answers health checks.");
    shown(&mut a, 60, 30);
    let line = find(&mut a, "The handler");
    let col = a.line_text(line).unwrap().chars().position(|c| c == 'T').unwrap() as u16 - 1;
    let got = drag(&mut a, (col, line), (59, line));
    assert_eq!(got, "The handler answers health checks.");
}
