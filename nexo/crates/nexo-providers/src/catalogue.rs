//! The models.dev catalogue: an open dataset of providers and models with
//! prices per million tokens and context/output limits (the one OpenCode
//! reads). Fetched once, cached on disk for a week, refreshed in the
//! background when stale; a request is never held for it.
//!
//! The file is `{ "<provider>": { "name", "models": { "<model>": { "name",
//! "cost": { "input", "output", "cache_read", "cache_write" }, "limit":
//! { "context", "output" } } } } }`; anything else in it is ignored.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nexo_core::{ErrorKind, Pricing, ProviderError};
use serde_json::Value;

pub const URL: &str = "https://models.dev/api.json";
/// A cached file older than this is refreshed (in the background).
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// What the catalogue says about one model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub name: Option<String>,
    pub pricing: Option<Pricing>,
    pub context: Option<u32>,
    pub output: Option<u32>,
    /// The model reasons, calls tools, reads images (models.dev `reasoning`, `tool_call`, `modalities.input`).
    pub reasoning: Option<bool>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
}

/// Providers (by their models.dev id) and their models.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalogue {
    pub providers: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// models.dev provider id → nexo preset name, where they differ.
const ALIASES: &[(&str, &str)] = &[
    ("google", "gemini"),
    ("togetherai", "together"),
    ("fireworks-ai", "fireworks"),
    ("moonshotai", "moonshot"),
    ("alibaba", "qwen"),
    ("alibaba-cn", "qwen"),
    ("zhipuai", "zhipu"),
    ("zai", "zhipu"),
    ("llama.cpp", "llamacpp"),
    ("lmstudio", "lmstudio"),
];

/// Subscription presets that serve the models of an API provider: nexo preset -> models.dev id.
const PARENTS: &[(&str, &str)] = &[("codex", "openai")];

/// The preset name a models.dev provider id maps to.
pub fn preset_name(id: &str) -> &str {
    ALIASES.iter().find(|(from, _)| *from == id).map(|(_, to)| *to).unwrap_or(id)
}

/// The models.dev ids that map to a preset name (the name itself first).
pub fn catalogue_ids(preset: &str) -> Vec<String> {
    let mut out = vec![preset.to_string()];
    for (from, to) in ALIASES {
        if *to == preset && *from != preset {
            out.push(from.to_string());
        }
    }
    out
}

impl Catalogue {
    /// Parses the dataset. Models without a price keep `pricing: None`.
    pub fn parse(text: &str) -> Result<Catalogue, String> {
        let root: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
        let obj = root.as_object().ok_or("the catalogue is not an object")?;
        let mut providers = BTreeMap::new();
        for (pid, p) in obj {
            let Some(models) = p.get("models").and_then(Value::as_object) else { continue };
            let mut out = BTreeMap::new();
            for (mid, m) in models {
                out.insert(mid.clone(), entry_of(m));
            }
            providers.insert(pid.clone(), out);
        }
        if providers.is_empty() {
            return Err("the catalogue has no providers".into());
        }
        Ok(Catalogue { providers })
    }

    /// The entry for `model` under the nexo provider `preset` (its models.dev
    /// ids are tried, then every provider, exact id first, then the longest
    /// id that is a prefix of the model or that the model is a prefix of:
    /// `gpt-5-2026-01-01` finds `gpt-5`, `deepseek-chat` finds
    /// `deepseek-chat-v3` only when nothing better exists).
    pub fn lookup(&self, preset: &str, model: &str) -> Option<&Entry> {
        let model = model.trim();
        for id in catalogue_ids(preset) {
            if let Some(models) = self.providers.get(&id) {
                if let Some(e) = models.get(model) {
                    return Some(e);
                }
            }
        }
        for id in catalogue_ids(preset) {
            if let Some(models) = self.providers.get(&id) {
                if let Some(e) = fuzzy(models, model) {
                    return Some(e);
                }
            }
        }
        // the same model under another vendor (an OpenAI-compatible server, a gateway)
        self.elsewhere(preset, model)
    }

    /// The model under the other providers: the preset's parent API first
    /// (`codex` -> `openai`), else the entry with the smallest window (the safe
    /// one; an entry with a window beats one without), ties by provider id.
    fn elsewhere(&self, preset: &str, model: &str) -> Option<&Entry> {
        let bare = model.rsplit('/').next().unwrap_or(model);
        fn pick<'a>(m: &'a BTreeMap<String, Entry>, model: &str, bare: &str) -> Option<&'a Entry> {
            m.get(model).or_else(|| m.get(bare))
        }
        if let Some(parent) = PARENTS.iter().find(|(p, _)| *p == preset).map(|(_, a)| *a) {
            if let Some(e) = self.providers.get(parent).and_then(|m| pick(m, model, bare)) {
                return Some(e);
            }
        }
        let smallest = |id: &str| {
            self.providers
                .values()
                .filter_map(|m| m.get(id))
                .min_by_key(|e| (e.context.is_none(), e.context.unwrap_or(u32::MAX)))
        };
        smallest(model).or_else(|| smallest(bare))
    }

    /// Every model known under a preset, with its entry.
    pub fn models_of(&self, preset: &str) -> Vec<(String, Entry)> {
        let mut out = Vec::new();
        for id in catalogue_ids(preset) {
            if let Some(models) = self.providers.get(&id) {
                for (k, v) in models {
                    out.push((k.clone(), v.clone()));
                }
            }
        }
        out
    }

    /// The model ids under a preset (for suggestions).
    pub fn model_names(&self, preset: &str) -> Vec<String> {
        self.models_of(preset).into_iter().map(|(k, _)| k).collect()
    }
}

fn fuzzy<'a>(models: &'a BTreeMap<String, Entry>, model: &str) -> Option<&'a Entry> {
    let m = model.to_ascii_lowercase();
    let mut best: Option<(usize, &Entry)> = None;
    for (id, e) in models {
        let i = id.to_ascii_lowercase();
        let len = if m.starts_with(&i) && m[i.len()..].starts_with(['-', ':', '@', '.']) {
            i.len()
        } else if i.starts_with(&m) && i[m.len()..].starts_with(['-', ':', '@', '.']) {
            m.len()
        } else {
            continue;
        };
        if best.map(|(l, _)| len > l).unwrap_or(true) {
            best = Some((len, e));
        }
    }
    best.map(|(_, e)| e)
}

fn entry_of(m: &Value) -> Entry {
    let num = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0);
    let int =
        |v: Option<&Value>| v.and_then(Value::as_u64).filter(|x| *x > 0 && *x <= u32::MAX as u64).map(|x| x as u32);
    let cost = m.get("cost");
    let pricing = cost.and_then(|c| {
        let p = Pricing {
            input: num(c.get("input")).unwrap_or(0.0),
            output: num(c.get("output")).unwrap_or(0.0),
            cache_read: num(c.get("cache_read")).unwrap_or(0.0),
            cache_write: num(c.get("cache_write")).unwrap_or(0.0),
        };
        // a free model is a price too: input and output present, both 0
        (p.is_known() || (c.get("input").is_some() && c.get("output").is_some())).then_some(p)
    });
    let limit = m.get("limit");
    Entry {
        name: m.get("name").and_then(Value::as_str).map(String::from),
        pricing,
        context: int(limit.and_then(|l| l.get("context"))),
        output: int(limit.and_then(|l| l.get("output"))),
        reasoning: m.get("reasoning").and_then(Value::as_bool),
        tools: m.get("tool_call").and_then(Value::as_bool),
        vision: m.pointer("/modalities/input").and_then(Value::as_array).map(|a| a.iter().any(|x| x == "image")),
    }
}

// ------------------------------------------------------------ the cache on disk

/// Where the dataset is cached.
pub fn cache_file() -> PathBuf {
    nexo_core::paths::cache_dir().join("models.json")
}

/// Age of the cached file, None when there is none.
pub fn cache_age() -> Option<Duration> {
    std::fs::metadata(cache_file()).ok()?.modified().ok()?.elapsed().ok()
}

/// Whether a refresh is due (no cache, or older than [`MAX_AGE`]).
pub fn stale() -> bool {
    cache_age().map(|a| a > MAX_AGE).unwrap_or(true)
}

static LOADED: OnceLock<Mutex<Option<Arc<Catalogue>>>> = OnceLock::new();
static REFRESHING: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static Mutex<Option<Arc<Catalogue>>> {
    LOADED.get_or_init(|| Mutex::new(None))
}

/// The catalogue as cached on disk (read once per process; `refresh`
/// replaces it). None when there is no usable cache.
pub fn cached() -> Option<Arc<Catalogue>> {
    let mut g = slot().lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let text = std::fs::read_to_string(cache_file()).ok()?;
        *g = Catalogue::parse(&text).ok().map(Arc::new);
    }
    g.clone()
}

/// Replaces the in-memory catalogue (tests, and `refresh`).
pub fn install(c: Catalogue) -> Arc<Catalogue> {
    let c = Arc::new(c);
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(c.clone());
    c
}

/// Fetches the dataset now, writes the cache and installs it. Goes through
/// the shared client, so the system proxy settings apply.
pub async fn refresh() -> Result<Arc<Catalogue>, ProviderError> {
    let resp = crate::http::client()?
        .get(URL)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| ProviderError::new(ErrorKind::Network, format!("cannot reach models.dev: {e}")))?;
    let status = resp.status().as_u16();
    let text =
        resp.text().await.map_err(|e| ProviderError::new(ErrorKind::Network, format!("models.dev cut off: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_http(status, "models.dev did not answer with the catalogue"));
    }
    let c = Catalogue::parse(&text).map_err(|e| ProviderError::new(ErrorKind::Parse, e))?;
    let file = cache_file();
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, &text).and_then(|_| std::fs::rename(&tmp, &file)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(install(c))
}

/// What `refresh_if_stale` calls when the refresh ends.
pub type Done = Box<dyn FnOnce(Result<(), String>) + Send>;

/// Starts a refresh in the background when the cache is stale and none is
/// running; returns at once. `done` is called with the result, if given.
pub fn refresh_if_stale(done: Option<Done>) {
    if !stale() || REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let r = refresh().await.map(|_| ()).map_err(|e| e.message);
        REFRESHING.store(false, Ordering::SeqCst);
        if let Some(f) = done {
            f(r);
        }
    });
}

/// `(pricing, context window, largest reply)` of a model, from the cached
/// catalogue; None when the catalogue has no such model (or no cache).
pub fn lookup(preset: &str, model: &str) -> Option<(Option<Pricing>, Option<u32>, Option<u32>)> {
    let c = cached()?;
    let e = c.lookup(preset, model)?;
    Some((e.pricing, e.context, e.output))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../tests/fixtures/models_dev_excerpt.json");

    #[test]
    fn parses_prices_and_limits() {
        let c = Catalogue::parse(SAMPLE).unwrap();
        let e = c.lookup("deepseek", "deepseek-chat").unwrap();
        assert_eq!(e.pricing.unwrap().cache_read, 0.028);
        assert_eq!(e.context, Some(128_000));
        assert_eq!(e.output, Some(8_192));
        let g = c.lookup("gemini", "gemini-2.5-pro").unwrap();
        assert_eq!(g.context, Some(1_048_576));
        assert!(c.lookup("openai", "gpt-5").unwrap().pricing.unwrap().input > 0.0);
    }

    #[test]
    fn finds_dated_and_gateway_names() {
        let c = Catalogue::parse(SAMPLE).unwrap();
        assert_eq!(c.lookup("openai", "gpt-5-2026-01-01").unwrap().context, Some(400_000));
        assert_eq!(c.lookup("openrouter", "deepseek/deepseek-chat").unwrap().context, Some(128_000));
        assert!(c.lookup("deepseek", "nothing-like-this").is_none());
        assert!(c.model_names("anthropic").contains(&"claude-sonnet-4-5".to_string()));
    }

    #[test]
    fn a_free_model_has_a_zero_price_and_a_priceless_one_none() {
        let c = Catalogue::parse(SAMPLE).unwrap();
        let free = c.lookup("openrouter", "free/model").unwrap();
        assert_eq!(free.pricing, Some(Pricing::default()));
        assert_eq!(c.lookup("ollama", "qwen3:8b").unwrap().pricing, None);
        assert!(Catalogue::parse("{}").is_err());
        assert!(Catalogue::parse("[1]").is_err());
    }

    #[test]
    fn names_map_both_ways() {
        assert_eq!(preset_name("google"), "gemini");
        assert_eq!(preset_name("deepseek"), "deepseek");
        assert_eq!(catalogue_ids("qwen"), vec!["qwen", "alibaba", "alibaba-cn"]);
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    fn cat(rows: &[(&str, &str, Option<u32>)]) -> Catalogue {
        let mut c = Catalogue::default();
        for (p, m, w) in rows {
            c.providers
                .entry(p.to_string())
                .or_default()
                .insert(m.to_string(), Entry { context: *w, ..Default::default() });
        }
        c
    }

    #[test]
    fn own_provider_wins() {
        let c = cat(&[("deepseek", "m", Some(64_000)), ("openai", "m", Some(8_000))]);
        assert_eq!(c.lookup("deepseek", "m").unwrap().context, Some(64_000));
    }

    #[test]
    fn missing_under_own_is_found_under_the_parent_api() {
        let c = cat(&[("abacus", "m", Some(1_000_000)), ("openai", "m", Some(1_050_000)), ("zed", "m", Some(5_000))]);
        assert_eq!(c.lookup("codex", "m").unwrap().context, Some(1_050_000));
    }

    #[test]
    fn several_others_give_the_smallest_window() {
        let c =
            cat(&[("a", "x/m", Some(900_000)), ("b", "m", Some(500_000)), ("c", "m", None), ("d", "m", Some(700_000))]);
        assert_eq!(c.lookup("nobody", "m").unwrap().context, Some(500_000));
        assert_eq!(c.lookup("nobody", "x/m").unwrap().context, Some(900_000));
    }

    #[test]
    fn nowhere_is_none() {
        let c = cat(&[("openai", "m", Some(1))]);
        assert!(c.lookup("codex", "other").is_none());
    }
}
