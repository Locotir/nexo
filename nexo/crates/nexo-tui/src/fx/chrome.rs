//! Motion of the chrome around the transcript: the welcome card coming in, the context gauge easing
//! and changing colour, the scrollbar thumb lighting while the view scrolls, a new chip in the draft
//! and a rejected draft. Pure functions of the time they are asked about; the caller injects `now`.

use std::time::Instant;

use ratatui::style::Color;

use crate::anim::{ease_out, progress, Tween};
use crate::motion::fade;
use crate::theme::Theme;

/// The welcome card's lines come in, top to bottom, in this long in all.
pub const WELCOME_MS: u64 = 350;
/// One line of the welcome card fades in over this long.
const LINE_MS: f32 = 150.0;
/// The context gauge eases to a new fill, and fades to a new colour band, in this long.
pub const GAUGE_MS: u64 = 300;
/// The scrollbar thumb fades back to its resting colour this long after the last scroll.
pub const BAR_MS: u64 = 600;
/// A new chip in the draft is lit once for this long.
pub const CHIP_MS: u64 = 300;
/// The rule over the draft flashes the error colour once for this long when the draft is rejected.
pub const REJECT_MS: u64 = 300;

/// 0..1: how far line `i` of `n` of the welcome card has come in `since` its start (1 = settled).
pub fn welcome_level(i: usize, n: usize, since: Instant, now: Instant) -> f32 {
    let e = now.saturating_duration_since(since).as_millis() as f32;
    let stagger = (WELCOME_MS as f32 - LINE_MS) / n.saturating_sub(1).max(1) as f32;
    ease_out(((e - i as f32 * stagger) / LINE_MS).clamp(0.0, 1.0) as f64) as f32
}

/// 1 right after `at`, easing to 0 over `ms`; 0 when there is no start or it is over.
pub fn fade_out(at: Option<Instant>, now: Instant, ms: u64) -> f32 {
    match at {
        Some(_) => (1.0 - ease_out(progress(at, now, ms))) as f32,
        None => 0.0,
    }
}

/// The context gauge as drawn: its three parts (tokens) and its share ease to new values, and its
/// colour fades when the level crosses into a new band.
#[derive(Debug, Clone)]
pub struct GaugeFx {
    parts: [Tween; 3],
    frac: Tween,
    band_to: Option<Color>,
    band_from: Color,
    band_at: Option<Instant>,
}

impl Default for GaugeFx {
    fn default() -> Self {
        GaugeFx {
            parts: [Tween::new(0.0, GAUGE_MS); 3],
            frac: Tween::new(0.0, GAUGE_MS),
            band_to: None,
            band_from: Color::Reset,
            band_at: None,
        }
    }
}

impl GaugeFx {
    /// What the gauge is now: its parts (None while the last call is only guessed), its share and the
    /// colour of its level. The first look and the motion off are the values themselves.
    pub fn note(&mut self, theme: &Theme, parts: Option<[u64; 3]>, frac: f64, colour: Color, now: Instant, on: bool) {
        let first = self.band_to.is_none();
        let still = first || !on;
        if let Some(p) = parts {
            for (t, v) in self.parts.iter_mut().zip(p) {
                if still {
                    t.snap(v as f64);
                } else {
                    t.retarget(v as f64, now);
                }
            }
        }
        if still {
            self.frac.snap(frac);
        } else {
            self.frac.retarget(frac, now);
        }
        if still {
            self.band_at = None;
        } else if let Some(old) = self.band_to.filter(|o| *o != colour) {
            self.band_from = self.colour_at(theme, now, old);
            self.band_at = Some(now);
        }
        self.band_to = Some(colour);
    }

    /// The parts at `now`; `target` itself when the gauge has not been told about it (yet).
    pub fn parts_at(&self, now: Instant, target: [u64; 3]) -> [u64; 3] {
        let told = self.parts.iter().zip(target).all(|(t, v)| t.target() == v as f64);
        if !told {
            return target;
        }
        let mut out = target;
        for (o, t) in out.iter_mut().zip(&self.parts) {
            *o = t.value_at(now).round().max(0.0) as u64;
        }
        out
    }

    /// The share at `now`; `target` itself when not told.
    pub fn frac_at(&self, now: Instant, target: f64) -> f64 {
        if (self.frac.target() - target).abs() > f64::EPSILON {
            return target;
        }
        self.frac.value_at(now)
    }

    /// The colour of the level at `now`: a fade from the old band's colour to `target`.
    pub fn colour_at(&self, theme: &Theme, now: Instant, target: Color) -> Color {
        if self.band_to != Some(target) {
            return target;
        }
        match self.band_at {
            Some(_) => fade(theme, self.band_from, target, ease_out(progress(self.band_at, now, GAUGE_MS)) as f32),
            None => target,
        }
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.frac.is_moving(now)
            || self.parts.iter().any(|t| t.is_moving(now))
            || crate::motion::within(self.band_at, now, GAUGE_MS)
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
    fn welcome_lines_come_in_one_after_another_and_settle() {
        let t0 = Instant::now();
        assert_eq!(welcome_level(0, 5, t0, t0), 0.0);
        assert_eq!(welcome_level(4, 5, t0, at(t0, 100)), 0.0, "the last line has not begun");
        let mid = welcome_level(0, 5, t0, at(t0, 50));
        assert!(mid > 0.0 && mid < 1.0);
        assert!(welcome_level(0, 5, t0, at(t0, 120)) > welcome_level(4, 5, t0, at(t0, 120)));
        for i in 0..5 {
            assert_eq!(welcome_level(i, 5, t0, at(t0, WELCOME_MS)), 1.0);
        }
        assert_eq!(welcome_level(0, 1, t0, at(t0, WELCOME_MS)), 1.0);
    }

    #[test]
    fn fade_out_starts_lit_and_rests_at_zero() {
        let t0 = Instant::now();
        assert_eq!(fade_out(None, t0, BAR_MS), 0.0);
        assert_eq!(fade_out(Some(t0), t0, BAR_MS), 1.0);
        let mid = fade_out(Some(t0), at(t0, 300), BAR_MS);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(fade_out(Some(t0), at(t0, BAR_MS), BAR_MS), 0.0);
    }

    #[test]
    fn gauge_eases_to_the_new_fill_and_fades_to_the_new_band() {
        let th = Theme::dark();
        let t0 = Instant::now();
        let mut g = GaugeFx::default();
        g.note(&th, Some([10, 10, 0]), 0.2, th.success, t0, true);
        assert_eq!(g.parts_at(t0, [10, 10, 0]), [10, 10, 0], "the first look is the value");
        assert!(!g.moving(t0));
        g.note(&th, Some([60, 40, 0]), 0.6, th.warn, t0, true);
        assert!(g.moving(t0));
        assert_eq!(g.parts_at(t0, [60, 40, 0]), [10, 10, 0]);
        assert_eq!(g.colour_at(&th, t0, th.warn), th.success);
        let m = g.parts_at(at(t0, 100), [60, 40, 0]);
        assert!(m[0] > 10 && m[0] < 60);
        let c = g.colour_at(&th, at(t0, 100), th.warn);
        assert!(c != th.success && c != th.warn);
        let end = at(t0, GAUGE_MS);
        assert_eq!(g.parts_at(end, [60, 40, 0]), [60, 40, 0]);
        assert_eq!(g.colour_at(&th, end, th.warn), th.warn);
        assert!(!g.moving(end));
        assert!((g.frac_at(end, 0.6) - 0.6).abs() < 1e-9);
    }

    #[test]
    fn gauge_without_motion_is_the_value_and_untold_targets_pass_through() {
        let th = Theme::dark();
        let t0 = Instant::now();
        let mut g = GaugeFx::default();
        g.note(&th, Some([1, 1, 1]), 0.1, th.success, t0, false);
        g.note(&th, Some([9, 9, 9]), 0.9, th.error, t0, false);
        assert_eq!(g.parts_at(t0, [9, 9, 9]), [9, 9, 9]);
        assert_eq!(g.colour_at(&th, t0, th.error), th.error);
        assert!(!g.moving(t0));
        assert_eq!(g.parts_at(t0, [5, 5, 5]), [5, 5, 5]);
        assert_eq!(g.frac_at(t0, 0.5), 0.5);
    }
}
