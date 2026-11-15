use super::oauth::{oauth_poll_error, Poll};
use super::*;
use base64::Engine;
use httpmock::prelude::*;
use nexo_core::auth::{self, Credential};
use nexo_core::ProviderConfig;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

fn at(s: &MockServer) -> Endpoints {
    Endpoints {
        github: s.base_url(),
        github_api: s.base_url(),
        openai_issuer: s.base_url(),
        xai_issuer: s.base_url(),
        xai_billing: s.base_url(),
        codex_auth: Some(std::path::PathBuf::from("/nonexistent/nexo-test/auth.json")),
        callback_ports: vec![0],
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
        t.status(200)
            .json_body(json!({"chat_enabled": true, "endpoints": {"api": "https://api.business.githubcopilot.com/"}}));
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
        t.status(200).json_body(json!({"device_code": "dc", "user_code": "C", "verification_uri": "u", "interval": 0}));
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
    let cred = login_at(&at(&s), "codex-device", &|d| *seen.lock().unwrap() = Some(d.clone())).await.unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_sessions_renewing_the_same_login_at_once_renew_it_once() {
    let s = MockServer::start();
    let renew = s.mock(|w, t| {
        w.method(POST).path("/oauth/token").body_includes("refresh_token=rt1");
        t.status(200)
            .delay(Duration::from_millis(300))
            .json_body(json!({"access_token": "at2", "refresh_token": "rt2", "expires_in": 3600}));
    });
    let dir = std::env::temp_dir().join(format!("nexo-login-test-{}", std::process::id()));
    let store = dir.join("auth.json");
    let mut md = BTreeMap::new();
    md.insert("flow".to_string(), "codex".to_string());
    let old = Credential::Oauth { access: "at1".into(), refresh: "rt1".into(), expires: auth::now() + 5, metadata: md };
    auth::set_in(&store, "codex", old).unwrap();
    let mut cfg = ProviderConfig::new("openai-responses", CODEX_BASE_URL, None);
    cfg.auth = Some("codex".into());
    let e = at(&s);
    let (a, b) = tokio::join!(prepare_in(&store, &e, &cfg), prepare_in(&store, &e, &cfg));
    let (a, b) = (a.unwrap().1.unwrap(), b.unwrap().1.unwrap());
    let saved = auth::read_from(&store);
    let _ = std::fs::remove_dir_all(&dir);
    renew.assert_calls(1);
    assert!(a == "at2" && b == "at2", "both sessions use the renewed token");
    assert_eq!(saved["codex"].secret(), "at2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewal_given_up_by_its_request_still_saves_the_new_tokens() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(POST).path("/oauth/token").body_includes("refresh_token=rt1");
        t.status(200)
            .delay(Duration::from_millis(400))
            .json_body(json!({"access_token": "at2", "refresh_token": "rt2", "expires_in": 3600}));
    });
    let dir = std::env::temp_dir().join(format!("nexo-login-cancel-test-{}", std::process::id()));
    let store = dir.join("auth.json");
    let mut md = BTreeMap::new();
    md.insert("flow".to_string(), "codex".to_string());
    let old = Credential::Oauth { access: "at1".into(), refresh: "rt1".into(), expires: auth::now() + 5, metadata: md };
    auth::set_in(&store, "codex", old).unwrap();
    let mut cfg = ProviderConfig::new("openai-responses", CODEX_BASE_URL, None);
    cfg.auth = Some("codex".into());
    // the turn is interrupted while the server, which has spent rt1 already, still answers
    let e = at(&s);
    assert!(tokio::time::timeout(Duration::from_millis(100), prepare_in(&store, &e, &cfg)).await.is_err());
    let mut saved = auth::read_from(&store);
    for _ in 0..50 {
        if saved["codex"].secret() == "at2" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        saved = auth::read_from(&store);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(saved["codex"].secret(), "at2", "the rotated tokens are kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_renewal_lets_the_store_lock_go() {
    let s = MockServer::start();
    s.mock(|w, t| {
        w.method(POST).path("/oauth/token");
        t.status(200)
            .delay(Duration::from_secs(6))
            .json_body(json!({"access_token": "at2", "refresh_token": "rt2", "expires_in": 3600}));
    });
    let dir = std::env::temp_dir().join(format!("nexo-login-stall-test-{}", std::process::id()));
    let store = dir.join("auth.json");
    let mut md = BTreeMap::new();
    md.insert("flow".to_string(), "codex".to_string());
    let old = Credential::Oauth { access: "at1".into(), refresh: "rt1".into(), expires: auth::now() + 5, metadata: md };
    auth::set_in(&store, "codex", old).unwrap();
    let mut cfg = ProviderConfig::new("openai-responses", CODEX_BASE_URL, None);
    cfg.auth = Some("codex".into());
    let e = at(&s);
    let _ = tokio::time::timeout(Duration::from_millis(100), prepare_in(&store, &e, &cfg)).await;
    // another writer is not kept waiting for the stalled request
    let st = store.clone();
    let started = std::time::Instant::now();
    let wrote = tokio::task::spawn_blocking(move || auth::remove_in(&st, "other")).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(wrote.is_ok(), "{wrote:?}");
    assert!(started.elapsed() < Duration::from_secs(4), "the lock was held {:?}", started.elapsed());
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
