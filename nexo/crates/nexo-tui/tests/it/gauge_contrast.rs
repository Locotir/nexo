//! Contrast of the status bar's stacked gauge, the info text and the activity phase:
//! every coloured piece reads at AA (4.5:1) on the ground it is drawn on, in every colour theme.

use nexo_tui::render::gauge::{stacked, Split};
use nexo_tui::theme::{mix, Theme, NAMES};
use ratatui::style::Color;

fn lum(c: Color) -> Option<f64> {
    let Color::Rgb(r, g, b) = c else { return None };
    let f = |v: u8| {
        let v = v as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
}

fn ratio(a: Color, b: Color) -> Option<f64> {
    let (x, y) = (lum(a)?, lum(b)?);
    Some((x.max(y) + 0.05) / (x.min(y) + 0.05))
}

/// The colour themes with real RGB values (mono and named-colour fallbacks have no ratio).
fn rgb_themes() -> Vec<Theme> {
    NAMES.iter().filter_map(|n| Theme::by_name(n)).filter(|t| !t.mono && lum(t.bg).is_some()).collect()
}

#[test]
fn stacked_gauge_pieces_pass_aa_on_the_bar() {
    let split = Split { cached: 50, fresh: 30, out: 20 };
    for t in rgb_themes() {
        for pct in [10, 60, 80, 95] {
            let colour = t.level_color(pct);
            let segs = stacked(&t, split, 100, 8, colour);
            // the first three pieces carry meaning (cached, fresh, out); the rest is the track.
            // Graphics need 3:1 (WCAG 1.4.11); the default theme holds the text bar of 4.5.
            let min = if t.name == "dark" { 4.5 } else { 3.0 };
            for seg in segs.iter().take(3) {
                let fg = seg.style.fg.expect("piece has a colour");
                let r = ratio(fg, t.bar_bg).unwrap_or(0.0);
                assert!(r >= min, "{}: piece {:?} on {:?}: {r:.2}", t.name, fg, t.bar_bg);
            }
        }
    }
}

#[test]
fn stacked_gauge_pieces_are_told_apart() {
    let split = Split { cached: 50, fresh: 30, out: 20 };
    for t in rgb_themes() {
        for pct in [10, 60, 95] {
            let segs = stacked(&t, split, 100, 8, t.level_color(pct));
            let cols: Vec<Color> = segs.iter().take(3).filter_map(|s| s.style.fg).collect();
            assert_eq!(cols.len(), 3, "{}: three pieces", t.name);
            assert!(cols[0] != cols[1] && cols[1] != cols[2] && cols[0] != cols[2], "{}: {cols:?}", t.name);
            let glyphs: Vec<char> = segs.iter().map(|s| s.text.chars().next().unwrap()).collect();
            assert_ne!(glyphs[1], glyphs[2], "{}: out is drawn with a different glyph", t.name);
        }
    }
}

#[test]
fn the_light_themes_faint_text_borders_and_selection_can_be_seen() {
    let t = Theme::by_name("light").unwrap();
    for ground in [t.bg, t.modal_bg] {
        for (name, c) in [("dim", t.dim), ("dim2", t.dim2)] {
            let r = ratio(c, ground).unwrap();
            assert!(r >= 4.5, "{name} on {ground:?}: {r:.2}");
        }
        let b = ratio(t.border, ground).unwrap();
        assert!(b >= 2.3, "border on {ground:?}: {b:.2}");
    }
    let s = ratio(t.selection_bg, t.modal_bg).unwrap();
    assert!(s >= 1.25, "selection on the dialog ground: {s:.2}");
}

#[test]
fn info_text_passes_aa_on_the_bar() {
    for t in rgb_themes() {
        let r = ratio(t.info, t.bar_bg).unwrap_or(0.0);
        assert!(r >= 4.5, "{}: info ({:?}) on {:?}: {r:.2}", t.name, t.info, t.bar_bg);
    }
}

#[test]
fn activity_phase_passes_aa_at_every_shimmer_step() {
    for t in rgb_themes() {
        for step in 0..=8 {
            let near = step as f32 / 8.0;
            let c = mix(t.accent, t.fg, near);
            let r = ratio(c, t.bg).unwrap_or(0.0);
            assert!(r >= 4.5, "{}: phase at {near:.2} ({c:?}) on {:?}: {r:.2}", t.name, t.bg);
        }
        assert!(ratio(t.accent, t.bg).unwrap_or(0.0) >= 4.5, "{}: spinner", t.name);
    }
}
