//! Motion that feels alive: colour easing, counters that roll, the shimmer and
//! the pop of tool cards, the glow along the rule when a turn ends. Everything
//! is a pure function of the time it is asked about (the caller injects `now`),
//! and every effect has a still final state for `[ui] animations = false`.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::anim::{ease_out, Tween};
use crate::theme::{mix, Theme};

/// A figure rolls to its new value in this long.
pub const ROLL_MS: u64 = 400;
/// The glow along the rule, and the figures' single pulse, after a turn.
pub const GLOW_MS: u64 = 600;
/// A finished card's mark pops, and its rail eases to the final colour, in this long.
pub const POP_MS: u64 = 300;
pub const RAIL_MS: u64 = 350;
/// One breath of the running rail.
const SHIMMER_MS: f32 = 1600.0;

/// The RGB of an entry of the 256-colour palette (cube and grey ramp); None for the first 16.
pub fn indexed_rgb(i: u8) -> Option<(u8, u8, u8)> {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        16..=231 => {
            let n = (i - 16) as usize;
            Some((LEVELS[n / 36], LEVELS[(n / 6) % 6], LEVELS[n % 6]))
        }
        232..=255 => {
            let g = 8 + 10 * (i - 232);
            Some((g, g, g))
        }
        _ => None,
    }
}

fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(i) => indexed_rgb(i),
        _ => None,
    }
}

/// `a` eased `t` (0..1) of the way to `b`. Truecolor: a smooth blend. A 256-colour theme steps between the
/// nearest palette entries of the blend; 16 colours (and anything else) switch half way. A mono theme has no
/// colour motion: it stays on `a`.
pub fn fade(theme: &Theme, a: Color, b: Color, t: f32) -> Color {
    if theme.mono {
        return a;
    }
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(..), Color::Rgb(..)) => mix(a, b, t),
        (Color::Indexed(_), Color::Indexed(_)) => match (rgb_of(a), rgb_of(b)) {
            (Some(x), Some(y)) => {
                let m = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t).round() as u8;
                Color::Indexed(crate::theme::nearest_256(m(x.0, y.0), m(x.1, y.1), m(x.2, y.2)))
            }
            _ => step(a, b, t),
        },
        _ => step(a, b, t),
    }
}

fn step(a: Color, b: Color, t: f32) -> Color {
    if t < 0.5 {
        a
    } else {
        b
    }
}

/// A figure that rolls to its target (ease-out, `ROLL_MS`); with the motion off it is the target at once.
#[derive(Debug, Clone, Copy)]
pub struct Counter {
    tween: Tween,
}

impl Default for Counter {
    fn default() -> Counter {
        Counter { tween: Tween::new(0.0, ROLL_MS) }
    }
}

impl Counter {
    /// The value to show at `now` for `target`; true while it still rolls.
    pub fn sample(&mut self, target: f64, now: Instant, animate: bool) -> (f64, bool) {
        if !animate {
            self.tween.snap(target);
            return (target, false);
        }
        self.tween.retarget(target, now);
        let v = self.tween.value_at(now);
        (v, self.tween.moving(now))
    }
}

/// `shown` padded on the left to the width of `settled`, so a rolling figure never moves the text around it.
pub fn fixed(shown: &str, settled: &str) -> String {
    let w = unicode_width::UnicodeWidthStr::width;
    let pad = w(settled).saturating_sub(w(shown));
    format!("{}{shown}", " ".repeat(pad))
}

/// 0..1..0 once over `ms` since `at` (a hump); 0 outside.
pub fn hump(at: Option<Instant>, now: Instant, ms: u64) -> f32 {
    let Some(at) = at else { return 0.0 };
    let p = crate::anim::progress(Some(at), now, ms);
    if p >= 1.0 {
        return 0.0;
    }
    (p as f32 * std::f32::consts::PI).sin()
}

/// Whether something that began at `at` is still moving `ms` later.
pub fn within(at: Option<Instant>, now: Instant, ms: u64) -> bool {
    at.is_some_and(|t| (now.saturating_duration_since(t).as_millis() as u64) < ms)
}

/// The rule of `n` cells with a glow sweeping once from left to right (`GLOW_MS`) after `at`.
pub fn sweep(theme: &Theme, n: usize, at: Instant, now: Instant) -> Vec<Span<'static>> {
    const TAIL: f32 = 10.0;
    let p = crate::anim::progress(Some(at), now, GLOW_MS) as f32;
    let head = -TAIL + p * (n as f32 + 2.0 * TAIL);
    let mut out: Vec<Span<'static>> = Vec::new();
    for i in 0..n {
        let d = head - i as f32;
        let glow = if d < 0.0 { (1.0 + d).max(0.0) } else { (1.0 - d / TAIL).max(0.0) };
        let style = theme.style(fade(theme, theme.border, theme.accent, glow));
        match out.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push('─'),
            _ => out.push(Span::styled("─".to_string(), style)),
        }
    }
    out
}

/// What a tool card is doing, for its rail and mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Card {
    /// Running: the rail breathes.
    Running { now_ms: u64 },
    /// Finished `since_ms` ago: the mark pops, the rail eases to its final colour.
    Done { since_ms: u64 },
}

const RAIL: &str = "│ ";

/// Applies the card's motion to its rendered lines: the rail is the `│ ` spans, the mark the first span of the head.
pub fn card_fx(lines: &mut [Line<'static>], card: Card, theme: &Theme) {
    if theme.mono {
        // no colour motion; the pop still reads as weight
        if let Card::Done { since_ms } = card {
            if since_ms < POP_MS {
                if let Some(s) = lines.first_mut().and_then(|l| l.spans.first_mut()) {
                    s.style = s.style.add_modifier(Modifier::BOLD);
                }
            }
        }
        return;
    }
    match card {
        Card::Running { now_ms } => {
            for (row, line) in lines.iter_mut().enumerate() {
                for s in line.spans.iter_mut().filter(|s| s.content == RAIL) {
                    let phase = now_ms as f32 / SHIMMER_MS - row as f32 / 7.0;
                    let wave = 0.5 + 0.5 * (phase * std::f32::consts::TAU).sin();
                    let base = s.style.fg.unwrap_or(theme.accent);
                    s.style = s.style.fg(fade(theme, base, theme.border, wave * 0.65));
                }
            }
        }
        Card::Done { since_ms } => {
            if since_ms < RAIL_MS {
                let t = ease_out(since_ms as f64 / RAIL_MS as f64) as f32;
                for line in lines.iter_mut() {
                    for s in line.spans.iter_mut().filter(|s| s.content == RAIL) {
                        if let Some(end) = s.style.fg {
                            s.style = s.style.fg(fade(theme, theme.accent, end, t));
                        }
                    }
                }
            }
            if since_ms < POP_MS {
                if let Some(s) = lines.first_mut().and_then(|l| l.spans.first_mut()) {
                    let k = 1.0 - since_ms as f32 / POP_MS as f32;
                    let base = s.style.fg.unwrap_or(theme.fg);
                    s.style = Style { fg: Some(fade(theme, base, theme.fg, 0.7 * k)), ..s.style }
                        .add_modifier(Modifier::BOLD);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn fade_blends_truecolor_and_steps_on_the_small_palettes() {
        let th = Theme::dark();
        let (a, b) = (Color::Rgb(0, 0, 0), Color::Rgb(100, 200, 50));
        assert_eq!(fade(&th, a, b, 0.0), a);
        assert_eq!(fade(&th, a, b, 1.0), b);
        assert_eq!(fade(&th, a, b, 0.5), Color::Rgb(50, 100, 25));
        let (i, j) = (Color::Indexed(16), Color::Indexed(231));
        assert_eq!(fade(&th, i, j, 0.0), i);
        assert_eq!(fade(&th, i, j, 1.0), j);
        let mid = fade(&th, i, j, 0.5);
        assert!(matches!(mid, Color::Indexed(n) if n != 16 && n != 231), "a palette entry between: {mid:?}");
        assert_eq!(fade(&th, Color::Red, Color::Green, 0.2), Color::Red);
        assert_eq!(fade(&th, Color::Red, Color::Green, 0.8), Color::Green);
        let mono = Theme::mono();
        assert_eq!(fade(&mono, a, b, 1.0), a, "no colour motion in mono");
    }

    #[test]
    fn a_counter_rolls_to_the_target_and_snaps_without_motion() {
        let t0 = Instant::now();
        let mut c = Counter::default();
        assert_eq!(c.sample(0.0, t0, true), (0.0, false));
        let (v0, m0) = c.sample(1000.0, t0, true);
        assert_eq!(v0, 0.0);
        assert!(m0);
        let (mid, moving) = c.sample(1000.0, at(t0, 100), true);
        assert!(mid > 0.0 && mid < 1000.0 && moving, "{mid}");
        assert!(mid > 100.0 * 1000.0 / ROLL_MS as f64, "ease-out: ahead of linear");
        assert_eq!(c.sample(1000.0, at(t0, ROLL_MS), true).0, 1000.0);
        assert!(!c.sample(1000.0, at(t0, ROLL_MS + 10), true).1);
        assert_eq!(c.sample(5.0, at(t0, 900), false), (5.0, false));
    }

    #[test]
    fn a_rolling_figure_keeps_its_width() {
        assert_eq!(fixed("9", "1.2k"), "   9");
        assert_eq!(fixed("1.2k", "1.2k"), "1.2k");
        assert_eq!(fixed("12.5k", "9"), "12.5k");
    }

    #[test]
    fn a_hump_rises_and_falls_once() {
        let t0 = Instant::now();
        assert_eq!(hump(None, t0, GLOW_MS), 0.0);
        assert!(hump(Some(t0), t0, GLOW_MS) < 0.01);
        assert!(hump(Some(t0), at(t0, GLOW_MS / 2), GLOW_MS) > 0.99);
        assert_eq!(hump(Some(t0), at(t0, GLOW_MS), GLOW_MS), 0.0);
        assert!(within(Some(t0), at(t0, 10), GLOW_MS));
        assert!(!within(Some(t0), at(t0, GLOW_MS), GLOW_MS));
    }

    #[test]
    fn the_sweep_is_as_wide_as_asked_and_moves() {
        let th = Theme::dark();
        let t0 = Instant::now();
        let cells = |ms| -> (usize, Vec<Style>) {
            let s = sweep(&th, 40, t0, at(t0, ms));
            (s.iter().map(|x| x.content.chars().count()).sum(), s.iter().map(|x| x.style).collect())
        };
        let (w1, a) = cells(100);
        let (w2, b) = cells(300);
        assert_eq!((w1, w2), (40, 40));
        assert_ne!(a, b, "the glow has moved");
    }

    fn rail_card(th: &Theme) -> Vec<Line<'static>> {
        let mut c = card(th);
        c[1].spans[0].style = th.style(th.accent);
        c
    }

    fn card(th: &Theme) -> Vec<Line<'static>> {
        vec![
            Line::from(vec![Span::styled("✓ ", th.style(th.success)), Span::raw("bash")]),
            Line::from(vec![Span::styled(RAIL, th.style(th.border)), Span::raw("out")]),
        ]
    }

    #[test]
    fn a_running_rail_breathes_and_a_finished_one_settles() {
        let th = Theme::dark();
        let mut a = rail_card(&th);
        let mut b = rail_card(&th);
        card_fx(&mut a, Card::Running { now_ms: 0 }, &th);
        card_fx(&mut b, Card::Running { now_ms: 400 }, &th);
        assert_ne!(a[1].spans[0].style.fg, b[1].spans[0].style.fg);
        let mut start = card(&th);
        card_fx(&mut start, Card::Done { since_ms: 0 }, &th);
        assert_eq!(start[1].spans[0].style.fg, Some(th.accent), "starts on the running colour");
        assert!(start[0].spans[0].style.add_modifier.contains(Modifier::BOLD), "the mark pops");
        let mut end = card(&th);
        card_fx(&mut end, Card::Done { since_ms: RAIL_MS.max(POP_MS) }, &th);
        assert_eq!(end, card(&th), "settled: untouched");
    }

    #[test]
    fn mono_has_no_colour_motion() {
        let th = Theme::mono();
        let mut c = card(&th);
        card_fx(&mut c, Card::Running { now_ms: 400 }, &th);
        assert_eq!(c, card(&th));
        card_fx(&mut c, Card::Done { since_ms: 10 }, &th);
        assert_eq!(c[1], card(&th)[1]);
    }
}
