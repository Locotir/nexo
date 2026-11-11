//! A client for MCP servers over stdio (JSON-RPC 2.0, one JSON object per
//! line). The server's tools become [`Tool`]s named `mcp__<server>__<name>`.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
/// The most characters of one tool result that go back to the model (the same as `bash`).
pub const MAX_RESULT_CHARS: usize = crate::bash::MAX_OUTPUT_CHARS;
/// The longest tool name the provider APIs take.
const MAX_NAME: usize = 64;

/// The variables of nexo's environment a server gets (with its own `env`): what a process needs
/// to run, and never another service's keys.
fn passed_env(name: &str) -> bool {
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
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
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

/// A resource the server lists.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: String,
}

/// A resource template (`file:///{path}`).
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateDef {
    pub uri_template: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptDef {
    pub name: String,
    pub description: String,
    pub arguments: Vec<PromptArg>,
}

/// The answer of `prompts/get`: `(role, text)` per message.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptText {
    pub description: String,
    pub messages: Vec<(String, String)>,
}

impl PromptText {
    /// The text that goes in as the user's request: the messages in order, those not from the user
    /// marked with their role.
    pub fn as_request(&self) -> String {
        let one = self.messages.len() == 1;
        self.messages
            .iter()
            .map(|(role, text)| if one || role == "user" { text.clone() } else { format!("[{role}] {text}") })
            .collect::<Vec<_>>()
            .join("\n\n")
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

/// The words of `input`, a quoted phrase being one.
fn words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut quote, mut any) = (String::new(), None::<char>, false);
    for c in input.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The arguments of a prompt from what was typed after its command: `key=value` for a name the
/// prompt has, the other words filling the remaining arguments in order. A missing required one is an error.
pub fn parse_prompt_args(def: &PromptDef, input: &str) -> Result<BTreeMap<String, String>, String> {
    let mut named = BTreeMap::new();
    let mut loose = Vec::new();
    for w in words(input) {
        match w.split_once('=') {
            Some((k, v)) if def.arguments.iter().any(|a| a.name == k) => {
                named.insert(k.to_string(), v.to_string());
            }
            _ => loose.push(w),
        }
    }
    let mut loose = loose.into_iter();
    for a in &def.arguments {
        if !named.contains_key(&a.name) {
            if let Some(w) = loose.next() {
                named.insert(a.name.clone(), w);
            }
        }
    }
    let rest: Vec<String> = loose.collect();
    if !rest.is_empty() {
        // a prompt with one free-text argument takes the rest of the line as it is
        if let Some(last) = def.arguments.last().and_then(|a| named.get_mut(&a.name)) {
            for w in rest {
                last.push(' ');
                last.push_str(&w);
            }
        } else {
            return Err(format!("`{}` takes no arguments", def.name));
        }
    }
    for a in &def.arguments {
        if a.required && !named.contains_key(&a.name) {
            return Err(format!("missing the argument `{}` of the prompt `{}`", a.name, def.name));
        }
    }
    Ok(named)
}

/// Reads up to a newline into `buf`, stopping once it holds more than [`MAX_LINE`] bytes.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt;
    (&mut *r).take(MAX_LINE as u64 + 1).read_until(b'\n', buf).await
}

/// Every page of a listing: the items under `key`.
async fn paged(inner: &Inner, method: &str, key: &str) -> Result<Vec<Value>, String> {
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
    async fn call_timed(&self, method: &str, params: Value) -> Result<Value, String> {
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
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        if let Link::Http(h) = &self.link {
            return h.request(method, params).await;
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        if let Err(e) = self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(format!("cannot write to the server (it has probably exited): {e}"));
        }
        // the server's output may have ended just before the request was filed
        if let Some(why) = self.gone.get() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(why.clone());
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Err("the server closed the connection".into()),
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
                let _ = self.send(reply).await;
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
                let text = crate::util::cap_text(text.trim_end(), MAX_RESULT_CHARS);
                let text = if text.is_empty() { "(no content)".to_string() } else { text };
                ToolOutput { content: text, is_error, display: nexo_core::ToolDisplay::Plain, images: Vec::new() }
            }
        }
    }
}

/// `mcp__<server>__<tool>`, as a provider API accepts it: letters, digits, `_` and `-` (anything
/// else becomes `_`), at most 64 characters (a longer name is cut and ends in a hash of the whole).
pub fn full_name(server: &str, tool: &str) -> String {
    let raw = format!("mcp__{server}__{tool}");
    let mut name: String =
        raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if name.len() > MAX_NAME {
        let h = raw.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        name.truncate(MAX_NAME - 9);
        name.push_str(&format!("_{:08x}", h as u32));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_command_is_an_error() {
        let cfg = McpServerConfig { command: "/nonexistent/mcp-server".into(), ..Default::default() };
        assert!(McpClient::connect("x", &cfg).await.is_err());
        assert!(McpClient::connect("x", &McpServerConfig::default()).await.is_err());
    }
    #[test]
    fn a_server_gets_only_what_a_process_needs_to_run() {
        for k in ["PATH", "HOME", "LANG", "LC_ALL", "XDG_RUNTIME_DIR", "TMPDIR"] {
            assert!(passed_env(k), "{k}");
        }
        for k in ["DEEPSEEK_API_KEY", "OPENAI_API_KEY", "GITHUB_TOKEN", "AWS_SECRET_ACCESS_KEY", "SSH_AUTH_SOCK"] {
            assert!(!passed_env(k), "{k}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_line_without_end_is_cut_off_at_the_cap() {
        // after the first request the server writes 17 MiB with no newline and keeps the pipe open
        let script = "read l; yes x | tr -d '\\n' | head -c 17825792; sleep 30";
        let cfg =
            McpServerConfig { command: "sh".into(), args: vec!["-c".into(), script.into()], ..Default::default() };
        let e = match McpClient::connect("big", &cfg).await {
            Ok(_) => panic!("connected"),
            Err(e) => e.to_string(),
        };
        assert!(e.contains("over 16 MiB"), "{e}");
    }
}
