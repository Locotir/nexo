//! The loop against a scripted model and fake tools.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{
    channels, AddedKind, AgentCommand, AgentEvent, AgentHandle, CommandTx, GoalState, Level, TurnEnd,
};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::message::{Message, Part, Role};
use nexo_core::permission::{PermissionDecision, PermissionMode};
use nexo_core::pricing::Pricing;
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

enum Reply {
    Events(Vec<StreamEvent>),
    Err(ProviderError),
    /// Yields the events, then never ends (until dropped).
    Hang(Vec<StreamEvent>),
    /// Sends the command as the request goes out, then yields the events slowly (a command that arrives mid-request).
    Midway(CommandTx, Box<AgentCommand>, Vec<StreamEvent>),
}

struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<ModelRequest>>,
    /// The provider each request went to, by its address.
    bases: Mutex<Vec<String>>,
    /// Each wait the loop asked for before sending a request again itself, by its number.
    waits: Mutex<Vec<u32>>,
}

impl Scripted {
    fn new(replies: Vec<Reply>) -> Arc<Scripted> {
        Arc::new(Scripted {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            bases: Mutex::new(Vec::new()),
            waits: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(
        &self,
        cfg: &ProviderConfig,
        req: &ModelRequest,
        _retries: u32,
    ) -> Result<EventStream, ProviderError> {
        self.bases.lock().unwrap().push(cfg.base_url.clone());
        self.requests.lock().unwrap().push(req.clone());
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or(Reply::Events(text("(no script)")));
        match reply {
            Reply::Events(evs) => Ok(Box::pin(futures::stream::iter(evs))),
            Reply::Err(e) => Err(e),
            Reply::Hang(evs) => Ok(Box::pin(futures::stream::iter(evs).chain(futures::stream::pending()))),
            Reply::Midway(tx, c, evs) => {
                tx.send(*c).unwrap();
                Ok(Box::pin(futures::stream::iter(evs).then(|e| async move {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    e
                })))
            }
        }
    }

    fn backoff(&self, n: u32, _e: &ProviderError) -> Duration {
        self.waits.lock().unwrap().push(n);
        Duration::ZERO
    }
}

fn text(s: &str) -> Vec<StreamEvent> {
    let (a, b) = s.split_at(s.len() / 2);
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta(a.into()),
        StreamEvent::TextDelta(b.into()),
        StreamEvent::Usage(Usage { input: 1000, output: 100, cache_read: 2000, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ]
}

fn call(id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: id.into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args },
        StreamEvent::Usage(Usage { input: 10, output: 5, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

struct Fake {
    name: &'static str,
    kind: ToolKind,
    slow: bool,
}

#[async_trait]
impl Tool for Fake {
    fn name(&self) -> &str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name.into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        self.kind
    }
    fn summary(&self, args: &Value) -> String {
        format!("{} {}", self.name, args.get("path").or(args.get("command")).and_then(Value::as_str).unwrap_or(""))
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        if self.slow {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                _ = ctx.cancel.cancelled() => return ToolOutput::error("cancelled"),
            }
        }
        ctx.report("progress line");
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        if path.starts_with("missing") {
            return ToolOutput::error(format!("{path}: No such file or directory (os error 2)"));
        }
        ToolOutput::ok(format!(
            "{}:{}",
            self.name,
            args.get("path").or(args.get("command")).and_then(Value::as_str).unwrap_or("")
        ))
    }
}

fn tools() -> ToolSet {
    ToolSet::new()
        .with(Arc::new(Fake { name: "read", kind: ToolKind::Read, slow: false }))
        .with(Arc::new(Fake { name: "grep", kind: ToolKind::Search, slow: false }))
        .with(Arc::new(Fake { name: "write", kind: ToolKind::Write, slow: false }))
        .with(Arc::new(Fake { name: "bash", kind: ToolKind::Exec, slow: true }))
}

#[path = "../scratch/mod.rs"]
mod scratch;

mod attached_files;
mod background_jobs;
mod checkpoints;
mod compaction;
mod cut_replies;
mod goals_and_todo;
mod hooks;
mod loop_guards;
mod mail;
mod model_setup;
mod nexo_bin_hint;
mod permissions;
mod provider_limits;
mod reasoning_replies;
mod recall;
mod repeat_watch;
mod request_prefix;
mod resume_and_rewind;
mod session_commands;
mod session_models;
mod sub_agents;
mod tool_rounds;

fn data_dir() -> PathBuf {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let d = DIR.get_or_init(|| {
        // the scratch folder a bash call without a session gets lives until the process exits too
        scratch::at_exit(|| nexo_tools::bash::wipe_tmp(""));
        scratch::until_exit(test_tmpdir().unwrap())
    });
    std::env::set_var("NEXO_DATA_DIR", d);
    std::env::set_var("NEXO_CONFIG_DIR", d);
    std::env::set_var("NEXO_CACHE_DIR", d.join("cache"));
    d.clone()
}

/// Tests that write the user's configuration file take turns.
fn config_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn user_config() -> Config {
    Config::parse(&std::fs::read_to_string(data_dir().join("config.toml")).unwrap_or_default()).unwrap()
}

fn config(mode: PermissionMode) -> Config {
    let mut cfg = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into(), "m2".into()];
    p.pricing.insert("m1".into(), Pricing { input: 1.0, output: 2.0, cache_read: 0.1, cache_write: 0.0 });
    cfg.providers.insert("mock".into(), p);
    cfg.permissions.mode = mode;
    // these tests script every call: no curator between them
    cfg.context.policy = "plain".into();
    // and every file they read is read on purpose: nothing comes attached on its own, nothing runs at the start
    cfg.agent.attach_budget = 0;
    cfg.agent.check_at_start = false;
    cfg
}

struct Running {
    handle: AgentHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(client: Arc<Scripted>, cfg: Config, resume: Option<String>, cwd: &std::path::Path) -> Running {
    start_with(client, cfg, resume, cwd, tools()).await
}

async fn start_with(
    client: Arc<Scripted>,
    cfg: Config,
    resume: Option<String>,
    cwd: &std::path::Path,
    tools: ToolSet,
) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools,
        client,
        resume,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    Running { handle, task }
}

impl Running {
    fn send(&self, c: AgentCommand) {
        self.handle.commands.send(c).unwrap();
    }

    /// The next event, passing over `Idle` (the tests that want it use `raw`).
    async fn next(&mut self) -> AgentEvent {
        loop {
            match self.raw().await {
                AgentEvent::Idle { .. } => continue,
                ev => return ev,
            }
        }
    }

    async fn raw(&mut self) -> AgentEvent {
        self.raw_within(Duration::from_secs(10)).await
    }

    /// As `raw`, waiting up to `wait`: a Windows runner can take seconds to start a shell.
    async fn raw_within(&mut self, wait: Duration) -> AgentEvent {
        tokio::time::timeout(wait, self.handle.events.recv())
            .await
            .expect("an event in time")
            .expect("the agent is alive")
    }

    /// Events up to and including the next `TurnEnd`.
    async fn turn(&mut self) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            let ev = self.next().await;
            let end = matches!(ev, AgentEvent::TurnEnd { .. });
            out.push(ev);
            if end {
                return out;
            }
        }
    }

    async fn until(&mut self, f: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        loop {
            let ev = self.next().await;
            let hit = f(&ev);
            out.push(ev);
            if hit {
                return out;
            }
        }
    }

    async fn stop(mut self) {
        self.send(AgentCommand::Shutdown);
        self.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        self.task.await.unwrap().unwrap();
    }
}

fn text_of(evs: &[AgentEvent]) -> String {
    evs.iter().filter_map(|e| if let AgentEvent::TextDelta { text } = e { Some(text.as_str()) } else { None }).collect()
}

fn end_of(evs: &[AgentEvent]) -> TurnEnd {
    evs.iter().rev().find_map(|e| if let AgentEvent::TurnEnd { reason } = e { Some(*reason) } else { None }).unwrap()
}

fn text_of_parts(parts: &[Part]) -> String {
    parts.iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect()
}

fn is_summary(r: &ModelRequest) -> bool {
    r.system == nexo_agent::compact::SUMMARY_SYSTEM && r.tools.is_empty()
}

fn goal_state_of(evs: &[AgentEvent]) -> Option<GoalState> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::Goal { state, .. } => Some(state.clone()),
        _ => None,
    })
}

fn notices(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let AgentEvent::Notice { text, .. } = e { Some(text.clone()) } else { None }).collect()
}

#[cfg(unix)]
fn hook_cfg(event: &str, matcher: Option<&str>, command: &str) -> nexo_core::config::HookConfig {
    nexo_core::config::HookConfig {
        event: event.into(),
        matcher: matcher.map(String::from),
        command: command.into(),
        ..Default::default()
    }
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
