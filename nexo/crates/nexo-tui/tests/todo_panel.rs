//! The to-do window drawn on a test backend: the bar, the groups, the row being worked on, wrapped
//! text, the ASCII fallback, a narrow screen and a list that changes while it is open.

use std::path::PathBuf;

use nexo_core::agent::AgentEvent;
use nexo_core::tool::{TodoItem, TodoStatus, ToolDisplay, ToolOutput};
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(animations: bool, theme: &str) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: theme.into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        status_line: vec![],
        status_command: String::new(),
        notify_after: 15,
        animations,
        vim: false,
        session_id: Some("20261027-120000-ab12".into()),
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem { text: text.into(), status }
}

fn press(a: &mut App, c: crossterm::event::KeyCode) {
    let k = crossterm::event::KeyEvent::new(c, crossterm::event::KeyModifiers::NONE);
    nexo_tui::event::handle(a, crossterm::event::Event::Key(k));
}

fn click(a: &mut App, x: u16, y: u16) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let kind = MouseEventKind::Down(MouseButton::Left);
    let ev = MouseEvent { kind, column: x, row: y, modifiers: crossterm::event::KeyModifiers::NONE };
    nexo_tui::event::handle(a, crossterm::event::Event::Mouse(ev));
}

fn list() -> Vec<TodoItem> {
    vec![
        item("Read the todo code", TodoStatus::Done),
        item("Design the window", TodoStatus::Done),
        item("Draft it", TodoStatus::Done),
        item(
            "Wire the new modal into app.rs and make the click on the todo hint open it, keeping the scroll between updates",
            TodoStatus::InProgress,
        ),
        item("Add tests for the empty state", TodoStatus::Pending),
        item("Light theme pass", TodoStatus::Pending),
        item("Ascii fallback", TodoStatus::Pending),
    ]
}

fn open(a: &mut App, items: Vec<TodoItem>) {
    a.todo = items;
    a.open_todo();
}

fn render(a: &mut App, w: u16, h: u16) -> (Vec<String>, Buffer) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    (rows_of(&buf), buf)
}

#[test]
fn the_window_shows_the_bar_the_groups_and_the_whole_text() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    let (rows, _) = render(&mut a, 100, 32);
    let text = rows.join("\n");
    for want in ["≡ To-do", "▕", "3/7", "43%", "NOW", "NEXT · 3", "DONE · 3", "Space opens", "Enter copy"] {
        assert!(text.contains(want), "{want}\n{text}");
    }
    // the long task is wrapped, not cut: its end is there too
    assert!(text.contains("between updates"), "{text}");
    assert!(text.contains("◐ 4"), "the number the model sees\n{text}");
    assert!(!text.contains("Read the todo code"), "the finished ones are folded\n{text}");
}

#[test]
fn space_opens_the_finished_ones() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    press(&mut a, crossterm::event::KeyCode::Char(' '));
    let (rows, _) = render(&mut a, 100, 40);
    let text = rows.join("\n");
    assert!(text.contains("Read the todo code") && text.contains("▾ DONE · 3"), "{text}");
}

#[test]
fn the_row_being_worked_on_has_its_own_ground() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    let (rows, buf) = render(&mut a, 100, 32);
    let y = rows.iter().position(|r| r.contains("◐ 4")).unwrap() as u16;
    let x = rows[y as usize].chars().position(|c| c == '◐').unwrap() as u16;
    let on = buf[(x, y)].bg;
    let off_y = rows.iter().position(|r| r.contains("○ 5")).unwrap() as u16;
    assert_ne!(on, buf[(x, off_y)].bg, "the task being worked on stands out");
    assert!(matches!(on, Color::Rgb(..)));
}

#[test]
fn at_100_and_60_columns_nothing_is_wider_than_the_screen() {
    for w in [100u16, 60, 44, 34] {
        let mut a = app(false, "dark");
        open(&mut a, list());
        let (rows, _) = render(&mut a, w, 28);
        let text = rows.join("\n");
        assert!(text.contains("To-do") && text.contains("NEXT"), "{w}\n{text}");
        assert!(rows.iter().all(|r| r.chars().count() <= w as usize), "{w}");
        if w == 34 {
            assert!(!text.contains("◐ 1 ·"), "the counts need room\n{text}");
        }
        if w == 100 {
            assert!(text.contains("◐ 1 · ○ 3 · ● 3"), "{text}");
        }
    }
}

#[test]
fn without_animations_the_window_is_a_still_frame() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    let (one, b1) = render(&mut a, 90, 30);
    a.spinner += 3;
    let (two, b2) = render(&mut a, 90, 30);
    assert_eq!(one, two);
    assert_eq!(b1, b2);
    let mut b = app(true, "dark");
    open(&mut b, list());
    let (x, _) = render(&mut b, 90, 30);
    b.spinner += 1;
    let (y, _) = render(&mut b, 90, 30);
    assert_ne!(x, y, "the spinner turns when the motion is on");
}

#[test]
fn the_ascii_fallback_has_no_symbols_outside_ascii() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    let (_, mut buf) = render(&mut a, 100, 32);
    nexo_tui::ascii::fold(&mut buf);
    let rows = rows_of(&buf);
    let from = rows.iter().position(|r| r.contains("To-do")).unwrap();
    let to = rows.iter().rposition(|r| r.contains("Esc")).unwrap();
    for r in &rows[from..=to] {
        assert!(r.is_ascii(), "{r:?}");
    }
    assert!(rows[from..=to].join("\n").contains("NEXT"));
}

#[test]
fn the_list_changing_under_an_open_window_updates_it() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    let items = vec![item("Only one left", TodoStatus::InProgress), item("Done now", TodoStatus::Done)];
    a.handle_agent(AgentEvent::ToolDone {
        id: "t1".into(),
        output: ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items }),
        elapsed_ms: 1,
    });
    assert!(matches!(&a.modal, Some(Modal::Todo(v)) if v.len() == 2));
    let (rows, _) = render(&mut a, 100, 32);
    let text = rows.join("\n");
    assert!(text.contains("Only one left") && text.contains("1/2"), "{text}");
}

#[test]
fn an_empty_list_opens_a_window_that_says_so() {
    let mut a = app(false, "dark");
    open(&mut a, vec![]);
    let (rows, _) = render(&mut a, 80, 24);
    assert!(rows.join("\n").contains("No tasks yet"));
}

#[test]
fn both_themes_draw_the_window() {
    for theme in ["dark", "light"] {
        let mut a = app(false, theme);
        open(&mut a, list());
        let (rows, _) = render(&mut a, 80, 30);
        assert!(rows.join("\n").contains("NOW"), "{theme}");
    }
}

#[test]
fn a_click_on_a_task_selects_it_and_on_the_finished_header_opens_them() {
    let mut a = app(false, "dark");
    open(&mut a, list());
    render(&mut a, 100, 32);
    let (r, _) = a.picker_rows.iter().find(|(_, i)| *i == 1).cloned().expect("a row");
    click(&mut a, r.x, r.y);
    assert!(matches!(&a.modal, Some(Modal::Todo(v)) if v.selected == 1));
    render(&mut a, 100, 32);
    let (h, _) = a.picker_rows.iter().find(|(_, i)| *i == 4).cloned().expect("the header");
    click(&mut a, h.x, h.y);
    assert!(matches!(&a.modal, Some(Modal::Todo(v)) if v.done_open));
}

fn push_list(a: &mut App, items: Vec<TodoItem>) {
    a.handle_agent(AgentEvent::ToolDone {
        id: "t2".into(),
        output: ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items }),
        elapsed_ms: 1,
    });
}

#[test]
fn a_window_opened_on_one_task_grows_when_the_list_does() {
    let mut a = app(false, "dark");
    open(&mut a, vec![item("Only task", TodoStatus::Pending)]);
    render(&mut a, 100, 32);
    let many: Vec<TodoItem> = (1..=7).map(|n| item(&format!("Task number {n}"), TodoStatus::Pending)).collect();
    push_list(&mut a, many);
    let text = render(&mut a, 100, 32).0.join("\n");
    assert!(text.contains("Task number 7") && text.contains("NEXT · 7"), "{text}");
    assert!(text.contains("0%") && text.contains("Esc"), "the bar and the keys stay in view\n{text}");
}

#[test]
fn opening_the_finished_ones_makes_room_for_all_of_them() {
    let mut a = app(false, "dark");
    let mut items = vec![item("Still to do", TodoStatus::Pending)];
    items.extend((1..=4).map(|n| item(&format!("Finished {n}"), TodoStatus::Done)));
    open(&mut a, items);
    render(&mut a, 100, 32);
    press(&mut a, crossterm::event::KeyCode::Char(' '));
    let text = render(&mut a, 100, 32).0.join("\n");
    for n in 1..=4 {
        assert!(text.contains(&format!("Finished {n}")), "{n}\n{text}");
    }
    assert!(text.contains("Esc"), "{text}");
}

#[test]
fn only_the_row_under_the_cursor_has_the_bar() {
    for theme in ["dark", "light"] {
        let mut a = app(false, theme);
        open(&mut a, list());
        press(&mut a, crossterm::event::KeyCode::Down);
        let (rows, _) = render(&mut a, 100, 32);
        let marked: Vec<&String> = rows.iter().filter(|r| r.contains("▌ ")).collect();
        assert_eq!(marked.len(), 1, "{theme}\n{}", rows.join("\n"));
        assert!(marked[0].contains("○ 5"), "{theme}: the second task\n{}", rows.join("\n"));
        // the first row (the one being worked on) has no bar once the cursor left it
        assert!(rows.iter().any(|r| r.contains("◐ 4") && !r.contains('▌')));
    }
}

#[test]
fn the_side_panel_keeps_changed_files_in_sight_with_a_long_list() {
    let mut a = app(false, "dark");
    a.show_files = true;
    a.todo = (1..=16)
        .map(|n| item(&format!("Task {n} with a text long enough to wrap in a narrow side panel"), TodoStatus::Pending))
        .collect();
    a.changed = vec![nexo_tui::app::ChangedFile { path: "src/lib.rs".into(), added: 3, removed: 1 }];
    let (rows, _) = render(&mut a, 120, 30);
    let text = rows.join("\n");
    assert!(text.contains("changed files (1)") && text.contains("src/lib.rs"), "{text}");
    assert!(text.contains("..") || text.contains('…'), "{text}");
}
