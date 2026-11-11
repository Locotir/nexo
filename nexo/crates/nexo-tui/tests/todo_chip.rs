//! The to-do list as a count in the status bar (the default): no rows above the draft, a click opens the
//! list, a narrow bar drops the count before the model name, and `[ui] todo_strip = true` keeps the old strip.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::tool::{TodoItem, TodoStatus};
use nexo_tui::app::StatusHit;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use tokio::sync::mpsc;

fn app(strip: bool, animations: bool) -> App {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        theme: "dark".into(),
        timestamps: false,
        suggest: false,
        animations,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        todo_strip: strip,
        session_id: Some("20261027-120000-ab12".into()),
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem { text: text.into(), status }
}

/// Five tasks, two of them done.
fn list() -> Vec<TodoItem> {
    vec![
        item("Read the code", TodoStatus::Done),
        item("Design it", TodoStatus::Done),
        item("Write it", TodoStatus::InProgress),
        item("Test it", TodoStatus::Pending),
        item("Ship it", TodoStatus::Pending),
    ]
}

fn render(a: &mut App, w: u16, h: u16) -> (Vec<String>, Buffer) {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    let buf = t.backend().buffer().clone();
    (rows_of(&buf), buf)
}

fn click(a: &mut App, x: u16, y: u16) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let kind = MouseEventKind::Down(MouseButton::Left);
    let ev = MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    nexo_tui::event::handle(a, crossterm::event::Event::Mouse(ev));
}

#[test]
fn by_default_a_list_takes_no_rows_and_the_bar_says_2_of_5() {
    let mut none = app(false, false);
    let (bare, _) = render(&mut none, 120, 36);
    let mut a = app(false, false);
    a.todo = list();
    let (rows, _) = render(&mut a, 120, 36);
    assert_eq!(a.todo_area.height, 0, "no strip");
    // everything above the bar is the same frame: the draft sits on the same row, no hint repeats the count
    assert_eq!(rows[..35], bare[..35], "the list adds nothing above the bar");
    let bar = rows.last().unwrap();
    assert!(bar.contains("✓ 2/5"), "{bar}");
    assert!(!bare.last().unwrap().contains("2/5"));
    assert!(!rows.join("\n").contains("to-do 2/5"), "the strip's words are gone");
}

#[test]
fn the_count_goes_once_the_list_is_done_and_the_turn_is_over() {
    let mut a = app(false, false);
    a.todo = list();
    a.todo[2].status = TodoStatus::Done;
    a.todo[3].status = TodoStatus::Done;
    a.todo[4].status = TodoStatus::Done;
    a.todo_done_live = true;
    let (rows, buf) = render(&mut a, 120, 36);
    let y = rows.len() - 1;
    assert!(rows[y].contains("✓ 5/5"), "all done is still said this turn: {}", rows[y]);
    let x = rows[y].chars().position(|c| c == '5').unwrap() as u16;
    let ok = nexo_tui::theme::Theme::dark().success;
    assert_eq!(buf[(x, y as u16)].fg, ok, "all done takes the theme's ok colour");
    a.todo_done_live = false;
    let (rows, _) = render(&mut a, 120, 36);
    assert!(!rows.last().unwrap().contains("5/5"), "{}", rows.last().unwrap());
}

#[test]
fn with_the_strip_on_the_rows_stay_and_the_chip_is_not_drawn() {
    let mut a = app(true, false);
    a.todo = list();
    let (rows, _) = render(&mut a, 120, 36);
    assert_eq!(a.todo_area.height, 6, "the strip takes its rows (the 5 tasks and the count)");
    let text = rows.join("\n");
    assert!(text.contains("to-do 2/5"), "{text}");
    assert!(!rows.last().unwrap().contains("✓ 2/5"), "one place only: {}", rows.last().unwrap());
}

#[test]
fn a_click_on_the_count_opens_the_list() {
    let mut a = app(false, false);
    a.todo = list();
    render(&mut a, 120, 36);
    let r =
        a.status_hits.iter().find(|(_, h)| *h == StatusHit::Todo).map(|(r, _)| *r).expect("the chip answers a click");
    click(&mut a, r.x + 1, r.y);
    match &a.modal {
        Some(Modal::Todo(v)) => assert_eq!(v.len(), 5),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_narrow_bar_drops_the_count_before_the_model_name() {
    for w in 30..=120u16 {
        let mut a = app(false, false);
        a.todo = list();
        let (rows, _) = render(&mut a, w, 24);
        let bar = rows.last().unwrap();
        if bar.contains("2/5") {
            assert!(bar.contains("deepseek-chat"), "the count never pushes the model out at {w}: {bar}");
        }
    }
    let mut a = app(false, false);
    a.todo = list();
    let (rows, _) = render(&mut a, 40, 24);
    let bar = rows.last().unwrap();
    assert!(!bar.contains("2/5") && bar.contains("deepseek-chat"), "{bar}");
    let mut wide = app(false, false);
    wide.todo = list();
    let (rows, _) = render(&mut wide, 100, 24);
    assert!(rows.last().unwrap().contains("2/5"), "{}", rows.last().unwrap());
}

#[test]
fn a_list_that_is_drawn_nowhere_starts_no_effect() {
    let t0 = Instant::now() + Duration::from_secs(30);
    let mut a = app(false, true);
    a.fx.clock = Some(t0);
    a.tick_fx();
    let mut t = Terminal::new(TestBackend::new(120, 36)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    a.fx.clock = Some(t0 + Duration::from_secs(5));
    a.tick_fx();
    a.todo = list();
    a.fx.clock = Some(t0 + Duration::from_secs(6));
    assert!(!a.tick_fx(), "no strip and no panel: nothing animates");
    a.todo[2].status = TodoStatus::Done;
    a.fx.clock = Some(t0 + Duration::from_secs(7));
    assert!(!a.tick_fx(), "a checked task neither");
    // the same change with the strip on does move
    let mut b = app(true, true);
    b.fx.clock = Some(t0);
    b.tick_fx();
    draw_frame(&mut t, &mut b).unwrap();
    b.fx.clock = Some(t0 + Duration::from_secs(5));
    b.tick_fx();
    b.todo = list();
    b.fx.clock = Some(t0 + Duration::from_secs(6));
    assert!(b.tick_fx(), "the strip's rows fade in");
}
