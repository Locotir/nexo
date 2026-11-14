//! The light on the rule over the draft while the agent works: a soft crest that crosses it on the clock of the
//! turn (run with `COLORTERM=truecolor`; the colours are the theme's own either way).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::AgentEvent;
use nexo_tui::{draw_frame, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

const CYCLE_MS: u64 = 2600;

fn app(theme: &str, animations: bool) -> (App, Instant) {
    let (tx, _rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        theme: theme.into(),
        animations,
        detail: nexo_core::config::Detail::Full,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    let t0 = Instant::now();
    a.fx.clock = Some(t0);
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    (a, t0)
}

fn draw(a: &mut App, w: u16, ms_in: u64, t0: Instant) -> Buffer {
    a.fx.clock = Some(t0 + Duration::from_millis(ms_in));
    let mut t = Terminal::new(TestBackend::new(w, 24)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

/// The `─` cells at the right end of the work line (the rule after its words): their colours.
fn tail(b: &Buffer) -> Vec<Color> {
    let y = (0..b.area.height)
        .find(|y| {
            (0..b.area.width).map(|x| b[(x, *y)].symbol().to_string()).collect::<String>().contains("Esc interrupt")
        })
        .expect("the work line");
    let mut cells: Vec<Color> =
        (0..b.area.width).rev().map_while(|x| (b[(x, y)].symbol() == "─").then(|| b[(x, y)].fg)).collect();
    cells.reverse();
    cells
}

/// How lit a colour is, 0 (the rule's own) to 1 (the accent).
fn lit(c: Color, border: Color, accent: Color) -> f32 {
    let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb), Color::Rgb(ar, ag, ab)) = (c, border, accent) else {
        panic!("true colour expected: {c:?} {border:?} {accent:?}");
    };
    let d = |x: u8, y: u8| (x as f32 - y as f32).abs();
    (d(r, br) + d(g, bg) + d(b, bb)) / (d(ar, br) + d(ag, bg) + d(ab, bb))
}

/// The light of each cell of the tail of the rule `ms_in` milliseconds into the turn.
fn levels(a: &mut App, w: u16, ms_in: u64, t0: Instant) -> Vec<f32> {
    let b = draw(a, w, ms_in, t0);
    tail(&b).iter().map(|c| lit(*c, a.theme.border, a.theme.accent)).collect()
}

#[test]
fn the_crest_is_dim_at_both_ends_and_brightest_in_the_middle() {
    let (mut a, t0) = app("dark", true);
    // the second pass, with the opening of the turn long over: sweep it and take the frame with the crest mid-rule
    let frames: Vec<Vec<f32>> = (0..=200).map(|i| levels(&mut a, 120, CYCLE_MS + i * 10, t0)).collect();
    let n = frames[0].len();
    assert!(n > 60, "a long tail to cross: {n}");
    let mid = &frames[100];
    let top = mid.iter().cloned().fold(0.0f32, f32::max);
    assert!((0.7..=1.01).contains(&top), "the peak breathes between 70 and 100 %: {top}");
    let at = mid.iter().position(|v| *v == top).unwrap();
    assert!(at.abs_diff(n / 2) <= 3, "the middle of the crest is mid-rule: {at} of {n}");
    let lit_cells: Vec<usize> = (0..n).filter(|i| mid[*i] > 0.02).collect();
    let (first, last) = (*lit_cells.first().unwrap(), *lit_cells.last().unwrap());
    assert!((14..=20).contains(&(last - first + 1)), "about 14 to 20 cells wide: {}", last - first + 1);
    assert!(mid[first] < 0.12 * top && mid[last] < 0.12 * top, "dim at front and tail: {} {}", mid[first], mid[last]);
    assert!(mid[at] > 0.95 * top && mid[(first + at) / 2] < 0.8 * top && mid[(at + last) / 2] < 0.8 * top);
    // a smooth bell: it rises to the middle and falls after it, one step at a time
    assert!(mid[first..=at].windows(2).all(|p| p[0] <= p[1] + 1e-3), "rises to the middle");
    assert!(mid[at..=last].windows(2).all(|p| p[0] + 1e-3 >= p[1]), "falls after it");
    let step = mid[first..=last].windows(2).map(|p| (p[0] - p[1]).abs()).fold(0.0f32, f32::max);
    assert!(step < 0.25 * top, "no hard edge: {step}");
}

#[test]
fn a_pass_takes_two_seconds_at_80_and_at_200_columns_and_the_breath_stays_between_70_and_100_percent() {
    for w in [80u16, 200] {
        let (mut a, t0) = app("dark", true);
        let mut on: Vec<u64> = Vec::new();
        let mut tops: Vec<f32> = Vec::new();
        for i in 0..=(CYCLE_MS / 10) {
            let l = levels(&mut a, w, CYCLE_MS + i * 10, t0);
            let top = l.iter().cloned().fold(0.0f32, f32::max);
            if top > 0.02 {
                on.push(i * 10);
            }
            // with the whole crest on the rule (not coming in or leaving) the top cell is the peak
            if (400..=1600).contains(&(i * 10)) {
                tops.push(top);
            }
        }
        let (first, last) = (*on.first().unwrap(), *on.last().unwrap());
        assert!(first <= 150, "{w}: the crest comes in at once: {first} ms");
        assert!((1700..=2050).contains(&(last - first + 10)), "{w}: a pass of about 2 s: {} ms", last - first + 10);
        assert!(on.len() < (CYCLE_MS / 10) as usize - 30, "{w}: it rests between passes");
        assert!(tops.iter().all(|t| (0.69..=1.01).contains(t)), "{w}: the peak stays within 70 to 100 %");
    }
}

#[test]
fn the_speed_follows_the_clock_of_the_turn_not_the_frames_drawn() {
    let (mut a, t0) = app("dark", true);
    let at = |a: &mut App, ms| levels(a, 120, ms, t0);
    let direct = at(&mut a, CYCLE_MS + 700);
    // the same moment after many frames in between, and again after fewer
    for i in 0..40 {
        at(&mut a, CYCLE_MS + i * 17);
    }
    assert_eq!(direct, at(&mut a, CYCLE_MS + 700));
}

#[test]
fn the_rule_stays_plain_with_the_animations_off_or_on_a_mono_theme() {
    for (theme, animations) in [("dark", false), ("mono", true), ("mono", false)] {
        let (mut a, t0) = app(theme, animations);
        let first = tail(&draw(&mut a, 120, CYCLE_MS + 1000, t0));
        assert!(first.len() > 60, "{theme} {animations}: the rule is drawn");
        for ms in [CYCLE_MS, CYCLE_MS + 700, CYCLE_MS + 1500, 2 * CYCLE_MS + 300] {
            let b = draw(&mut a, 120, ms, t0);
            let t = tail(&b);
            assert_eq!(t.len(), first.len());
            assert!(t.iter().all(|c| *c == t[0]), "{theme} {animations} {ms}: one colour all along");
        }
    }
}
