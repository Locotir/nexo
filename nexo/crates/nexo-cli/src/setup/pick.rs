//! Which provider and model a run uses: presets, the pick and the model list.

use super::*;

pub struct Picked {
    pub provider: String,
    pub model: String,
    /// Named on the command line (or in `NEXO_PROVIDER` / `NEXO_MODEL`) for this launch.
    pub named: bool,
}

/// Presets are defaults: a configured provider with the same name keeps the
/// preset's fields it does not set itself.
pub fn apply_presets(cfg: &mut Config) {
    for (name, preset) in nexo_providers::presets() {
        match cfg.providers.get_mut(&name) {
            None => {
                cfg.providers.insert(name, preset);
            }
            Some(own) => {
                if own.protocol.is_empty() {
                    own.protocol = preset.protocol;
                }
                if own.base_url.is_empty() {
                    own.base_url = preset.base_url;
                }
                if own.api_key_env.is_none() {
                    own.api_key_env = preset.api_key_env;
                }
                if own.login.is_none() {
                    own.login = preset.login;
                }
                for (k, v) in preset.extra {
                    own.extra.entry(k).or_insert(v);
                }
                if own.models.is_empty() {
                    own.models = preset.models;
                }
                if own.default_model.is_none() {
                    own.default_model = preset.default_model;
                }
                for (k, v) in preset.pricing {
                    own.pricing.entry(k).or_insert(v);
                }
                for (k, v) in preset.context_window {
                    own.context_window.entry(k).or_insert(v);
                }
                for (k, v) in preset.max_output {
                    own.max_output.entry(k).or_insert(v);
                }
                for (k, v) in preset.headers {
                    own.headers.entry(k).or_insert(v);
                }
            }
        }
    }
    crate::account::add_directory(cfg);
}

/// Nothing was chosen and nothing can be used: no provider set, no key found. The fallback
/// `pick` makes is then not the person's choice and must not be named to them.
pub fn nothing_chosen(cfg: &Config) -> bool {
    let chosen = cfg.provider.as_ref().is_some_and(|p| cfg.providers.contains_key(p));
    !chosen && !cfg.providers.iter().any(|(n, p)| p.needs_key() && p.ready(n))
}

/// The wording of `nexo doctor`, for a run that cannot ask.
/// What the agent starts on while the person has not chosen: the fallback `pick` makes, never shown.
pub fn placeholder(cfg: &Config) -> Picked {
    if let Some((provider, model)) = cfg.pick() {
        return Picked { provider, model, named: false };
    }
    Picked { provider: cfg.providers.keys().next().cloned().unwrap_or_default(), model: String::new(), named: false }
}

pub fn nothing_chosen_message() -> String {
    format!(
        "no provider chosen and no key found\nset a key (e.g. {}) or run `nexo auth login <provider>`\nthen `nexo use <provider>`; `nexo guide` lists every way",
        crate::account::KEY_IN_ENV
    )
}

pub fn pick(cfg: &Config, provider: Option<&str>, model: Option<&str>) -> anyhow::Result<Picked> {
    pick_with(cfg, provider, model, true)
}

/// `pick`, with `need_key` false for what only reads the catalogue (`nexo pricing`): the key is not asked for.
pub(super) fn pick_with(
    cfg: &Config,
    provider: Option<&str>,
    model: Option<&str>,
    need_key: bool,
) -> anyhow::Result<Picked> {
    let (provider_given, model_given) = (provider.is_some(), model.is_some());
    let mut c = cfg.clone();
    if let Some(p) = provider {
        c.provider = Some(p.to_string());
        if !c.providers.contains_key(p) {
            anyhow::bail!("unknown provider {p:?}; `nexo providers` lists them");
        }
    }
    if let Some(m) = model {
        // `--model codex/luna` names its provider, as `/model codex/luna` does (unless no provider
        // was asked for by that name and the model id itself holds the slash, as on a gateway)
        match m.split_once('/') {
            Some((p, rest)) if provider.is_none() && c.providers.contains_key(p) && !on_gateway(&c, m) => {
                c.provider = Some(p.to_string());
                c.model = Some(rest.to_string());
            }
            _ => c.model = Some(m.to_string()),
        }
    }
    let (provider, model) =
        c.pick().ok_or_else(|| {
            anyhow::anyhow!(
                "no provider can be used: set a key (e.g. {}) or run `nexo auth login <provider>`; `nexo guide` explains every way",
                crate::account::KEY_IN_ENV
            )
        })?;
    // the managed policy has the last word on which provider and model run
    cfg.check_model(&provider, &model).map_err(|e| anyhow::anyhow!(e))?;
    let pc = &c.providers[&provider];
    if need_key && !pc.ready(&provider) {
        anyhow::bail!(
            "provider {provider:?} needs a key: {} (`nexo doctor` checks the setup)",
            crate::account::how_to_unlock(&provider, pc)
        );
    }
    Ok(Picked { provider, model, named: provider_given || model_given })
}

/// Whether the provider that would run holds a model whose id is `m` itself, slash and all.
pub(super) fn on_gateway(c: &Config, m: &str) -> bool {
    c.provider
        .as_ref()
        .and_then(|p| c.providers.get(p))
        .is_some_and(|p| p.models.iter().chain(p.default_model.iter()).any(|x| x == m))
}

pub fn model_choices(cfg: &Config) -> Vec<(String, String)> {
    nexo_providers::modellist::choices(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut c = Config { provider: Some("refuseless".into()), ..Default::default() };
        for n in ["refuseless", "codex"] {
            c.providers.insert(n.into(), ProviderConfig::new("openai-chat", "http://localhost:1/v1", None));
        }
        c.providers.get_mut("refuseless").unwrap().models = vec!["openai/gpt-x".into()];
        c
    }

    fn at(c: &Config, provider: Option<&str>, model: &str) -> (String, String) {
        let p = pick(c, provider, Some(model)).unwrap();
        (p.provider, p.model)
    }

    #[test]
    fn a_model_flag_that_names_its_provider_runs_on_it() {
        let c = cfg();
        assert_eq!(at(&c, None, "codex/luna"), ("codex".into(), "luna".into()));
        // a provider asked for apart wins; a slash that is part of the id stays in it
        assert_eq!(at(&c, Some("refuseless"), "codex/luna"), ("refuseless".into(), "codex/luna".into()));
        assert_eq!(at(&c, None, "plain"), ("refuseless".into(), "plain".into()));
        let mut g = c.clone();
        g.providers.get_mut("refuseless").unwrap().models.push("codex/gw".into());
        assert_eq!(at(&g, None, "codex/gw"), ("refuseless".into(), "codex/gw".into()));
    }
}
