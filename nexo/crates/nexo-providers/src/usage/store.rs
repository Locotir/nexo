use std::collections::BTreeMap;
use std::sync::Mutex;

use nexo_core::auth;
use nexo_core::ProviderConfig;
use reqwest::header::HeaderMap;
use serde_json::Value;

use super::codex::{codex_from_event, codex_from_headers};
use super::copilot::copilot_from_header;
use super::limits::limits_from_headers;
use super::model::{slot_rank, PlanUsage, CLAUDE, CODEX, COPILOT, LIMITS, XAI};

static STORE: Mutex<BTreeMap<String, PlanUsage>> = Mutex::new(BTreeMap::new());
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Wakes when a reading is stored (one wake-up is kept for a listener that is not waiting yet).
pub fn changed() -> &'static tokio::sync::Notify {
    &CHANGED
}

/// Keeps a reading on top of what is known of that plan.
pub fn record(u: PlanUsage) {
    {
        let mut s = STORE.lock().unwrap_or_else(|e| e.into_inner());
        match s.get_mut(&u.kind) {
            Some(old) => old.merge(u),
            None => {
                let mut u = u;
                u.meters.sort_by_key(|m| slot_rank(&m.slot));
                s.insert(u.kind.clone(), u);
            }
        }
    }
    CHANGED.notify_one();
}

/// The latest reading of the plan `kind` (`codex`, `copilot`).
pub fn get(kind: &str) -> Option<PlanUsage> {
    STORE.lock().unwrap_or_else(|e| e.into_inner()).get(kind).cloned()
}

/// Every reading kept, plans first.
pub fn all() -> Vec<PlanUsage> {
    let mut v: Vec<PlanUsage> = STORE.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    v.sort_by_key(PlanUsage::is_limits);
    v
}

/// The host of a URL: the key of the rate limits it answers with.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

/// Takes the plan figures and the rate limits out of the headers of a response of `url`, if it had any.
pub fn record_headers(url: &str, h: &HeaderMap) {
    let now = auth::now() as i64;
    let limits = limits_from_headers(&host_of(url), h, now);
    for u in [codex_from_headers(h, now), copilot_from_header(h, now), limits].into_iter().flatten() {
        record(u);
    }
}

/// Takes the plan figures out of a stream event, if it is one.
pub fn record_event(v: &Value) {
    if let Some(u) = codex_from_event(v, auth::now() as i64) {
        record(u);
    }
}

/// What is known of the plan of `cfg`'s login, else of the rate limits its host reported.
pub fn reading_for(cfg: &ProviderConfig) -> Option<PlanUsage> {
    kind_of(cfg).and_then(get).or_else(|| get(&format!("{LIMITS}{}", host_of(&cfg.base_url))))
}

/// Which plan a provider's login opens, if it is one that can be read.
pub fn kind_of(cfg: &ProviderConfig) -> Option<&'static str> {
    match cfg.login.as_deref()? {
        "codex" | "chatgpt" => Some(CODEX),
        "copilot" => Some(COPILOT),
        "xai" => Some(XAI),
        "claude" => Some(CLAUDE),
        _ => None,
    }
}
