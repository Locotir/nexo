//! The provider directory: some two hundred vendors from models.dev, each a
//! protocol, a base URL, the key variable and its tool-capable models with
//! their windows. It is built into the binary (`directory.tsv`, regenerated
//! by a one-off script), so any of them works with only its key set:
//!
//! ```sh
//! export CHUTES_API_KEY=...   # the directory knows the rest
//! nexo -P chutes
//! ```
//!
//! The hand-written presets ([`crate::presets`]) win over an entry with the
//! same name or alias; entries are offered when their key is present, when
//! the credential store has one, or when named.

use nexo_core::ProviderConfig;

const TSV: &str = include_str!("directory.tsv");

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    pub key_env: &'static str,
    pub doc: &'static str,
    /// Models with their context windows (0: unknown).
    pub models: Vec<(&'static str, u32)>,
}

impl Entry {
    pub fn to_config(&self) -> ProviderConfig {
        let mut c = ProviderConfig::new(self.protocol, self.base_url, Some(self.key_env));
        for (m, ctx) in &self.models {
            c.models.push(m.to_string());
            if *ctx > 0 {
                c.context_window.insert(m.to_string(), *ctx);
            }
        }
        c.default_model = self.models.first().map(|m| m.0.to_string());
        c
    }
}

/// Every entry, in the order of the file (by id).
pub fn all() -> Vec<Entry> {
    TSV.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).filter_map(parse).collect()
}

fn parse(line: &'static str) -> Option<Entry> {
    let f: Vec<&'static str> = line.split('\t').collect();
    if f.len() < 7 {
        return None;
    }
    let models = f[6]
        .split_whitespace()
        .map(|m| match m.rsplit_once('=') {
            Some((id, ctx)) => (id, ctx.parse().unwrap_or(0)),
            None => (m, 0),
        })
        .collect();
    Some(Entry { id: f[0], name: f[1], protocol: f[2], base_url: f[3], key_env: f[4], doc: f[5], models })
}

/// Directory ids a preset already covers under another name.
pub const ALIASES: &[(&str, &str)] = &[
    ("google", "gemini"),
    ("togetherai", "together"),
    ("fireworks-ai", "fireworks"),
    ("moonshotai", "moonshot"),
    ("alibaba", "qwen"),
    ("zhipuai", "zhipu"),
    ("zai", "zhipu"),
    ("github-copilot", "copilot"),
    ("lmstudio", "lmstudio"),
];

/// The entry for `id`, by its own id (not an alias of a preset).
pub fn get(id: &str) -> Option<Entry> {
    all().into_iter().find(|e| e.id == id)
}

/// The preset a directory id stands for, when one does.
pub fn alias(id: &str) -> Option<&'static str> {
    ALIASES.iter().find(|(d, _)| *d == id).map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_large_and_well_formed() {
        let all = all();
        assert!(all.len() > 150, "{}", all.len());
        for e in &all {
            assert!(e.base_url.starts_with("http"), "{}", e.id);
            assert!(!e.key_env.is_empty(), "{}", e.id);
            assert!(["openai-chat", "openai-responses", "anthropic", "gemini"].contains(&e.protocol), "{}", e.id);
        }
        let ds = get("deepseek").unwrap().to_config();
        assert_eq!(ds.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
        assert!(!ds.models.is_empty() && ds.default_model.is_some());
        assert_eq!(alias("google"), Some("gemini"));
    }
}
