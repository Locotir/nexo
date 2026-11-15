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
    /// Writing to the 1-hour cache, for the vendors that sell one (0: twice `input`, what the one that does charges).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_1h: f64,
}

fn is_zero(n: &f64) -> bool {
    *n == 0.0
}

impl Pricing {
    /// Per million tokens, what a 5-minute cache write costs.
    pub fn write_5m(&self) -> f64 {
        if self.cache_write > 0.0 {
            self.cache_write
        } else {
            self.input
        }
    }

    /// Per million tokens, what a 1-hour cache write costs.
    pub fn write_1h(&self) -> f64 {
        if self.cache_write_1h > 0.0 {
            self.cache_write_1h
        } else {
            2.0 * self.input
        }
    }

    /// Whether writing the cache for 1 hour instead of 5 minutes pays when one pause longer than 5 minutes is
    /// expected: the dearer write must cost less extra than the re-write of the whole prefix that a pause leaves
    /// instead of a read. Without prices there is nothing to weigh, so the answer is no.
    pub fn long_cache_pays(&self) -> bool {
        self.is_known() && self.write_1h() - self.write_5m() < self.write_5m() - self.cache_read
    }

    pub fn cost(&self, u: &Usage) -> f64 {
        // `cache_write_1h` is the part of `cache_write` that was written for an hour
        let long = u.cache_write_1h.min(u.cache_write);
        (u.input as f64 * self.input
            + u.output as f64 * self.output
            + u.cache_read as f64 * self.cache_read
            + (u.cache_write - long) as f64 * self.write_5m()
            + long as f64 * self.write_1h())
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
        let p = Pricing { input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0, cache_write_1h: 0.0 };
        let u = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 1_000_000,
            cache_write_1h: 0,
            reasoning: 0,
        };
        assert!((p.cost(&u) - (1.0 + 1.0 + 0.2 + 1.0)).abs() < 1e-9);
    }

    fn claude_like() -> Pricing {
        Pricing { input: 2.0, output: 10.0, cache_read: 0.2, cache_write: 2.5, cache_write_1h: 0.0 }
    }

    #[test]
    fn prices_the_one_hour_part_of_the_writes_apart() {
        let p = claude_like();
        let u = Usage { cache_write: 1_000_000, cache_write_1h: 400_000, ..Default::default() };
        // 600k at 2.5 and 400k at twice the input price
        assert!((p.cost(&u) - (1.5 + 1.6)).abs() < 1e-9);
        let own = Pricing { cache_write_1h: 5.0, ..p };
        assert!((own.cost(&u) - (1.5 + 2.0)).abs() < 1e-9);
    }

    #[test]
    fn a_count_without_the_split_costs_what_it_did() {
        let p = claude_like();
        let u = Usage { cache_write: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn the_long_cache_pays_when_its_extra_is_less_than_one_miss() {
        // extra 1.5 for the hour write, a miss re-writes at 2.5 instead of reading at 0.2
        assert!(claude_like().long_cache_pays());
        // a cheaper read makes a miss dearer, so it pays more
        assert!(Pricing { cache_read: 0.05, ..claude_like() }.long_cache_pays());
        // an hour write at 4.9 costs 2.4 extra against a miss of 2.3
        assert!(!Pricing { cache_write_1h: 4.9, ..claude_like() }.long_cache_pays());
    }

    #[test]
    fn a_vendor_with_free_caching_keeps_the_short_one() {
        // no write premium: the 5-minute write is the input price, the hour write twice it
        let p = Pricing { input: 1.0, output: 4.0, cache_read: 0.1, cache_write: 0.0, cache_write_1h: 0.0 };
        assert!(!p.long_cache_pays());
        assert!(!Pricing::default().long_cache_pays());
    }
}
