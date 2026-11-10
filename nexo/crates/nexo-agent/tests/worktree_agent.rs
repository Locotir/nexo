//! A sub-agent with `isolation: worktree` works in a worktree of its own.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::permission::PermissionMode;
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

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

fn call(id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: id.into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

/// Reads and writes files under the context's directory.
struct Files(&'static str, ToolKind);

#[async_trait]
impl Tool for Files {
    fn name(&self) -> &str {
        self.0
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.0.into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        self.1
    }
    fn summary(&self, args: &Value) -> String {
        format!("{} {}", self.0, args["path"].as_str().unwrap_or(""))
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let p = ctx.cwd.join(args["path"].as_str().unwrap_or("x"));
        if self.1 == ToolKind::Write {
            std::fs::write(&p, "from the sub-agent\n").unwrap();
        }
        ToolOutput::ok("ok")
    }
}

fn git(dir: &Path, args: &[&str]) {
    assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
}

async fn delegate(repo: &Path, replies: Vec<Vec<StreamEvent>>) -> String {
    let tools = ToolSet::new()
        .with(Arc::new(Files("read", ToolKind::Read)))
        .with(Arc::new(Files("grep", ToolKind::Search)))
        .with(Arc::new(Files("write", ToolKind::Write)));
    let mut cfg = Config::default();
    cfg.providers.insert("mock".into(), ProviderConfig::new("mock", "http://mock", None));
    cfg.permissions.mode = PermissionMode::Auto;
    cfg.context.policy = "plain".into();
    cfg.agent.worktree_setup = vec!["echo set > setup.txt".into()];
    let (mut handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: repo.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools,
        client: Arc::new(Scripted(Mutex::new(replies.into()))),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    handle.commands.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] }).unwrap();
    let mut result = String::new();
    loop {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(20), handle.events.recv()).await.unwrap().unwrap();
        match ev {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => result = output.content,
            AgentEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    handle.commands.send(AgentCommand::Shutdown).unwrap();
    task.abort();
    result
}

#[tokio::test]
async fn an_isolated_sub_agent_works_in_its_own_worktree() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_CONFIG_DIR", home.path());
    std::env::set_var("NEXO_DATA_DIR", home.path());
    std::env::set_var("NEXO_CACHE_DIR", home.path().join("cache"));
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().canonicalize().unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::create_dir_all(repo.join(".nexo/agents")).unwrap();
    std::fs::write(
        repo.join(".nexo/agents/iso.md"),
        "---\nname: iso\ndescription: edits in isolation\nisolation: worktree\n---\nDo the task.",
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "first"]);
    nexo_core::config::trust_project(&repo, &nexo_core::paths::trusted_projects_file()).unwrap();

    // it writes a file: the file is in its worktree, not here, and the parent is told the branch
    let got = delegate(
        &repo,
        vec![
            call("t1", "task", json!({"agent": "iso", "prompt": "write x.txt"})),
            call("s1", "write", json!({"path": "x.txt"})),
            text("wrote x.txt"),
            text("all done"),
        ],
    )
    .await;
    assert!(got.contains("wrote x.txt") && got.contains("branch: nexo/agent-iso-"), "{got}");
    assert!(got.contains("x.txt"), "the diff stat names the file: {got}");
    assert!(!repo.join("x.txt").exists(), "the main checkout is untouched");
    let wts: Vec<_> = std::fs::read_dir(repo.join(".nexo/worktrees")).unwrap().flatten().collect();
    assert_eq!(wts.len(), 1);
    assert!(wts[0].path().join("x.txt").is_file());
    assert!(wts[0].path().join("setup.txt").is_file(), "worktree_setup ran in it");
    let ex = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    assert_eq!(ex.matches(".nexo/worktrees/").count(), 1);

    // it changes nothing: the worktree is removed again
    let got = delegate(
        &repo,
        vec![call("t1", "task", json!({"agent": "iso", "prompt": "look"})), text("nothing to do"), text("done")],
    )
    .await;
    assert!(got.contains("changed nothing"), "{got}");
    assert_eq!(std::fs::read_dir(repo.join(".nexo/worktrees")).unwrap().count(), 1, "only the first one is left");
}
