//! The models `/model` offers: for each provider that is ready, the ones in the configuration (the
//! default first), then the ones the vendor's own list holds, then the chat models of the models.dev
//! catalogue. Both lists come from files on disk, so opening the picker never waits for the network;
//! [`refresh`] fills the vendor file in the background (once a day) and a failure keeps what was there.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nexo_core::{Config, ProviderConfig};
use serde::{Deserialize, Serialize};

use crate::catalogue::{self, Catalogue};

/// A vendor list older than this is asked for again.
pub const TTL: Duration = Duration::from_secs(24 * 3600);
/// After a failed or fruitless ask the same provider is left alone this long (this process only).
const RETRY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Stored {
    /// Seconds since the epoch.
    at: u64,
    /// Where it was asked: a list from another address is not this provider's.
    base_url: String,
    models: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Store {
    providers: BTreeMap<String, Stored>,
}

/// Where the vendor lists are kept.
pub fn cache_file() -> PathBuf {
    nexo_core::paths::cache_dir().join("vendor-models.json")
}

fn load(file: &Path) -> Store {
    std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(file: &Path, store: &Store) {
    let Ok(text) = serde_json::to_string(store) else { return };
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, file)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The model ids the vendor listed last time for `provider` at `base_url`, from `file`.
fn listed(store: &Store, provider: &str, base_url: &str) -> Vec<String> {
    store.providers.get(provider).filter(|s| s.base_url == base_url).map(|s| s.models.clone()).unwrap_or_default()
}

/// What `/model` offers, from the files on disk (the catalogue and the vendor lists as last cached).
pub fn choices(cfg: &Config) -> Vec<(String, String)> {
    choices_from(cfg, catalogue::cached().as_deref(), &cache_file())
}

/// [`choices`] with the catalogue and the vendor file given.
pub fn choices_from(cfg: &Config, catalogue: Option<&Catalogue>, vendor_file: &Path) -> Vec<(String, String)> {
    let store = load(vendor_file);
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, p) in &cfg.providers {
        if !p.ready(name) {
            continue;
        }
        let mut ids: Vec<String> = Vec::new();
        let add = |ids: &mut Vec<String>, m: &str| {
            if !m.is_empty() && !ids.iter().any(|x| x == m) {
                ids.push(m.to_string());
            }
        };
        // what the person wrote is offered as it is; the default is never left out
        for m in &p.models {
            add(&mut ids, m);
        }
        if let Some(d) = &p.default_model {
            add(&mut ids, d);
        }
        for m in listed(&store, name, &p.base_url) {
            let chat = catalogue.map_or(!catalogue::looks_like_non_chat(&m), |c| c.is_chat(name, &m));
            if chat {
                add(&mut ids, &m);
            }
        }
        if let Some(c) = catalogue {
            for m in c.chat_models_of(name) {
                add(&mut ids, &m);
            }
        }
        out.extend(ids.into_iter().map(|m| (name.clone(), m)));
    }
    out
}

static TRIED: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// Asks each ready provider for its model list when the cached one is older than [`TTL`] (or none),
/// through `fetch`, and keeps the answers in `file`. A provider that does not answer in `timeout`, or
/// answers with an error or nothing, keeps its last list. Returns whether the file changed.
pub async fn refresh<F, Fut>(cfg: &Config, file: &Path, timeout: Duration, fetch: F) -> bool
where
    F: Fn(ProviderConfig) -> Fut,
    Fut: Future<Output = Result<Vec<String>, String>>,
{
    let store = load(file);
    let mut due: Vec<(String, ProviderConfig)> = Vec::new();
    {
        let mut tried = TRIED.lock().unwrap_or_else(|e| e.into_inner());
        for (name, p) in &cfg.providers {
            if !p.ready(name) {
                continue;
            }
            let fresh = store
                .providers
                .get(name)
                .is_some_and(|s| s.base_url == p.base_url && now().saturating_sub(s.at) < TTL.as_secs());
            let key = format!("{file:?}|{name}");
            let recent = tried.iter().any(|(k, t)| *k == key && t.elapsed() < RETRY);
            if fresh || recent {
                continue;
            }
            tried.retain(|(k, _)| *k != key);
            tried.push((key, Instant::now()));
            due.push((name.clone(), p.clone()));
        }
    }
    if due.is_empty() {
        return false;
    }
    let answers = futures::future::join_all(due.into_iter().map(|(name, p)| {
        let ask = fetch(p.clone());
        async move {
            let r = tokio::time::timeout(timeout, ask).await.ok().and_then(Result::ok);
            (name, p.base_url, r)
        }
    }))
    .await;
    let mut store = load(file);
    let before = store.clone();
    for (name, base_url, models) in answers {
        let Some(models) = models.filter(|m| !m.is_empty()) else { continue };
        store.providers.insert(name, Stored { at: now(), base_url, models });
    }
    let changed = store != before;
    if changed {
        save(file, &store);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(models: &[&str]) -> Config {
        // `ready()` reads the credential store: not the one of whoever runs the tests
        std::env::set_var("NEXO_AUTH_FILE", "/nonexistent/nexo-test-auth.json");
        let mut c = Config::default();
        let mut p = ProviderConfig::new("openai-chat", "http://127.0.0.1:1/v1", None);
        p.models = models.iter().map(|m| m.to_string()).collect();
        c.providers.insert("xai".into(), p);
        let mut off = ProviderConfig::new("openai-chat", "https://api.example.com/v1", Some("NEXO_TEST_NO_SUCH_KEY"));
        off.models = vec!["off-model".into()];
        c.providers.insert("zzz-no-key".into(), off);
        c
    }

    fn catalogue() -> Catalogue {
        Catalogue::parse(
            r#"{"xai":{"models":{
              "grok-4.7":{"release_date":"2026-09-21","modalities":{"output":["text"]}},
              "grok-4.3":{"release_date":"2026-05-01","modalities":{"output":["text"]}},
              "grok-4":{"release_date":"2025-07-01","status":"deprecated","modalities":{"output":["text"]}},
              "grok-imagine-image":{"release_date":"2026-08-01","modalities":{"output":["image"]}},
              "grok-imagine-video":{"modalities":{"output":["video"]}},
              "grok-4.20-multi-agent-0309":{"modalities":{"output":["text"]}}}},
              "zzz-no-key":{"models":{"m":{"modalities":{"output":["text"]}}}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_ready_provider_offers_its_own_models_then_the_catalogue_chat_ones() {
        let d = tempfile::tempdir().unwrap();
        let got = choices_from(&config(&["grok-4"]), Some(&catalogue()), &d.path().join("v.json"));
        let xai: Vec<&str> = got.iter().filter(|(p, _)| p == "xai").map(|(_, m)| m.as_str()).collect();
        // newest first after the configured one; the retired and the image/video ones left out
        assert_eq!(xai, ["grok-4", "grok-4.7", "grok-4.3"]);
        // a provider that is not ready (a key that is not there) offers nothing
        assert!(!got.iter().any(|(p, _)| p == "zzz-no-key"));
    }

    #[test]
    fn without_a_catalogue_the_configured_models_stay() {
        let d = tempfile::tempdir().unwrap();
        let mut c = config(&["a"]);
        c.providers.get_mut("xai").unwrap().default_model = Some("b".into());
        let got = choices_from(&c, None, &d.path().join("v.json"));
        assert_eq!(got, [("xai".to_string(), "a".to_string()), ("xai".to_string(), "b".to_string())]);
    }

    #[tokio::test]
    async fn a_model_the_vendor_adds_shows_up_after_a_refresh_and_non_chat_ones_do_not() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("v.json");
        let c = config(&["grok-4.7"]);
        let ask = |ids: &'static [&'static str]| {
            move |_p: ProviderConfig| async move { Ok::<_, String>(ids.iter().map(|s| s.to_string()).collect()) }
        };
        assert!(
            refresh(&c, &f, Duration::from_secs(5), ask(&["grok-4.7", "grok-5", "grok-imagine-image-2", "tts-1"]))
                .await
        );
        let got = choices_from(&c, Some(&catalogue()), &f);
        let xai: Vec<&str> = got.iter().filter(|(p, _)| p == "xai").map(|(_, m)| m.as_str()).collect();
        assert_eq!(xai, ["grok-4.7", "grok-5", "grok-4.3"]);
    }

    #[tokio::test]
    async fn a_list_older_than_a_day_is_asked_again_and_a_fresh_one_is_not() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("v.json");
        let c = config(&["a"]);
        let mut store = Store::default();
        let base = c.providers["xai"].base_url.clone();
        store.providers.insert("xai".into(), Stored { at: 1, base_url: base.clone(), models: vec!["old".into()] });
        save(&f, &store);
        let new = |_p: ProviderConfig| async { Ok::<_, String>(vec!["new".to_string()]) };
        assert!(refresh(&c, &f, Duration::from_secs(5), new).await);
        assert_eq!(listed(&load(&f), "xai", &base), ["new"]);
        // asked a moment ago: left alone
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let again = |_p: ProviderConfig| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok::<_, String>(vec!["newer".to_string()]) }
        };
        assert!(!refresh(&c, &f, Duration::from_secs(5), again).await);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn offline_keeps_the_cached_list() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("v.json");
        let mut c = config(&["a"]);
        let mut store = Store::default();
        store.providers.insert(
            "xai".into(),
            Stored { at: 1, base_url: c.providers["xai"].base_url.clone(), models: vec!["old-one".into()] },
        );
        save(&f, &store);
        let fail = |_p: ProviderConfig| async { Err::<Vec<String>, String>("offline".into()) };
        assert!(!refresh(&c, &f, Duration::from_secs(5), fail).await);
        let got = choices_from(&c, None, &f);
        assert!(got.contains(&("xai".to_string(), "old-one".to_string())), "{got:?}");
        // a list from another address is not this provider's
        c.providers.get_mut("xai").unwrap().base_url = "http://127.0.0.1:2/v1".into();
        assert!(!choices_from(&c, None, &f).contains(&("xai".to_string(), "old-one".to_string())));
    }
}
