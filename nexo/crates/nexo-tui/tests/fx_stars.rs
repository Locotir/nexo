//! The constellation of dots above the welcome card, at fixed, injected times: where it shows and where
//! it does not, that it never touches the card, that it turns on the beat while someone is at the keys,
//! rests after 20 seconds and wakes on the next event, and that it is one still figure without motion.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
use nexo_core::config::Figure;
use nexo_core::ProviderConfig;
use nexo_tui::frame::Load;
use nexo_tui::ui::rows_of;
use nexo_tui::{draw_frame, event, App, TuiOptions};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use tokio::sync::mpsc;

/// The constellation (the tests below were written for it; the galaxy has its own section at the end).
fn locked(animations: bool, theme: &str) -> App {
    locked_with(animations, theme, Figure::Constellation)
}

fn locked_with(animations: bool, theme: &str, figure: Figure) -> App {
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
        // an empty session drifts its stars for a while (src/app/tests/bursts.rs); these frames are about other things
        home_stars: false,
        figure,
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

/// The shortest window, 120 columns wide, in which the figure is drawn.
fn first_height_with_the_figure(detail: nexo_core::config::Detail) -> u16 {
    let t0 = Instant::now();
    (16..=40)
        .find(|&h| {
            let mut a = locked(true, "dark");
            a.opts.detail = detail;
            !braille_rows(&draw_at(&mut a, t0, 3000, 120, h)).is_empty()
        })
        .expect("the figure shows at some height up to 40 rows")
}

#[test]
fn the_figure_needs_29_rows_on_the_full_screen_and_fewer_on_the_quiet_one() {
    use nexo_core::config::Detail;
    assert_eq!(first_height_with_the_figure(Detail::Full), 29, "full: as before the quiet screen");
    // the quiet screen has one fixed row less and a shorter welcome box, so the figure fits sooner
    assert_eq!(first_height_with_the_figure(Detail::Quiet), 28, "quiet (the default)");
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

// ---- the galaxy, the figure by default, and `[ui] figure` ----

fn galaxy(animations: bool, theme: &str) -> App {
    locked_with(animations, theme, Figure::Galaxy)
}

/// The shortest window, 120 columns wide, in which the galaxy is drawn.
fn first_height_with_the_galaxy(detail: nexo_core::config::Detail) -> u16 {
    let t0 = Instant::now();
    (16..=40)
        .find(|&h| {
            let mut a = galaxy(true, "dark");
            a.opts.detail = detail;
            !braille_rows(&draw_at(&mut a, t0, 3000, 120, h)).is_empty()
        })
        .expect("the galaxy shows at some height up to 40 rows")
}

#[test]
fn the_galaxy_is_the_default_figure() {
    use nexo_core::config::Detail;
    assert_eq!(TuiOptions::default().figure, Figure::Galaxy);
    let t0 = Instant::now();
    let mut a = locked_with(true, "dark", TuiOptions::default().figure);
    let mut g = galaxy(true, "dark");
    let mut c = locked(true, "dark");
    let (ba, bg, bc) =
        (draw_at(&mut a, t0, 3000, 120, 40), draw_at(&mut g, t0, 3000, 120, 40), draw_at(&mut c, t0, 3000, 120, 40));
    assert_eq!(symbols(&ba), symbols(&bg));
    assert_ne!(symbols(&ba), symbols(&bc));
    // it needs less height than the constellation (8 rows of room against 9), and still none at 80 x 24
    assert_eq!(first_height_with_the_galaxy(Detail::Full), 27);
    assert_eq!(first_height_with_the_galaxy(Detail::Quiet), 26);
}

#[test]
fn small_rooms_look_as_before_with_the_galaxy() {
    for (w, h) in [(40, 12), (80, 24), (120, 24), (200, 22)] {
        let t0 = Instant::now();
        let mut a = galaxy(true, "dark");
        let b = draw_at(&mut a, t0, 3000, w, h);
        assert!(braille_rows(&b).is_empty(), "{w}x{h}");
        let mut off = galaxy(false, "dark");
        assert_eq!(symbols(&b), symbols(&draw_at(&mut off, t0, 3000, w, h)), "{w}x{h}");
    }
}

#[test]
fn a_tall_room_shows_the_galaxy_above_the_card_and_touches_nothing() {
    let t0 = Instant::now();
    let mut a = galaxy(true, "dark");
    let b = draw_at(&mut a, t0, 3000, 120, 40);
    let rows = braille_rows(&b);
    assert!(rows.len() >= 7, "{rows:?}");
    let card = card_row(&b);
    assert!(rows.iter().all(|&y| y + 1 < card), "figure rows {rows:?} stay a row clear of the card at {card}");
    let text = rows_of(&b);
    let (mut left, mut right) = (usize::MAX, 0);
    for &y in &rows {
        assert!(text[y].chars().all(|c| c == ' ' || is_braille(c)), "{}", text[y]);
        left = left.min(text[y].chars().position(is_braille).unwrap());
        right = right.max(text[y].chars().count());
    }
    assert!(left >= 38 && right <= 82, "centred in 120 columns: {left}..{right}");
    let mut off = galaxy(false, "dark");
    let b2 = draw_at(&mut off, t0, 3000, 120, 40);
    let (s1, s2) = (symbols(&b), symbols(&b2));
    let from = (card - 1) * 120;
    assert_eq!(s1[from..], s2[from..]);
}

#[test]
fn the_galaxy_is_wider_than_tall_on_screen() {
    let t0 = Instant::now();
    let mut a = galaxy(false, "dark");
    let b = draw_at(&mut a, t0, 0, 120, 60);
    let rows = braille_rows(&b);
    let text = rows_of(&b);
    let (mut lo, mut hi) = (usize::MAX, 0);
    for &y in &rows {
        lo = lo.min(text[y].chars().position(is_braille).unwrap());
        hi = hi.max(text[y].chars().rev().position(is_braille).map_or(0, |p| text[y].chars().count() - p));
    }
    assert!(rows.len() <= 12 && hi - lo >= 30, "{} rows, columns {lo}..{hi}", rows.len());
}

#[test]
fn the_galaxy_gathers_once_then_turns_rests_and_wakes() {
    let t0 = Instant::now();
    let mut a = galaxy(true, "dark");
    let first = draw_at(&mut a, t0, 0, 120, 40);
    let mid = draw_at(&mut a, t0, 300, 120, 40);
    let done = draw_at(&mut a, t0, 700, 120, 40);
    assert_ne!(symbols(&first), symbols(&mid));
    assert_ne!(symbols(&mid), symbols(&done));
    let later = draw_at(&mut a, t0, 2000, 120, 40);
    assert_ne!(symbols(&done), symbols(&later));
    assert_eq!(later, draw_at(&mut a, t0, 2000, 120, 40));
    // the beat at 5 s, nothing at 25 s, and an event wakes it
    a.fx.clock = Some(ms(t0, 5000));
    assert!(a.tick_fx(), "turning at 5 s");
    a.tick();
    assert_eq!(a.load(), Load::Beat);
    draw_at(&mut a, t0, 25_000, 120, 40);
    assert!(!a.tick_fx(), "rest at 25 s");
    a.dirty = false;
    a.tick();
    assert_eq!(a.load(), Load::Idle);
    assert!(!a.dirty);
    let rest = draw_at(&mut a, t0, 25_000, 120, 40);
    assert_eq!(draw_at(&mut a, t0, 600_000, 120, 40), rest);
    a.fx.clock = Some(ms(t0, 600_000));
    mouse(&mut a);
    assert!(a.tick_fx(), "awake after an event");
    assert_ne!(draw_at(&mut a, t0, 602_000, 120, 40), rest);
    draw_at(&mut a, t0, 625_000, 120, 40);
    assert!(!a.tick_fx());
}

#[test]
fn the_galaxy_still_frame_is_never_redrawn() {
    let t0 = Instant::now();
    let mut a = galaxy(false, "dark");
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
fn the_galaxy_in_mono_has_no_colour_and_a_bold_core() {
    let t0 = Instant::now();
    let mut a = galaxy(true, "mono");
    let b = draw_at(&mut a, t0, 0, 120, 40);
    let dots: Vec<_> = b.content().iter().filter(|c| c.symbol().chars().any(is_braille)).collect();
    assert!(!dots.is_empty());
    assert!(dots.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
    let bold = dots.iter().filter(|c| c.modifier.contains(Modifier::BOLD)).count();
    assert!(bold > 0 && bold * 3 < dots.len(), "{bold} bold of {}", dots.len());
    assert_eq!(draw_at(&mut a, t0, 4000, 120, 40), b);
    assert!(!a.tick_fx());
}

#[test]
fn the_galaxy_core_is_lit_toward_the_text_colour() {
    let t0 = Instant::now();
    let mut a = galaxy(true, "dark");
    draw_at(&mut a, t0, 0, 120, 40);
    let b = draw_at(&mut a, t0, 3000, 120, 40);
    let theme = nexo_tui::theme::Theme::dark();
    let fg: Vec<Color> = b.content().iter().filter(|c| c.symbol().chars().any(is_braille)).map(|c| c.fg).collect();
    let lum = |c: Color| match c {
        Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
        _ => 0,
    };
    let top = fg.iter().copied().max_by_key(|&c| lum(c)).unwrap();
    assert!(lum(top) >= lum(theme.accent), "{top:?} against {:?} and fg {:?}", theme.accent, theme.fg);
    let mut shades = fg.clone();
    shades.sort_by_key(|c| format!("{c:?}"));
    shades.dedup();
    assert!(shades.len() >= 8, "{} shades", shades.len());
}

#[test]
fn the_galaxy_leaves_with_the_card() {
    let t0 = Instant::now();
    let mut a = galaxy(true, "dark");
    let before = braille_rows(&draw_at(&mut a, t0, 3000, 120, 40));
    a.locked = false;
    a.fx.unlock_at = Some(ms(t0, 3000));
    let rows = braille_rows(&draw_at(&mut a, t0, 3140, 120, 40));
    assert!(!rows.is_empty() && rows[0] <= before[0], "{rows:?} against {before:?}");
    assert!(braille_rows(&draw_at(&mut a, t0, 3400, 120, 40)).is_empty());
}

#[test]
fn figure_none_draws_nothing_and_asks_for_no_frame() {
    let t0 = Instant::now();
    for animations in [true, false] {
        let mut a = locked_with(animations, "dark", Figure::None);
        let b = draw_at(&mut a, t0, 0, 120, 40);
        assert!(braille_rows(&b).is_empty());
        // (the card's own entrance asks for frames for its first moments; the figure would after that)
        for n in [3000, 5000, 25_000] {
            let b = draw_at(&mut a, t0, n, 120, 40);
            assert!(braille_rows(&b).is_empty());
            assert!(!a.tick_fx(), "no frame at {n} ms");
        }
        a.tick();
        assert_eq!(a.load(), Load::Idle);
        // the rest of the screen is what the other figures leave: the card and what is below it
        let mut g = locked_with(false, "dark", Figure::Galaxy);
        let (none, with) = (symbols(&draw_at(&mut a, t0, 0, 120, 40)), symbols(&draw_at(&mut g, t0, 0, 120, 40)));
        let card = card_row(&draw_at(&mut a, t0, 0, 120, 40));
        assert_eq!(none[(card - 1) * 120..], with[(card - 1) * 120..]);
    }
}

#[test]
fn figure_constellation_is_the_frame_the_engine_always_drew() {
    let t0 = Instant::now();
    let mut a = locked_with(false, "dark", Figure::Constellation);
    let b = draw_at(&mut a, t0, 0, 120, 60);
    let want = nexo_tui::fx_stars::grid(nexo_tui::fx_stars::NICE_MS, 1.0, 36, 16, false);
    let text = rows_of(&b);
    let y0 = card_row(&b) - 1 - 16;
    let x0 = (120 - 36) / 2;
    for i in 0..16 {
        let y = y0 + i;
        let got: String = text[y].chars().chain(std::iter::repeat(' ')).skip(x0).take(36).collect();
        assert_eq!(got, want.line(i as u16), "row {i}");
    }
}

#[test]
fn no_size_panics_with_the_galaxy() {
    let t0 = Instant::now();
    for animations in [true, false] {
        for w in [0u16, 1, 2, 19, 23, 24, 25, 39, 40, 41, 80, 200] {
            for h in [0u16, 1, 2, 5, 12, 24, 25, 26, 27, 38, 40, 70] {
                let mut a = galaxy(animations, "dark");
                draw_at(&mut a, t0, 300, w, h);
                draw_at(&mut a, t0, 8000, w, h);
            }
        }
    }
}
