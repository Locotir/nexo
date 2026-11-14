//! Local, non-secret planning facts for an ultracode request. Never resolve credentials here:
//! a key command may have side effects, and readiness is not proof that an endpoint works.

use nexo_core::config::Config;
use nexo_providers::usage::{self, PlanUsage};

pub(super) fn facts(config: &Config, current_provider: &str, current_model: &str, now: i64) -> String {
    render(config, current_provider, current_model, now, usage::get)
}

fn render(
    config: &Config,
    current_provider: &str,
    current_model: &str,
    now: i64,
    cached: impl Fn(&str) -> Option<PlanUsage>,
) -> String {
    let mut out =
        vec!["[Ultracode planning facts — local snapshot, not a connectivity or billing guarantee]".to_string()];
    if config.providers.is_empty() {
        out.push("Configured providers: unknown (none in configuration).".into());
    }
    for (name, cfg) in &config.providers {
        let auth = if cfg.login.is_some() {
            "subscription login configured"
        } else {
            "API/key or local endpoint (billing type unknown)"
        };
        let readiness = if name == current_provider {
            "current session provider; connectivity not verified"
        } else {
            "readiness unknown"
        };
        out.push(format!("Provider {name:?}: {auth}; {readiness}."));
        let mut models = cfg.models.clone();
        if let Some(m) = &cfg.default_model {
            models.push(m.clone());
        }
        if name == current_provider {
            models.push(current_model.to_string());
        }
        models.sort();
        models.dedup();
        if models.is_empty() {
            out.push("  Models: unknown.".into());
        }
        for model in models {
            let l = crate::limits::resolve(name, cfg, &model, None);
            match l.pricing {
                Some(p) => out.push(format!("  Model {model:?}: reference USD/million tokens input {}, output {}, cache read {}, cache write {} (source: {}; cache write 0 uses input rate). Actual subscription charges unknown.", p.input, p.output, p.cache_read, p.cache_write, l.source)),
                None => out.push(format!("  Model {model:?}: price unknown.")),
            }
        }
        match usage::kind_of(cfg).and_then(&cached) {
            Some(plan) => {
                out.push(format!("  Cached subscription quota: observed at unix {}; cache is shared by login kind, account attribution unverified.", plan.seen));
                if plan.meters.is_empty() {
                    out.push("  Quota remaining: unknown (no meters).".into());
                }
                for meter in &plan.meters {
                    if meter.is_stale(now) {
                        out.push(format!("  {:?}: unknown (cached reading has passed reset).", meter.label));
                    } else {
                        let counts = meter
                            .counts
                            .map(|(used, total)| format!(", {} of {} remaining", total.saturating_sub(used), total))
                            .unwrap_or_default();
                        out.push(format!(
                            "  {:?}: {:.1}% used{counts}; reset unix {}.",
                            meter.label,
                            meter.used_pct,
                            meter.resets_at.map(|v| v.to_string()).unwrap_or_else(|| "unknown".into())
                        ));
                    }
                }
            }
            None => out.push("  Subscription quota: unknown (no applicable cached reading).".into()),
        }
    }
    out.push("Estimates require token assumptions; unknown prices, availability, billing and quota must not be invented. Cached readings may be outdated.".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::config::ProviderConfig;
    use nexo_core::pricing::Pricing;
    use nexo_providers::usage::Meter;

    #[test]
    fn facts_are_allowlisted_and_mark_stale_and_missing_data() {
        let mut cfg = Config::default();
        cfg.providers.clear();
        let mut p =
            ProviderConfig { login: Some("codex".into()), models: vec!["model-x".into()], ..Default::default() };
        p.headers.insert("Authorization".into(), "secret-header".into());
        p.api_key_cmd = Some("secret-command".into());
        p.pricing.insert("model-x".into(), Pricing { input: 2.0, output: 4.0, ..Default::default() });
        cfg.providers.insert("example".into(), p);
        cfg.providers.insert("other".into(), ProviderConfig::default());
        let plan = PlanUsage {
            seen: 10,
            meters: vec![
                Meter {
                    slot: "a".into(),
                    label: "fresh".into(),
                    used_pct: 25.0,
                    counts: Some((25, 100)),
                    window_minutes: None,
                    resets_at: Some(200),
                },
                Meter {
                    slot: "b".into(),
                    label: "old".into(),
                    used_pct: 90.0,
                    counts: None,
                    window_minutes: None,
                    resets_at: Some(50),
                },
            ],
            ..Default::default()
        };
        let s = render(&cfg, "example", "model-x", 100, |_| Some(plan.clone()));
        assert!(s.contains("75 of 100 remaining"));
        assert!(s.contains("cached reading has passed reset"));
        assert!(s.contains("input 2, output 4"));
        assert!(s.contains("readiness unknown"));
        assert!(s.contains("Models: unknown"));
        assert!(!s.contains("secret-header") && !s.contains("secret-command"));
    }
}
