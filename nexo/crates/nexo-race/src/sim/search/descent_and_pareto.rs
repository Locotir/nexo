use super::*;

/// The outcome of a descent.
#[derive(Debug, Clone)]
pub struct Descent {
    pub config: Config,
    pub j: f64,
    /// Sweeps run (a sweep that improves nothing ends the descent and counts).
    pub sweeps: usize,
    pub evals: usize,
    /// J after each sweep.
    pub per_sweep: Vec<f64>,
}

/// Up to 5 other values of a knob to try: a numeric knob gets the current value moved by a quarter of its
/// range and by an eighth (halving every sweep) both ways, and one drawn at random; a bool is flipped; an
/// enum tries up to 5 other values.
fn candidates(space: &Space, k: usize, cur: f64, sweep: usize, rng: &mut Rng) -> Vec<f64> {
    let mut raw = Vec::new();
    match &space.knobs[k].kind {
        Kind::Float(lo, hi) => {
            let r = (hi - lo) / 4.0 / f64::from(1u32 << sweep.min(20));
            raw.extend([cur - r, cur - r / 2.0, cur + r / 2.0, cur + r, lo + rng.unit() * (hi - lo)]);
        }
        Kind::Int(lo, hi) => {
            let r = ((hi - lo) as f64 / 4.0 / f64::from(1u32 << sweep.min(20))).max(1.0);
            raw.extend([
                cur - r,
                cur - r / 2.0,
                cur + r / 2.0,
                cur + r,
                *lo as f64 + (rng.unit() * (hi - lo + 1) as f64).floor(),
            ]);
        }
        Kind::Bool => raw.push(1.0 - cur),
        Kind::Enum(e) => raw.extend((0..e.len()).map(|i| i as f64).filter(|&i| i != cur).take(5)),
    }
    let mut out: Vec<f64> = Vec::new();
    for v in raw {
        let v = space.quantize(k, v);
        if v != cur && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// Coordinate descent: one knob at a time, up to 5 candidates, a candidate is accepted if J (the closure,
/// lower is better, infinity for a config that cannot run) improves; `sweeps` passes over the knobs.
pub fn coordinate_descent(
    space: &Space,
    start: Config,
    obj: &(dyn Fn(&Config) -> f64 + Sync),
    sweeps: usize,
    seed: u64,
) -> Descent {
    coordinate_descent_budget(space, start, obj, sweeps, seed, usize::MAX)
}

/// [`coordinate_descent`] that stops after `max_evals` evaluations of the objective.
pub fn coordinate_descent_budget(
    space: &Space,
    start: Config,
    obj: &(dyn Fn(&Config) -> f64 + Sync),
    sweeps: usize,
    seed: u64,
    max_evals: usize,
) -> Descent {
    let mut rng = Rng::new(seed);
    let (mut cur, mut evals) = (start, 1usize);
    let mut j = obj(&cur);
    let mut d = Descent { config: cur.clone(), j, sweeps: 0, evals, per_sweep: Vec::new() };
    for s in 0..sweeps {
        let mut improved = false;
        for k in 0..space.knobs.len() {
            let mut best: Option<(f64, f64)> = None;
            for v in candidates(space, k, cur[k], s, &mut rng) {
                if evals >= max_evals {
                    break;
                }
                let mut c = cur.clone();
                c[k] = v;
                let jc = obj(&c);
                evals += 1;
                if jc < best.map_or(j, |b| b.0) - 1e-12 * j.abs().max(1e-9) {
                    best = Some((jc, v));
                }
            }
            if let Some((jc, v)) = best {
                cur[k] = v;
                j = jc;
                improved = true;
            }
        }
        d.sweeps = s + 1;
        d.per_sweep.push(j);
        if !improved || evals >= max_evals {
            break;
        }
    }
    d.config = cur;
    d.j = j;
    d.evals = evals;
    d
}

// ---- Pareto ----

/// The indices of the points no other point beats on all three axes (lower is better) and strictly on one.
pub fn pareto(points: &[[f64; 3]]) -> Vec<usize> {
    let beats =
        |a: &[f64; 3], b: &[f64; 3]| a.iter().zip(b).all(|(x, y)| x <= y) && a.iter().zip(b).any(|(x, y)| x < y);
    (0..points.len()).filter(|&i| !points.iter().any(|p| beats(p, &points[i]))).collect()
}

/// The configs of the front (rank 1) and, while fewer than `want` are listed, of the next fronts after it
/// (rank 2, 3, ...): `(index, rank)`. Index 0 is the reference and does not count toward `want`.
pub fn pareto_layers(points: &[[f64; 3]], want: usize) -> Vec<(usize, usize)> {
    let (mut left, mut out, mut rank): (Vec<usize>, Vec<(usize, usize)>, usize) =
        ((0..points.len()).collect(), Vec::new(), 0);
    while !left.is_empty() {
        rank += 1;
        let sub: Vec<[f64; 3]> = left.iter().map(|&i| points[i]).collect();
        let layer: Vec<usize> = pareto(&sub).into_iter().map(|k| left[k]).collect();
        out.extend(layer.iter().map(|&i| (i, rank)));
        left.retain(|i| !layer.contains(i));
        if out.iter().filter(|&&(i, _)| i != 0).count() >= want {
            break;
        }
    }
    out
}

/// The axes (cost, extra steps, chance of failing) on which every point is the same: they cannot tell one
/// config from another, so a front over three axes is really over fewer.
pub fn collapsed_axes(points: &[[f64; 3]]) -> Vec<&'static str> {
    const NAMES: [&str; 3] = ["cost", "extra_steps", "p_fail"];
    if points.len() < 2 {
        return Vec::new();
    }
    (0..3)
        .filter(|&a| {
            let lo = points.iter().map(|p| p[a]).fold(f64::INFINITY, f64::min);
            let hi = points.iter().map(|p| p[a]).fold(f64::NEG_INFINITY, f64::max);
            hi - lo <= 1e-9 * hi.abs().max(1e-12)
        })
        .map(|a| NAMES[a])
        .collect()
}

// ---- fixtures and tiers ----
