//! The MCP client over HTTP against a mock server on 127.0.0.1 (no network): streamable HTTP with
//! JSON and SSE answers, the session id, the old HTTP+SSE transport, and a 401 that leads through
//! the OAuth discovery steps to a stored login (the browser is replaced by a function that follows
//! the redirect).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use nexo_core::auth::{self, Credential};
use nexo_core::config::McpServerConfig;
use nexo_core::{ToolContext, ToolKind};
use nexo_tools::mcp::McpClient;
use nexo_tools::mcp_http::NeedsLogin;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

// ---- a small HTTP/1.1 server ----

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

enum Resp {
    Full(u16, Vec<(&'static str, String)>, String),
    /// An SSE stream that stays open: `first`, then whatever arrives on the channel.
    Stream(String, mpsc::UnboundedReceiver<String>),
}

fn json_resp(v: Value) -> Resp {
    Resp::Full(200, vec![("content-type", "application/json".into())], v.to_string())
}

fn status(code: u16) -> Resp {
    Resp::Full(code, vec![], String::new())
}

type Handler = Arc<dyn Fn(Req) -> Resp + Send + Sync>;

async fn start(handler: Handler) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let h = handler.clone();
            tokio::spawn(async move {
                let _ = serve(sock, h).await;
            });
        }
    });
    base
}

async fn serve(mut sock: tokio::net::TcpStream, handler: Handler) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < end + len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[end..]).into_owned();
    match handler(Req { method, path, headers, body }) {
        Resp::Full(code, headers, body) => {
            let mut out = format!("HTTP/1.1 {code} X\r\nconnection: close\r\ncontent-length: {}\r\n", body.len());
            for (k, v) in headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            out.push_str(&body);
            sock.write_all(out.as_bytes()).await?;
        }
        Resp::Stream(first, mut rx) => {
            let head = "HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: text/event-stream\r\n\r\n";
            sock.write_all(head.as_bytes()).await?;
            sock.write_all(first.as_bytes()).await?;
            while let Some(m) = rx.recv().await {
                sock.write_all(m.as_bytes()).await?;
            }
        }
    }
    sock.shutdown().await
}

// ---- a mock MCP server ----

/// (method, session, protocol version, api key header)
type Post = (String, Option<String>, Option<String>, Option<String>);

#[derive(Default)]
struct Seen {
    /// (method, mcp-session-id, mcp-protocol-version, x-api-key) of each POST
    posts: Vec<Post>,
    deleted: bool,
}

fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// What the server says to a JSON-RPC message (None: a notification).
fn answer(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?;
    Some(match msg["method"].as_str().unwrap_or("") {
        "initialize" => rpc_result(
            id,
            json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                   "serverInfo": {"name": "mock", "version": "1"}}),
        ),
        "tools/list" => rpc_result(
            id,
            json!({"tools": [{"name": "echo", "description": "Echoes.",
                              "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}),
        ),
        "tools/call" => {
            let text = msg["params"]["arguments"]["text"].as_str().unwrap_or("");
            rpc_result(id, json!({"content": [{"type": "text", "text": format!("echo: {text}")}]}))
        }
        _ => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "unknown"}}),
    })
}

fn sse_event(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn http_cfg(base: &str) -> McpServerConfig {
    McpServerConfig { url: Some(format!("{base}/mcp")), ..Default::default() }
}

async fn echo(c: &McpClient, text: &str) -> String {
    let tools = c.tools();
    let ctx = ToolContext::new(std::env::temp_dir());
    let out = tools[0].run(json!({"text": text}), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    out.content
}

fn temp_auth() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("auth.json");
    (d, f)
}

/// A streamable HTTP server. `sse_calls`: tools/call is answered as an event stream.
fn streamable(sse_calls: bool, seen: Arc<Mutex<Seen>>) -> Handler {
    Arc::new(move |r: Req| {
        if r.path != "/mcp" {
            return status(404);
        }
        if r.method == "DELETE" {
            seen.lock().unwrap().deleted = true;
            return status(200);
        }
        let msg: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
        let method = msg["method"].as_str().unwrap_or("").to_string();
        let session = r.headers.get("mcp-session-id").cloned();
        seen.lock().unwrap().posts.push((
            method.clone(),
            session.clone(),
            r.headers.get("mcp-protocol-version").cloned(),
            r.headers.get("x-api-key").cloned(),
        ));
        if method != "initialize" && session.as_deref() != Some("sess-42") {
            return status(404);
        }
        let Some(a) = answer(&msg) else { return status(202) };
        let mut headers = vec![];
        if method == "initialize" {
            headers.push(("mcp-session-id", "sess-42".to_string()));
        }
        if sse_calls && method == "tools/call" {
            headers.push(("content-type", "text/event-stream".to_string()));
            let body = format!(
                "{}{}",
                sse_event("message", r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#),
                sse_event("message", &a.to_string())
            );
            return Resp::Full(200, headers, body);
        }
        headers.push(("content-type", "application/json".to_string()));
        Resp::Full(200, headers, a.to_string())
    })
}

#[tokio::test]
async fn streamable_http_with_json_answers_keeps_the_session() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = start(streamable(false, seen.clone())).await;
    std::env::set_var("NEXO_MCP_TEST_KEY", "k-123");
    let mut cfg = http_cfg(&base);
    cfg.headers.insert("X-Api-Key".into(), "${NEXO_MCP_TEST_KEY}".into());
    let (_d, auth) = temp_auth();
    let c = McpClient::connect_in("mock", &cfg, &auth).await.unwrap();
    assert_eq!(c.protocol_version, "2025-06-18");
    assert_eq!(c.defs().len(), 1);
    assert_eq!(c.tools()[0].kind(), ToolKind::Other);
    assert_eq!(echo(&c, "hi").await, "echo: hi");
    c.close().await;
    let s = seen.lock().unwrap();
    let methods: Vec<_> = s.posts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(methods, ["initialize", "notifications/initialized", "tools/list", "tools/call"]);
    assert_eq!(s.posts[0].1, None, "no session before initialize");
    for p in &s.posts[1..] {
        assert_eq!(p.1.as_deref(), Some("sess-42"), "{}", p.0);
        assert_eq!(p.2.as_deref(), Some("2025-06-18"), "{}", p.0);
    }
    assert!(s.posts.iter().all(|p| p.3.as_deref() == Some("k-123")), "the header's variable is filled in");
    assert!(s.deleted, "closing ends the session");
}

#[tokio::test]
async fn streamable_http_with_an_sse_answer() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = start(streamable(true, seen)).await;
    let (_d, auth) = temp_auth();
    let c = McpClient::connect_in("mock", &http_cfg(&base), &auth).await.unwrap();
    assert_eq!(echo(&c, "stream").await, "echo: stream");
}

#[tokio::test]
async fn a_missing_variable_is_named() {
    let mut cfg = http_cfg("http://127.0.0.1:9");
    cfg.headers.insert("Authorization".into(), "Bearer ${NEXO_MCP_TEST_NOT_SET_ANYWHERE}".into());
    let (_d, auth) = temp_auth();
    let e = McpClient::connect_in("mock", &cfg, &auth).await.err().unwrap().to_string();
    assert!(e.contains("NEXO_MCP_TEST_NOT_SET_ANYWHERE"), "{e}");
}

/// The old transport: the POST of `initialize` gets a 405; GET /sse announces /messages.
fn legacy() -> Handler {
    let stream: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>> = Arc::new(Mutex::new(None));
    Arc::new(move |r: Req| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/mcp") => status(405),
        ("GET", "/mcp") => {
            let (tx, rx) = mpsc::unbounded_channel();
            *stream.lock().unwrap() = Some(tx);
            Resp::Stream(sse_event("endpoint", "/messages?s=1"), rx)
        }
        ("POST", "/messages?s=1") => {
            let msg: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
            if let Some(a) = answer(&msg) {
                let tx = stream.lock().unwrap().clone().unwrap();
                tx.send(sse_event("message", &a.to_string())).unwrap();
            }
            status(202)
        }
        _ => status(404),
    })
}

#[tokio::test]
async fn the_old_sse_transport_is_the_fallback() {
    let base = start(legacy()).await;
    let (_d, auth) = temp_auth();
    let c = McpClient::connect_in("old", &http_cfg(&base), &auth).await.unwrap();
    assert_eq!(c.defs().len(), 1);
    assert_eq!(echo(&c, "legacy").await, "echo: legacy");
}

#[tokio::test]
async fn transport_sse_skips_the_streamable_attempt() {
    let base = start(legacy()).await;
    let (_d, auth) = temp_auth();
    let mut cfg = http_cfg(&base);
    cfg.transport = Some("sse".into());
    let c = McpClient::connect_in("old", &cfg, &auth).await.unwrap();
    assert_eq!(echo(&c, "explicit").await, "echo: explicit");
}

// ---- OAuth ----

#[derive(Default)]
struct Auth {
    /// the access token the server accepts now
    current: String,
    challenge: String,
    registered: bool,
    codes_traded: u32,
    refreshes: u32,
    redirect: String,
}

/// One server that is the MCP resource and its own authorization server.
fn oauth_server(base: Arc<Mutex<String>>, st: Arc<Mutex<Auth>>) -> Handler {
    Arc::new(move |r: Req| {
        let base = base.lock().unwrap().clone();
        let mut st = st.lock().unwrap();
        let form = |body: &str| -> HashMap<String, String> {
            url::form_urlencoded::parse(body.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
        };
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/mcp") => {
                let ok = r.headers.get("authorization").map(String::as_str) == Some(&format!("Bearer {}", st.current));
                if !ok || st.current.is_empty() {
                    return Resp::Full(
                        401,
                        vec![(
                            "www-authenticate",
                            format!(
                                "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\", scope=\"mcp\""
                            ),
                        )],
                        String::new(),
                    );
                }
                let msg: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
                match answer(&msg) {
                    Some(a) => json_resp(a),
                    None => status(202),
                }
            }
            ("GET", "/.well-known/oauth-protected-resource") => {
                json_resp(json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]}))
            }
            ("GET", "/.well-known/oauth-authorization-server") => json_resp(json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"]
            })),
            ("POST", "/register") => {
                let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
                st.redirect = v["redirect_uris"][0].as_str().unwrap_or("").to_string();
                st.registered = true;
                Resp::Full(
                    201,
                    vec![("content-type", "application/json".into())],
                    json!({"client_id": "cid-1"}).to_string(),
                )
            }
            ("POST", "/token") => {
                let f = form(&r.body);
                match f.get("grant_type").map(String::as_str) {
                    Some("authorization_code") => {
                        let verifier = f.get("code_verifier").cloned().unwrap_or_default();
                        let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier));
                        if f.get("code").map(String::as_str) != Some("the-code")
                            || hash != st.challenge
                            || f.get("client_id").map(String::as_str) != Some("cid-1")
                            || f.get("redirect_uri") != Some(&st.redirect)
                            || f.get("resource") != Some(&format!("{base}/mcp"))
                        {
                            return Resp::Full(400, vec![], json!({"error": "invalid_grant"}).to_string());
                        }
                        st.codes_traded += 1;
                        st.current = "access-1".into();
                        json_resp(json!({"access_token": "access-1", "refresh_token": "refresh-1", "expires_in": 3600}))
                    }
                    Some("refresh_token") if f.get("refresh_token").map(String::as_str) == Some("refresh-1") => {
                        st.refreshes += 1;
                        st.current = "access-2".into();
                        json_resp(json!({"access_token": "access-2", "expires_in": 3600}))
                    }
                    _ => Resp::Full(400, vec![], json!({"error": "invalid_grant"}).to_string()),
                }
            }
            _ => status(404),
        }
    })
}

#[tokio::test]
async fn a_401_leads_through_discovery_and_login_to_a_working_connection() {
    let base = Arc::new(Mutex::new(String::new()));
    let st = Arc::new(Mutex::new(Auth { current: "unset".into(), ..Default::default() }));
    let url = start(oauth_server(base.clone(), st.clone())).await;
    *base.lock().unwrap() = url.clone();
    st.lock().unwrap().current = String::new();
    let (_d, auth_file) = temp_auth();
    let cfg = http_cfg(&url);

    // without a login the server asks for one
    let e = McpClient::connect_in("srv", &cfg, &auth_file).await.err().unwrap();
    assert!(e.downcast_ref::<NeedsLogin>().is_some(), "{e}");
    assert!(e.to_string().contains("nexo mcp login srv"), "{e}");

    // the "browser": checks the address and follows the redirect with a code
    let opened = Arc::new(Mutex::new(String::new()));
    let seen_url = opened.clone();
    let st2 = st.clone();
    let open = move |u: &str| {
        let parsed = url::Url::parse(u).unwrap();
        let q: HashMap<String, String> = parsed.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(parsed.path(), "/authorize");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["client_id"], "cid-1");
        assert_eq!(q["scope"], "mcp");
        assert!(q["redirect_uri"].starts_with("http://127.0.0.1:"));
        st2.lock().unwrap().challenge = q["code_challenge"].clone();
        *seen_url.lock().unwrap() = u.to_string();
        let back = format!("{}?code=the-code&state={}", q["redirect_uri"], q["state"]);
        tokio::spawn(async move {
            // a stray request first: the callback must wait for the right one
            let _ = reqwest::get(back.replace("/callback", "/other")).await;
            reqwest::get(back).await.unwrap();
        });
    };
    let host = nexo_tools::mcp_oauth::login("srv", &cfg, &auth_file, &open).await.unwrap();
    assert_eq!(host, "127.0.0.1");
    assert!(st.lock().unwrap().registered);
    assert_eq!(st.lock().unwrap().codes_traded, 1);

    // the tokens are in the store, under the server's name
    let cred = auth::read_from(&auth_file).remove("mcp:srv").expect("stored");
    assert_eq!(cred.secret(), "access-1");
    assert_eq!(cred.metadata("flow"), Some("mcp"));

    let c = McpClient::connect_in("srv", &cfg, &auth_file).await.unwrap();
    assert_eq!(echo(&c, "authed").await, "echo: authed");

    // an expired access token is renewed with the refresh token, and the new one is stored
    let Credential::Oauth { refresh, metadata, .. } = cred else { panic!("oauth") };
    auth::set_in(&auth_file, "mcp:srv", Credential::Oauth { access: "access-1".into(), refresh, expires: 1, metadata })
        .unwrap();
    let c = McpClient::connect_in("srv", &cfg, &auth_file).await.unwrap();
    assert_eq!(echo(&c, "renewed").await, "echo: renewed");
    assert_eq!(st.lock().unwrap().refreshes, 1);
    let cred = auth::read_from(&auth_file).remove("mcp:srv").unwrap();
    assert_eq!(cred.secret(), "access-2");
    assert!(matches!(&cred, Credential::Oauth { refresh, .. } if refresh == "refresh-1"), "the refresh token stays");
}

#[tokio::test]
async fn a_server_that_needs_no_login_is_said_so() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let base = start(streamable(false, seen)).await;
    let (_d, auth_file) = temp_auth();
    let e = nexo_tools::mcp_oauth::login("x", &http_cfg(&base), &auth_file, &|_| {}).await.unwrap_err();
    assert!(e.to_string().contains("did not ask for a login"), "{e}");
}
