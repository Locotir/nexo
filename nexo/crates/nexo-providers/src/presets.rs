//! Vendors as presets: a protocol, a base URL, the key variable, a few
//! models and prices (per million tokens; 0 where unknown). A preset is a
//! starting point: the configuration file overrides any field.

use std::collections::BTreeMap;

use nexo_core::{Pricing, ProviderConfig};

use crate::{anthropic, copilot, directory, gemini, login, openai_chat, openai_responses};

struct Preset {
    name: &'static str,
    protocol: &'static str,
    base_url: &'static str,
    key: Option<&'static str>,
    models: &'static [(&'static str, Pricing, u32)],
    headers: &'static [(&'static str, &'static str)],
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "openai",
        protocol: openai_chat::ID,
        base_url: "https://api.openai.com/v1",
        key: Some("OPENAI_API_KEY"),
        models: &[
            ("gpt-5", Pricing { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: 0.0 }, 400_000),
            ("gpt-5-mini", Pricing { input: 0.25, output: 2.0, cache_read: 0.025, cache_write: 0.0 }, 400_000),
            ("gpt-4.1", Pricing { input: 2.0, output: 8.0, cache_read: 0.5, cache_write: 0.0 }, 1_047_576),
            ("o3", Pricing { input: 2.0, output: 8.0, cache_read: 0.5, cache_write: 0.0 }, 200_000),
            ("o4-mini", Pricing { input: 1.1, output: 4.4, cache_read: 0.275, cache_write: 0.0 }, 200_000),
        ],
        headers: &[],
    },
    Preset {
        name: "anthropic",
        protocol: anthropic::ID,
        base_url: "https://api.anthropic.com",
        key: Some("ANTHROPIC_API_KEY"),
        models: &[
            ("claude-sonnet-5-5", Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }, 200_000),
            ("claude-opus-5-5", Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }, 200_000),
            ("claude-haiku-5-5", Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }, 200_000),
            ("claude-sonnet-4-5", Pricing { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 }, 200_000),
            ("claude-haiku-4-5", Pricing { input: 1.0, output: 5.0, cache_read: 0.1, cache_write: 1.25 }, 200_000),
        ],
        headers: &[],
    },
    Preset {
        name: "gemini",
        protocol: gemini::ID,
        base_url: "https://generativelanguage.googleapis.com",
        key: Some("GEMINI_API_KEY"),
        models: &[
            ("gemini-2.5-pro", Pricing { input: 1.25, output: 10.0, cache_read: 0.31, cache_write: 0.0 }, 1_048_576),
            ("gemini-2.5-flash", Pricing { input: 0.3, output: 2.5, cache_read: 0.075, cache_write: 0.0 }, 1_048_576),
        ],
        headers: &[],
    },
    Preset {
        name: "deepseek",
        protocol: openai_chat::ID,
        base_url: "https://api.deepseek.com",
        key: Some("DEEPSEEK_API_KEY"),
        models: &[
            ("deepseek-chat", Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }, 128_000),
            ("deepseek-reasoner", Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }, 128_000),
        ],
        headers: &[],
    },
    // xAI also serves `openai-responses` (its recommended API for reasoning
    // models): set `protocol = "openai-responses"` to use it.
    Preset {
        name: "xai",
        protocol: openai_chat::ID,
        base_url: "https://api.x.ai/v1",
        key: Some("XAI_API_KEY"),
        models: &[
            ("grok-4", Pricing { input: 3.0, output: 15.0, cache_read: 0.75, cache_write: 0.0 }, 256_000),
            ("grok-4-fast", Pricing { input: 0.2, output: 0.5, cache_read: 0.05, cache_write: 0.0 }, 2_000_000),
            ("grok-code-fast-1", Pricing { input: 0.2, output: 1.5, cache_read: 0.02, cache_write: 0.0 }, 256_000),
        ],
        headers: &[],
    },
    Preset {
        name: "groq",
        protocol: openai_chat::ID,
        base_url: "https://api.groq.com/openai/v1",
        key: Some("GROQ_API_KEY"),
        models: &[
            ("openai/gpt-oss-120b", Pricing { input: 0.15, output: 0.75, cache_read: 0.0, cache_write: 0.0 }, 131_072),
            (
                "moonshotai/kimi-k2-instruct",
                Pricing { input: 1.0, output: 3.0, cache_read: 0.0, cache_write: 0.0 },
                131_072,
            ),
            (
                "llama-3.3-70b-versatile",
                Pricing { input: 0.59, output: 0.79, cache_read: 0.0, cache_write: 0.0 },
                131_072,
            ),
        ],
        headers: &[],
    },
    Preset {
        name: "mistral",
        protocol: openai_chat::ID,
        base_url: "https://api.mistral.ai/v1",
        key: Some("MISTRAL_API_KEY"),
        models: &[
            ("mistral-large-latest", Pricing { input: 2.0, output: 6.0, cache_read: 0.0, cache_write: 0.0 }, 128_000),
            ("devstral-medium-latest", Pricing { input: 0.4, output: 2.0, cache_read: 0.0, cache_write: 0.0 }, 128_000),
            ("codestral-latest", Pricing { input: 0.3, output: 0.9, cache_read: 0.0, cache_write: 0.0 }, 256_000),
        ],
        headers: &[],
    },
    Preset {
        name: "openrouter",
        protocol: openai_chat::ID,
        base_url: "https://openrouter.ai/api/v1",
        key: Some("OPENROUTER_API_KEY"),
        models: &[
            (
                "anthropic/claude-sonnet-4.5",
                Pricing { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 },
                200_000,
            ),
            ("openai/gpt-5", Pricing { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: 0.0 }, 400_000),
            (
                "deepseek/deepseek-chat-v3.1",
                Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 },
                128_000,
            ),
            ("qwen/qwen3-coder", Pricing { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }, 262_144),
        ],
        headers: &[("HTTP-Referer", "https://github.com/Locotir/nexo"), ("X-Title", "nexo")],
    },
    Preset {
        name: "together",
        protocol: openai_chat::ID,
        base_url: "https://api.together.xyz/v1",
        key: Some("TOGETHER_API_KEY"),
        models: &[
            (
                "deepseek-ai/DeepSeek-V3.1",
                Pricing { input: 0.6, output: 1.7, cache_read: 0.0, cache_write: 0.0 },
                128_000,
            ),
            (
                "Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8",
                Pricing { input: 2.0, output: 2.0, cache_read: 0.0, cache_write: 0.0 },
                262_144,
            ),
            (
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                Pricing { input: 0.88, output: 0.88, cache_read: 0.0, cache_write: 0.0 },
                131_072,
            ),
        ],
        headers: &[],
    },
    Preset {
        name: "fireworks",
        protocol: openai_chat::ID,
        base_url: "https://api.fireworks.ai/inference/v1",
        key: Some("FIREWORKS_API_KEY"),
        models: &[
            (
                "accounts/fireworks/models/kimi-k2-instruct-0905",
                Pricing { input: 0.6, output: 2.5, cache_read: 0.0, cache_write: 0.0 },
                262_144,
            ),
            (
                "accounts/fireworks/models/deepseek-v3p1",
                Pricing { input: 0.56, output: 1.68, cache_read: 0.0, cache_write: 0.0 },
                163_840,
            ),
            (
                "accounts/fireworks/models/qwen3-coder-480b-a35b-instruct",
                Pricing { input: 0.45, output: 1.8, cache_read: 0.0, cache_write: 0.0 },
                262_144,
            ),
        ],
        headers: &[],
    },
    Preset {
        name: "cerebras",
        protocol: openai_chat::ID,
        base_url: "https://api.cerebras.ai/v1",
        key: Some("CEREBRAS_API_KEY"),
        models: &[
            ("gpt-oss-120b", Pricing { input: 0.25, output: 0.69, cache_read: 0.0, cache_write: 0.0 }, 131_072),
            (
                "qwen-3-235b-a22b-instruct-2507",
                Pricing { input: 0.6, output: 1.2, cache_read: 0.0, cache_write: 0.0 },
                131_072,
            ),
            ("llama-3.3-70b", Pricing { input: 0.85, output: 1.2, cache_read: 0.0, cache_write: 0.0 }, 131_072),
        ],
        headers: &[],
    },
    Preset {
        name: "moonshot",
        protocol: openai_chat::ID,
        base_url: "https://api.moonshot.ai/v1",
        key: Some("MOONSHOT_API_KEY"),
        models: &[
            ("kimi-k2-0905-preview", Pricing { input: 0.6, output: 2.5, cache_read: 0.15, cache_write: 0.0 }, 262_144),
            (
                "kimi-k2-turbo-preview",
                Pricing { input: 1.15, output: 8.0, cache_read: 0.15, cache_write: 0.0 },
                262_144,
            ),
        ],
        headers: &[],
    },
    Preset {
        name: "qwen",
        protocol: openai_chat::ID,
        base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        key: Some("DASHSCOPE_API_KEY"),
        models: &[
            ("qwen3-coder-plus", Pricing { input: 1.0, output: 5.0, cache_read: 0.0, cache_write: 0.0 }, 1_000_000),
            ("qwen-plus", Pricing { input: 0.4, output: 1.2, cache_read: 0.0, cache_write: 0.0 }, 131_072),
            ("qwen-max", Pricing { input: 1.6, output: 6.4, cache_read: 0.0, cache_write: 0.0 }, 32_768),
        ],
        headers: &[],
    },
    Preset {
        name: "zhipu",
        protocol: openai_chat::ID,
        base_url: "https://api.z.ai/api/paas/v4",
        key: Some("ZHIPU_API_KEY"),
        models: &[
            ("glm-4.6", Pricing { input: 0.6, output: 2.2, cache_read: 0.11, cache_write: 0.0 }, 200_000),
            ("glm-4.5-air", Pricing { input: 0.2, output: 1.1, cache_read: 0.03, cache_write: 0.0 }, 128_000),
        ],
        headers: &[],
    },
    Preset {
        name: "minimax",
        protocol: openai_chat::ID,
        base_url: "https://api.minimax.io/v1",
        key: Some("MINIMAX_API_KEY"),
        models: &[("MiniMax-M2", Pricing { input: 0.3, output: 1.2, cache_read: 0.0, cache_write: 0.0 }, 204_800)],
        headers: &[],
    },
    Preset {
        name: "ollama",
        protocol: openai_chat::ID,
        base_url: "http://localhost:11434/v1",
        key: None,
        models: &[],
        headers: &[],
    },
    Preset {
        name: "lmstudio",
        protocol: openai_chat::ID,
        base_url: "http://localhost:1234/v1",
        key: None,
        models: &[],
        headers: &[],
    },
    Preset {
        name: "vllm",
        protocol: openai_chat::ID,
        base_url: "http://localhost:8000/v1",
        key: None,
        models: &[],
        headers: &[],
    },
    Preset {
        name: "llamacpp",
        protocol: openai_chat::ID,
        base_url: "http://localhost:8080/v1",
        key: None,
        models: &[],
        headers: &[],
    },
];

/// The models a ChatGPT subscription serves through Codex.
const CODEX_MODELS: &[&str] = &[
    "gpt-6.1-sol",
    "gpt-6.1-terra",
    "gpt-6-sol",
    "gpt-6-astra",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
];

/// Every vendor preset, by name: the hand-written ones plus the two that
/// only a subscription login opens (`copilot`, `codex`).
pub fn presets() -> BTreeMap<String, ProviderConfig> {
    let mut all = written();
    all.insert("copilot".into(), copilot_preset());
    all.insert("codex".into(), codex_preset());
    if let Some(x) = all.get_mut("xai") {
        x.login = Some("xai".into());
    }
    all
}

fn copilot_preset() -> ProviderConfig {
    let mut c = directory::get("github-copilot").map(|e| e.to_config()).unwrap_or_default();
    c.protocol = copilot::ID.into();
    c.base_url = copilot::BASE_URL.into();
    c.api_key_env = None;
    c.login = Some("copilot".into());
    // a subscription: nothing is billed per token
    for m in &c.models {
        c.pricing.insert(m.clone(), Pricing::default());
    }
    c
}

fn codex_preset() -> ProviderConfig {
    let mut c = ProviderConfig::new(openai_responses::ID, login::CODEX_BASE_URL, None);
    c.login = Some("codex".into());
    c.extra.insert("omit_max_tokens".into(), toml::Value::Boolean(true));
    for m in CODEX_MODELS {
        c.models.push(m.to_string());
        c.models.push(format!("{m}-fast"));
    }
    c.default_model = CODEX_MODELS.first().map(|m| m.to_string());
    c
}

fn written() -> BTreeMap<String, ProviderConfig> {
    PRESETS
        .iter()
        .map(|pr| {
            let mut cfg = ProviderConfig::new(pr.protocol, pr.base_url, pr.key);
            for (m, price, ctx) in pr.models {
                cfg.models.push(m.to_string());
                if price.is_known() {
                    cfg.pricing.insert(m.to_string(), *price);
                }
                if *ctx > 0 {
                    cfg.context_window.insert(m.to_string(), *ctx);
                }
            }
            cfg.default_model = pr.models.first().map(|m| m.0.to_string());
            for (k, v) in pr.headers {
                cfg.headers.insert(k.to_string(), v.to_string());
            }
            (pr.name.to_string(), cfg)
        })
        .collect()
}

/// The names of the presets, in the order they are listed.
pub fn names() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.name).chain(["copilot", "codex"]).collect()
}

/// A provider by name: a preset, else a directory entry (by its id or an
/// alias of a preset).
pub fn find(name: &str) -> Option<ProviderConfig> {
    let mut all = presets();
    if let Some(p) = all.remove(name) {
        return Some(p);
    }
    if let Some(p) = directory::alias(name) {
        return all.remove(p);
    }
    directory::get(name).map(|e| e.to_config())
}

/// The presets whose key is present in the environment (or that need none).
pub fn available() -> Vec<String> {
    presets().into_iter().filter(|(n, c)| c.ready(n)).map(|(n, _)| n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_complete_and_distinct() {
        let all = presets();
        assert!(all.len() >= 19);
        for (name, cfg) in &all {
            assert!(!cfg.protocol.is_empty(), "{name}");
            assert!(cfg.base_url.starts_with("http"), "{name}");
            assert!(!cfg.base_url.ends_with('/'), "{name}");
        }
        assert_eq!(all["deepseek"].api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
        assert_eq!(all["deepseek"].default_model.as_deref(), Some("deepseek-chat"));
        assert_eq!(all["deepseek"].pricing_for("deepseek-chat").unwrap().cache_read, 0.028);
        assert_eq!(all["ollama"].api_key_env, None);
        assert_eq!(all["openrouter"].headers["X-Title"], "nexo");
        assert!(names().contains(&"xai"));
        assert_eq!(all["copilot"].protocol, "copilot");
        assert_eq!(all["copilot"].login.as_deref(), Some("copilot"));
        assert!(all["copilot"].needs_key() && all["copilot"].api_key_env.is_none());
        assert!(!all["copilot"].models.is_empty());
        assert_eq!(all["codex"].base_url, "https://chatgpt.com/backend-api/codex");
        assert!(all["codex"].models.contains(&"gpt-6.1-sol-fast".to_string()));
        assert_eq!(all["xai"].login.as_deref(), Some("xai"));
    }

    #[test]
    fn a_name_finds_a_preset_an_alias_or_a_directory_entry() {
        assert_eq!(find("deepseek").unwrap().default_model.as_deref(), Some("deepseek-chat"));
        assert_eq!(find("google").unwrap().protocol, "gemini");
        let d = directory::all()
            .into_iter()
            .find(|e| find(e.id).is_some() && !presets().contains_key(e.id) && directory::alias(e.id).is_none())
            .unwrap();
        assert_eq!(find(d.id).unwrap().api_key_env.as_deref(), Some(d.key_env));
        assert!(find("no-such-vendor").is_none());
    }

    #[test]
    fn a_model_without_known_prices_has_no_pricing_entry() {
        assert!(presets()["anthropic"].pricing_for("claude-sonnet-5-5").is_none());
        assert!(!presets()["anthropic"].context_window.is_empty());
    }
}
