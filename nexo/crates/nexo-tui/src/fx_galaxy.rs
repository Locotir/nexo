//! A turning spiral galaxy of dots, the second scene of the welcome figure (`[ui] figure = "galaxy"`).
//!
//! It is drawn by the same engine as the constellation (`fx_stars`: the braille canvas, the rest after
//! 20 s, the beat, the sizes' rules) from a fixed point set: a dense bulge, two logarithmic arms with a few
//! bright knots and strays, and a faint halo. The disc is tilted about 62 degrees from face-on, leans 18 degrees
//! and turns around its own axis with the arms trailing. Everything is a pure function of the phase asked about;
//! no randomness, no libm at run time (`sin` and `cos` are the module ones, the spiral's exponential is a chain
//! of square roots), so the same phase gives the same frame on every machine.

use std::sync::OnceLock;

use crate::anim::ease_out;
use crate::fx_stars::{cos, lit, sin, unit, Canvas, Grid, PI, TAU};

/// The full figure in cells and the smallest one a lower or narrower room still gets: the galaxy is wider
/// than tall (a flat disc seen at a slant), so it asks less height than the constellation: 10 columns to
/// every 3 rows of a braille cell (a dot is square).
pub const FULL: (u16, u16) = (40, 12);
pub const SMALL: (u16, u16) = (27, 8);
/// One turn around its own axis, and the breath of the tilt.
pub const TURN_MS: f64 = 14_000.0;
pub const BREATH_MS: f64 = 23_000.0;
/// The phase of the still frame (animations off, mono): both arms read clearly.
pub const NICE_MS: f64 = 1000.0;

const CORE: usize = 70;
const ARM_BODY: usize = 150;
const KNOTS_PER_ARM: usize = 5;
const STRAYS: usize = 24;
const HALO: usize = 40;
/// Radius of the bulge, and where the arms begin and end (the rim is 1).
const CORE_R: f32 = 0.18;
const ARM_IN: f32 = 0.13;
/// Turns each arm winds from the bulge to the rim.
const ARM_TURNS: f32 = 1.25;
/// Tilt from face-on, its breath, and the lean of the long axis, in radians (62, 3.5 and 18 degrees).
const FACE_TILT: f32 = 1.0821;
const BREATH: f32 = 0.0611;
const LEAN: f32 = 0.3142;
/// A twinkle once in this long, as the constellation's.
const TWINKLE_MS: f64 = 2600.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
    Core,
    Arm,
    Knot,
    Halo,
}

/// One point of the disc: polar around the centre, `y` above (+) or below the plane.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pt {
    pub r: f32,
    pub th: f32,
    pub y: f32,
    pub kind: Kind,
    /// How bright it is at most (strays and the halo are fainter).
    pub weight: f32,
    /// 0..1: where in its twinkle a knot is at phase 0.
    pub phase: f32,
}

/// `k` to the power `t` (k >= 1, t in 0..1) from square roots only: the bits of `t` pick which of
/// k^(1/2), k^(1/4), ... are multiplied in. IEEE `sqrt` and products, so every machine agrees.
pub(crate) fn pow_frac(k: f32, t: f32) -> f32 {
    let (mut out, mut root, mut t) = (1.0f32, k, t.clamp(0.0, 1.0));
    for _ in 0..14 {
        root = root.sqrt();
        t *= 2.0;
        if t >= 1.0 {
            out *= root;
            t -= 1.0;
        }
    }
    out
}

/// -1..1, most likely near 0 (two uniforms added).
fn tri(seed: u32) -> f32 {
    unit(seed) + unit(seed + 7_919) - 1.0
}

pub(crate) fn points() -> &'static [Pt] {
    static P: OnceLock<Vec<Pt>> = OnceLock::new();
    P.get_or_init(|| {
        let golden = PI * (3.0 - 5.0f32.sqrt());
        let k = 1.0 / ARM_IN;
        let mut v = Vec::with_capacity(CORE + 2 * (ARM_BODY + KNOTS_PER_ARM) + STRAYS + HALO);
        // the bulge: thickest in the middle, a little flattened
        for i in 0..CORE {
            let u = (i as f32 + unit(100 + i as u32)) / CORE as f32;
            let r = CORE_R * u * u.sqrt();
            let flat = 0.02 + 0.11 * (1.0 - (r / CORE_R) * (r / CORE_R));
            let th = golden * i as f32;
            v.push(Pt {
                r,
                th,
                y: (2.0 * unit(300 + i as u32) - 1.0) * flat,
                kind: Kind::Core,
                weight: 1.0,
                phase: 0.0,
            });
        }
        // the arms: a logarithmic spiral each, opposite one another
        let on_arm = |arm: usize, t: f32, spread: f32, seed: u32, kind: Kind, weight: f32| {
            let rc = ARM_IN * pow_frac(k, t);
            let th = arm as f32 * PI + ARM_TURNS * TAU * t;
            // tight near the bulge, wider toward the rim
            let r = (rc + tri(seed) * spread * (0.012 + 0.05 * rc)).max(0.05);
            let y = (2.0 * unit(seed + 5) - 1.0) * 0.03 * (1.0 - 0.4 * r);
            Pt { r, th, y, kind, weight, phase: unit(seed + 9) }
        };
        for arm in 0..2 {
            for i in 0..ARM_BODY {
                let seed = 1000 + arm as u32 * 4000 + i as u32 * 11;
                let t = (i as f32 + unit(seed + 1)) / ARM_BODY as f32;
                v.push(on_arm(arm, t, 1.0, seed, Kind::Arm, 1.0));
            }
            for i in 0..KNOTS_PER_ARM {
                let seed = 9000 + arm as u32 * 400 + i as u32 * 11;
                // spread along the arm, never in the bulge
                let t = 0.16 + 0.72 * (i as f32 + 0.5 * unit(seed + 1)) / (KNOTS_PER_ARM as f32 - 0.5);
                v.push(on_arm(arm, t, 0.3, seed, Kind::Knot, 1.0));
            }
        }
        // strays between the arms, so they do not look drawn with a pen
        for i in 0..STRAYS {
            let seed = 12_000 + i as u32 * 13;
            let r = ARM_IN * pow_frac(k, 0.15 + 0.85 * unit(seed));
            v.push(Pt {
                r,
                th: golden * i as f32 + unit(seed + 1),
                y: (2.0 * unit(seed + 2) - 1.0) * 0.03,
                kind: Kind::Arm,
                weight: 0.7,
                phase: 0.0,
            });
        }
        // the halo: faint, in a wider and slightly thicker disc
        for i in 0..HALO {
            let seed = 15_000 + i as u32 * 17;
            let r = 0.25 + 0.9 * ((i as f32 + unit(seed)) / HALO as f32).sqrt();
            v.push(Pt {
                r,
                th: golden * i as f32 * 1.7,
                y: tri(seed + 3) * 0.14,
                kind: Kind::Halo,
                weight: 0.3,
                phase: 0.0,
            });
        }
        v
    })
}

/// Where a point is in its plane at `gather` (0 scattered, 1 gathered): the dots wind in, each starting
/// further out along its own arm and a little behind in angle. The bulge stays where it is.
pub(crate) fn polar(p: &Pt, gather: f32) -> (f32, f32) {
    let g = ease_out(gather.clamp(0.0, 1.0) as f64) as f32;
    if p.kind == Kind::Core {
        return (p.r, p.th);
    }
    (p.r * (1.0 + 0.8 * (1.0 - g)), p.th + 0.9 * (1.0 - g))
}

/// A knot's brightness: 1 most of the time, a brief dip and flare once per `TWINKLE_MS`.
fn twinkle(phase_ms: f64, p: &Pt) -> f32 {
    let k = ((phase_ms / TWINKLE_MS) as f32 + p.phase).fract();
    let h = if k < 0.2 { sin(k / 0.2 * PI) } else { 0.0 };
    0.62 + 0.38 * h
}

/// The galaxy at `phase_ms` of its turn, the dots `gather` of the way in.
pub fn grid(phase_ms: f64, gather: f32, cols: u16, rows: u16, ascii: bool) -> Grid {
    let mut cv = Canvas::new(cols, rows, ascii);
    if cols == 0 || rows == 0 {
        return cv.finish();
    }
    let turn = ((phase_ms % TURN_MS) / TURN_MS) as f32 * TAU;
    let breath = ((phase_ms % BREATH_MS) / BREATH_MS) as f32 * TAU;
    let alpha = PI / 2.0 - FACE_TILT + BREATH * sin(breath);
    let (ca, sa, cl, sl) = (cos(alpha), sin(alpha), cos(LEAN), sin(LEAN));
    let (w, h) = cv.dots();
    let s = (0.455 * w).min(0.82 * h);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let raw = gather.clamp(0.0, 1.0);
    let seen = 0.35 + 0.65 * ease_out(raw as f64) as f32;
    // the bulge comes first
    let core_seen = (raw / 0.3).min(1.0);
    // fewer halo points in a small room
    let halo = if (cols as usize * rows as usize) < 300 { HALO / 2 } else { HALO };
    let mut halo_left = halo;
    for p in points() {
        if p.kind == Kind::Halo {
            if halo_left == 0 {
                continue;
            }
            halo_left -= 1;
        }
        let (r, th) = polar(p, gather);
        let a = th - turn;
        let (xd, zd) = (r * cos(a), r * sin(a));
        let y1 = ca * p.y - sa * zd;
        let z1 = sa * p.y + ca * zd;
        let (px, py) = (cl * xd - sl * y1, sl * xd + cl * y1);
        let f = 1.0 / (1.0 - 0.12 * z1);
        let (x, y) = (cx + px * s * f, cy - py * s * f);
        // the depth of the disc is about +-0.5: stretch it over the whole range of brightness
        let near = lit(z1 * 2.0);
        let (level, node, core) = match p.kind {
            Kind::Core => ((0.72 + 0.28 * near) * core_seen, false, true),
            Kind::Arm => (near * p.weight * (1.0 - 0.3 * r.min(1.0)) * seen, false, false),
            Kind::Knot => (near.max(0.6) * twinkle(phase_ms, p) * seen.max(0.6), true, false),
            Kind::Halo => (near * p.weight * seen, false, false),
        };
        cv.plot_kind(x, y, level, node, core);
        if node && !ascii {
            cv.plot_kind(x + 1.0, y, level, true, false);
            cv.plot_kind(x, y + 1.0, level, true, false);
            cv.plot_kind(x + 1.0, y + 1.0, level, true, false);
        }
    }
    cv.finish()
}

/// The lines of the galaxy at `t_ms`, fully gathered, as braille (a space where there is no dot).
pub fn frame(t_ms: u32, cols: u16, rows: u16) -> Vec<String> {
    let g = grid(t_ms as f64, 1.0, cols, rows, false);
    (0..g.rows).map(|y| g.line(y)).collect()
}

/// The size of the galaxy for the room above the card (`free_w` x `free_h`): the full one when it fits, else
/// the same shape scaled down to the room, never smaller than `SMALL`; none when even that does not fit.
pub fn fit(free_w: u16, free_h: u16) -> Option<(u16, u16)> {
    // 27 columns go with 8 rows and 40 with 12
    let cols = |rows: u16| (27 + ((rows as u32 - 8) * 13 + 2) / 4) as u16;
    let rows = (SMALL.1..=free_h.min(FULL.1)).rev().find(|&r| cols(r) <= free_w)?;
    Some((cols(rows), rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dots(g: &[String]) -> u32 {
        g.iter().flat_map(|l| l.chars()).map(|c| (c as u32).saturating_sub(0x2800).count_ones()).sum()
    }

    fn lines(g: &Grid) -> Vec<String> {
        (0..g.rows).map(|y| g.line(y)).collect()
    }

    #[test]
    fn the_same_phase_gives_the_same_frame_and_a_quarter_turn_another() {
        assert_eq!(frame(3210, 40, 12), frame(3210, 40, 12));
        assert_ne!(frame(0, 40, 12), frame(TURN_MS as u32 / 4, 40, 12));
        assert_ne!(frame(0, 40, 12), frame(TURN_MS as u32 / 2, 40, 12));
    }

    #[test]
    fn every_char_is_braille_or_a_space() {
        for t in (0..14_000).step_by(700) {
            for line in frame(t, 40, 12) {
                assert_eq!(line.chars().count(), 40);
                assert!(line.chars().all(|c| c == ' ' || ('\u{2800}'..='\u{28ff}').contains(&c)), "{line}");
            }
        }
    }

    #[test]
    fn the_dot_count_stays_within_a_fifth_over_a_minute() {
        for (c, r) in [FULL, SMALL, (32, 9)] {
            let counts: Vec<u32> = (0..120).map(|i| dots(&frame(i * 500, c, r))).collect();
            let (lo, hi) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
            assert!(lo > 0 && (hi - lo) as f32 <= 0.2 * hi as f32, "{c}x{r}: {lo}..{hi}");
        }
    }

    #[test]
    fn the_power_of_a_constant_comes_from_square_roots_and_is_pinned() {
        assert_eq!(pow_frac(8.0, 0.0), 1.0);
        for (k, t, want) in [(8.0f32, 1.0 / 3.0, 2.0), (8.0, 0.5, 2.828_427), (100.0, 0.25, 3.162_278), (7.7, 0.9, 6.3)]
        {
            assert!((pow_frac(k, t) - want).abs() < 0.01 * want, "{k}^{t} = {}", pow_frac(k, t));
        }
        // the end of the arm is the rim
        assert!((ARM_IN * pow_frac(1.0 / ARM_IN, 1.0) - 1.0).abs() < 0.001);
        // and the point set is the same on every machine: its size and a checksum of every radius
        let p = points();
        assert_eq!(p.len(), CORE + 2 * (ARM_BODY + KNOTS_PER_ARM) + STRAYS + HALO);
        assert_eq!(p.iter().filter(|q| q.kind == Kind::Knot).count(), 2 * KNOTS_PER_ARM);
        assert_eq!(p.iter().map(|q| q.r.to_bits()).fold(0u32, |a, b| a.rotate_left(5) ^ b), 2_238_229_441);
    }

    #[test]
    fn the_scene_has_a_bulge_two_arms_knots_and_a_thin_disc() {
        let p = points();
        let core: Vec<&Pt> = p.iter().filter(|q| q.kind == Kind::Core).collect();
        assert!(core.iter().all(|q| q.r <= CORE_R + 1e-6));
        // thickest and densest in the middle
        let inner = core.iter().filter(|q| q.r < CORE_R / 2.0).count();
        assert!(inner * 2 > core.len(), "{inner} of {}", core.len());
        // the disc is thin, the bulge thicker
        let arm_y =
            p.iter().filter(|q| matches!(q.kind, Kind::Arm | Kind::Knot)).map(|q| q.y.abs()).fold(0.0, f32::max);
        let core_y = core.iter().map(|q| q.y.abs()).fold(0.0, f32::max);
        assert!(arm_y <= 0.031 && core_y > 2.0 * arm_y, "arms {arm_y}, bulge {core_y}");
        // everything is inside the rim plus the halo
        assert!(p.iter().all(|q| q.r <= 1.2));
        // tight near the bulge, wider toward the rim: how far the first arm's dots sit from its centre line
        let k = 1.0 / ARM_IN;
        let dev = |lo: f32, hi: f32| {
            let d: Vec<f32> = p
                .iter()
                .filter(|q| q.kind == Kind::Arm && q.weight == 1.0)
                .take(ARM_BODY)
                .map(|q| (q.th / (ARM_TURNS * TAU), q))
                .filter(|(t, _)| (lo..hi).contains(t))
                .map(|(t, q)| (q.r - ARM_IN * pow_frac(k, t)).abs())
                .collect();
            d.iter().sum::<f32>() / d.len() as f32
        };
        assert!(dev(0.7, 1.0) > 1.5 * dev(0.0, 0.3), "{} against {}", dev(0.7, 1.0), dev(0.0, 0.3));
    }

    #[test]
    fn the_galaxy_is_a_wide_lens_not_a_ball() {
        let g = grid(NICE_MS, 1.0, FULL.0, FULL.1, false);
        let (w, h) = (g.cols as usize, g.rows as usize);
        let lit = |x: usize, y: usize| g.cells[y * w + x].ch != ' ';
        let xs: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| lit(x, y))).collect();
        let ys: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| lit(x, y))).collect();
        let (bw, bh) = (xs.last().unwrap() - xs[0] + 1, ys.last().unwrap() - ys[0] + 1);
        assert!(bw as f32 >= 1.6 * bh as f32, "lit cells {bw} wide, {bh} tall");
        // the densest 3 x 3 block of cells holds the centre
        let weight = |x: usize, y: usize| (g.cells[y * w + x].ch as u32).saturating_sub(0x2800);
        let mut best = (0, 0, 0);
        for y in 0..h - 2 {
            for x in 0..w - 2 {
                let n: u32 = (0..3)
                    .flat_map(|j| (0..3).map(move |i| (i, j)))
                    .map(|(i, j)| weight(x + i, y + j).count_ones())
                    .sum();
                if n > best.0 {
                    best = (n, x, y);
                }
            }
        }
        let (cx, cy) = (w / 2, h / 2);
        assert!(
            (best.1..best.1 + 3).contains(&cx) && (best.2..best.2 + 3).contains(&cy),
            "densest block at {},{}; centre at {cx},{cy}",
            best.1,
            best.2
        );
        // the four corners (a fifth by a fifth) hold almost nothing
        let (fw, fh) = (w / 5, h / 5 + 1);
        let all: u32 = (0..w * h).map(|i| (g.cells[i].ch as u32).saturating_sub(0x2800).count_ones()).sum();
        let mut corners = 0;
        for (x0, y0) in [(0, 0), (w - fw, 0), (0, h - fh), (w - fw, h - fh)] {
            for y in y0..y0 + fh {
                for x in x0..x0 + fw {
                    corners += weight(x, y).count_ones();
                }
            }
        }
        assert!(corners * 25 <= all, "{corners} of {all} dots sit in the corners");
    }

    #[test]
    fn two_arms_balance_by_angle_in_the_discs_own_plane() {
        let p = points();
        let total = p.len() as f32;
        for k in 0..12 {
            let cut = k as f32 * PI / 12.0;
            let half = p
                .iter()
                .filter(|q| {
                    let (_, th) = polar(q, 1.0);
                    (th - cut).rem_euclid(TAU) < PI
                })
                .count() as f32;
            let other = total - half;
            assert!((half - other).abs() <= 0.15 * half.max(other), "cut {k}: {half} against {other}");
        }
        // the arms alone, too: each of the two has the same number of points on each side
        for kind in [Kind::Arm, Kind::Knot] {
            let pts: Vec<&Pt> = p.iter().filter(|q| q.kind == kind && q.weight == 1.0).collect();
            let half = pts.iter().filter(|q| q.th.rem_euclid(TAU) < PI).count() as f32;
            let other = pts.len() as f32 - half;
            assert!((half - other).abs() <= 0.15 * half.max(other), "{kind:?}: {half} against {other}");
        }
    }

    #[test]
    fn each_arm_winds_about_a_turn_and_a_quarter() {
        let arm: Vec<&Pt> = points().iter().filter(|q| q.kind == Kind::Arm && q.weight == 1.0).take(ARM_BODY).collect();
        let (lo, hi) = arm.iter().fold((f32::MAX, f32::MIN), |(a, b), q| (a.min(q.th), b.max(q.th)));
        assert!(((hi - lo) / TAU - ARM_TURNS).abs() < 0.05, "{} turns", (hi - lo) / TAU);
        // the angle grows with the radius: the arm winds outwards
        let mut by_r = arm.clone();
        by_r.sort_by(|a, b| a.r.total_cmp(&b.r));
        assert!(by_r[0].th < by_r[by_r.len() - 1].th);
    }

    #[test]
    fn the_dots_wind_in_and_the_gathered_frame_is_the_turning_one() {
        let mean = |g: f32| {
            let moving: Vec<&Pt> = points().iter().filter(|q| q.kind != Kind::Core).collect();
            moving.iter().map(|q| polar(q, g).0).sum::<f32>() / moving.len() as f32
        };
        assert!(mean(0.0) > 1.5 * mean(1.0), "{} against {}", mean(0.0), mean(1.0));
        assert!(mean(0.0) > mean(0.5) && mean(0.5) > mean(1.0));
        // each starts a little behind in angle, and the bulge stays where it is
        for q in points() {
            let (r, th) = polar(q, 0.0);
            if q.kind == Kind::Core {
                assert_eq!((r, th), (q.r, q.th));
            } else {
                assert!(r > q.r && th > q.th);
            }
            assert_eq!(polar(q, 1.0), (q.r, q.th), "gathered is in place");
        }
        // gathered: the frame of that phase
        assert_eq!(lines(&grid(2345.0, 1.0, 40, 12, false)), frame(2345, 40, 12));
        assert_ne!(lines(&grid(2345.0, 0.0, 40, 12, false)), frame(2345, 40, 12));
        // the bulge comes up first: nothing of it at the start, and soon after it outshines the rest
        assert!(grid(0.0, 0.0, 40, 12, false).cells.iter().all(|c| !c.core));
        let early = grid(0.0, 0.15, 40, 12, false);
        let mean = |core: bool| {
            let l: Vec<f32> =
                early.cells.iter().filter(|c| c.ch != ' ' && c.core == core && !c.node).map(|c| c.level).collect();
            l.iter().sum::<f32>() / l.len() as f32
        };
        assert!(mean(true) > 1.3 * mean(false), "{} against {}", mean(true), mean(false));
    }

    #[test]
    fn the_tilt_breathes_so_the_figure_does_not_repeat_within_a_minute() {
        let turn = TURN_MS as u32;
        // after whole turns the knots twinkle as before (13 turns are 70 twinkles), so only the tilt differs
        assert_ne!(frame(500, 40, 12), frame(500 + 13 * turn, 40, 12));
        assert_ne!(frame(500, 40, 12), frame(500 + 26 * turn, 40, 12));
    }

    #[test]
    fn the_knots_twinkle() {
        let node_levels = |t: f64| {
            let g = grid(t, 1.0, 40, 12, false);
            g.cells.iter().filter(|c| c.node).map(|c| c.level).sum::<f32>()
        };
        let a: Vec<f32> = (0..10).map(|i| node_levels(i as f64 * 260.0)).collect();
        assert!(a.iter().any(|&x| (x - a[0]).abs() > 0.05), "the knots never change: {a:?}");
        let knots = points().iter().filter(|q| q.kind == Kind::Knot).count();
        assert_eq!(knots, 10);
    }

    #[test]
    fn ascii_uses_only_dots_colons_and_stars() {
        let g = grid(NICE_MS, 1.0, 40, 12, true);
        assert!(g.cells.iter().all(|c| matches!(c.ch, ' ' | '.' | ':' | '*')));
        assert!(g.cells.iter().any(|c| c.ch == '*') && g.cells.iter().any(|c| c.ch == ':'));
    }

    #[test]
    fn fit_scales_from_the_full_size_down_to_the_smallest_and_then_nothing() {
        assert_eq!(fit(40, 12), Some(FULL));
        assert_eq!(fit(200, 60), Some(FULL));
        assert_eq!(fit(27, 8), Some(SMALL));
        assert_eq!(fit(120, 8), Some(SMALL));
        assert_eq!(fit(26, 40), None);
        assert_eq!(fit(120, 7), None);
        assert_eq!(fit(34, 40), Some((34, 10)));
        assert_eq!(fit(0, 0), None);
        for w in 0..60 {
            for h in 0..30 {
                if let Some((c, r)) = fit(w, h) {
                    assert!(c <= w && r <= h && c >= SMALL.0 && r >= SMALL.1 && r <= FULL.1, "{w}x{h}");
                }
            }
        }
    }

    #[test]
    fn tiny_canvases_do_not_panic() {
        for c in 0..6 {
            for r in 0..6 {
                assert_eq!(frame(77, c, r).len(), r as usize);
                assert_eq!(grid(77.0, 0.3, c, r, true).cells.len(), c as usize * r as usize);
            }
        }
    }
}
