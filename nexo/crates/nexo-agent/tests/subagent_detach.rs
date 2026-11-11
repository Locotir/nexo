//! A sub-agent never makes the main chat wait: a message or a key sends it to the background.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::message::Message;
use nexo_core::permission::{PermissionDecision, PermissionMode};
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

/// Two scripts: one for the main agent (its requests offer the `task` tool), one for the sub-agents.
/// `Hang` yields its events and never ends.
enum Reply {
    Events(Vec<StreamEvent>),
    Hang(Vec<StreamEvent>),
}

struct Scripted {
    main: Mutex<VecDeque<Reply>>,
    sub: Mutex<VecDeque<Reply>>,
    main_requests: Mutex<Vec<ModelRequest>>,
    sub_requests: Mutex<usize>,
}

impl Scripted {
    fn new(main: Vec<Reply>, sub: Vec<Reply>) -> Arc<Scripted> {
        Arc::new(Scripted {
            main: Mutex::new(main.into()),
            sub: Mutex::new(sub.into()),
            main_requests: Mutex::new(Vec::new()),
            sub_requests: Mutex::new(0),
        })
    }

    fn main_requests(&self) -> Vec<ModelRequest> {
        self.main_requests.lock().unwrap().clone()
    }

    fn sub_count(&self) -> usize {
        *self.sub_requests.lock().unwrap()
    }
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let is_main = req.tools.iter().any(|t| t.name == "task");
        let reply = if is_main {
            self.main_requests.lock().unwrap().push(req.clone());
            self.main.lock().unwrap().pop_front()
        } else {
            *self.sub_requests.lock().unwrap() += 1;
            self.sub.lock().unwrap().pop_front()
        };
        match reply.unwrap_or(Reply::Events(text("(no script)"))) {
            Reply::Events(evs) => Ok(Box::pin(futures::stream::iter(evs))),
            Reply::Hang(evs) => {
                use futures::StreamExt;
                Ok(Box::pin(futures::stream::iter(evs).chain(futures::stream::pending())))
            }
        }
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

fn two_calls(a: (&str, &str, Value), b: (&str, &str, Value)) -> Vec<StreamEvent> {
    let mut evs = Vec::new();
    for (id, name, args) in [a, b] {
        evs.push(StreamEvent::ToolCallStart { id: id.into(), name: name.into() });
        evs.push(StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args });
    }
    evs.push(StreamEvent::Finish(FinishReason::ToolUse));
    evs.push(StreamEvent::Done);
    evs
}

/// Waits until the file `args.file` exists (or it is cancelled); a reader.
struct Wait;

#[async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        "wait"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "wait".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "wait".into()
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let file = ctx.cwd.join(args["file"].as_str().unwrap_or("go"));
        gate(&file, ctx).await
    }
}

async fn gate(file: &Path, ctx: &ToolContext) -> ToolOutput {
    let wait = async {
        while !file.exists() {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    };
    tokio::select! {
        _ = wait => ToolOutput::ok("went"),
        _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled"),
    }
}

/// A write that holds (waits for `go`) when asked to, else is quick. It writes nothing.
struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "write".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, args: &Value) -> String {
        format!("write {}", args["path"].as_str().unwrap_or(""))
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        if args["hold"].as_bool() == Some(true) {
            let out = gate(&ctx.cwd.join("go"), ctx).await;
            if out.is_error {
                return out;
            }
        }
        ToolOutput::ok(format!("wrote {}", args["path"].as_str().unwrap_or("")))
    }
}

/// The reader that makes the sub-agents available.
struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _: &Value) -> String {
        "read".into()
    }
    async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
        ToolOutput::ok("contents")
    }
}

fn tools() -> ToolSet {
    ToolSet::new().with(Arc::new(Read)).with(Arc::new(Wait)).with(Arc::new(Write))
}

fn data_dir() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_CONFIG_DIR", d.path());
    std::env::set_var("NEXO_CACHE_DIR", d.path().join("cache"));
}

fn config(mode: PermissionMode) -> Config {
    let mut cfg = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    cfg.providers.insert("mock".into(), p);
    cfg.permissions.mode = mode;
    cfg.permissions.allow.push("write".into());
    cfg.context.policy = "plain".into();
    cfg.agent.attach_budget = 0;
    cfg.agent.check_at_start = false;
    cfg
}

struct Running {
    handle: AgentHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(client: Arc<Scripted>, cfg: Config, cwd: &Path) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: cfg,
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client,
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    r.next().await;
    r
}

impl Running {
    fn send(&self, c: AgentCommand) {
        self.handle.commands.send(c).unwrap();
    }

    fn submit(&self, t: &str) {
        self.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
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

    async fn turn(&mut self) -> Vec<AgentEvent> {
        self.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await
    }

    /// No event of this kind within a short while.
    async fn quiet(&mut self, ms: u64, f: impl Fn(&AgentEvent) -> bool) -> bool {
        let end = tokio::time::Instant::now() + Duration::from_millis(ms);
        while let Ok(Some(ev)) = tokio::time::timeout_at(end, self.handle.events.recv()).await {
            if f(&ev) {
                return false;
            }
        }
        true
    }

    async fn stop(mut self) {
        self.send(AgentCommand::Shutdown);
        self.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        self.task.await.unwrap().unwrap();
    }
}

fn done_of(evs: &[AgentEvent], id: &str) -> Option<String> {
    evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id: i, output, .. } if i == id => Some(output.content.clone()),
        _ => None,
    })
}

async fn until_sub(client: &Scripted, n: usize) {
    for _ in 0..400 {
        if client.sub_count() >= n {
            // and a moment for the tool it asked for to start
            tokio::time::sleep(Duration::from_millis(80)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    panic!("the sub-agent never made request {n}");
}

fn release(d: &Path, name: &str) {
    std::fs::write(d.join(name), "").unwrap();
}

fn texts(m: &Message) -> String {
    m.text()
}

fn any_message_has(reqs: &[ModelRequest], needle: &str) -> bool {
    reqs.iter().any(|r| r.messages.iter().any(|m| texts(m).contains(needle)))
}

fn task(agent: &str, prompt: &str) -> Value {
    json!({"agent": agent, "prompt": prompt})
}

#[tokio::test]
async fn a_quiet_person_gets_the_report_as_the_tool_output_and_no_ended_note() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(call("t2", "task", task("general", "change a.txt"))),
            Reply::Events(text("done")),
        ],
        vec![
            Reply::Events(text("reader report")),
            Reply::Events(call("s1", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("writer report")),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("reader report"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("writer report"));
    // the third request: the history holds both outputs and nothing about ended jobs
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 3);
    assert!(!any_message_has(&reqs, "background jobs ended"));
    assert!(!any_message_has(&reqs, "Moved to the background"));
    r.stop().await;
}

#[tokio::test]
async fn a_message_while_a_sub_agent_runs_moves_it_to_the_background_and_its_report_comes_as_a_note() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow look"))),
            Reply::Events(text("answering you")),
            // idle again: the sub-agent ends and a turn starts with its note
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_sub(&client, 1).await;
    r.submit("hello meanwhile");
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background as job") && out.contains("the person wrote while it ran"), "{out}");
    // the main agent answered before the sub-agent ended; the message came in the same turn, after the results
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("answering"))));
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 2);
    let last = reqs[1].messages.last().unwrap();
    assert!(texts(last).contains("hello meanwhile"), "{last:?}");
    assert_eq!(client.sub_count(), 1, "it has not ended");
    // it ends while idle: a turn starts with its report inline
    release(d.path(), "go");
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("noted"))));
    let reqs = client.main_requests();
    let note = texts(reqs[2].messages.last().unwrap());
    assert!(note.contains("background jobs ended") && note.contains("late report"), "{note}");
    assert!(!note.contains("`jobs` tool"), "the report is inline, no jobs read is needed: {note}");
    r.stop().await;
}

#[tokio::test]
async fn a_detached_sub_agent_that_ends_while_the_main_agent_works_joins_at_the_next_round() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow look"))),
            Reply::Events(call("m1", "wait", json!({"file": "go2"}))),
            Reply::Events(text("all in")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_sub(&client, 1).await;
    r.submit("hello");
    // the main agent now waits in its own tool; the sub-agent ends meanwhile
    r.until(|e| matches!(e, AgentEvent::ToolStart { id, .. } if id == "m1")).await;
    release(d.path(), "go");
    until_sub(&client, 2).await;
    release(d.path(), "go2");
    r.turn().await;
    let reqs = client.main_requests();
    let last = reqs.last().unwrap();
    let all: String = last.messages.iter().map(texts).collect::<Vec<_>>().join("\n");
    assert!(all.contains("late report"), "the note joined the round: {all}");
    r.stop().await;
}

#[tokio::test]
async fn the_key_sends_it_to_the_background_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("explore", "slow"))), Reply::Events(text("free"))],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("r"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_sub(&client, 1).await;
    r.send(AgentCommand::Detach);
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background") && out.contains("the person sent it to the background"), "{out}");
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn two_attached_sub_agents_both_detach_on_one_message() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(two_calls(("t1", "task", task("explore", "a")), ("t2", "task", task("explore", "b")))),
            Reply::Events(text("free")),
        ],
        vec![
            Reply::Events(call("w1", "wait", json!({}))),
            Reply::Events(call("w2", "wait", json!({}))),
            Reply::Events(text("ra")),
            Reply::Events(text("rb")),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_sub(&client, 2).await;
    r.submit("hey");
    let evs = r.turn().await;
    for id in ["t1", "t2"] {
        let out = done_of(&evs, id).unwrap();
        assert!(out.contains("Moved to the background"), "{id}: {out}");
    }
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn with_detach_on_message_off_the_message_waits_for_the_round() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow"))),
            Reply::Events(text("round over")),
            Reply::Events(text("now your message")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("the report"))],
    );
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.detach_on_message = false;
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    until_sub(&client, 1).await;
    r.submit("hello");
    assert!(r.quiet(300, |e| matches!(e, AgentEvent::ToolDone { .. })).await, "the call is still waiting");
    release(d.path(), "go");
    let evs = r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("the report"));
    r.stop().await;
}

#[tokio::test]
async fn a_background_writer_of_any_profile_is_a_job_and_its_claim_keeps_others_out_until_it_ends() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "hold a.txt", "background": true}))),
            // the main agent's own write of the same path is refused
            Reply::Events(call("m1", "write", json!({"path": "a.txt"}))),
            // a second writer (background too)
            Reply::Events(call("t2", "task", json!({"agent": "general", "prompt": "also a.txt", "background": true}))),
            Reply::Events(text("waiting")),
            // the first one ended: its note starts a turn, then the path is free
            Reply::Events(call("m2", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("written")),
        ],
        vec![
            Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true}))),
            Reply::Events(call("s2", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("second gave up")),
            Reply::Events(text("first done")),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let first = r.until(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "t1")).await;
    let out = done_of(&first, "t1").unwrap();
    assert!(out.contains("Started sub-agent job"), "{out}");
    until_sub(&client, 1).await;
    let evs = r.turn().await;
    let m1 = done_of(&evs, "m1").unwrap();
    assert!(m1.contains("being changed by a sub-agent"), "{m1}");
    // the second writer was refused too (it said so to its own model, then gave up)
    let t2 = done_of(&evs, "t2").unwrap();
    assert!(t2.contains("Started sub-agent job"), "{t2}");
    release(d.path(), "go");
    let evs = r.turn().await;
    let m2 = done_of(&evs, "m2").unwrap();
    assert!(m2.contains("wrote a.txt"), "free after the holder ended: {m2}");
    r.stop().await;
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {args:?}");
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().canonicalize().unwrap();
    git(&p, &["init", "-q"]);
    std::fs::write(p.join("a.txt"), "a\n").unwrap();
    std::fs::create_dir_all(p.join(".nexo/agents")).unwrap();
    std::fs::write(
        p.join(".nexo/agents/iso.md"),
        "---\nname: iso\ndescription: edits in isolation\nisolation: worktree\n---\nDo the task.",
    )
    .unwrap();
    git(&p, &["add", "."]);
    git(&p, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "first"]);
    nexo_core::config::trust_project(&p, &nexo_core::paths::trusted_projects_file()).unwrap();
    (d, p)
}

#[tokio::test]
async fn esc_stops_an_attached_isolated_writer_and_cleans_its_worktree() {
    data_dir();
    let (_keep, repo) = repo();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("iso", "work"))), Reply::Events(text("stopped"))],
        // the worktree has its own copy of the project: the sub-agent waits for `go` there
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("never"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), &repo).await;
    r.submit("go");
    until_sub(&client, 1).await;
    let wts = repo.join(".nexo/worktrees");
    assert_eq!(std::fs::read_dir(&wts).unwrap().count(), 1, "the sub-agent works in a worktree");
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert!(done_of(&evs, "t1").unwrap().contains("interrupted"));
    // the worktree was finished before the call returned: it changed nothing, so it is gone
    assert_eq!(std::fs::read_dir(&wts).map(|d| d.count()).unwrap_or(0), 0, "no worktree is left");
    r.stop().await;
}

#[tokio::test]
async fn esc_leaves_a_detached_sub_agent_running() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow"))),
            Reply::Hang(text("thinking")),
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("survived"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_sub(&client, 1).await;
    r.submit("meanwhile");
    r.until(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "t1")).await;
    r.send(AgentCommand::Interrupt);
    r.turn().await;
    // it was not touched: it goes on and reports
    release(d.path(), "go");
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("noted"))));
    let reqs = client.main_requests();
    assert!(texts(reqs.last().unwrap().messages.last().unwrap()).contains("survived"));
    r.stop().await;
}

#[tokio::test]
async fn a_question_of_a_sub_agent_names_it_and_esc_leaves_a_detached_ones_open() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.allow.clear();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("general", "change a.txt"))), Reply::Hang(text("thinking"))],
        vec![Reply::Events(call("s1", "write", json!({"path": "a.txt"}))), Reply::Events(text("done"))],
    );
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    let AgentEvent::PermissionAsk { request } = evs.last().unwrap() else { unreachable!() };
    assert_eq!(request.agent.as_deref(), Some("general"));
    let id = request.id.clone();
    // the person writes: the sub-agent goes to the background with its question open
    r.submit("meanwhile");
    r.until(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "t1")).await;
    r.send(AgentCommand::Interrupt);
    r.turn().await;
    assert!(
        r.quiet(300, |e| matches!(e, AgentEvent::PermissionResolved { id: i, .. } if *i == id)).await,
        "Esc did not deny the question of the sub-agent in the background"
    );
    r.send(AgentCommand::Permission { id, decision: PermissionDecision::Allow });
    r.stop().await;
}
