//! The top reasoning level in motion, in pieces already on screen. When the level is set, a comet runs
//! the rule over the draft into the small tag with the level (`◆ high`), which lights up and settles into
//! a two-colour gradient; the `◆` of the status bar pops with it. While the model works, a soft band drifts
//! along the rule, the gradient of the tag flows and the `◆` breathes. Nothing new is drawn: at rest the
//! rule is the rule and the tag has its still gradient. Pure functions of the time they are asked about;
//! the caller injects `now`.

use std::f32::consts::{PI, TAU};
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::anim::ease_out;
use crate::app::App;
use crate::motion::{ease_in_out, fade, within};
use crate::theme::Theme;

/// The whole ignite: the comet, then the landing, then it is over.
pub const IGNITE_MS: u64 = 1100;
/// The comet runs the rule in this long and its tail drains into the tag over `DRAIN_MS` more.
const COMET_MS: u64 = 700;
const DRAIN_MS: u64 = 300;
/// The tag (and the `◆` of the bar) light up from this moment and settle by `IGNITE_MS`.
const LAND_FROM: u64 = 600;
/// One breath of the mark, and one turn of the flow of the tag, while the model works.
pub const BREATH_MS: u64 = 2400;
/// One pass of the band along the rule while the model works.
pub const BAND_MS: u64 = 3200;
/// The tail of the comet is a third of the rule and at least this many cells.
const TAIL_MIN: f32 = 14.0;
/// How far the head of the comet goes from the bar's text colour toward `accent2` (0..1).
const HEAD: f32 = 0.3;
/// The band is a fifth of the rule wide (at least 6 cells) and, at its peak, this far from the rule's
/// colour toward `accent` (0..1).
const BAND_WIDTH: f32 = 0.2;
const BAND_PEAK: f32 = 0.45;
/// How far toward the bright colour the mark goes at the top of a breath (0..1).
const DEPTH: f32 = 0.8;
/// How far toward the bar's text colour the brightest mark and tag are.
const BRIGHT: f32 = 1.0;
const TAG_BRIGHT: f32 = 0.85;

/// The cell the head of the comet is at, `ms` after the level was set, over the first `l` cells of the rule
/// (the ones before the tag): it runs eased from the first cell to the tag and stops there. None once it
/// has stopped.
pub fn comet_head(l: usize, ms: u64) -> Option<usize> {
    (l > 0 && ms < COMET_MS).then(|| ((ease_in_out(ms as f64 / COMET_MS as f64) as f32 * l as f32) as usize).min(l - 1))
}

/// The colour of the head of the comet.
pub fn head_colour(theme: &Theme) -> Color {
    fade(theme, theme.bar_fg, theme.accent2, HEAD)
}

/// The colour of the tail of the comet at cell `x` of the first `l` cells of the rule, `ms` after the level
/// was set, over a rule whose own colour there is `rest`; None where it does not reach. The tail is `accent2`
/// near the head, `accent` further back and then the rule's colour, and drains away into the tag once the
/// head has stopped.
pub fn comet_tail(theme: &Theme, rest: Color, x: usize, l: usize, ms: u64) -> Option<Color> {
    if l == 0 || x >= l || ms >= COMET_MS + DRAIN_MS {
        return None;
    }
    let tail = (l as f32 / 3.0).max(TAIL_MIN);
    let (h, alive) = if ms < COMET_MS {
        (ease_in_out(ms as f64 / COMET_MS as f64) as f32 * l as f32, 1.0)
    } else {
        ((l - 1) as f32, 1.0 - (ms - COMET_MS) as f32 / DRAIN_MS as f32)
    };
    if x as f32 > h {
        return None;
    }
    let s = (1.0 - (h - x as f32) / tail).max(0.0) * alive;
    if s <= 0.0 {
        return None;
    }
    Some(if s > 0.5 {
        fade(theme, theme.accent, theme.accent2, (s - 0.5) * 2.0)
    } else {
        fade(theme, rest, theme.accent, s * 2.0)
    })
}

/// How far the band lifts cell `x` of a rule `n` cells long, 0..1, `ms` into its pass: a smooth hump that
/// enters at the left, leaves at the right and is gone at the start and at the end of a pass.
pub fn band(x: usize, n: usize, ms: u64) -> f32 {
    let half = (n as f32 * BAND_WIDTH / 2.0).max(3.0);
    let at = (ms % BAND_MS) as f32 / BAND_MS as f32;
    let centre = -half + at * (n as f32 + 2.0 * half);
    let d = (x as f32 - centre).abs();
    if d >= half {
        return 0.0;
    }
    (1.0 + (PI * d / half).cos()) / 2.0
}

/// The colour of letter `i` of `count` in the tag, `phase` (0..1) into a turn of the flow: `accent2` at the
/// left to `accent` at the right at phase 0, the same colours travelling through the letters as it grows.
pub fn tag_colour(theme: &Theme, i: usize, count: usize, phase: f32) -> Color {
    let s = if count > 1 { i as f32 / (count - 1) as f32 * 0.5 } else { 0.0 };
    let g = (1.0 - (TAU * (s - phase)).cos()) / 2.0;
    fade(theme, theme.accent2, theme.accent, g)
}

/// 1 when the tag lights up (`LAND_FROM`), easing to 0 by `IGNITE_MS`; 0 before and after.
pub fn landing(at: Option<Instant>, now: Instant) -> f32 {
    let Some(at) = at else { return 0.0 };
    let ms = now.saturating_duration_since(at).as_millis() as u64;
    if !(LAND_FROM..IGNITE_MS).contains(&ms) {
        return 0.0;
    }
    1.0 - ease_out((ms - LAND_FROM) as f64 / (IGNITE_MS - LAND_FROM) as f64) as f32
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
    /// The reasoning level is the hardest one the model has: the last of three or more. A model with a
    /// switch (`off` / `on`) has no top level.
    pub fn effort_top(&self) -> bool {
        self.effort_levels.len() >= 3
            && self.effort.as_deref().is_some_and(|e| self.effort_levels.last().map(String::as_str) == Some(e))
    }

    /// An `Effort` event came in: `was_top` and `same_list` are what was true before it. The ignite starts
    /// only when the level changed to the top one on the same list of levels (not at the start, not on a
    /// model with other levels, not on a repeat).
    pub(crate) fn note_effort_event(&mut self, was_top: bool, same_list: bool) {
        if same_list && !was_top && self.effort_top() && self.fx_on() {
            self.fx.effort_at = Some(self.now());
        }
    }

    /// Starts and ends the breath and the band: they run while a turn runs at the top level, and a stopped
    /// one finishes its cycle. Off the top level (or with the motion off) every clock of this module is
    /// cleared at once. The tick and the draw both call it; once is enough.
    pub(crate) fn note_effort(&mut self, now: Instant) {
        if !(self.fx_on() && self.effort_top()) {
            self.fx.effort_at = None;
            (self.fx.breath_from, self.fx.breath_stop) = (None, None);
            (self.fx.band_from, self.fx.band_stop) = (None, None);
            return;
        }
        let live = self.activity().is_some();
        (self.fx.breath_from, self.fx.breath_stop) =
            step(live, self.fx.breath_from, self.fx.breath_stop, BREATH_MS, now);
        (self.fx.band_from, self.fx.band_stop) = step(live, self.fx.band_from, self.fx.band_stop, BAND_MS, now);
    }

    /// Whether something of this module is drawn and moves: the ignite, the breath or the band (the frame
    /// loop keeps drawing while one does). The rule and its tag play even where the status chip is dropped.
    pub(crate) fn effort_moving(&self, now: Instant) -> bool {
        self.fx_on()
            && self.effort_top()
            && (within(self.fx.effort_at, now, IGNITE_MS)
                || cycling(self.fx.breath_from, self.fx.breath_stop, now, BREATH_MS)
                || cycling(self.fx.band_from, self.fx.band_stop, now, BAND_MS))
    }

    /// The colour of the `◆` of the effort chip, when it is not its resting colour `rest`.
    pub(crate) fn effort_mark(&self, rest: Color) -> Option<Color> {
        if !self.fx_on() || !self.effort_top() {
            return None;
        }
        let now = self.now();
        let k = landing(self.fx.effort_at, now).max(breath(self.fx.breath_from, self.fx.breath_stop, now) * DEPTH);
        if k <= 0.0 {
            return None;
        }
        let bright = fade(&self.theme, rest, self.theme.bar_fg, BRIGHT);
        let c = fade(&self.theme, rest, bright, k);
        (c != rest).then_some(c)
    }

    /// The effects on the rule row `area` (the `─` cells only; colour, and the head of the comet heavy).
    /// `edge` is how many cells of the row come before the tag at its right, if it has one: the comet stops
    /// there and the band never paints the tag. A change of mode wipes the rule at its own pace and wins.
    pub(crate) fn paint_crest(&self, buf: &mut Buffer, area: Rect, edge: Option<u16>) {
        let now = self.now();
        if !self.fx_on() || !self.effort_top() || within(self.fx.mode_at, now, crate::motion::MODE_MS) {
            return;
        }
        let n = area.width as usize;
        if n < 2 {
            return;
        }
        let l = edge.map_or(n, |e| (e as usize).min(n));
        let ms = |at: Option<Instant>| at.map_or(0, |t| now.saturating_duration_since(t).as_millis() as u64);
        let comet_on = within(self.fx.effort_at, now, IGNITE_MS);
        let from = self.fx.band_from;
        let band_on = cycling(from, self.fx.band_stop, now, BAND_MS);
        if !comet_on && !band_on {
            return;
        }
        let (since, into) = (ms(self.fx.effort_at), ms(from));
        // the head sits on a rule cell: where the label breaks the rule, on the nearest one behind
        let head = comet_head(l, since).filter(|_| comet_on).and_then(|h| {
            (0..=h).rev().find(|x| buf.cell((area.x + *x as u16, area.y)).is_some_and(|c| c.symbol() == "─"))
        });
        for x in 0..n {
            let Some(c) = buf.cell_mut((area.x + x as u16, area.y)) else { continue };
            if c.symbol() != "─" {
                continue;
            }
            if band_on {
                let k = band(x, n, into);
                if k > 0.0 {
                    c.fg = fade(&self.theme, c.fg, self.theme.accent, k * BAND_PEAK);
                }
            }
            if comet_on {
                if head == Some(x) {
                    c.fg = head_colour(&self.theme);
                    c.set_char('━');
                } else if let Some(col) = comet_tail(&self.theme, c.fg, x, l, since) {
                    c.fg = col;
                }
            }
        }
    }

    /// The tag on the rule (`label`, drawn in the style `rest`) as spans, when it holds the level and the
    /// level is the top one: the letters of `◆ level` in their gradient, which lights up when the level was
    /// just set and flows while the model works. None where the tag keeps today's look.
    pub(crate) fn effort_tag_spans(&self, label: &str, rest: Style) -> Option<Vec<Span<'static>>> {
        if !self.fx_on() || !self.effort_top() {
            return None;
        }
        let tag = format!("◆ {}", self.effort.as_deref()?);
        let after = label.strip_prefix(' ')?.strip_prefix(tag.as_str())?;
        let now = self.now();
        let count = tag.chars().filter(|c| *c != ' ').count();
        let flow = phase(self.fx.breath_from, self.fx.breath_stop, now, BREATH_MS);
        let lit = landing(self.fx.effort_at, now);
        let mut spans = vec![Span::styled(" ".to_string(), rest)];
        let mut i = 0;
        for ch in tag.chars() {
            if ch == ' ' {
                spans.push(Span::styled(" ".to_string(), rest));
                continue;
            }
            let mut c = tag_colour(&self.theme, i, count, flow);
            let mut style = rest;
            if lit > 0.0 {
                c = fade(&self.theme, c, self.theme.bar_fg, lit * TAG_BRIGHT);
                style = style.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(ch.to_string(), style.fg(c)));
            i += 1;
        }
        spans.push(Span::styled(after.to_string(), rest));
        Some(spans)
    }
}
