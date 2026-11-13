//! A sub-agent never makes the main chat wait: a message or a key sends it to the background.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent::{AgentSetup, ModelClient};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::message::{Message, Part};
use nexo_core::permission::{PermissionDecision, PermissionMode};
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

mod detaching_to_the_background;
mod escape_and_questions;
mod writer_claims;

/// Scripts: one for the main agent (its requests offer the `task` tool), and one queue for each kind of
/// sub-agent (picked by a word in the task it was given). `Hang` yields its events and never ends; `Late`
/// yields them once the file exists.
enum Reply {
    Events(Vec<StreamEvent>),
    Hang(Vec<StreamEvent>),
    Late(PathBuf, Vec<StreamEvent>),
}

/// What the mock tells the test: how many requests the sub-agents made and how many tools (that wait)
/// started. The test waits on `notify` instead of sleeping.
#[derive(Default)]
struct Probe {
    main_requests: AtomicUsize,
    sub_requests: AtomicUsize,
    tools: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl Probe {
    fn bump(&self, n: &AtomicUsize) {
        n.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// The task a sub-agent was given: its first message without the briefing the main agent's context adds.
fn task_of(req: &ModelRequest) -> String {
    let t = req.messages.first().map(|m| m.text()).unwrap_or_default();
    match t.find("</briefing>") {
        Some(i) => t[i + "</briefing>".len()..].to_string(),
        None => t,
    }
}

struct Scripted {
    main: Mutex<VecDeque<Reply>>,
    /// (a word of the task, its queue): the first whose word the sub-agent's task holds.
    subs: Mutex<Vec<(String, VecDeque<Reply>)>>,
    main_requests: Mutex<Vec<ModelRequest>>,
    sub_requests: Mutex<Vec<ModelRequest>>,
    probe: Arc<Probe>,
}

impl Scripted {
    /// One queue for every sub-agent.
    fn new(main: Vec<Reply>, sub: Vec<Reply>) -> Arc<Scripted> {
        Scripted::with_subs(main, vec![("", sub)])
    }

    /// A queue for each sub-agent, by a word of its task.
    fn with_subs(main: Vec<Reply>, subs: Vec<(&str, Vec<Reply>)>) -> Arc<Scripted> {
        Arc::new(Scripted {
            main: Mutex::new(main.into()),
            subs: Mutex::new(subs.into_iter().map(|(k, q)| (k.to_string(), q.into())).collect()),
            main_requests: Mutex::new(Vec::new()),
            sub_requests: Mutex::new(Vec::new()),
            probe: Arc::new(Probe::default()),
        })
    }

    fn main_requests(&self) -> Vec<ModelRequest> {
        self.main_requests.lock().unwrap().clone()
    }

    fn sub_requests(&self) -> Vec<ModelRequest> {
        self.sub_requests.lock().unwrap().clone()
    }

    fn sub_count(&self) -> usize {
        self.probe.sub_requests.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelClient for Scripted {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let is_main = req.tools.iter().any(|t| t.name == "task");
        let reply = if is_main {
            self.main_requests.lock().unwrap().push(req.clone());
            self.probe.bump(&self.probe.main_requests);
            self.main.lock().unwrap().pop_front()
        } else {
            self.sub_requests.lock().unwrap().push(req.clone());
            let task = task_of(req);
            let reply = {
                let mut subs = self.subs.lock().unwrap();
                subs.iter_mut().find(|(k, _)| task.contains(k.as_str())).and_then(|(_, q)| q.pop_front())
            };
            self.probe.bump(&self.probe.sub_requests);
            reply
        };
        use futures::StreamExt;
        match reply.unwrap_or(Reply::Events(text("(no script)"))) {
            Reply::Events(evs) => Ok(Box::pin(futures::stream::iter(evs))),
            Reply::Hang(evs) => Ok(Box::pin(futures::stream::iter(evs).chain(futures::stream::pending()))),
            Reply::Late(file, evs) => {
                let wait = async move {
                    while !file.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                };
                Ok(Box::pin(futures::stream::once(wait).flat_map(move |_| futures::stream::iter(evs.clone()))))
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
struct Wait(Arc<Probe>);

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
        self.0.bump(&self.0.tools);
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
struct Write(Arc<Probe>);

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
            self.0.bump(&self.0.tools);
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

fn tools(probe: &Arc<Probe>) -> ToolSet {
    ToolSet::new().with(Arc::new(Read)).with(Arc::new(Wait(probe.clone()))).with(Arc::new(Write(probe.clone())))
}

#[path = "../scratch/mod.rs"]
mod scratch;

fn data_dir() {
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    let d = DIR.get_or_init(|| scratch::until_exit(test_tmpdir().unwrap()));
    std::env::set_var("NEXO_DATA_DIR", d);
    std::env::set_var("NEXO_CONFIG_DIR", d);
    std::env::set_var("NEXO_CACHE_DIR", d.join("cache"));
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
        tools: tools(&client.probe),
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
            let ev = tokio::time::timeout(PATIENCE, self.handle.events.recv())
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

    /// Everything sent before has been handled: the agent answers a harmless command (no message may be
    /// queued before it, the answer is a `Queued` event).
    async fn sync(&mut self) {
        self.send(AgentCommand::Unqueue { index: 0, text: "(nothing)".into() });
        self.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    }

    /// Reads events until each of the wanted ones has been seen (in any order).
    async fn until_all(&mut self, wants: &[&dyn Fn(&AgentEvent) -> bool]) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        let mut got = vec![false; wants.len()];
        while !got.iter().all(|g| *g) {
            let ev = self.next().await;
            for (i, w) in wants.iter().enumerate() {
                got[i] |= w(&ev);
            }
            out.push(ev);
        }
        out
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

/// How long the tests wait for something to happen before they give up (a loaded machine is slow, not wrong).
const PATIENCE: Duration = Duration::from_secs(30);

/// Waits, without polling, until `ready` holds; the mock wakes it whenever it counts something.
async fn wait_for(probe: &Probe, what: &str, ready: impl Fn() -> bool) {
    let end = tokio::time::Instant::now() + PATIENCE;
    loop {
        let woke = probe.notify.notified();
        tokio::pin!(woke);
        woke.as_mut().enable();
        if ready() {
            return;
        }
        if tokio::time::timeout_at(end, woke).await.is_err() {
            panic!("never happened: {what}");
        }
    }
}

/// The main agent made `n` requests to the model in all.
async fn until_main(client: &Scripted, n: usize) {
    wait_for(&client.probe, &format!("main request {n}"), || client.probe.main_requests.load(Ordering::SeqCst) >= n)
        .await;
}

/// The sub-agents made `n` requests to the model in all.
async fn until_sub(client: &Scripted, n: usize) {
    wait_for(&client.probe, &format!("sub-agent request {n}"), || client.sub_count() >= n).await;
}

/// `n` tools that wait (a reader `wait` or a held write) have started in all.
async fn until_tool(client: &Scripted, n: usize) {
    wait_for(&client.probe, &format!("waiting tool {n}"), || client.probe.tools.load(Ordering::SeqCst) >= n).await;
}

/// The jobs event that says no job is running.
fn all_jobs_ended(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::Jobs { jobs } if !jobs.is_empty() && jobs.iter().all(|j| !j.status.running()))
}

fn release(d: &Path, name: &str) {
    std::fs::write(d.join(name), "").unwrap();
}

fn texts(m: &Message) -> String {
    m.text()
}

fn task(agent: &str, prompt: &str) -> Value {
    json!({"agent": agent, "prompt": prompt})
}

fn bg(agent: &str, prompt: &str) -> Value {
    json!({"agent": agent, "prompt": prompt, "background": true})
}

/// A model reply that comes once the test creates the file `name` in `d`.
fn late(d: &Path, name: &str, evs: Vec<StreamEvent>) -> Reply {
    Reply::Late(d.join(name), evs)
}

/// The requests one kind of sub-agent made (by a word of its task).
fn requests_of(client: &Scripted, word: &str) -> Vec<ModelRequest> {
    client.sub_requests().into_iter().filter(|r| task_of(r).contains(word)).collect()
}

/// What the model read as the result of its call `id` in the last of these requests.
fn result_for(reqs: &[ModelRequest], id: &str) -> Option<String> {
    reqs.iter().rev().flat_map(|r| r.messages.iter()).flat_map(|m| m.content.iter()).find_map(|p| match p {
        Part::ToolResult { call_id, content, .. } if call_id == id => {
            Some(content.iter().filter_map(Part::as_text).collect::<String>())
        }
        _ => None,
    })
}

/// A request's messages, one line per part, for a golden.
fn dump(req: &ModelRequest) -> String {
    let mut out = Vec::new();
    for m in &req.messages {
        for p in &m.content {
            out.push(match p {
                Part::Text { text } => format!("{:?} text: {text}", m.role),
                Part::ToolCall { id, name, args } => format!("{:?} call {id} {name} {args}", m.role),
                Part::ToolResult { call_id, content, is_error, .. } => {
                    let t: String = content.iter().filter_map(Part::as_text).collect();
                    format!("{:?} result {call_id} error={is_error}: {t}", m.role)
                }
                _ => format!("{:?} other", m.role),
            });
        }
    }
    out.join("\n")
}

fn is_done(id: &'static str) -> impl Fn(&AgentEvent) -> bool {
    move |e| matches!(e, AgentEvent::ToolDone { id: i, .. } if i == id)
}

fn is_resolved(id: String) -> impl Fn(&AgentEvent) -> bool {
    move |e| matches!(e, AgentEvent::PermissionResolved { id: i, .. } if *i == id)
}

/// The jobs event that says job `n` is no longer running.
fn job_ended(n: u32) -> impl Fn(&AgentEvent) -> bool {
    move |e| matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == n && !j.status.running()))
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {args:?}");
}

/// The tests that trust a project write one shared file: one at a time.
static TRUST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn repo() -> (tempfile::TempDir, PathBuf) {
    let d = test_tmpdir().unwrap();
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

fn is_ask(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::PermissionAsk { .. })
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
