//! Unofficial Claude Code OAuth (authorization code + PKCE).
//!
//! The caller must display the warning and obtain explicit consent before calling
//! `begin`. Do not log the returned URL, pasted code, or this request object.

use std::collections::BTreeMap;

use nexo_core::auth::{self, Credential};
use nexo_core::ProviderError;
use serde_json::Value;

use super::{fail, form};

pub const WARNING: &str = "Using Claude Code OAuth in Nexo may violate Anthropic's policies or terms. Your account may be restricted. Continue only if you understand and accept these risks; otherwise use an API key.";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const SCOPE: &str = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// One authorization attempt. Deliberately does not implement Debug or Clone.
pub struct Authorization {
    url: String,
    verifier: String,
    state: String,
}

impl Authorization {
    /// Open only after the user has accepted the warning; never log this URL.
    pub fn url(&self) -> &str { &self.url }

    /// Exchange the pasted `code#state`; reject a missing or mismatched state.
    pub async fn exchange(self, code: &str) -> Result<Credential, ProviderError> {
        self.exchange_at(TOKEN_URL, code).await
    }

    /// As `exchange`, with an injectable token endpoint for offline tests.
    async fn exchange_at(self, token_url: &str, raw: &str) -> Result<Credential, ProviderError> {
        let raw = raw.trim();
        let (code, state) = raw.split_once('#')
            .ok_or_else(|| fail("Claude authorization state is missing; paste the complete code and start again"))?;
        if state != self.state {
            return Err(fail("Claude authorization state does not match; start again"));
        }
        if code.is_empty() || code.chars().any(char::is_whitespace) {
            return Err(fail("Claude authorization code is empty or invalid"));
        }
        let body = form(&[
            ("grant_type", "authorization_code"), ("code", code),
            ("code_verifier", &self.verifier), ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI), ("state", &self.state),
        ]);
        request(token_url, body, None).await
    }
}

/// Consent must be an explicit affirmative response to `WARNING`. This function
/// does no network I/O, and refusal does not generate an authorization request.
pub fn begin(consent: bool) -> Result<Authorization, ProviderError> {
    if !consent { return Err(fail("Claude OAuth consent was not granted")); }
    let pkce = nexo_http::pkce::pkce();
    // An independent random value; never expose the PKCE verifier in the URL.
    let state = nexo_http::pkce::pkce().verifier;
    let query = form(&[
        ("code", "true"), ("response_type", "code"), ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI), ("scope", SCOPE),
        ("code_challenge", &pkce.challenge), ("code_challenge_method", "S256"),
        ("state", &state),
    ]);
    Ok(Authorization { url: format!("{AUTHORIZE_URL}?{query}"), verifier: pkce.verifier, state })
}

/// Refresh an existing Claude credential. Call under the credential-store lock:
/// refresh tokens may rotate and must not be spent concurrently.
pub async fn refresh(cred: &Credential) -> Result<Credential, ProviderError> {
    refresh_at(TOKEN_URL, cred).await
}

async fn refresh_at(token_url: &str, cred: &Credential) -> Result<Credential, ProviderError> {
    let Credential::Oauth { refresh, .. } = cred else { return Err(fail("not a Claude OAuth credential")); };
    if cred.metadata("flow") != Some("claude") || refresh.is_empty() {
        return Err(fail("Claude OAuth refresh token unavailable; sign in again"));
    }
    let body = form(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", CLIENT_ID)]);
    request(token_url, body, Some(cred)).await
}

async fn request(url: &str, body: String, old: Option<&Credential>) -> Result<Credential, ProviderError> {
    // Never include server bodies, request URLs, or transport errors: any of
    // these may contain credentials or an echoed authorization code.
    // 307/308 redirects preserve POST bodies, including refresh tokens and PKCE
    // verifiers. Never follow redirects from the token endpoint.
    let client = crate::tls::build_client(|| reqwest::Client::builder()
        .user_agent(crate::http::USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none()))
        .map_err(|_| fail("Claude OAuth token request failed"))?;
    let response = client.post(url).header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded").body(body)
        .send().await.map_err(|_| fail("Claude OAuth token request failed"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(fail(format!("Claude OAuth token request failed (HTTP {status})")));
    }
    let value = response.json::<Value>().await.map_err(|_| fail("Claude OAuth token response was invalid"))?;
    parse_tokens(&value, old)
}

fn parse_tokens(value: &Value, old: Option<&Credential>) -> Result<Credential, ProviderError> {
    let invalid = || fail("Claude OAuth token response was invalid");
    let access = value.get("access_token").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(invalid)?;
    let refresh = value.get("refresh_token").and_then(Value::as_str).filter(|s| !s.is_empty())
        .or_else(|| match old { Some(Credential::Oauth { refresh, .. }) if !refresh.is_empty() => Some(refresh.as_str()), _ => None })
        .ok_or_else(invalid)?;
    let seconds = value.get("expires_in").and_then(Value::as_u64).filter(|n| *n > 0).ok_or_else(invalid)?;
    let expires = auth::now().checked_add(seconds).ok_or_else(invalid)?;
    let mut metadata = match old { Some(Credential::Oauth { metadata, .. }) => metadata.clone(), _ => BTreeMap::new() };
    metadata.insert("flow".into(), "claude".into());
    Ok(Credential::Oauth { access: access.into(), refresh: refresh.into(), expires, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use serde_json::json;

    #[test]
    fn consent_and_pkce() {
        assert!(begin(false).is_err());
        let a = begin(true).unwrap();
        let url = reqwest::Url::parse(a.url()).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["state"], a.state);
        assert_ne!(q["state"], a.verifier);
        assert!(!a.url().contains(&a.verifier));
        assert_eq!(q["code_challenge"], nexo_http::pkce::challenge_of(&a.verifier));
        assert_eq!(q["code_challenge_method"], "S256");
        let next = begin(true).unwrap();
        assert_ne!(a.verifier, next.verifier);
        assert_ne!(a.state, next.state);
    }

    #[tokio::test]
    async fn exchange_refresh_and_redact() {
        let server = MockServer::start_async().await;
        let exchange = server.mock_async(|when, then| {
            when.method(POST).path("/exchange").body_includes("grant_type=authorization_code").body_includes("code=secretcode").body_includes("code_verifier=").body_includes("state=");
            then.status(200).json_body(json!({"access_token":"access", "refresh_token":"old", "expires_in":3600}));
        }).await;
        let a = begin(true).unwrap();
        let code = format!("secretcode#{}", a.state);
        let cred = a.exchange_at(&server.url("/exchange"), &code).await.unwrap();
        exchange.assert_async().await;
        assert_eq!(cred.metadata("flow"), Some("claude"));
        assert!(cred.expiring(auth::now() + 3600, 0));
        let refresh = server.mock_async(|when, then| {
            when.method(POST).path("/refresh").body_includes("refresh_token=old");
            then.status(200).json_body(json!({"access_token":"new", "refresh_token":"rotated", "expires_in":100}));
        }).await;
        let renewed = refresh_at(&server.url("/refresh"), &cred).await.unwrap();
        refresh.assert_async().await;
        assert!(matches!(renewed, Credential::Oauth { ref refresh, .. } if refresh == "rotated"));
        let bad = server.mock_async(|when, then| { when.path("/bad"); then.status(400).body("secretcode old access"); }).await;
        let err = refresh_at(&server.url("/bad"), &cred).await.unwrap_err().to_string();
        bad.assert_async().await;
        assert!(!err.contains("old") && !err.contains("secretcode") && !err.contains("access"));
        for raw in ["secretcode#wrong", "secretcode", "secretcode#", "#wrong", "secretcode#wrong#extra"] {
            let err = begin(true).unwrap().exchange_at(&server.url("/exchange"), raw).await.unwrap_err().to_string();
            assert!(!err.contains("secretcode"));
        }
        exchange.assert_calls_async(1).await;
    }

    #[tokio::test]
    async fn token_requests_never_follow_redirects() {
        let origin = MockServer::start_async().await;
        let destination = MockServer::start_async().await;
        let leaked = destination.mock_async(|when, then| { when.any_request(); then.status(200); }).await;
        for status in [301, 302, 303, 307, 308] {
            let path = format!("/redirect{status}");
            let redirect = origin.mock_async(|when, then| {
                when.method(POST).path(&path);
                then.status(status).header("location", &destination.url("/leak"));
            }).await;
            let cred = Credential::Oauth { access: "a".into(), refresh: "secret".into(), expires: 0,
                metadata: BTreeMap::from([("flow".into(), "claude".into())]) };
            let err = refresh_at(&origin.url(&path), &cred).await.unwrap_err().to_string();
            assert!(err.contains(&format!("HTTP {status}")));
            redirect.assert_async().await;
        }
        leaked.assert_calls_async(0).await;
    }

    #[test]
    fn rejects_invalid_responses_without_echoing() {
        let err = parse_tokens(&json!({"error":"secret", "access_token":"secret"}), None).unwrap_err().to_string();
        assert!(!err.contains("secret"));
        assert!(parse_tokens(&json!({"access_token":"a", "refresh_token":"r", "expires_in":0}), None).is_err());
    }
}
