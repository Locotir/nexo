//! What is known about each vendor, in one place: the variable its key is read from, how its keys start,
//! the page where one is made, the other ids it goes by in the model catalogue and the provider directory,
//! the reasoning levels its protocol offers, and the model list route it has of its own. The readers
//! ([`crate::keys`], [`crate::catalogue`], [`crate::directory`], [`crate::presets`], [`crate::probe`]) ask
//! this table, so a new vendor is one record here. Prices and models are not vendor facts and stay with
//! the presets.

use crate::{anthropic, copilot, gemini};

/// Which of the vendor's own model list routes answers for it (the others list models in the
/// OpenAI-compatible way, or not at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Anthropic,
    Gemini,
}

impl ListKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ListKind::Anthropic => "anthropic",
            ListKind::Gemini => "gemini",
        }
    }
}

/// The reasoning levels a vendor's protocol takes: `any`, and `pro` for the models whose name says so
/// when those take fewer.
#[derive(Debug, Clone, Copy)]
pub struct Efforts {
    pub any: &'static [&'static str],
    pub pro: Option<&'static [&'static str]>,
}

#[derive(Debug, Clone, Copy)]
pub struct Vendor {
    /// The preset name.
    pub id: &'static str,
    /// The wire protocol that is its own (only for a vendor with a protocol of its own).
    pub protocol: Option<&'static str>,
    /// The environment variable its key is read from.
    pub key_env: Option<&'static str>,
    /// How its keys start, when that is the service's alone.
    pub key_prefix: Option<&'static str>,
    /// The page where a key is made.
    pub key_page: Option<&'static str>,
    /// The ids models.dev gives it, where they differ from `id`.
    pub catalogue_ids: &'static [&'static str],
    /// The ids of the provider directory that stand for this preset.
    pub directory_ids: &'static [&'static str],
    pub list: Option<ListKind>,
    pub efforts: Option<Efforts>,
    /// A request with reasoning off is a thinking budget of 0 (so it thinks less than one with no level).
    pub off_is_zero_budget: bool,
    /// A host whose models reason by themselves: no level is offered for an address with this in it.
    pub fixed_reasoning_host: Option<&'static str>,
}

const BLANK: Vendor = Vendor {
    id: "",
    protocol: None,
    key_env: None,
    key_prefix: None,
    key_page: None,
    catalogue_ids: &[],
    directory_ids: &[],
    list: None,
    efforts: None,
    off_is_zero_budget: false,
    fixed_reasoning_host: None,
};

/// Every vendor with a preset, plus `copilot`, which only a subscription login opens.
pub const VENDORS: &[Vendor] = &[
    Vendor {
        id: "openai",
        key_env: Some("OPENAI_API_KEY"),
        key_prefix: Some("sk-proj-"),
        key_page: Some("https://platform.openai.com/api-keys"),
        ..BLANK
    },
    Vendor {
        id: "anthropic",
        protocol: Some(anthropic::ID),
        key_env: Some("ANTHROPIC_API_KEY"),
        key_prefix: Some("sk-ant-"),
        key_page: Some("https://platform.claude.com/settings/keys"),
        list: Some(ListKind::Anthropic),
        efforts: Some(Efforts { any: &["off", "low", "medium", "high"], pro: None }),
        ..BLANK
    },
    Vendor {
        id: "gemini",
        protocol: Some(gemini::ID),
        key_env: Some("GEMINI_API_KEY"),
        key_prefix: Some("AIza"),
        key_page: Some("https://aistudio.google.com/apikey"),
        catalogue_ids: &["google"],
        directory_ids: &["google"],
        list: Some(ListKind::Gemini),
        efforts: Some(Efforts { any: &["off", "low", "medium", "high"], pro: Some(&["low", "medium", "high"]) }),
        off_is_zero_budget: true,
        ..BLANK
    },
    Vendor {
        id: "deepseek",
        key_env: Some("DEEPSEEK_API_KEY"),
        key_page: Some("https://platform.deepseek.com/api_keys"),
        fixed_reasoning_host: Some("deepseek.com"),
        ..BLANK
    },
    Vendor { id: "xai", key_env: Some("XAI_API_KEY"), ..BLANK },
    Vendor {
        id: "groq",
        key_env: Some("GROQ_API_KEY"),
        key_prefix: Some("gsk_"),
        key_page: Some("https://console.groq.com/keys"),
        ..BLANK
    },
    Vendor {
        id: "mistral",
        key_env: Some("MISTRAL_API_KEY"),
        key_page: Some("https://console.mistral.ai/api-keys"),
        ..BLANK
    },
    Vendor {
        id: "openrouter",
        key_env: Some("OPENROUTER_API_KEY"),
        key_prefix: Some("sk-or-v1-"),
        key_page: Some("https://openrouter.ai/keys"),
        ..BLANK
    },
    Vendor {
        id: "together",
        key_env: Some("TOGETHER_API_KEY"),
        key_page: Some("https://api.together.ai/settings/api-keys"),
        catalogue_ids: &["togetherai"],
        directory_ids: &["togetherai"],
        ..BLANK
    },
    Vendor {
        id: "fireworks",
        key_env: Some("FIREWORKS_API_KEY"),
        catalogue_ids: &["fireworks-ai"],
        directory_ids: &["fireworks-ai"],
        ..BLANK
    },
    Vendor { id: "cerebras", key_env: Some("CEREBRAS_API_KEY"), key_prefix: Some("csk-"), ..BLANK },
    Vendor {
        id: "moonshot",
        key_env: Some("MOONSHOT_API_KEY"),
        catalogue_ids: &["moonshotai"],
        directory_ids: &["moonshotai"],
        ..BLANK
    },
    Vendor {
        id: "qwen",
        key_env: Some("DASHSCOPE_API_KEY"),
        // models.dev also has a China-region provider that the directory does not list
        catalogue_ids: &["alibaba", "alibaba-cn"],
        directory_ids: &["alibaba"],
        ..BLANK
    },
    Vendor {
        id: "zhipu",
        key_env: Some("ZHIPU_API_KEY"),
        catalogue_ids: &["zhipuai", "zai"],
        directory_ids: &["zhipuai", "zai"],
        ..BLANK
    },
    Vendor { id: "minimax", key_env: Some("MINIMAX_API_KEY"), ..BLANK },
    Vendor { id: "ollama", ..BLANK },
    // local servers name themselves the same in the directory
    Vendor { id: "lmstudio", directory_ids: &["lmstudio"], ..BLANK },
    Vendor { id: "vllm", ..BLANK },
    // models.dev calls it `llama.cpp`; the directory has no entry for it
    Vendor { id: "llamacpp", catalogue_ids: &["llama.cpp"], ..BLANK },
    // the directory lists it as `github-copilot`; the catalogue has nothing for it
    Vendor { id: "copilot", protocol: Some(copilot::ID), directory_ids: &["github-copilot"], ..BLANK },
];

/// The vendor with this preset name.
pub fn get(id: &str) -> Option<&'static Vendor> {
    VENDORS.iter().find(|v| v.id == id)
}

/// The vendor whose protocol is this one, for the vendors that have a protocol of their own.
pub fn by_protocol(protocol: &str) -> Option<&'static Vendor> {
    VENDORS.iter().find(|v| v.protocol == Some(protocol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_ways_to_name_a_vendor_belong_to_one_vendor() {
        let mut seen = std::collections::HashSet::new();
        for v in VENDORS {
            assert!(seen.insert(v.id), "{}", v.id);
        }
        let mut names = std::collections::HashSet::new();
        for v in VENDORS {
            for n in v.catalogue_ids {
                assert!(names.insert(("catalogue", *n)), "{n}");
            }
            for n in v.directory_ids {
                assert!(names.insert(("directory", *n)), "{n}");
            }
        }
        let mut starts = std::collections::HashSet::new();
        for v in VENDORS {
            if let Some(p) = v.key_prefix {
                assert!(starts.insert(p), "{p}");
            }
        }
    }

    #[test]
    fn every_preset_has_its_record() {
        for n in crate::presets::names() {
            if n != "codex" && n != "anthropic-oauth" {
                assert!(get(n).is_some(), "{n}");
            }
        }
    }
}
