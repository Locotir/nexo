//! The `bash` tool, run in the editor's terminal.
//!
//! When the client says it has `terminal`, a foreground command goes through `terminal/create`
//! and is shown live in the tool call (`content: [{type: "terminal", terminalId}]`); nexo waits
//! for it with `terminal/wait_for_exit`, reads `terminal/output`, and always ends with
//! `terminal/release`. A cancelled turn or a timeout kills the command first. Background jobs
//! stay nexo's own process (the `jobs` tool needs the output in nexo), and so does everything
//! when the client has no terminal or the OS sandbox is on (the editor's terminal is not
//! sandboxed, and a command the sandbox holds in is allowed without asking).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSet, ToolSpec};
use nexo_tools::bash::{Bash, DEFAULT_TIMEOUT_S, MAX_OUTPUT_CHARS, MAX_TIMEOUT_S};
use nexo_tools::util::{bool_arg, cap_text, int_arg, str_arg};
use serde_json::{json, Value};

use super::rpc::Rpc;

/// The terminal each running tool call shows, by tool call id; the session reads it when the call
/// ends, so the finished call keeps the terminal in its content.
pub type Shown = Arc<Mutex<HashMap<String, String>>>;

/// One terminal of the editor. If it is dropped before `release` (the call was abandoned), the
/// command is killed and the terminal released in the background.
struct Term {
    rpc: Arc<Rpc>,
    sid: String,
    id: String,
    released: bool,
}

impl Term {
    async fn call(&self, method: &str) -> Result<Value, String> {
        self.rpc.request(method, json!({"sessionId": self.sid, "terminalId": self.id})).await.map_err(|e| e.message)
    }

    async fn release(mut self) {
        self.released = true;
        let _ = self.call("terminal/release").await;
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let (rpc, params) = (self.rpc.clone(), json!({"sessionId": self.sid, "terminalId": self.id}));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = rpc.request("terminal/kill", params.clone()).await;
                let _ = rpc.request("terminal/release", params).await;
            });
        }
    }
}

struct TerminalBash {
    rpc: Arc<Rpc>,
    sid: Arc<OnceLock<String>>,
    shown: Shown,
}

/// Puts the terminal-backed `bash` in `tools` in place of the built-in one.
pub fn install(tools: &mut ToolSet, rpc: Arc<Rpc>, sid: Arc<OnceLock<String>>) -> Shown {
    let shown = Shown::default();
    tools.add(Arc::new(TerminalBash { rpc, sid, shown: shown.clone() }));
    shown
}

enum End {
    Exited(Option<i64>),
    TimedOut,
    Cancelled,
    Failed(String),
}

#[async_trait]
impl Tool for TerminalBash {
    fn name(&self) -> &str {
        "bash"
    }
    fn spec(&self) -> ToolSpec {
        Bash.spec()
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn summary(&self, args: &Value) -> String {
        Bash.summary(args)
    }

    /// As the built-in tool: a command the OS sandbox holds in is allowed without asking. That is
    /// sound because such a command always runs here, in the sandbox (see `run`).
    fn sandbox_holds(&self, args: &Value, sandbox: &nexo_core::sandbox::SandboxSettings) -> bool {
        Bash.sandbox_holds(args, sandbox)
    }
    fn sandbox_status(&self, sandbox: &nexo_core::sandbox::SandboxSettings) -> Option<(String, bool)> {
        Bash.sandbox_status(sandbox)
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        // the editor's terminal knows no sandbox: with one on, nexo runs the command itself, inside it
        if ctx.sandbox.mode.is_on() {
            return Bash.run(args, ctx).await;
        }
        // a job outlives the call and its output is read by the `jobs` tool: nexo runs it
        if bool_arg(&args, "background") {
            return Bash.run(args, ctx).await;
        }
        let Some(command) = str_arg(&args, "command") else {
            return ToolOutput::error("bash: `command` is required");
        };
        let timeout =
            Duration::from_secs(int_arg(&args, "timeout").unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, MAX_TIMEOUT_S));
        let sid = self.sid.get().cloned().unwrap_or_default();
        let shell = nexo_core::shell::shell();
        let created = self
            .rpc
            .request(
                "terminal/create",
                json!({
                    "sessionId": sid,
                    "command": shell.program.display().to_string(),
                    "args": shell.args_for(command),
                    "cwd": ctx.cwd.display().to_string(),
                    "outputByteLimit": MAX_OUTPUT_CHARS * 4,
                }),
            )
            .await;
        let id = match created.as_ref().map(|r| r.get("terminalId").and_then(Value::as_str)) {
            Ok(Some(id)) => id.to_string(),
            Ok(None) => return ToolOutput::error("bash: the editor's terminal/create gave no terminalId"),
            Err(e) => return ToolOutput::error(format!("bash: the editor could not start the command: {}", e.message)),
        };
        let term = Term { rpc: self.rpc.clone(), sid: sid.clone(), id: id.clone(), released: false };
        self.shown.lock().unwrap_or_else(|e| e.into_inner()).insert(ctx.call_id.clone(), id.clone());
        // the editor shows the terminal live from here on
        self.rpc.notify(
            "session/update",
            json!({"sessionId": sid, "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": ctx.call_id,
                "status": "in_progress",
                "content": [{"type": "terminal", "terminalId": id}],
            }}),
        );

        let end = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => End::Cancelled,
            _ = tokio::time::sleep(timeout) => End::TimedOut,
            r = term.call("terminal/wait_for_exit") => match r {
                Ok(v) => End::Exited(v.get("exitCode").and_then(Value::as_i64)),
                Err(e) => End::Failed(e),
            },
        };
        if matches!(end, End::TimedOut | End::Cancelled) {
            let _ = term.call("terminal/kill").await;
        }
        let output = match term.call("terminal/output").await {
            Ok(v) => v.get("output").and_then(Value::as_str).unwrap_or_default().to_string(),
            Err(_) => String::new(),
        };
        term.release().await;

        let mut content =
            if output.trim().is_empty() { "(no output)".to_string() } else { cap_text(&output, MAX_OUTPUT_CHARS) };
        let (is_error, exit_code) = match end {
            End::Exited(Some(0)) => (false, Some(0)),
            End::Exited(Some(code)) => {
                content.push_str(&format!("\n\n[exit code {code}]"));
                (true, i32::try_from(code).ok())
            }
            End::Exited(None) => {
                content.push_str("\n\n[killed by a signal]");
                (true, None)
            }
            End::TimedOut => {
                content.push_str(&format!("\n\n[killed: timed out after {} s]", timeout.as_secs()));
                (true, None)
            }
            End::Cancelled => {
                content.push_str("\n\n[cancelled]");
                (true, None)
            }
            End::Failed(e) => {
                content.push_str(&format!("\n\n[the editor's terminal failed: {e}]"));
                (true, None)
            }
        };
        let display =
            ToolDisplay::Shell { command: command.to_string(), stdout: output, stderr: String::new(), exit_code };
        ToolOutput { content, is_error, display }
    }
}
