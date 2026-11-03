//! What is known of the model in use: prices per million tokens, the
//! context window, the largest reply. Resolved from the configuration, the
//! preset, the models.dev catalogue and the vendor's own list, in that
//! order, each filling what the previous left empty; corrected from the
//! provider's errors as they come (`turn.rs`).

use std::collections::BTreeMap;

use nexo_core::agent::AgentEvent;
use nexo_core::config::ProviderConfig;
use nexo_core::pricing::Pricing;
use nexo_core::provider::ModelInfo;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub pricing: Option<Pricing>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    /// Where the prices come from (`config`, `preset`, `catalogue`, `vendor`,
    /// `error`), or where the first number came from when there are no prices;
    /// `unknown` when nothing is known.
    pub source: String,
}

impl Limits {
    pub fn event(&self, provider: &str, model: &str) -> AgentEvent {
        AgentEvent::ModelLimits {
            provider: provider.to_string(),
            model: model.to_string(),
            pricing: self.pricing,
            context_window: self.context_window,
            max_output: self.max_output,
            source: self.source.clone(),
        }
    }

    /// `wanted` capped by the largest reply, when known.
    pub fn max_tokens(&self, wanted: u32) -> u32 {
        self.max_output.map(|m| wanted.min(m)).unwrap_or(wanted)
    }

    /// Whether the vendor's list could still add something.
    pub fn incomplete(&self) -> bool {
        self.pricing.is_none() || self.context_window.is_none() || self.max_output.is_none()
    }
}

/// `map[model]`, or the entry whose key ends with `*` and is a prefix of the model.
pub fn by_model(map: &BTreeMap<String, u32>, model: &str) -> Option<u32> {
    map.get(model)
        .copied()
        .or_else(|| map.iter().find(|(k, _)| k.ends_with('*') && model.starts_with(&k[..k.len() - 1])).map(|(_, v)| *v))
}

/// The limits of `model` under `provider`, whose configuration is `cfg`
/// (the preset's fields may already be merged into it; a value equal to the
/// preset's counts as the preset's). `vendor`: the vendor's list when fetched.
pub fn resolve(provider: &str, cfg: &ProviderConfig, model: &str, vendor: Option<&[ModelInfo]>) -> Limits {
    let presets = nexo_providers::presets();
    let preset = presets.get(provider);
    let mut l = Limits::default();
    let mut price_src: Option<&str> = None;
    let mut any_src: Option<&str> = None;
    let mut fill = |l: &mut Limits, pricing: Option<Pricing>, ctx: Option<u32>, out: Option<u32>, src: &'static str| {
        if l.pricing.is_none() && pricing.is_some() {
            l.pricing = pricing;
            price_src.get_or_insert(src);
            any_src.get_or_insert(src);
        }
        if l.context_window.is_none() && ctx.is_some() {
            l.context_window = ctx;
            any_src.get_or_insert(src);
        }
        if l.max_output.is_none() && out.is_some() {
            l.max_output = out;
            any_src.get_or_insert(src);
        }
    };
    // the configuration, told apart from the preset it may carry
    let own_price = cfg.pricing_for(model);
    let preset_price = preset.and_then(|p| p.pricing_for(model));
    let src = if own_price.is_some() && own_price == preset_price { "preset" } else { "config" };
    fill(&mut l, own_price, by_model(&cfg.context_window, model), by_model(&cfg.max_output, model), src);
    if let Some(p) = preset {
        fill(&mut l, preset_price, by_model(&p.context_window, model), by_model(&p.max_output, model), "preset");
    }
    if l.incomplete() {
        if let Some((p, c, o)) = nexo_providers::catalogue::lookup(provider, model) {
            fill(&mut l, p, c, o, "catalogue");
        }
    }
    if let (true, Some(list)) = (l.incomplete(), vendor) {
        if let Some(m) = list.iter().find(|m| m.id == model) {
            fill(&mut l, m.pricing, m.context_window, m.max_output, "vendor");
        }
    }
    l.source = price_src.or(any_src).unwrap_or("unknown").to_string();
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_wins_then_preset_then_catalogue_then_vendor() {
        // the catalogue is whatever is installed in this process
        nexo_providers::catalogue::install(
            nexo_providers::catalogue::Catalogue::parse(include_str!(
                "../../nexo-providers/tests/fixtures/models_dev_excerpt.json"
            ))
            .unwrap(),
        );
        let mut cfg = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("DEEPSEEK_API_KEY"));
        // nothing configured: the preset gives the prices, the catalogue the output limit
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!(l.source, "preset");
        assert_eq!(l.pricing.unwrap().cache_read, 0.028);
        assert_eq!(l.max_output, Some(8_192), "from the catalogue");
        // a configured price that differs from the preset's is the configuration's
        cfg.pricing
            .insert("deepseek-chat".into(), Pricing { input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0 });
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!((l.source.as_str(), l.pricing.unwrap().input), ("config", 1.0));
        // an unknown model: only the vendor's list can say
        let l = resolve("deepseek", &cfg, "deepseek-new", None);
        assert_eq!(l.source, "unknown");
        let vendor =
            vec![ModelInfo { id: "deepseek-new".into(), context_window: Some(1_000_000), ..Default::default() }];
        let l = resolve("deepseek", &cfg, "deepseek-new", Some(&vendor));
        assert_eq!((l.source.as_str(), l.context_window), ("vendor", Some(1_000_000)));
        assert_eq!(l.max_tokens(16_384), 16_384);
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!(l.max_tokens(16_384), 8_192);
    }

    #[test]
    fn wildcard_keys_match_prefixes() {
        let mut m = BTreeMap::new();
        m.insert("gpt-5*".to_string(), 400_000);
        assert_eq!(by_model(&m, "gpt-5-mini"), Some(400_000));
        assert_eq!(by_model(&m, "o3"), None);
    }
}
