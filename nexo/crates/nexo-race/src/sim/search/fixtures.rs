use super::*;

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

    /// The families that weigh in J and have no fixture in this setup: the search never sees their load.
    pub fn uncovered(&self) -> Vec<(String, f64)> {
        self.weights
            .iter()
            .filter(|(fam, w)| **w > 0.0 && !self.fixtures.iter().any(|f| &f.family == *fam))
            .map(|(fam, w)| (fam.clone(), *w))
            .collect()
    }

    /// Whether fixture `i` is outside the fast tier, which the configs were searched on.
    pub fn unseen(&self, i: usize) -> bool {
        !FAST.contains(&self.fixtures[i].name.as_str())
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
