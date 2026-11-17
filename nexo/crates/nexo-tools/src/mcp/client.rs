use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::names::{full_name, MAX_NAME};
use super::types::{PromptArg, PromptDef, PromptText, ResourceDef, TemplateDef, ToolDef};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use nexo_core::config::McpServerConfig;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// The protocol versions asked for, newest first.
pub const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2024-11-05"];
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest line a server may send; past it the connection ends.
const MAX_LINE: usize = 16 << 20;
/// How many replies to the server's requests may wait for the pipe at once. A server that stops reading
/// its input while it floods requests gets no more answers beyond this, instead of a task for each.
const MAX_REPLIES_WAITING: usize = 256;
/// The most characters of one tool result that go back to the model (the same as `bash`).
pub const MAX_RESULT_CHARS: usize = crate::bash::MAX_OUTPUT_CHARS;
/// The variables of nexo's environment a server gets (with its own `env`): what a process needs
/// to run, and never another service's keys.
pub(super) fn passed_env(name: &str) -> bool {
    const UNIX: [&str; 9] = ["PATH", "HOME", "USER", "LANG", "TERM", "TMPDIR", "TMP", "TEMP", "SHELL"];
    const WINDOWS: [&str; 10] = [
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "PATHEXT",
        "COMSPEC",
        "WINDIR",
        "PROGRAMDATA",
        "PROGRAMFILES",
    ];
    let up = if cfg!(windows) { name.to_ascii_uppercase() } else { name.to_string() };
    UNIX.contains(&up.as_str())
        || up.starts_with("LC_")
        || up.starts_with("XDG_")
        || (cfg!(windows) && WINDOWS.contains(&up.as_str()))
}

/// A connected server.
pub struct McpClient {
    name: String,
    inner: Arc<Inner>,
    /// The server's process (stdio servers only).
    child: AsyncMutex<Option<Child>>,
    /// What the server said it is.
    pub server_info: Value,
    pub protocol_version: String,
}

/// How messages reach the server.
enum Link {
    Stdio(AsyncMutex<ChildStdin>),
    Http(Box<crate::mcp_http::HttpLink>),
}

struct Inner {
    name: String,
    /// `trust_read_only` of the server's config: whether its `readOnlyHint` is believed.
    trust_read_only: bool,
    link: Link,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    /// What the server advertised in `initialize`.
    caps: std::sync::OnceLock<Value>,
    /// Why the server's output ended (set once, before the waiting requests are failed).
    gone: std::sync::OnceLock<String>,
    /// The lists the server offers, kept up to date by the `list_changed` notifications.
    lists: Mutex<Lists>,
    /// Counts every change of the lists; the hub and the interface wait on it.
    changes: tokio::sync::watch::Sender<u64>,
    /// Replies to the server's own requests that are still waiting to be written.
    replies_waiting: AtomicUsize,
}

impl McpClient {
    /// Starts the server (or reaches it, for a `url`) and performs the handshake and the tool
    /// listing. A remote server's login is read from nexo's credential store.
    pub async fn connect(name: &str, cfg: &McpServerConfig) -> anyhow::Result<McpClient> {
        McpClient::connect_in(name, cfg, &nexo_core::auth::file()).await
    }

    /// [`McpClient::connect`] with the credential store at `auth_file`.
    pub async fn connect_in(
        name: &str,
        cfg: &McpServerConfig,
        auth_file: &std::path::Path,
    ) -> anyhow::Result<McpClient> {
        if cfg.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
            let link = crate::mcp_http::HttpLink::new(name, cfg, auth_file).map_err(|e| anyhow!(e))?;
            let inner = Inner::new(name, cfg.trust_read_only, Link::Http(Box::new(link)));
            // the notifications that come with an answer, or down the old transport's stream
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            if let Link::Http(h) = &inner.link {
                h.set_sink(tx);
            }
            let weak = Arc::downgrade(&inner);
            tokio::spawn(async move {
                while let Some(method) = rx.recv().await {
                    match weak.upgrade() {
                        Some(i) => i.on_notification(&method),
                        None => break,
                    }
                }
            });
            return McpClient::handshake(name, inner, None).await;
        }
        if cfg.command.trim().is_empty() {
            return Err(anyhow!("mcp server `{name}`: no command or url"));
        }
        // on Windows `npx` and `uvx` are `.cmd` files, which a bare name does not find
        let mut cmd = tokio::process::Command::new(nexo_core::shell::program(&cfg.command));
        cmd.args(&cfg.args)
            .env_clear()
            .envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(passed_env)))
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(d) = &cfg.cwd {
            cmd.current_dir(d);
        }
        let mut child = cmd.spawn().with_context(|| format!("mcp server `{name}`: cannot start `{}`", cfg.command))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let inner = Inner::new(name, cfg.trust_read_only, Link::Stdio(AsyncMutex::new(stdin)));
        let reader = inner.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stdout);
            let mut buf = Vec::new();
            let mut why = "the server closed the connection".to_string();
            // bytes, not lines(): one non-UTF-8 byte would end the connection
            while matches!(read_line(&mut r, &mut buf).await, Ok(n) if n > 0) {
                if buf.len() > MAX_LINE {
                    why = format!("the server sent a line over {} MiB; the connection was ended", MAX_LINE >> 20);
                    tracing::warn!(server = %reader.name, "mcp: {why}");
                    break;
                }
                let line = String::from_utf8_lossy(&buf).trim().to_string();
                buf.clear();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(&line) {
                    Ok(msg) => reader.dispatch(msg).await,
                    Err(e) => tracing::debug!(server = %reader.name, "mcp: not json: {e}"),
                }
            }
            // the server went away: later requests fail at once, and so does what is still waiting
            let _ = reader.gone.set(why.clone());
            let pending: Vec<_> = reader.pending.lock().map(|mut p| p.drain().collect()).unwrap_or_default();
            for (_, tx) in pending {
                let _ = tx.send(Err(why.clone()));
            }
        });
        McpClient::handshake(name, inner, Some(child)).await
    }

    /// The `initialize` exchange and the tool listing.
    async fn handshake(name: &str, inner: Arc<Inner>, child: Option<Child>) -> anyhow::Result<McpClient> {
        let mut init = Err(anyhow!("no protocol version accepted"));
        for v in PROTOCOL_VERSIONS {
            let params = json!({
                "protocolVersion": v,
                "capabilities": {},
                "clientInfo": {"name": "nexo", "version": env!("CARGO_PKG_VERSION")}
            });
            match tokio::time::timeout(INIT_TIMEOUT, inner.request("initialize", params)).await {
                Ok(Ok(r)) => {
                    init = Ok(r);
                    break;
                }
                Ok(Err(e)) if e.contains("version") => continue,
                Ok(Err(_)) if inner.needs_login() => return Err(crate::mcp_http::NeedsLogin(name.to_string()).into()),
                Ok(Err(e)) => return Err(anyhow!("mcp server `{name}`: initialize failed: {e}")),
                Err(_) => return Err(anyhow!("mcp server `{name}`: initialize timed out")),
            }
        }
        let init = init?;
        inner.notify("notifications/initialized", json!({})).await?;
        let protocol_version = init.get("protocolVersion").and_then(Value::as_str).unwrap_or("").to_string();
        let server_info = init.get("serverInfo").cloned().unwrap_or(Value::Null);
        let _ = inner.caps.set(init.get("capabilities").cloned().unwrap_or(Value::Null));
        let tools = list_tools(&inner).await?;
        // resources and prompts only when the server says it has them; a failure there is not the server's end
        let (resources, templates) = if inner.has_cap("resources") {
            inner.list_resources().await.unwrap_or_default()
        } else {
            Default::default()
        };
        let prompts =
            if inner.has_cap("prompts") { list_prompts(&inner).await.unwrap_or_default() } else { Vec::new() };
        if let Ok(mut l) = inner.lists.lock() {
            *l = Lists { tools, resources, templates, prompts };
        }
        Ok(McpClient { name: name.to_string(), inner, child: AsyncMutex::new(child), server_info, protocol_version })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn defs(&self) -> Vec<ToolDef> {
        self.inner.lists().tools
    }

    /// What the server advertised in `initialize` (`tools`, `resources`, `prompts`...).
    pub fn capabilities(&self) -> Value {
        self.inner.caps.get().cloned().unwrap_or(Value::Null)
    }

    /// The listed resources (empty when the server has none or does not offer them).
    pub fn resources(&self) -> Vec<ResourceDef> {
        self.inner.lists().resources
    }

    pub fn templates(&self) -> Vec<TemplateDef> {
        self.inner.lists().templates
    }

    pub fn prompts(&self) -> Vec<PromptDef> {
        self.inner.lists().prompts
    }

    /// A counter that goes up whenever the server's lists change (a `list_changed` notification was
    /// handled, or `refresh` ran).
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// The server's tools as nexo tools (they keep the connection alive through a shared handle).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut taken = std::collections::HashSet::new();
        self.defs()
            .into_iter()
            .map(|d| {
                // two names that end up the same (`a b`, `a_b`) get a number
                let mut full_name = full_name(&self.name, &d.name);
                let mut n = 1;
                while !taken.insert(full_name.clone()) {
                    n += 1;
                    let tail = format!("_{n}");
                    full_name = format!("{}{tail}", &full_name[..full_name.len().min(MAX_NAME - tail.len())]);
                }
                Arc::new(McpTool { server: self.name.clone(), full_name, def: d, inner: self.inner.clone() })
                    as Arc<dyn Tool>
            })
            .collect()
    }

    /// Lists everything again (a server may add some after a while); the number of tools.
    pub async fn refresh(&self) -> anyhow::Result<usize> {
        self.inner.refresh(Kind::Tools).await.map_err(|e| anyhow!(e))?;
        if self.inner.has_cap("resources") {
            let _ = self.inner.refresh(Kind::Resources).await;
        }
        if self.inner.has_cap("prompts") {
            let _ = self.inner.refresh(Kind::Prompts).await;
        }
        Ok(self.defs().len())
    }

    /// `resources/read`: the text of the resource (a binary one is described, not returned).
    pub async fn read_resource(&self, uri: &str) -> Result<String, String> {
        let r = self.inner.call_timed("resources/read", json!({"uri": uri})).await?;
        let mut out = Vec::new();
        for c in r.get("contents").and_then(Value::as_array).into_iter().flatten() {
            if let Some(t) = c.get("text").and_then(Value::as_str) {
                out.push(t.to_string());
            } else if let Some(b) = c.get("blob").and_then(Value::as_str) {
                let mime = c.get("mimeType").and_then(Value::as_str).unwrap_or("binary");
                out.push(format!("[{mime} content, {} bytes in base64, not shown]", b.len()));
            }
        }
        if out.is_empty() {
            return Err("the resource has no content".into());
        }
        Ok(out.join("\n"))
    }

    /// `prompts/get`: the messages of the prompt filled in with `args`.
    pub async fn get_prompt(&self, name: &str, args: &BTreeMap<String, String>) -> Result<PromptText, String> {
        let r = self.inner.call_timed("prompts/get", json!({"name": name, "arguments": args})).await?;
        let mut messages = Vec::new();
        for m in r.get("messages").and_then(Value::as_array).into_iter().flatten() {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user").to_string();
            let c = m.get("content").cloned().unwrap_or(Value::Null);
            // one block, or a list of them
            let blocks = match c {
                Value::Array(a) => a,
                one => vec![one],
            };
            let mut text = String::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => text.push_str(b.get("text").and_then(Value::as_str).unwrap_or("")),
                    Some("resource") => {
                        let t = b.pointer("/resource/text").and_then(Value::as_str);
                        text.push_str(t.unwrap_or("[resource]"));
                    }
                    Some("image") => text.push_str("[image]"),
                    Some("audio") => text.push_str("[audio]"),
                    _ => {}
                }
            }
            messages.push((role, text));
        }
        if messages.is_empty() {
            return Err("the prompt has no messages".into());
        }
        let description = r.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        Ok(PromptText { description, messages })
    }

    /// Stops the server.
    pub async fn close(&self) {
        if let Some(c) = self.child.lock().await.as_mut() {
            let _ = c.kill().await;
        }
        if let Link::Http(h) = &self.inner.link {
            h.close().await;
        }
    }
}

#[derive(Default, Clone)]
struct Lists {
    tools: Vec<ToolDef>,
    resources: Vec<ResourceDef>,
    templates: Vec<TemplateDef>,
    prompts: Vec<PromptDef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tools,
    Resources,
    Prompts,
}

fn text_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

async fn list_tools(inner: &Arc<Inner>) -> anyhow::Result<Vec<ToolDef>> {
    let items = paged(inner, "tools/list", "tools")
        .await
        .map_err(|e| anyhow!("mcp server `{}`: tools/list failed: {e}", inner.name))?;
    let mut out = Vec::new();
    for t in items {
        let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
        if out.iter().any(|d: &ToolDef| d.name == name) {
            tracing::warn!(server = %inner.name, "mcp: tool `{name}` is listed twice; the second is ignored");
            continue;
        }
        out.push(ToolDef {
            name: name.to_string(),
            description: text_of(&t, "description").to_string(),
            input_schema: t
                .get("inputSchema")
                .filter(|s| s.is_object())
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            read_only: t.pointer("/annotations/readOnlyHint").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(out)
}

async fn list_prompts(inner: &Arc<Inner>) -> Result<Vec<PromptDef>, String> {
    let mut out = Vec::new();
    for p in paged(inner, "prompts/list", "prompts").await? {
        let Some(name) = p.get("name").and_then(Value::as_str) else { continue };
        let arguments = p
            .get("arguments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| {
                Some(PromptArg {
                    name: a.get("name").and_then(Value::as_str)?.to_string(),
                    description: text_of(a, "description").to_string(),
                    required: a.get("required").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        out.push(PromptDef { name: name.to_string(), description: text_of(&p, "description").to_string(), arguments });
    }
    Ok(out)
}

impl Inner {
    fn new(name: &str, trust_read_only: bool, link: Link) -> Arc<Inner> {
        Arc::new(Inner {
            name: name.to_string(),
            trust_read_only,
            link,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            caps: std::sync::OnceLock::new(),
            gone: std::sync::OnceLock::new(),
            lists: Mutex::new(Lists::default()),
            changes: tokio::sync::watch::channel(0).0,
            replies_waiting: AtomicUsize::new(0),
        })
    }

    fn lists(&self) -> Lists {
        self.lists.lock().map(|l| l.clone()).unwrap_or_default()
    }

    fn has_cap(&self, what: &str) -> bool {
        self.caps.get().is_some_and(|c| c.get(what).is_some())
    }

    /// `resources/list` and `resources/templates/list`. A server may not do the second.
    async fn list_resources(&self) -> Result<(Vec<ResourceDef>, Vec<TemplateDef>), String> {
        let resources = paged(self, "resources/list", "resources")
            .await?
            .iter()
            .filter_map(|r| {
                Some(ResourceDef {
                    uri: r.get("uri").and_then(Value::as_str)?.to_string(),
                    name: text_of(r, "name").to_string(),
                    description: text_of(r, "description").to_string(),
                    mime_type: text_of(r, "mimeType").to_string(),
                })
            })
            .collect();
        let templates = paged(self, "resources/templates/list", "resourceTemplates")
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| {
                Some(TemplateDef {
                    uri_template: r.get("uriTemplate").and_then(Value::as_str)?.to_string(),
                    name: text_of(r, "name").to_string(),
                    description: text_of(r, "description").to_string(),
                })
            })
            .collect();
        Ok((resources, templates))
    }

    /// Lists one kind again and tells the watchers.
    async fn refresh(self: &Arc<Self>, kind: Kind) -> Result<(), String> {
        match kind {
            Kind::Tools => {
                let t = list_tools(self).await.map_err(|e| e.to_string())?;
                if let Ok(mut l) = self.lists.lock() {
                    l.tools = t;
                }
            }
            Kind::Resources => {
                let (r, t) = self.list_resources().await?;
                if let Ok(mut l) = self.lists.lock() {
                    l.resources = r;
                    l.templates = t;
                }
            }
            Kind::Prompts => {
                let p = list_prompts(self).await?;
                if let Ok(mut l) = self.lists.lock() {
                    l.prompts = p;
                }
            }
        }
        self.changes.send_modify(|n| *n += 1);
        Ok(())
    }

    /// A notification of the server: a list changed, so it is read again (in a task, never in the
    /// reader that delivered the news).
    fn on_notification(self: &Arc<Self>, method: &str) {
        let kind = match method {
            "notifications/tools/list_changed" => Kind::Tools,
            "notifications/resources/list_changed" => Kind::Resources,
            "notifications/prompts/list_changed" => Kind::Prompts,
            _ => return,
        };
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.refresh(kind).await {
                tracing::debug!(server = %me.name, "mcp: cannot refresh {kind:?}: {e}");
            }
        });
    }

    /// A request with the call timeout.
    async fn call_timed(self: &Arc<Self>, method: &str, params: Value) -> Result<Value, String> {
        match tokio::time::timeout(CALL_TIMEOUT, self.request(method, params)).await {
            Ok(r) => r,
            Err(_) => Err(format!("{method} timed out after {} s", CALL_TIMEOUT.as_secs())),
        }
    }

    async fn send(&self, msg: Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        let Link::Stdio(stdin) = &self.link else { return Err(anyhow!("not a stdio server")) };
        let mut stdin = stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    fn needs_login(&self) -> bool {
        matches!(&self.link, Link::Http(h) if h.needs_login())
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        if let Link::Http(h) = &self.link {
            return h.notify(method, params).await.map_err(|e| anyhow!(e));
        }
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params})).await
    }

    /// A request; the error is the server's message or the transport's.
    async fn request(self: &Arc<Self>, method: &str, params: Value) -> Result<Value, String> {
        if let Link::Http(h) = &self.link {
            return h.request(method, params).await;
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        // A caller that gives up (call timeout, Esc) drops this future; the guard then frees the entry
        // and tells the server, so it does not go on working for nobody.
        let mut guard = PendingGuard { inner: self.clone(), id, method: method.to_string(), armed: true };
        if let Err(e) = self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await {
            guard.armed = false;
            return Err(format!("cannot write to the server (it has probably exited): {e}"));
        }
        // the server's output may have ended just before the request was filed
        if let Some(why) = self.gone.get() {
            guard.armed = false;
            return Err(why.clone());
        }
        match rx.await {
            Ok(r) => {
                // dispatch() already took the entry
                guard.armed = false;
                r
            }
            Err(_) => {
                guard.armed = false;
                Err("the server closed the connection".into())
            }
        }
    }

    async fn dispatch(self: &Arc<Self>, msg: Value) {
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if msg.get("method").is_some() {
                // a request from the server: answer ping, refuse the rest
                let reply = if msg["method"] == "ping" {
                    json!({"jsonrpc": "2.0", "id": id, "result": {}})
                } else {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported by this client"}})
                };
                // Answered from its own task: the reader must go on draining the server's output while a
                // write waits for the stdin lock or a full pipe, or both sides block on each other.
                if self.replies_waiting.fetch_add(1, Ordering::SeqCst) >= MAX_REPLIES_WAITING {
                    self.replies_waiting.fetch_sub(1, Ordering::SeqCst);
                    tracing::debug!(server = %self.name, "mcp: {MAX_REPLIES_WAITING} replies are waiting to be written; one was dropped");
                    return;
                }
                let me = self.clone();
                tokio::spawn(async move {
                    let _ = me.send(reply).await;
                    me.replies_waiting.fetch_sub(1, Ordering::SeqCst);
                });
                return;
            }
            let tx = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(tx) = tx {
                let r = if let Some(err) = msg.get("error") {
                    Err(err.get("message").and_then(Value::as_str).unwrap_or("error").to_string())
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(r);
            }
        } else if let Some(m) = msg.get("method").and_then(Value::as_str) {
            self.on_notification(m);
        }
    }
}

/// Frees a stdio request's `pending` entry when its future is dropped before the answer came, and
/// then asks the server to stop working on it with `notifications/cancelled`.
struct PendingGuard {
    inner: Arc<Inner>,
    id: u64,
    method: String,
    /// False once the request ended by itself (answered, or the server is gone): nothing to cancel.
    armed: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut p) = self.inner.pending.lock() {
            p.remove(&self.id);
        }
        // the spec forbids cancelling `initialize`
        if !self.armed || self.method == "initialize" {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let inner = self.inner.clone();
        let params = json!({"requestId": self.id, "reason": "client timeout/cancel"});
        rt.spawn(async move {
            let _ = inner.notify("notifications/cancelled", params).await;
        });
    }
}

/// One remote tool.
pub struct McpTool {
    server: String,
    full_name: String,
    def: ToolDef,
    inner: Arc<Inner>,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn server(&self) -> Option<&str> {
        Some(&self.server)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.full_name.clone(),
            description: if self.def.description.is_empty() {
                format!("Tool `{}` of the MCP server `{}`.", self.def.name, self.server)
            } else {
                self.def.description.clone()
            },
            parameters: self.def.input_schema.clone(),
        }
    }

    fn kind(&self) -> ToolKind {
        // a server's own word that a tool only reads is taken when its config says to trust it
        if self.def.read_only && self.inner.trust_read_only {
            ToolKind::Read
        } else {
            ToolKind::Other
        }
    }

    fn summary(&self, args: &Value) -> String {
        let a = serde_json::to_string(args).unwrap_or_default();
        format!("mcp {}:{} {}", self.server, self.def.name, crate::util::cut_line(&a, 80))
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let params = json!({"name": self.def.name, "arguments": if args.is_object() { args } else { json!({}) }});
        let call = self.inner.request("tools/call", params);
        let r = tokio::select! {
            _ = ctx.cancel.cancelled() => return ToolOutput::error("mcp: cancelled"),
            r = tokio::time::timeout(CALL_TIMEOUT, call) => r,
        };
        match r {
            Err(_) => ToolOutput::error(format!(
                "mcp {}:{}: timed out after {} s",
                self.server,
                self.def.name,
                CALL_TIMEOUT.as_secs()
            )),
            Ok(Err(e)) => ToolOutput::error(format!("mcp {}:{}: {e}", self.server, self.def.name)),
            Ok(Ok(result)) => {
                let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let mut text = String::new();
                for c in result.get("content").and_then(Value::as_array).into_iter().flatten() {
                    match c.get("type").and_then(Value::as_str) {
                        Some("text") => text.push_str(c.get("text").and_then(Value::as_str).unwrap_or("")),
                        Some("image") => text.push_str("[image]"),
                        Some("audio") => text.push_str("[audio]"),
                        Some("resource") => {
                            if let Some(t) = c.pointer("/resource/text").and_then(Value::as_str) {
                                text.push_str(t);
                            } else {
                                text.push_str("[resource]");
                            }
                        }
                        _ => {}
                    }
                    text.push('\n');
                }
                if text.trim().is_empty() {
                    if let Some(s) = result.get("structuredContent") {
                        text = serde_json::to_string_pretty(s).unwrap_or_default();
                    }
                }
                // a server may send megabytes; the model gets the head and the tail
                let text = text.trim_end();
                let cap = ctx.cap(MAX_RESULT_CHARS);
                let mut capped = crate::util::cap_text(text, cap);
                let rest = "call the tool again with arguments that ask for less";
                if let Some(fact) = ctx.cut_fact(MAX_RESULT_CHARS, cap, text.chars().count(), rest) {
                    capped.push_str(&format!("\n[{fact}]"));
                }
                let text = capped;
                let text = if text.is_empty() { "(no content)".to_string() } else { text };
                ToolOutput { content: text, is_error, display: nexo_core::ToolDisplay::Plain, images: Vec::new() }
            }
        }
    }
}

/// Reads up to a newline into `buf`, stopping once it holds more than [`MAX_LINE`] bytes.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt;
    (&mut *r).take(MAX_LINE as u64 + 1).read_until(b'\n', buf).await
}

/// Every page of a listing: the items under `key`.
async fn paged(inner: &Arc<Inner>, method: &str, key: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let r = tokio::time::timeout(INIT_TIMEOUT, inner.request(method, params))
            .await
            .map_err(|_| format!("{method} timed out"))??;
        out.extend(r.get(key).and_then(Value::as_array).cloned().unwrap_or_default());
        cursor = r.get("nextCursor").and_then(Value::as_str).map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A request the caller gave up on (call timeout, Esc) leaves nothing in `pending`, and the server is
    /// told so, except for `initialize`, which the protocol does not allow to be cancelled.
    #[tokio::test]
    async fn a_dropped_request_frees_its_entry_and_tells_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("stdin.log");
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", &format!("cat > '{}'", log.display())])
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let inner = Inner::new("silent", false, Link::Stdio(AsyncMutex::new(child.stdin.take().unwrap())));
        for method in ["tools/call", "initialize"] {
            let r = tokio::time::timeout(Duration::from_millis(200), inner.request(method, json!({}))).await;
            assert!(r.is_err(), "the server never answers {method}");
            assert_eq!(inner.pending.lock().unwrap().len(), 0, "{method}");
        }
        // The cancels are written by tasks that run once this one yields: wait for both to be due
        // before looking, or the check for `initialize` would run before its cancel could exist.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut seen = String::new();
        for _ in 0..100 {
            seen = std::fs::read_to_string(&log).unwrap_or_default();
            if seen.contains("notifications/cancelled") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let cancels: Vec<Value> = seen
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|m| m["method"] == "notifications/cancelled")
            .collect();
        assert_eq!(cancels.len(), 1, "only the tool call is cancelled: {seen}");
        assert_eq!(cancels[0]["params"]["requestId"], 1);
    }

    /// A server that floods requests and never reads its input gets a bounded number of replies waiting,
    /// not a parked task for each.
    #[tokio::test]
    async fn a_flood_of_requests_to_a_server_that_does_not_read_keeps_the_replies_bounded() {
        let mut child =
            tokio::process::Command::new("sleep").arg("60").stdin(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let inner = Inner::new("deaf", false, Link::Stdio(AsyncMutex::new(child.stdin.take().unwrap())));
        let ping = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
        // more replies than the pipe holds, so the writes really block
        for i in 0..6000 {
            inner.dispatch(ping.clone()).await;
            if i % 100 == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let waiting = inner.replies_waiting.load(Ordering::SeqCst);
        assert!(waiting > 0 && waiting <= MAX_REPLIES_WAITING, "{waiting} replies are waiting");
    }
}
