//! The constellation of dots above the welcome card, at fixed, injected times: where it shows and where
//! it does not, that it never touches the card, that it turns on the beat while someone is at the keys,
//! rests after 20 seconds and wakes on the next event, and that it is one still figure without motion.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use nexo_core::ProviderConfig;
use nexo_tui::frame::Load;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use tokio::sync::mpsc;

fn locked(animations: bool, theme: &str) -> App {
    std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut c = ProviderConfig::new("openai-chat", "https://codex.example", None);
    c.login = Some("codex".into());
    let opts = TuiOptions {
        cwd: PathBuf::from("/tmp/project"),
        provider: "codex".into(),
        model: "m".into(),
        providers: vec![("codex".to_string(), c)],
        theme: theme.into(),
        animations,
        ..TuiOptions::default()
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    a
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

fn draw_at(a: &mut App, t0: Instant, n: u64, w: u16, h: u16) -> Buffer {
    a.fx.clock = Some(ms(t0, n));
    a.tick_fx();
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    draw_frame(&mut t, a).unwrap();
    t.backend().buffer().clone()
}

fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

fn braille_rows(b: &Buffer) -> Vec<usize> {
    rows_of(b).iter().enumerate().filter(|(_, r)| r.chars().any(is_braille)).map(|(y, _)| y).collect()
}

fn symbols(b: &Buffer) -> Vec<String> {
    b.content().iter().map(|c| c.symbol().to_string()).collect()
}

fn card_row(b: &Buffer) -> usize {
    rows_of(b).iter().position(|r| r.contains("n e x o")).expect("the card")
}

fn mouse(a: &mut App) {
    event::handle(
        a,
        Event::Mouse(MouseEvent { kind: MouseEventKind::Moved, column: 1, row: 1, modifiers: KeyModifiers::NONE }),
    );
}

#[test]
fn small_rooms_look_as_before() {
    for (w, h) in [(40, 12), (80, 24), (120, 24), (200, 22)] {
        let t0 = Instant::now();
        let mut a = locked(true, "dark");
        let b = draw_at(&mut a, t0, 3000, w, h);
        assert!(braille_rows(&b).is_empty(), "{w}x{h}");
        let mut off = locked(false, "dark");
        let b2 = draw_at(&mut off, t0, 3000, w, h);
        assert_eq!(symbols(&b), symbols(&b2), "{w}x{h}: the same text with and without motion");
    }
}

#[test]
fn the_figure_shows_from_29_rows_up_and_not_at_28() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    assert!(braille_rows(&draw_at(&mut a, t0, 3000, 120, 28)).is_empty(), "28 rows: no figure");
    let mut a = locked(true, "dark");
    assert!(!braille_rows(&draw_at(&mut a, t0, 3000, 120, 29)).is_empty(), "29 rows: the figure");
}

#[test]
fn a_tall_room_shows_the_figure_above_the_card_and_touches_nothing() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    let b = draw_at(&mut a, t0, 3000, 120, 40);
    let rows = braille_rows(&b);
    assert!(rows.len() >= 9, "{rows:?}");
    let card = card_row(&b);
    assert!(rows.iter().all(|&y| y + 1 < card), "figure rows {rows:?} stay a row clear of the card at {card}");
    // the rows of the figure hold nothing but dots and spaces, and not at the screen's edges
    let text = rows_of(&b);
    let (mut left, mut right) = (usize::MAX, 0);
    for &y in &rows {
        assert!(text[y].chars().all(|c| c == ' ' || is_braille(c)), "{}", text[y]);
        left = left.min(text[y].chars().position(is_braille).unwrap());
        // the rows are trimmed on the right
        right = right.max(text[y].chars().count());
    }
    assert!(left >= 40 && right <= 80, "centred in 120 columns: {left}..{right}");
    // the card and everything below it is what the animations-off screen has
    let mut off = locked(false, "dark");
    let b2 = draw_at(&mut off, t0, 3000, 120, 40);
    let (s1, s2) = (symbols(&b), symbols(&b2));
    let from = (card - 1) * 120;
    assert_eq!(s1[from..], s2[from..]);
}

#[test]
fn the_figure_grows_with_the_room_up_to_the_full_size() {
    let t0 = Instant::now();
    let height = |w: u16, h: u16| {
        let mut a = locked(false, "dark");
        let b = draw_at(&mut a, t0, 0, w, h);
        braille_rows(&b).len()
    };
    let tall: Vec<usize> = [24, 30, 36, 40, 48, 54, 60].iter().map(|&h| height(120, h)).collect();
    assert_eq!(tall[0], 0, "80 x 24 and its like have none: {tall:?}");
    assert!(tall.windows(2).all(|p| p[0] <= p[1]), "{tall:?}");
    assert!(tall[3] >= 9, "a 40-row terminal has one: {tall:?}");
    assert!((13..=16).contains(&tall[6]), "{tall:?}");
    // narrow: the screen itself needs 24 columns (`ui::MIN_W`) and the smallest figure fits in them
    assert!(height(24, 50) > 0 && height(23, 50) == 0);
}

#[test]
fn the_dots_gather_once_and_then_the_figure_turns() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    let first = draw_at(&mut a, t0, 0, 120, 40);
    let mid = draw_at(&mut a, t0, 300, 120, 40);
    let done = draw_at(&mut a, t0, 700, 120, 40);
    assert_ne!(symbols(&first), symbols(&mid));
    assert_ne!(symbols(&mid), symbols(&done));
    // turning: a second later it is another frame
    let later = draw_at(&mut a, t0, 2000, 120, 40);
    assert_ne!(symbols(&done), symbols(&later));
    // the same instant drawn twice is the same frame
    let again = draw_at(&mut a, t0, 2000, 120, 40);
    assert_eq!(later, again);
}

#[test]
fn the_still_frame_is_never_redrawn() {
    let t0 = Instant::now();
    let mut a = locked(false, "dark");
    let first = draw_at(&mut a, t0, 0, 120, 40);
    assert!(!braille_rows(&first).is_empty());
    for n in [500, 5000, 30_000] {
        assert_eq!(draw_at(&mut a, t0, n, 120, 40), first);
        assert!(!a.tick_fx());
    }
    a.tick();
    assert_eq!(a.load(), Load::Idle);
}

#[test]
fn mono_has_no_colour_and_the_near_dots_are_bold() {
    let t0 = Instant::now();
    let mut a = locked(true, "mono");
    let b = draw_at(&mut a, t0, 0, 120, 40);
    let dots: Vec<_> = b.content().iter().filter(|c| c.symbol().chars().any(is_braille)).collect();
    assert!(!dots.is_empty());
    assert!(dots.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
    assert!(dots.iter().any(|c| c.modifier.contains(Modifier::BOLD)));
    // mono has no motion: the same frame later, and no frames asked for
    assert_eq!(draw_at(&mut a, t0, 4000, 120, 40), b);
    assert!(!a.tick_fx());
}

#[test]
fn near_dots_are_brighter_than_far_ones() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    let b = draw_at(&mut a, t0, 3000, 120, 40);
    let mut colours: Vec<Color> =
        b.content().iter().filter(|c| c.symbol().chars().any(is_braille)).map(|c| c.fg).collect();
    colours.sort_by_key(|c| format!("{c:?}"));
    colours.dedup();
    assert!(colours.len() >= 6, "{} shades", colours.len());
}

#[test]
fn the_turn_beats_for_twenty_seconds_then_rests_and_an_event_wakes_it() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    draw_at(&mut a, t0, 0, 120, 40);
    // at five seconds it asks for the beat, not for frames at 60 a second
    a.fx.clock = Some(ms(t0, 5000));
    assert!(a.tick_fx(), "turning at 5 s");
    a.tick();
    assert_eq!(a.load(), Load::Beat);
    assert!(a.dirty, "a frame per tick");
    // at 25 s it has come to a stop and nothing asks for a frame
    draw_at(&mut a, t0, 25_000, 120, 40);
    assert!(!a.tick_fx(), "rest at 25 s");
    a.dirty = false;
    a.tick();
    assert_eq!(a.load(), Load::Idle);
    assert!(!a.dirty, "no frame asked while resting");
    // the frame at rest is the same one for as long as it rests
    let rest = draw_at(&mut a, t0, 25_000, 120, 40);
    assert_eq!(draw_at(&mut a, t0, 600_000, 120, 40), rest);
    // an event wakes it
    a.fx.clock = Some(ms(t0, 600_000));
    mouse(&mut a);
    assert!(a.tick_fx(), "awake after an event");
    a.tick();
    assert_eq!(a.load(), Load::Beat);
    assert_ne!(draw_at(&mut a, t0, 602_000, 120, 40), rest);
    // and it rests again 20 s after the last one
    draw_at(&mut a, t0, 625_000, 120, 40);
    assert!(!a.tick_fx());
}

#[test]
fn the_figure_leaves_with_the_card() {
    let t0 = Instant::now();
    let mut a = locked(true, "dark");
    let b = draw_at(&mut a, t0, 3000, 120, 40);
    let before = braille_rows(&b);
    a.locked = false;
    a.fx.unlock_at = Some(ms(t0, 3000));
    let mid = draw_at(&mut a, t0, 3140, 120, 40);
    let rows = braille_rows(&mid);
    assert!(!rows.is_empty() && rows[0] <= before[0], "{rows:?} against {before:?}");
    let gone = draw_at(&mut a, t0, 3400, 120, 40);
    assert!(braille_rows(&gone).is_empty());
}

#[test]
fn no_size_panics() {
    let t0 = Instant::now();
    for animations in [true, false] {
        for w in [0u16, 1, 2, 19, 20, 21, 35, 36, 37, 80, 200] {
            for h in [0u16, 1, 2, 5, 12, 24, 26, 27, 28, 38, 40, 70] {
                let mut a = locked(animations, "dark");
                draw_at(&mut a, t0, 300, w, h);
                draw_at(&mut a, t0, 8000, w, h);
            }
        }
    }
}
