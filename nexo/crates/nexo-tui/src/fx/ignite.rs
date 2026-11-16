//! The ignition: how nexo opens. The mark of nexo is a constellation (four strokes curving into one centre,
//! five nodes; `docs/img/logo.svg`), and when a session opens on its banner the mark lights up twice at
//! once: small in the banner (see `render::blocks::banner`) and large in the sky of the empty transcript
//! under it. The outer nodes ignite one after another, clockwise from the top left, each with a flare that
//! overshoots and settles. From each node a light runs down its stroke to the centre, slow at the ends and
//! quick in the middle, with a white-hot head and a tail that cools to the accent. The centre, a faint seed
//! that gathers the lights as they arrive, ignites with the last one: it sends the light back out along every
//! stroke and a ring out through the sky, and each star the ring crosses flashes once in answer. Then the
//! large mark folds into its centre and goes out, and from `END_MS` on only the still banner and the
//! starfield are left.
//!
//! Every frame is a pure function of the time since the session opened (fixed seeds, no libm: square roots
//! only), drawn on the blank cells away from text, and nothing is drawn from `END_MS` on, so the frame loop
//! goes idle once it is over. A key cuts it short to that end state, and with `[ui] animations = false` the
//! banner shows its end state from the start. Braille dots where the terminal has them, `.`, `:` and `*` in
//! ASCII mode (no ring there); on 16 colours a few steps of the theme's colours instead of a blend, and the
//! faintest dots left out; a monochrome theme has no motion at all.

use std::sync::OnceLock;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::fx_starfield::{brightness, cell_seed, glyph, hosts_star, Look};
use crate::fx_stars::{cos, sin, Canvas, FigCell, Grid, TAU};
use crate::motion::fade;
use crate::theme::{mix, Theme};

// ---- the score ----

/// When each outer node ignites, in ms from the opening, clockwise from the top left.
pub const BORN: [u64; 4] = [40, 110, 180, 250];
/// A node sends its light down its stroke this long after it ignites ...
pub const SEND_AFTER: u64 = 50;
/// ... and the light takes this long to reach the centre.
pub const RUN_MS: u64 = 340;
/// The centre ignites when the last light reaches it.
pub const CENTRE_MS: u64 = BORN[3] + SEND_AFTER + RUN_MS;
/// From the centre the light runs back out to every node in this long ...
pub const ECHO_MS: u64 = 220;
/// ... and a ring crosses the sky in this long.
pub const RING_MS: u64 = 560;
/// The large mark folds into its centre from this moment ...
pub const FOLD_FROM: u64 = 1030;
/// ... and everything has settled here: the still banner, the starfield, nothing of the ignition.
pub const END_MS: u64 = 1450;

// The whole ignition, and the frames the loop asks for after it, take under 1.6 s.
const _: () = assert!(CENTRE_MS + ECHO_MS < FOLD_FROM && CENTRE_MS + RING_MS + 200 < END_MS && END_MS + 100 < 1600);

/// How long a node takes to grow to its size (past it and back), and how long its flare lasts.
const GROW_MS: f32 = 200.0;
const FLARE_MS: f32 = 300.0;
/// How long a node or the centre stays white-hot after it ignites.
const HOT_MS: f32 = 110.0;
/// The brightness of a stroke once lit, and after the light from the centre passed back over it.
const LIT: f32 = 0.45;
const LIT_AFTER: f32 = 0.6;
/// How much of a stroke the bright tail behind a running light covers.
const TAIL: f32 = 0.24;
/// The brightness of the faint outline the mark shows before it lights.
const GHOST: f32 = 0.16;

/// Slow at both ends and quick in the middle: two halves of a parabola, so the inverse needs only a square root.
fn in_out(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    if u < 0.5 {
        2.0 * u * u
    } else {
        1.0 - 2.0 * (1.0 - u) * (1.0 - u)
    }
}

/// The `u` at which `in_out(u)` is `f`.
fn in_out_inv(f: f32) -> f32 {
    let f = f.clamp(0.0, 1.0);
    if f < 0.5 {
        (f / 2.0).sqrt()
    } else {
        1.0 - ((1.0 - f) / 2.0).sqrt()
    }
}

/// Fast at first, settling softly.
fn out(u: f32) -> f32 {
    let v = 1.0 - u.clamp(0.0, 1.0);
    1.0 - v * v
}

/// The `u` at which `out(u)` is `f`.
fn out_inv(f: f32) -> f32 {
    1.0 - (1.0 - f.clamp(0.0, 1.0)).sqrt()
}

/// Past 1 and back: the size of a node as it ignites, about a tenth over its own at two thirds of the way.
fn overshoot(u: f32) -> f32 {
    let (c, v) = (1.70158, u.clamp(0.0, 1.0) - 1.0);
    1.0 + (c + 1.0) * v * v * v + c * v * v
}

/// When the light of stroke `i` leaves its node.
fn sent(i: usize) -> f32 {
    (BORN[i] + SEND_AFTER) as f32
}

/// 0..1: how far down stroke `i` (from its node to the centre) the light has run at `t` ms; `None` before it
/// leaves. The strokes are numbered clockwise from the top left, like the nodes they leave.
pub fn run(i: usize, t: f32) -> Option<f32> {
    let u = (t - sent(i)) / RUN_MS as f32;
    (u >= 0.0).then(|| in_out(u))
}

/// When the light of stroke `i` is `f` (0..1) of the way from its node to the centre, in ms.
pub fn reach(i: usize, f: f32) -> f32 {
    sent(i) + RUN_MS as f32 * in_out_inv(f)
}

/// When the light the centre sends back out is `f` (0..1) of the way from the centre to the nodes, in ms.
pub fn echo(f: f32) -> f32 {
    CENTRE_MS as f32 + ECHO_MS as f32 * out_inv(f)
}

/// 0..1: how brightly a star `d` dots from the centre of the large mark answers the ring at `t` ms, for a
/// ring that reaches `rmax` dots: nothing until the ring crosses it, a quick flash, a slower fall that is
/// over by `END_MS` (shorter for the far stars, which the ring reaches late).
pub fn answer(t: f32, d: f32, rmax: f32) -> f32 {
    const RISE: f32 = 50.0;
    if rmax <= 0.0 || d >= rmax {
        return 0.0;
    }
    let crossed = CENTRE_MS as f32 + RING_MS as f32 * out_inv(d / rmax);
    let fall = (END_MS as f32 - 20.0 - crossed - RISE).min(450.0);
    let a = t - crossed;
    if a < 0.0 {
        0.0
    } else if a < RISE {
        a / RISE
    } else {
        let v = (1.0 - (a - RISE) / fall).max(0.0);
        v * v
    }
}

// ---- the mark, from docs/img/logo.svg ----

/// The mark is 240 x 80 units, its outer nodes at the corners and its centre in the middle.
const MARK_W: f32 = 240.0;
const MARK_H: f32 = 80.0;
const CENTRE: (f32, f32) = (MARK_W / 2.0, MARK_H / 2.0);
/// The radius of an outer node and of the centre, in units of the mark.
const NODE_R: f32 = 11.0;
const CENTRE_R: f32 = 15.0;
/// Points spread evenly along a stroke.
const SAMPLES: usize = 97;

struct Stroke {
    pts: [(f32, f32); SAMPLES],
    len: f32,
}

/// The top left stroke of the mark (`M0 0 H56 Q80 0 80 24 Q80 40 100 40 H120`) as points spread evenly along
/// it, and its length; the other three are its mirror images.
fn stroke() -> &'static Stroke {
    static S: OnceLock<Stroke> = OnceLock::new();
    S.get_or_init(|| {
        let quad = |a: (f32, f32), c: (f32, f32), b: (f32, f32), t: f32| {
            let s = 1.0 - t;
            (s * s * a.0 + 2.0 * s * t * c.0 + t * t * b.0, s * s * a.1 + 2.0 * s * t * c.1 + t * t * b.1)
        };
        let mut fine = vec![(0.0, 0.0), (56.0, 0.0)];
        fine.extend((1..=32).map(|j| quad((56.0, 0.0), (80.0, 0.0), (80.0, 24.0), j as f32 / 32.0)));
        fine.extend((1..=32).map(|j| quad((80.0, 24.0), (80.0, 40.0), (100.0, 40.0), j as f32 / 32.0)));
        fine.push(CENTRE);
        let mut acc = vec![0.0f32];
        for w in fine.windows(2) {
            let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
            acc.push(acc[acc.len() - 1] + (dx * dx + dy * dy).sqrt());
        }
        let len = acc[acc.len() - 1];
        let mut pts = [(0.0, 0.0); SAMPLES];
        let mut seg = 0;
        for (n, p) in pts.iter_mut().enumerate() {
            let at = len * n as f32 / (SAMPLES - 1) as f32;
            while seg + 2 < acc.len() && acc[seg + 1] < at {
                seg += 1;
            }
            let k = ((at - acc[seg]) / (acc[seg + 1] - acc[seg]).max(1e-6)).clamp(0.0, 1.0);
            let (a, b) = (fine[seg], fine[seg + 1]);
            *p = (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
        }
        Stroke { pts, len }
    })
}

/// The point `f` (0..1) of the way down stroke `i`, from its node to the centre, in units of the mark.
fn point(i: usize, f: f32) -> (f32, f32) {
    let s = stroke();
    let x = f.clamp(0.0, 1.0) * (SAMPLES - 1) as f32;
    let j = (x as usize).min(SAMPLES - 2);
    let k = x - j as f32;
    let (a, b) = (s.pts[j], s.pts[j + 1]);
    let (px, py) = (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k);
    match i {
        0 => (px, py),
        1 => (MARK_W - px, py),
        2 => (MARK_W - px, MARK_H - py),
        _ => (px, MARK_H - py),
    }
}

// ---- the sky ----

/// Where the large mark sits: its centre and its width, in dots of the room.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub w: f32,
}

/// The widest the large mark is drawn, and the narrowest (with less room there is none), in dots.
const MAX_W: f32 = 150.0;
const MIN_W: f32 = 44.0;

/// Dots per cell, across and down.
fn per_cell(ascii: bool) -> (f32, f32) {
    if ascii {
        (1.0, 2.0)
    } else {
        (2.0, 4.0)
    }
}

/// Where the large mark goes in a room of `cols` x `rows` cells whose cells clear of text are `free`: in the
/// tallest band of rows free from side to side (under the banner), a row of room above and below it, centred
/// across and a little above the middle down. `None` when that band is too small for it.
pub fn spot(free: &[bool], cols: u16, rows: u16, ascii: bool) -> Option<Spot> {
    let (w, h) = (cols as usize, rows as usize);
    if w == 0 || free.len() < w * h {
        return None;
    }
    let (mut best, mut from) = ((0, 0), None);
    for r in 0..=h {
        let clear = r < h && free[r * w..(r + 1) * w].iter().all(|&f| f);
        match (clear, from) {
            (true, None) => from = Some(r),
            (false, Some(top)) => {
                if r - top > best.1 {
                    best = (top, r - top);
                }
                from = None;
            }
            _ => {}
        }
    }
    let (dx, dy) = per_cell(ascii);
    let (top, band) = (best.0 as f32, best.1 as f32);
    // the mark with its nodes is about 100 units tall for its 240 across
    let fit = ((band - 2.0) * dy * MARK_W / 100.0).min(w as f32 * dx * 0.55).min(MAX_W);
    (fit >= MIN_W).then(|| Spot { x: w as f32 * dx / 2.0, y: (top + band * 0.45) * dy, w: fit })
}

/// How far the ring goes: from the centre of the large mark to the farthest corner of a room `w` x `h` dots.
fn ring_reach(spot: Spot, w: f32, h: f32) -> f32 {
    let dx = spot.x.max(w - spot.x);
    let dy = spot.y.max(h - spot.y);
    (dx * dx + dy * dy).sqrt() + 2.0
}

/// A filled disc of radius `r` dots (one dot when it is smaller than that).
fn disc(cv: &mut Canvas, x: f32, y: f32, r: f32, level: f32, node: bool, hot: bool) {
    let n = r.floor() as i32;
    for j in -n..=n {
        for i in -n..=n {
            if (i * i + j * j) as f32 <= r * r + 0.5 {
                cv.plot_kind(x + i as f32, y + j as f32, level, node, hot);
            }
        }
    }
    if n == 0 {
        cv.plot_kind(x, y, level, node, hot);
    }
}

/// A ring of radius `r` dots, a dot about every dot of its length.
fn circle(cv: &mut Canvas, x: f32, y: f32, r: f32, level: f32, node: bool) {
    let n = (TAU * r).ceil().clamp(8.0, 4000.0) as usize;
    for j in 0..n {
        let a = TAU * j as f32 / n as f32;
        cv.plot_kind(x + cos(a) * r, y + sin(a) * r, level, node, false);
    }
}

/// The sky `t` ms after the session opened, over `cols` x `rows` cells with the large mark at `spot`: empty
/// before the first node ignites and from `END_MS` on.
pub fn sky(t: f32, spot: Spot, cols: u16, rows: u16, ascii: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 || !(BORN[0] as f32..END_MS as f32).contains(&t) {
        return cv.finish();
    }
    let (w, h) = cv.dots();
    let centre_ms = CENTRE_MS as f32;
    // the fold: the mark shrinks into its centre with a quickening pull, the strokes and nodes fading first
    let q = ((t - FOLD_FROM as f32) / (END_MS - FOLD_FROM) as f32).clamp(0.0, 1.0);
    let pull = 1.0 - q * q;
    let dim = 1.0 - crate::fx_stars::smooth(0.35, 0.95, q);
    let k = spot.w / MARK_W;
    let at = |(x, y): (f32, f32)| (spot.x + (x - CENTRE.0) * k * pull, spot.y + (y - CENTRE.1) * k * pull);
    // the outline waits faintly from the first node until the centre lights
    let ghost = if ascii {
        0.0
    } else {
        GHOST * ((t - BORN[0] as f32) / 200.0).clamp(0.0, 1.0) * (1.0 - ((t - centre_ms) / 200.0).clamp(0.0, 1.0))
    };
    // the light the centre sends back out: where it is (1 at the centre, 0 at the nodes) and how bright
    let back = (t >= centre_ms).then(|| {
        let e = out((t - centre_ms) / ECHO_MS as f32);
        let glow = 1.0 - ((t - centre_ms - ECHO_MS as f32) / 150.0).clamp(0.0, 1.0);
        (1.0 - e, glow)
    });
    let steps = ((stroke().len * k / 0.6).ceil() as usize).max(8);
    for i in 0..4 {
        let head = run(i, t);
        let arrived = reach(i, 1.0);
        // the tail cools once its light has reached the centre
        let fresh = if t < arrived { 1.0 } else { (1.0 - (t - arrived) / 140.0).max(0.0) };
        for n in 0..=steps {
            let f = n as f32 / steps as f32;
            let mut level = if n % 3 == 0 { ghost } else { 0.0 };
            let mut hot = false;
            if let Some(p) = head.filter(|&p| f <= p) {
                let behind = p - f;
                let mut l = LIT;
                if behind < TAIL {
                    let near = 1.0 - behind / TAIL;
                    l += (1.0 - LIT) * near * near * fresh;
                    hot = behind < 0.04 && t < arrived;
                }
                level = level.max(l);
            }
            if let Some((front, glow)) = back {
                if f >= front {
                    level = level.max(LIT_AFTER);
                }
                let d = (f - front).abs();
                if d < 0.1 && glow > 0.0 {
                    level = level.max(0.95 * (1.0 - d / 0.1) * glow);
                    hot |= d < 0.025 && glow >= 1.0;
                }
            }
            level *= dim;
            if level > 0.0 {
                let (x, y) = at(point(i, f));
                cv.plot_kind(x, y, level, false, hot);
            }
        }
    }
    // the outer nodes: born white-hot with a flare, growing past their size and back, flaring again a little
    // when the light from the centre reaches them
    let rn = (NODE_R * k * 0.5).max(1.0);
    let home = centre_ms + ECHO_MS as f32;
    for (i, born) in BORN.iter().enumerate() {
        let a = t - *born as f32;
        let (x, y) = at(point(i, 0.0));
        if a < 0.0 {
            if ghost > 0.0 {
                cv.plot_kind(x, y, ghost, true, false);
            }
            continue;
        }
        let hot = a < HOT_MS || (0.0..90.0).contains(&(t - home));
        disc(&mut cv, x, y, rn * overshoot(a / GROW_MS) * pull, 0.9 * dim, true, hot);
        if a < FLARE_MS && !ascii {
            let u = a / FLARE_MS;
            circle(&mut cv, x, y, rn * (1.3 + 2.4 * out(u)), 0.8 * (1.0 - u) * (1.0 - u), true);
        }
        let b = t - home;
        if (0.0..FLARE_MS * 0.7).contains(&b) && !ascii {
            let u = b / (FLARE_MS * 0.7);
            circle(&mut cv, x, y, rn * (1.2 + 1.4 * out(u)), 0.55 * (1.0 - u) * (1.0 - u), true);
        }
    }
    // the centre: a seed that gathers the lights as they arrive, then ignites with the biggest flare; in the
    // fold it outlasts the rest, white-hot, and goes out last
    let (x, y) = at(CENTRE);
    let a = t - centre_ms;
    if a < 0.0 {
        let gathered = (0..4).filter(|&i| t >= reach(i, 1.0)).count() as f32;
        let seed = (0.2 + 0.12 * gathered + 0.06 * sin(t / 90.0)) * ((t - BORN[0] as f32) / 150.0).clamp(0.0, 1.0);
        cv.plot_kind(x, y, seed, false, false);
    } else {
        let rc = (CENTRE_R * k * 0.5).max(1.0);
        let last = 1.0 - crate::fx_stars::smooth(0.8, 1.0, q);
        let hot = a < HOT_MS * 1.4 || q > 0.55;
        disc(&mut cv, x, y, rc * overshoot(a / GROW_MS) * pull, 0.95 * last, false, hot);
        if a < FLARE_MS * 1.3 && !ascii {
            let u = a / (FLARE_MS * 1.3);
            circle(&mut cv, x, y, rc * (1.3 + 3.2 * out(u)), 0.9 * (1.0 - u) * (1.0 - u), false);
        }
    }
    // the ring through the sky, a fainter one just behind it
    let u = (t - centre_ms) / RING_MS as f32;
    if (0.0..1.0).contains(&u) && !ascii {
        let r = ring_reach(spot, w, h) * out(u);
        let l = 0.6 * (1.0 - u) * (1.0 - u).sqrt();
        circle(&mut cv, spot.x, spot.y, r, l, false);
        circle(&mut cv, spot.x, spot.y, r * 0.94, l * 0.35, false);
    }
    cv.finish()
}

// ---- drawing ----

/// How the ignition takes the theme's colours: blended up from the ground on a true- or 256-colour theme, in
/// a few steps of the theme's own colours on 16 (a blend there would only jump between two of them).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ink {
    Blend(Color),
    Steps,
}

impl Ink {
    fn of(theme: &Theme) -> Option<Ink> {
        if theme.mono {
            return None;
        }
        // a ground of another kind than the accent (the terminal's own, mostly) is taken as black, as the
        // stars take it
        Some(match (theme.accent, theme.bg) {
            (Color::Rgb(..), Color::Rgb(..)) | (Color::Indexed(_), Color::Indexed(_)) => Ink::Blend(theme.bg),
            (Color::Rgb(..), _) => Ink::Blend(Color::Rgb(0, 0, 0)),
            (Color::Indexed(_), _) => Ink::Blend(Color::Indexed(16)),
            _ => Ink::Steps,
        })
    }
}

/// The colour of a cell of the sky: the outer nodes in the second accent and the rest in the first (as in the
/// banner), rising from the ground with the brightness and towards the text's colour at the top of it,
/// white-hot (the text's colour, bold) where it is hot. `None` for a dot too faint for the theme's colours.
fn look(theme: &Theme, ink: Ink, c: &FigCell) -> Option<Style> {
    let l = c.level.clamp(0.0, 1.0);
    let hue = if c.node { theme.accent2 } else { theme.accent };
    let bold = if c.core { Modifier::BOLD } else { Modifier::empty() };
    let fg = match ink {
        Ink::Blend(ground) => {
            let lit = fade(theme, ground, hue, l / 0.7);
            if c.core {
                theme.fg
            } else if l > 0.7 {
                fade(theme, lit, theme.fg, (l - 0.7) / 0.3 * 0.6)
            } else {
                lit
            }
        }
        Ink::Steps if c.core || l >= 0.85 => theme.fg,
        Ink::Steps if l >= 0.4 => hue,
        Ink::Steps if l >= 0.2 => theme.dim,
        Ink::Steps => return None,
    };
    Some(Style::default().fg(fg).add_modifier(bold))
}

/// Draws the sky's part of the ignition `t` ms in over `area` (the transcript): the large mark on the cells
/// clear of text (`free`), and the stars of the field (seen with `stars`, none in ASCII mode or without true
/// colour) that the ring has crossed, brighter for a moment. Nothing where there is no room for the mark.
pub(crate) fn draw(
    buf: &mut Buffer,
    theme: &Theme,
    area: Rect,
    free: &[bool],
    t: f32,
    ascii: bool,
    stars: Option<Look>,
) {
    let area = area.intersection(buf.area);
    let (Some(ink), Some(spot)) = (Ink::of(theme), spot(free, area.width, area.height, ascii)) else {
        return;
    };
    let g = sky(t, spot, area.width, area.height, ascii);
    let cols = area.width as usize;
    for (i, c) in g.cells.iter().enumerate() {
        if c.ch == ' ' || !free.get(i).copied().unwrap_or(false) {
            continue;
        }
        let Some(style) = look(theme, ink, c) else { continue };
        let ch = if ascii && c.ch == ':' { '+' } else { c.ch };
        buf[(area.x + (i % cols) as u16, area.y + (i / cols) as u16)].set_char(ch).set_style(style);
    }
    let Some(look) = stars.filter(|_| !ascii) else { return };
    let (dx, dy) = per_cell(false);
    let rmax = ring_reach(spot, area.width as f32 * dx, area.height as f32 * dy);
    let (ground, ink) =
        (Color::Rgb(look.ground.0, look.ground.1, look.ground.2), Color::Rgb(look.ink.0, look.ink.1, look.ink.2));
    for (i, &clear) in free.iter().enumerate().take(cols * area.height as usize) {
        let (col, row) = ((i % cols) as u16, (i / cols) as u16);
        let seed = cell_seed(col, row);
        if !clear || !hosts_star(seed) || buf[(area.x + col, area.y + row)].symbol() != " " {
            continue;
        }
        let (px, py) = ((col as f32 + 0.5) * dx - spot.x, (row as f32 + 0.5) * dy - spot.y);
        let flash = answer(t, (px * px + py * py).sqrt(), rmax);
        // a star brighter on its own cycle than the flash keeps its own light
        if flash <= 0.02 || flash <= brightness(seed, look.time, look.visibility) {
            continue;
        }
        // the flash is the star's own light, tinted at its peak by the ring that set it off
        let fg = mix(mix(ground, ink, flash), theme.accent, 0.3 * flash);
        let mut utf = [0u8; 4];
        buf[(area.x + col, area.y + row)].set_symbol(glyph(seed).encode_utf8(&mut utf)).set_fg(fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u16 = 100;
    const H: u16 = 24;

    fn room() -> Spot {
        let free = vec![true; W as usize * H as usize];
        spot(&free, W, H, false).unwrap()
    }

    fn at(t: f32) -> Grid {
        sky(t, room(), W, H, false)
    }

    /// The cell of the room holding the dot at `(x, y)` (dots).
    fn cell(g: &Grid, (x, y): (f32, f32)) -> FigCell {
        g.cells[(y / 4.0) as usize * g.cols as usize + (x / 2.0) as usize]
    }

    /// Where a point of the mark (its units) is in the room, before the fold.
    fn dot(p: (f32, f32)) -> (f32, f32) {
        let s = room();
        let k = s.w / MARK_W;
        (s.x + (p.0 - CENTRE.0) * k, s.y + (p.1 - CENTRE.1) * k)
    }

    fn lit(g: &Grid) -> usize {
        g.cells.iter().filter(|c| c.ch != ' ').count()
    }

    #[test]
    fn the_same_moment_gives_the_same_sky() {
        for t in [60.0, 400.0, 700.0, 1200.0] {
            assert_eq!(at(t).cells, at(t).cells, "{t}");
        }
    }

    #[test]
    fn the_nodes_ignite_one_after_another_clockwise_from_the_top_left() {
        for (i, born) in BORN.iter().enumerate() {
            let before = cell(&at(*born as f32 - 5.0), dot(point(i, 0.0)));
            let after = cell(&at(*born as f32 + 20.0), dot(point(i, 0.0)));
            assert!(before.level < 0.3, "node {i} waits: {before:?}");
            assert!(after.level >= 0.85 && after.node && after.core, "node {i} is born white-hot: {after:?}");
        }
        assert!(BORN.windows(2).all(|w| w[0] < w[1]));
        // the nodes are the corners of the mark, clockwise from the top left
        let corners: Vec<(f32, f32)> = (0..4).map(|i| point(i, 0.0)).collect();
        assert_eq!(corners, vec![(0.0, 0.0), (240.0, 0.0), (240.0, 80.0), (0.0, 80.0)]);
    }

    #[test]
    fn a_node_grows_past_its_size_and_settles_back() {
        assert_eq!(overshoot(0.0), 0.0);
        assert!((overshoot(1.0) - 1.0).abs() < 1e-6);
        let peak = (0..=100).map(|j| overshoot(j as f32 / 100.0)).fold(0.0, f32::max);
        assert!(peak > 1.05 && peak < 1.2, "{peak}");
    }

    #[test]
    fn the_light_runs_down_each_stroke_and_the_centre_ignites_when_the_last_one_arrives() {
        for i in 0..4 {
            assert_eq!(run(i, sent(i) - 1.0), None);
            for f in [0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
                let p = run(i, reach(i, f)).unwrap();
                assert!((p - f).abs() < 1e-3, "stroke {i}: {p} at the time it should be {f}");
            }
            // slow at the ends, quick in the middle
            let ms = RUN_MS as f32 / 10.0;
            let edge = run(i, sent(i) + ms).unwrap();
            let middle = run(i, sent(i) + 5.0 * ms).unwrap() - run(i, sent(i) + 4.0 * ms).unwrap();
            assert!(middle > 3.0 * edge, "{middle} {edge}");
        }
        assert!((reach(3, 1.0) - CENTRE_MS as f32).abs() < 1e-3);
        assert!((0..3).all(|i| reach(i, 1.0) < CENTRE_MS as f32));
        // the head of a running light is white-hot where the light is
        let t = reach(0, 0.5);
        let g = at(t);
        assert!(cell(&g, dot(point(0, 0.5))).core, "the head is hot");
        let ahead = cell(&g, dot(point(0, 0.8)));
        assert!(ahead.level <= GHOST + 1e-6 && !ahead.core, "only the faint outline ahead of it: {ahead:?}");
        // the centre waits as a faint seed and ignites with the last light
        let seed = cell(&at(BORN[3] as f32), dot(CENTRE));
        let lit = cell(&at(CENTRE_MS as f32 + 20.0), dot(CENTRE));
        assert!(seed.level > 0.0 && seed.level < 0.7 && !seed.core, "{seed:?}");
        assert!(lit.level > 0.9 && lit.core, "{lit:?}");
    }

    #[test]
    fn the_ring_crosses_the_sky_and_each_star_answers_once_in_order() {
        let rmax = 200.0;
        let crossing = |d: f32| CENTRE_MS as f32 + RING_MS as f32 * out_inv(d / rmax);
        for d in [10.0, 60.0, 120.0, 190.0] {
            assert_eq!(answer(crossing(d) - 1.0, d, rmax), 0.0, "{d}: not before the ring");
            assert!(answer(crossing(d) + 50.0, d, rmax) > 0.99, "{d}: a flash as it passes");
            assert_eq!(answer(crossing(d) + 600.0, d, rmax), 0.0, "{d}: once");
        }
        assert!(crossing(10.0) < crossing(60.0) && crossing(60.0) < crossing(190.0));
        assert_eq!(answer(900.0, rmax + 1.0, rmax), 0.0, "beyond the ring's reach");
        // the last star the ring reaches has gone back to its own light by the end
        assert_eq!(answer(END_MS as f32, rmax - 0.01, rmax), 0.0);
        // the ring is in the sky after the centre lights, far out, and gone before the end
        let far = |g: &Grid| g.cells.iter().enumerate().filter(|(i, c)| c.ch != ' ' && i % W as usize > 90).count();
        assert_eq!(far(&at(CENTRE_MS as f32 - 5.0)), 0);
        assert!(far(&at(CENTRE_MS as f32 + RING_MS as f32 * 0.5)) > 0);
        assert_eq!(far(&at((CENTRE_MS + RING_MS) as f32 + 5.0)), 0);
    }

    #[test]
    fn the_mark_folds_into_its_centre_and_nothing_is_left_at_the_end() {
        let span = |g: &Grid| {
            let xs: Vec<usize> = (0..g.cells.len()).filter(|&i| g.cells[i].ch != ' ').map(|i| i % W as usize).collect();
            xs.iter().max().unwrap_or(&0) - xs.iter().min().unwrap_or(&0)
        };
        let whole = span(&at(FOLD_FROM as f32));
        let folding = span(&at(FOLD_FROM as f32 + 250.0));
        assert!(whole > 40 && folding < whole / 2, "{whole} -> {folding}");
        assert!(lit(&at(END_MS as f32 - 20.0)) <= 2, "only the centre's last light");
        assert_eq!(lit(&at(END_MS as f32)), 0);
        assert_eq!(lit(&at(END_MS as f32 + 500.0)), 0);
        assert_eq!(lit(&at(0.0)), 0, "nothing before the first node");
    }

    #[test]
    fn the_large_mark_finds_the_band_under_the_banner_or_is_left_out() {
        let (w, h) = (80u16, 30u16);
        // a banner over the first seven rows: the mark goes in the band under it
        let free: Vec<bool> = (0..w as usize * h as usize).map(|i| i / w as usize >= 7).collect();
        let s = spot(&free, w, h, false).unwrap();
        assert!(s.y > 7.0 * 4.0 && s.y < 30.0 * 4.0 && s.x == 80.0, "{s:?}");
        assert!(s.w <= MAX_W && s.w >= MIN_W);
        let g = sky(800.0, s, w, h, false);
        assert!(
            g.cells[..7 * w as usize].iter().all(|c| c.ch == ' ' || c.level < 0.7),
            "the mark keeps under the banner"
        );
        // too little room: no large mark
        let tight: Vec<bool> = (0..w as usize * h as usize).map(|i| i / w as usize >= 26).collect();
        assert_eq!(spot(&tight, w, h, false), None);
        assert_eq!(spot(&[], 0, 0, false), None);
    }

    #[test]
    fn ascii_uses_only_dots_colons_and_stars_and_draws_no_ring() {
        let free = vec![true; W as usize * H as usize];
        let s = spot(&free, W, H, true).unwrap();
        for t in [100.0, 500.0, 700.0, 900.0, 1200.0] {
            let g = sky(t, s, W, H, true);
            assert!(g.cells.iter().all(|c| matches!(c.ch, ' ' | '.' | ':' | '*')), "{t}");
        }
        let g = sky(CENTRE_MS as f32 + RING_MS as f32 * 0.5, s, W, H, true);
        assert!(g.cells.iter().enumerate().all(|(i, c)| c.ch == ' ' || i % W as usize <= 85), "no ring far out");
    }

    #[test]
    fn sixteen_colours_take_steps_of_the_theme_and_leave_out_the_faintest_dots() {
        let mut theme = Theme::nexo();
        theme.accent = Color::Cyan;
        theme.accent2 = Color::Magenta;
        theme.fg = Color::White;
        theme.dim = Color::DarkGray;
        theme.bg = Color::Reset;
        assert_eq!(Ink::of(&theme), Some(Ink::Steps));
        let dot = |level: f32, node: bool, core: bool| FigCell { ch: '⠁', level, node, core, soft: false };
        let fg = |c: FigCell| look(&theme, Ink::Steps, &c).map(|s| s.fg.unwrap());
        assert_eq!(fg(dot(0.1, false, false)), None);
        assert_eq!(fg(dot(0.3, false, false)), Some(Color::DarkGray));
        assert_eq!(fg(dot(0.6, false, false)), Some(Color::Cyan));
        assert_eq!(fg(dot(0.6, true, false)), Some(Color::Magenta));
        assert_eq!(fg(dot(0.5, false, true)), Some(Color::White));
        // true colour and 256 colours blend from the ground; a mono theme draws nothing
        assert_eq!(Ink::of(&Theme::nexo()), Some(Ink::Blend(Color::Rgb(0, 0, 0))));
        let mut indexed = Theme::nexo();
        indexed.accent = Color::Indexed(81);
        assert_eq!(Ink::of(&indexed), Some(Ink::Blend(Color::Indexed(16))));
        assert_eq!(Ink::of(&Theme::mono()), None);
        let blend = Theme::nexo();
        let faint = look(&blend, Ink::Blend(Color::Rgb(0, 0, 0)), &dot(0.1, false, false)).unwrap();
        let bright = look(&blend, Ink::Blend(Color::Rgb(0, 0, 0)), &dot(0.7, false, false)).unwrap();
        assert_eq!(bright.fg, Some(blend.accent));
        assert_ne!(faint.fg, bright.fg);
    }

    #[test]
    fn a_frame_on_a_big_screen_is_cheap() {
        let (w, h) = (200u16, 60u16);
        let area = Rect::new(0, 0, w, h);
        let free: Vec<bool> = (0..w as usize * h as usize).map(|i| i / w as usize >= 7).collect();
        let theme = Theme::nexo();
        let look = Look::on(&theme, 3.0, 1.0);
        let mut buf = Buffer::empty(area);
        let frames = 60;
        let start = std::time::Instant::now();
        for j in 0..frames {
            draw(&mut buf, &theme, area, &free, j as f32 * END_MS as f32 / frames as f32, false, look);
        }
        let per = start.elapsed() / frames;
        eprintln!("an ignition frame at 200x60: {per:?}");
        // a debug build is several times slower than the release one the 2 ms are for
        let budget = if cfg!(debug_assertions) { 20 } else { 2 };
        assert!(per < std::time::Duration::from_millis(budget), "{per:?}");
    }
}
