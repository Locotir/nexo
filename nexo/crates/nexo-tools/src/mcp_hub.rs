//! The MCP servers of a session as a whole: which are connected, which need a login or failed,
//! which the person turned off. It keeps their lists (tools, resources, prompts) current, builds the
//! tools the model gets (the servers' own, plus `list_mcp_resources` and `read_mcp_resource`), reads
//! `@server:uri` mentions, and runs the prompts.
//!
//! A server that changes a list tells the hub; the agent then rebuilds its tool set at the start of
//! the next turn ([`McpHub::sync_tools`]), so the tools never change in the middle of a turn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::agent::{McpArgInfo, McpPromptInfo, McpServerInfo, McpStatus};
use nexo_core::config::McpServerConfig;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet, ToolSpec};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::mcp::{parse_prompt_args, McpClient};

/// The most of a resource that is attached to a request.
const MENTION_BYTES: usize = 200_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

pub const LIST_TOOL: &str = "list_mcp_resources";
pub const READ_TOOL: &str = "read_mcp_resource";

/// Makes a turn-off last (or a turn-on): `(server, on)`. Returns what to tell the person.
pub type Persist = Arc<dyn Fn(&str, bool) -> Result<String, String> + Send + Sync>;

struct Entry {
    cfg: McpServerConfig,
    status: McpStatus,
    client: Option<Arc<McpClient>>,
    watcher: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(w) = self.watcher.take() {
            w.abort();
        }
    }
}

pub struct McpHub {
    entries: Mutex<BTreeMap<String, Entry>>,
    auth_file: PathBuf,
    /// Goes up whenever anything about a server changes.
    gen: watch::Sender<u64>,
    persist: Option<Persist>,
}

/// How a server's connection ended up.
async fn connect_one(name: &str, cfg: &McpServerConfig, auth: &Path) -> (McpStatus, Option<Arc<McpClient>>) {
    if cfg.enabled == Some(false) {
        return (McpStatus::Disabled, None);
    }
    match tokio::time::timeout(CONNECT_TIMEOUT, McpClient::connect_in(name, cfg, auth)).await {
        Ok(Ok(c)) => (McpStatus::Connected, Some(Arc::new(c))),
        Ok(Err(e)) if e.downcast_ref::<crate::mcp_http::NeedsLogin>().is_some() => (McpStatus::NeedsLogin, None),
        Ok(Err(e)) => {
            let text = nexo_core::redact::redact(&format!("{e:#}")).replace('\n', " ");
            (McpStatus::Failed(crate::util::cut_line(&text, 160)), None)
        }
        Err(_) => (McpStatus::Failed("timed out".into()), None),
    }
}

fn transport_of(cfg: &McpServerConfig) -> &'static str {
    match (&cfg.url, cfg.transport.as_deref()) {
        (None, _) => "stdio",
        (Some(_), Some("sse")) => "sse",
        _ => "http",
    }
}

impl McpHub {
    /// Connects every server of `cfgs` that is not turned off, all at once.
    pub async fn start(
        cfgs: &BTreeMap<String, McpServerConfig>,
        auth_file: &Path,
        persist: Option<Persist>,
    ) -> Arc<McpHub> {
        let mut tasks = Vec::new();
        for (name, cfg) in cfgs {
            let (n, c, a) = (name.clone(), cfg.clone(), auth_file.to_path_buf());
            tasks.push((name.clone(), cfg.clone(), tokio::spawn(async move { connect_one(&n, &c, &a).await })));
        }
        let hub = Arc::new(McpHub {
            entries: Mutex::new(BTreeMap::new()),
            auth_file: auth_file.to_path_buf(),
            gen: watch::channel(0).0,
            persist,
        });
        for (name, cfg, task) in tasks {
            let (status, client) = task.await.unwrap_or((McpStatus::Failed("start crashed".into()), None));
            hub.put(&name, cfg, status, client);
        }
        hub
    }

    /// Stores a server's new state and starts watching its lists.
    fn put(self: &Arc<Self>, name: &str, cfg: McpServerConfig, status: McpStatus, client: Option<Arc<McpClient>>) {
        let watcher = client.as_ref().map(|c| {
            let mut rx = c.changes();
            let hub = Arc::downgrade(self);
            tokio::spawn(async move {
                while rx.changed().await.is_ok() {
                    match hub.upgrade() {
                        Some(h) => h.bump(),
                        None => break,
                    }
                }
            })
        });
        if let Ok(mut e) = self.entries.lock() {
            e.insert(name.to_string(), Entry { cfg, status, client, watcher });
        }
        self.bump();
    }

    fn bump(&self) {
        self.gen.send_modify(|n| *n += 1);
    }

    /// A counter that goes up whenever a server's state or lists change.
    pub fn generation(&self) -> u64 {
        *self.gen.borrow()
    }

    pub fn watch(&self) -> watch::Receiver<u64> {
        self.gen.subscribe()
    }

    pub fn server_names(&self) -> Vec<String> {
        self.entries.lock().map(|e| e.keys().cloned().collect()).unwrap_or_default()
    }

    /// The servers that failed to start, with the reason (for a line at start-up).
    pub fn failures(&self) -> Vec<(String, String)> {
        let e = self.entries.lock();
        let Ok(e) = e else { return Vec::new() };
        e.iter()
            .filter_map(|(n, x)| match &x.status {
                McpStatus::Failed(why) => Some((n.clone(), why.clone())),
                McpStatus::NeedsLogin => Some((n.clone(), format!("needs login: run `nexo mcp login {n}`"))),
                _ => None,
            })
            .collect()
    }

    fn client(&self, name: &str) -> Result<Arc<McpClient>, String> {
        let e = self.entries.lock().map_err(|_| "mcp state is locked".to_string())?;
        match e.get(name) {
            None => Err(format!("no MCP server named `{name}`")),
            Some(x) => x.client.clone().ok_or_else(|| format!("MCP server `{name}` is not connected")),
        }
    }

    /// The state of every server, for the `/mcp` window and the completion.
    pub fn info(&self) -> Vec<McpServerInfo> {
        let Ok(e) = self.entries.lock() else { return Vec::new() };
        e.iter()
            .map(|(name, x)| {
                let c = x.client.as_ref();
                McpServerInfo {
                    name: name.clone(),
                    transport: transport_of(&x.cfg).into(),
                    status: x.status.clone(),
                    tools: c.map_or(0, |c| c.defs().len()),
                    resources: c.map(|c| c.resources().into_iter().map(|r| r.uri).collect()).unwrap_or_default(),
                    templates: c
                        .map(|c| c.templates().into_iter().map(|t| t.uri_template).collect())
                        .unwrap_or_default(),
                    prompts: c
                        .map(|c| {
                            c.prompts()
                                .into_iter()
                                .map(|p| McpPromptInfo {
                                    name: p.name,
                                    description: p.description,
                                    args: p
                                        .arguments
                                        .into_iter()
                                        .map(|a| McpArgInfo { name: a.name, required: a.required })
                                        .collect(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The tools of every connected server, and the two resource tools when a server has resources.
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        let mut resources = false;
        if let Ok(e) = self.entries.lock() {
            for x in e.values() {
                if let Some(c) = &x.client {
                    out.extend(c.tools());
                    resources |= c.capabilities().get("resources").is_some();
                }
            }
        }
        if resources {
            out.push(Arc::new(ResourceTool { hub: self.clone(), read: false }));
            out.push(Arc::new(ResourceTool { hub: self.clone(), read: true }));
        }
        out
    }

    /// Brings `set` up to date with the servers when anything changed since `seen` (the count last
    /// synced; updated). True when the set was rebuilt.
    pub fn sync_tools(self: &Arc<Self>, set: &mut ToolSet, seen: &mut u64) -> bool {
        let now = self.generation();
        if now == *seen {
            return false;
        }
        *seen = now;
        let old: Vec<String> = set
            .names()
            .filter(|n| n.starts_with("mcp__") || *n == LIST_TOOL || *n == READ_TOOL)
            .map(String::from)
            .collect();
        for n in old {
            set.remove(&n);
        }
        for t in self.tools() {
            set.add(t);
        }
        true
    }

    // ---- resources ----

    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<String, String> {
        self.client(server)?.read_resource(uri).await
    }

    /// The resources and templates of `server` (or of all), one per line.
    pub fn resources_text(&self, server: Option<&str>) -> String {
        let Ok(e) = self.entries.lock() else { return String::new() };
        let mut lines = Vec::new();
        for (name, x) in e.iter().filter(|(n, _)| server.is_none_or(|s| s == n.as_str())) {
            let Some(c) = &x.client else { continue };
            for r in c.resources() {
                let mut l = format!("{name}: {}", r.uri);
                for extra in [&r.name, &r.mime_type, &r.description] {
                    if !extra.is_empty() {
                        l.push_str(&format!("  | {extra}"));
                    }
                }
                lines.push(l);
            }
            for t in c.templates() {
                let mut l = format!("{name}: {}  | template", t.uri_template);
                for extra in [&t.name, &t.description] {
                    if !extra.is_empty() {
                        l.push_str(&format!("  | {extra}"));
                    }
                }
                lines.push(l);
            }
        }
        if lines.is_empty() {
            "no resources".into()
        } else {
            lines.join("\n")
        }
    }

    /// The `@server:uri` mentions in `text`, in order, each once.
    pub fn mentions(&self, text: &str) -> Vec<(String, String)> {
        find_mentions(text, &self.server_names())
    }

    /// `text` with every `@server:uri` mention followed by the resource it names, as fenced blocks
    /// (the way a file mention is attached). A resource that cannot be read says why in its place.
    pub async fn expand_mentions(&self, text: String) -> String {
        let found = self.mentions(&text);
        if found.is_empty() {
            return text;
        }
        let mut out = text;
        for (server, uri) in found {
            match self.read_resource(&server, &uri).await {
                Ok(body) if body.len() > MENTION_BYTES => out.push_str(&format!(
                    "\n\n@{server}:{uri}: not attached, it is {} bytes (ask for it with read_mcp_resource if needed)",
                    body.len()
                )),
                Ok(body) => {
                    let fence = if body.contains("```") { "````" } else { "```" };
                    out.push_str(&format!(
                        "\n\n[MCP resource {uri} from server {server}]\n{fence}\n{}\n{fence}",
                        body.trim_end_matches('\n')
                    ));
                }
                Err(e) => out.push_str(&format!("\n\n@{server}:{uri}: not attached ({e})")),
            }
        }
        out
    }

    // ---- prompts ----

    /// The request a prompt makes: `args` as typed after the command (positional or `key=value`).
    pub async fn prompt_request(&self, server: &str, prompt: &str, args: &str) -> Result<String, String> {
        let c = self.client(server)?;
        let def = c
            .prompts()
            .into_iter()
            .find(|p| p.name == prompt)
            .ok_or_else(|| format!("MCP server `{server}` has no prompt `{prompt}`"))?;
        let args = parse_prompt_args(&def, args)?;
        Ok(c.get_prompt(prompt, &args).await?.as_request())
    }

    // ---- the person's controls ----

    fn cfg(&self, name: &str) -> Result<(McpServerConfig, McpStatus), String> {
        let e = self.entries.lock().map_err(|_| "mcp state is locked".to_string())?;
        e.get(name).map(|x| (x.cfg.clone(), x.status.clone())).ok_or_else(|| format!("no MCP server named `{name}`"))
    }

    /// Closes a server's connection and leaves it with `status`.
    async fn stop(self: &Arc<Self>, name: &str, status: McpStatus) {
        let taken = {
            let Ok(mut e) = self.entries.lock() else { return };
            e.get_mut(name).map(|x| {
                x.status = status;
                (x.client.take(), x.watcher.take())
            })
        };
        if let Some((client, watcher)) = taken {
            if let Some(w) = watcher {
                w.abort();
            }
            if let Some(c) = client {
                c.close().await;
            }
        }
        self.bump();
    }

    /// Turns a server on or off for this session; `persist` also writes it to the configuration.
    pub async fn set_enabled(self: &Arc<Self>, name: &str, on: bool, persist: bool) -> Result<String, String> {
        let (mut cfg, _) = self.cfg(name)?;
        let mut text = if on {
            // this session's word beats the file's `enabled = false`
            cfg.enabled = None;
            self.stop(name, McpStatus::Disabled).await;
            let (status, client) = connect_one(name, &cfg, &self.auth_file).await;
            let shown = match &status {
                McpStatus::Connected => "connected".to_string(),
                McpStatus::NeedsLogin => "needs login".to_string(),
                McpStatus::Failed(w) => format!("failed: {w}"),
                McpStatus::Disabled => "off".to_string(),
            };
            self.put(name, cfg, status, client);
            format!("{name}: on, {shown}")
        } else {
            self.stop(name, McpStatus::Disabled).await;
            format!("{name}: off for this session")
        };
        if persist {
            match self.persist.as_ref().map(|p| p(name, on)) {
                Some(Ok(m)) => text.push_str(&format!("; {m}")),
                Some(Err(e)) => text.push_str(&format!("; not saved: {e}")),
                None => text.push_str("; not saved: no configuration file to write"),
            }
        }
        Ok(text)
    }

    /// Connects a server again (it must not be turned off).
    pub async fn reconnect(self: &Arc<Self>, name: &str) -> Result<String, String> {
        let (cfg, status) = self.cfg(name)?;
        if status == McpStatus::Disabled {
            return Err(format!("{name} is off; turn it on first"));
        }
        self.stop(name, status).await;
        let (status, client) = connect_one(name, &cfg, &self.auth_file).await;
        let text = match &status {
            McpStatus::Connected => format!("{name}: connected"),
            McpStatus::NeedsLogin => format!("{name}: needs login"),
            McpStatus::Failed(w) => format!("{name}: failed: {w}"),
            McpStatus::Disabled => format!("{name}: off"),
        };
        self.put(name, cfg, status, client);
        Ok(text)
    }

    /// Signs in to a remote server (`open` gets the address for the browser), then connects it.
    pub async fn login(self: &Arc<Self>, name: &str, open: &(dyn Fn(&str) + Sync)) -> Result<String, String> {
        let (cfg, status) = self.cfg(name)?;
        if cfg.url.is_none() {
            return Err(format!("{name} is a local command; only remote servers have a login"));
        }
        if status == McpStatus::Disabled {
            return Err(format!("{name} is off; turn it on first"));
        }
        let host = crate::mcp_oauth::login(name, &cfg, &self.auth_file, open).await.map_err(|e| format!("{e:#}"))?;
        let r = self.reconnect(name).await?;
        Ok(format!("signed in to {host}; {r}"))
    }

    /// Stops every server.
    pub async fn close(self: &Arc<Self>) {
        for n in self.server_names() {
            self.stop(&n, McpStatus::Disabled).await;
        }
    }
}

/// The `@server:uri` mentions of `text` for the given server names. A mention starts a word and
/// runs to the next white space; the punctuation that ends a sentence is not part of it.
pub fn find_mentions(text: &str, servers: &[String]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices('@') {
        if i > 0 && !(bytes[i - 1] as char).is_whitespace() && !matches!(bytes[i - 1], b'(' | b'[' | b'"' | b'\'') {
            continue;
        }
        let word: &str = text[i + 1..].split(char::is_whitespace).next().unwrap_or("");
        let Some((server, rest)) = word.split_once(':') else { continue };
        if !servers.iter().any(|s| s == server) {
            continue;
        }
        let uri = rest.trim_end_matches([',', '.', ';', '!', '?', ')', ']', '}', '"', '\'']);
        if uri.is_empty() {
            continue;
        }
        let m = (server.to_string(), uri.to_string());
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// `list_mcp_resources` and `read_mcp_resource`.
struct ResourceTool {
    hub: Arc<McpHub>,
    read: bool,
}

#[async_trait]
impl Tool for ResourceTool {
    fn name(&self) -> &str {
        if self.read {
            READ_TOOL
        } else {
            LIST_TOOL
        }
    }

    fn spec(&self) -> ToolSpec {
        if self.read {
            ToolSpec {
                name: READ_TOOL.into(),
                description:
                    "Read a resource of an MCP server by its URI (as listed by list_mcp_resources; a URI built \
                              from a listed template works too)."
                        .into(),
                parameters: json!({"type": "object", "properties": {
                    "server": {"type": "string", "description": "The server's name."},
                    "uri": {"type": "string", "description": "The resource's URI."}},
                    "required": ["server", "uri"]}),
            }
        } else {
            ToolSpec {
                name: LIST_TOOL.into(),
                description: "List the resources and resource templates the MCP servers offer (files, records, \
                              documents). Read one with read_mcp_resource."
                    .into(),
                parameters: json!({"type": "object", "properties": {
                    "server": {"type": "string", "description": "Only this server's (default: all)."}}}),
            }
        }
    }

    fn kind(&self) -> ToolKind {
        if self.read {
            ToolKind::Network
        } else {
            ToolKind::Read
        }
    }

    fn summary(&self, args: &Value) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        if self.read {
            format!("mcp read {}:{}", s("server"), s("uri"))
        } else {
            format!("mcp resources {}", s("server"))
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let s = |k: &str| args.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        if !self.read {
            return ToolOutput::ok(self.hub.resources_text(s("server")));
        }
        let (Some(server), Some(uri)) = (s("server"), s("uri")) else {
            return ToolOutput::error("read_mcp_resource needs `server` and `uri`");
        };
        let r = tokio::select! {
            _ = ctx.cancel.cancelled() => return ToolOutput::error("mcp: cancelled"),
            r = self.hub.read_resource(server, uri) => r,
        };
        match r {
            Ok(t) => ToolOutput::ok(t),
            Err(e) => ToolOutput::error(format!("mcp {server}:{uri}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_need_a_known_server_and_start_a_word() {
        let servers = vec!["docs".to_string(), "db".to_string()];
        let m = find_mentions(
            "see @docs:file:///a/b.md, and (@db:table/users). mail me@docs:x or @other:y and @docs:file:///a/b.md",
            &servers,
        );
        assert_eq!(
            m,
            vec![("docs".to_string(), "file:///a/b.md".to_string()), ("db".to_string(), "table/users".to_string())]
        );
        assert!(find_mentions("@docs: nothing", &servers).is_empty());
    }
}
