//! `nexo acp`: an Agent Client Protocol server on stdin and stdout, so an editor (Zed,
//! JetBrains, Neovim) can use nexo as its coding agent. See docs/ACP.md.
//!
//! The protocol is JSON-RPC 2.0, one message a line. Nothing but protocol JSON is ever
//! written to stdout; diagnostics go to stderr (and to `--log`).
//!
//! Each ACP session is a nexo agent of its own: the same loop, curator and tools as the
//! terminal, driven through the agent's command and event channels.

mod convert;
mod fs;
mod rpc;
mod session;
mod terminal;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use fs::FsCaps;
use rpc::{Incoming, Rpc, RpcError};
use session::{Opts, Sess};

/// The protocol version spoken (ACP numbers only breaking changes).
const PROTOCOL_VERSION: u64 = 1;

pub async fn run(args: &crate::cli::Args) -> anyhow::Result<i32> {
    let permission_mode = match (&args.permission_mode, args.yolo) {
        (_, true) => Some(nexo_core::PermissionMode::Auto),
        (Some(m), false) => {
            Some(nexo_core::PermissionMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown permission mode {m:?}"))?)
        }
        (None, false) => None,
    };
    let opts = Opts {
        provider: args.provider.clone(),
        model: args.model.clone(),
        permission_mode,
        context: args.context.clone(),
        curator_spec: args.curator_spec.clone(),
    };
    serve(tokio::io::stdin(), tokio::io::stdout(), opts).await
}

/// Serves one client on `input` and `output` until it closes the input.
async fn serve<R, W>(input: R, output: W, opts: Opts) -> anyhow::Result<i32>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let rpc = Rpc::start(output);
    let server =
        Arc::new(Server { rpc: rpc.clone(), opts, caps: Mutex::new(FsCaps::default()), sessions: Mutex::default() });
    let mut lines = BufReader::new(input).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                rpc.respond(Value::Null, Err(RpcError::new(-32700, format!("parse error: {e}"))));
                continue;
            }
        };
        match Incoming::parse(value) {
            Incoming::Request { id, method, params } => {
                // each request on its own task: a prompt runs for minutes and `session/cancel` must get through
                let server = server.clone();
                tokio::spawn(async move {
                    let result = server.handle(&method, params).await;
                    server.rpc.respond(id, result);
                });
            }
            Incoming::Notification { method, params } => server.notification(&method, &params),
            Incoming::Response { id, result } => rpc.resolve(&id, result),
            Incoming::Invalid { id } => rpc.respond(id, Err(RpcError::new(-32600, "invalid request"))),
        }
    }
    // the client closed the input: finish what is open and save the sessions
    rpc.fail_all("the client closed the connection");
    server.shutdown().await;
    rpc.flush().await;
    Ok(0)
}

struct Server {
    rpc: Arc<Rpc>,
    opts: Opts,
    caps: Mutex<FsCaps>,
    sessions: Mutex<HashMap<String, Arc<Sess>>>,
}

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params.get(key).and_then(Value::as_str).ok_or_else(|| RpcError::invalid_params(format!("`{key}` is required")))
}

impl Server {
    fn session(&self, params: &Value) -> Result<Arc<Sess>, RpcError> {
        let id = str_param(params, "sessionId")?;
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params(format!("unknown session {id}")))
    }

    fn notification(&self, method: &str, params: &Value) {
        match method {
            "session/cancel" => match self.session(params) {
                Ok(s) => s.cancel(),
                Err(e) => eprintln!("nexo acp: cancel: {}", e.message),
            },
            // `$/cancel_request` and the like: nothing to do
            other => tracing::debug!("ignored notification {other}"),
        }
    }

    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(&params)),
            // no sign-in method is offered: `nexo auth login` does it in a terminal
            "authenticate" => Ok(json!({})),
            "session/new" => self.new_session(&params, None).await,
            "session/load" => {
                let id = str_param(&params, "sessionId")?.to_string();
                self.new_session(&params, Some(id)).await
            }
            "session/prompt" => {
                let s = self.session(&params)?;
                let blocks = params.get("prompt").and_then(Value::as_array).cloned().unwrap_or_default();
                s.prompt(&self.rpc, &blocks).await
            }
            "session/set_mode" => {
                let s = self.session(&params)?;
                let id = str_param(&params, "modeId")?;
                let mode =
                    convert::parse_mode(id).ok_or_else(|| RpcError::invalid_params(format!("unknown mode {id}")))?;
                s.set_mode(&self.rpc, mode);
                Ok(json!({}))
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let fs = &params["clientCapabilities"]["fs"];
        *self.caps.lock().unwrap_or_else(|e| e.into_inner()) = FsCaps {
            read: fs["readTextFile"].as_bool().unwrap_or(false),
            write: fs["writeTextFile"].as_bool().unwrap_or(false),
            terminal: params["clientCapabilities"]["terminal"].as_bool().unwrap_or(false),
        };
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "agentCapabilities": {
                "loadSession": true,
                "promptCapabilities": {"image": false, "audio": false, "embeddedContext": true},
                "mcpCapabilities": {"http": true, "sse": true},
            },
            "agentInfo": {"name": "nexo", "title": "nexo", "version": nexo_core::version::get()},
            "authMethods": [],
        })
    }

    /// `session/new`, or `session/load` when `resume` names a recorded session.
    async fn new_session(&self, params: &Value, resume: Option<String>) -> Result<Value, RpcError> {
        let cwd = PathBuf::from(str_param(params, "cwd")?);
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(RpcError::invalid_params(format!(
                "`cwd` must be an existing absolute directory: {}",
                cwd.display()
            )));
        }
        let mcp = params.get("mcpServers").and_then(Value::as_array).cloned().unwrap_or_default();
        let caps = *self.caps.lock().unwrap_or_else(|e| e.into_inner());
        let (sess, early, mode) = session::start(self.rpc.clone(), &self.opts, caps, cwd, &mcp, resume.clone()).await?;
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(sess.id.clone(), sess.clone());
        let modes = convert::modes_state(mode);
        if resume.is_some() {
            // the conversation goes out before the response, as the protocol asks
            for body in sess.replay(&early) {
                sess.update(&self.rpc, body);
            }
            Ok(json!({"modes": modes}))
        } else {
            Ok(json!({"sessionId": sess.id, "modes": modes}))
        }
    }

    async fn shutdown(&self) {
        let all: Vec<Arc<Sess>> =
            self.sessions.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, s)| s).collect();
        for s in all {
            s.shutdown().await;
        }
    }
}
