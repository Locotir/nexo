//! A turning constellation of dots on the welcome screen: about nine bright nodes on a sphere, joined
//! by thin lines of dots to their nearest neighbours, inside a cloud of faint stars and a slim tilted
//! ring. It is drawn live into braille cells (2 x 4 dots each, U+2800), or into `.`, `:`, `*` in ASCII mode.
//!
//! The scene is a fixed point set written by a small generator, rotated by a pure function of the
//! phase it is asked about; no randomness and no libm at run time (`sin` and `cos` here use only
//! IEEE operations), so the same phase gives the same frame on every machine.
//!
//! The faint stars do not turn or drift: each one fades in, shines, fades out, rests dark and comes back
//! somewhere else, on a cycle of its own (`twinkle`), and never on a cell the figure uses.
//!
//! The figure turns on the spinner beat while someone is at the keys. After `REST_AFTER_MS` with no key
//! or mouse event it eases to a stop over `STOP_MS` and asks for no more frames; the next input wakes it.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use nexo_core::config::Figure;

use crate::anim::{ease_out, progress};
use crate::app::App;
use crate::fx_galaxy;
use crate::motion::fade;

/// The full figure in cells, and the smallest one a lower or narrower room still gets (the same shape,
/// 9 columns to every 4 rows, because a braille dot is square).
pub const FULL: (u16, u16) = (36, 16);
pub const SMALL: (u16, u16) = (20, 9);
/// One turn around the axis.
pub const TURN_MS: f64 = 9000.0;
/// The dots gather from scattered to the figure in this long, once per start.
pub const GATHER_MS: u64 = 600;
/// With no key or mouse event for this long the figure comes to rest.
pub const REST_AFTER_MS: u64 = 20_000;
/// ... easing to a stop over this long.
pub const STOP_MS: u64 = 1000;
/// The phase of the still frame (animations off, mono): the nicest angle.
pub const NICE_MS: f64 = 1500.0;

const NODES: usize = 9;
const STARS: usize = 150;
const RING_DOTS: usize = 120;
/// Tilt of the axis (towards the viewer) and its roll, in radians.
const TILT: f32 = 0.42;
const ROLL: f32 = 0.30;
/// How far the ring is tilted against the equator.
const RING_TILT: f32 = 0.25;
const RING_R: f32 = 1.22;
/// A faint star's whole cycle (glow in, shine, fade out, dark gap) lasts between these, each its own.
const CYCLE_MIN_MS: f64 = 2000.0;
const CYCLE_SPAN_MS: f64 = 3000.0;
/// The steps of a star's brightness, dimmest to brightest, and the dip of its flicker.
const GLOW: [f32; 4] = [0.22, 0.45, 0.62, 0.8];

// ---- maths without libm ----

pub(crate) const PI: f32 = std::f32::consts::PI;
pub(crate) const TAU: f32 = std::f32::consts::TAU;

/// `sin` from IEEE operations only (a Taylor polynomial on [-pi/2, pi/2]).
pub(crate) fn sin(x: f32) -> f32 {
    let mut a = x - TAU * (x / TAU + 0.5).floor();
    if a > PI / 2.0 {
        a = PI - a;
    } else if a < -PI / 2.0 {
        a = -PI - a;
    }
    let a2 = a * a;
    a * (1.0 + a2 * (-1.0 / 6.0 + a2 * (1.0 / 120.0 + a2 * (-1.0 / 5040.0 + a2 * (1.0 / 362_880.0)))))
}

pub(crate) fn cos(x: f32) -> f32 {
    sin(x + PI / 2.0)
}

fn mix(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// 0..1 from a seed.
pub(crate) fn unit(seed: u32) -> f32 {
    (mix(seed) >> 8) as f32 / 16_777_216.0
}

// ---- the faint stars ----

/// A faint star at one moment: where (0..1 each way across the room) and how bright (0 while dark).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Twinkle {
    pub x: f32,
    pub y: f32,
    pub level: f32,
    /// How many times it has come and gone.
    pub cycle: u32,
}

/// Star number `seed` at `phase_ms`: it fades in, shines with a slight flicker, fades out, stays dark a
/// short while and comes back somewhere else. Its cycle is its own length (2 to 5 s) and offset, so no two
/// stars pulse together; where it comes back and how long it rests come from a hash of the seed and the
/// cycle number, so the same phase always gives the same star.
pub(crate) fn twinkle(seed: u32, phase_ms: f64) -> Twinkle {
    let period = CYCLE_MIN_MS + CYCLE_SPAN_MS * unit(seed) as f64;
    let t = phase_ms.max(0.0) / period + unit(seed.wrapping_add(1)) as f64;
    let cycle = t.floor() as u32;
    let k = t.fract() as f32;
    let key = mix(seed ^ mix(cycle.wrapping_add(0x9e37_79b9)));
    let (x, y) = (unit(key.wrapping_add(1)), unit(key.wrapping_add(2)));
    let life = 1.0 - (0.12 + 0.20 * unit(key.wrapping_add(3)));
    let level = if k >= life {
        0.0
    } else {
        let u = k / life;
        if u < 0.12 {
            GLOW[0]
        } else if u < 0.28 {
            GLOW[1]
        } else if u < 0.66 {
            // a dip now and then, about every tenth of a second
            let beat = (phase_ms.max(0.0) / 110.0) as u32;
            if unit(seed.wrapping_add(7).wrapping_add(beat.wrapping_mul(31))) < 0.1 {
                GLOW[2]
            } else {
                GLOW[3]
            }
        } else if u < 0.82 {
            GLOW[1]
        } else {
            GLOW[0]
        }
    };
    Twinkle { x, y, level, cycle }
}

// ---- the scene ----

struct Scene {
    nodes: [[f32; 3]; NODES],
    edges: Vec<(usize, usize)>,
}

/// `n` points spread evenly over the unit sphere, a little shaken by the seed.
fn sphere(n: usize, shake: f32, seed: u32) -> Vec<[f32; 3]> {
    let golden = PI * (3.0 - 5.0f32.sqrt());
    (0..n)
        .map(|i| {
            let y = (1.0 - 2.0 * (i as f32 + 0.5) / n as f32 + (unit(seed + i as u32 * 3) - 0.5) * shake)
                .clamp(-0.98, 0.98);
            let r = (1.0 - y * y).sqrt();
            let a = golden * i as f32 + (unit(seed + i as u32 * 3 + 1) - 0.5) * shake * 6.0;
            [r * cos(a), y, r * sin(a)]
        })
        .collect()
}

fn scene() -> &'static Scene {
    static S: OnceLock<Scene> = OnceLock::new();
    S.get_or_init(|| {
        let pts = sphere(NODES, 0.25, 40);
        let mut nodes = [[0.0; 3]; NODES];
        nodes.copy_from_slice(&pts);
        // each node is joined to its two nearest ones
        let d2 = |a: &[f32; 3], b: &[f32; 3]| (0..3).map(|k| (a[k] - b[k]) * (a[k] - b[k])).sum::<f32>();
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for i in 0..NODES {
            let mut order: Vec<usize> = (0..NODES).filter(|&j| j != i).collect();
            order.sort_by(|&a, &b| d2(&nodes[i], &nodes[a]).total_cmp(&d2(&nodes[i], &nodes[b])));
            for &j in order.iter().take(2) {
                let e = (i.min(j), i.max(j));
                if !edges.contains(&e) {
                    edges.push(e);
                }
            }
        }
        Scene { nodes, edges }
    })
}

/// The rotation of the figure: a turn around the tilted axis.
struct Rot {
    c: f32,
    s: f32,
    ct: f32,
    st: f32,
    cr: f32,
    sr: f32,
}

impl Rot {
    fn new(turn: f32) -> Rot {
        let a = turn * TAU;
        Rot { c: cos(a), s: sin(a), ct: cos(TILT), st: sin(TILT), cr: cos(ROLL), sr: sin(ROLL) }
    }

    /// x, y (up) and z (towards the viewer) of `p` turned.
    fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let x1 = self.c * p[0] + self.s * p[2];
        let z1 = -self.s * p[0] + self.c * p[2];
        let y2 = self.ct * p[1] - self.st * z1;
        let z2 = self.st * p[1] + self.ct * z1;
        [self.cr * x1 - self.sr * y2, self.sr * x1 + self.cr * y2, z2]
    }
}

// ---- the canvas ----

/// One cell of the figure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FigCell {
    pub ch: char,
    /// 0..1: how near and how lit (the colour follows it).
    pub level: f32,
    /// Part of a bright node.
    pub node: bool,
    /// Part of the galaxy's bright core.
    pub core: bool,
}

/// The figure as cells, `cols` x `rows`, row by row.
#[derive(Debug, Clone)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<FigCell>,
}

impl Grid {
    /// The characters of row `y`.
    pub fn line(&self, y: u16) -> String {
        let w = self.cols as usize;
        self.cells[y as usize * w..(y as usize + 1) * w].iter().map(|c| c.ch).collect()
    }
}

pub(crate) struct Canvas {
    cols: usize,
    rows: usize,
    /// Dots per cell, horizontally and vertically.
    dx: usize,
    dy: usize,
    ascii: bool,
    bits: Vec<u8>,
    level: Vec<f32>,
    node: Vec<bool>,
    core: Vec<bool>,
}

impl Canvas {
    pub(crate) fn new(cols: u16, rows: u16, ascii: bool) -> Canvas {
        let (cols, rows) = (cols as usize, rows as usize);
        let (dx, dy) = if ascii { (1, 2) } else { (2, 4) };
        Canvas {
            cols,
            rows,
            dx,
            dy,
            ascii,
            bits: vec![0; cols * rows],
            level: vec![0.0; cols * rows],
            node: vec![false; cols * rows],
            core: vec![false; cols * rows],
        }
    }

    /// Nothing is drawn yet in the cell holding the dot at `x`, `y`.
    fn free(&self, x: f32, y: f32) -> bool {
        let (w, h) = self.dots();
        x >= 0.0
            && y >= 0.0
            && x < w
            && y < h
            && self.bits[(y as usize / self.dy) * self.cols + x as usize / self.dx] == 0
    }

    pub(crate) fn dots(&self) -> (f32, f32) {
        ((self.cols * self.dx) as f32, (self.rows * self.dy) as f32)
    }

    fn plot(&mut self, x: f32, y: f32, level: f32, node: bool) {
        self.plot_kind(x, y, level, node, false);
    }

    /// A dot that is part of a bright node and/or of the galaxy's core (the colour follows).
    pub(crate) fn plot_kind(&mut self, x: f32, y: f32, level: f32, node: bool, core: bool) {
        let (w, h) = self.dots();
        if !(x >= 0.0 && y >= 0.0 && x < w && y < h) || level <= 0.0 {
            return;
        }
        let (px, py) = (x as usize, y as usize);
        let i = (py / self.dy) * self.cols + px / self.dx;
        let (ix, iy) = (px % self.dx, py % self.dy);
        // braille dot numbers: column 0 is 1, 2, 3, 7 and column 1 is 4, 5, 6, 8
        const BIT: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
        self.bits[i] |= if self.ascii { 1 } else { BIT[ix][iy] };
        if level > self.level[i] {
            self.level[i] = level;
        }
        self.node[i] |= node;
        self.core[i] |= core;
    }

    pub(crate) fn finish(self) -> Grid {
        let cells = (0..self.cols * self.rows)
            .map(|i| {
                let ch = match (self.bits[i], self.ascii) {
                    (0, _) => ' ',
                    (_, true) if self.node[i] || self.core[i] || self.level[i] >= 0.72 => '*',
                    (_, true) if self.level[i] >= 0.42 => ':',
                    (_, true) => '.',
                    (b, false) => char::from_u32(0x2800 + b as u32).unwrap_or(' '),
                };
                FigCell { ch, level: self.level[i], node: self.node[i], core: self.core[i] }
            })
            .collect();
        Grid { cols: self.cols as u16, rows: self.rows as u16, cells }
    }
}

/// The brightness of a dot at depth `z` (-1 far, +1 near).
pub(crate) fn lit(z: f32) -> f32 {
    let d = ((z + 1.0) / 2.0).clamp(0.0, 1.0);
    0.12 + 0.88 * d
}

/// The figure at `phase_ms` of its turn, with the dots `gather` (0 scattered, 1 gathered) of the way in.
pub fn grid(phase_ms: f64, gather: f32, cols: u16, rows: u16, ascii: bool) -> Grid {
    build(phase_ms, gather, cols, rows, ascii, true)
}

/// `grid`, with or without the faint stars.
fn build(phase_ms: f64, gather: f32, cols: u16, rows: u16, ascii: bool, faint: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 {
        return cv.finish();
    }
    let sc = scene();
    let rot = Rot::new(((phase_ms % TURN_MS) / TURN_MS) as f32);
    let (w, h) = cv.dots();
    // the ascii cell is twice as tall as it is wide, and its canvas has 2 dots in that height
    let s = (0.39 * h).min(0.34 * w);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let g = ease_out(gather.clamp(0.0, 1.0) as f64) as f32;
    let seen = 0.35 + 0.65 * g;
    let project = |p: [f32; 3]| {
        let q = rot.apply(p);
        let f = 1.0 / (1.0 - 0.10 * q[2]);
        (cx + q[0] * s * f, cy - q[1] * s * f, q[2])
    };
    let place =
        |x: f32, y: f32, from: (f32, f32)| (from.0 * w + (x - from.0 * w) * g, from.1 * h + (y - from.1 * h) * g);
    // the ring
    let (cr, sr) = (cos(RING_TILT), sin(RING_TILT));
    let ring = if (cols as usize * rows as usize) < 300 { RING_DOTS / 2 } else { RING_DOTS };
    for k in 0..ring {
        let a = k as f32 / ring as f32 * TAU;
        let (x, y, z) = project([RING_R * cos(a), RING_R * sin(a) * sr, RING_R * sin(a) * cr]);
        let from = (unit(3000 + k as u32 * 2), unit(3001 + k as u32 * 2));
        let (x, y) = place(x, y, from);
        cv.plot(x, y, lit(z / RING_R) * 0.7 * seen, false);
    }
    // the lines come once the dots are nearly in place
    let shown = ((gather - 0.45) / 0.55).clamp(0.0, 1.0);
    for &(a, b) in &sc.edges {
        let (pa, pb) = (sc.nodes[a], sc.nodes[b]);
        let dot = pa[0] * pb[0] + pa[1] * pb[1] + pa[2] * pb[2];
        // the angle between the nodes, from their chord (no acos)
        let chord = (2.0 - 2.0 * dot).clamp(0.0, 4.0).sqrt();
        let ang = chord * (1.0 + chord * chord / 24.0);
        let n = ((ang / 0.075) as usize).max(2);
        let take = (n as f32 * shown) as usize;
        for k in 0..=take.min(n) {
            let t = k as f32 / n as f32;
            let m = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t, pa[2] + (pb[2] - pa[2]) * t];
            let len = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt().max(0.01);
            let (x, y, z) = project([m[0] / len, m[1] / len, m[2] / len]);
            cv.plot(x, y, lit(z) * 0.85, false);
        }
    }
    // the nodes: a small block of dots, bigger when near
    for (i, p) in sc.nodes.iter().enumerate() {
        let (x, y, z) = project(*p);
        let from = (unit(2000 + i as u32 * 2), unit(2001 + i as u32 * 2));
        let (x, y) = place(x, y, from);
        let l = lit(z).max(0.4) * seen.max(0.6);
        cv.plot(x, y, l, true);
        if !ascii {
            cv.plot(x + 1.0, y, l, true);
            cv.plot(x, y + 1.0, l, true);
            cv.plot(x + 1.0, y + 1.0, l, true);
        }
    }
    // the faint stars come last, in screen space and only on cells the figure has left empty
    let stars = if faint { ((cols as usize * rows as usize) / 4).min(STARS) } else { 0 };
    for i in 0..stars {
        let st = twinkle(5000 + i as u32 * 4, phase_ms);
        let (x, y) = (st.x * w, st.y * h);
        if st.level > 0.0 && cv.free(x, y) {
            cv.plot(x, y, st.level * seen, false);
        }
    }
    cv.finish()
}

/// The lines of the figure at `t_ms`, fully gathered, as braille (a space where there is no dot).
pub fn frame(t_ms: u32, cols: u16, rows: u16) -> Vec<String> {
    let g = grid(t_ms as f64, 1.0, cols, rows, false);
    (0..g.rows).map(|y| g.line(y)).collect()
}

/// The size of the figure for the room above the card (`free_w` x `free_h`): the full one when it fits, else
/// the same shape scaled down to the room, never smaller than `SMALL`; none when even that does not fit.
pub fn fit(free_w: u16, free_h: u16) -> Option<(u16, u16)> {
    // 20 columns go with 9 rows and 36 with 16
    let cols = |rows: u16| ((rows as u32 * 9 + 2) / 4) as u16;
    let rows = (SMALL.1..=free_h.min(FULL.1)).rev().find(|&r| cols(r) <= free_w)?;
    Some((cols(rows), rows))
}

// ---- time ----

/// What the figure remembers: when it first showed, and where its turn is (it stops when nobody is there).
#[derive(Debug, Default, Clone)]
pub struct StarsFx {
    begun: Option<Instant>,
    /// The turn so far at `start`, in ms, and the input that last kept it going.
    base: f64,
    start: Option<Instant>,
    last_input: Option<Instant>,
    /// The last draw had room for the figure (the beat is only worth asking for then).
    shown: Cell<bool>,
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1000.0
}

impl StarsFx {
    /// The first look: the dots gather now, once per start.
    pub fn begin(&mut self, now: Instant) {
        if self.begun.is_none() {
            *self = StarsFx {
                begun: Some(now),
                base: 0.0,
                start: Some(now),
                last_input: Some(now),
                shown: Cell::new(false),
            };
        }
    }

    /// 0..1: how far the dots have gathered.
    pub fn gather(&self, now: Instant) -> f32 {
        progress(self.begun, now, GATHER_MS) as f32
    }

    /// How far the turn has gone at `now`, in ms: it runs, then eases to a stop, then stays.
    pub fn phase(&self, now: Instant) -> f64 {
        let Some(start) = self.start else { return 0.0 };
        let e = ms(start, now);
        let run = self.last_input.map_or(0.0, |l| ms(start, l)) + REST_AFTER_MS as f64;
        if e <= run {
            return self.base + e;
        }
        // speed (1 - u)^2 over the stop: the distance is a third of its length
        let u = ((e - run) / STOP_MS as f64).min(1.0);
        self.base + run + STOP_MS as f64 / 3.0 * (1.0 - (1.0 - u).powi(3))
    }

    pub(crate) fn at_rest(&self, now: Instant) -> bool {
        let Some(start) = self.start else { return true };
        let run = self.last_input.map_or(0.0, |l| ms(start, l)) + REST_AFTER_MS as f64;
        ms(start, now) > run + STOP_MS as f64
    }

    /// A key or a mouse event: the turn goes on from where it is.
    pub fn input(&mut self, now: Instant) {
        let Some(start) = self.start else { return };
        let running = ms(start, now) <= self.last_input.map_or(0.0, |l| ms(start, l)) + REST_AFTER_MS as f64;
        if !running {
            self.base = self.phase(now);
            self.start = Some(now);
        }
        self.last_input = Some(now);
    }

    /// The dots are still gathering.
    pub fn gathering(&self, now: Instant) -> bool {
        crate::motion::within(self.begun, now, GATHER_MS)
    }
}

impl App {
    /// A key or a mouse event wakes the figure.
    pub fn stars_input(&mut self) {
        let now = self.now();
        self.fx.stars.input(now);
        self.home_input(now);
    }

    /// The figure is on screen and turning (the beat is wanted): the welcome screen is up, the motion is
    /// on, the last draw had room for it and the person was at the keys a short while ago.
    pub fn stars_alive(&self, now: Instant) -> bool {
        self.locked && self.fx_on() && self.fx.stars.shown.get() && !self.fx.stars.at_rest(now)
    }

    /// For the tick: the gather moves at frame rate; with `turning`, the turn itself (a beat) counts too.
    pub fn stars_moving(&self, now: Instant, turning: bool) -> bool {
        self.locked
            && self.fx_on()
            && self.fx.stars.shown.get()
            && (self.fx.stars.gathering(now) || (turning && self.stars_alive(now)))
    }
}

// ---- drawing ----

/// Draws the figure above the card whose first row is `top` in `area`, slid up by `slide` rows and
/// faded by `away` (0..1) while the home unlocks. Nothing when the room is too small.
pub fn draw(f: &mut Frame, app: &App, area: Rect, top: u16, slide: u16, away: f64) {
    let free = top.saturating_sub(area.y).saturating_sub(1);
    let figure = app.opts.figure;
    let galaxy = figure == Figure::Galaxy;
    let size = match figure {
        Figure::None => None,
        Figure::Galaxy => fx_galaxy::fit(area.width, free),
        Figure::Constellation => fit(area.width, free),
    };
    let Some((cols, rows)) = size else {
        app.fx.stars.shown.set(false);
        return;
    };
    app.fx.stars.shown.set(true);
    let theme = &app.theme;
    let now = app.now();
    let still = !app.fx_on();
    let nice = if galaxy { fx_galaxy::NICE_MS } else { NICE_MS };
    let (phase, gather) = if still { (nice, 1.0) } else { (app.fx.stars.phase(now), app.fx.stars.gather(now)) };
    let scene = if galaxy { fx_galaxy::grid } else { grid };
    let grid = scene(phase, gather, cols, rows, crate::ascii::wanted());
    let x0 = area.x + (area.width - cols) / 2;
    let y0 = top as i32 - 1 - rows as i32 - slide as i32;
    let buf = f.buffer_mut();
    let keep = (1.0 - away).clamp(0.0, 1.0) as f32;
    for (i, c) in grid.cells.iter().enumerate() {
        if c.ch == ' ' {
            continue;
        }
        let (x, y) = (x0 + (i % cols as usize) as u16, y0 + (i / cols as usize) as i32);
        if y < area.y as i32 || y >= area.bottom() as i32 || x >= buf.area.right() || y >= buf.area.bottom() as i32 {
            continue;
        }
        let level = c.level * keep;
        let style = if theme.mono {
            // no colour: the near ones bold, the far ones dim (the galaxy: its core and knots bold)
            let m = if galaxy && (c.core || c.node) {
                Modifier::BOLD
            } else if galaxy {
                if level < 0.35 {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }
            } else if level > 0.66 {
                Modifier::BOLD
            } else if level < 0.35 {
                Modifier::DIM
            } else {
                Modifier::empty()
            };
            Style::default().add_modifier(m)
        } else {
            let to = if c.core {
                theme.fg
            } else if c.node {
                theme.accent2
            } else {
                theme.accent
            };
            let k = 0.18 + 0.82 * level;
            let s = Style::default().fg(fade(theme, theme.bg, to, k));
            if level > 0.9 {
                s.add_modifier(Modifier::BOLD)
            } else {
                s
            }
        };
        buf[(x, y as u16)].set_char(c.ch).set_style(style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn dots(g: &[String]) -> u32 {
        g.iter().flat_map(|l| l.chars()).map(|c| (c as u32).saturating_sub(0x2800).count_ones()).sum()
    }

    #[test]
    fn the_same_time_gives_the_same_frame() {
        assert_eq!(frame(1234, 36, 16), frame(1234, 36, 16));
        assert_ne!(frame(0, 36, 16), frame(2000, 36, 16));
    }

    #[test]
    fn every_char_is_braille_or_a_space() {
        for t in (0..9000).step_by(500) {
            for line in frame(t, 36, 16) {
                assert_eq!(line.chars().count(), 36);
                assert!(line.chars().all(|c| c == ' ' || ('\u{2800}'..='\u{28ff}').contains(&c)), "{line}");
            }
        }
    }

    #[test]
    fn the_dot_count_stays_within_a_fifth_over_a_turn() {
        for (c, r) in [FULL, SMALL, (29, 13)] {
            let counts: Vec<u32> = (0..90).map(|i| dots(&frame(i * 100, c, r))).collect();
            let (lo, hi) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
            assert!(lo > 0 && (hi - lo) as f32 <= 0.2 * hi as f32, "{c}x{r}: {lo}..{hi}");
        }
    }

    #[test]
    fn the_turn_comes_round_after_a_period() {
        // the figure itself (nodes) comes round; the faint stars follow their own cycles
        let nodes = |t: f64| -> Vec<bool> { grid(t, 1.0, 36, 16, false).cells.iter().map(|c| c.node).collect() };
        assert_eq!(nodes(100.0), nodes(100.0 + TURN_MS));
        assert_ne!(nodes(100.0), nodes(2600.0));
    }

    #[test]
    fn a_star_rises_holds_and_falls_within_its_cycle() {
        for seed in [5000u32, 5004, 5008, 5012] {
            let period = CYCLE_MIN_MS + CYCLE_SPAN_MS * unit(seed) as f64;
            // sample one whole cycle that starts at a cycle boundary
            let t0 = (1.0 - unit(seed + 1) as f64) * period + 3.0 * period + 1.0;
            let lv: Vec<f32> = (0..400).map(|i| twinkle(seed, t0 + (period - 2.0) * i as f64 / 400.0).level).collect();
            assert_eq!(lv[0], GLOW[0], "starts dim");
            let peak = lv.iter().position(|&l| l == GLOW[3]).expect("it reaches full glow");
            let last_peak = lv.iter().rposition(|&l| l == GLOW[3]).unwrap();
            assert!(last_peak - peak > 40, "and holds there a while");
            assert!(lv[..peak].windows(2).all(|w| w[1] >= w[0]), "rises only");
            let after = &lv[last_peak..];
            assert!(after.windows(2).all(|w| w[1] <= w[0]), "falls only");
            assert_eq!(*lv.last().unwrap(), 0.0, "ends dark");
            assert!(lv.iter().rev().take_while(|&&l| l == 0.0).count() > 20, "after a dark gap");
            assert!((CYCLE_MIN_MS..=CYCLE_MIN_MS + CYCLE_SPAN_MS).contains(&period));
        }
    }

    #[test]
    fn a_star_comes_back_somewhere_else() {
        for seed in [5000u32, 5004, 5008, 5012, 5016] {
            let at = |n: u32| {
                let period = CYCLE_MIN_MS + CYCLE_SPAN_MS * unit(seed) as f64;
                let t = twinkle(seed, (n as f64 + 1.0 - unit(seed + 1) as f64 + 0.01) * period);
                (t.cycle, t.x, t.y)
            };
            for n in 1..6 {
                let (a, b) = (at(n), at(n + 1));
                assert_eq!(b.0, a.0 + 1);
                assert!((a.1, a.2) != (b.1, b.2), "seed {seed} cycle {n}");
            }
            // and it stays put through one cycle
            let period = CYCLE_MIN_MS + CYCLE_SPAN_MS * unit(seed) as f64;
            let t0 = (1.0 - unit(seed + 1) as f64) * period + 2.0 * period + 1.0;
            let (p, q) = (twinkle(seed, t0), twinkle(seed, t0 + period * 0.5));
            assert_eq!((p.x, p.y), (q.x, q.y));
        }
    }

    #[test]
    fn two_stars_are_not_in_phase() {
        let level = |seed: u32, t: f64| twinkle(seed, t).level;
        let mut same = 0;
        for i in 0..300 {
            let t = i as f64 * 50.0;
            same += usize::from(level(5000, t) == level(5004, t));
        }
        assert!(same < 200, "{same} of 300 samples equal");
        let offs: Vec<u32> = (0..20).map(|i| (unit(5000 + i * 4 + 1) * 100.0) as u32).collect();
        let mut sorted = offs.clone();
        sorted.dedup();
        assert!(sorted.len() > 10);
        // at any moment the field holds stars at every step, not all in one
        for t in [0.0, 1234.0, 7777.0] {
            let n = (0..140).filter(|&i| twinkle(5000 + i * 4, t).level > 0.0).count();
            assert!((60..140).contains(&n), "{n} lit at {t}");
        }
    }

    #[test]
    fn no_star_sits_on_the_figure() {
        for ascii in [false, true] {
            let mut added = 0;
            for t in (0..9000).step_by(300) {
                let bare = build(t as f64, 1.0, 36, 16, ascii, false);
                let with = build(t as f64, 1.0, 36, 16, ascii, true);
                for (a, b) in bare.cells.iter().zip(&with.cells) {
                    if a.ch != ' ' {
                        assert_eq!(a, b, "a cell of the figure or ring was touched at {t}");
                    } else if b.ch != ' ' {
                        added += 1;
                        assert!(!b.node && !b.core);
                    }
                }
            }
            assert!(added > 100, "stars fill the empty cells: {added}");
        }
    }

    #[test]
    fn ascii_steps_go_dot_colon_star_colon_dot() {
        let mut seq: Vec<char> = Vec::new();
        for i in 0..600 {
            let l = twinkle(5000, 10_000.0 + i as f64 * 10.0).level;
            let ch = if l >= 0.72 {
                '*'
            } else if l >= 0.42 {
                ':'
            } else if l > 0.0 {
                '.'
            } else {
                ' '
            };
            if seq.last() != Some(&ch) {
                seq.push(ch);
            }
        }
        assert!(seq.windows(2).all(|w| w[0] != w[1]));
        let s: String = seq.iter().collect();
        assert!(s.contains(" .:*"), "{s}");
        assert!(s.contains("*:. "), "{s}");
    }

    #[test]
    fn resting_asks_for_no_frames_and_freezes_the_stars() {
        let t0 = Instant::now();
        let mut fx = StarsFx::default();
        fx.begin(t0);
        let late = t0 + Duration::from_millis(REST_AFTER_MS + STOP_MS + 500);
        assert!(fx.at_rest(late));
        let (a, b) = (fx.phase(late), fx.phase(late + Duration::from_secs(60)));
        assert_eq!(a, b);
        assert_eq!(grid(a, 1.0, 36, 16, false).cells, grid(b, 1.0, 36, 16, false).cells);
        fx.input(late);
        assert!(!fx.at_rest(late + Duration::from_secs(1)));
        assert!(fx.phase(late + Duration::from_secs(2)) > a);
    }

    #[test]
    fn the_figure_has_something_in_every_part() {
        let f = frame(0, 36, 16);
        let rows_with_dots = f.iter().filter(|l| l.trim().chars().count() > 0).count();
        assert!(rows_with_dots >= 12, "{rows_with_dots}");
    }

    #[test]
    fn fit_scales_from_the_full_size_down_to_the_smallest_and_then_nothing() {
        assert_eq!(fit(36, 16), Some(FULL));
        assert_eq!(fit(200, 60), Some(FULL));
        assert_eq!(fit(20, 9), Some(SMALL));
        assert_eq!(fit(80, 9), Some(SMALL));
        assert_eq!(fit(120, 13), Some((29, 13)));
        // narrower than the full one: a smaller one of the same shape
        assert_eq!(fit(29, 40), Some((29, 13)));
        assert_eq!(fit(35, 40), Some((34, 15)));
        assert_eq!(fit(19, 40), None);
        assert_eq!(fit(80, 8), None);
        assert_eq!(fit(0, 0), None);
        for w in 0..60 {
            for h in 0..30 {
                if let Some((c, r)) = fit(w, h) {
                    assert!(c <= w && r <= h && c >= SMALL.0 && r >= SMALL.1, "{w}x{h}");
                }
            }
        }
    }

    #[test]
    fn tiny_canvases_do_not_panic() {
        for c in 0..6 {
            for r in 0..6 {
                assert_eq!(frame(77, c, r).len(), r as usize);
                let a = grid(77.0, 0.3, c, r, true);
                assert_eq!(a.cells.len(), c as usize * r as usize);
            }
        }
    }

    #[test]
    fn the_gather_starts_scattered_and_ends_in_the_figure() {
        let scattered = grid(0.0, 0.0, 36, 16, false);
        let done = grid(0.0, 1.0, 36, 16, false);
        let line = |g: &Grid| (0..g.rows).map(|y| g.line(y)).collect::<Vec<_>>();
        assert_ne!(line(&scattered), line(&done));
        // halfway is neither
        let half = grid(0.0, 0.5, 36, 16, false);
        assert_ne!(line(&half), line(&done));
        assert_ne!(line(&half), line(&scattered));
    }

    #[test]
    fn ascii_uses_only_dots_colons_and_stars() {
        let g = grid(900.0, 1.0, 36, 16, true);
        assert!(g.cells.iter().all(|c| matches!(c.ch, ' ' | '.' | ':' | '*')));
        assert!(g.cells.iter().any(|c| c.ch == '*'));
    }

    #[test]
    fn the_turn_runs_then_eases_to_a_stop_and_stays() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_millis((s * 1000.0) as u64);
        let mut fx = StarsFx::default();
        fx.begin(t0);
        // running at full speed
        assert!((fx.phase(at(5.0)) - 5000.0).abs() < 1.0);
        assert!(!fx.at_rest(at(5.0)));
        // easing: slower than before, never backwards, ends at a third of the stop time further
        let run = REST_AFTER_MS as f64;
        let mut prev = fx.phase(at(20.0));
        assert!((prev - run).abs() < 1.0);
        for i in 1..=10 {
            let p = fx.phase(at(20.0 + i as f64 * 0.1));
            assert!(p >= prev && p - prev <= 100.0 + 0.5, "{prev} {p}");
            prev = p;
        }
        assert!((fx.phase(at(21.0)) - (run + STOP_MS as f64 / 3.0)).abs() < 1.0);
        // and then it stays
        assert_eq!(fx.phase(at(21.5)), fx.phase(at(90.0)));
        assert!(fx.at_rest(at(21.5)));
        // an input wakes it from where it stopped, without a jump
        let stopped = fx.phase(at(90.0));
        fx.input(at(90.0));
        assert!(!fx.at_rest(at(95.0)));
        assert!((fx.phase(at(90.0)) - stopped).abs() < 1.0);
        assert!((fx.phase(at(92.0)) - stopped - 2000.0).abs() < 1.0);
        // the rest clock counts from the last input
        fx.input(at(100.0));
        assert!(!fx.at_rest(at(119.0)));
        assert!(fx.at_rest(at(121.5)));
    }

    #[test]
    fn the_gather_lasts_the_set_time() {
        let t0 = Instant::now();
        let mut fx = StarsFx::default();
        fx.begin(t0);
        assert_eq!(fx.gather(t0), 0.0);
        assert!(fx.gathering(t0 + Duration::from_millis(300)));
        assert_eq!(fx.gather(t0 + Duration::from_millis(GATHER_MS)), 1.0);
        assert!(!fx.gathering(t0 + Duration::from_millis(GATHER_MS + 10)));
        // a second start of the welcome does not gather again
        fx.begin(t0 + Duration::from_secs(5));
        assert_eq!(fx.gather(t0 + Duration::from_secs(5)), 1.0);
    }

    #[test]
    fn a_near_dot_is_brighter_than_a_far_one() {
        assert!(lit(1.0) > lit(0.0) && lit(0.0) > lit(-1.0));
        assert!(lit(-5.0) > 0.0 && lit(5.0) <= 1.0, "out of range depths stay visible and in range");
    }

    #[test]
    fn sin_and_cos_are_close_to_the_real_ones() {
        for i in -400..400 {
            let x = i as f32 * 0.05;
            assert!((sin(x) - x.sin()).abs() < 1e-4, "{x}");
            assert!((cos(x) - x.cos()).abs() < 1e-4, "{x}");
        }
    }
}
