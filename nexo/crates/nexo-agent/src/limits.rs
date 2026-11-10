//! What is known of the model in use: prices per million tokens, the
//! context window, the largest reply. Resolved from the configuration, the
//! preset, the models.dev catalogue and the vendor's own list, in that
//! order, each filling what the previous left empty; corrected from the
//! provider's errors as they come (`turn.rs`).

use std::collections::BTreeMap;

use nexo_core::agent::AgentEvent;
use nexo_core::config::{ProviderConfig, DEFAULT_MAX_TOKENS};
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

    /// `wanted` capped by the largest reply, when known, and by a quarter of the window: a reply as large as
    /// a small window (16k asked of an 8k local model) would leave no room for the conversation.
    pub fn max_tokens(&self, wanted: u32) -> u32 {
        let wanted = self.max_output.map(|m| wanted.min(m)).unwrap_or(wanted);
        self.context_window.map(|w| wanted.min((w / 4).max(256))).unwrap_or(wanted)
    }

    /// The reply size to ask for. A thinking model's reasoning counts inside the reply, and a reply cut while
    /// it is still thinking is thrown away and paid twice, so when the window is 16 times the largest reply
    /// (room costs nothing) and `configured` is the default or more, ask for the largest reply from the first
    /// request. Under the default `configured` is the person's own cap and is kept. `boost` doubles what is
    /// asked after a cut reply, as before.
    pub fn reply_room(&self, configured: u32, boost: u32) -> u32 {
        if configured >= DEFAULT_MAX_TOKENS {
            if let (Some(w), Some(m)) = (self.context_window, self.max_output) {
                if m > 0 && u64::from(w) >= 16 * u64::from(m) {
                    return self.max_tokens(m);
                }
            }
        }
        self.max_tokens(configured.saturating_mul(boost.max(1)))
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
    let mut capped = false;
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
    if let Some(cap) = cfg.window_cap.or_else(|| preset.and_then(|p| p.window_cap)) {
        if l.context_window.is_none_or(|w| w > cap) {
            l.context_window = Some(cap);
            capped = true;
        }
    }
    l.source = price_src.or(any_src).unwrap_or("unknown").to_string();
    if capped {
        l.source = if l.source == "unknown" {
            "provider cap".into()
        } else {
            format!("{}, window capped by the provider", l.source)
        };
    }
    l
}

/// `old` with its empty fields taken from `new`; None when that adds nothing.
pub fn filled(old: &Limits, new: Limits) -> Option<Limits> {
    let mut l = old.clone();
    l.pricing = l.pricing.or(new.pricing);
    l.context_window = l.context_window.or(new.context_window);
    l.max_output = l.max_output.or(new.max_output);
    if l == *old {
        return None;
    }
    if l.source == "unknown" {
        l.source = new.source;
    }
    Some(l)
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
    fn a_provider_cap_limits_the_window_its_backend_allows() {
        nexo_providers::catalogue::install(
            nexo_providers::catalogue::Catalogue::parse(include_str!(
                "../../nexo-providers/tests/fixtures/models_dev_excerpt.json"
            ))
            .unwrap(),
        );
        let mut cfg = ProviderConfig::new("openai-chat", "https://api.deepseek.com", Some("DEEPSEEK_API_KEY"));
        // no cap: the catalogue's window (128000) stays
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!(l.context_window, Some(128_000));
        assert!(!l.source.contains("capped"), "{}", l.source);
        // above the cap: capped, and the source says so; the prices are untouched
        let price = l.pricing;
        cfg.window_cap = Some(100_000);
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!(l.context_window, Some(100_000));
        assert!(l.source.ends_with("window capped by the provider"), "{}", l.source);
        assert_eq!(l.pricing, price);
        // below the cap: kept
        cfg.window_cap = Some(200_000);
        let l = resolve("deepseek", &cfg, "deepseek-chat", None);
        assert_eq!(l.context_window, Some(128_000));
        assert!(!l.source.contains("capped"), "{}", l.source);
        // a window nobody knows becomes the cap (the backend refuses more)
        let l = resolve("deepseek", &cfg, "deepseek-unlisted", None);
        assert_eq!((l.context_window, l.source.as_str()), (Some(200_000), "provider cap"));
        // a smaller window the person set stays
        cfg.context_window.insert("deepseek-chat".into(), 50_000);
        assert_eq!(resolve("deepseek", &cfg, "deepseek-chat", None).context_window, Some(50_000));
    }

    #[test]
    fn a_late_catalogue_fills_only_the_gaps() {
        let old = Limits { context_window: Some(100), source: "vendor".into(), ..Default::default() };
        let new =
            Limits { context_window: Some(5), max_output: Some(7), source: "catalogue".into(), ..Default::default() };
        let l = filled(&old, new.clone()).unwrap();
        assert_eq!((l.context_window, l.max_output, l.source.as_str()), (Some(100), Some(7), "vendor"));
        let cold = Limits { source: "unknown".into(), ..Default::default() };
        assert_eq!(filled(&cold, new).unwrap().source, "catalogue");
        assert!(filled(&old, Limits::default()).is_none());
    }

    #[test]
    fn wildcard_keys_match_prefixes() {
        let mut m = BTreeMap::new();
        m.insert("gpt-5*".to_string(), 400_000);
        assert_eq!(by_model(&m, "gpt-5-mini"), Some(400_000));
        assert_eq!(by_model(&m, "o3"), None);
    }

    #[test]
    fn a_small_window_keeps_three_quarters_for_the_conversation() {
        let l = Limits { context_window: Some(8_000), ..Default::default() };
        assert_eq!(l.max_tokens(16_384), 2_000);
        let l = Limits { context_window: Some(1_000_000), max_output: Some(8_192), ..Default::default() };
        assert_eq!(l.max_tokens(16_384), 8_192);
        assert_eq!(Limits::default().max_tokens(16_384), 16_384);
    }

    #[test]
    fn a_big_window_asks_for_the_largest_reply_at_once() {
        let big = Limits { context_window: Some(1_048_576), max_output: Some(32_768), ..Default::default() };
        assert_eq!(big.reply_room(16_384, 1), 32_768);
        let mid = Limits { context_window: Some(128_000), max_output: Some(32_768), ..Default::default() };
        assert_eq!(mid.reply_room(16_384, 1), 16_384);
        assert_eq!(mid.reply_room(16_384, 2), 32_000);
        assert_eq!(big.reply_room(4_096, 1), 4_096);
        assert_eq!(big.reply_room(4_096, 2), 8_192);
        assert_eq!(Limits::default().reply_room(16_384, 2), 32_768);
    }
}
