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

#[tokio::test]
async fn a_quiet_person_gets_the_report_as_the_tool_output_and_no_ended_note() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(call("t2", "task", task("general", "change a.txt"))),
            Reply::Events(text("done")),
        ],
        vec![
            ("look", vec![Reply::Events(text("reader report"))]),
            (
                "change a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt"}))),
                    Reply::Events(text("writer report")),
                ],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r.turn().await;
    // the old code made the output `{note}{report}{tail}`: no note unless background was asked, no tail
    // unless the sub-agent was isolated. A quiet person gets the report and nothing else.
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("reader report"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("writer report"));
    // the third request: the history holds both outputs, as it did, and nothing about ended jobs
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(
        dump(&reqs[2]),
        "User text: go\n\
         Assistant call t1 task {\"agent\":\"explore\",\"prompt\":\"look\"}\n\
         User result t1 error=false: reader report\n\
         Assistant call t2 task {\"agent\":\"general\",\"prompt\":\"change a.txt\"}\n\
         User result t2 error=false: writer report"
    );
    // and the writer's own tool ran as it always did
    assert_eq!(result_for(&requests_of(&client, "change a.txt"), "s1").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn an_isolated_writer_that_changed_nothing_ends_with_the_old_worktree_note() {
    let _one = TRUST.lock().await;
    data_dir();
    let (_keep, repo) = repo();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("iso", "work"))), Reply::Events(text("done"))],
        vec![Reply::Events(text("isolated report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), &repo).await;
    r.submit("go");
    let evs = r.turn().await;
    // `{report}{tail}` with the tail of `worktree_note` for a worktree without changes, as it was written before
    assert_eq!(
        done_of(&evs, "t1").as_deref(),
        Some("isolated report\n\n[worktree] the sub-agent changed nothing; its worktree was removed")
    );
    r.stop().await;
}

#[tokio::test]
async fn a_background_reader_keeps_the_old_started_text_and_the_old_ended_note() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "look"))),
            // the main agent's own tool keeps the round open while the reader ends
            Reply::Events(call("m1", "wait", json!({}))),
            Reply::Events(text("ok")),
        ],
        vec![("look", vec![Reply::Events(text("bg report"))])],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r.until_all(&[&is_done("t1"), &job_ended(1)]).await;
    // read-only with `background: true`: byte for byte what the model was told before
    assert_eq!(
        done_of(&evs, "t1").as_deref(),
        Some(
            "Started sub-agent job 1 (explore): \"look\". It works while you go on; you will be told when it ends. \
             Its report is the job's output: jobs {\"action\": \"output\", \"id\": 1} (add \"wait\" to wait for it), \
             stop it with {\"action\": \"kill\", \"id\": 1}."
        )
    );
    release(d.path(), "go");
    r.turn().await;
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 3);
    // the ended note of a job: the line, then the sentence that sends the model to the `jobs` tool
    let note = reqs[2].messages.iter().map(texts).find(|t| t.contains("[background jobs ended]")).unwrap_or_default();
    let note = note[note.find("[background jobs ended]").unwrap_or(0)..].to_string();
    assert!(
        note.starts_with("[background jobs ended]\n- job 1 sub-agent \"look\" done (reported) after ")
            && note
                .ends_with(" lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id)."),
        "{note}"
    );
    assert!(!note.contains("bg report"), "its report is read with the tool, as before: {note}");
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
    until_tool(&client, 1).await;
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
    until_tool(&client, 1).await;
    r.submit("hello");
    // the main agent now waits in its own tool; the sub-agent ends meanwhile
    r.until(|e| matches!(e, AgentEvent::ToolStart { id, .. } if id == "m1")).await;
    release(d.path(), "go");
    r.until(all_jobs_ended).await;
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
    until_tool(&client, 1).await;
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
    until_tool(&client, 2).await;
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
    until_tool(&client, 1).await;
    r.submit("hello");
    // (a negative: nothing may happen while the message waits)
    assert!(r.quiet(300, |e| matches!(e, AgentEvent::ToolDone { .. })).await, "the call is still waiting");
    release(d.path(), "go");
    let evs = r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("the report"));
    r.stop().await;
}

#[tokio::test]
async fn a_key_with_nothing_attached_leaves_no_signal_for_a_sub_agent_that_starts_later() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            // the model is still writing its reply when the key is pressed; the sub-agent starts after it
            late(d.path(), "g1", call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.send(AgentCommand::Detach);
    r.sync().await;
    release(d.path(), "g1");
    until_tool(&client, 1).await;
    release(d.path(), "go");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "it stayed attached: the key had nothing to act on");
    r.stop().await;
}

#[tokio::test]
async fn a_message_in_a_round_detaches_a_sub_agent_that_starts_later_in_the_same_round() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![late(d.path(), "g1", call("t1", "task", task("explore", "look"))), Reply::Events(text("done"))],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "g1");
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background") && out.contains("the person wrote while it ran"), "{out}");
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn a_message_does_not_detach_a_sub_agent_of_a_later_round() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("m1", "wait", json!({"file": "go2"}))),
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    // nothing is attached: the message is heard at the end of this round
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "go2");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "the next round's sub-agent is waited for");
    r.stop().await;
}

#[tokio::test]
async fn a_signal_does_not_survive_a_round_that_went_on_without_hearing_the_inbox() {
    let d = tempfile::tempdir().unwrap();
    let reasoning_only = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta("hmm".into()),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ];
    let client = Scripted::new(
        vec![
            // a reply of only reasoning: the agent asks the model to go on, no tools ran, the inbox was not read
            late(d.path(), "g1", reasoning_only),
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "g1");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "the message belonged to the round before");
    r.stop().await;
}

#[tokio::test]
async fn a_background_writer_of_any_profile_is_a_job_and_its_claim_keeps_others_out_until_it_ends() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            // the main agent's own write of the same path is refused (it waits until the holder holds)
            late(d.path(), "m1go", call("m1", "write", json!({"path": "a.txt"}))),
            // a second writer (background too)
            Reply::Events(call("t2", "task", bg("general", "also a.txt"))),
            // the main agent's own tool keeps the round open until the second writer has given up
            Reply::Events(call("m4", "wait", json!({"file": "m4go"}))),
            Reply::Events(text("waiting")),
            // the first one ended: its note starts a turn, then the path is free
            Reply::Events(call("m2", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("written")),
        ],
        vec![
            (
                "hold a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true}))),
                    Reply::Events(text("first done")),
                ],
            ),
            (
                "also a.txt",
                vec![
                    Reply::Events(call("s2", "write", json!({"path": "a.txt"}))),
                    Reply::Events(text("second gave up")),
                ],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let first = r.until(is_done("t1")).await;
    let out = done_of(&first, "t1").unwrap();
    assert!(out.contains("Started sub-agent job"), "{out}");
    until_tool(&client, 1).await;
    release(d.path(), "m1go");
    let evs = r.until_all(&[&is_done("m1"), &is_done("t2"), &job_ended(2)]).await;
    let m1 = done_of(&evs, "m1").unwrap();
    assert!(m1.contains("being changed by a sub-agent"), "{m1}");
    // the second writer was refused: its own model read who holds the file
    let refusal = result_for(&requests_of(&client, "also a.txt"), "s2").unwrap();
    assert!(
        refusal.contains("`a.txt` is being changed by another sub-agent running at the same time (hold a.txt)"),
        "{refusal}"
    );
    release(d.path(), "m4go");
    r.turn().await;
    // the holder ends: the path is free
    release(d.path(), "go");
    let evs = r.turn().await;
    let m2 = done_of(&evs, "m2").unwrap();
    assert!(m2.contains("wrote a.txt"), "free after the holder ended: {m2}");
    r.stop().await;
}

#[tokio::test]
async fn a_second_sub_agent_writes_the_path_after_the_holder_ended_normally() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // the holder's note starts this turn
            Reply::Events(call("t2", "task", task("general", "second writer"))),
            Reply::Events(text("both done")),
        ],
        vec![
            (
                "hold a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true}))),
                    Reply::Events(text("first done")),
                ],
            ),
            (
                "second writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("second done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    release(d.path(), "go");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("second done"));
    assert_eq!(result_for(&requests_of(&client, "second writer"), "s2").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn killing_the_holder_frees_its_file_for_the_main_agent_and_for_a_new_sub_agent() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // the next turn: the write waits until the test says the kill is over
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(call("t2", "task", task("general", "new writer"))),
            Reply::Events(text("all written")),
        ],
        vec![
            ("hold a.txt", vec![Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true})))]),
            (
                "new writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("new done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    r.send(AgentCommand::KillJob { id: Some(1) });
    r.until(job_ended(1)).await;
    // let the stopped run finish its own cleanup (a current-thread runtime runs it when this task yields)
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    r.submit("again");
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert!(done_of(&evs, "m1").unwrap().contains("wrote a.txt"), "{:?}", done_of(&evs, "m1"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("new done"));
    assert_eq!(result_for(&requests_of(&client, "new writer"), "s2").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn clear_frees_the_file_of_a_background_writer_too() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // after /clear, in the new conversation
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(call("t2", "task", task("general", "new writer"))),
            Reply::Events(text("all written")),
        ],
        vec![
            ("hold a.txt", vec![Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true})))]),
            (
                "new writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("new done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    r.send(AgentCommand::Clear);
    r.until(job_ended(1)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    r.submit("again");
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert!(done_of(&evs, "m1").unwrap().contains("wrote a.txt"), "{:?}", done_of(&evs, "m1"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("new done"));
    r.stop().await;
}

#[tokio::test]
async fn an_edit_of_several_files_claims_each_and_a_refused_call_gives_back_what_it_took() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold files"))),
            late(d.path(), "g2", call("t2", "task", task("general", "take b"))),
            late(d.path(), "g3", call("t3", "task", task("general", "take c"))),
            Reply::Events(text("done")),
        ],
        vec![
            (
                "hold files",
                vec![
                    Reply::Events(call(
                        "s1",
                        "write",
                        json!({"files": [{"path": "a.txt"}, {"path": "b.txt"}], "hold": true}),
                    )),
                    Reply::Events(text("held")),
                ],
            ),
            (
                "take b",
                vec![
                    // c.txt is free, b.txt is the holder's: the whole call is refused and c.txt is not kept
                    Reply::Events(call("s2", "write", json!({"paths": ["c.txt", "b.txt"]}))),
                    Reply::Events(text("gave up")),
                ],
            ),
            (
                "take c",
                vec![Reply::Events(call("s3", "write", json!({"path": "c.txt"}))), Reply::Events(text("c done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    until_tool(&client, 1).await;
    release(d.path(), "g2");
    let evs = r.until(is_done("t2")).await;
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("gave up"));
    let refusal = result_for(&requests_of(&client, "take b"), "s2").unwrap();
    assert!(
        refusal.contains("`b.txt` is being changed by another sub-agent running at the same time (hold files)"),
        "{refusal}"
    );
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t3").as_deref(), Some("c done"));
    assert_eq!(result_for(&requests_of(&client, "take c"), "s3").as_deref(), Some("wrote c.txt"));
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn a_call_id_that_comes_again_does_not_share_the_claims_of_the_first_writer() {
    let d = tempfile::tempdir().unwrap();
    // a provider that sends no ids makes `call_0` in every turn
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("call_0", "task", bg("general", "hold a.txt"))),
            late(d.path(), "g2", call("call_0", "task", task("general", "second writer"))),
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("done")),
        ],
        vec![
            (
                "hold a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true}))),
                    Reply::Events(text("first done")),
                ],
            ),
            (
                "second writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("gave up"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    until_tool(&client, 1).await;
    release(d.path(), "g2");
    // the holder's request, the second writer's two
    until_sub(&client, 3).await;
    release(d.path(), "g3");
    let evs = r.turn().await;
    let refusal = result_for(&requests_of(&client, "second writer"), "s2").unwrap();
    assert!(refusal.contains("is being changed by another sub-agent running at the same time"), "{refusal}");
    // and the second writer's end did not free the holder's file
    let m1 = done_of(&evs, "m1").unwrap();
    assert!(m1.contains("being changed by a sub-agent"), "{m1}");
    release(d.path(), "go");
    r.stop().await;
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(dir).status().unwrap();
    assert!(st.success(), "git {args:?}");
}

/// The tests that trust a project write one shared file: one at a time.
static TRUST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _one = TRUST.lock().await;
    data_dir();
    let (_keep, repo) = repo();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("iso", "work"))), Reply::Events(text("stopped"))],
        // the worktree has its own copy of the project: the sub-agent waits for `go` there
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Hang(text("never"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), &repo).await;
    r.submit("go");
    until_tool(&client, 1).await;
    let wts = repo.join(".nexo/worktrees");
    assert_eq!(std::fs::read_dir(&wts).unwrap().count(), 1, "the sub-agent works in a worktree");
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    // the call ends with "interrupted", or with what the sub-agent had said when Esc reached it (both end at
    // the same moment and either may be seen first); it never goes on to its next step
    assert!(out.contains("interrupted") || out.starts_with("never"), "{out}");
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
    until_tool(&client, 1).await;
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

fn is_ask(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::PermissionAsk { .. })
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
    let evs = r.until(is_ask).await;
    let AgentEvent::PermissionAsk { request } = evs.last().unwrap() else { unreachable!() };
    assert_eq!(request.agent.as_deref(), Some("general"));
    let id = request.id.clone();
    // the person writes: the sub-agent goes to the background with its question open
    r.submit("meanwhile");
    r.until(is_done("t1")).await;
    r.send(AgentCommand::Interrupt);
    // the events of the turn that Esc ends: none of them resolves the question
    let turn = r.turn().await;
    assert!(!turn.iter().any(is_resolved(id.clone())), "Esc denied the question of a detached sub-agent: {turn:?}");
    assert!(
        r.quiet(300, is_resolved(id.clone())).await,
        "Esc did not deny the question of the sub-agent in the background"
    );
    // the question is alive: the answer lets the sub-agent go on to its end
    r.send(AgentCommand::Permission { id, decision: PermissionDecision::Allow });
    r.until(all_jobs_ended).await;
    r.stop().await;
}

#[tokio::test]
async fn a_question_of_a_sub_agent_started_in_the_background_stays_open_on_esc() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.allow.clear();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", bg("general", "change a.txt"))), Reply::Hang(text("thinking"))],
        vec![Reply::Events(call("s1", "write", json!({"path": "a.txt"}))), Reply::Events(text("done"))],
    );
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    // the call returns at once and the sub-agent asks, in either order
    let mut evs = r.until(is_done("t1")).await;
    if !evs.iter().any(is_ask) {
        evs.extend(r.until(is_ask).await);
    }
    let request = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionAsk { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(request.agent.as_deref(), Some("general"));
    let id = request.id.clone();
    r.send(AgentCommand::Interrupt);
    let turn = r.turn().await;
    assert!(!turn.iter().any(is_resolved(id.clone())), "Esc denied the question of a background sub-agent: {turn:?}");
    assert!(r.quiet(300, is_resolved(id.clone())).await, "Esc denied it a moment later");
    r.send(AgentCommand::Permission { id, decision: PermissionDecision::Allow });
    r.until(all_jobs_ended).await;
    r.stop().await;
}
