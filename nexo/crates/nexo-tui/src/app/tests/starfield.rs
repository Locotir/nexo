//! The starfield of the welcome screen (see `fx_starfield`): where it is painted, that it breathes frame
//! to frame, fades out when the figure rests, asks for no frames after that and comes back with a key.

use super::*;
use crate::fx_starfield::is_star_glyph;
use crate::fx_stars::{FADE_IN_MS, REST_AFTER_MS, STOP_MS};
use ratatui::buffer::Buffer;
use ratatui::style::Color;

const W: u16 = 110;
const H: u16 = 36;

/// A locked home (no model yet: the welcome card and its figure), its clock at `t0`, ticked once so it starts.
fn welcome(t0: Instant) -> App {
    let (mut a, _rx) = app();
    a.locked = true;
    fx_at(&mut a, t0, 0);
    a.tick_fx();
    a
}

/// The same screen without the stars: the sky only paints with a true-colour ink.
fn bare(a: &mut App, w: u16, h: u16) -> Buffer {
    let ink = a.theme.fg;
    a.theme.fg = Color::Reset;
    let b = buf_of(a, w, h);
    a.theme.fg = ink;
    b
}

/// The cells the stars were painted on: blank without them, one of the eight dots with them.
fn stars(a: &mut App) -> Vec<(u16, u16, Color)> {
    let plain = bare(a, W, H);
    let with = buf_of(a, W, H);
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            if plain[(x, y)].symbol() != with[(x, y)].symbol() {
                assert_eq!(plain[(x, y)].symbol(), " ", "a cell with something in it was painted at {x},{y}");
                assert!(is_star_glyph(with[(x, y)].symbol()), "{x},{y}");
                out.push((x, y, with[(x, y)].fg));
            }
        }
    }
    out
}

#[test]
fn stars_fill_the_free_area_over_the_whole_width_and_keep_off_the_card() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    let mut seen = std::collections::HashSet::new();
    for i in 0..20 {
        fx_at(&mut a, t0, 2000 + i * 150);
        seen.extend(stars(&mut a).into_iter().map(|(x, y, _)| (x, y)));
    }
    assert!(seen.len() > 100, "{}", seen.len());
    // the figure is 36 wide in the middle: there are stars to its left and to its right as well
    assert!(seen.iter().any(|(x, _)| *x < 30) && seen.iter().any(|(x, _)| *x > 80));
    // none at or below the first row of the card (the card is centred, three rows of text at least)
    let card = bare(&mut a, W, H);
    let first =
        (0..H).find(|y| (0..W).any(|x| card[(x, *y)].symbol().trim().chars().any(|c| c.is_alphabetic()))).unwrap();
    assert!(seen.iter().all(|(_, y)| *y < first), "the card starts at row {first}");
}

#[test]
fn the_stars_breathe_from_one_frame_to_the_next() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    fx_at(&mut a, t0, 3000);
    let mut frames = Vec::new();
    for i in 0..3 {
        fx_at(&mut a, t0, 3000 + i * 150);
        frames.push(stars(&mut a));
    }
    for w in frames.windows(2) {
        assert_ne!(w[0], w[1], "the field changes between frames 150 ms apart");
    }
    // a star lit in two frames in a row changes its colour (it swells or dims), by a little
    let mut changed = 0;
    for w in frames.windows(2) {
        for (x, y, c0) in &w[0] {
            if let Some((_, _, c1)) = w[1].iter().find(|(x1, y1, _)| x1 == x && y1 == y) {
                let (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) = (*c0, *c1) else { panic!("true colour") };
                let d = r0.abs_diff(r1).max(g0.abs_diff(g1)).max(b0.abs_diff(b1));
                assert!(d < 70, "{c0:?} -> {c1:?} jumps");
                if d > 0 {
                    changed += 1;
                }
            }
        }
    }
    assert!(changed > 0, "a star that stays lit breathes");
}

#[test]
fn no_stars_with_the_motion_off_or_on_a_mono_theme() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    fx_at(&mut a, t0, 3000);
    assert!(!stars(&mut a).is_empty());
    a.opts.animations = false;
    let symbols = |b: Buffer| b.content.iter().map(|c| c.symbol().to_string()).collect::<Vec<_>>();
    assert_eq!(symbols(buf_of(&mut a, W, H)), symbols(bare(&mut a, W, H)));
    assert!(!a.stars_alive(a.now()));
    a.opts.animations = true;
    a.theme = crate::theme::Theme::mono();
    assert_eq!(symbols(buf_of(&mut a, W, H)), symbols(bare(&mut a, W, H)));
    assert!(!a.stars_alive(a.now()));
}

#[test]
fn at_rest_the_stars_fade_out_in_a_second_and_ask_for_no_more_frames() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    fx_at(&mut a, t0, REST_AFTER_MS - 100);
    let full = stars(&mut a).len();
    assert!(full > 20 && a.stars_alive(a.now()));
    assert_eq!(a.load(), crate::frame::Load::Beat);
    // half way through the fade there are fewer, never more
    fx_at(&mut a, t0, REST_AFTER_MS + STOP_MS / 2);
    let half = stars(&mut a).len();
    assert!(half < full, "{half} against {full}");
    assert!(a.stars_alive(a.now()), "frames still come while they fade");
    // gone within the second, and the frame at that time is the empty sky
    fx_at(&mut a, t0, REST_AFTER_MS + STOP_MS);
    assert!(stars(&mut a).is_empty());
    fx_at(&mut a, t0, REST_AFTER_MS + STOP_MS + 200);
    assert!(stars(&mut a).is_empty());
    a.tick_fx();
    assert!(!a.stars_alive(a.now()));
    assert_eq!(a.load(), crate::frame::Load::Idle, "no frames at rest");
    // and it stays that way
    fx_at(&mut a, t0, REST_AFTER_MS + STOP_MS + 60_000);
    assert!(stars(&mut a).is_empty());
}

#[test]
fn a_key_brings_the_stars_back_and_they_fade_in_instead_of_popping() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    let gone = REST_AFTER_MS + STOP_MS + 5000;
    fx_at(&mut a, t0, gone);
    assert!(stars(&mut a).is_empty());
    a.stars_input();
    assert!(a.stars_alive(a.now()));
    assert!(stars(&mut a).is_empty(), "visibility starts from where it was: nothing");
    fx_at(&mut a, t0, gone + 150);
    let early = stars(&mut a);
    fx_at(&mut a, t0, gone + FADE_IN_MS + 150);
    let full = stars(&mut a);
    assert!(early.len() < full.len(), "{} then {}", early.len(), full.len());
    // dim at first: the colour of every star is nearer the ground than it is later
    let bright = |s: &[(u16, u16, Color)]| {
        s.iter()
            .map(|(_, _, c)| if let Color::Rgb(r, g, b) = c { *r as u32 + *g as u32 + *b as u32 } else { 0 })
            .max()
            .unwrap_or(0)
    };
    assert!(bright(&early) < bright(&full));
}

#[test]
fn a_theme_without_a_ground_of_its_own_gets_the_stars_too() {
    let t0 = Instant::now();
    let mut a = welcome(t0);
    a.theme.bg = Color::Reset;
    let mut seen = 0;
    for i in 0..10 {
        fx_at(&mut a, t0, 2000 + i * 150);
        seen += stars(&mut a).len();
    }
    assert!(seen > 50, "{seen}");
}
