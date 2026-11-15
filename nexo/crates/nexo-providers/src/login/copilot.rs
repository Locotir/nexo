use std::collections::BTreeMap;
use std::time::Duration;

use nexo_core::auth::Credential;
use nexo_core::ProviderError;
use serde_json::{json, Value};

use super::oauth::{fail, oauth_poll_error, poll, post, said, secs, text, Body, Poll, DEVICE_GRANT};
use super::{DeviceCode, Endpoints};
use crate::http;

const COPILOT_CLIENT: &str = "Ov23li8tweQw6odWQebz";

pub(super) async fn copilot(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    let (st, v) = post(
        &format!("{}/login/device/code", at.github),
        Body::Json(json!({"client_id": COPILOT_CLIENT, "scope": "read:user"})),
    )
    .await?;
    let (Some(device), Some(code), Some(url)) =
        (text(&v, "device_code"), text(&v, "user_code"), text(&v, "verification_uri"))
    else {
        return Err(fail(format!("GitHub did not start the login ({st}): {}", said(&v))));
    };
    show(&DeviceCode { url, code });
    let interval = Duration::from_secs(secs(&v, "interval").unwrap_or(5));
    let token_url = format!("{}/login/oauth/access_token", at.github);
    let token = poll(at, interval, || async {
        let (_, v) = post(
            &token_url,
            Body::Json(json!({"client_id": COPILOT_CLIENT, "device_code": device, "grant_type": DEVICE_GRANT})),
        )
        .await?;
        match text(&v, "access_token") {
            Some(t) => Ok(Poll::Done(t)),
            None => oauth_poll_error(&v),
        }
    })
    .await?;
    let mut metadata = BTreeMap::from([("flow".to_string(), "copilot".to_string())]);
    if let Some(base) = copilot_entitlement(at, &token).await? {
        metadata.insert("base_url".into(), base);
    }
    Ok(Credential::Oauth { access: token.clone(), refresh: token, expires: 0, metadata })
}

/// Asks GitHub whether the account has Copilot, and where its API lives.
/// An account without it is an error; a lookup that fails is not.
async fn copilot_entitlement(at: &Endpoints, token: &str) -> Result<Option<String>, ProviderError> {
    let r = http::client()?
        .get(format!("{}/copilot_internal/user", at.github_api))
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("X-GitHub-Api-Version", "2025-04-01")
        .send()
        .await;
    let Ok(resp) = r else { return Ok(None) };
    if !resp.status().is_success() {
        return Ok(None);
    }
    let Ok(v) = resp.json::<Value>().await else { return Ok(None) };
    if v["chat_enabled"] == json!(false) {
        let hint = if v["can_signup_for_limited"] == json!(true) {
            " Copilot Free is available: enable it at https://github.com/settings/copilot"
        } else {
            ""
        };
        return Err(fail(format!("this GitHub account has no Copilot chat access.{hint}")));
    }
    Ok(text(&v["endpoints"], "api").map(|s| s.trim_end_matches('/').to_string()))
}
