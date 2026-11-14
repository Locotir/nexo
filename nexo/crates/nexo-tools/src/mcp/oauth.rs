//! OAuth 2.1 for remote MCP servers (the MCP authorization spec): a server answers 401 with a
//! `WWW-Authenticate` header; the client finds the server's protected-resource metadata, then the
//! authorization server's metadata, registers itself (dynamic client registration), signs the
//! person in in the browser with PKCE and a redirect to `127.0.0.1`, and keeps the tokens in nexo's
//! credential store under `mcp:<server>`, renewing them with the refresh token. Token values are
//! never printed or logged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use nexo_core::auth::{self, Credential};
use nexo_core::config::McpServerConfig;
use nexo_providers::codex::pkce;
use nexo_providers::login::form;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::mcp_http::expand_env;

/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(10 * 60);
const REQUEST: Duration = Duration::from_secs(5);
/// Renew an access token this many seconds before it expires.
const MARGIN: u64 = 60;

/// The credential store key of a server.
pub fn key(server: &str) -> String {
    format!("mcp:{server}")
}

/// The stored login of one server, and the means to keep it fresh.
pub struct OAuth {
    key: String,
    file: PathBuf,
    client: Client,
}

impl OAuth {
    pub fn new(server: &str, auth_file: &Path, client: Client) -> OAuth {
        OAuth { key: key(server), file: auth_file.to_path_buf(), client }
    }

    /// The access token to send, renewed first when it is about to expire (or when `force` says the
    /// server refused it). None: there is no login, or it cannot be renewed and has expired.
    pub async fn token(&self, force: bool) -> Option<String> {
        let cred = auth::read_from(&self.file).remove(&self.key)?;
        let Credential::Oauth { access, refresh, expires, .. } = &cred else { return None };
        let stale = force || cred.expiring(auth::now(), MARGIN);
        if !stale {
            return Some(access.clone());
        }
        if refresh.is_empty() {
            return (!force && (*expires == 0 || *expires > auth::now())).then(|| access.clone());
        }
        match self.renew(force).await {
            Some(c) => Some(c.secret().to_string()),
            None => (!force && (*expires == 0 || *expires > auth::now())).then(|| access.clone()),
        }
    }

    /// Trades the refresh token for new tokens, with the store locked so two sessions do not both do it.
    async fn renew(&self, force: bool) -> Option<Credential> {
        let lock = auth::lock_in(&self.file).ok()?;
        // another session may have renewed them while this one waited for the lock
        let cred = lock.get(&self.key)?;
        if !force && !cred.expiring(auth::now(), MARGIN) {
            return Some(cred);
        }
        let Credential::Oauth { refresh, metadata, .. } = &cred else { return None };
        let endpoint = metadata.get("token_endpoint")?;
        let client_id = metadata.get("client_id")?;
        let mut pairs = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id.as_str()),
        ];
        if let Some(r) = metadata.get("resource") {
            pairs.push(("resource", r));
        }
        if let Some(s) = metadata.get("client_secret") {
            pairs.push(("client_secret", s));
        }
        let v = post_form(&self.client, endpoint, &form(&pairs)).await.ok()?;
        let fresh = credential_from(&v, Some(&cred)).ok()?;
        lock.set(&self.key, fresh.clone()).ok()?;
        Some(fresh)
    }
}

/// What a `WWW-Authenticate: Bearer ...` header says.
#[derive(Debug, Default, PartialEq)]
pub struct Challenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
}

/// Reads the `resource_metadata` and `scope` parameters of a Bearer challenge.
pub fn parse_challenge(header: &str) -> Challenge {
    let mut out = Challenge::default();
    let mut rest = header;
    while let Some(eq) = rest.find('=') {
        let name = rest[..eq].rsplit([' ', ',']).next().unwrap_or("").trim().to_ascii_lowercase();
        let after = &rest[eq + 1..];
        let (value, next) = if let Some(q) = after.strip_prefix('"') {
            match q.find('"') {
                Some(e) => (q[..e].to_string(), &q[e + 1..]),
                None => (q.to_string(), ""),
            }
        } else {
            let e = after.find([',', ' ']).unwrap_or(after.len());
            (after[..e].to_string(), &after[e..])
        };
        match name.as_str() {
            "resource_metadata" => out.resource_metadata = Some(value),
            "scope" => out.scope = Some(value),
            _ => {}
        }
        rest = next;
    }
    out
}

async fn get_json(client: &Client, url: &str) -> Option<Value> {
    let r = client.get(url).header("accept", "application/json").send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// A form POST that answers JSON. The error holds the server's `error` words, never a token.
async fn post_form(client: &Client, url: &str, body: &str) -> anyhow::Result<Value> {
    let r = client
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| anyhow!("{}", e.without_url()))?;
    let ok = r.status().is_success();
    let code = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !ok {
        let why = v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("refused");
        bail!("the token endpoint answered HTTP {code}: {why}");
    }
    Ok(v)
}

/// Tokens from a token endpoint's answer. `old` supplies what a refresh does not repeat.
fn credential_from(v: &Value, old: Option<&Credential>) -> anyhow::Result<Credential> {
    let access =
        v["access_token"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("no access token in the answer"))?;
    let (old_refresh, mut metadata) = match old {
        Some(Credential::Oauth { refresh, metadata, .. }) => (refresh.clone(), metadata.clone()),
        _ => (String::new(), Default::default()),
    };
    let refresh = v["refresh_token"].as_str().map(String::from).unwrap_or(old_refresh);
    let expires = v["expires_in"].as_u64().map(|s| auth::now() + s).unwrap_or(0);
    metadata.insert("flow".into(), "mcp".into());
    Ok(Credential::Oauth { access: access.to_string(), refresh, expires, metadata })
}

/// `scheme://host[:port]` of a URL.
fn origin(u: &url::Url) -> String {
    u.origin().ascii_serialization()
}

/// Finds the authorization server's metadata for an issuer (RFC 8414, then OpenID discovery).
async fn auth_server_metadata(client: &Client, issuer: &str) -> anyhow::Result<Value> {
    let u = url::Url::parse(issuer).context("bad authorization server address")?;
    let path = u.path().trim_end_matches('/');
    let o = origin(&u);
    let mut tries = vec![format!("{o}/.well-known/oauth-authorization-server{path}")];
    tries.push(format!("{o}/.well-known/openid-configuration{path}"));
    if !path.is_empty() {
        tries.push(format!("{issuer}/.well-known/openid-configuration"));
    }
    for t in tries {
        if let Some(v) = get_json(client, &t).await {
            if v["authorization_endpoint"].is_string() && v["token_endpoint"].is_string() {
                return Ok(v);
            }
        }
    }
    bail!("{issuer} publishes no authorization server metadata")
}

/// Signs the person in to a remote server and stores the tokens. `open` is told the address to
/// open in the browser (a test follows it by hand). Returns the host the login is for.
pub async fn login(
    name: &str,
    cfg: &McpServerConfig,
    auth_file: &Path,
    open: &(dyn Fn(&str) + Sync),
) -> anyhow::Result<String> {
    let server = expand_env(cfg.url.as_deref().unwrap_or("")).map_err(|e| anyhow!("url: {e}"))?;
    let server_url = url::Url::parse(&server).context("bad url")?;
    let client = nexo_providers::build_client(|| {
        Client::builder().connect_timeout(Duration::from_secs(15)).timeout(Duration::from_secs(30))
    })
    .map_err(|e| anyhow!(e))?;

    // 1. a request without a token, to read the 401
    let mut rb = client
        .post(&server)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "nexo", "version": env!("CARGO_PKG_VERSION")}}})
            .to_string(),
        );
    for (k, v) in &cfg.headers {
        if !k.eq_ignore_ascii_case("authorization") {
            rb = rb.header(k.as_str(), expand_env(v).map_err(|e| anyhow!("header {k}: {e}"))?);
        }
    }
    let mut resp = rb.send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
    if resp.status().as_u16() != 401 {
        // an old SSE server refuses the POST; its GET stream is what asks for the login
        let get = client.get(&server).header("accept", "text/event-stream").send().await.ok();
        match get {
            Some(g) if g.status().as_u16() == 401 => resp = g,
            _ if resp.status().is_success() => bail!("`{name}` did not ask for a login"),
            _ => bail!("`{name}` answered HTTP {} and did not ask for a login", resp.status().as_u16()),
        }
    }
    let challenge = resp
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(parse_challenge)
        .find(|c| c.resource_metadata.is_some() || c.scope.is_some())
        .unwrap_or_default();

    // 2. the protected-resource metadata, then the authorization server's
    let o = origin(&server_url);
    let path = server_url.path().trim_end_matches('/');
    let mut prm_urls = Vec::new();
    prm_urls.extend(challenge.resource_metadata.clone());
    prm_urls.push(format!("{o}/.well-known/oauth-protected-resource{path}"));
    prm_urls.push(format!("{o}/.well-known/oauth-protected-resource"));
    let mut issuer = o.clone();
    let mut scope = challenge.scope.clone();
    for u in prm_urls {
        if let Some(v) = get_json(&client, &u).await {
            if let Some(a) = v["authorization_servers"].as_array().and_then(|a| a.first()).and_then(Value::as_str) {
                issuer = a.to_string();
                if scope.is_none() {
                    scope = v["scopes_supported"]
                        .as_array()
                        .map(|s| s.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "));
                }
                break;
            }
        }
    }
    let meta = auth_server_metadata(&client, &issuer).await?;
    if let Some(m) = meta["code_challenge_methods_supported"].as_array() {
        if !m.iter().any(|x| x == "S256") {
            bail!("the authorization server does not support PKCE with S256");
        }
    }
    let authorize = meta["authorization_endpoint"].as_str().unwrap_or_default().to_string();
    let token_endpoint = meta["token_endpoint"].as_str().unwrap_or_default().to_string();

    // 3. the redirect address, then the client registration
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.context("cannot listen for the browser")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let Some(registration) = meta["registration_endpoint"].as_str() else {
        bail!("the authorization server does not allow registering a client (no registration_endpoint)");
    };
    let reg = client
        .post(registration)
        .json(&json!({
            "client_name": "nexo",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .map_err(|e| anyhow!("registration: {}", e.without_url()))?;
    if !reg.status().is_success() {
        bail!("the client registration was refused (HTTP {})", reg.status().as_u16());
    }
    let reg: Value = reg.json().await.context("the registration answer is not JSON")?;
    let client_id = reg["client_id"].as_str().ok_or_else(|| anyhow!("the registration gave no client_id"))?.to_string();
    let client_secret = reg["client_secret"].as_str().map(String::from);

    // 4. the browser
    let p = pkce();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let resource = server.split('#').next().unwrap_or(&server).to_string();
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_challenge", p.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("resource", resource.as_str()),
    ];
    if let Some(s) = scope.as_deref().filter(|s| !s.is_empty()) {
        pairs.push(("scope", s));
    }
    let sep = if authorize.contains('?') { '&' } else { '?' };
    open(&format!("{authorize}{sep}{}", form(&pairs)));
    let code = tokio::time::timeout(WAIT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| anyhow!("the browser did not come back in ten minutes; run the login again"))??;
    drop(listener);

    // 5. the tokens
    let mut body = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", p.verifier.as_str()),
        ("resource", resource.as_str()),
    ];
    if let Some(s) = &client_secret {
        body.push(("client_secret", s));
    }
    let v = post_form(&client, &token_endpoint, &form(&body)).await?;
    let mut cred = credential_from(&v, None)?;
    if let Credential::Oauth { metadata, .. } = &mut cred {
        metadata.insert("token_endpoint".into(), token_endpoint);
        metadata.insert("client_id".into(), client_id);
        metadata.insert("resource".into(), resource);
        if let Some(s) = client_secret {
            metadata.insert("client_secret".into(), s);
        }
    }
    auth::set_in(auth_file, &key(name), cred).context("cannot save the login")?;
    Ok(server_url.host_str().unwrap_or("").to_string())
}

/// Waits for the redirect and returns its `code`.
async fn wait_for_code(listener: &TcpListener, state: &str) -> anyhow::Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await.context("the login callback failed")?;
        let outcome = tokio::time::timeout(REQUEST, serve(&mut sock, state)).await.unwrap_or(Callback::Other);
        let _ = sock.shutdown().await;
        match outcome {
            Callback::Code(c) => return Ok(c),
            Callback::Denied(why) => bail!("the login was declined: {why}"),
            Callback::Other => {}
        }
    }
}

enum Callback {
    Code(String),
    Denied(String),
    Other,
}

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
    let parsed = target.and_then(|t| url::Url::parse(&format!("http://127.0.0.1{t}")).ok());
    let Some(u) = parsed.filter(|u| u.path() == "/callback") else {
        reply(sock, 404, "Not found").await;
        return Callback::Other;
    };
    let q: Vec<(String, String)> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("state") != Some(state) {
        reply(sock, 400, "This sign-in link does not belong to this session. Run the login again.").await;
        return Callback::Other;
    }
    if let Some(e) = get("error") {
        reply(sock, 200, "The sign-in was declined. You can close this tab.").await;
        return Callback::Denied(get("error_description").unwrap_or(e).to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_challenge_is_read() {
        let c = parse_challenge(
            r#"Bearer realm="mcp", resource_metadata="https://x.test/.well-known/oauth-protected-resource", scope="read write""#,
        );
        assert_eq!(c.resource_metadata.as_deref(), Some("https://x.test/.well-known/oauth-protected-resource"));
        assert_eq!(c.scope.as_deref(), Some("read write"));
        assert_eq!(parse_challenge("Bearer"), Challenge::default());
        assert_eq!(parse_challenge("Bearer scope=files").scope.as_deref(), Some("files"));
    }
}
