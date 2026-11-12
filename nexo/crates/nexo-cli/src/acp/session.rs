//! One ACP session: a nexo agent of its own, started in the session's directory, and the
//! translation of what it does into `session/update` notifications.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_core::agent::{channels, AgentCommand, AgentEvent, CommandTx, EventRx, Level, TurnEnd};
use nexo_core::config::McpServerConfig;
use nexo_core::{PermissionDecision, PermissionMode, PermissionRequest, Question, ToolSet};
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use super::convert;
use super::fs::{self, FsCaps};
use super::rpc::{Rpc, RpcError};
use super::terminal;
use crate::setup;

/// What the command line set for every session of this server.
#[derive(Clone, Default)]
pub struct Opts {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    pub context: Option<String>,
    pub curator_spec: Option<String>,
}

pub struct Sess {
    pub id: String,
    cwd: PathBuf,
    cmds: CommandTx,
    events: AsyncMutex<EventRx>,
    tools: ToolSet,
    /// The editor terminal each running tool call shows (empty without terminal support).
    shown: terminal::Shown,
    cancelled: AtomicBool,
    agent: Mutex<Option<JoinHandle<anyhow::Result<()>>>>,
}

/// The MCP servers of `session/new`, as nexo's MCP configuration: stdio ones (a `command`), and
/// remote ones of type `http` or `sse` (a `url` and `headers`). Any other kind is left out with a
/// line in the log.
fn mcp_servers(list: &[Value]) -> Vec<(String, McpServerConfig)> {
    let mut out = Vec::new();
    for s in list {
        let name = s.get("name").and_then(Value::as_str).unwrap_or("");
        let kind = s.get("type").and_then(Value::as_str).unwrap_or("stdio");
        if matches!(kind, "http" | "sse") {
            let Some(url) = s.get("url").and_then(Value::as_str).filter(|u| !u.is_empty()) else {
                eprintln!("nexo acp: MCP server {name:?} has no url; left out");
                continue;
            };
            let headers = s
                .get("headers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|h| {
                            Some((h.get("name")?.as_str()?.to_string(), h.get("value")?.as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let transport = Some(kind.to_string());
            out.push((
                name.to_string(),
                McpServerConfig { url: Some(url.to_string()), headers, transport, ..Default::default() },
            ));
            continue;
        }
        let Some(command) = s.get("command").and_then(Value::as_str) else {
            eprintln!("nexo acp: MCP server {name:?} is not a stdio, http or sse server; left out");
            continue;
        };
        let args = s
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        let env = s
            .get("env")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get("name")?.as_str()?.to_string(), e.get("value")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.push((name.to_string(), McpServerConfig { command: command.to_string(), args, env, ..Default::default() }));
    }
    out
}

/// Starts the agent for `cwd` (resuming `resume`). Returns the session, the events the start
/// produced before the agent was ready for a request (a resumed conversation's replay), and the mode in force.
pub async fn start(
    rpc: Arc<Rpc>,
    opts: &Opts,
    caps: FsCaps,
    cwd: PathBuf,
    mcp: &[Value],
    resume: Option<String>,
) -> Result<(Arc<Sess>, Vec<AgentEvent>, PermissionMode), RpcError> {
    let mut config = nexo_core::Config::load(&cwd).map_err(|e| RpcError::internal(e.to_string()))?;
    setup::apply_presets(&mut config);
    for p in [opts.provider.clone(), config.provider.clone()].into_iter().flatten() {
        crate::account::adopt(&mut config, &p);
    }
    config.name_auth_entries();
    if let Some(m) = opts.permission_mode {
        config.permissions.mode = m;
    }
    if let Some(c) = &opts.context {
        config.context.policy = c.clone();
    }
    if let Some(s) = &opts.curator_spec {
        config.context.curator_spec = s.clone();
    }
    for (name, server) in mcp_servers(mcp) {
        config.mcp.insert(name, server);
    }
    if opts.provider.is_none() && setup::nothing_chosen(&config) {
        return Err(RpcError::auth_required(setup::nothing_chosen_message()));
    }
    let picked = setup::pick(&config, opts.provider.as_deref(), opts.model.as_deref())
        .map_err(|e| RpcError::auth_required(format!("{e:#}")))?;
    let mut agent_setup = setup::agent_setup(&config, &cwd, &picked, resume, None, None)
        .await
        .map_err(|e| RpcError::internal(format!("{e:#}")))?;
    let sid_cell = fs::install(&mut agent_setup.tools, rpc.clone(), caps);
    let shown = if caps.terminal {
        terminal::install(&mut agent_setup.tools, rpc, sid_cell.clone())
    } else {
        Default::default()
    };
    let tools = agent_setup.tools.clone();
    let (handle, ends) = channels();
    let join = tokio::spawn(nexo_agent::run(ends, agent_setup));
    let (cmds, mut events) = (handle.commands, handle.events);

    // everything up to the model limits is start-up: `Ready`, then the replay of a resumed session
    let (mut early, mut id, mut mode) = (Vec::new(), None, config.permissions.mode);
    loop {
        match events.recv().await {
            Some(AgentEvent::ModelLimits { .. }) => break,
            Some(ev) => {
                if let AgentEvent::Ready { session_id, permission_mode, .. } = &ev {
                    id = Some(session_id.clone());
                    mode = *permission_mode;
                }
                early.push(ev);
            }
            None => {
                let why = match join.await {
                    Ok(Err(e)) => format!("{e:#}"),
                    Ok(Ok(())) => "the agent stopped while starting".to_string(),
                    Err(e) => format!("the agent stopped: {e}"),
                };
                return Err(RpcError::internal(why));
            }
        }
    }
    let id = id.ok_or_else(|| RpcError::internal("the agent started without a session"))?;
    let _ = sid_cell.set(id.clone());
    let sess = Sess {
        id,
        cwd,
        cmds,
        events: AsyncMutex::new(events),
        tools,
        shown,
        cancelled: AtomicBool::new(false),
        agent: Mutex::new(Some(join)),
    };
    Ok((Arc::new(sess), early, mode))
}

impl Sess {
    /// The updates that show a resumed conversation to the editor.
    pub fn replay(&self, early: &[AgentEvent]) -> Vec<Value> {
        convert::replay(early, &self.tools, &self.cwd)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.cmds.send(AgentCommand::Interrupt);
    }

    pub fn set_mode(&self, rpc: &Rpc, mode: PermissionMode) {
        let _ = self.cmds.send(AgentCommand::SetPermissionMode(mode));
        self.update(rpc, convert::mode_update(mode));
    }

    pub fn update(&self, rpc: &Rpc, body: Value) {
        rpc.notify("session/update", json!({"sessionId": self.id, "update": body}));
    }

    /// Stops the agent; waits a moment for it to save the session.
    pub async fn shutdown(&self) {
        let _ = self.cmds.send(AgentCommand::Shutdown);
        let join = self.agent.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(j) = join {
            let _ = tokio::time::timeout(Duration::from_secs(2), j).await;
        }
    }

    /// Runs one prompt turn: streams the updates, puts the agent's questions to the client,
    /// and returns the response of `session/prompt`.
    pub async fn prompt(&self, rpc: &Arc<Rpc>, blocks: &[Value]) -> Result<Value, RpcError> {
        let text = convert::prompt_text(blocks);
        if text.trim().is_empty() {
            return Err(RpcError::invalid_params("the prompt has no text"));
        }
        // one turn at a time per session; a second prompt waits for the first
        let mut events = self.events.lock().await;
        // what was left over from the last turn (an Idle, a job notice) is not this turn's
        while events.try_recv().is_ok() {}
        self.cancelled.store(false, Ordering::SeqCst);
        self.cmds
            .send(AgentCommand::Submit { text, attachments: vec![] })
            .map_err(|_| RpcError::internal("the agent stopped"))?;

        let mut calls: HashMap<String, (Option<PathBuf>, Value)> = HashMap::new();
        let (mut mid_line, mut last_error, mut ended) = (false, None::<String>, None::<TurnEnd>);
        // reasoning streamed with no text or call after it yet
        let mut mid_thought = false;
        loop {
            let Some(ev) = events.recv().await else {
                return Err(RpcError::internal(last_error.unwrap_or_else(|| "the agent stopped".into())));
            };
            match ev {
                AgentEvent::AssistantStart if mid_line => {
                    self.update(rpc, convert::chunk("agent_message_chunk", "\n\n"));
                    mid_line = false;
                }
                // a reply that starts while its reasoning is still open: the request was sent again (the stream
                // was cut inside the reasoning), so the dropped thought is not joined to the new one
                AgentEvent::AssistantStart if mid_thought => {
                    self.update(rpc, convert::chunk("agent_thought_chunk", "\n\n"));
                    mid_thought = false;
                }
                AgentEvent::TextDelta { text } => {
                    mid_line = true;
                    mid_thought = false;
                    self.update(rpc, convert::chunk("agent_message_chunk", &text));
                }
                AgentEvent::ReasoningDelta { text } => {
                    mid_thought = true;
                    self.update(rpc, convert::chunk("agent_thought_chunk", &text));
                }
                AgentEvent::ToolCall { id, name, args, summary } => {
                    mid_thought = false;
                    calls.insert(id.clone(), (convert::call_path(&self.cwd, &args), args.clone()));
                    self.update(rpc, convert::tool_call(&self.tools, &self.cwd, &id, &name, &args, &summary));
                }
                AgentEvent::ToolStart { id } => self.update(rpc, convert::tool_status(&id, "in_progress")),
                AgentEvent::ToolDone { id, output, .. } => {
                    let path = calls.get(&id).and_then(|(p, _)| p.as_deref());
                    let term = self.shown.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    self.update(rpc, convert::tool_done(&id, &output, path, term.as_deref()));
                    if let Some(p) = convert::plan(&output) {
                        self.update(rpc, p);
                    }
                }
                AgentEvent::PermissionAsk { request } => {
                    let args = calls.get(&request.id).map(|(_, a)| a.clone());
                    self.spawn_permission(rpc, request, args);
                }
                AgentEvent::Question { question } => self.spawn_question(rpc, question),
                AgentEvent::PermissionModeChanged { mode } => self.update(rpc, convert::mode_update(mode)),
                AgentEvent::Notice { level, text, .. } => {
                    eprintln!("nexo acp: [{level:?}] {text}");
                    if level == Level::Error {
                        last_error = Some(text);
                    }
                }
                AgentEvent::TurnEnd { reason } => {
                    ended = Some(reason);
                    // a turn that did not finish is not followed by another one
                    if reason != TurnEnd::Done {
                        break;
                    }
                }
                AgentEvent::Idle { .. } if ended.is_some() => break,
                AgentEvent::Shutdown => {
                    return Err(RpcError::internal("the agent shut down"));
                }
                _ => {}
            }
        }
        let stop = match ended {
            _ if self.cancelled.load(Ordering::SeqCst) => "cancelled",
            Some(TurnEnd::Done) | None => "end_turn",
            Some(TurnEnd::Interrupted) => "cancelled",
            Some(TurnEnd::MaxTurns) => "max_turn_requests",
            Some(TurnEnd::Error) => {
                return Err(RpcError::internal(last_error.unwrap_or_else(|| "the turn failed".into())));
            }
        };
        Ok(json!({"stopReason": stop}))
    }

    /// Asks the client about a call; the answer goes back to the agent as a command.
    fn spawn_permission(&self, rpc: &Arc<Rpc>, req: PermissionRequest, args: Option<Value>) {
        let params = json!({
            "sessionId": self.id,
            "toolCall": convert::permission_tool_call(&self.tools, &self.cwd, &req, args.as_ref()),
            "options": convert::permission_options(&req),
        });
        let (rpc, cmds) = (rpc.clone(), self.cmds.clone());
        tokio::spawn(async move {
            let decision = match rpc.request("session/request_permission", params).await {
                Ok(v) => convert::decision(&v, &req),
                Err(e) => {
                    PermissionDecision::Deny { reason: Some(format!("the editor did not answer: {}", e.message)) }
                }
            };
            let _ = cmds.send(AgentCommand::Permission { id: req.id, decision });
        });
    }

    /// Puts a question of the agent (a plan to approve, a choice) to the client.
    fn spawn_question(&self, rpc: &Arc<Rpc>, q: Question) {
        let (rpc, cmds, sid) = (rpc.clone(), self.cmds.clone(), self.id.clone());
        tokio::spawn(async move {
            let answer = if q.options.is_empty() {
                None
            } else {
                let mut params = convert::question_request(&q);
                params["sessionId"] = json!(sid);
                match rpc.request("session/request_permission", params).await {
                    Ok(v) => convert::answer(&v, &q),
                    Err(_) => None,
                }
            };
            let _ = cmds.send(AgentCommand::Answer { id: q.id, answer });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_sse_and_stdio_servers_become_nexo_configuration() {
        let list = vec![
            json!({"name": "web", "type": "http", "url": "https://x.test/mcp", "headers": [{"name": "Authorization", "value": "Bearer t"}]}),
            json!({"name": "old", "type": "sse", "url": "https://x.test/sse", "headers": []}),
            json!({"name": "local", "command": "srv", "args": ["-x"], "env": [{"name": "K", "value": "v"}]}),
            json!({"name": "nourl", "type": "http"}),
            json!({"name": "weird", "type": "carrier-pigeon"}),
        ];
        let s = mcp_servers(&list);
        let names: Vec<&str> = s.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["web", "old", "local"]);
        assert_eq!(s[0].1.url.as_deref(), Some("https://x.test/mcp"));
        assert_eq!(s[0].1.transport.as_deref(), Some("http"));
        assert_eq!(s[0].1.headers["Authorization"], "Bearer t");
        assert_eq!(s[1].1.transport.as_deref(), Some("sse"));
        assert_eq!((s[2].1.command.as_str(), s[2].1.args.as_slice()), ("srv", ["-x".to_string()].as_slice()));
        assert_eq!(s[2].1.env["K"], "v");
    }
}
