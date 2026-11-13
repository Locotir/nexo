use super::*;

/// A small deterministic generator for the bootstrap.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

/// A point estimate with its 95 % interval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Est {
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    /// The sample size the estimate stands on (events, pairs, fetches; see each parameter).
    pub n: f64,
    /// The interval is the rule of three (a count of zero), not a bootstrap.
    pub rule3: bool,
}

impl Est {
    pub const NONE: Est = Est { value: 0.0, lo: 0.0, hi: 1.0, n: 0.0, rule3: false };

    pub fn json(&self) -> Value {
        json!([round(self.lo), round(self.hi)])
    }

    pub fn show(&self) -> String {
        if self.n == 0.0 {
            return "n/a".to_string();
        }
        format!("{:.3} [{:.3}, {:.3}]{}", self.value, self.lo, self.hi, if self.rule3 { "*" } else { "" })
    }
}

pub(super) fn round(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Percentile interval of `stat` over `BOOT` resamples of `units` (resamples whose statistic is undefined
/// are skipped).
pub(super) fn percentile_ci<T: Copy>(units: &[T], seed: u64, stat: impl Fn(&[T]) -> Option<f64>) -> (f64, f64) {
    let mut rng = Rng::new(seed);
    let mut vals = Vec::with_capacity(BOOT);
    let mut pick: Vec<T> = Vec::with_capacity(units.len());
    for _ in 0..BOOT {
        pick.clear();
        for _ in 0..units.len() {
            pick.push(units[rng.below(units.len())]);
        }
        if let Some(v) = stat(&pick) {
            vals.push(v);
        }
    }
    if vals.is_empty() {
        return (0.0, 0.0);
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| vals[((vals.len() - 1) as f64 * p).round() as usize];
    (at(0.025), at(0.975))
}
