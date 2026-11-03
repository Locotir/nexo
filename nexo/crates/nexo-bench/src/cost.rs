//! What a run costs: the provider's token counts priced per million.

use nexo_core::{Pricing, Usage};

/// DeepSeek's list prices ($ per million: input on a cache miss, cache hit, output). It charges
/// nothing extra for a cache write, so those tokens are billed as input (`cache_write` 0 here).
pub const DEEPSEEK: Pricing = Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 };

/// The prices of `model`: the DeepSeek models, then whatever a nexo provider preset prices. None for
/// a model nexo has no prices for: its cost is unknown, never guessed from another vendor.
pub fn price_of(model: &str) -> Option<Pricing> {
    if matches!(model, "deepseek-chat" | "deepseek-reasoner") {
        return Some(DEEPSEEK);
    }
    nexo_providers::presets().values().find_map(|p| p.pricing_for(model).filter(|x| x.is_known()))
}

/// The cost of `usage` at `price`; `input` counts only the tokens that missed the cache.
pub fn cost(price: &Pricing, usage: &Usage) -> f64 {
    price.cost(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_chat_is_priced_per_million_by_kind() {
        let p = price_of("deepseek-chat").unwrap();
        let u = Usage { input: 1_000_000, cache_read: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!((cost(&p, &u) - 0.728).abs() < 1e-9);
    }

    #[test]
    fn a_cache_hit_costs_a_tenth_of_a_miss() {
        let p = price_of("deepseek-chat").unwrap();
        let miss = cost(&p, &Usage { input: 10_000, ..Default::default() });
        let hit = cost(&p, &Usage { cache_read: 10_000, ..Default::default() });
        assert!((miss - 10.0 * hit).abs() < 1e-12);
    }

    #[test]
    fn an_unknown_model_has_no_price_instead_of_a_deepseek_one() {
        assert_eq!(price_of("no-such-model-7"), None);
    }

    #[test]
    fn a_cache_write_is_billed_as_input_unless_the_vendor_prices_it() {
        let w = Usage { cache_write: 1_000_000, ..Default::default() };
        assert!((cost(&DEEPSEEK, &w) - 0.28).abs() < 1e-9);
        let own = Pricing { cache_write: 1.25, ..DEEPSEEK };
        assert!((cost(&own, &w) - 1.25).abs() < 1e-9);
    }
}
