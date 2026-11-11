//! Faint stars drifting across an empty session: while the home is open and nothing has been sent yet,
//! a slow field of braille dots crosses the transcript in three depths, the far ones dim and slow, the
//! near ones brighter and faster. Now and then one twinkles into a small cross, and once in a while a
//! shooting star runs down the sky. The field keeps clear of text: it draws on blank cells only, and
//! not on the ones beside a line of text or above and below it.
//!
//! Each frame is a pure function of the drift's phase, like the welcome figure (`fx_stars`): fixed
//! seeds, no randomness and no libm. The field rests like that figure too: after `REST_AFTER_MS` with no
//! key or mouse event it eases to a stop and asks for no more frames, and the next input wakes it. It
//! fades in when the empty home shows and out in `OUT_MS` once a message is sent or a session with
//! messages is loaded.

use std::cell::Cell;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::anim::progress;
use crate::app::App;
use crate::fx_stars::{cos, sin, unit, Canvas, FigCell, Grid, StarsFx, PI};
use crate::motion::{fade, within};
use crate::theme::Theme;

/// The stars come in over this long when the empty home shows ...
pub const IN_MS: u64 = 900;
/// ... and go out over this long once it is not empty any more.
pub const OUT_MS: u64 = 300;
/// The smallest room the field is drawn in, in cells.
pub const MIN: (u16, u16) = (24, 6);
/// A shooting star may cross once in each slot of this long ...
pub const SLOT_MS: f64 = 14_000.0;
/// ... and takes this long to.
pub const SHOOT_MS: f64 = 1300.0;
/// Columns kept clear on each side of a line of text, and rows above and below it.
const MARGIN_X: u16 = 2;
const MARGIN_Y: u16 = 1;
/// About one star in this many cells.
const CELLS_PER_STAR: usize = 24;
const MAX_STARS: usize = 300;
/// The depths, far to near: share of the stars, speed (dots a second), brightness from and to.
const LAYERS: [(f32, f32, f32, f32); 3] =
    [(0.62, 0.30, 0.20, 0.36), (0.27, 0.80, 0.42, 0.62), (0.11, 1.70, 0.74, 0.96)];
/// The drift heads left and this much down for each dot it goes left.
const SLOPE: f32 = 0.16;
/// The dots near an edge fade, so a star that wraps does not pop in.
const EDGE: f32 = 5.0;

// ---- the field ----

/// The field at `phase_ms` of its drift over `cols` x `rows` cells. `shown` (0..1) fades it in and out,
/// the stars coming and going in a scattered order; `shoot` (0..1) is how much of a shooting star shows
/// (none while the drift comes to rest).
pub fn field(phase_ms: f64, shown: f32, shoot: f32, cols: u16, rows: u16, ascii: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 || shown <= 0.0 {
        return cv.finish();
    }
    let (w, h) = cv.dots();
    let secs = phase_ms / 1000.0;
    let n = (cols as usize * rows as usize / CELLS_PER_STAR).clamp(4, MAX_STARS);
    for i in 0..n {
        let s = 40_000 + i as u32 * 8;
        let (layer, (_, speed, lo, hi)) = layer_of(unit(s));
        let went = (speed * (0.8 + 0.4 * unit(s + 1))) as f64 * secs;
        let x = (unit(s + 2) as f64 * w as f64 - went).rem_euclid(w as f64) as f32;
        let y = (unit(s + 3) as f64 * h as f64 + went * SLOPE as f64).rem_euclid(h as f64) as f32;
        // the stars come in (and go) one by one over the fade
        let late = unit(s + 4) * 0.5;
        let k = ((shown - late) / 0.5).clamp(0.0, 1.0);
        let edge = (x.min(w - 1.0 - x).min(y).min(h - 1.0 - y) / EDGE).clamp(0.0, 1.0);
        let base = lo + (hi - lo) * unit(s + 5);
        // one in four twinkles (one in eight of the far ones), each on its own period
        let tw = if unit(s + 6) < if layer == 0 { 0.12 } else { 0.25 } {
            twinkle(phase_ms, 3200.0 + 4200.0 * unit(s + 7) as f64, unit(s + 8))
        } else {
            0.0
        };
        let level = (base * (0.8 + 0.2 * tw) + 0.32 * tw).min(1.0) * k * edge;
        let near = layer == 2;
        cv.plot_kind(x, y, level, near, false);
        // a third of the near stars are two dots wide
        if near && unit(s + 9) < 0.34 && !ascii {
            cv.plot_kind(x + 1.0, y, level * 0.8, true, false);
        }
        // the peak of a twinkle: a small cross
        if tw > 0.6 && layer > 0 && !ascii {
            let arm = level * (tw - 0.6) / 0.4 * 0.7;
            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                cv.plot_kind(x + dx, y + dy, arm, near, false);
            }
        }
    }
    if shoot > 0.0 {
        if let Some(shot) = shot_at(phase_ms, w, h) {
            shooting(&mut cv, &shot, shoot * shown, ascii);
        }
    }
    cv.finish()
}

/// The depth a star falls in, from `pick` (0..1), with that depth's numbers.
fn layer_of(pick: f32) -> (usize, (f32, f32, f32, f32)) {
    let mut acc = 0.0;
    for (i, l) in LAYERS.iter().enumerate() {
        acc += l.0;
        if pick < acc {
            return (i, *l);
        }
    }
    (LAYERS.len() - 1, LAYERS[LAYERS.len() - 1])
}

/// 0 most of the time; a soft rise and fall (0..1..0) over the first sixth of each `period_ms`.
fn twinkle(phase_ms: f64, period_ms: f64, offset: f32) -> f32 {
    let k = ((phase_ms / period_ms) as f32 + offset).fract();
    if k < 1.0 / 6.0 {
        let s = sin(k * 6.0 * PI);
        s * s
    } else {
        0.0
    }
}

/// A shooting star in flight: how far along (0..1), where it set out, its heading and how far it goes (dots).
struct Shot {
    u: f32,
    from: (f32, f32),
    dir: (f32, f32),
    len: f32,
}

/// The shooting star of the slot `phase_ms` falls in, while it crosses; most slots have none.
fn shot_at(phase_ms: f64, w: f32, h: f32) -> Option<Shot> {
    if w < 60.0 || h < 16.0 || phase_ms < 0.0 {
        return None;
    }
    let slot = (phase_ms / SLOT_MS).floor();
    let s = 70_000 + slot as u32 * 6;
    if unit(s) > 0.45 {
        return None;
    }
    let start = slot * SLOT_MS + 1500.0 + unit(s + 1) as f64 * (SLOT_MS - SHOOT_MS - 1500.0);
    let u = ((phase_ms - start) / SHOOT_MS) as f32;
    if !(0.0..1.0).contains(&u) {
        return None;
    }
    // left and down, between 17 and 31 degrees under the horizon
    let a = 0.30 + 0.24 * unit(s + 2);
    Some(Shot {
        u,
        from: (w * (0.40 + 0.55 * unit(s + 3)), h * (0.05 + 0.35 * unit(s + 4))),
        dir: (-cos(a), sin(a)),
        len: (w * 0.42).min(110.0),
    })
}

/// A shooting star: a bright head with a tail that thins behind it; it flares in and goes out at the end.
fn shooting(cv: &mut Canvas, shot: &Shot, k: f32, ascii: bool) {
    let u = shot.u;
    // a little slower at the end
    let gone = shot.len * u * (1.2 - 0.2 * u);
    let glow = (u / 0.12).min(1.0) * (1.0 - u * u * u) * k;
    let tail = if ascii { 6 } else { 18 };
    for j in 0..=tail {
        let back = j as f32 * if ascii { 1.5 } else { 1.0 };
        if back > gone {
            break;
        }
        let d = gone - back;
        let f = 1.0 - j as f32 / (tail + 1) as f32;
        cv.plot_kind(shot.from.0 + shot.dir.0 * d, shot.from.1 + shot.dir.1 * d, glow * f * f, false, j < 3);
    }
}

/// The room of the field in `area`, if it is big enough.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN.0 && area.height >= MIN.1
}

// ---- time ----

/// What the field remembers: when the empty home showed and when it stopped being empty, and the drift's
/// clock (it runs and rests like the welcome figure's turn).
#[derive(Debug, Default, Clone)]
pub struct HomeFx {
    since: Option<Instant>,
    gone: Option<Instant>,
    clock: StarsFx,
    /// The last draw had room for the field (the beat is only worth asking for then).
    shown: Cell<bool>,
}

impl HomeFx {
    /// 0..1: how much of the field shows at `now` (fading in, all of it, fading out, none).
    pub fn level(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return 0.0 };
        match self.gone {
            None => progress(Some(since), now, IN_MS) as f32,
            Some(gone) => {
                let was = progress(Some(since), gone, IN_MS) as f32;
                was * (1.0 - progress(Some(gone), now, OUT_MS) as f32)
            }
        }
    }

    /// The drift's phase at `now`, in ms.
    pub fn phase(&self, now: Instant) -> f64 {
        self.clock.phase(now)
    }

    /// 0..1: how fast the drift goes at `now` (1 while it runs, easing to 0 as it comes to rest).
    pub fn pace(&self, now: Instant) -> f32 {
        let dt = 40.0;
        ((self.clock.phase(now + Duration::from_millis(dt as u64)) - self.clock.phase(now)) / dt).clamp(0.0, 1.0) as f32
    }

    /// The drift has come to rest: no more frames until a key or the mouse wakes it.
    pub fn at_rest(&self, now: Instant) -> bool {
        self.clock.at_rest(now)
    }

    /// The field is drifting on screen (the beat is wanted).
    fn drifting(&self, now: Instant) -> bool {
        self.shown.get() && self.since.is_some() && self.gone.is_none() && !self.at_rest(now)
    }

    /// A fade or a shooting star is under way (frames are wanted).
    fn moving(&self, now: Instant) -> bool {
        if !self.shown.get() {
            return false;
        }
        let fading = within(self.since, now, IN_MS) || within(self.gone, now, OUT_MS);
        // the whole room is not known here: any room the field draws a shooting star in will do
        let shooting = self.gone.is_none() && self.pace(now) > 0.0 && shot_at(self.phase(now), 1e4, 1e4).is_some();
        fading || shooting
    }
}

impl App {
    /// The empty home the field is for: a model works, nothing has been said or sent, and the switch is on.
    fn home_empty(&self) -> bool {
        self.opts.home_stars && self.fx_on() && !self.locked && !self.busy && self.fresh()
    }

    /// Notices the home becoming empty (the stars come in) or not empty any more (they go out).
    pub(crate) fn note_home(&mut self, now: Instant) {
        let empty = self.home_empty();
        let h = &mut self.fx.home;
        if empty && (h.since.is_none() || h.gone.is_some()) {
            let mut clock = StarsFx::default();
            clock.begin(now);
            *h = HomeFx { since: Some(now), gone: None, clock, shown: Cell::new(h.shown.get()) };
        } else if !empty && h.since.is_some() && h.gone.is_none() {
            h.gone = Some(now);
        }
    }

    /// A key or a mouse event wakes the drift.
    pub(crate) fn home_input(&mut self, now: Instant) {
        self.fx.home.clock.input(now);
    }

    /// The field drifts on the beat (a frame per tick, not 60 a second).
    pub fn home_alive(&self, now: Instant) -> bool {
        self.opts.home_stars && self.fx_on() && self.fx.home.drifting(now)
    }

    /// For the tick: the fades and a shooting star move at frame rate.
    pub fn home_moving(&self, now: Instant) -> bool {
        self.opts.home_stars && self.fx_on() && self.fx.home.moving(now)
    }
}

// ---- drawing ----

/// Draws the field into `area` (the transcript, its scrollbar column left out) on the blank cells
/// away from text. Nothing with the motion off or the switch off, or in a room under `MIN`.
pub fn draw(buf: &mut Buffer, app: &App, area: Rect) {
    let now = app.now();
    let hf = &app.fx.home;
    let area = area.intersection(buf.area);
    let on = app.opts.home_stars && app.fx_on() && fits(area);
    let shown = if on { hf.level(now) } else { 0.0 };
    hf.shown.set(on && hf.since.is_some());
    if shown <= 0.0 {
        return;
    }
    let ascii = crate::ascii::wanted();
    let grid = field(hf.phase(now), shown, hf.pace(now), area.width, area.height, ascii);
    let free = clear_of_text(buf, area, &app.theme, MARGIN_X, MARGIN_Y, u16::MAX);
    let theme = &app.theme;
    paint(buf, area, &grid, &free, ascii, |c| star_style(theme, c));
}

/// The look of a cell of the field: the far stars grey, the middle ones the accent, the near ones the
/// text's colour, the shooting star's head brightest; on a mono theme bold and dim instead.
fn star_style(theme: &Theme, c: &FigCell) -> Style {
    if theme.mono {
        return Style::default().add_modifier(if c.core || c.level > 0.66 {
            Modifier::BOLD
        } else if c.level < 0.35 {
            Modifier::DIM
        } else {
            Modifier::empty()
        });
    }
    let to = if c.core || c.node {
        theme.fg
    } else if c.level < 0.4 {
        theme.dim
    } else {
        theme.accent
    };
    let style = Style::default().fg(fade(theme, theme.bg, to, 0.12 + 0.88 * c.level));
    if c.core && c.level > 0.8 {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// Which cells of `area` an effect may use, row by row: blank ones on the ground's colour, at least `mx`
/// columns from the text of their row and of the `my` rows above and below. The cells between two words
/// count as text; a gap wider than `gap` blanks (a column of text beside another) does not.
pub(crate) fn clear_of_text(buf: &Buffer, area: Rect, theme: &Theme, mx: u16, my: u16, gap: u16) -> Vec<bool> {
    let ground = theme.base().bg.unwrap_or(Color::Reset);
    let (w, h) = (area.width as usize, area.height as usize);
    let blank = |x: u16, y: u16| {
        let c = &buf[(x, y)];
        c.symbol() == " " && c.bg == ground
    };
    // the runs of text on each row (split at a wide gap), each widened by the margin
    let spans: Vec<Vec<(i32, i32)>> = (0..area.height)
        .map(|dy| {
            let y = area.y + dy;
            let mut runs: Vec<(i32, i32)> = Vec::new();
            for x in (area.left()..area.right()).filter(|&x| buf[(x, y)].symbol() != " ") {
                let x = x as i32;
                match runs.last_mut() {
                    Some((_, r)) if x - *r <= gap as i32 + 1 => *r = x,
                    _ => runs.push((x, x)),
                }
            }
            runs.into_iter().map(|(l, r)| (l - mx as i32, r + mx as i32)).collect()
        })
        .collect();
    let mut free = vec![false; w * h];
    for row in 0..h {
        let near: Vec<(i32, i32)> = (row.saturating_sub(my as usize)..(row + my as usize + 1).min(h))
            .flat_map(|r| spans[r].iter().copied())
            .collect();
        for col in 0..w {
            let x = area.x + col as u16;
            free[row * w + col] =
                blank(x, area.y + row as u16) && !near.iter().any(|&(l, r)| (l..=r).contains(&(x as i32)));
        }
    }
    free
}

/// Sets the dots of `grid` (laid over `area` from its top left) into the free cells, in the style `look` gives.
/// ASCII mode draws `.`, `+` and `*`.
pub(crate) fn paint(
    buf: &mut Buffer,
    area: Rect,
    grid: &Grid,
    free: &[bool],
    ascii: bool,
    look: impl Fn(&FigCell) -> Style,
) {
    let cols = grid.cols as usize;
    for (i, c) in grid.cells.iter().enumerate() {
        if c.ch == ' ' || c.level <= 0.02 || !free.get(i).copied().unwrap_or(false) {
            continue;
        }
        let (x, y) = (area.x + (i % cols) as u16, area.y + (i / cols) as u16);
        let ch = if ascii && c.ch == ':' { '+' } else { c.ch };
        let cell = &mut buf[(x, y)];
        cell.set_char(ch);
        cell.set_style(look(c));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dots(g: &Grid) -> Vec<(usize, char)> {
        g.cells.iter().enumerate().filter(|(_, c)| c.ch != ' ').map(|(i, c)| (i, c.ch)).collect()
    }

    #[test]
    fn the_field_is_the_same_for_the_same_phase() {
        let a = field(4321.0, 1.0, 1.0, 80, 24, false);
        let b = field(4321.0, 1.0, 1.0, 80, 24, false);
        assert_eq!(dots(&a), dots(&b));
        assert!(dots(&a).len() > 30, "a room this big holds a few dozen stars");
    }

    #[test]
    fn the_field_drifts_and_wraps_round() {
        let (cols, rows) = (60u16, 20u16);
        let a = field(0.0, 1.0, 0.0, cols, rows, false);
        let b = field(6000.0, 1.0, 0.0, cols, rows, false);
        assert_ne!(dots(&a), dots(&b), "six seconds later the stars have moved");
        // a near star goes about 1.7 dots a second: in a long while it has gone round the room
        // more than once, and it is still inside the room
        let far = field(3_600_000.0, 1.0, 0.0, cols, rows, false);
        assert_eq!(far.cells.len(), cols as usize * rows as usize);
        assert!(dots(&far).len() > 10, "the stars wrap round instead of leaving");
    }

    #[test]
    fn the_far_stars_are_dimmer_and_slower_than_the_near_ones() {
        assert!(LAYERS[0].1 < LAYERS[1].1 && LAYERS[1].1 < LAYERS[2].1);
        assert!(LAYERS[0].3 < LAYERS[2].2);
        let shares: f32 = LAYERS.iter().map(|l| l.0).sum();
        assert!((shares - 1.0).abs() < 1e-4);
    }

    #[test]
    fn nothing_shows_before_the_fade_in_and_less_part_way() {
        assert!(dots(&field(1000.0, 0.0, 1.0, 80, 24, false)).is_empty());
        let half = dots(&field(1000.0, 0.4, 1.0, 80, 24, false)).len();
        let all = dots(&field(1000.0, 1.0, 1.0, 80, 24, false)).len();
        assert!(half < all, "{half} < {all}");
    }

    #[test]
    fn a_shooting_star_crosses_now_and_then_and_only_while_it_runs() {
        let (w, h) = (200.0, 100.0);
        let slots = 40;
        let mut seen = 0;
        for k in 0..slots {
            let s = (0..(SLOT_MS as u64)).step_by(50).any(|t| shot_at(k as f64 * SLOT_MS + t as f64, w, h).is_some());
            seen += usize::from(s);
        }
        assert!(seen > 5 && seen < slots - 5, "{seen} of {slots} slots");
        // the one in flight draws dots with the head marked; none when it is not to be shown
        let t =
            (0..400_000).step_by(20).map(|t| t as f64).find(|t| shot_at(*t, w, h).is_some_and(|s| s.u > 0.4)).unwrap();
        let with = field(t, 1.0, 1.0, 100, 25, false);
        assert!(with.cells.iter().any(|c| c.core));
        let without = field(t, 1.0, 0.0, 100, 25, false);
        assert!(!without.cells.iter().any(|c| c.core));
    }

    #[test]
    fn ascii_draws_plain_marks() {
        let g = field(2500.0, 1.0, 1.0, 80, 24, true);
        assert!(!dots(&g).is_empty());
        assert!(dots(&g).iter().all(|(_, c)| matches!(c, '.' | ':' | '*')));
    }

    #[test]
    fn the_field_fades_in_rests_and_wakes() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut h = HomeFx::default();
        assert_eq!(h.level(at(0)), 0.0);
        let mut clock = StarsFx::default();
        clock.begin(t0);
        h = HomeFx { since: Some(t0), gone: None, clock, shown: Cell::new(true) };
        assert!(h.level(at(IN_MS / 3)) < 1.0);
        assert_eq!(h.level(at(IN_MS)), 1.0);
        assert!(h.drifting(at(5000)));
        let rest = crate::fx_stars::REST_AFTER_MS + crate::fx_stars::STOP_MS + 10;
        assert!(!h.drifting(at(rest)), "at rest after a while with no input");
        assert_eq!(h.pace(at(rest)), 0.0);
        let still = h.phase(at(rest));
        assert_eq!(h.phase(at(rest + 5000)), still, "the still frame stays");
        h.clock.input(at(rest + 6000));
        assert!(h.drifting(at(rest + 6100)));
        assert!(h.phase(at(rest + 7000)) > still);
        // gone: out in OUT_MS
        h.gone = Some(at(rest + 8000));
        assert!(h.level(at(rest + 8000 + OUT_MS / 2)) > 0.0);
        assert_eq!(h.level(at(rest + 8000 + OUT_MS)), 0.0);
        assert!(!h.drifting(at(rest + 8100)));
    }

    #[test]
    fn the_cells_by_text_are_not_free() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 30, 5);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, theme.base());
        buf.set_string(10, 2, "hello world", theme.base());
        let free = clear_of_text(&buf, area, &theme, 2, 1, u16::MAX);
        let at = |x: usize, y: usize| free[y * 30 + x];
        // the text, the gap between its words, the margin beside it and the rows above and below
        for (x, y) in [(10, 2), (15, 2), (8, 2), (22, 2), (12, 1), (12, 3)] {
            assert!(!at(x, y), "({x}, {y}) is by the text");
        }
        for (x, y) in [(2, 2), (26, 2), (12, 0), (12, 4), (0, 0)] {
            assert!(at(x, y), "({x}, {y}) is clear");
        }
        // a second column of text: with a small `gap` the room between the two is free, a word gap is not
        buf.set_string(27, 2, "ok", theme.base());
        let whole = clear_of_text(&buf, area, &theme, 0, 0, u16::MAX);
        let split = clear_of_text(&buf, area, &theme, 0, 0, 3);
        assert!(!whole[2 * 30 + 23] && split[2 * 30 + 23], "between the columns");
        assert!(!split[2 * 30 + 15], "between the words");
        assert!(!split[2 * 30 + 27] && split[2 * 30 + 9]);
    }
}
