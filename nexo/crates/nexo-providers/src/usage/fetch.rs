use nexo_core::auth::{self, Credential};
use nexo_core::{ProviderConfig, ProviderError};
use reqwest::header::HeaderMap;
use serde_json::Value;

use super::codex::codex_from_usage;
use super::copilot::copilot_from_user;
use super::model::{PlanUsage, CODEX, XAI};
use super::store::{kind_of, record};
use super::xai::xai_from_billing;
use crate::login::{self, Endpoints};

/// The plan of provider `name` as its stored login sees it. `Ok(None)` when it has none to read: a
/// provider on a key, no login stored, or a subscription whose usage cannot be read.
pub async fn fetch(name: &str, cfg: &ProviderConfig) -> Result<Option<PlanUsage>, ProviderError> {
    fetch_in(&auth::file(), &Endpoints::default(), name, cfg).await
}

pub async fn fetch_in(
    store: &std::path::Path,
    at: &Endpoints,
    name: &str,
    cfg: &ProviderConfig,
) -> Result<Option<PlanUsage>, ProviderError> {
    let Some(kind) = kind_of(cfg) else { return Ok(None) };
    // a key in the environment (or a command, or a file) is what the requests go with: no plan
    if (ProviderConfig { auth: None, ..cfg.clone() }).resolve_key().is_some() {
        return Ok(None);
    }
    let entry = cfg.auth.clone().unwrap_or_else(|| name.to_string());
    if !matches!(auth::read_from(store).get(&entry), Some(Credential::Oauth { .. })) {
        return Ok(None);
    }
    let (cfg, key) = login::prepare_in(store, at, cfg).await?;
    let Some(key) = key else { return Ok(None) };
    let now = auth::now() as i64;
    let usage = match kind {
        CODEX => {
            let base = cfg.base_url.trim_end_matches('/');
            let root = base.strip_suffix("/codex").unwrap_or(base);
            let mut h = crate::http::headers(&cfg, &[("accept", "application/json".to_string())]);
            if let Ok(v) = format!("Bearer {key}").parse() {
                h.insert(reqwest::header::AUTHORIZATION, v);
            }
            let v = crate::http::get_json(&format!("{root}/wham/usage"), h).await?;
            codex_from_usage(&v, now)
        }
        XAI => xai_from_billing(&xai_billing(at, &key).await?, now),
        _ => {
            let v = copilot_user(at, &key).await?;
            copilot_from_user(&v, now)
        }
    };
    if let Some(u) = &usage {
        record(u.clone());
    }
    Ok(usage)
}

async fn copilot_user(at: &Endpoints, token: &str) -> Result<Value, ProviderError> {
    let resp = crate::http::client()?
        .get(format!("{}/copilot_internal/user", at.github_api))
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("X-GitHub-Api-Version", "2025-04-01")
        .send()
        .await
        .map_err(crate::http::net_error)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_http(status, "GitHub did not give the Copilot plan"));
    }
    resp.json::<Value>().await.map_err(crate::http::net_error)
}

/// `GET {chat proxy}/billing?format=credits`, the request the Grok CLI's `/usage` makes.
async fn xai_billing(at: &Endpoints, token: &str) -> Result<Value, ProviderError> {
    let mut h = HeaderMap::new();
    h.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static("application/json"));
    if let Ok(v) = format!("Bearer {token}").parse() {
        h.insert(reqwest::header::AUTHORIZATION, v);
    }
    crate::http::get_json(&format!("{}/billing?format=credits", at.xai_billing.trim_end_matches('/')), h).await
}
