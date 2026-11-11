//! The top reasoning level in motion, in pieces already on screen: a crest runs once along the rule
//! over the draft when the level is set, the `◆` of the status bar pops with it, and the `◆` breathes
//! while the model works. Nothing new is drawn; at rest everything looks as it always did. Pure
//! functions of the time they are asked about; the caller injects `now`.

use std::f32::consts::TAU;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::anim::{ease_out, progress};
use crate::app::App;
use crate::motion::{fade, within};
use crate::theme::Theme;

/// The crest runs along the rule, and the mark pops, in this long.
pub const IGNITE_MS: u64 = 900;
/// One breath of the mark while the model works.
pub const BREATH_MS: u64 = 2400;
/// Cells the crest fades over behind its head.
const TAIL: f32 = 14.0;
/// How far toward the bright colour the mark goes at the top of a breath (0..1).
const DEPTH: f32 = 0.8;
/// How far toward the bar's text colour the brightest mark is.
const BRIGHT: f32 = 1.0;

/// The colour of cell `x` of a rule `n` cells long, whose own colour there is `rest`, while a crest runs
/// along it; `p` is 0..1 of the run. The head is `accent2`, the tail turns to `accent` and then to `rest`.
pub fn crest(theme: &Theme, rest: Color, x: usize, n: usize, p: f32) -> Color {
    let head = -2.0 + p.clamp(0.0, 1.0) * (n as f32 + TAIL + 2.0);
    let d = head - x as f32;
    let s = if d < 0.0 { (1.0 + d / 2.0).max(0.0) } else { (1.0 - d / TAIL).max(0.0) };
    if s <= 0.0 {
        return rest;
    }
    fade(theme, rest, fade(theme, theme.accent, theme.accent2, s), s)
}

/// 1 when the level was just set, easing to 0 by `IGNITE_MS`.
pub fn pop(at: Option<Instant>, now: Instant) -> f32 {
    if !within(at, now, IGNITE_MS) {
        return 0.0;
    }
    1.0 - ease_out(progress(at, now, IGNITE_MS)) as f32
}

/// How long the breath that began at `from` runs when the work stopped at `stop`: to the end of the cycle
/// it was in, so the mark always comes back to its resting colour.
fn breath_len(from: Instant, stop: Instant) -> u64 {
    let ran = stop.saturating_duration_since(from).as_millis() as u64;
    ran.div_ceil(BREATH_MS) * BREATH_MS
}

/// Whether the breath that began at `from` (and ended its work at `stop`, if it has) still moves.
pub fn breathing(from: Option<Instant>, stop: Option<Instant>, now: Instant) -> bool {
    let Some(from) = from else { return false };
    match stop {
        None => true,
        Some(stop) => (now.saturating_duration_since(from).as_millis() as u64) < breath_len(from, stop),
    }
}

/// 0..1..0 once every `BREATH_MS` from rest, a smooth curve; 0 when it does not move.
pub fn breath(from: Option<Instant>, stop: Option<Instant>, now: Instant) -> f32 {
    let Some(at) = from.filter(|_| breathing(from, stop, now)) else { return 0.0 };
    let ms = now.saturating_duration_since(at).as_millis() as u64 % BREATH_MS;
    (1.0 - (ms as f32 / BREATH_MS as f32 * TAU).cos()) / 2.0
}

impl App {
    /// The reasoning level is the hardest one the model has: the last of three or more. A model with a
    /// switch (`off` / `on`) has no top level.
    pub fn effort_top(&self) -> bool {
        self.effort_levels.len() >= 3
            && self.effort.as_deref().is_some_and(|e| self.effort_levels.last().map(String::as_str) == Some(e))
    }

    /// An `Effort` event came in: `was_top` and `same_list` are what was true before it. The crest and the
    /// pop start only when the level changed to the top one on the same list of levels (not at the start,
    /// not on a model with other levels, not on a repeat).
    pub(crate) fn note_effort_event(&mut self, was_top: bool, same_list: bool) {
        if same_list && !was_top && self.effort_top() && self.fx_on() {
            self.fx.effort_at = Some(self.now());
        }
    }

    /// Starts and ends the breath: it runs while a turn runs at the top level, and a stopped one finishes
    /// its cycle. The tick and the draw both call it; once is enough.
    pub(crate) fn note_effort(&mut self, now: Instant) {
        let live = self.fx_on() && self.effort_top() && self.activity().is_some();
        let (from, stop) = (self.fx.breath_from, self.fx.breath_stop);
        match (live, from) {
            (true, None) => self.fx.breath_from = Some(now),
            // work came back while the last breath was finishing: it goes on from where it is
            (true, Some(_)) if stop.is_some() => {
                if breathing(from, stop, now) {
                    self.fx.breath_stop = None;
                } else {
                    self.fx.breath_from = Some(now);
                    self.fx.breath_stop = None;
                }
            }
            (false, Some(_)) if stop.is_none() => self.fx.breath_stop = Some(now),
            (false, Some(_)) if !breathing(from, stop, now) => {
                self.fx.breath_from = None;
                self.fx.breath_stop = None;
            }
            _ => {}
        }
    }

    /// Whether the crest or the breath still moves (the frame loop keeps drawing while it does).
    pub(crate) fn effort_moving(&self, now: Instant) -> bool {
        self.fx_on()
            && (within(self.fx.effort_at, now, IGNITE_MS) || breathing(self.fx.breath_from, self.fx.breath_stop, now))
    }

    /// The colour of the `◆` of the effort chip, when it is not its resting colour `rest`.
    pub(crate) fn effort_mark(&self, rest: Color) -> Option<Color> {
        if !self.fx_on() || !self.effort_top() {
            return None;
        }
        let now = self.now();
        let k = pop(self.fx.effort_at, now).max(breath(self.fx.breath_from, self.fx.breath_stop, now) * DEPTH);
        if k <= 0.0 {
            return None;
        }
        let bright = fade(&self.theme, rest, self.theme.bar_fg, BRIGHT);
        let c = fade(&self.theme, rest, bright, k);
        (c != rest).then_some(c)
    }

    /// The crest on the rule row `area` (the `─` cells only; colour only). A change of mode wipes the
    /// rule at its own pace and wins.
    pub(crate) fn paint_crest(&self, buf: &mut Buffer, area: Rect) {
        let now = self.now();
        if !self.fx_on()
            || !self.effort_top()
            || !within(self.fx.effort_at, now, IGNITE_MS)
            || within(self.fx.mode_at, now, crate::motion::MODE_MS)
        {
            return;
        }
        let p = progress(self.fx.effort_at, now, IGNITE_MS) as f32;
        let n = area.width as usize;
        for x in 0..n {
            if let Some(c) = buf.cell_mut((area.x + x as u16, area.y)) {
                if c.symbol() == "─" {
                    c.fg = crest(&self.theme, c.fg, x, n, p);
                }
            }
        }
    }
}
