use std::collections::BTreeMap;
use std::time::Duration;

use nexo_core::auth::{self, Credential};
use nexo_core::{ErrorKind, ProviderError};
use serde_json::json;
use serde_json::Value;

use super::codex::account_id;
use super::Endpoints;
use crate::http;

pub(super) const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Device codes live about fifteen minutes.
const GIVE_UP: Duration = Duration::from_secs(15 * 60);

pub(crate) fn fail(msg: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Auth, msg)
}

pub(crate) enum Body {
    Json(Value),
    Form(String),
}

/// POSTs and returns the status with the body as JSON (`{}` when it is not).
pub(crate) async fn post(url: &str, body: Body) -> Result<(u16, Value), ProviderError> {
    let r = http::client()?.post(url).header("accept", "application/json");
    let r = match body {
        Body::Json(v) => r.json(&v),
        Body::Form(s) => r.header("content-type", "application/x-www-form-urlencoded").body(s),
    };
    let resp = r.send().await.map_err(http::net_error)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(http::net_error)?;
    Ok((status, serde_json::from_str(&text).unwrap_or_else(|_| json!({ "text": text }))))
}

pub(crate) fn text(v: &Value, k: &str) -> Option<String> {
    match &v[k] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) fn secs(v: &Value, k: &str) -> Option<u64> {
    text(v, k).and_then(|s| s.parse().ok())
}

pub(crate) fn said(v: &Value) -> String {
    text(v, "error_description").or_else(|| text(v, "error")).or_else(|| text(v, "text")).unwrap_or_default()
}

/// Polls `once` every `interval` (plus the pad) until it gives a value or
/// the code expires. `once` returns Ok(None) while the person has not
/// finished, or asks to slow down through the interval it is handed.
pub(super) async fn poll<T, F, Fut>(at: &Endpoints, mut interval: Duration, mut once: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Poll<T>, ProviderError>>,
{
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(interval + at.pad).await;
        match once().await? {
            Poll::Done(t) => return Ok(t),
            Poll::Pending => {}
            Poll::SlowDown(next) => interval = next.unwrap_or(interval + Duration::from_secs(5)),
        }
        if start.elapsed() > GIVE_UP {
            return Err(fail("the login code expired before it was entered; run the login again"));
        }
    }
}

pub(super) enum Poll<T> {
    Done(T),
    Pending,
    SlowDown(Option<Duration>),
}

/// The standard device-token errors: wait, slow down, or give up.
pub(super) fn oauth_poll_error<T>(v: &Value) -> Result<Poll<T>, ProviderError> {
    match text(v, "error").as_deref() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown(secs(v, "interval").map(Duration::from_secs))),
        Some("expired_token") => Err(fail("the login code expired; run the login again")),
        Some("access_denied" | "authorization_denied") => Err(fail("the login was declined")),
        _ => Err(fail(format!("login failed: {}", said(v)))),
    }
}

/// A token response turned into a credential; `old` keeps what a refresh
/// does not repeat (the refresh token, the account).
pub(crate) fn tokens(flow: &str, st: u16, v: &Value, old: Option<&Credential>) -> Result<Credential, ProviderError> {
    let Some(access) = text(v, "access_token") else {
        return Err(fail(format!("{flow} login: no token ({st}): {}", said(v))));
    };
    let mut metadata = match old {
        Some(Credential::Oauth { metadata, .. }) => metadata.clone(),
        _ => BTreeMap::new(),
    };
    metadata.insert("flow".into(), flow.into());
    if flow == "codex" {
        let id = text(v, "id_token").and_then(|t| account_id(&t)).or_else(|| account_id(&access));
        if let Some(id) = id {
            metadata.insert("account_id".into(), id);
        }
    }
    let refresh = text(v, "refresh_token")
        .or_else(|| match old {
            Some(Credential::Oauth { refresh, .. }) => Some(refresh.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let expires = secs(v, "expires_in").map(|s| auth::now() + s).unwrap_or(0);
    Ok(Credential::Oauth { access, refresh, expires, metadata })
}
