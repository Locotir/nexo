//! The knob search of the session simulator (`SIMULATOR-SPEC.md` 4): a knob space read from `knobs.toml`,
//! Latin-hypercube sampling, tiers of fixtures, the objective J, coordinate descent, k-fold held-out J and
//! the Pareto front over cost, expected extra steps and chance of failing.

use super::behave::{analytic, auto_family, Context, ParamSet};
use super::events::run;
use super::input::{load, Request};
use super::price::{tokens_of, Cache, Prices};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// ---- knob space ----

/// A point of the knob space, one number per knob in the order of the space (an enum is its index).
pub type Config = Vec<f64>;

/// What a knob is.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Float(f64, f64),
    Int(i64, i64),
    Bool,
    Enum(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Knob {
    pub name: String,
    pub kind: Kind,
}

/// Settings fixed by rule, never searched (`SIMULATOR-SPEC.md` 4.1).
pub const FIXED: &str = "directives=0;nudge=0;recall=1;rule=rent;head=turn";

#[derive(Debug, Clone)]
pub struct Space {
    pub knobs: Vec<Knob>,
}

/// A xorshift64* generator: a seed gives the same numbers everywhere.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x2545_F491_4F6C_DD1D);
        if r.0 == 0 {
            r.0 = 0x1234_5678_9ABC_DEF1;
        }
        for _ in 0..4 {
            r.next_u64();
        }
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn list(v: &str) -> Vec<String> {
    v.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Space {
    /// Reads `knobs.toml`: sections `[float]` and `[int]` with `name = [lo, hi]`, `[bool]` with one name per
    /// line, `[enum]` with `name = ["a", "b"]`. `#` starts a comment.
    pub fn parse(text: &str) -> Result<Space, String> {
        let (mut section, mut knobs) = (String::new(), Vec::new());
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = s.trim().to_string();
                continue;
            }
            let at = |m: &str| format!("knobs line {}: {m}", n + 1);
            let (name, val) = line.split_once('=').map_or((line, ""), |(a, b)| (a.trim(), b.trim()));
            let nums = || -> Result<(f64, f64), String> {
                let v = list(val);
                match (v.first().map(|s| s.parse::<f64>()), v.get(1).map(|s| s.parse::<f64>())) {
                    (Some(Ok(a)), Some(Ok(b))) if a <= b => Ok((a, b)),
                    _ => Err(at("expected [lo, hi]")),
                }
            };
            let kind = match section.as_str() {
                "float" => {
                    let (a, b) = nums()?;
                    Kind::Float(a, b)
                }
                "int" => {
                    let (a, b) = nums()?;
                    Kind::Int(a as i64, b as i64)
                }
                "bool" => Kind::Bool,
                "enum" => {
                    let v = list(val);
                    if v.is_empty() {
                        return Err(at("an enum needs values"));
                    }
                    Kind::Enum(v)
                }
                other => return Err(at(&format!("unknown section [{other}]"))),
            };
            if knobs.iter().any(|k: &Knob| k.name == name) {
                return Err(at(&format!("{name} twice")));
            }
            knobs.push(Knob { name: name.to_string(), kind });
        }
        if knobs.is_empty() {
            return Err("knobs: no knob found".into());
        }
        Ok(Space { knobs })
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.knobs.iter().position(|k| k.name == name)
    }

    /// `v` made a legal value of knob `k`: clamped, rounded to 4 significant digits (floats) or to a whole
    /// number, so a config and its spec string are the same thing.
    pub fn quantize(&self, k: usize, v: f64) -> f64 {
        match &self.knobs[k].kind {
            Kind::Float(lo, hi) => format!("{:.3e}", v.clamp(*lo, *hi)).parse::<f64>().unwrap_or(*lo).clamp(*lo, *hi),
            Kind::Int(lo, hi) => v.round().clamp(*lo as f64, *hi as f64),
            Kind::Bool => f64::from(v >= 0.5),
            Kind::Enum(e) => v.round().clamp(0.0, (e.len() - 1) as f64),
        }
    }

    /// The config a spec string describes, every knob not in `spec` taken from `D_SPEC`.
    pub fn parse_spec(&self, spec: &str) -> Result<Config, String> {
        let read = |s: &str| -> BTreeMap<String, String> {
            s.split(';')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect()
        };
        let (given, base) = (read(spec), read(crate::D_SPEC));
        let mut c = Vec::new();
        for (k, knob) in self.knobs.iter().enumerate() {
            let raw = given.get(&knob.name).or_else(|| base.get(&knob.name));
            let v = match (&knob.kind, raw) {
                (Kind::Enum(e), Some(s)) => {
                    e.iter().position(|x| x == s).ok_or(format!("{}={s} is not allowed", knob.name))? as f64
                }
                (Kind::Enum(_), None) => 0.0,
                (Kind::Float(lo, _), None) => *lo,
                (Kind::Int(lo, _), None) => *lo as f64,
                (Kind::Bool, None) => 0.0,
                (_, Some(s)) => {
                    s.replace('_', "").parse::<f64>().map_err(|_| format!("{}={s} is not a number", knob.name))?
                }
            };
            c.push(self.quantize(k, v));
        }
        Ok(c)
    }

    /// `D_SPEC` as a config.
    pub fn default_config(&self) -> Config {
        self.parse_spec(crate::D_SPEC).expect("D_SPEC holds every knob")
    }

    fn value(&self, k: usize, v: f64) -> String {
        match &self.knobs[k].kind {
            Kind::Float(..) => format!("{v}"),
            Kind::Int(..) | Kind::Bool => format!("{}", v as i64),
            Kind::Enum(e) => e[v as usize].clone(),
        }
    }

    /// The spec string of a config: the fixed settings, then every knob, over `D_SPEC`. It pastes into
    /// `nexo bench --arms curator:<spec>`.
    pub fn render(&self, c: &Config) -> String {
        let mut s = FIXED.to_string();
        for (k, knob) in self.knobs.iter().enumerate() {
            let _ = write!(s, ";{}={}", knob.name, self.value(k, c[k]));
        }
        s
    }

    fn at_unit(&self, k: usize, u: f64) -> f64 {
        let v = match &self.knobs[k].kind {
            Kind::Float(lo, hi) => lo + u * (hi - lo),
            Kind::Int(lo, hi) => (*lo as f64 + (u * (hi - lo + 1) as f64).floor()).min(*hi as f64),
            Kind::Bool => f64::from(u >= 0.5),
            Kind::Enum(e) => (u * e.len() as f64).floor().min((e.len() - 1) as f64),
        };
        self.quantize(k, v)
    }
}

/// `n` configs by Latin hypercube sampling: every knob's range is cut into `n` strata and each stratum is
/// used once, in an order drawn per knob. The same seed gives the same configs.
pub fn lhs(space: &Space, n: usize, seed: u64) -> Vec<Config> {
    let mut rng = Rng::new(seed);
    let mut out = vec![vec![0.0; space.knobs.len()]; n];
    for k in 0..space.knobs.len() {
        let mut perm: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            perm.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
        }
        for (i, row) in out.iter_mut().enumerate() {
            let u = (perm[i] as f64 + rng.unit()) / n as f64;
            row[k] = space.at_unit(k, u);
        }
    }
    out
}

// ---- coordinate descent ----

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

// ---- fixtures and tiers ----

/// The fixtures of each tier (`SIMULATOR-SPEC.md` 4.3), as file names without `.jsonl`.
pub const FAST: &[&str] = &[
    "nexo-mock-edab",
    "protocol-msgs",
    "bench-results-D-2",
    "bench-results-full-0",
    "topics",
    "bench-long-D-0",
    "bench-dir-Ddir-4",
    "bench-long-full-1",
    "bench-results-full-4",
    "bench-dir-full-4",
    "synthetic-g3-t8-s1",
    "real-session-vault-audit",
    "bench-realm-D",
];
pub const MID: &[&str] =
    &["synthetic-g5-t24-s0", "bench-marathon-full", "bench-marathon4-full", "bench-marathon-D", "bench-realm100-full"];
pub const SLOW: &[&str] = &["synthetic-g2-t40-s3", "synthetic-g8-t40-s7", "edge"];

/// The fixture names of a tier: `fast`, `mid` (fast and mid) or `all`.
pub fn tier(name: &str) -> Result<Vec<&'static str>, String> {
    let mut v: Vec<&'static str> = FAST.to_vec();
    match name {
        "fast" => {}
        "mid" => v.extend(MID),
        "all" => {
            v.extend(MID);
            v.extend(SLOW);
        }
        other => return Err(format!("unknown tier {other:?} (fast, mid, all)")),
    }
    Ok(v)
}

pub struct Fixture {
    pub name: String,
    pub family: String,
    pub requests: Vec<Request>,
    pub max_steps: usize,
    /// What the session costs with no curator (the price of a failed run, paid again).
    pub plain: f64,
}

/// The cost of sending every request as recorded.
pub fn plain_cost(requests: &[Request], prices: Prices, life: f64, slots: usize) -> f64 {
    let mut cache = Cache::new(life, slots);
    let (mut miss, mut hit, mut out) = (0i64, 0i64, 0i64);
    for q in requests {
        let h = cache.send(&q.messages, q.system_tokens as i64, q.now);
        let all = q.messages.iter().map(tokens_of).sum::<i64>() + q.system_tokens as i64;
        hit += h;
        miss += all - h;
        out += q.out_tokens.unwrap_or(0) as i64;
    }
    prices.dollars(miss, hit) + prices.output(out)
}

/// The numbers of one config on one fixture.
#[derive(Debug, Clone, Copy)]
pub struct Fix {
    pub cost: f64,
    pub extra_cost: f64,
    pub extra_steps: f64,
    pub p_fail: f64,
    pub blind: f64,
}

/// Everything an evaluation needs.
pub struct Setup {
    pub fixtures: Vec<Fixture>,
    pub params: ParamSet,
    pub prices: Prices,
    pub life: f64,
    pub slots: usize,
    /// Family weights; a family without one weighs 0.
    pub weights: BTreeMap<String, f64>,
    /// `c` of the objective: the price of a blind token.
    pub c_blind: f64,
    /// The fold of each fixture.
    pub fold: Vec<usize>,
    pub folds: usize,
}

/// Folds stratified by family: the fixtures sorted by family and name are dealt out in turn.
pub fn assign_folds(fixtures: &[Fixture], folds: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..fixtures.len()).collect();
    order.sort_by(|&a, &b| (&fixtures[a].family, &fixtures[a].name).cmp(&(&fixtures[b].family, &fixtures[b].name)));
    let mut fold = vec![0; fixtures.len()];
    for (n, i) in order.into_iter().enumerate() {
        fold[i] = n % folds.max(1);
    }
    fold
}

impl Setup {
    pub fn weight(&self, i: usize) -> f64 {
        self.weights.get(&self.fixtures[i].family).copied().unwrap_or(0.0)
    }

    /// One config on one fixture: the open-loop pass and the analytic behaviour layer.
    pub fn eval_fixture(&self, i: usize, spec: &str) -> Result<Fix, String> {
        let f = &self.fixtures[i];
        let r = run(&f.requests, spec, self.prices, self.life, self.slots)?;
        let cx = Context::measure(&f.requests, spec, self.prices, f.max_steps);
        let a = analytic(&r, self.params.get(&f.family), self.params.r_den, &cx);
        Ok(Fix {
            cost: r.cost,
            extra_cost: a.extra_cost,
            extra_steps: a.extra_steps,
            p_fail: a.p_fail,
            blind: r.blind.0 as f64,
        })
    }

    /// One config on every fixture; a spec the curator refuses, or a panic, is an error.
    pub fn eval(&self, spec: &str) -> Result<Vec<Fix>, String> {
        let go = || (0..self.fixtures.len()).map(|i| self.eval_fixture(i, spec)).collect::<Result<Vec<_>, _>>();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(go)).unwrap_or_else(|_| Err("panic".into()))
    }

    /// One fixture's part of J.
    pub fn part(&self, i: usize, f: &Fix) -> f64 {
        self.weight(i) * (f.cost + f.extra_cost + self.fixtures[i].plain * f.p_fail + self.c_blind * f.blind)
    }

    /// J over the fixtures `keep` accepts.
    pub fn j(&self, e: &[Fix], keep: &dyn Fn(usize) -> bool) -> f64 {
        (0..e.len()).filter(|&i| keep(i)).map(|i| self.part(i, &e[i])).sum()
    }

    /// J without the family weights.
    pub fn j_flat(&self, e: &[Fix], keep: &dyn Fn(usize) -> bool) -> f64 {
        (0..e.len())
            .filter(|&i| keep(i))
            .map(|i| e[i].cost + e[i].extra_cost + self.fixtures[i].plain * e[i].p_fail + self.c_blind * e[i].blind)
            .sum()
    }

    /// The weighted means over cost, expected extra steps and chance of failing (weights add to 1; equal
    /// weights when every family weighs 0).
    pub fn axes(&self, e: &[Fix]) -> [f64; 3] {
        let total: f64 = (0..e.len()).map(|i| self.weight(i)).sum();
        let w = |i: usize| if total > 0.0 { self.weight(i) / total } else { 1.0 / e.len() as f64 };
        let mut a = [0.0; 3];
        for (i, f) in e.iter().enumerate() {
            a[0] += w(i) * f.cost;
            a[1] += w(i) * f.extra_steps;
            a[2] += w(i) * f.p_fail;
        }
        a
    }

    /// J of fold `k`'s fixtures.
    pub fn j_fold(&self, e: &[Fix], k: usize) -> f64 {
        self.j(e, &|i| self.fold[i] == k)
    }

    /// J of the fixtures outside fold `k`.
    pub fn j_train(&self, e: &[Fix], k: usize) -> f64 {
        self.j(e, &|i| self.fold[i] != k)
    }
}

/// Runs `f(0..n)` on `workers` threads, results in order.
pub fn parallel_map<T: Send>(workers: usize, n: usize, f: &(dyn Fn(usize) -> T + Sync)) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                let v = f(i);
                slots.lock().unwrap()[i] = Some(v);
            });
        }
    });
    slots.into_inner().unwrap().into_iter().map(|v| v.expect("every index ran")).collect()
}

// ---- stages ----

/// One evaluated config.
struct Scored {
    spec: String,
    fix: Vec<Fix>,
}

fn weights_from(text: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for line in text.lines().map(|l| l.split('#').next().unwrap_or("").trim()) {
        if let Some((k, v)) = line.split_once('=') {
            if let Ok(x) = v.trim().parse::<f64>() {
                out.insert(k.trim().to_string(), x);
            }
        }
    }
    out
}

struct Opts {
    knobs: String,
    tier: String,
    configs: usize,
    workers: usize,
    seed: u64,
    stage: String,
    from: Option<String>,
    folds: usize,
    weights: String,
    params: Option<String>,
    out: Option<String>,
    fixtures: String,
    price: String,
    life: f64,
    slots: usize,
    max_steps: usize,
    c_blind: f64,
    holdout: usize,
    sweeps: usize,
    starts: usize,
    pareto_max: usize,
    pareto_min: usize,
}

const USAGE: &str = "usage: sim search [--knobs sim/knobs.toml] [--tier fast|mid|all] [--configs N] [--workers W] [--seed X]
                  [--stage lhs|coord|confirm] [--from FILE] [--folds 3] [--weights sim/weights.toml] [--params sim/params.json]
                  [--out DIR] [--fixtures DIR[:DIR...]] [--price live|legacy] [--life S] [--slots K] [--max-steps 40] [--blind-c C]
                  [--holdout K] [--sweeps 3] [--starts 20] [--pareto-max 50] [--pareto-min 5]
  lhs      sample N configs by Latin hypercube, score them on the tier, write configs.tsv, pareto.tsv, folds.tsv
  coord    --from configs.tsv: coordinate descent from its best --starts configs and D_SPEC (N evaluations in all)
  confirm  --from pareto.tsv: rerun its configs on all 21 fixtures, write confirm.tsv";

fn parse_opts(mut args: impl Iterator<Item = String>) -> Result<Opts, String> {
    let mut o = Opts {
        knobs: "sim/knobs.toml".into(),
        tier: "fast".into(),
        configs: 1000,
        workers: std::thread::available_parallelism().map_or(4, |n| n.get()),
        seed: 1,
        stage: "lhs".into(),
        from: None,
        folds: 3,
        weights: "sim/weights.toml".into(),
        params: None,
        out: None,
        fixtures: format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR")),
        price: "live".into(),
        life: 0.0,
        slots: 0,
        max_steps: super::input::MAX_STEPS,
        c_blind: 0.0,
        holdout: 0,
        sweeps: 3,
        starts: 20,
        pareto_max: 50,
        pareto_min: 5,
    };
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("{a} needs a value\n{USAGE}"));
        let num = |s: String| s.parse::<f64>().map_err(|_| format!("{a}: {s:?} is not a number"));
        match a.as_str() {
            "--knobs" => o.knobs = val()?,
            "--tier" => o.tier = val()?,
            "--configs" => o.configs = num(val()?)? as usize,
            "--workers" => o.workers = num(val()?)? as usize,
            "--seed" => o.seed = num(val()?)? as u64,
            "--stage" => o.stage = val()?,
            "--from" => o.from = Some(val()?),
            "--folds" => o.folds = num(val()?)? as usize,
            "--weights" => o.weights = val()?,
            "--params" => o.params = Some(val()?),
            "--out" => o.out = Some(val()?),
            "--fixtures" => o.fixtures = val()?,
            "--price" => o.price = val()?,
            "--life" => o.life = num(val()?)?,
            "--slots" => o.slots = num(val()?)? as usize,
            "--max-steps" => o.max_steps = num(val()?)? as usize,
            "--blind-c" => o.c_blind = num(val()?)?,
            "--holdout" => o.holdout = num(val()?)? as usize,
            "--sweeps" => o.sweeps = num(val()?)? as usize,
            "--starts" => o.starts = num(val()?)? as usize,
            "--pareto-max" => o.pareto_max = num(val()?)? as usize,
            "--pareto-min" => o.pareto_min = num(val()?)? as usize,
            _ => return Err(format!("unknown option {a:?}\n{USAGE}")),
        }
    }
    if o.folds < 2 {
        return Err("--folds takes 2 or more".into());
    }
    Ok(o)
}

fn build_setup(o: &Opts, tier_name: &str) -> Result<Setup, String> {
    let names = tier(tier_name)?;
    let loaded = parallel_map(o.workers, names.len(), &|i| -> Result<Option<(String, Vec<Request>)>, String> {
        let Some(path) =
            o.fixtures.split(':').map(|d| format!("{d}/{}.jsonl", names[i])).find(|p| std::path::Path::new(p).exists())
        else {
            return Ok(None);
        };
        Ok(Some((names[i].to_string(), load(&path)?.requests)))
    });
    let prices = Prices::named(&o.price).ok_or("--price takes live or legacy")?;
    let mut fixtures = Vec::new();
    for l in loaded {
        if let Some((name, requests)) = l? {
            let family = auto_family(&requests).to_string();
            let plain = plain_cost(&requests, prices, o.life, o.slots);
            fixtures.push(Fixture { name, family, requests, max_steps: o.max_steps, plain });
        }
    }
    if fixtures.is_empty() {
        return Err(format!("no fixture of tier {tier_name} found in {}", o.fixtures));
    }
    if fixtures.len() < names.len() {
        eprintln!(
            "note: {} of {} fixtures of tier {tier_name} are missing in {}",
            names.len() - fixtures.len(),
            names.len(),
            o.fixtures
        );
    }
    let params = match &o.params {
        Some(p) => ParamSet::load(p)?,
        None => ParamSet::default(),
    };
    let weights =
        std::fs::read_to_string(&o.weights).map(|t| weights_from(&t)).map_err(|e| format!("{}: {e}", o.weights))?;
    let fold = assign_folds(&fixtures, o.folds);
    Ok(Setup {
        fixtures,
        params,
        prices,
        life: o.life,
        slots: o.slots,
        weights,
        c_blind: o.c_blind,
        fold,
        folds: o.folds,
    })
}

fn evaluate_all(setup: &Setup, space: &Space, configs: Vec<Config>, workers: usize) -> (Vec<Scored>, usize) {
    let results = parallel_map(workers, configs.len(), &|i| {
        let spec = space.render(&configs[i]);
        let r = setup.eval(&spec);
        (spec, r)
    });
    let mut bad = 0;
    let mut out = Vec::new();
    for (spec, r) in results {
        match r {
            Ok(fix) => out.push(Scored { spec, fix }),
            Err(e) => {
                bad += 1;
                if bad <= 3 {
                    eprintln!("config refused ({e}): {spec}");
                }
            }
        }
    }
    (out, bad)
}

fn row_numbers(setup: &Setup, fix: &[Fix]) -> String {
    let a = setup.axes(fix);
    let mut s = format!(
        "{:.6}\t{:.6}\t{:.4}\t{:.5}\t{:.6}",
        setup.j(fix, &|_| true),
        a[0],
        a[1],
        a[2],
        setup.j_flat(fix, &|_| true)
    );
    for k in 0..setup.folds {
        let _ = write!(s, "\t{:.6}", setup.j_fold(fix, k));
    }
    s
}

fn fold_header(setup: &Setup) -> String {
    (0..setup.folds).map(|k| format!("J_fold{k}")).collect::<Vec<_>>().join("\t")
}

/// The folds D_SPEC loses in held-out J to `fix`: how many of the folds `fix` beats D_SPEC in.
fn folds_beaten(setup: &Setup, fix: &[Fix], dspec: &[Fix]) -> usize {
    (0..setup.folds).filter(|&k| setup.j_fold(fix, k) < setup.j_fold(dspec, k)).count()
}

fn write(dir: &str, name: &str, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    std::fs::write(format!("{dir}/{name}"), text).map_err(|e| format!("{dir}/{name}: {e}"))
}

/// configs.tsv, pareto.tsv and folds.tsv for a set of scored configs and D_SPEC.
fn report(
    setup: &Setup,
    o: &Opts,
    dir: &str,
    dspec: &Scored,
    scored: &[Scored],
) -> Result<Vec<(usize, usize)>, String> {
    let head = format!("J\tcost\textra_steps\tp_fail\tJ_unweighted\t{}", fold_header(setup));
    let mut all: Vec<&Scored> = scored.iter().collect();
    all.sort_by(|a, b| a.spec.cmp(&b.spec));
    all.dedup_by(|a, b| a.spec == b.spec);
    let mut t = format!("{head}\tspec\n");
    t += &format!("{}\t{}\n", row_numbers(setup, &dspec.fix), dspec.spec);
    for s in &all {
        t += &format!("{}\t{}\n", row_numbers(setup, &s.fix), s.spec);
    }
    write(dir, "configs.tsv", &t)?;

    // Pareto: D_SPEC takes part, so a config it dominates is not listed; its own row is the reference
    let mut pool: Vec<&Scored> = vec![dspec];
    pool.extend(all.iter().copied());
    let pts: Vec<[f64; 3]> = pool.iter().map(|s| setup.axes(&s.fix)).collect();
    let layers = pareto_layers(&pts, o.pareto_min);
    let total = layers.iter().filter(|&&(i, r)| i != 0 && r == 1).count();
    let mut front: Vec<(usize, usize)> = layers.into_iter().filter(|&(i, _)| i != 0).collect();
    front.sort_by(|&(a, ra), &(b, rb)| {
        let (ja, jb) = (setup.j(&pool[a].fix, &|_| true), setup.j(&pool[b].fix, &|_| true));
        ra.cmp(&rb).then(ja.total_cmp(&jb)).then(pool[a].spec.cmp(&pool[b].spec))
    });
    front.truncate(o.pareto_max);
    let mut t = format!("id\t{head}\tfolds_beaten\trank\tspec\n");
    t += &format!("D_SPEC\t{}\t-\t-\t{}\n", row_numbers(setup, &dspec.fix), dspec.spec);
    for (n, &(i, rank)) in front.iter().enumerate() {
        let s = pool[i];
        t += &format!(
            "P{:03}\t{}\t{}/{}\t{}\t{}\n",
            n + 1,
            row_numbers(setup, &s.fix),
            folds_beaten(setup, &s.fix, &dspec.fix),
            setup.folds,
            rank,
            s.spec
        );
    }
    write(dir, "pareto.tsv", &t)?;
    println!(
        "pareto: {total} non-dominated configs of {} (listed {}, ranks 1-{}), D_SPEC row first",
        all.len(),
        front.len(),
        front.iter().map(|f| f.1).max().unwrap_or(1)
    );

    // folds: per fold, the config chosen on the other folds and how it does on the held-out one
    let mut t = String::from("fold\tfixtures\tJ_train\tJ_heldout\tdspec_J_train\tdspec_J_heldout\tbeats_dspec\tspec\n");
    for k in 0..setup.folds {
        let names: Vec<&str> = (0..setup.fixtures.len())
            .filter(|&i| setup.fold[i] == k)
            .map(|i| setup.fixtures[i].name.as_str())
            .collect();
        let Some(best) = all
            .iter()
            .min_by(|a, b| setup.j_train(&a.fix, k).total_cmp(&setup.j_train(&b.fix, k)).then(a.spec.cmp(&b.spec)))
        else {
            continue;
        };
        let (jt, jh) = (setup.j_train(&best.fix, k), setup.j_fold(&best.fix, k));
        let (dt, dh) = (setup.j_train(&dspec.fix, k), setup.j_fold(&dspec.fix, k));
        t += &format!("{k}\t{}\t{jt:.6}\t{jh:.6}\t{dt:.6}\t{dh:.6}\t{}\t{}\n", names.join(","), jh < dh, best.spec);
        println!("fold {k}: held-out J {jh:.6} vs D_SPEC {dh:.6} ({}), selected on the other folds (train J {jt:.6} vs {dt:.6})", if jh < dh { "better" } else { "worse" });
    }
    write(dir, "folds.tsv", &t)?;
    Ok(front)
}

fn summary_line(setup: &Setup, scored: &[Scored], bad: usize, secs: f64) {
    let n = scored.len() + bad;
    println!(
        "evaluated {n} configs ({bad} refused) on {} fixtures in {secs:.1} s wall: configs/hour {:.0}",
        setup.fixtures.len(),
        n as f64 / secs.max(1e-9) * 3600.0
    );
}

/// The `spec` column of a tsv written by this module.
fn read_specs(path: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().ok_or(format!("{path}: empty"))?.split('\t').collect();
    let at = head.iter().position(|h| *h == "spec").ok_or(format!("{path}: no spec column"))?;
    Ok(lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let c: Vec<String> = l.split('\t').map(str::to_string).collect();
            (c[at].clone(), c)
        })
        .collect())
}

/// `sim search`.
pub fn cli(args: impl Iterator<Item = String>) -> Result<(), String> {
    let o = parse_opts(args)?;
    let space = Space::parse(&std::fs::read_to_string(&o.knobs).map_err(|e| format!("{}: {e}", o.knobs))?)?;
    let dir = o.out.clone().unwrap_or_else(|| format!("sim/reports/search-{}-{}-s{}", o.stage, o.tier, o.seed));
    let t0 = Instant::now();
    match o.stage.as_str() {
        "lhs" => {
            let setup = build_setup(&o, &o.tier)?;
            println!(
                "tier {}: {} fixtures, families {:?}, folds {:?}",
                o.tier,
                setup.fixtures.len(),
                families(&setup),
                setup.fold
            );
            let d = space.default_config();
            let dspec = evaluate_all(&setup, &space, vec![d], 1).0.pop().ok_or("D_SPEC does not run")?;
            let t1 = Instant::now();
            let (scored, bad) = evaluate_all(&setup, &space, lhs(&space, o.configs, o.seed), o.workers);
            summary_line(&setup, &scored, bad, t1.elapsed().as_secs_f64());
            report(&setup, &o, &dir, &dspec, &scored)?;
        }
        "coord" => {
            let from = o.from.as_deref().ok_or("--stage coord needs --from configs.tsv")?;
            let setup = build_setup(&o, &o.tier)?;
            let held = o.holdout % setup.folds;
            let rows = read_specs(from)?;
            // the best starts on the training folds of the held-out one, from the J_fold columns of the file
            let mut tops: Vec<(f64, &str)> = rows
                .iter()
                .skip(1)
                .filter_map(|(spec, c)| {
                    let cols: Vec<f64> = c.iter().filter_map(|x| x.parse().ok()).collect();
                    // J, cost, extra_steps, p_fail, J_unweighted, then the folds
                    let f = cols.get(5..)?;
                    (f.len() > held).then(|| (f.iter().sum::<f64>() - f[held], spec.as_str()))
                })
                .collect();
            tops.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(b.1)));
            let mut starts: Vec<Config> = vec![space.default_config()];
            for (_, spec) in tops.iter().take(o.starts) {
                starts.push(space.parse_spec(spec)?);
            }
            let per = (o.configs / starts.len()).max(1);
            let memo: Mutex<HashMap<String, Option<Vec<Fix>>>> = Mutex::new(HashMap::new());
            let eval = |c: &Config| -> Option<Vec<Fix>> {
                let spec = space.render(c);
                if let Some(v) = memo.lock().unwrap().get(&spec) {
                    return v.clone();
                }
                let r = setup.eval(&spec).ok();
                memo.lock().unwrap().insert(spec, r.clone());
                r
            };
            println!("coord: {} starts, {} evaluations each, hold out fold {held}", starts.len(), per);
            let obj = |c: &Config| eval(c).map_or(f64::INFINITY, |f| setup.j_train(&f, held));
            let runs = parallel_map(o.workers, starts.len(), &|i| {
                coordinate_descent_budget(&space, starts[i].clone(), &obj, o.sweeps, o.seed.wrapping_add(i as u64), per)
            });
            for (i, d) in runs.iter().enumerate() {
                println!(
                    "  start {i}: train J {:.6} -> {:.6} in {} sweeps, {} evals",
                    obj(&starts[i]),
                    d.j,
                    d.sweeps,
                    d.evals
                );
            }
            let memo = memo.into_inner().unwrap();
            let mut scored: Vec<Scored> =
                memo.into_iter().filter_map(|(spec, f)| Some(Scored { spec, fix: f? })).collect();
            scored.sort_by(|a, b| a.spec.cmp(&b.spec));
            let dspec = Scored {
                spec: space.render(&space.default_config()),
                fix: setup.eval(&space.render(&space.default_config()))?,
            };
            summary_line(&setup, &scored, 0, t0.elapsed().as_secs_f64());
            report(&setup, &o, &dir, &dspec, &scored)?;
        }
        "confirm" => {
            let from = o.from.clone().ok_or("--stage confirm needs --from pareto.tsv")?;
            let mut o = o;
            o.tier = "all".into();
            let setup = build_setup(&o, "all")?;
            let rows = read_specs(&from)?;
            let mut configs = vec![space.default_config()];
            for (spec, c) in &rows {
                if c.first().map(String::as_str) != Some("D_SPEC") {
                    configs.push(space.parse_spec(spec)?);
                }
            }
            let ids: Vec<String> = std::iter::once("D_SPEC".to_string())
                .chain(
                    rows.iter()
                        .filter(|(_, c)| c.first().map(String::as_str) != Some("D_SPEC"))
                        .map(|(_, c)| c[0].clone()),
                )
                .collect();
            let t1 = Instant::now();
            let (scored, bad) = evaluate_all(&setup, &space, configs, o.workers);
            summary_line(&setup, &scored, bad, t1.elapsed().as_secs_f64());
            let dspec = scored.first().ok_or("D_SPEC does not run")?;
            let mut t = format!("id\tJ_weighted\tJ_unweighted\tcost\textra_steps\tp_fail\t{}\tfolds_beaten\tbeats_dspec_all_folds\tspec\n", fold_header(&setup));
            for (id, s) in ids.iter().zip(&scored) {
                let a = setup.axes(&s.fix);
                let folds: Vec<String> = (0..setup.folds).map(|k| format!("{:.6}", setup.j_fold(&s.fix, k))).collect();
                let beaten = folds_beaten(&setup, &s.fix, &dspec.fix);
                let all = beaten == setup.folds && id != "D_SPEC";
                t += &format!(
                    "{id}\t{:.6}\t{:.6}\t{:.6}\t{:.4}\t{:.5}\t{}\t{beaten}/{}\t{}\t{}\n",
                    setup.j(&s.fix, &|_| true),
                    setup.j_flat(&s.fix, &|_| true),
                    a[0],
                    a[1],
                    a[2],
                    folds.join("\t"),
                    setup.folds,
                    if id == "D_SPEC" { "-".to_string() } else { all.to_string() },
                    s.spec
                );
                println!(
                    "{id:>6}: J weighted {:.6}, unweighted {:.6}, folds beaten {beaten}/{}{}",
                    setup.j(&s.fix, &|_| true),
                    setup.j_flat(&s.fix, &|_| true),
                    setup.folds,
                    if id == "D_SPEC" {
                        " (reference)".to_string()
                    } else if all {
                        " : beats D_SPEC on held-out J in all folds".into()
                    } else {
                        String::new()
                    }
                );
            }
            write(&dir, "confirm.tsv", &t)?;
        }
        other => return Err(format!("unknown stage {other:?} (lhs, coord, confirm)\n{USAGE}")),
    }
    println!("wrote {dir}/ in {:.1} s", t0.elapsed().as_secs_f64());
    Ok(())
}

fn families(setup: &Setup) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for f in &setup.fixtures {
        *m.entry(f.family.as_str()).or_insert(0) += 1;
    }
    m
}
