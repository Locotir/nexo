//! Subscription logins: sign in once in the browser with a code (the OAuth
//! device flow), and nexo keeps the tokens in the credential store
//! ([`nexo_core::auth`]) and refreshes them before they expire.
//!
//! | flow      | what you pay for                  | endpoint it unlocks                        |
//! |-----------|-----------------------------------|--------------------------------------------|
//! | `copilot` | GitHub Copilot (Free, Pro, ...)   | `api.githubcopilot.com` (protocol `copilot`) |
//! | `codex`   | ChatGPT Plus / Pro / Team         | `chatgpt.com/backend-api/codex` (openai-responses) |
//! | `xai`     | SuperGrok / X Premium             | `api.x.ai/v1` (openai-chat)                  |
//!
//! [`login`] runs a flow, [`prepare`] turns a provider's configuration into
//! the one a request goes out with: the key, refreshed when needed, and what
//! the login learned (the Copilot endpoint, the ChatGPT account).

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use nexo_core::auth::{self, Credential};
use nexo_core::{ErrorKind, ProviderConfig, ProviderError};
use serde_json::{json, Value};

use crate::http;

/// The flows nexo can run, with a line on each.
pub const FLOWS: &[(&str, &str)] = &[
    ("copilot", "GitHub Copilot subscription (github.com device code)"),
    ("codex", "ChatGPT Plus/Pro/Team subscription (auth.openai.com device code)"),
    ("xai", "SuperGrok / X Premium subscription (auth.x.ai device code)"),
];

pub fn is_flow(name: &str) -> bool {
    FLOWS.iter().any(|(f, _)| *f == name)
}

/// Where each flow talks to; tests point these at a local server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub github: String,
    pub github_api: String,
    pub openai_issuer: String,
    pub xai_issuer: String,
    /// Added to every wait between two polls (GitHub asks for a margin).
    pub pad: Duration,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            github: "https://github.com".into(),
            github_api: "https://api.github.com".into(),
            openai_issuer: "https://auth.openai.com".into(),
            xai_issuer: "https://auth.x.ai/oauth2".into(),
            pad: Duration::from_secs(3),
        }
    }
}

const COPILOT_CLIENT: &str = "Ov23li8tweQw6odWQebz";
const CODEX_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const XAI_CLIENT: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Device codes live about fifteen minutes.
const GIVE_UP: Duration = Duration::from_secs(15 * 60);

pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// What the person has to do: open `url`, type `code`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub url: String,
    pub code: String,
}

/// Runs `flow`; `show` is told the address and the code to type there.
pub async fn login(flow: &str, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    login_at(&Endpoints::default(), flow, show).await
}

pub async fn login_at(
    at: &Endpoints,
    flow: &str,
    show: &(dyn Fn(&DeviceCode) + Sync),
) -> Result<Credential, ProviderError> {
    match flow {
        "copilot" => copilot(at, show).await,
        "codex" => codex(at, show).await,
        "xai" => xai(at, show).await,
        other => Err(fail(format!(
            "no login flow {other:?} (known: {})",
            FLOWS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
        ))),
    }
}

fn fail(msg: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Auth, msg)
}

enum Body {
    Json(Value),
    Form(String),
}

/// POSTs and returns the status with the body as JSON (`{}` when it is not).
async fn post(url: &str, body: Body) -> Result<(u16, Value), ProviderError> {
    let r = http::client().post(url).header("accept", "application/json");
    let r = match body {
        Body::Json(v) => r.json(&v),
        Body::Form(s) => r.header("content-type", "application/x-www-form-urlencoded").body(s),
    };
    let resp = r.send().await.map_err(http::net_error)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(http::net_error)?;
    Ok((status, serde_json::from_str(&text).unwrap_or_else(|_| json!({ "text": text }))))
}

/// `a=1&b=x%20y`: the body of a form POST.
pub fn form(pairs: &[(&str, &str)]) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

fn text(v: &Value, k: &str) -> Option<String> {
    match &v[k] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn secs(v: &Value, k: &str) -> Option<u64> {
    text(v, k).and_then(|s| s.parse().ok())
}

fn said(v: &Value) -> String {
    text(v, "error_description").or_else(|| text(v, "error")).or_else(|| text(v, "text")).unwrap_or_default()
}

/// Polls `once` every `interval` (plus the pad) until it gives a value or
/// the code expires. `once` returns Ok(None) while the person has not
/// finished, or asks to slow down through the interval it is handed.
async fn poll<T, F, Fut>(at: &Endpoints, mut interval: Duration, mut once: F) -> Result<T, ProviderError>
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

enum Poll<T> {
    Done(T),
    Pending,
    SlowDown(Option<Duration>),
}

/// The standard device-token errors: wait, slow down, or give up.
fn oauth_poll_error<T>(v: &Value) -> Result<Poll<T>, ProviderError> {
    match text(v, "error").as_deref() {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown(secs(v, "interval").map(Duration::from_secs))),
        Some("expired_token") => Err(fail("the login code expired; run the login again")),
        Some("access_denied" | "authorization_denied") => Err(fail("the login was declined")),
        _ => Err(fail(format!("login failed: {}", said(v)))),
    }
}

// ---- GitHub Copilot ----

async fn copilot(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
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
    let r = http::client()
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

// ---- ChatGPT (Codex) ----

async fn codex(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
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

/// A token response turned into a credential; `old` keeps what a refresh
/// does not repeat (the refresh token, the account).
fn tokens(flow: &str, st: u16, v: &Value, old: Option<&Credential>) -> Result<Credential, ProviderError> {
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

// ---- xAI ----

async fn xai(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
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

// ---- refresh and use ----

/// A fresh access token for `cred`, when its flow has one to give.
pub async fn refresh(cred: &Credential) -> Result<Credential, ProviderError> {
    refresh_at(&Endpoints::default(), cred).await
}

pub async fn refresh_at(at: &Endpoints, cred: &Credential) -> Result<Credential, ProviderError> {
    let Credential::Oauth { refresh, .. } = cred else { return Ok(cred.clone()) };
    let flow = cred.metadata("flow").unwrap_or_default().to_string();
    let (url, client) = match flow.as_str() {
        "codex" => (format!("{}/oauth/token", at.openai_issuer), CODEX_CLIENT),
        "xai" => (format!("{}/token", at.xai_issuer), XAI_CLIENT),
        _ => return Ok(cred.clone()),
    };
    if refresh.is_empty() {
        return Err(fail(format!("the {flow} login has expired: run `nexo auth login` again")));
    }
    let (st, v) = post(
        &url,
        Body::Form(form(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client)])),
    )
    .await?;
    if !(200..300).contains(&st) {
        return Err(fail(format!(
            "the {flow} login could not be renewed ({st}: {}); run `nexo auth login` again",
            said(&v)
        )));
    }
    tokens(&flow, st, &v, Some(cred))
}

/// The configuration and key a request to `cfg` goes out with. A key from
/// the environment, a file or a command wins; else the store's entry (under
/// `auth`) is used, renewed first when it is about to expire, and what its
/// login learned is applied.
pub async fn prepare(cfg: &ProviderConfig) -> Result<(ProviderConfig, Option<String>), ProviderError> {
    let direct = ProviderConfig { auth: None, ..cfg.clone() };
    if let Some(k) = direct.resolve_key() {
        return Ok((cfg.clone(), Some(k)));
    }
    let Some(name) = cfg.auth.as_deref() else { return Ok((cfg.clone(), None)) };
    let Some(mut cred) = auth::get(name) else { return Ok((cfg.clone(), None)) };
    if cred.expiring(auth::now(), 60) {
        cred = refresh(&cred).await?;
        // a store that cannot be written still leaves this request working
        if let Err(e) = auth::set(name, cred.clone()) {
            tracing::warn!("could not save the renewed login for {name}: {e}");
        }
    }
    Ok((apply(cfg, &cred), Some(cred.secret().to_string())))
}

/// What a login adds to the provider's configuration.
pub fn apply(cfg: &ProviderConfig, cred: &Credential) -> ProviderConfig {
    let mut c = cfg.clone();
    match cred.metadata("flow") {
        Some("copilot") => {
            if let Some(base) = cred.metadata("base_url") {
                if c.base_url == crate::copilot::BASE_URL {
                    c.base_url = base.to_string();
                }
            }
        }
        Some("codex") => {
            let mut set = |k: &str, v: &str| {
                c.headers.entry(k.to_string()).or_insert_with(|| v.to_string());
            };
            set("originator", "nexo");
            set("x-codex-beta-features", "remote_compaction_v2");
            if let Some(id) = cred.metadata("account_id") {
                set("chatgpt-account-id", id);
            }
            c.extra.entry("omit_max_tokens".into()).or_insert(toml::Value::Boolean(true));
        }
        _ => {}
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::sync::Mutex;

    fn at(s: &MockServer) -> Endpoints {
        Endpoints {
            github: s.base_url(),
            github_api: s.base_url(),
            openai_issuer: s.base_url(),
            xai_issuer: s.base_url(),
            pad: Duration::ZERO,
        }
    }

    fn jwt(claims: Value) -> String {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", b.encode(b"{}"), b.encode(claims.to_string()))
    }

    #[test]
    fn forms_are_url_encoded() {
        assert_eq!(form(&[("a", "x y"), ("scope", "openid api:access")]), "a=x%20y&scope=openid%20api%3Aaccess");
    }

    #[test]
    fn the_chatgpt_account_is_read_from_the_token() {
        assert_eq!(account_id(&jwt(json!({"chatgpt_account_id": "acc1"}))).as_deref(), Some("acc1"));
        assert_eq!(
            account_id(&jwt(json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acc2"}}))).as_deref(),
            Some("acc2")
        );
        assert_eq!(account_id(&jwt(json!({"organizations": [{"id": "org3"}]}))).as_deref(), Some("org3"));
        assert_eq!(account_id("nope"), None);
    }

    #[tokio::test]
    async fn copilot_login_checks_the_subscription_and_finds_its_endpoint() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(POST).path("/login/device/code").json_body_includes(r#"{"scope":"read:user"}"#);
            t.status(200).json_body(json!({"device_code": "dc", "user_code": "ABCD-1234", "verification_uri": "https://github.com/login/device", "interval": 0}));
        });
        s.mock(|w, t| {
            w.method(POST).path("/login/oauth/access_token").json_body_includes(r#"{"device_code":"dc"}"#);
            t.status(200).json_body(json!({"access_token": "gho_x"}));
        });
        s.mock(|w, t| {
            w.method(GET).path("/copilot_internal/user").header("authorization", "Bearer gho_x");
            t.status(200).json_body(
                json!({"chat_enabled": true, "endpoints": {"api": "https://api.business.githubcopilot.com/"}}),
            );
        });
        let seen = Mutex::new(None);
        let cred = login_at(&at(&s), "copilot", &|d| *seen.lock().unwrap() = Some(d.clone())).await.unwrap();
        assert_eq!(seen.lock().unwrap().as_ref().unwrap().code, "ABCD-1234");
        assert_eq!(cred.secret(), "gho_x");
        assert_eq!(cred.metadata("base_url"), Some("https://api.business.githubcopilot.com"));
        let cfg = ProviderConfig::new("copilot", crate::copilot::BASE_URL, None);
        assert_eq!(apply(&cfg, &cred).base_url, "https://api.business.githubcopilot.com");
    }

    #[tokio::test]
    async fn an_account_without_copilot_is_told_so() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(POST).path("/login/device/code");
            t.status(200)
                .json_body(json!({"device_code": "dc", "user_code": "C", "verification_uri": "u", "interval": 0}));
        });
        s.mock(|w, t| {
            w.method(POST).path("/login/oauth/access_token");
            t.status(200).json_body(json!({"access_token": "gho_y"}));
        });
        s.mock(|w, t| {
            w.method(GET).path("/copilot_internal/user");
            t.status(200).json_body(json!({"chat_enabled": false, "can_signup_for_limited": true}));
        });
        let e = login_at(&at(&s), "copilot", &|_| {}).await.unwrap_err();
        assert!(e.message.contains("Copilot Free"), "{}", e.message);
    }

    #[tokio::test]
    async fn chatgpt_login_trades_the_code_for_tokens_and_finds_the_account() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(POST).path("/api/accounts/deviceauth/usercode");
            t.status(200).json_body(json!({"device_auth_id": "da", "user_code": "QW-ER", "interval": "0"}));
        });
        s.mock(|w, t| {
            w.method(POST).path("/api/accounts/deviceauth/token").json_body_includes(r#"{"device_auth_id":"da"}"#);
            t.status(200).json_body(json!({"authorization_code": "ac", "code_verifier": "cv"}));
        });
        s.mock(|w, t| {
            w.method(POST)
                .path("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body_includes("grant_type=authorization_code")
                .body_includes("code_verifier=cv");
            t.status(200).json_body(json!({
                "id_token": jwt(json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct"}})),
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3600
            }));
        });
        let seen = Mutex::new(None);
        let cred = login_at(&at(&s), "codex", &|d| *seen.lock().unwrap() = Some(d.clone())).await.unwrap();
        assert_eq!(seen.lock().unwrap().as_ref().unwrap().url, format!("{}/codex/device", s.base_url()));
        let Credential::Oauth { access, refresh, expires, .. } = &cred else { panic!() };
        assert_eq!((access.as_str(), refresh.as_str()), ("at1", "rt1"));
        assert!(*expires > auth::now());
        assert_eq!(cred.metadata("account_id"), Some("acct"));
        let cfg = apply(&ProviderConfig::new("openai-responses", CODEX_BASE_URL, None), &cred);
        assert_eq!(cfg.headers["chatgpt-account-id"], "acct");
        assert_eq!(cfg.headers["originator"], "nexo");
        assert_eq!(cfg.extra["omit_max_tokens"], toml::Value::Boolean(true));

        // renewing keeps the account and swaps the tokens
        s.mock(|w, t| {
            w.method(POST)
                .path("/oauth/token")
                .body_includes("grant_type=refresh_token")
                .body_includes("refresh_token=rt1");
            t.status(200).json_body(json!({"access_token": "at2", "refresh_token": "rt2", "expires_in": 3600}));
        });
        let renewed = refresh_at(&at(&s), &cred).await.unwrap();
        assert_eq!(renewed.secret(), "at2");
        assert_eq!(renewed.metadata("account_id"), Some("acct"));
    }

    #[tokio::test]
    async fn xai_login_polls_with_the_device_grant_and_slows_down_when_asked() {
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(POST).path("/device/code").body_includes("scope=openid").body_includes("referrer=nexo");
            t.status(200).json_body(json!({"device_code": "xd", "user_code": "XY", "verification_uri": "https://accounts.x.ai/device", "interval": 0}));
        });
        s.mock(|w, t| {
            w.method(POST).path("/token").body_includes("device_code=xd");
            t.status(200).json_body(json!({"access_token": "xat", "refresh_token": "xrt", "expires_in": 600}));
        });
        let cred = login_at(&at(&s), "xai", &|_| {}).await.unwrap();
        assert_eq!(cred.secret(), "xat");
        assert_eq!(cred.metadata("flow"), Some("xai"));
        assert!(
            matches!(oauth_poll_error::<()>(&json!({"error": "slow_down", "interval": 9})), Ok(Poll::SlowDown(Some(d))) if d == Duration::from_secs(9))
        );
        assert!(oauth_poll_error::<()>(&json!({"error": "access_denied"})).is_err());
        assert!(login_at(&at(&s), "nope", &|_| {}).await.is_err());
    }
}
