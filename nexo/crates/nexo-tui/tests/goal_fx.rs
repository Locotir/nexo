//! The goal and to-do views in motion on a test backend with the injected clock: the strip's checked item,
//! the side panel opening with a goal and a list, the goal window. A mid-effect buffer differs from the
//! settled one only in style, the settled one equals the animations-off one, and the frames stop.

use std::path::PathBuf;

use std::time::{Duration, Instant};

use nexo_core::agent::{GoalLog, GoalState};
use nexo_core::tool::{TodoItem, TodoStatus};
use nexo_tui::app::Goal;
use nexo_tui::goal_view::GoalView;
use nexo_tui::modal::Modal;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

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
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        // an empty session drifts its stars for a while (src/app/tests/bursts.rs); these frames are about other things
        home_stars: false,
        bursts: true,
        todo_strip: true,
        vim: false,
        prompt: None,
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

fn goal(round: u32, state: GoalState) -> Goal {
    Goal { text: "ship the release".into(), round, max: 5, state }
}

/// The rows of text and the style of every cell of one frame.
type Shot = (Vec<String>, Vec<String>);

/// Text and style of every cell.
fn snap(a: &mut App, t: &mut Terminal<TestBackend>) -> Shot {
    draw_frame(t, a).unwrap();
    let buf: &Buffer = t.backend().buffer();
    let styles = buf.content().iter().map(|c| format!("{:?}{:?}{:?}", c.fg, c.bg, c.modifier)).collect();
    // the spinner rides its own beat while the agent works: it is the same task glyph here
    let rows = rows_of(buf)
        .into_iter()
        .map(|r| r.chars().map(|c| if ('\u{2800}'..='\u{28ff}').contains(&c) { '◐' } else { c }).collect())
        .collect();
    (rows, styles)
}

/// Two apps through the same change: one animated, one still. Returns the animated one's frame at each of
/// `at` ms after the change, then the still one's final frame.
fn run(w: u16, h: u16, setup: impl Fn(&mut App), change: impl Fn(&mut App), at: &[u64]) -> (Vec<Shot>, Shot, bool) {
    let t0 = Instant::now() + Duration::from_secs(30);
    let mut a = app(true, "dark");
    a.fx.clock = Some(t0);
    setup(&mut a);
    a.tick_fx();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, &mut a).unwrap();
    // let whatever the setup started finish before the change
    a.fx.clock = Some(t0 + Duration::from_secs(5));
    a.tick_fx();
    let t1 = t0 + Duration::from_secs(5);
    change(&mut a);
    a.fx.clock = Some(t1);
    a.tick_fx();
    let mut frames = Vec::new();
    for ms in at {
        a.fx.clock = Some(t1 + Duration::from_millis(*ms));
        a.tick_fx();
        frames.push(snap(&mut a, &mut t));
    }
    a.fx.clock = Some(t1 + Duration::from_secs(3));
    let rests = !a.tick_fx();
    let settled = snap(&mut a, &mut t);
    assert_eq!(frames.len(), at.len());
    let mut b = app(false, "dark");
    setup(&mut b);
    change(&mut b);
    b.tick_fx();
    let mut t2 = Terminal::new(TestBackend::new(w, h)).unwrap();
    let still = snap(&mut b, &mut t2);
    assert_eq!(settled.0, still.0, "the settled text is the still text");
    assert_eq!(settled.1, still.1, "the settled colours are the still colours");
    (frames.into_iter().chain(std::iter::once(settled)).collect(), still, rests)
}

fn assert_style_only(frames: &[Shot], mid: usize) {
    assert_style_only_of("", frames, mid)
}

fn assert_style_only_of(what: &str, frames: &[Shot], mid: usize) {
    let last = frames.last().unwrap();
    // the bar eases through its eighths: only those glyphs may differ, inside the cells it already has
    let mask = |rows: &Vec<String>| -> Vec<String> {
        rows.iter().map(|r| r.chars().map(|c| if "█▉▊▋▌▍▎▏░".contains(c) { '#' } else { c }).collect()).collect()
    };
    assert_eq!(mask(&frames[mid].0), mask(&last.0), "no text moves");
    assert_ne!(frames[mid].1, last.1, "{what}: the colours are in between");
}

#[test]
fn a_checked_item_in_the_strip_pops_and_wipes_then_rests() {
    let before = |a: &mut App| {
        a.todo = vec![
            item("write the report", TodoStatus::InProgress),
            item("send it", TodoStatus::Pending),
            item("celebrate", TodoStatus::Pending),
        ]
    };
    let after = |a: &mut App| {
        a.todo = vec![
            item("write the report", TodoStatus::Done),
            item("send it", TodoStatus::InProgress),
            item("celebrate", TodoStatus::Pending),
        ]
    };
    let (frames, _, rests) = run(100, 30, before, after, &[100, 220]);
    assert!(rests, "the frames stop");
    assert_style_only(&frames, 0);
    assert_style_only(&frames, 1);
    assert!(frames[0].0.join("\n").contains("to-do 1/3"));
}

#[test]
fn a_new_item_and_a_new_list_fade_in_and_the_count_is_lit() {
    let before = |a: &mut App| a.todo = vec![item("one", TodoStatus::InProgress)];
    let after =
        |a: &mut App| a.todo = vec![item("one", TodoStatus::InProgress), item("two brand new", TodoStatus::Pending)];
    let (frames, _, rests) = run(100, 30, before, after, &[60, 150]);
    assert!(rests);
    assert_style_only(&frames, 0);
    let fresh = |a: &mut App| a.todo = vec![item("other list", TodoStatus::Pending), item("more", TodoStatus::Pending)];
    let (frames, _, rests) = run(100, 30, before, fresh, &[80]);
    assert!(rests);
    assert_style_only(&frames, 0);
}

#[test]
fn the_panel_opens_row_by_row_and_the_goal_and_bar_move_on_change() {
    let setup = |a: &mut App| {
        a.show_files = true;
        a.goal = Some(goal(1, GoalState::Active));
        a.todo = vec![item("a", TodoStatus::InProgress), item("b", TodoStatus::Pending)];
    };
    // the round changes: its figure is lit
    let (frames, _, rests) = run(120, 30, setup, |a| a.goal = Some(goal(2, GoalState::Active)), &[100]);
    assert!(rests);
    assert_style_only_of("round", &frames, 0);
    // an item is checked: the bar eases, the figures are lit, the mark pops
    let (frames, _, rests) = run(
        120,
        30,
        setup,
        |a| a.todo = vec![item("a", TodoStatus::Done), item("b", TodoStatus::InProgress)],
        &[100, 200],
    );
    assert!(rests);
    assert_style_only_of("checked", &frames, 0);
    // the goal is met: the mark pops and a sweep of light runs along the row
    let (frames, _, rests) = run(120, 30, setup, |a| a.goal = Some(goal(2, GoalState::Done)), &[150]);
    assert!(rests);
    assert_style_only_of("met", &frames, 0);
    assert!(frames[0].0.join("\n").contains("met ✓"));
    // the panel itself opens
    let (frames, _, rests) = run(
        120,
        30,
        |a| {
            a.goal = Some(goal(1, GoalState::Active));
            a.todo = vec![item("a", TodoStatus::InProgress)];
        },
        |a| a.show_files = true,
        &[50],
    );
    assert!(rests);
    assert_style_only_of("open", &frames, 0);
}

#[test]
fn the_goal_window_opens_row_by_row() {
    let log = GoalLog { original: Some("ship the release".into()), steps: 3, ..Default::default() };
    let (frames, _, rests) = run(
        100,
        30,
        |_| {},
        |a| a.modal = Some(Modal::Goal(Box::new(GoalView::new(goal(1, GoalState::Active), log.clone())))),
        &[0, 170],
    );
    assert!(rests);
    // the window's own pop (140 ms from its first drawn frame) moves its frame: past it, only the rows still
    // coming in differ
    assert_style_only_of("goal window", &frames, 1);
}

#[test]
fn small_terminals_and_a_mono_theme_do_not_panic() {
    for (w, h) in [(20, 8), (40, 10), (12, 5), (60, 6)] {
        let (_, _, _) = run(
            w,
            h,
            |a| {
                a.show_files = true;
                a.goal = Some(goal(1, GoalState::Active));
                a.todo = vec![item("a", TodoStatus::InProgress), item("b", TodoStatus::Pending)];
            },
            |a| {
                a.goal = Some(goal(2, GoalState::Done));
                a.todo = vec![
                    item("a", TodoStatus::Done),
                    item("b", TodoStatus::InProgress),
                    item("c", TodoStatus::Pending),
                ];
            },
            &[40, 120],
        );
    }
    let mut a = app(true, "mono");
    a.todo = vec![item("a", TodoStatus::InProgress)];
    a.tick_fx();
    a.todo = vec![item("a", TodoStatus::Done)];
    a.todo_done_live = true;
    assert!(!a.tick_fx(), "a mono theme has no colour motion");
}
