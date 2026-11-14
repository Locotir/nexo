//! The faint stars of the welcome screen: single braille dots that come out of the dark, swell for a
//! moment and go out again, each on its own cycle, on the blank cells of the free area.
//!
//! A cell hosts a star when a hash of its place in the area says so (about one in five). The star has one
//! of the eight single-dot glyphs of a braille cell and a cycle of its own of `CYCLE_MIN_S` to
//! `CYCLE_MIN_S + CYCLE_SPAN_S` seconds with an offset of its own, so no two pulse together. Its brightness
//! is a high power of a sine over the cycle (`SHARP`): a short, sharp peak, and dark for most of the cycle,
//! which is what makes the field look alive instead of lit. A star under `MIN_BRIGHTNESS` is not drawn. The
//! colour is the ink mixed into the cell's own ground (true colour only: the caller checks that).
//!
//! The field is a pure function of the clock it is asked about, with the `sin` and the hash of `fx_stars`
//! (no libm, no randomness). The caller owns the clock, the fade of the whole field (`visibility`) and
//! the frames (see `StarsFx`).

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use unicode_width::UnicodeWidthStr;

use crate::fx_stars::{mix, sin, unit, PI};
use crate::theme::{mix as mix_colour, Theme};

/// A star dimmer than this is not drawn.
pub(crate) const MIN_BRIGHTNESS: f32 = 0.04;
/// How far the ink is mixed in at the top of a star's peak, at full visibility.
const PEAK: f32 = 0.6;
/// A star's cycle lasts between these many seconds, each its own.
const CYCLE_MIN_S: f32 = 4.0;
const CYCLE_SPAN_S: f32 = 3.0;
/// One cell in this many hosts a star.
const HOST_ONE_IN: f32 = 5.0;

/// The seed of the cell `dx`, `dy` from the corner of the area.
pub(crate) fn cell_seed(dx: u16, dy: u16) -> u32 {
    mix(mix(u32::from(dy) << 16 | u32::from(dx)) ^ 0x5bd1_e995)
}

/// Whether the cell with this seed hosts a star.
pub(crate) fn hosts_star(seed: u32) -> bool {
    unit(seed) < 1.0 / HOST_ONE_IN
}

/// The glyph of the star of this seed: one of the eight single dots of a braille cell.
pub(crate) fn glyph(seed: u32) -> char {
    char::from_u32(0x2800 + (1 << (mix(seed ^ 0x9e37_79b9) % 8))).unwrap_or('\u{2801}')
}

/// Whether `c` is one of the eight single-dot glyphs.
#[cfg(test)]
pub(crate) fn is_star_glyph(c: &str) -> bool {
    let mut it = c.chars();
    matches!((it.next(), it.next()), (Some(c), None) if (0x2801..=0x28ff).contains(&(c as u32)) && (c as u32 - 0x2800).count_ones() == 1)
}

/// The sine of the cycle raised to the twelfth power: 1 at the top, and under a twentieth for more than half of it.
fn sharp(x: f32) -> f32 {
    let s2 = sin(x) * sin(x);
    let s4 = s2 * s2;
    s4 * s4 * s4
}

/// How bright the star of this seed is `time` seconds in, at `visibility` 0..1.
pub(crate) fn brightness(seed: u32, time: f32, visibility: f32) -> f32 {
    let period = CYCLE_MIN_S + CYCLE_SPAN_S * unit(seed.wrapping_add(1));
    let phase = (time / period + unit(seed.wrapping_add(2))).fract();
    sharp(phase * PI) * PEAK * visibility
}

/// How the field looks at one moment: the seconds on its clock, the ink, the ground a cell with the default
/// ground is taken to have, and the visibility (0..1) that fades the whole field.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Look {
    pub time: f32,
    pub ink: (u8, u8, u8),
    pub ground: (u8, u8, u8),
    pub visibility: f32,
}

impl Look {
    /// The look of the field on `theme` at `time` seconds and `visibility`: the ink is the theme's text colour
    /// and a cell with the default ground rises from black (the default theme paints no ground of its own).
    /// None where the field is not drawn: a theme without true colour (mono, indexed) has no stars.
    pub(crate) fn on(theme: &Theme, time: f32, visibility: f32) -> Option<Look> {
        let ground = match theme.bg {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Reset => (0, 0, 0),
            _ => return None,
        };
        let Color::Rgb(r, g, b) = theme.fg else { return None };
        Some(Look { time, ink: (r, g, b), ground, visibility })
    }
}

/// Paints the stars of `look` on the blank cells of `area` in `buf`. A cell is left alone when it holds
/// anything but one space (a wide glyph also covers the cell after it), has a modifier, is the `cursor`
/// or lies in `protected`. Nothing at 0 visibility.
pub(crate) fn render_stars(
    area: Rect,
    cursor: Option<(u16, u16)>,
    protected: Option<Rect>,
    look: Look,
    buf: &mut Buffer,
) {
    render_stars_in(area, cursor, protected, None, look, buf);
}

/// `render_stars`, and with `free` (row by row over `area`, which must lie in the buffer) only the cells it
/// marks are used.
pub(crate) fn render_stars_in(
    area: Rect,
    cursor: Option<(u16, u16)>,
    protected: Option<Rect>,
    free: Option<&[bool]>,
    look: Look,
    buf: &mut Buffer,
) {
    let Look { time, ink, ground, visibility } = look;
    let area = area.intersection(buf.area);
    if visibility <= 0.0 || area.is_empty() {
        return;
    }
    for y in area.y..area.bottom() {
        let mut occupied_until = area.x;
        for x in area.x..area.right() {
            let cell = &buf[(x, y)];
            if x < occupied_until {
                continue;
            }
            if cell.symbol() != " " {
                occupied_until = x.saturating_add(cell.symbol().width() as u16);
                continue;
            }
            if free.is_some_and(|f| {
                !f.get(usize::from(y - area.y) * usize::from(area.width) + usize::from(x - area.x))
                    .copied()
                    .unwrap_or(false)
            }) || cursor == Some((x, y))
                || protected.is_some_and(|p| p.contains(Position::new(x, y)))
                || !cell.modifier.is_empty()
            {
                continue;
            }
            let under = match cell.bg {
                Color::Rgb(r, g, b) => (r, g, b),
                Color::Reset => ground,
                _ => continue,
            };
            let seed = cell_seed(x - area.x, y - area.y);
            if !hosts_star(seed) {
                continue;
            }
            let level = brightness(seed, time, visibility);
            if level < MIN_BRIGHTNESS {
                continue;
            }
            let (u, i) = (Color::Rgb(under.0, under.1, under.2), Color::Rgb(ink.0, ink.1, ink.2));
            let mut utf = [0u8; 4];
            buf[(x, y)].set_symbol(glyph(seed).encode_utf8(&mut utf)).set_fg(mix_colour(u, i, level));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Modifier, Style};

    const INK: (u8, u8, u8) = (230, 230, 240);
    const GROUND: (u8, u8, u8) = (10, 10, 20);

    fn field(w: u16, h: u16) -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, w, h));
        for c in b.content.iter_mut() {
            c.set_bg(Color::Rgb(GROUND.0, GROUND.1, GROUND.2));
        }
        b
    }

    fn paint(b: &mut Buffer, t: f32, vis: f32) {
        render_stars(b.area, None, None, Look { time: t, ink: INK, ground: GROUND, visibility: vis }, b);
    }

    fn stars(b: &Buffer) -> Vec<(u16, u16)> {
        let mut v = Vec::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                if is_star_glyph(b[(x, y)].symbol()) {
                    v.push((x, y));
                }
            }
        }
        v
    }

    #[test]
    fn only_blank_unstyled_cells_get_stars() {
        let mut b = field(60, 8);
        b.set_string(2, 1, "hello world", Style::default());
        b.set_string(2, 2, "wide 日本語 text", Style::default());
        for x in 20..30 {
            b[(x, 3)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        b[(40, 4)].set_bg(Color::Indexed(4));
        let protected = Rect::new(0, 5, 60, 1);
        let cursor = Some((10u16, 6u16));
        let before = b.clone();
        let mut found = 0;
        for i in 0..400 {
            let mut now = before.clone();
            let look = Look { time: i as f32 * 0.15, ink: INK, ground: GROUND, visibility: 1.0 };
            render_stars(now.area, cursor, Some(protected), look, &mut now);
            for (i, (a, c)) in before.content.iter().zip(&now.content).enumerate() {
                if a != c {
                    found += 1;
                    let (x, y) = ((i % 60) as u16, (i / 60) as u16);
                    assert_eq!(a.symbol(), " ", "a cell with text was painted at {x},{y}");
                    assert!(a.modifier.is_empty(), "a styled cell was painted at {x},{y}");
                    assert!(y != 5 && (x, y) != (10, 6) && (x, y) != (40, 4), "{x},{y} is kept");
                    assert!(is_star_glyph(c.symbol()));
                    assert!(matches!(c.fg, Color::Rgb(..)));
                }
            }
        }
        assert!(found > 100, "the free cells do get stars: {found}");
    }

    #[test]
    fn about_one_cell_in_five_hosts_a_star() {
        let b = field(100, 30);
        let mut seen = std::collections::HashSet::new();
        for i in 0..600 {
            let mut now = b.clone();
            paint(&mut now, i as f32 * 0.15, 1.0);
            seen.extend(stars(&now));
        }
        let share = seen.len() as f32 / (100.0 * 30.0);
        assert!((0.17..0.23).contains(&share), "{share}");
        for (x, y) in &seen {
            assert!(hosts_star(cell_seed(*x, *y)));
        }
    }

    #[test]
    fn a_star_changes_smoothly_between_frames_and_is_dark_most_of_its_cycle() {
        let mut dark = 0;
        let mut total = 0;
        for dx in 0..200u16 {
            let seed = cell_seed(dx, 3);
            if !hosts_star(seed) {
                continue;
            }
            let mut last = brightness(seed, 0.0, 1.0);
            for i in 1..=300 {
                let now = brightness(seed, i as f32 * 0.15, 1.0);
                assert!((now - last).abs() <= 0.2, "jump {last} -> {now} at frame {i}");
                last = now;
                total += 1;
                if now < MIN_BRIGHTNESS {
                    dark += 1;
                }
            }
        }
        assert!(total > 5000);
        let share = dark as f32 / total as f32;
        assert!(share > 0.5 && share < 0.7, "dark {share} of the time");
    }

    #[test]
    fn a_star_swells_to_its_peak_and_goes_out() {
        let seed = (0..).map(|d| cell_seed(d, 0)).find(|h| hosts_star(*h)).unwrap();
        let samples: Vec<f32> = (0..2000).map(|i| brightness(seed, i as f32 * 0.01, 1.0)).collect();
        let top = samples.iter().cloned().fold(0.0, f32::max);
        assert!(top > 0.5 && top <= PEAK + 1e-4, "{top}");
        assert!(samples.iter().any(|v| *v < 0.001));
    }

    #[test]
    fn no_visibility_paints_nothing_and_half_is_dimmer() {
        let b = field(80, 20);
        let mut off = b.clone();
        paint(&mut off, 3.0, 0.0);
        assert_eq!(off, b);
        let (mut full, mut half) = (b.clone(), b.clone());
        paint(&mut full, 5.0, 1.0);
        paint(&mut half, 5.0, 0.5);
        assert!(stars(&half).len() < stars(&full).len());
    }

    #[test]
    fn the_colour_runs_from_the_ground_towards_the_ink() {
        let b = field(80, 20);
        let mut now = b.clone();
        paint(&mut now, 5.0, 1.0);
        let lit = stars(&now);
        assert!(!lit.is_empty());
        for (x, y) in lit {
            let Color::Rgb(r, g, bl) = now[(x, y)].fg else { panic!("true colour") };
            assert!(r >= GROUND.0 && r <= INK.0 && g >= GROUND.1 && g <= INK.1 && bl >= GROUND.2 && bl <= INK.2);
        }
    }

    #[test]
    fn cells_with_the_default_ground_take_the_one_given() {
        let mut b = Buffer::empty(Rect::new(0, 0, 80, 20));
        paint(&mut b, 5.0, 1.0);
        assert!(!stars(&b).is_empty());
    }
}
