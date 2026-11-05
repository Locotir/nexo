//! Signing in with a ChatGPT subscription, beyond the device code: the
//! browser flow (OAuth with PKCE and a callback on `localhost`), and reuse of
//! the login the Codex CLI keeps in `auth.json`.
//!
//! The browser flow opens `{issuer}/oauth/authorize`, waits for the redirect
//! to `http://localhost:{port}/auth/callback` (port 1455, else 1457: the
//! ones the login client is registered for), and trades the code for tokens
//! at `{issuer}/oauth/token`. The tokens go to nexo's credential store like
//! any other login.
//!
//! A login read from the Codex CLI's file stays tied to that file: when the
//! access token runs out, nexo first looks whether the CLI renewed it
//! meanwhile, and when it renews it itself, it writes the new tokens back
//! there too, because the refresh token is single use and the CLI would
//! otherwise hold a spent one. No token is ever printed or put in an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use nexo_core::auth::{self, Credential};
use nexo_core::ProviderError;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::login::{fail, form, post, text, tokens, Body, DeviceCode, Endpoints, CODEX_CLIENT};

/// The ports the login client accepts a redirect on, in the order tried.
pub const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const SCOPE: &str = "openid profile email offline_access";
/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(10 * 60);
/// How long one connection to the callback may take to send its request.
const REQUEST: Duration = Duration::from_secs(5);

// ---- PKCE ----

/// A code verifier and the challenge sent in its place.
#[derive(Debug, Clone, PartialEq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// `BASE64URL(SHA256(verifier))`, no padding (RFC 7636, S256).
pub fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh verifier: 64 characters from 256 random bits, within the 43..128 the RFC allows.
pub fn pkce() -> Pkce {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    Pkce { challenge: challenge_of(&verifier), verifier }
}

fn state() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The address the person opens to sign in.
pub fn authorize_url(issuer: &str, redirect: &str, challenge: &str, state: &str) -> String {
    let query = form(&[
        ("response_type", "code"),
        ("client_id", CODEX_CLIENT),
        ("redirect_uri", redirect),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "nexo"),
    ]);
    format!("{}/oauth/authorize?{query}", issuer.trim_end_matches('/'))
}

// ---- the browser flow ----

/// Runs the browser flow: `show` is told the address to open (its `code` is empty), and the
/// call returns when the browser has come back and the code is traded for tokens.
pub async fn browser(at: &Endpoints, show: &(dyn Fn(&DeviceCode) + Sync)) -> Result<Credential, ProviderError> {
    let (listener, port) = bind(&at.callback_ports).await?;
    let redirect = format!("http://localhost:{port}/auth/callback");
    let pkce = pkce();
    let state = state();
    show(&DeviceCode {
        url: authorize_url(&at.openai_issuer, &redirect, &pkce.challenge, &state),
        code: String::new(),
    });
    let code = tokio::time::timeout(WAIT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| fail("the browser did not come back in ten minutes; run the login again"))??;
    drop(listener);
    let (st, v) = post(
        &format!("{}/oauth/token", at.openai_issuer.trim_end_matches('/')),
        Body::Form(form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("client_id", CODEX_CLIENT),
            ("code_verifier", &pkce.verifier),
        ])),
    )
    .await?;
    tokens("codex", st, &v, None)
}

/// The first of `ports` that is free (port 0 asks the system for one).
async fn bind(ports: &[u16]) -> Result<(TcpListener, u16), ProviderError> {
    for p in ports {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", *p)).await {
            if let Ok(addr) = l.local_addr() {
                return Ok((l, addr.port()));
            }
        }
    }
    let tried = ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ");
    Err(fail(format!(
        "port {tried} is in use (another sign-in running?); close it, or use the device code: `nexo auth login codex --flow codex-device`"
    )))
}

enum Callback {
    Code(String),
    Denied(String),
    /// Not the redirect (a favicon, a wrong state): keep waiting.
    Other,
}

async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String, ProviderError> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| fail(format!("the login callback failed: {e}")))?;
        let outcome = tokio::time::timeout(REQUEST, serve(&mut sock, state)).await.unwrap_or(Callback::Other);
        let _ = sock.shutdown().await;
        match outcome {
            Callback::Code(c) => return Ok(c),
            Callback::Denied(why) => return Err(fail(format!("the login was declined: {why}"))),
            Callback::Other => {}
        }
    }
}

/// Reads one request, answers it, and says what it was.
async fn serve(sock: &mut tokio::net::TcpStream, state: &str) -> Callback {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match sock.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let target = head.lines().next().and_then(|l| l.strip_prefix("GET ")).and_then(|l| l.split(' ').next());
    let Some((path, query)) = target.map(|t| t.split_once('?').unwrap_or((t, ""))) else {
        reply(sock, 400, "Bad request").await;
        return Callback::Other;
    };
    if path != "/auth/callback" {
        reply(sock, 404, "Not found").await;
        return Callback::Other;
    }
    let q = parse_query(query);
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("state") != Some(state) {
        reply(sock, 400, "This sign-in link does not belong to this session. Run the login again.").await;
        return Callback::Other;
    }
    if let Some(e) = get("error") {
        let why = get("error_description").unwrap_or(e).to_string();
        reply(sock, 200, "The sign-in was declined. You can close this tab.").await;
        return Callback::Denied(why);
    }
    match get("code").filter(|c| !c.is_empty()) {
        Some(c) => {
            reply(sock, 200, "Signed in. You can close this tab and go back to the terminal.").await;
            Callback::Code(c.to_string())
        }
        None => {
            reply(sock, 400, "The sign-in came back without a code.").await;
            Callback::Other
        }
    }
}

async fn reply(sock: &mut tokio::net::TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>nexo</title><body style=\"font-family:sans-serif;margin:3em\"><p>{message}</p>"
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\ncache-control: no-store\r\n\r\n",
        page.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(page.as_bytes()).await;
}

/// `a=1&b=x%20y` as pairs, decoded.
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- the Codex CLI's login ----

/// Where the Codex CLI keeps its login: `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`.
pub fn cli_file() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h).join("auth.json"),
        None => dirs::home_dir().unwrap_or_default().join(".codex").join("auth.json"),
    }
}

/// True when `file` holds a ChatGPT login (tokens), whatever their state; nothing is returned of them.
pub fn cli_login_in(file: &Path) -> bool {
    read_cli(file).is_ok()
}

/// What the Codex CLI's file holds of a login.
struct CliTokens {
    access: String,
    refresh: String,
    id: Option<String>,
    account: Option<String>,
}

fn read_cli(file: &Path) -> Result<CliTokens, ProviderError> {
    let shown = nexo_core::paths::shown(file);
    let raw = std::fs::read_to_string(file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            fail(format!(
                "no Codex CLI login at {shown}: sign in with `codex login` first, or use `--flow codex-browser`"
            ))
        } else {
            fail(format!("{shown} cannot be read: {}", e.kind()))
        }
    })?;
    let v: Value = serde_json::from_str(&raw).map_err(|_| fail(format!("{shown} is not a Codex CLI login file")))?;
    let t = &v["tokens"];
    let (Some(access), Some(refresh)) = (text(t, "access_token"), text(t, "refresh_token")) else {
        let why = if text(&v, "OPENAI_API_KEY").is_some() {
            "it holds an API key, not a ChatGPT sign-in: use the `openai` provider with that key"
        } else {
            "it has no ChatGPT tokens: sign in with `codex login`"
        };
        return Err(fail(format!("{shown}: {why}")));
    };
    Ok(CliTokens { access, refresh, id: text(t, "id_token"), account: text(t, "account_id") })
}

/// The login the Codex CLI holds in `file`, as a credential tied to that file.
pub fn from_cli(file: &Path) -> Result<Credential, ProviderError> {
    let t = read_cli(file)?;
    Ok(cli_credential(file, t))
}

fn cli_credential(file: &Path, t: CliTokens) -> Credential {
    let account = t
        .account
        .clone()
        .or_else(|| t.id.as_deref().and_then(crate::login::account_id))
        .or_else(|| crate::login::account_id(&t.access));
    let mut metadata = BTreeMap::from([
        ("flow".to_string(), "codex".to_string()),
        ("source".to_string(), "codex-cli".to_string()),
        ("source_file".to_string(), file.to_string_lossy().into_owned()),
    ]);
    if let Some(a) = account {
        metadata.insert("account_id".into(), a);
    }
    // a token whose expiry cannot be read counts as expiring: the first request renews it
    let expires = expiry(&t.access).unwrap_or_else(auth::now);
    Credential::Oauth { access: t.access, refresh: t.refresh, expires, metadata }
}

/// When a token (a JWT) says it ends, in unix seconds.
pub fn expiry(jwt: &str) -> Option<u64> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v["exp"].as_u64()
}

/// A renewal of a login that lives in the Codex CLI's file: that file is read first (the CLI may
/// have renewed already), and the new tokens are written back to it after a renewal of our own.
pub(crate) async fn renew_shared(at: &Endpoints, cred: &Credential, file: &Path) -> Result<Credential, ProviderError> {
    let mut base = cred.clone();
    if let Ok(t) = read_cli(file) {
        let theirs = cli_credential(file, t);
        if theirs.secret() != cred.secret() && !theirs.expiring(auth::now(), 60) {
            return Ok(theirs);
        }
        // the refresh token the CLI holds is the newest one there is
        base = theirs;
    }
    let renewed = crate::login::refresh_plain(at, &base).await?;
    if let Err(e) = write_back(file, &renewed) {
        tracing::warn!("could not update {}: {e}", file.display());
    }
    Ok(renewed)
}

/// The new tokens into the CLI's file, the rest of it as it was.
fn write_back(file: &Path, renewed: &Credential) -> std::io::Result<()> {
    let raw = std::fs::read_to_string(file)?;
    let mut top: BTreeMap<String, Value> = serde_json::from_str(&raw).map_err(std::io::Error::other)?;
    let Credential::Oauth { access, refresh, .. } = renewed else { return Ok(()) };
    let mut t = top.get("tokens").cloned().unwrap_or_else(|| json!({}));
    t["access_token"] = json!(access);
    if !refresh.is_empty() {
        t["refresh_token"] = json!(refresh);
    }
    if let Some(a) = renewed.metadata("account_id") {
        t["account_id"] = json!(a);
    }
    top.insert("tokens".into(), t);
    let now =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    top.insert("last_refresh".into(), json!(now));
    auth::write_to(file, &top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_is_the_s256_of_the_verifier() {
        // the example of RFC 7636, appendix B
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = pkce();
        assert!((43..=128).contains(&p.verifier.len()));
        assert_eq!(p.challenge, challenge_of(&p.verifier));
        assert_ne!(pkce().verifier, p.verifier);
    }

    #[test]
    fn queries_are_decoded() {
        assert_eq!(
            parse_query("code=a%2Fb&state=s+t&x"),
            [("code".into(), "a/b".into()), ("state".into(), "s t".into()), ("x".into(), "".into())]
        );
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn the_authorize_address_carries_the_pkce_challenge_and_the_state() {
        let u = authorize_url("https://auth.openai.com/", "http://localhost:1455/auth/callback", "CH", "ST");
        assert!(u.starts_with("https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_"), "{u}");
        for part in [
            "code_challenge=CH",
            "code_challenge_method=S256",
            "state=ST",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "scope=openid%20profile%20email%20offline_access",
        ] {
            assert!(u.contains(part), "{part} in {u}");
        }
    }

    use httpmock::prelude::*;
    use std::sync::{Arc, Mutex};

    fn jwt(claims: Value) -> String {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", b.encode(b"{}"), b.encode(claims.to_string()))
    }

    fn endpoints(s: &MockServer, file: Option<PathBuf>) -> Endpoints {
        Endpoints {
            openai_issuer: s.base_url(),
            codex_auth: file,
            callback_ports: vec![0],
            pad: Duration::ZERO,
            ..Endpoints::default()
        }
    }

    async fn shown_url(shown: &Arc<Mutex<Option<DeviceCode>>>) -> DeviceCode {
        loop {
            if let Some(d) = shown.lock().unwrap().clone() {
                return d;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// What a browser does after the person agrees: opens the address, and the page sends it back.
    async fn fake_browser(shown: Arc<Mutex<Option<DeviceCode>>>, answer: &str) -> (String, String) {
        let d = shown_url(&shown).await;
        let u = reqwest::Url::parse(&d.url).unwrap();
        let q: BTreeMap<String, String> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert!(d.code.is_empty());
        assert_eq!(u.path(), "/oauth/authorize");
        assert_eq!(q["code_challenge_method"], "S256");
        let back = format!("{}?{}&state={}", q["redirect_uri"], answer, q["state"]);
        let r = reqwest::get(&back).await.unwrap();
        assert!(r.status().is_success(), "{}", r.status());
        (q["code_challenge"].clone(), q["redirect_uri"].clone())
    }

    #[tokio::test]
    async fn the_browser_flow_trades_the_code_with_the_verifier_of_its_challenge() {
        let s = MockServer::start();
        let challenge = Arc::new(Mutex::new(String::new()));
        let seen = challenge.clone();
        let tok = s.mock(|w, t| {
            w.method(POST)
                .path("/oauth/token")
                .header("content-type", "application/x-www-form-urlencoded")
                .body_includes("grant_type=authorization_code")
                .body_includes("code=thecode")
                .body_includes("redirect_uri=http%3A%2F%2Flocalhost%3A")
                .is_true(move |req| {
                    let body = req.body_string();
                    let verifier = body.split('&').find_map(|p| p.strip_prefix("code_verifier=")).unwrap_or("");
                    challenge_of(verifier) == *seen.lock().unwrap()
                });
            t.status(200).json_body(json!({
                "id_token": jwt(json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct"}})),
                "access_token": "at1", "refresh_token": "rt1", "expires_in": 3600
            }));
        });
        let shown = Arc::new(Mutex::new(None));
        let sink = shown.clone();
        let at = endpoints(&s, None);
        let show = move |d: &DeviceCode| *sink.lock().unwrap() = Some(d.clone());
        // the challenge is noted before the code goes back, so the mock can check the verifier against it
        let (cred, (ch, redirect)) = tokio::join!(browser(&at, &show), async {
            let d = shown_url(&shown).await;
            let u = reqwest::Url::parse(&d.url).unwrap();
            let ch = u.query_pairs().find(|(k, _)| k == "code_challenge").unwrap().1.into_owned();
            *challenge.lock().unwrap() = ch;
            fake_browser(shown.clone(), "code=thecode").await
        });
        let cred = cred.unwrap();
        tok.assert();
        assert!(redirect.starts_with("http://localhost:") && redirect.ends_with("/auth/callback"), "{redirect}");
        assert!(!ch.is_empty());
        assert_eq!(cred.secret(), "at1");
        assert_eq!(cred.metadata("account_id"), Some("acct"));
        assert_eq!(cred.metadata("flow"), Some("codex"));
    }

    #[tokio::test]
    async fn a_declined_sign_in_and_a_foreign_callback_are_told_apart() {
        let s = MockServer::start();
        let at = endpoints(&s, None);
        let shown = Arc::new(Mutex::new(None));
        let sink = shown.clone();
        let show = move |d: &DeviceCode| *sink.lock().unwrap() = Some(d.clone());
        let (res, _) = tokio::join!(browser(&at, &show), async {
            let d = shown_url(&shown).await;
            let u = reqwest::Url::parse(&d.url).unwrap();
            let redirect = u.query_pairs().find(|(k, _)| k == "redirect_uri").unwrap().1.into_owned();
            // another state is refused and the flow goes on waiting; so is any other path
            let bad = reqwest::get(format!("{redirect}?code=x&state=other")).await.unwrap();
            assert_eq!(bad.status().as_u16(), 400);
            let other = reqwest::get(redirect.replace("/auth/callback", "/favicon.ico")).await.unwrap();
            assert_eq!(other.status().as_u16(), 404);
            fake_browser(shown.clone(), "error=access_denied&error_description=no").await;
        });
        let e = res.unwrap_err();
        assert!(e.message.contains("declined") && e.message.contains("no"), "{}", e.message);
    }

    #[tokio::test]
    async fn a_port_in_use_is_named_with_the_way_out() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = taken.local_addr().unwrap().port();
        let s = MockServer::start();
        let mut at = endpoints(&s, None);
        at.callback_ports = vec![port];
        let e = browser(&at, &|_| {}).await.unwrap_err();
        assert!(e.message.contains(&port.to_string()) && e.message.contains("codex-device"), "{}", e.message);
    }

    fn cli_file(dir: &Path, access: &str, refresh: &str) -> PathBuf {
        let f = dir.join("auth.json");
        let body = json!({
            "OPENAI_API_KEY": null,
            "tokens": {"id_token": jwt(json!({"email": "x"})), "access_token": access, "refresh_token": refresh,
                       "account_id": "acct-cli"},
            "last_refresh": "2026-11-01T00:00:00Z",
            "auth_mode": "chatgpt"
        });
        std::fs::write(&f, body.to_string()).unwrap();
        f
    }

    #[test]
    fn the_codex_cli_login_is_read_without_showing_a_token() {
        let d = tempfile::tempdir().unwrap();
        let soon = jwt(json!({"exp": auth::now() + 3600}));
        let f = cli_file(d.path(), &soon, "rt-cli");
        assert!(cli_login_in(&f));
        let c = from_cli(&f).unwrap();
        assert_eq!(c.secret(), soon);
        assert_eq!(c.metadata("account_id"), Some("acct-cli"));
        assert_eq!(c.metadata("source"), Some("codex-cli"));
        assert!(!c.expiring(auth::now(), 60));
        // a file with only an API key is not a ChatGPT login, and the error shows no key
        let g = d.path().join("key.json");
        std::fs::write(&g, r#"{"OPENAI_API_KEY":"apikey-synthetic-123"}"#).unwrap();
        assert!(!cli_login_in(&g));
        let e = from_cli(&g).unwrap_err();
        assert!(e.message.contains("API key") && !e.message.contains("apikey-synthetic"), "{}", e.message);
        let e = from_cli(&d.path().join("none.json")).unwrap_err();
        assert!(e.message.contains("codex login"), "{}", e.message);
    }

    #[tokio::test]
    async fn the_codex_flow_takes_the_cli_login_when_there_is_one_and_stores_it_privately() {
        let d = tempfile::tempdir().unwrap();
        let f = cli_file(d.path(), &jwt(json!({"exp": auth::now() + 3600})), "rt-cli");
        let s = MockServer::start();
        let cred =
            crate::login::login_at(&endpoints(&s, Some(f)), "codex", &|_| panic!("no browser needed")).await.unwrap();
        let store = d.path().join("nexo").join("auth.json");
        auth::set_in(&store, "codex", cred).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&store).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(auth::read_from(&store)["codex"].metadata("source"), Some("codex-cli"));
        // without the file the same flow asks for the browser
        let none = endpoints(&s, Some(d.path().join("missing.json")));
        let shown = Mutex::new(false);
        let _ = tokio::time::timeout(
            Duration::from_millis(300),
            crate::login::login_at(&none, "codex", &|dc| {
                assert!(dc.url.contains("/oauth/authorize"));
                *shown.lock().unwrap() = true;
            }),
        )
        .await;
        assert!(*shown.lock().unwrap());
    }

    #[tokio::test]
    async fn a_login_from_the_cli_follows_the_file_and_writes_its_renewal_back() {
        let d = tempfile::tempdir().unwrap();
        let old = jwt(json!({"exp": auth::now() - 10}));
        let f = cli_file(d.path(), &old, "rt1");
        let cred = from_cli(&f).unwrap();
        assert!(cred.expiring(auth::now(), 60));
        let s = MockServer::start();
        let at = endpoints(&s, Some(f.clone()));

        // the CLI renewed meanwhile: its tokens are taken, nothing is asked of the server
        let fresh = jwt(json!({"exp": auth::now() + 3600, "n": 1}));
        cli_file(d.path(), &fresh, "rt-newer");
        let same = crate::login::refresh_at(&at, &cred).await.unwrap();
        assert_eq!(same.secret(), fresh);

        // nobody renewed: nexo does it with the newest refresh token and writes the result back
        cli_file(d.path(), &old, "rt-newest");
        let renew = s.mock(|w, t| {
            w.method(POST)
                .path("/oauth/token")
                .body_includes("grant_type=refresh_token")
                .body_includes("refresh_token=rt-newest");
            t.status(200).json_body(json!({"access_token": "at2", "refresh_token": "rt2", "expires_in": 3600}));
        });
        let renewed = crate::login::refresh_at(&at, &cred).await.unwrap();
        renew.assert();
        assert_eq!(renewed.secret(), "at2");
        assert_eq!(renewed.metadata("source_file"), cred.metadata("source_file"));
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(back["tokens"]["access_token"], "at2");
        assert_eq!(back["tokens"]["refresh_token"], "rt2");
        assert_eq!(back["tokens"]["account_id"], "acct-cli");
        assert_eq!(back["auth_mode"], "chatgpt");
        assert_ne!(back["last_refresh"], "2026-11-01T00:00:00Z");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn the_ways_in_put_the_cli_login_first_only_when_it_exists() {
        let d = tempfile::tempdir().unwrap();
        let none = crate::login::choices_in("codex", &d.path().join("missing.json"));
        assert_eq!(none.iter().map(|c| c.flow.as_str()).collect::<Vec<_>>(), ["codex-browser", "codex-device"]);
        let f = cli_file(d.path(), &jwt(json!({"exp": auth::now() + 3600})), "rt");
        let some = crate::login::choices_in("codex", &f);
        assert_eq!(
            some.iter().map(|c| c.flow.as_str()).collect::<Vec<_>>(),
            ["codex-cli", "codex-browser", "codex-device"]
        );
        // the hint never carries a token
        assert!(some.iter().all(|c| !c.hint.contains("rt") || !c.hint.contains(".sig")));
        // the other subscriptions have the one way
        let one = crate::login::choices_in("copilot", &f);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].flow, "copilot");
    }

    #[tokio::test]
    async fn a_subscription_provider_is_probed_without_asking_the_server_for_models() {
        let mut cfg = nexo_core::ProviderConfig::new("openai-responses", "http://127.0.0.1:9/backend-api/codex", None);
        cfg.login = Some("codex".into());
        cfg.models = vec!["gpt-x".into(), "gpt-x-fast".into()];
        let p = crate::probe::server(&cfg, Some("synthetic-token")).await.unwrap();
        assert_eq!(p.kind, "subscription");
        assert_eq!(p.protocol, "openai-responses");
        assert!(!p.needs_key);
        assert_eq!(p.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["gpt-x", "gpt-x-fast"]);
    }
}
