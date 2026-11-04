//! A client for MCP servers over stdio (JSON-RPC 2.0, one JSON object per
//! line). The server's tools become [`Tool`]s named `mcp__<server>__<name>`.

use std::collections::HashMap;
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
    child: Child,
    tools: Vec<ToolDef>,
    /// What the server said it is.
    pub server_info: Value,
    pub protocol_version: String,
}

struct Inner {
    name: String,
    /// `trust_read_only` of the server's config: whether its `readOnlyHint` is believed.
    trust_read_only: bool,
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

impl McpClient {
    /// Starts the server and performs the handshake and the tool listing.
    pub async fn connect(name: &str, cfg: &McpServerConfig) -> anyhow::Result<McpClient> {
        if cfg.command.trim().is_empty() {
            return Err(anyhow!("mcp server `{name}`: no command"));
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
        let inner = Arc::new(Inner {
            name: name.to_string(),
            trust_read_only: cfg.trust_read_only,
            stdin: AsyncMutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
        });
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
            // the server went away: fail what is still waiting
            let pending: Vec<_> = reader.pending.lock().map(|mut p| p.drain().collect()).unwrap_or_default();
            for (_, tx) in pending {
                let _ = tx.send(Err(why.clone()));
            }
        });
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
                Ok(Err(e)) => return Err(anyhow!("mcp server `{name}`: initialize failed: {e}")),
                Err(_) => return Err(anyhow!("mcp server `{name}`: initialize timed out")),
            }
        }
        let init = init?;
        inner.notify("notifications/initialized", json!({})).await?;
        let protocol_version = init.get("protocolVersion").and_then(Value::as_str).unwrap_or("").to_string();
        let server_info = init.get("serverInfo").cloned().unwrap_or(Value::Null);
        let tools = list_tools(&inner).await?;
        Ok(McpClient { name: name.to_string(), inner, child, tools, server_info, protocol_version })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn defs(&self) -> &[ToolDef] {
        &self.tools
    }

    /// The server's tools as nexo tools (they keep the connection alive through a shared handle).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .iter()
            .map(|d| {
                Arc::new(McpTool {
                    server: self.name.clone(),
                    full_name: full_name(&self.name, &d.name),
                    def: d.clone(),
                    inner: self.inner.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    /// Lists the tools again (a server may add some after a while).
    pub async fn refresh(&mut self) -> anyhow::Result<usize> {
        self.tools = list_tools(&self.inner).await?;
        Ok(self.tools.len())
    }

    /// Stops the server.
    pub async fn close(mut self) {
        let _ = self.child.kill().await;
    }
}

/// Reads up to a newline into `buf`, stopping once it holds more than [`MAX_LINE`] bytes.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    use tokio::io::AsyncReadExt;
    (&mut *r).take(MAX_LINE as u64 + 1).read_until(b'\n', buf).await
}

async fn list_tools(inner: &Arc<Inner>) -> anyhow::Result<Vec<ToolDef>> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let r = tokio::time::timeout(INIT_TIMEOUT, inner.request("tools/list", params))
            .await
            .map_err(|_| anyhow!("mcp server `{}`: tools/list timed out", inner.name))?
            .map_err(|e| anyhow!("mcp server `{}`: tools/list failed: {e}", inner.name))?;
        for t in r.get("tools").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
            out.push(ToolDef {
                name: name.to_string(),
                description: t.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                read_only: t.pointer("/annotations/readOnlyHint").and_then(Value::as_bool).unwrap_or(false),
            });
        }
        cursor = r.get("nextCursor").and_then(Value::as_str).map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

impl Inner {
    async fn send(&self, msg: Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params})).await
    }

    /// A request; the error is the server's message or the transport's.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        if let Err(e) = self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err(format!("cannot write to the server: {e}"));
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Err("the server closed the connection".into()),
        }
    }

    async fn dispatch(&self, msg: Value) {
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
        }
        // notifications are ignored
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
                let text = text.trim_end().to_string();
                let text = if text.is_empty() { "(no content)".to_string() } else { text };
                ToolOutput { content: text, is_error, display: nexo_core::ToolDisplay::Plain }
            }
        }
    }
}

/// `mcp__<server>__<tool>`.
pub fn full_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
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
