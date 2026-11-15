use std::time::Duration;

use base64::Engine;
use nexo_core::auth::Credential;
use nexo_core::ProviderError;
use serde_json::{json, Value};

use super::form;
use super::oauth::{fail, poll, post, said, secs, text, tokens, Body, Poll};
use super::{DeviceCode, Endpoints, CODEX_CLIENT};

pub(super) async fn codex(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    let iss = &at.openai_issuer;
    let (st, v) =
        post(&format!("{iss}/api/accounts/deviceauth/usercode"), Body::Json(json!({"client_id": CODEX_CLIENT})))
            .await?;
    let (Some(id), Some(code)) = (text(&v, "device_auth_id"), text(&v, "user_code").or_else(|| text(&v, "usercode")))
    else {
        return Err(fail(format!("ChatGPT did not start the login ({st}): {}", said(&v))));
    };
    show(&DeviceCode { url: format!("{iss}/codex/device"), code: code.clone() });
    let interval = Duration::from_secs(secs(&v, "interval").unwrap_or(5));
    let poll_url = format!("{iss}/api/accounts/deviceauth/token");
    let (auth_code, verifier) = poll(at, interval, || async {
        let (st, v) = post(&poll_url, Body::Json(json!({"device_auth_id": id, "user_code": code}))).await?;
        match st {
            200 => match (text(&v, "authorization_code"), text(&v, "code_verifier")) {
                (Some(c), Some(ver)) => Ok(Poll::Done((c, ver))),
                _ => Err(fail("ChatGPT answered the login without a code")),
            },
            403 | 404 => Ok(Poll::Pending),
            _ => Err(fail(format!("login failed ({st}): {}", said(&v)))),
        }
    })
    .await?;
    let redirect = format!("{iss}/deviceauth/callback");
    let (st, v) = post(
        &format!("{iss}/oauth/token"),
        Body::Form(form(&[
            ("grant_type", "authorization_code"),
            ("code", &auth_code),
            ("redirect_uri", &redirect),
            ("client_id", CODEX_CLIENT),
            ("code_verifier", &verifier),
        ])),
    )
    .await?;
    tokens("codex", st, &v, None)
}

/// The ChatGPT account a token belongs to, from its claims.
pub fn account_id(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    text(&v, "chatgpt_account_id")
        .or_else(|| text(&v["https://api.openai.com/auth"], "chatgpt_account_id"))
        .or_else(|| text(&v["organizations"][0], "id"))
}
