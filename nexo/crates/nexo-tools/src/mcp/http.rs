//! MCP over HTTP: the treamable HTTP transport of the 2025-06-18 spec (every message is a POST; the
//! answer is JSON or an SSE stream; `Mcp-Session-Id` is kept and sent back) and, when a server
//! answers a 4xx to the first POST, the older HTTP+SSE transport (a GET stream announces the
//! address to POST to, and the answers come back on the stream).
//!
//! A server that answers 401 needs a login: see [`crate::mcp_oauth`].

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_core::config::McpServerConfig;
use reqwest::{Client, Response};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::mcp_oauth::OAuth;

/// The most a single message or stream buffer may hold.
const MAX_MESSAGE: usize = 16 << 20;
const ENDPOINT_WAIT: Duration = Duration::from_secs(30);

/// A server that answered 401 and has no usable login.
#[derive(Debug)]
pub struct NeedsLogin(pub String);

impl std::fmt::Display for NeedsLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mcp server `{0}` needs login: run `nexo mcp login {0}`", self.0)
    }
}

impl std::error::Error for NeedsLogin {}

/// Why a send failed.
#[derive(Debug)]
enum Fail {
    /// 401 and no login that works.
    Login,
    /// Any other HTTP status, with the start of its body.
    Status(u16, String),
    /// The network, or something the server sent that cannot be used.
    Other(String),
}

fn net(e: reqwest::Error) -> Fail {
    // the error text names the address, never a header
    Fail::Other(e.without_url().to_string())
}

/// Replaces every `${NAME}` (or `${NAME:-default}`) with the environment variable.
pub fn expand_env(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("`{s}` has a `${{` that is not closed"));
        };
        let spec = &after[..end];
        let (name, default) = match spec.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (spec, None),
        };
        match std::env::var(name) {
            Ok(v) if !v.is_empty() => out.push_str(&v),
            _ => match default {
                Some(d) => out.push_str(d),
                None => return Err(format!("the environment variable {name} is not set")),
            },
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// An incremental parser of a server-sent events stream: bytes in, `(event, data)` out.
#[derive(Default)]
pub(crate) struct Sse {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl Sse {
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                if !self.data.is_empty() {
                    let ev = std::mem::take(&mut self.event);
                    out.push((if ev.is_empty() { "message".to_string() } else { ev }, self.data.join("\n")));
                }
                self.event.clear();
                self.data.clear();
            } else if !line.starts_with(':') {
                let (field, value) = line.split_once(':').unwrap_or((&line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => self.event = value.to_string(),
                    "data" => self.data.push(value.to_string()),
                    _ => {}
                }
            }
        }
        out
    }

    fn too_big(&self) -> bool {
        self.buf.len() > MAX_MESSAGE
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
/// Where the methods of the server's notifications go.
type Sink = Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>;

fn forward(sink: &Sink, method: &str) {
    if let Some(tx) = sink.lock().ok().and_then(|s| s.clone()) {
        let _ = tx.send(method.to_string());
    }
}

/// The legacy transport's open stream.
struct Legacy {
    post_url: String,
    pending: Pending,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
enum Mode {
    /// Nothing sent yet.
    Unknown,
    Streamable,
    Legacy(Arc<Legacy>),
}

/// One remote server's connection.
pub struct HttpLink {
    name: String,
    url: String,
    headers: Vec<(String, String)>,
    legacy_only: bool,
    client: Client,
    oauth: OAuth,
    next_id: AtomicU64,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    mode: Mutex<Mode>,
    needs_login: AtomicBool,
    sink: Sink,
    reinit: tokio::sync::Mutex<()>,
}

impl HttpLink {
    /// Reads the address and headers of `cfg` (their `${VAR}` filled in).
    pub fn new(name: &str, cfg: &McpServerConfig, auth_file: &Path) -> Result<HttpLink, String> {
        let url = expand_env(cfg.url.as_deref().unwrap_or("")).map_err(|e| format!("mcp server `{name}`: url: {e}"))?;
        let parsed = url::Url::parse(&url).map_err(|e| format!("mcp server `{name}`: bad url: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!("mcp server `{name}`: the url must start with http:// or https://"));
        }
        let mut headers = Vec::new();
        for (k, v) in &cfg.headers {
            let v = expand_env(v).map_err(|e| format!("mcp server `{name}`: header {k}: {e}"))?;
            headers.push((k.clone(), v));
        }
        let client = nexo_providers::build_client(|| {
            Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .user_agent(concat!("nexo/", env!("CARGO_PKG_VERSION")))
        })?;
        Ok(HttpLink {
            name: name.to_string(),
            url,
            headers,
            legacy_only: cfg.transport.as_deref() == Some("sse"),
            oauth: OAuth::new(name, auth_file, client.clone()),
            client,
            next_id: AtomicU64::new(1),
            session: Mutex::new(None),
            protocol: Mutex::new(None),
            mode: Mutex::new(Mode::Unknown),
            needs_login: AtomicBool::new(false),
            sink: Arc::default(),
            reinit: tokio::sync::Mutex::new(()),
        })
    }

    /// Sends the method of every notification the server makes to `tx`.
    pub fn set_sink(&self, tx: tokio::sync::mpsc::UnboundedSender<String>) {
        if let Ok(mut s) = self.sink.lock() {
            *s = Some(tx);
        }
    }

    /// True once a request met a 401 that no stored login could answer.
    pub fn needs_login(&self) -> bool {
        self.needs_login.load(Ordering::SeqCst)
    }

    fn msg(&self, f: Fail) -> String {
        match f {
            Fail::Login => format!("needs login: run `nexo mcp login {}`", self.name),
            Fail::Status(c, b) if b.is_empty() => format!("HTTP {c}"),
            Fail::Status(c, b) => format!("HTTP {c}: {b}"),
            Fail::Other(m) => m,
        }
    }

    /// A request; the error is the server's message or the transport's.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.run(method, &body, id).await.map_err(|f| self.msg(f))
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mode = self.mode.lock().map(|m| m.clone()).unwrap_or(Mode::Unknown);
        let url = match &mode {
            Mode::Legacy(l) => l.post_url.clone(),
            _ => self.url.clone(),
        };
        self.post(&url, &body).await.map(|_| ()).map_err(|f| self.msg(f))
    }

    async fn run(&self, method: &str, body: &Value, id: u64) -> Result<Value, Fail> {
        let stale = self.session.lock().ok().and_then(|s| s.clone());
        match self.run_once(method, body, id).await {
            // 404 on a session the server forgot: start a new one once and send the request again
            Err(Fail::Status(404, _)) if method != "initialize" && stale.is_some() => {
                self.reinit(stale).await?;
                self.run_once(method, body, id).await.map_err(|f| match f {
                    Fail::Status(404, _) => {
                        Fail::Other("the server ended the session (HTTP 404) and did not accept a new one".into())
                    }
                    f => f,
                })
            }
            r => r,
        }
    }

    /// A new session after the server answered 404 to `stale`. Requests that met the same 404 at
    /// once share the one new session.
    async fn reinit(&self, stale: Option<String>) -> Result<(), Fail> {
        let _one_at_a_time = self.reinit.lock().await;
        if self.session.lock().ok().and_then(|s| s.clone()) != stale {
            return Ok(());
        }
        let version = self.protocol.lock().ok().and_then(|p| p.clone());
        if let Ok(mut s) = self.session.lock() {
            *s = None;
        }
        if let Ok(mut p) = self.protocol.lock() {
            *p = None;
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let version = version.unwrap_or_else(|| crate::mcp::PROTOCOL_VERSIONS[0].to_string());
        let body = json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "nexo", "version": env!("CARGO_PKG_VERSION")}
        }});
        let resp = self.post(&self.url, &body).await?;
        let sid = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
        if let (Some(sid), Ok(mut s)) = (sid, self.session.lock()) {
            *s = Some(sid);
        }
        let r = self.read_reply(resp, id).await?;
        if let (Some(v), Ok(mut p)) = (r.get("protocolVersion").and_then(Value::as_str), self.protocol.lock()) {
            *p = Some(v.to_string());
        }
        self.post(&self.url, &json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})).await?;
        Ok(())
    }

    async fn run_once(&self, method: &str, body: &Value, id: u64) -> Result<Value, Fail> {
        let mut mode = self.mode.lock().map(|m| m.clone()).unwrap_or(Mode::Unknown);
        if matches!(mode, Mode::Unknown) && self.legacy_only {
            mode = Mode::Legacy(self.connect_legacy().await?);
            self.set_mode(mode.clone());
        }
        if let Mode::Legacy(l) = &mode {
            return self.legacy_request(l, body, id).await;
        }
        match self.post(&self.url, body).await {
            Ok(resp) => {
                if method == "initialize" {
                    let sid = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
                    if let (Some(sid), Ok(mut s)) = (sid, self.session.lock()) {
                        *s = Some(sid);
                    }
                }
                self.set_mode(Mode::Streamable);
                let r = self.read_reply(resp, id).await?;
                if method == "initialize" {
                    if let (Some(v), Ok(mut p)) =
                        (r.get("protocolVersion").and_then(Value::as_str), self.protocol.lock())
                    {
                        *p = Some(v.to_string());
                    }
                }
                Ok(r)
            }
            // the first POST refused with a 4xx other than 401: maybe an older server
            Err(Fail::Status(code, text)) if method == "initialize" && matches!(mode, Mode::Unknown) => {
                match self.connect_legacy().await {
                    Ok(l) => {
                        self.set_mode(Mode::Legacy(l.clone()));
                        self.legacy_request(&l, body, id).await
                    }
                    Err(Fail::Login) => Err(Fail::Login),
                    Err(_) => Err(Fail::Status(code, text)),
                }
            }
            Err(f) => Err(f),
        }
    }

    fn set_mode(&self, m: Mode) {
        if let Ok(mut g) = self.mode.lock() {
            *g = m;
        }
    }

    /// POSTs one message. A 401 is tried again once with a refreshed token.
    async fn post(&self, url: &str, body: &Value) -> Result<Response, Fail> {
        for attempt in 0..2 {
            let token = self.oauth.token(attempt == 1).await;
            let rb = self
                .client
                .post(url)
                .header("accept", "application/json, text/event-stream")
                .header("content-type", "application/json")
                .body(body.to_string());
            let resp = self.common(rb, token.as_deref()).send().await.map_err(net)?;
            let code = resp.status().as_u16();
            if code == 401 {
                if attempt == 0 && token.is_some() {
                    continue;
                }
                self.needs_login.store(true, Ordering::SeqCst);
                return Err(Fail::Login);
            }
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(Fail::Status(
                    code,
                    nexo_core::redact::redact(&crate::util::cut_line(&text, 200)).to_string(),
                ));
            }
            return Ok(resp);
        }
        Err(Fail::Login)
    }

    /// The configured headers, the session, the protocol version and the login's token.
    fn common(&self, mut rb: reqwest::RequestBuilder, token: Option<&str>) -> reqwest::RequestBuilder {
        let mut own_auth = false;
        for (k, v) in &self.headers {
            own_auth |= k.eq_ignore_ascii_case("authorization");
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let (Some(t), false) = (token, own_auth) {
            rb = rb.bearer_auth(t);
        }
        if let Some(s) = self.session.lock().ok().and_then(|s| s.clone()) {
            rb = rb.header("mcp-session-id", s);
        }
        if let Some(p) = self.protocol.lock().ok().and_then(|p| p.clone()) {
            rb = rb.header("mcp-protocol-version", p);
        }
        rb
    }

    /// The answer to request `id` from a POST's response: a JSON body or an SSE stream.
    async fn read_reply(&self, mut resp: Response, id: u64) -> Result<Value, Fail> {
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
        if ct.starts_with("text/event-stream") {
            let mut p = Sse::default();
            while let Some(chunk) = resp.chunk().await.map_err(net)? {
                for (ev, data) in p.push(&chunk) {
                    if ev == "message" {
                        if let Some(r) = self.on_message(&data, id).await {
                            return r;
                        }
                    }
                }
                if p.too_big() {
                    return Err(Fail::Other("the server sent a message over 16 MiB".into()));
                }
            }
            return Err(Fail::Other("the stream ended without an answer".into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net)? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_MESSAGE {
                return Err(Fail::Other("the server sent a message over 16 MiB".into()));
            }
        }
        let v: Value = serde_json::from_slice(&body).map_err(|_| Fail::Other("the answer is not JSON".into()))?;
        let msgs = match v {
            Value::Array(a) => a,
            one => vec![one],
        };
        for m in msgs {
            if let Some(r) = self.on_message(&m.to_string(), id).await {
                return r;
            }
        }
        Err(Fail::Other("the server answered without a result".into()))
    }

    /// One JSON-RPC message of a reply: the answer to `id` ends the wait; a ping is answered; the
    /// rest is ignored.
    async fn on_message(&self, data: &str, id: u64) -> Option<Result<Value, Fail>> {
        let msg: Value = serde_json::from_str(data).ok()?;
        if let (Some(rid), Some(_)) = (msg.get("id"), msg.get("method")) {
            let reply = if msg["method"] == "ping" {
                json!({"jsonrpc": "2.0", "id": rid, "result": {}})
            } else {
                json!({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "not supported by this client"}})
            };
            let url = match self.mode.lock().map(|m| m.clone()) {
                Ok(Mode::Legacy(l)) => l.post_url.clone(),
                _ => self.url.clone(),
            };
            let _ = self.post(&url, &reply).await;
            return None;
        }
        if let (None, Some(m)) = (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
            forward(&self.sink, m);
            return None;
        }
        if msg.get("id").and_then(Value::as_u64) != Some(id) {
            return None;
        }
        Some(match msg.get("error") {
            Some(e) => Err(Fail::Other(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string())),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        })
    }

    // ---- the legacy HTTP+SSE transport ----

    async fn connect_legacy(&self) -> Result<Arc<Legacy>, Fail> {
        let token = self.oauth.token(false).await;
        let rb = self.client.get(&self.url).header("accept", "text/event-stream");
        let resp = self.common(rb, token.as_deref()).send().await.map_err(net)?;
        let code = resp.status().as_u16();
        if code == 401 {
            self.needs_login.store(true, Ordering::SeqCst);
            return Err(Fail::Login);
        }
        if !resp.status().is_success() {
            return Err(Fail::Status(code, String::new()));
        }
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ep_tx, ep_rx) = oneshot::channel::<String>();
        let reader_pending = pending.clone();
        let reader_sink = self.sink.clone();
        let task = tokio::spawn(async move {
            let mut resp = resp;
            let mut p = Sse::default();
            let mut ep = Some(ep_tx);
            while let Ok(Some(chunk)) = resp.chunk().await {
                for (ev, data) in p.push(&chunk) {
                    if ev == "endpoint" {
                        if let Some(tx) = ep.take() {
                            let _ = tx.send(data.trim().to_string());
                        }
                    } else if ev == "message" {
                        dispatch(&reader_pending, &reader_sink, &data);
                    }
                }
                if p.too_big() {
                    break;
                }
            }
            let waiting: Vec<_> = reader_pending.lock().map(|mut m| m.drain().collect()).unwrap_or_default();
            for (_, tx) in waiting {
                let _ = tx.send(Err("the server closed the stream".into()));
            }
        });
        let endpoint = match tokio::time::timeout(ENDPOINT_WAIT, ep_rx).await {
            Ok(Ok(e)) => e,
            _ => {
                task.abort();
                return Err(Fail::Other("the server did not announce where to send messages".into()));
            }
        };
        // the address may be relative, and must stay on the server's own origin
        let base = url::Url::parse(&self.url).map_err(|e| Fail::Other(e.to_string()))?;
        let post_url = base.join(&endpoint).map_err(|e| Fail::Other(format!("bad endpoint: {e}")))?;
        if post_url.origin() != base.origin() {
            task.abort();
            return Err(Fail::Other("the server named an address on another host to send messages to".into()));
        }
        Ok(Arc::new(Legacy { post_url: post_url.to_string(), pending, task }))
    }

    async fn legacy_request(&self, l: &Legacy, body: &Value, id: u64) -> Result<Value, Fail> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = l.pending.lock() {
            p.insert(id, tx);
        }
        if let Err(e) = self.post(&l.post_url, body).await {
            if let Ok(mut p) = l.pending.lock() {
                p.remove(&id);
            }
            return Err(e);
        }
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(Fail::Other(e)),
            Err(_) => Err(Fail::Other("the server closed the stream".into())),
        }
    }

    /// Ends the session: tells the server (best effort) and stops the stream.
    pub async fn close(&self) {
        let mode = self.mode.lock().map(|m| m.clone()).unwrap_or(Mode::Unknown);
        if let Mode::Legacy(l) = mode {
            l.task.abort();
        }
        let sid = self.session.lock().ok().and_then(|s| s.clone());
        if sid.is_some() {
            let rb = self.client.delete(&self.url).timeout(Duration::from_secs(3));
            let _ = self.common(rb, None).send().await;
        }
    }
}

/// A message that came down the legacy stream.
fn dispatch(pending: &Pending, sink: &Sink, data: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(data) else { return };
    if let Some(m) = msg.get("method").and_then(Value::as_str) {
        if msg.get("id").is_none() {
            forward(sink, m);
        }
        return;
    }
    let Some(id) = msg.get("id").and_then(Value::as_u64) else { return };
    let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
    if let Some(tx) = tx {
        let _ = tx.send(match msg.get("error") {
            Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_are_filled_in() {
        std::env::set_var("NEXO_TEST_MCP_VAR", "abc");
        assert_eq!(expand_env("Bearer ${NEXO_TEST_MCP_VAR}!").unwrap(), "Bearer abc!");
        assert_eq!(expand_env("${NEXO_TEST_MCP_UNSET_X:-fallback}").unwrap(), "fallback");
        assert_eq!(expand_env("plain").unwrap(), "plain");
        assert!(expand_env("${NEXO_TEST_MCP_UNSET_X}").unwrap_err().contains("NEXO_TEST_MCP_UNSET_X"));
        assert!(expand_env("${open").is_err());
    }

    #[test]
    fn events_are_cut_out_of_chunks() {
        let mut p = Sse::default();
        assert!(p.push(b"event: endpoint\nda").is_empty());
        let a = p.push(b"ta: /m?s=1\n\n: comment\ndata: {\"a\":\r\ndata: 1}\r\n\r\n");
        assert_eq!(
            a,
            vec![("endpoint".to_string(), "/m?s=1".to_string()), ("message".to_string(), "{\"a\":\n1}".to_string())]
        );
    }
}
