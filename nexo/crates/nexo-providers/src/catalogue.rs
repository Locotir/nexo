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

pub const URL: &str = "https://models.dev/api.json";
/// A cached file older than this is refreshed (in the background).
pub const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

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
    /// The model answers in text (models.dev `modalities.output`); None when the catalogue does not say.
    pub text_out: Option<bool>,
    /// models.dev `release_date` (`2026-09-21`), to put the newest first.
    pub released: Option<String>,
    /// models.dev `status = "deprecated"`.
    pub retired: bool,
}

/// Providers (by their models.dev id) and their models.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalogue {
    pub providers: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// Subscription presets that serve the models of an API provider: nexo preset -> models.dev id.
const PARENTS: &[(&str, &str)] = &[("codex", "openai")];

/// The preset name a models.dev provider id maps to.
pub fn preset_name(id: &str) -> &str {
    crate::vendors::VENDORS.iter().find(|v| v.catalogue_ids.contains(&id)).map(|v| v.id).unwrap_or(id)
}

/// The models.dev ids that map to a preset name (the name itself first).
pub fn catalogue_ids(preset: &str) -> Vec<String> {
    let mut out = vec![preset.to_string()];
    for v in crate::vendors::VENDORS.iter().filter(|v| v.id == preset) {
        out.extend(v.catalogue_ids.iter().filter(|c| **c != preset).map(|c| c.to_string()));
    }
    out
}

impl Catalogue {
    /// Parses the dataset. Models without a price keep `pricing: None`.
    pub fn parse(text: &str) -> Result<Catalogue, String> {
        // Typed, so the 6 MB file is never built into a `Value` tree (that cost about 55 ms and 60 MB at every
        // start); every leaf is tolerant of a wrong type, as the dataset is not ours to keep clean.
        let root: BTreeMap<String, raw::Obj<raw::Provider>> =
            serde_json::from_str(text).map_err(|e| format!("not a JSON object: {e}"))?;
        let mut providers = BTreeMap::new();
        for (pid, p) in root {
            let Some(models) = p.0.and_then(|p| p.models.0) else { continue };
            let out = models.into_iter().map(|(mid, m)| (mid, entry_of(m.0.unwrap_or_default()))).collect();
            providers.insert(pid, out);
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

    /// The chat models under a preset, newest first (images, video, speech, embeddings and retired
    /// models left out).
    pub fn chat_models_of(&self, preset: &str) -> Vec<String> {
        let mut all: Vec<(String, Entry)> = self
            .models_of(preset)
            .into_iter()
            .filter(|(id, e)| {
                !e.retired && e.output != Some(0) && e.text_out != Some(false) && !looks_like_non_chat(id)
            })
            .collect();
        all.sort_by(|(ia, a), (ib, b)| b.released.cmp(&a.released).then_with(|| ib.cmp(ia)));
        all.into_iter().map(|(id, _)| id).collect()
    }

    /// Whether `model` is a chat model of `preset`: what the catalogue says of it when it knows it, else its id.
    pub fn is_chat(&self, preset: &str, model: &str) -> bool {
        let exact = catalogue_ids(preset).into_iter().find_map(|id| self.providers.get(&id).and_then(|m| m.get(model)));
        match exact {
            Some(e) => !e.retired && e.output != Some(0) && e.text_out != Some(false) && !looks_like_non_chat(model),
            None => !looks_like_non_chat(model),
        }
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

/// The wire shape of the dataset, read straight into small structs. Every leaf tolerates a wrong type
/// (it reads as absent) and a wrongly shaped object is skipped, as the old `Value` walk did.
mod raw {
    use std::collections::BTreeMap;
    use std::fmt;

    use serde::de::value::MapAccessDeserializer;
    use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
    use serde::Deserialize;

    /// One JSON scalar of any kind; an array or object reads as `Other`.
    #[derive(Default)]
    pub enum Scalar {
        #[default]
        Absent,
        Bool(bool),
        Uint(u64),
        Int(i64),
        Float(f64),
        Str(String),
        /// null, an array or an object: present, but none of the above.
        Other,
    }

    impl Scalar {
        pub fn present(&self) -> bool {
            !matches!(self, Scalar::Absent)
        }
        pub fn as_f64(&self) -> Option<f64> {
            match *self {
                Scalar::Uint(n) => Some(n as f64),
                Scalar::Int(n) => Some(n as f64),
                Scalar::Float(n) => Some(n),
                _ => None,
            }
        }
        /// Only a non-negative integer, as `Value::as_u64` (a float such as `1e5` is not one).
        pub fn as_u64(&self) -> Option<u64> {
            match *self {
                Scalar::Uint(n) => Some(n),
                _ => None,
            }
        }
        pub fn as_bool(&self) -> Option<bool> {
            match *self {
                Scalar::Bool(b) => Some(b),
                _ => None,
            }
        }
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Scalar::Str(s) => Some(s),
                _ => None,
            }
        }
    }

    fn drain_map<'de, A: MapAccess<'de>>(mut m: A) -> Result<(), A::Error> {
        while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn drain_seq<'de, A: SeqAccess<'de>>(mut s: A) -> Result<(), A::Error> {
        while s.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    impl<'de> Deserialize<'de> for Scalar {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Scalar;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("any JSON value")
                }
                fn visit_bool<E>(self, v: bool) -> Result<Scalar, E> {
                    Ok(Scalar::Bool(v))
                }
                fn visit_u64<E>(self, v: u64) -> Result<Scalar, E> {
                    Ok(Scalar::Uint(v))
                }
                fn visit_i64<E>(self, v: i64) -> Result<Scalar, E> {
                    Ok(Scalar::Int(v))
                }
                fn visit_f64<E>(self, v: f64) -> Result<Scalar, E> {
                    Ok(Scalar::Float(v))
                }
                fn visit_str<E>(self, v: &str) -> Result<Scalar, E> {
                    Ok(Scalar::Str(v.to_string()))
                }
                fn visit_unit<E>(self) -> Result<Scalar, E> {
                    Ok(Scalar::Other)
                }
                fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Scalar, A::Error> {
                    drain_seq(s).map(|_| Scalar::Other)
                }
                fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Scalar, A::Error> {
                    drain_map(m).map(|_| Scalar::Other)
                }
            }
            d.deserialize_any(V)
        }
    }

    /// A list of scalars; None when the value is not an array.
    #[derive(Default)]
    pub struct List(pub Option<Vec<Scalar>>);

    impl<'de> Deserialize<'de> for List {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = List;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("any JSON value")
                }
                fn visit_bool<E>(self, _: bool) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_u64<E>(self, _: u64) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_i64<E>(self, _: i64) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_f64<E>(self, _: f64) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_str<E>(self, _: &str) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_unit<E>(self) -> Result<List, E> {
                    Ok(List(None))
                }
                fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<List, A::Error> {
                    drain_map(m).map(|_| List(None))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<List, A::Error> {
                    let mut v = Vec::new();
                    while let Some(x) = s.next_element::<Scalar>()? {
                        v.push(x);
                    }
                    Ok(List(Some(v)))
                }
            }
            d.deserialize_any(V)
        }
    }

    impl List {
        /// Whether the list holds the string `s`; None when the value was not a list.
        pub fn has(&self, s: &str) -> Option<bool> {
            self.0.as_ref().map(|v| v.iter().any(|x| x.as_str() == Some(s)))
        }
    }

    /// A JSON object read as `T`; None when the value is anything else.
    pub struct Obj<T>(pub Option<T>);

    impl<T> Default for Obj<T> {
        fn default() -> Self {
            Obj(None)
        }
    }

    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Obj<T> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V<T>(std::marker::PhantomData<T>);
            impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
                type Value = Obj<T>;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("any JSON value")
                }
                fn visit_bool<E>(self, _: bool) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_u64<E>(self, _: u64) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_i64<E>(self, _: i64) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_f64<E>(self, _: f64) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_str<E>(self, _: &str) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_unit<E>(self) -> Result<Obj<T>, E> {
                    Ok(Obj(None))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Obj<T>, A::Error> {
                    drain_seq(s).map(|_| Obj(None))
                }
                fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Obj<T>, A::Error> {
                    T::deserialize(MapAccessDeserializer::new(m)).map(|t| Obj(Some(t)))
                }
            }
            d.deserialize_any(V(std::marker::PhantomData))
        }
    }

    /// A struct read from a JSON object by hand: a key it does not know is skipped and a repeated key
    /// keeps its last value, as the old `Value` walk did (a derived `Deserialize` fails the whole file on
    /// a repeated key, with `duplicate field`). The key is matched without allocating.
    macro_rules! lenient_struct {
        ($(#[$meta:meta])* pub struct $name:ident { $(pub $field:ident: $ty:ty),* $(,)? }) => {
            $(#[$meta])*
            #[derive(Default)]
            pub struct $name {
                $(pub $field: $ty),*
            }

            impl<'de> Deserialize<'de> for $name {
                fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                    /// The name of a known key, or "" for any other.
                    struct Key(&'static str);
                    impl<'de> Deserialize<'de> for Key {
                        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                            struct KV;
                            impl<'de> Visitor<'de> for KV {
                                type Value = Key;
                                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                                    f.write_str("a key")
                                }
                                fn visit_str<E>(self, v: &str) -> Result<Key, E> {
                                    $(if v == stringify!($field) {
                                        return Ok(Key(stringify!($field)));
                                    })*
                                    Ok(Key(""))
                                }
                            }
                            d.deserialize_identifier(KV)
                        }
                    }

                    struct V;
                    impl<'de> Visitor<'de> for V {
                        type Value = $name;
                        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                            f.write_str("an object")
                        }
                        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<$name, A::Error> {
                            let mut out = $name::default();
                            while let Some(Key(k)) = m.next_key()? {
                                $(if k == stringify!($field) {
                                    out.$field = m.next_value()?;
                                    continue;
                                })*
                                m.next_value::<IgnoredAny>()?;
                            }
                            Ok(out)
                        }
                    }
                    d.deserialize_map(V)
                }
            }
        };
    }

    lenient_struct! {
        pub struct Provider {
            pub models: Obj<BTreeMap<String, Obj<Model>>>,
        }
    }

    lenient_struct! {
        pub struct Model {
            pub name: Scalar,
            pub cost: Obj<Cost>,
            pub limit: Obj<Limit>,
            pub reasoning: Scalar,
            pub tool_call: Scalar,
            pub modalities: Obj<Modalities>,
            pub release_date: Scalar,
            pub status: Scalar,
        }
    }

    lenient_struct! {
        pub struct Cost {
            pub input: Scalar,
            pub output: Scalar,
            pub cache_read: Scalar,
            pub cache_write: Scalar,
        }
    }

    lenient_struct! {
        pub struct Limit {
            pub context: Scalar,
            pub output: Scalar,
        }
    }

    lenient_struct! {
        pub struct Modalities {
            pub input: List,
            pub output: List,
        }
    }
}

fn entry_of(m: raw::Model) -> Entry {
    let num = |s: &raw::Scalar| s.as_f64().filter(|x| x.is_finite() && *x >= 0.0);
    let int = |s: &raw::Scalar| s.as_u64().filter(|x| *x > 0 && *x <= u32::MAX as u64).map(|x| x as u32);
    let pricing = m.cost.0.as_ref().and_then(|c| {
        let p = Pricing {
            input: num(&c.input).unwrap_or(0.0),
            output: num(&c.output).unwrap_or(0.0),
            cache_read: num(&c.cache_read).unwrap_or(0.0),
            cache_write: num(&c.cache_write).unwrap_or(0.0),
            cache_write_1h: 0.0,
        };
        // a free model is a price too: input and output present, both 0
        (p.is_known() || (c.input.present() && c.output.present())).then_some(p)
    });
    let limit = m.limit.0.unwrap_or_default();
    let modalities = m.modalities.0.unwrap_or_default();
    Entry {
        name: m.name.as_str().map(String::from),
        pricing,
        context: int(&limit.context),
        output: int(&limit.output),
        reasoning: m.reasoning.as_bool(),
        tools: m.tool_call.as_bool(),
        vision: modalities.input.has("image"),
        text_out: modalities.output.has("text"),
        released: m.release_date.as_str().map(String::from),
        retired: m.status.as_str() == Some("deprecated"),
    }
}

/// Whether an id names something that is not a chat model (images, video, speech, embeddings...).
pub fn looks_like_non_chat(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    if ["imagine", "embed", "dall-e", "multi-agent"].iter().any(|k| id.contains(k)) {
        return true;
    }
    id.split(['-', '_', '/', '.', ':', '@']).any(|t| {
        matches!(
            t,
            "image"
                | "video"
                | "tts"
                | "whisper"
                | "transcribe"
                | "moderation"
                | "rerank"
                | "realtime"
                | "sora"
                | "veo"
        )
    })
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
    fn a_wrongly_typed_field_reads_as_absent_and_never_fails_the_file() {
        let text = r#"{
            "broken": "not an object",
            "nomodels": { "models": [1, 2] },
            "p": { "models": {
                "str": "not an object",
                "m": {
                    "name": 7, "reasoning": "yes", "tool_call": true, "release_date": null,
                    "cost": { "input": "x", "output": [1], "cache_read": 0.5, "cache_write": -1 },
                    "limit": { "context": 1e5, "output": 4096 },
                    "modalities": { "input": ["text", 3, "image"], "output": "text" },
                    "status": "deprecated", "extra": { "deep": [1, { "a": null }] }
                }
            } }
        }"#;
        let c = Catalogue::parse(text).unwrap();
        assert_eq!(c.providers.len(), 1);
        assert_eq!(c.providers["p"]["str"], Entry::default());
        let e = &c.providers["p"]["m"];
        assert_eq!((e.name.clone(), e.reasoning, e.tools, e.released.clone()), (None, None, Some(true), None));
        assert_eq!(e.pricing.unwrap().cache_read, 0.5);
        assert_eq!((e.context, e.output), (None, Some(4096)));
        assert_eq!((e.vision, e.text_out, e.retired), (Some(true), None, true));
    }

    #[test]
    fn a_repeated_key_keeps_its_last_value_and_never_fails_the_file() {
        let text = r#"{
            "p": { "models": { "m": {
                "cost": { "input": 1, "output": 2 },
                "cost": { "input": 3, "output": 4, "input": 5 },
                "limit": { "context": 1000, "context": 2000 },
                "modalities": { "input": ["text"], "input": ["image"] },
                "reasoning": false, "reasoning": true
            } } },
            "p": { "models": { "a": {}, "a": { "tool_call": true } } }
        }"#;
        let c = Catalogue::parse(text).unwrap();
        // the last provider wins whole, as the old value walk did
        assert_eq!(c.providers["p"].len(), 1);
        assert_eq!(c.providers["p"]["a"].tools, Some(true));
        let text = text.replace(r#""p": { "models": { "a": {}, "a": { "tool_call": true } } }"#, r#""q": {}"#);
        let c = Catalogue::parse(&text).unwrap();
        let e = &c.providers["p"]["m"];
        let p = e.pricing.unwrap();
        assert_eq!((p.input, p.output), (5.0, 4.0));
        assert_eq!((e.context, e.vision, e.reasoning), (Some(2000), Some(true), Some(true)));
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
