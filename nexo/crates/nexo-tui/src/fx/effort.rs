//! Motion confined to the reasoning level chip in the status bar.
use crate::anim::ease_out;
use crate::app::App;
use crate::motion::{fade, within};
use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::f32::consts::TAU;
use std::time::Instant;

pub const IGNITE_MS: u64 = 1100;
pub const BREATH_MS: u64 = 2400;

pub fn tag_colour(theme: &Theme, i: usize, count: usize, phase: f32) -> Color {
    let s = if count > 1 { i as f32 / (count - 1) as f32 * 0.5 } else { 0.0 };
    let g = (1.0 - (TAU * (s - phase)).cos()) / 2.0;
    fade(theme, theme.accent2, theme.accent, g)
}

/// The chip lights immediately, then eases back to its gradient.
fn landing(at: Option<Instant>, now: Instant) -> f32 {
    let Some(at) = at else { return 0.0 };
    let ms = now.saturating_duration_since(at).as_millis() as u64;
    if ms >= IGNITE_MS {
        return 0.0;
    }
    1.0 - ease_out(ms as f64 / IGNITE_MS as f64) as f32
}

/// How long a cycle of `period` that began at `from` runs when the work stopped at `stop`: to the end of
/// the cycle it was in, so everything comes back to rest on a whole cycle.
fn cycle_len(from: Instant, stop: Instant, period: u64) -> u64 {
    let ran = stop.saturating_duration_since(from).as_millis() as u64;
    ran.div_ceil(period) * period
}

/// Whether the cycle of `period` that began at `from` (and ended its work at `stop`, if it has) still moves.
fn cycling(from: Option<Instant>, stop: Option<Instant>, now: Instant, period: u64) -> bool {
    let Some(from) = from else { return false };
    match stop {
        None => true,
        Some(stop) => (now.saturating_duration_since(from).as_millis() as u64) < cycle_len(from, stop, period),
    }
}

/// Whether the breath that began at `from` (and ended its work at `stop`, if it has) still moves.
pub fn breathing(from: Option<Instant>, stop: Option<Instant>, now: Instant) -> bool {
    cycling(from, stop, now, BREATH_MS)
}

/// Where in its cycle of `period` the clock is, 0..1; 0 when it does not move.
fn phase(from: Option<Instant>, stop: Option<Instant>, now: Instant, period: u64) -> f32 {
    let Some(at) = from.filter(|_| cycling(from, stop, now, period)) else { return 0.0 };
    (now.saturating_duration_since(at).as_millis() as u64 % period) as f32 / period as f32
}

/// 0..1..0 once every `BREATH_MS` from rest, a smooth curve; 0 when it does not move.
pub fn breath(from: Option<Instant>, stop: Option<Instant>, now: Instant) -> f32 {
    (1.0 - (phase(from, stop, now, BREATH_MS) * TAU).cos()) / 2.0
}

/// Starts and ends a cycle: it runs while there is work, and a stopped one finishes its cycle.
fn step(
    live: bool,
    from: Option<Instant>,
    stop: Option<Instant>,
    period: u64,
    now: Instant,
) -> (Option<Instant>, Option<Instant>) {
    match (live, from) {
        (true, None) => (Some(now), None),
        // work came back while the last cycle was finishing: it goes on from where it is
        (true, Some(_)) if stop.is_some() => {
            if cycling(from, stop, now, period) {
                (from, None)
            } else {
                (Some(now), None)
            }
        }
        (false, Some(_)) if stop.is_none() => (from, Some(now)),
        (false, Some(_)) if !cycling(from, stop, now, period) => (None, None),
        _ => (from, stop),
    }
}

impl App {
    pub fn effort_top(&self) -> bool {
        self.effort_levels.len() >= 3
            && self.effort.as_deref().is_some_and(|e| self.effort_levels.last().map(String::as_str) == Some(e))
    }

    pub(crate) fn note_effort_event(&mut self, changed: bool, same_list: bool) {
        if changed && same_list && self.fx_on() {
            self.fx.effort_at = Some(self.now());
        } else if !same_list {
            self.fx.effort_at = None;
        }
    }

    pub(crate) fn note_effort(&mut self, now: Instant) {
        if !self.fx_on() {
            self.fx.effort_at = None;
            (self.fx.breath_from, self.fx.breath_stop) = (None, None);
            return;
        }
        let live = self.busy;
        if !self.effort_top() {
            (self.fx.breath_from, self.fx.breath_stop) = (None, None);
        } else {
            (self.fx.breath_from, self.fx.breath_stop) =
                step(live, self.fx.breath_from, self.fx.breath_stop, BREATH_MS, now);
        }
    }

    pub(crate) fn effort_moving(&self, now: Instant) -> bool {
        self.fx_on()
            && self.status_hits.iter().any(|(_, h)| *h == crate::app::StatusHit::Effort)
            && (within(self.fx.effort_at, now, IGNITE_MS)
                || (self.effort_top() && cycling(self.fx.breath_from, self.fx.breath_stop, now, BREATH_MS)))
    }

    /// Render the existing chip without changing its text, width or hover style.
    pub(crate) fn effort_chip_spans(&self, shown: &str, rest: Style, hover: Modifier) -> Option<Vec<Span<'static>>> {
        if !self.fx_on() {
            return None;
        }
        let now = self.now();
        let lit = landing(self.fx.effort_at, now);
        if !self.effort_top() && lit <= 0.0 {
            return None;
        }
        let lead = shown.len() - shown.trim_start().len();
        let (blank, tag) = shown.split_at(lead);
        let count = tag.chars().filter(|c| *c != ' ').count();
        let flow = phase(self.fx.breath_from, self.fx.breath_stop, now, BREATH_MS);
        let mut spans = vec![Span::styled(blank.to_string(), rest)];
        let mut i = 0;
        for ch in tag.chars() {
            let mut style = rest.add_modifier(hover);
            if ch != ' ' {
                let base = if self.effort_top() {
                    tag_colour(&self.theme, i, count, flow)
                } else {
                    rest.fg.unwrap_or(self.theme.accent2)
                };
                style = style.fg(fade(&self.theme, base, self.theme.bar_fg, lit * 0.85));
                if lit > 0.0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                i += 1;
            }
            spans.push(Span::styled(ch.to_string(), style));
        }
        Some(spans)
    }
}
