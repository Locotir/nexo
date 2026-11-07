//! The managed policy inside a running agent: what `/model`, the permission mode, the hooks and
//! `/status` do when an administrator's file is in force.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::managed::ManagedPolicy;
use nexo_core::permission::PermissionMode;
use nexo_core::provider::{EventStream, ModelRequest};
use nexo_core::stream::{FinishReason, StreamEvent};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

/// Answers every request from a script of event lists.
struct Scripted(Mutex<VecDeque<Vec<StreamEvent>>>);

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let evs = self.0.lock().unwrap().pop_front().unwrap_or_else(|| text("(no script)"));
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

fn text(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta(s.into()),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ]
}

fn call(name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: "c1".into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: "c1".into(), name: name.into(), args },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

struct Writer;

#[async_trait]
impl Tool for Writer {
    fn name(&self) -> &str {
        "write"
    }
    fn spec(&self) -> nexo_core::provider::ToolSpec {
        nexo_core::provider::ToolSpec {
            name: "write".into(),
            description: "fake".into(),
            parameters: json!({"type": "object"}),
        }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, _: &Value) -> String {
        "write".into()
    }
    async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
        ToolOutput::ok("written")
    }
}

fn data_dir() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_CONFIG_DIR", d.path());
    std::env::set_var("NEXO_CACHE_DIR", d.path().join("cache"));
}

fn policy(text: &str) -> Arc<ManagedPolicy> {
    let mut p = ManagedPolicy::parse(text).unwrap();
    p.file = PathBuf::from("/etc/nexo/managed.toml");
    Arc::new(p)
}

fn config(p: &Arc<ManagedPolicy>) -> Config {
    let mut cfg = Config::default();
    let mut m = ProviderConfig::new("mock", "http://mock", None);
    m.models = vec!["m1".into(), "m2".into()];
    cfg.providers.insert("mock".into(), m);
    cfg.context.policy = "plain".into();
    // loaded as `Config::load` does it: the policy is attached and applied last
    cfg.managed = Some(p.clone());
    cfg.enforce_managed();
    cfg
}

struct Running {
    handle: AgentHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(cfg: Config, script: Vec<Vec<StreamEvent>>, cwd: &std::path::Path) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let tools = ToolSet::new().with(Arc::new(Writer));
    let setup = AgentSetup {
        config: cfg,
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools,
        client: Arc::new(Scripted(Mutex::new(script.into()))),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    r.until(|e| matches!(e, AgentEvent::Ready { .. })).await;
    r
}

impl Running {
    fn send(&self, c: AgentCommand) {
        self.handle.commands.send(c).unwrap();
    }

    async fn next(&mut self) -> AgentEvent {
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(10), self.handle.events.recv())
                .await
                .expect("an event in time")
                .expect("the agent is alive");
            if !matches!(ev, AgentEvent::Idle { .. }) {
                return ev;
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

fn notice_text(ev: &AgentEvent) -> String {
    match ev {
        AgentEvent::Notice { text, .. } => text.clone(),
        _ => String::new(),
    }
}

#[tokio::test]
async fn a_denied_model_is_refused_at_model_and_an_allowed_one_switches() {
    let d = tempfile::tempdir().unwrap();
    let p = policy("[models]\ndeny = [\"m2\"]");
    let mut r = start(config(&p), vec![], d.path()).await;
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    let ev = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    let said = notice_text(ev.last().unwrap());
    assert!(said.contains("blocked by your organization's policy (/etc/nexo/managed.toml)"), "{said}");
    r.send(AgentCommand::SetModel { provider: None, model: "m1".into() });
    r.until(|e| matches!(e, AgentEvent::ModelChanged { model, .. } if model == "m1")).await;
    r.stop().await;
}

#[tokio::test]
async fn bypass_mode_is_refused_when_the_policy_turns_it_off() {
    let d = tempfile::tempdir().unwrap();
    let p = policy("disable_bypass_mode = true");
    let mut r = start(config(&p), vec![], d.path()).await;
    r.send(AgentCommand::SetPermissionMode(PermissionMode::Auto));
    let ev = r.until(|e| matches!(e, AgentEvent::PermissionModeChanged { .. })).await;
    assert!(ev.iter().any(|e| notice_text(e).contains("blocked by your organization's policy")), "{ev:?}");
    assert!(matches!(ev.last(), Some(AgentEvent::PermissionModeChanged { mode: PermissionMode::Ask })));
    r.send(AgentCommand::SetPermissionMode(PermissionMode::AcceptEdits));
    r.until(|e| matches!(e, AgentEvent::PermissionModeChanged { mode: PermissionMode::AcceptEdits })).await;
    r.stop().await;
}

#[tokio::test]
async fn a_managed_deny_rule_beats_the_users_allow_rule() {
    let d = tempfile::tempdir().unwrap();
    let p = policy("[permissions]\ndeny = [\"write\"]");
    let mut cfg = config(&p);
    cfg.permissions.allow.push("write".into());
    cfg.permissions.mode = PermissionMode::Auto;
    let mut r = start(cfg, vec![call("write", json!({"path": "x"})), text("ok")], d.path()).await;
    r.send(AgentCommand::Submit { text: "write it".into(), attachments: vec![] });
    let ev = r.until(|e| matches!(e, AgentEvent::ToolDone { .. })).await;
    assert!(
        ev.iter().any(|e| matches!(e, AgentEvent::ToolDone { output, .. } if output.is_error && output.content.contains("denied by rule"))),
        "{ev:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn status_lists_what_the_policy_forces_with_its_source() {
    let d = tempfile::tempdir().unwrap();
    let p = policy("disable_web = true\n[permissions]\nsandbox = \"workspace-write\"\ndeny = [\"bash(rm *)\"]");
    let mut r = start(config(&p), vec![], d.path()).await;
    r.send(AgentCommand::ShowStatus);
    let ev = r.until(|e| notice_text(e).contains("managed policy /etc")).await;
    let s = notice_text(ev.last().unwrap());
    assert!(s.contains("managed policy /etc/nexo/managed.toml"), "{s}");
    assert!(s.contains("permissions.sandbox = workspace-write (at least)  (source: managed)"), "{s}");
    assert!(s.contains("permissions.deny = bash(rm *)  (source: managed)"), "{s}");
    assert!(s.contains("disable_web = true  (source: managed)"), "{s}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn user_hooks_are_off_when_disallowed_and_forced_hooks_run() {
    let d = tempfile::tempdir().unwrap();
    let p = policy(
        "allow_user_hooks = false\n[[forced_hooks]]\nevent = \"user_prompt_submit\"\ncommand = \"touch forced_ran\"",
    );
    let mut cfg = Config::default();
    let mut m = ProviderConfig::new("mock", "http://mock", None);
    m.models = vec!["m1".into()];
    cfg.providers.insert("mock".into(), m);
    cfg.context.policy = "plain".into();
    cfg.hooks.push(nexo_core::config::HookConfig {
        event: "user_prompt_submit".into(),
        command: "touch user_ran".into(),
        ..Default::default()
    });
    cfg.managed = Some(p);
    cfg.enforce_managed();
    let mut r = start(cfg, vec![text("hi")], d.path()).await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    assert!(d.path().join("forced_ran").exists(), "the forced hook runs");
    assert!(!d.path().join("user_ran").exists(), "the user's hook is off");
    r.send(AgentCommand::ShowHooks);
    let ev = r.until(|e| notice_text(e).contains("forced_ran")).await;
    assert!(notice_text(ev.last().unwrap()).contains("managed"));
    r.stop().await;
}
