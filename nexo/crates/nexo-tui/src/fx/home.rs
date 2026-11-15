//! Faint stars on an empty session: while the home is open and nothing has been sent yet, the starfield
//! of the welcome screen (`fx_starfield`: single braille dots, about one cell in five, each on a cycle of
//! its own with a sharp peak) breathes over the transcript. It keeps clear of text: it draws on blank
//! cells only, and not on the ones beside a line of text or above and below it. About every 12 s on
//! average a comet crosses the sky (one at a time, each its own direction, length and speed, its tail
//! swelling in and fading out softly). It is drawn on the beat (12 frames a second), never at the faster
//! frame rate.
//!
//! Each frame is a pure function of the clocks, like the welcome figure (`fx_stars`): fixed seeds, no
//! randomness and no libm. The field rests like that figure too: after `REST_AFTER_MS` with no key or
//! mouse event the stars fade out over `STOP_MS` and the comets ease to a stop, and then no more frames
//! are asked for; the next input fades the stars back in over `FADE_IN_MS`. The field comes in when the
//! empty home shows and goes out in `OUT_MS` once a message is sent or a session with messages is loaded.

use std::cell::Cell;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::anim::progress;
use crate::app::App;
use crate::fx_starfield::{render_stars_in, Look};
use crate::fx_stars::{cos, sin, smooth, soft_k, unit, Canvas, FigCell, Grid, StarsFx};
use crate::motion::{fade, within};
use crate::theme::Theme;

/// The stars come in over this long when the empty home shows ...
pub const IN_MS: u64 = 900;
/// ... and go out over this long once it is not empty any more.
pub const OUT_MS: u64 = 300;
/// The smallest room the field is drawn in, in cells.
pub const MIN: (u16, u16) = (24, 6);
/// A comet may cross once in each slot of this long (half of the slots have one), never two at once ...
pub const SLOT_MS: f64 = 6000.0;
/// ... and takes between this long and this long plus `SHOOT_SPAN_MS` to.
pub const SHOOT_MS: f64 = 1800.0;
pub const SHOOT_SPAN_MS: f64 = 1800.0;
/// Columns kept clear on each side of a line of text, and rows above and below it.
const MARGIN_X: u16 = 2;
const MARGIN_Y: u16 = 1;

// ---- the comets ----

/// The comets at `phase_ms` of the drift over `cols` x `rows` cells. `shown` (0..1) fades them in and out;
/// `shoot` (0..1) is how much of a comet shows (none while the drift comes to rest).
pub fn comet_grid(phase_ms: f64, shown: f32, shoot: f32, cols: u16, rows: u16, ascii: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 || shown <= 0.0 {
        return cv.finish();
    }
    let (w, h) = cv.dots();
    if shoot > 0.0 {
        if let Some(shot) = shot_at(phase_ms, w, h) {
            shooting(&mut cv, &shot, shoot * shown, ascii);
        }
    }
    cv.finish()
}

/// A comet in flight: how far along (0..1), where it set out, its heading, how far it goes (dots) and how
/// long its tail is (dots).
struct Shot {
    u: f32,
    from: (f32, f32),
    dir: (f32, f32),
    len: f32,
    tail: f32,
}

/// The comet of the slot `phase_ms` falls in, while it crosses; half of the slots have none. Each slot's
/// comet has its own duration (1.8 to 3.6 s), heading (left or right, 11 to 69 degrees down), length and tail,
/// so its speed varies too; it starts and ends inside its slot, so two never cross together.
fn shot_at(phase_ms: f64, w: f32, h: f32) -> Option<Shot> {
    if w < 60.0 || h < 16.0 || phase_ms < 0.0 {
        return None;
    }
    let slot = (phase_ms / SLOT_MS).floor();
    let s = 70_000 + slot as u32 * 12;
    if unit(s) > 0.5 {
        return None;
    }
    let dur = SHOOT_MS + SHOOT_SPAN_MS * unit(s + 5) as f64;
    let start = slot * SLOT_MS + 250.0 + unit(s + 1) as f64 * (SLOT_MS - dur - 500.0);
    let u = ((phase_ms - start) / dur) as f32;
    if !(0.0..1.0).contains(&u) {
        return None;
    }
    // down and to the left or the right, between 11 and 69 degrees under the horizon
    let a = 0.2 + 1.0 * unit(s + 2);
    let right = unit(s + 6) < 0.4;
    let x = unit(s + 3);
    Some(Shot {
        u,
        from: (if right { w * (0.02 + 0.55 * x) } else { w * (0.43 + 0.55 * x) }, h * (0.03 + 0.45 * unit(s + 4))),
        dir: (if right { cos(a) } else { -cos(a) }, sin(a)),
        len: (w * (0.2 + 0.25 * unit(s + 7))).min(50.0 + 70.0 * unit(s + 8)),
        tail: 12.0 + 14.0 * unit(s + 9),
    })
}

/// A comet: a bright head with a tail that thins behind it. Its light swells in and fades out like a
/// breath (`k` scales it), and the tail grows out of the head and shrinks back into it, so nothing pops.
fn shooting(cv: &mut Canvas, shot: &Shot, k: f32, ascii: bool) {
    let u = shot.u;
    // a little slower at the end
    let gone = shot.len * u * (1.2 - 0.2 * u);
    let env = smooth(0.0, 0.45, u) * (1.0 - smooth(0.55, 1.0, u));
    let glow = env * k;
    let tail = if ascii { 6 } else { shot.tail as usize };
    // the tail stretches to full length through the first half and draws back through the second
    let reach = 0.25 + 0.75 * env;
    for j in 0..=tail {
        let back = j as f32 * if ascii { 1.5 } else { 1.0 } * reach;
        if back > gone {
            break;
        }
        let d = gone - back;
        let f = 1.0 - j as f32 / (tail + 1) as f32;
        cv.plot_soft(shot.from.0 + shot.dir.0 * d, shot.from.1 + shot.dir.1 * d, glow * f * f, false, j < 3);
    }
}

/// The room of the field in `area`, if it is big enough.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN.0 && area.height >= MIN.1
}

// ---- time ----

/// What the field remembers: when the empty home showed and when it stopped being empty, and the clock of
/// the stars and comets (it runs and rests like the welcome figure's turn).
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

    /// 0..1: how visible the stars are at `now`: the welcome's fades (in on a key, out at rest) and the
    /// fade out of the field once the home is not empty any more.
    fn stars(&self, now: Instant) -> f32 {
        let out = self.gone.map_or(1.0, |gone| 1.0 - progress(Some(gone), now, OUT_MS) as f32);
        self.clock.visibility(now) * out
    }

    /// The field is on screen (the beat is wanted) until the stars have faded out and the empty sky has
    /// been drawn.
    fn drifting(&self, now: Instant) -> bool {
        self.shown.get() && self.since.is_some() && self.gone.is_none() && !self.clock.settled(now)
    }

    /// A fade is under way (frames are wanted). The comets do not ask for more: they ride the beat like
    /// the stars' breath.
    fn moving(&self, now: Instant) -> bool {
        self.shown.get() && (within(self.since, now, IN_MS) || within(self.gone, now, OUT_MS))
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

    /// For the tick: the fades move at frame rate.
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
    let grid = comet_grid(hf.phase(now), shown, hf.pace(now), area.width, area.height, ascii);
    let free = clear_of_text(buf, area, &app.theme, MARGIN_X, MARGIN_Y, u16::MAX);
    let theme = &app.theme;
    paint(buf, area, &grid, &free, ascii, |c| star_style(theme, c));
    // the stars, as on the welcome screen (none in ASCII mode or without true colour), under the comets
    if !ascii {
        if let Some(look) = Look::on(theme, hf.clock.sky_time(now), hf.stars(now)) {
            render_stars_in(area, None, None, Some(&free), look, buf);
        }
    }
}

/// The look of a cell of the field: the far stars grey, the middle ones the accent, the near ones the
/// text's colour, the comet's head brightest, the colours blending as the brightness rises; on a mono theme bold and dim instead.
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
    } else {
        // dim to accent through the middle of the range, not at a step
        fade(theme, theme.dim, theme.accent, smooth(0.3, 0.5, c.level))
    };
    let k = if c.soft { soft_k(c.level) } else { 0.12 + 0.88 * c.level };
    let style = Style::default().fg(fade(theme, theme.bg, to, k));
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

    /// A moment (ms) at which a comet is half way across a room of `w` x `h` dots.
    fn comet_time(w: f32, h: f32) -> f64 {
        (0..400_000)
            .step_by(40)
            .map(|t| t as f64)
            .find(|t| shot_at(*t, w, h).is_some_and(|s| (0.4..0.6).contains(&s.u)))
            .unwrap()
    }

    #[test]
    fn the_comets_are_the_same_for_the_same_phase_and_none_before_the_fade_in() {
        let t = comet_time(200.0, 100.0);
        let a = comet_grid(t, 1.0, 1.0, 100, 25, false);
        assert_eq!(dots(&a), dots(&comet_grid(t, 1.0, 1.0, 100, 25, false)));
        assert!(!dots(&a).is_empty());
        assert!(dots(&comet_grid(t, 0.0, 1.0, 100, 25, false)).is_empty());
        let peak = |g: &Grid| g.cells.iter().map(|c| c.level).fold(0.0, f32::max);
        assert!(peak(&comet_grid(t, 0.3, 1.0, 100, 25, false)) < peak(&a));
    }

    /// The slots of `n` of them in which a comet crosses (sampled every 40 ms) and the comets seen.
    fn comets(n: u32, w: f32, h: f32) -> (usize, Vec<Shot>) {
        let (mut slots, mut shots) = (0, Vec::new());
        for k in 0..n {
            let mut any = false;
            for t in (0..SLOT_MS as u64).step_by(40) {
                if let Some(s) = shot_at(k as f64 * SLOT_MS + t as f64, w, h) {
                    any = true;
                    if s.u < 0.04 {
                        shots.push(s);
                    }
                }
            }
            slots += usize::from(any);
        }
        (slots, shots)
    }

    #[test]
    fn comets_come_about_every_twelve_seconds_and_never_across_slots() {
        let (w, h) = (200.0, 100.0);
        let n = 400;
        let (slots, _) = comets(n, w, h);
        // about half of the slots: one comet per 12 s, 2.5 times as often as the old one per 31 s
        let every = n as f64 * SLOT_MS / 1000.0 / slots as f64;
        assert!((8.0..18.0).contains(&every), "one every {every} s");
        assert!(every < 31.0 / 2.0);
        // a comet starts and ends inside its slot, so two are never up together
        for k in 1..n {
            let edge = k as f64 * SLOT_MS;
            assert!(shot_at(edge, w, h).is_none() && shot_at(edge - 1.0, w, h).is_none(), "slot {k}");
        }
        // not in a room too small for it
        assert!(shot_at(3000.0, 40.0, 10.0).is_none());
    }

    #[test]
    fn comets_vary_in_direction_length_and_speed() {
        let (_, shots) = comets(300, 200.0, 100.0);
        assert!(shots.len() > 80);
        assert!(shots.iter().any(|s| s.dir.0 < 0.0) && shots.iter().any(|s| s.dir.0 > 0.0), "both ways");
        assert!(shots.iter().all(|s| s.dir.1 > 0.15 && s.dir.1 < 0.95), "always down");
        let range = |f: &dyn Fn(&Shot) -> f32| {
            let v: Vec<f32> = shots.iter().map(f).collect();
            (v.iter().cloned().fold(f32::MAX, f32::min), v.iter().cloned().fold(f32::MIN, f32::max))
        };
        let (a, b) = range(&|s| s.len);
        assert!(b > 1.5 * a, "lengths {a}..{b}");
        let (a, b) = range(&|s| s.tail);
        assert!(b > 1.5 * a, "tails {a}..{b}");
        let (a, b) = range(&|s| s.dir.1);
        assert!(b > 2.0 * a, "angles {a}..{b}");
    }

    /// The brightest cell of one comet on its own canvas at `u`.
    fn comet_peak(shot: &Shot, u: f32) -> f32 {
        let mut cv = Canvas::new(100, 25, false);
        shooting(&mut cv, &Shot { u, ..*shot }, 1.0, false);
        cv.finish().cells.iter().map(|c| c.level).fold(0.0, f32::max)
    }

    #[test]
    fn a_comet_swells_in_and_fades_out_instead_of_popping() {
        let shot = Shot { u: 0.0, from: (120.0, 20.0), dir: (-0.8, 0.6), len: 90.0, tail: 20.0 };
        // sampled at the beat over the slowest-to-fastest comets (1.8 to 3.6 s)
        for dur in [1800.0f32, 2700.0, 3600.0] {
            let n = (dur / 83.0) as usize;
            let v: Vec<f32> = (0..=n).map(|i| comet_peak(&shot, i as f32 / n as f32)).collect();
            assert!(v[0] < 0.01 && v[n] < 0.01, "invisible at both ends: {} {}", v[0], v[n]);
            let step = v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
            assert!(step < 0.2, "{dur} ms: a jump of {step} between frames");
            assert!(v.iter().cloned().fold(0.0, f32::max) > 0.9, "and it does shine");
        }
        // the tail comes out of the head: short early, longer at the middle
        let dots = |u: f32| {
            let mut cv = Canvas::new(100, 25, false);
            shooting(&mut cv, &Shot { u, ..shot }, 1.0, false);
            cv.finish().cells.iter().filter(|c| c.ch != ' ' && c.level > 0.05).count()
        };
        assert!(dots(0.12) < dots(0.5));
        assert!(dots(0.95) <= dots(0.5));
    }

    #[test]
    fn comets_show_in_the_field_and_none_when_it_is_at_rest() {
        let t = comet_time(200.0, 100.0);
        let with = comet_grid(t, 1.0, 1.0, 100, 25, false);
        assert!(with.cells.iter().any(|c| c.core));
        let without = comet_grid(t, 1.0, 0.0, 100, 25, false);
        assert!(!without.cells.iter().any(|c| c.core));
    }

    #[test]
    fn nothing_of_the_field_or_a_comet_touches_text() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 100, 30);
        let mut text = Buffer::empty(area);
        text.set_style(area, theme.base());
        for (x, y, line) in [
            (4, 3, "a line of text near the top"),
            (30, 9, "another one in the middle of it"),
            (8, 20, "and a last one"),
        ] {
            text.set_string(x, y, line, theme.base());
        }
        let free = clear_of_text(&text, area, &theme, MARGIN_X, MARGIN_Y, u16::MAX);
        let (mut core, mut painted) = (0, 0);
        for t in (0..240_000).step_by(700) {
            let g = comet_grid(t as f64, 1.0, 1.0, area.width, area.height, false);
            core += g.cells.iter().filter(|c| c.core && c.ch != ' ').count();
            let mut buf = text.clone();
            paint(&mut buf, area, &g, &free, false, |c| star_style(&theme, c));
            let look = Look::on(&theme, t as f32 / 1000.0, 1.0).unwrap();
            render_stars_in(area, None, None, Some(&free), look, &mut buf);
            for y in 0..area.height {
                for x in 0..area.width {
                    let (a, b) = (&text[(x, y)], &buf[(x, y)]);
                    if !free[y as usize * area.width as usize + x as usize] {
                        assert_eq!(a, b, "({x}, {y}) is by the text, at {t}");
                    } else if b.symbol() != " " {
                        painted += 1;
                    }
                }
            }
        }
        assert!(core > 0 && painted > 1000, "comets {core}, dots {painted}");
    }

    #[test]
    fn ascii_comets_draw_plain_marks() {
        let g = comet_grid(comet_time(160.0, 96.0), 1.0, 1.0, 80, 24, true);
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
        assert_eq!(h.stars(at(5000)), 1.0);
        let rest = crate::fx_stars::REST_AFTER_MS + crate::fx_stars::STOP_MS + 200;
        assert!(h.drifting(at(rest - 600)), "the stars are still fading out");
        assert!(h.stars(at(rest - 600)) < 1.0);
        assert!(!h.drifting(at(rest)), "at rest after a while with no input");
        assert_eq!(h.stars(at(rest)), 0.0);
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
