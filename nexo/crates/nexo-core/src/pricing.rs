//! Prices per million tokens and the cost of a request.

use serde::{Deserialize, Serialize};

use crate::stream::Usage;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    /// Uncached input, per million tokens, in dollars.
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    /// Writing to the cache (0 where the vendor charges nothing extra; then
    /// `cache_write` tokens are priced as `input`).
    #[serde(default)]
    pub cache_write: f64,
}

impl Pricing {
    pub fn cost(&self, u: &Usage) -> f64 {
        let write = if self.cache_write > 0.0 { self.cache_write } else { self.input };
        (u.input as f64 * self.input
            + u.output as f64 * self.output
            + u.cache_read as f64 * self.cache_read
            + u.cache_write as f64 * write)
            / 1_000_000.0
    }

    pub fn is_known(&self) -> bool {
        self.input > 0.0 || self.output > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_the_four_counts() {
        let p = Pricing { input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0 };
        let u =
            Usage { input: 1_000_000, output: 500_000, cache_read: 2_000_000, cache_write: 1_000_000, reasoning: 0 };
        assert!((p.cost(&u) - (1.0 + 1.0 + 0.2 + 1.0)).abs() < 1e-9);
    }
}
