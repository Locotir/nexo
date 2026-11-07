//! `/fast`: the fast version of a model. Some providers list a twin of a model under the same
//! name plus a suffix (`deepseek-v4.1-flash` and `deepseek-v4.1-flash-fast`); others take speed as
//! an option of the request (`service_tier`), which [`option`] names for the providers where that
//! is known.

use crate::ProviderConfig;

/// The suffixes of a fast twin, best first: with several twins listed, the earliest wins.
const SUFFIXES: &[&str] = &["-fast", "_fast", "-highspeed", "-ultraspeed", "-turbo"];

/// What `/fast` finds for a model among the names a provider lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Twin {
    /// The model is a base; this is its fast version.
    Fast(String),
    /// The model is itself a fast version; this is the base.
    Base(String),
    /// Neither is listed.
    None,
}

/// The names `name` would have as a fast twin's base (lowercase), one for each suffix it ends with.
fn bases(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let mut out: Vec<String> = SUFFIXES
        .iter()
        .filter_map(|s| lower.strip_suffix(&s.to_ascii_lowercase()))
        .filter(|b| !b.is_empty())
        .map(String::from)
        .collect();
    // `glm-5.3-flash` and `glm-5.3-flashx`
    if let Some(b) = lower.strip_suffix('x').filter(|b| b.ends_with("flash")) {
        out.push(b.to_string());
    }
    out
}

/// The names `name` would have as a fast twin, best first (lowercase).
fn fasts(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    let mut out: Vec<String> = SUFFIXES.iter().map(|s| format!("{lower}{s}")).collect();
    if lower.ends_with("flash") {
        out.push(format!("{lower}x"));
    }
    out
}

/// The twin of `current` among `listed`, matched without regard to case and returned as the
/// provider spells it. A name that is a fast version of a listed model goes back to that base;
/// otherwise its fast version is looked up, `-fast` first.
pub fn twin<'a>(current: &str, listed: impl IntoIterator<Item = &'a str>) -> Twin {
    let listed: Vec<&str> = listed.into_iter().collect();
    let find = |lower: &str| listed.iter().find(|m| m.to_ascii_lowercase() == lower).map(|m| m.to_string());
    if let Some(b) = bases(current).iter().find_map(|b| find(b)) {
        return Twin::Base(b);
    }
    match fasts(current).iter().find_map(|f| find(f)) {
        Some(f) => Twin::Fast(f),
        None => Twin::None,
    }
}

/// The request option that makes a provider answer faster, `(key, value)`, where it is known
/// and no other name is needed: the ChatGPT subscription (`codex`, a `service_tier` of
/// `priority`: codex-rs/protocol `ServiceTier::Fast.request_value()`) and the OpenAI API
/// (`service_tier=priority` for Responses and Chat Completions, its "fast mode").
pub fn option(cfg: &ProviderConfig) -> Option<(&'static str, &'static str)> {
    let openai = cfg.base_url.trim_end_matches('/').starts_with("https://api.openai.com/")
        && matches!(cfg.protocol.as_str(), "openai-responses" | "openai-chat");
    (cfg.login.as_deref() == Some("codex") || openai).then_some(("service_tier", "priority"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(current: &str, listed: &[&str]) -> Twin {
        twin(current, listed.iter().copied())
    }

    #[test]
    fn a_base_finds_its_fast_version_and_back() {
        let l = ["deepseek-v4.1-flash", "deepseek-v4.1-flash-fast", "deepseek-v4-pro"];
        assert_eq!(t("deepseek-v4.1-flash", &l), Twin::Fast("deepseek-v4.1-flash-fast".into()));
        assert_eq!(t("deepseek-v4.1-flash-fast", &l), Twin::Base("deepseek-v4.1-flash".into()));
        assert_eq!(t("deepseek-v4-pro", &l), Twin::None);
    }

    #[test]
    fn case_is_ignored_and_the_providers_spelling_kept() {
        let l = ["zai-org/GLM-5.2", "zai-org/GLM-5.2-Fast"];
        assert_eq!(t("zai-org/glm-5.2", &l), Twin::Fast("zai-org/GLM-5.2-Fast".into()));
        assert_eq!(t("zai-org/GLM-5.2-Fast", &l), Twin::Base("zai-org/GLM-5.2".into()));
        let l = ["DeepSeek-V4.1-Flash", "DeepSeek-V4.1-Flash-Fast"];
        assert_eq!(t("DeepSeek-V4.1-Flash", &l), Twin::Fast("DeepSeek-V4.1-Flash-Fast".into()));
    }

    #[test]
    fn other_suffixes_are_twins_too() {
        assert_eq!(
            t("minimax-m2.7", &["minimax-m2.7", "minimax-m2.7-highspeed"]),
            Twin::Fast("minimax-m2.7-highspeed".into())
        );
        assert_eq!(
            t("mimo-v2.6-pro-ultraspeed", &["mimo-v2.6-pro", "mimo-v2.6-pro-ultraspeed"]),
            Twin::Base("mimo-v2.6-pro".into())
        );
        assert_eq!(t("m", &["m", "m_fast"]), Twin::Fast("m_fast".into()));
        assert_eq!(t("m", &["m", "m-turbo"]), Twin::Fast("m-turbo".into()));
        assert_eq!(t("glm-5.3-flash", &["glm-5.3-flash", "glm-5.3-flashx"]), Twin::Fast("glm-5.3-flashx".into()));
        assert_eq!(t("glm-5.3-flashx", &["glm-5.3-flash", "glm-5.3-flashx"]), Twin::Base("glm-5.3-flash".into()));
    }

    #[test]
    fn with_several_twins_fast_wins() {
        let l = ["m", "m-turbo", "m-highspeed", "m-fast"];
        assert_eq!(t("m", &l), Twin::Fast("m-fast".into()));
        let l = ["m", "m-turbo", "m-highspeed"];
        assert_eq!(t("m", &l), Twin::Fast("m-highspeed".into()));
    }

    #[test]
    fn a_suffix_without_its_base_is_a_model_of_its_own() {
        assert_eq!(t("grok-4-fast", &["grok-4-fast", "grok-4"]), Twin::Base("grok-4".into()));
        assert_eq!(t("grok-code-fast-1", &["grok-code-fast-1"]), Twin::None);
        assert_eq!(t("m-fast", &["m-fast"]), Twin::None);
        assert_eq!(t("m", &[]), Twin::None);
        // `flash` alone is not a fast version of anything
        assert_eq!(t("flash", &["flash"]), Twin::None);
    }

    #[test]
    fn the_request_option_is_known_for_the_subscription_and_the_openai_api() {
        let mut c = ProviderConfig::new("openai-responses", "https://chatgpt.com/backend-api/codex", None);
        assert_eq!(option(&c), None);
        c.login = Some("codex".into());
        assert_eq!(option(&c), Some(("service_tier", "priority")));
        assert_eq!(
            option(&ProviderConfig::new("openai-chat", "https://api.openai.com/v1", None)),
            Some(("service_tier", "priority"))
        );
        assert_eq!(option(&ProviderConfig::new("openai-chat", "https://api.deepseek.com/v1", None)), None);
        assert_eq!(option(&ProviderConfig::new("anthropic", "https://api.openai.com/v1", None)), None);
    }
}
