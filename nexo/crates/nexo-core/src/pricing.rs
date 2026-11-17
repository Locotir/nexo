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
    /// The dearer rates a request pays when its prompt is longer than `long.above` tokens (none: one rate at any size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<LongTier>,
}

/// Prices per million tokens that replace the base ones for a whole request whose prompt (uncached input plus
/// cache reads and writes) is longer than `above` tokens, as Anthropic and Google bill a long prompt: the rate of
/// the tier applies to every token of that request, not only to the part over the line. A write to the 1-hour
/// cache in such a request costs twice the tier's `input`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LongTier {
    pub above: u32,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
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

    /// The rates that price a request with this usage: the long tier's when the prompt is past its line, else these.
    fn at(&self, u: &Usage) -> Pricing {
        match self.long {
            Some(t) if u.input + u.cache_read + u.cache_write > t.above as u64 => Pricing {
                input: t.input,
                output: t.output,
                cache_read: t.cache_read,
                cache_write: t.cache_write,
                cache_write_1h: 0.0,
                long: None,
            },
            _ => *self,
        }
    }

    pub fn cost(&self, u: &Usage) -> f64 {
        let p = self.at(u);
        // `cache_write_1h` is the part of `cache_write` that was written for an hour
        let hour = u.cache_write_1h.min(u.cache_write);
        (u.input as f64 * p.input
            + u.output as f64 * p.output
            + u.cache_read as f64 * p.cache_read
            + (u.cache_write - hour) as f64 * p.write_5m()
            + hour as f64 * p.write_1h())
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
        let p = Pricing { input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0, cache_write_1h: 0.0, long: None };
        let u = Usage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 1_000_000,
            cache_write_1h: 0,
            transformations: 0,
            reasoning: 0,
        };
        assert!((p.cost(&u) - (1.0 + 1.0 + 0.2 + 1.0)).abs() < 1e-9);
    }

    fn claude_like() -> Pricing {
        Pricing { input: 2.0, output: 10.0, cache_read: 0.2, cache_write: 2.5, cache_write_1h: 0.0, long: None }
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

    fn tiered() -> Pricing {
        let long = LongTier { above: 200_000, input: 6.0, output: 22.5, cache_read: 0.6, cache_write: 7.5 };
        Pricing { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75, long: Some(long), ..Default::default() }
    }

    #[test]
    fn a_prompt_past_the_line_pays_the_long_rates_for_the_whole_request() {
        let u = Usage { input: 100_000, output: 1_000_000, cache_read: 100_001, ..Default::default() };
        // 200,001 prompt tokens: everything at the tier, not only the 1 token over
        assert!((tiered().cost(&u) - (0.6 + 22.5 + 0.0600006)).abs() < 1e-9);
    }

    #[test]
    fn a_prompt_at_the_line_or_under_pays_the_base_rates() {
        let u = Usage { input: 100_000, output: 1_000_000, cache_read: 100_000, ..Default::default() };
        assert!((tiered().cost(&u) - (0.3 + 15.0 + 0.03)).abs() < 1e-9);
    }

    #[test]
    fn cache_writes_count_toward_the_prompt_size_and_pay_the_tier_rate() {
        let u = Usage { cache_write: 250_000, ..Default::default() };
        assert!((tiered().cost(&u) - 250_000.0 * 7.5 / 1e6).abs() < 1e-9);
    }

    #[test]
    fn a_price_without_a_tier_is_written_and_read_as_before() {
        let text = serde_json::to_string(&claude_like()).unwrap();
        assert!(!text.contains("long"), "{text}");
        assert_eq!(serde_json::from_str::<Pricing>(&text).unwrap(), claude_like());
        let again: Pricing = serde_json::from_str(&serde_json::to_string(&tiered()).unwrap()).unwrap();
        assert_eq!(again, tiered());
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
        let p = Pricing { input: 1.0, output: 4.0, cache_read: 0.1, cache_write: 0.0, cache_write_1h: 0.0, long: None };
        assert!(!p.long_cache_pays());
        assert!(!Pricing::default().long_cache_pays());
    }
}
