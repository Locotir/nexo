use std::time::Duration;

use nexo_core::auth::Credential;
use nexo_core::ProviderError;

use super::form::form;
use super::oauth::{fail, oauth_poll_error, poll, post, said, secs, text, tokens, Body, Poll, DEVICE_GRANT};
use super::{DeviceCode, Endpoints};

pub(super) const XAI_CLIENT: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

pub(super) async fn xai(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    let iss = &at.xai_issuer;
    let (st, v) = post(
        &format!("{iss}/device/code"),
        Body::Form(form(&[("client_id", XAI_CLIENT), ("scope", XAI_SCOPE), ("referrer", "nexo")])),
    )
    .await?;
    let (Some(device), Some(code)) = (text(&v, "device_code"), text(&v, "user_code")) else {
        return Err(fail(format!("xAI did not start the login ({st}): {}", said(&v))));
    };
    let url = text(&v, "verification_uri_complete").or_else(|| text(&v, "verification_uri")).unwrap_or_default();
    show(&DeviceCode { url, code });
    let interval = Duration::from_secs(secs(&v, "interval").unwrap_or(5));
    let token_url = format!("{iss}/token");
    let (st, v) = poll(at, interval, || async {
        let (st, v) = post(
            &token_url,
            Body::Form(form(&[("grant_type", DEVICE_GRANT), ("client_id", XAI_CLIENT), ("device_code", &device)])),
        )
        .await?;
        if text(&v, "access_token").is_some() {
            Ok(Poll::Done((st, v)))
        } else {
            oauth_poll_error(&v)
        }
    })
    .await?;
    tokens("xai", st, &v, None)
}
