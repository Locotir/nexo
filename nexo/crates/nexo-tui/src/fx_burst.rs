//! A burst of dots at the end of a piece of work. A goal met: green stars burst from where the goal
//! shows (its state in the side panel, else its mark in the status bar, else the middle of the
//! transcript) with a white-hot flash and a ring, fly out under drag, arc down under gravity, leave
//! short trails, crackle into small second bursts and glitter as they fade, in `MET_MS`. A turn or a
//! goal stopped by a failure: a shorter red one, a row of dots that shudders and a few heavy embers
//! that hop up, fall and go out, in `FAIL_MS`.
//!
//! Each frame is a pure function of the time since the burst began (fixed seeds, no libm), drawn on
//! blank cells only, so the text is never touched and nothing is left once it is over.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::app::{App, StatusHit};
use crate::fx_stars::{cos, sin, unit, Canvas, FigCell, Grid, PI, TAU};
use crate::motion::{fade, within};
use crate::theme::Theme;

/// How long a burst for a goal met lasts ...
pub const MET_MS: u64 = 1600;
/// ... and the red one for a failure, shorter and heavier.
pub const FAIL_MS: u64 = 950;
/// The smallest room a burst is drawn in, in cells.
pub const MIN: (u16, u16) = (12, 4);
/// Blanks between two pieces of text on a row past which the space between them is free to the burst
/// (a sentence's gaps stay out of it; the room between the transcript and the panel does not).
const GAP: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A goal met: green.
    Met,
    /// A turn or a goal stopped by a failure: red.
    Failed,
}

impl Kind {
    pub fn ms(self) -> u64 {
        match self {
            Kind::Met => MET_MS,
            Kind::Failed => FAIL_MS,
        }
    }
}

/// A burst under way: what it is for and when it began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Burst {
    pub kind: Kind,
    pub at: Instant,
}

impl Burst {
    pub fn alive(&self, now: Instant) -> bool {
        within(Some(self.at), now, self.kind.ms())
    }
}

/// Whether a goal paused for `why` stopped on a failure (an error, a limit, the rounds used up, a round that
/// did nothing), not because the person stopped it (Esc, `/goal pause`) or the model chose a checkpoint.
pub fn failed(why: &str) -> bool {
    let w = why.trim().to_ascii_lowercase();
    !(w.is_empty() || w == "interrupted" || w.starts_with("paused by you") || w.starts_with("stopped at a checkpoint"))
}

/// Where a burst comes from, in dots of its room: `up` throws it upwards only, from under the room's
/// bottom edge (the status bar's goal mark is below the room).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Throw {
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub up: bool,
}

// ---- the scene ----

/// The burst `t_ms` after it began over a room of `cols` x `rows` cells; empty once it is over.
pub fn grid(th: &Throw, t_ms: f64, cols: u16, rows: u16, ascii: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 || !(0.0..th.kind.ms() as f64).contains(&t_ms) {
        return cv.finish();
    }
    let t = t_ms as f32;
    match th.kind {
        Kind::Met => met(&mut cv, th, t, ascii),
        Kind::Failed => fail(&mut cv, th, t),
    }
    cv.finish()
}

/// How far the strongest spark flies, in dots, for a room `w` x `h` dots.
fn reach(w: f32, h: f32) -> f32 {
    (w.min(h * 1.5) * 0.42).clamp(10.0, 60.0)
}

/// The heading of spark `i` of `n` (radians, y down): all round, or the upper half only.
fn heading(i: usize, n: usize, jitter: f32, up: bool) -> f32 {
    let f = (i as f32 + 0.5 + (jitter - 0.5) * 0.8) / n as f32;
    if up {
        PI + PI * (0.06 + 0.88 * f)
    } else {
        TAU * f + 0.3
    }
}

/// 0..1 of the way out: quick at first, slowed by the drag.
fn drag(u: f32) -> f32 {
    let v = 1.0 - u.clamp(0.0, 1.0);
    1.0 - v * v * v
}

fn met(cv: &mut Canvas, th: &Throw, t: f32, ascii: bool) {
    let (w, h) = cv.dots();
    let r = reach(w, h);
    let n = if w * h < 2400.0 { 28 } else { 56 };
    let g = r * 0.5;
    // the flash: a white-hot point and a ring that opens and thins
    if t < 220.0 {
        let k = 1.0 - t / 220.0;
        for (dx, dy) in [(0.0, 0.0), (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            cv.plot_kind(th.x + dx, th.y + dy, k, false, true);
        }
    }
    if t < 340.0 {
        let p = t / 340.0;
        let rr = r * 0.32 * drag(p);
        let dots = if ascii { 12 } else { 30 };
        for j in 0..dots {
            let a = TAU * j as f32 / dots as f32;
            cv.plot_kind(th.x + cos(a) * rr, th.y + sin(a) * rr, 0.85 * (1.0 - p), true, false);
        }
    }
    for i in 0..n {
        let s = 500 + i as u32 * 11;
        let a = heading(i, n, unit(s), th.up);
        let (ca, sa) = (cos(a), sin(a));
        let v = r * (0.38 + 0.62 * unit(s + 1));
        let life = MET_MS as f32 * (0.6 + 0.4 * unit(s + 2));
        let at = |tt: f32| {
            let u = tt / life;
            (th.x + ca * v * drag(u), th.y + sa * v * drag(u) + g * u * u)
        };
        let u = t / life;
        if u >= 1.0 {
            continue;
        }
        // one in fourteen crackles: at 42% of its life it pops into eight small sparks
        if i % 14 == 3 && u > 0.42 {
            let pop = 0.42 * life;
            let (px, py) = at(pop);
            let q = (t - pop) / (0.45 * life);
            if q < 1.0 {
                for k in 0..8 {
                    let b = TAU * k as f32 / 8.0 + unit(s + 3) * 0.6;
                    let d = r * 0.18 * drag(q);
                    let l = (1.0 - q) * (1.0 - q).sqrt();
                    cv.plot_kind(px + cos(b) * d, py + sin(b) * d + g * 0.15 * q * q, l, q < 0.25, false);
                }
            }
            continue;
        }
        let (x, y) = at(t);
        let mut level = (1.0 - u) * (1.0 - u).sqrt().max(0.2);
        let mut hot = u < 0.16;
        // after its middle a spark glitters: off and on in 50 ms steps, and one in four flares
        if u > 0.45 {
            let step = (t / 50.0) as u32;
            let roll = unit(s + 7 + step * 13);
            if roll < 0.3 {
                level *= 0.2;
            } else if i % 4 == 0 && roll > 0.82 {
                level = level.max(0.9);
                hot = true;
                if !ascii {
                    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                        cv.plot_kind(x + dx, y + dy, level * 0.6, true, false);
                    }
                }
            }
        }
        cv.plot_kind(x, y, level, hot, false);
        // the trail: where it was a moment ago, fainter
        if u < 0.7 && !ascii {
            for (lag, f) in [(30.0, 0.5), (60.0, 0.3), (95.0, 0.15)] {
                if t > lag {
                    let (tx, ty) = at(t - lag);
                    cv.plot_kind(tx, ty, level * f, false, false);
                }
            }
        }
    }
}

fn fail(cv: &mut Canvas, th: &Throw, t: f32) {
    let (w, h) = cv.dots();
    let r = reach(w, h) * 0.55;
    // the shudder: a short row of dots shaking left and right, settling
    let base_y = if th.up { h - 1.0 } else { th.y };
    if t < 300.0 {
        let p = t / 300.0;
        let amp = 2.0 * (1.0 - p);
        let side = if ((t / 40.0) as u32).is_multiple_of(2) { 1.0 } else { -1.0 };
        for j in -6..=6 {
            let x = th.x + j as f32 + amp * side;
            cv.plot_kind(x, base_y, 0.85 * (1.0 - p), p < 0.3, false);
        }
    }
    // the embers: a few heavy dots that hop up, fall and flicker out
    let n = 22;
    let g = r * 1.1;
    for i in 0..n {
        let s = 900 + i as u32 * 13;
        let a = if th.up { PI + PI * (0.12 + 0.76 * unit(s)) } else { heading(i, n, unit(s), false) };
        let (ca, sa) = (cos(a), sin(a));
        let v = r * (0.25 + 0.75 * unit(s + 1));
        let life = FAIL_MS as f32 * (0.55 + 0.45 * unit(s + 2));
        let delay = 60.0 * unit(s + 3);
        let at = |tt: f32| {
            let u = (tt - delay) / life;
            let out = 1.0 - (1.0 - u) * (1.0 - u);
            (th.x + ca * v * out, base_y + sa * v * out + g * u * u)
        };
        let u = (t - delay) / life;
        if !(0.0..1.0).contains(&u) {
            continue;
        }
        let flicker = if unit(s + 5 + (t / 70.0) as u32 * 7) < 0.25 { 0.45 } else { 1.0 };
        let level = 0.9 * (1.0 - u) * flicker;
        let (x, y) = at(t);
        cv.plot_kind(x, y, level, u < 0.25, false);
        if t - 45.0 > delay {
            let (tx, ty) = at(t - 45.0);
            cv.plot_kind(tx, ty, level * 0.35, false, false);
        }
    }
}

// ---- the app ----

impl App {
    /// Starts a burst, unless the switch or the motion is off; the same kind already under way goes on.
    pub fn burst(&mut self, kind: Kind) {
        if !self.opts.bursts || !self.fx_on() {
            return;
        }
        let now = self.now();
        if self.fx.burst.is_some_and(|b| b.kind == kind && b.alive(now)) {
            return;
        }
        self.fx.burst = Some(Burst { kind, at: now });
        self.dirty = true;
    }

    /// For the tick: a burst moves at frame rate.
    pub fn burst_moving(&self, now: Instant) -> bool {
        self.opts.bursts && self.fx_on() && self.fx.burst.is_some_and(|b| b.alive(now))
    }
}

/// Where a burst starts on screen: the goal's state in the side panel, else its mark in the status bar (the
/// burst rises from under `room`), else the middle of `room` (a failure: from its bottom edge).
pub(crate) fn spot(app: &App, room: Rect, kind: Kind) -> (u16, u16, bool) {
    let inside = |x: u16, y: u16| x >= room.left() && x < room.right() && y >= room.top() && y < room.bottom();
    // a failure starts at the goal only while the goal is the one stopped, not beside a goal already met
    let goals = kind == Kind::Met || app.goal.as_ref().is_some_and(|g| g.state != nexo_core::agent::GoalState::Done);
    if let Some((x, y)) = app.fx.goal_spot.get().filter(|&(x, y)| goals && inside(x, y)) {
        return (x, y, false);
    }
    let mark = app.status_hits.iter().find(|(_, h)| goals && *h == StatusHit::Goal).map(|(r, _)| r.x + r.width / 2);
    let mid = room.x + room.width / 2;
    match (mark, kind) {
        (Some(x), _) => (x.clamp(room.left(), room.right().saturating_sub(1)), room.bottom(), true),
        (None, Kind::Met) => (mid, room.y + room.height / 2, false),
        (None, Kind::Failed) => (mid, room.bottom(), true),
    }
}

/// Draws the burst under way over the blank cells of `room` (the transcript and the side panel).
pub fn draw(buf: &mut Buffer, app: &App, room: Rect) {
    let Some(b) = app.fx.burst else { return };
    let now = app.now();
    let room = room.intersection(buf.area);
    if !app.opts.bursts || !app.fx_on() || !b.alive(now) || room.width < MIN.0 || room.height < MIN.1 {
        return;
    }
    let ascii = crate::ascii::wanted();
    let (dx, dy) = if ascii { (1.0, 2.0) } else { (2.0, 4.0) };
    let (x, y, up) = spot(app, room, b.kind);
    let th = Throw {
        kind: b.kind,
        x: (x - room.x) as f32 * dx + dx / 2.0,
        y: if up { room.height as f32 * dy + 1.0 } else { (y - room.y) as f32 * dy + dy / 2.0 },
        up,
    };
    let t = now.saturating_duration_since(b.at).as_secs_f64() * 1000.0;
    let g = grid(&th, t, room.width, room.height, ascii);
    // the transcript and the side panel share the room: their text is apart, the space between is free
    let free = crate::fx_home::clear_of_text(buf, room, &app.theme, 0, 0, GAP);
    let theme = &app.theme;
    crate::fx_home::paint(buf, room, &g, &free, ascii, |c| look(theme, b.kind, c));
}

/// The colour of a cell of a burst: green or red by its brightness, white-hot (or ember orange) where it
/// is hot; on a mono theme bold and dim instead.
pub fn look(theme: &Theme, kind: Kind, c: &FigCell) -> Style {
    if theme.mono {
        return Style::default().add_modifier(if c.core || c.node || c.level > 0.66 {
            Modifier::BOLD
        } else if c.level < 0.35 {
            Modifier::DIM
        } else {
            Modifier::empty()
        });
    }
    let l = c.level.clamp(0.0, 1.0);
    match kind {
        Kind::Met => {
            let base = fade(theme, theme.bg, theme.success, 0.2 + 0.8 * l);
            let fg = if c.core {
                theme.fg
            } else if c.node {
                fade(theme, base, theme.fg, 0.55 * l)
            } else {
                base
            };
            let style = Style::default().fg(fg);
            if l > 0.8 {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }
        }
        Kind::Failed => {
            let base = fade(theme, theme.bg, theme.error, 0.25 + 0.7 * l);
            let fg = if c.node { fade(theme, base, theme.warn, 0.45 * l) } else { base };
            Style::default().fg(fg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(g: &Grid) -> usize {
        g.cells.iter().filter(|c| c.ch != ' ').count()
    }

    fn mid(kind: Kind) -> Throw {
        Throw { kind, x: 80.0, y: 48.0, up: false }
    }

    #[test]
    fn a_burst_is_the_same_for_the_same_time() {
        let a = grid(&mid(Kind::Met), 400.0, 80, 24, false);
        let b = grid(&mid(Kind::Met), 400.0, 80, 24, false);
        assert_eq!(a.cells, b.cells);
    }

    #[test]
    fn a_burst_spreads_then_ends_leaving_nothing() {
        for kind in [Kind::Met, Kind::Failed] {
            let early = grid(&mid(kind), 30.0, 80, 24, false);
            let later = grid(&mid(kind), 350.0, 80, 24, false);
            assert!(lit(&early) > 0 && lit(&later) > 0, "{kind:?}");
            assert_eq!(lit(&grid(&mid(kind), kind.ms() as f64, 80, 24, false)), 0, "{kind:?} over");
            assert_eq!(lit(&grid(&mid(kind), kind.ms() as f64 + 500.0, 80, 24, false)), 0);
        }
        // the green one opens out: its dots are spread wider at 400 ms than at 40 ms
        let span = |g: &Grid| {
            let xs: Vec<usize> = (0..g.cells.len()).filter(|&i| g.cells[i].ch != ' ').map(|i| i % 80).collect();
            xs.iter().max().unwrap_or(&0) - xs.iter().min().unwrap_or(&0)
        };
        assert!(
            span(&grid(&mid(Kind::Met), 400.0, 80, 24, false)) > span(&grid(&mid(Kind::Met), 40.0, 80, 24, false)) + 10
        );
    }

    #[test]
    fn the_red_one_is_shorter_and_smaller() {
        const { assert!(FAIL_MS < MET_MS) };
        let green = lit(&grid(&mid(Kind::Met), 300.0, 80, 24, false));
        let red = lit(&grid(&mid(Kind::Failed), 300.0, 80, 24, false));
        assert!(red < green, "{red} < {green}");
    }

    #[test]
    fn a_burst_from_below_rises_into_the_room() {
        let th = Throw { kind: Kind::Met, x: 80.0, y: 24.0 * 4.0 + 1.0, up: true };
        let g = grid(&th, 300.0, 80, 24, false);
        let rows: Vec<usize> = (0..g.cells.len()).filter(|&i| g.cells[i].ch != ' ').map(|i| i / 80).collect();
        assert!(!rows.is_empty());
        assert!(*rows.iter().min().unwrap() < 20, "it reaches a few rows up");
    }

    #[test]
    fn only_a_failure_turns_it_red() {
        for why in [
            "the turn failed",
            "the tool-round limit (agent.max_turns)",
            "the last round did nothing new",
            "8 rounds without GOAL DONE",
            "stopped after 3 checkpoints",
        ] {
            assert!(failed(why), "{why}");
        }
        for why in ["interrupted", "paused by you", "stopped at a checkpoint: tests pass", ""] {
            assert!(!failed(why), "{why}");
        }
    }

    #[test]
    fn the_colours_follow_the_theme_and_mono_uses_weight() {
        let t = Theme::dark();
        let hot = FigCell { ch: '⠁', level: 1.0, node: false, core: false };
        assert_eq!(look(&t, Kind::Met, &hot).fg, Some(t.success));
        assert_eq!(look(&t, Kind::Failed, &FigCell { level: 1.0, ..hot }).fg, Some(fade(&t, t.bg, t.error, 0.95)));
        let m = Theme::mono();
        assert!(look(&m, Kind::Met, &hot).add_modifier.contains(Modifier::BOLD));
        let faint = FigCell { level: 0.2, ..hot };
        assert!(look(&m, Kind::Failed, &faint).add_modifier.contains(Modifier::DIM));
        assert_eq!(look(&m, Kind::Met, &faint).fg, None);
    }
}
