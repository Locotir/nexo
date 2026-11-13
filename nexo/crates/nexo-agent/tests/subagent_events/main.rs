//! Each sub-agent keeps a bounded conversation that can be watched and written to. The first test pins
//! the bytes the main agent sends when nobody uses it; the rest drive the sub-agents through the `task`
//! tool (no model, no network) or through the agent loop.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use nexo_agent::subagent::SubAgent;
use nexo_agent::{AgentSetup, ModelClient, Target};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::message::Message;
use nexo_core::permission::PermissionMode;
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::subs::{SubChange, SubInfo, SubItem, SubState};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

mod golden_requests;
mod lagging_views_and_mail;
mod messages_to_sub_agents;
mod run_conversations;

/// Scripts: one for the main agent (its requests offer the `task` tool), and one queue for each kind of
/// sub-agent (picked by a word in the task it was given). `Late`
/// yields them once the file exists.
enum Reply {
    Events(Vec<StreamEvent>),
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

/// The sub-agents made `n` requests to the model in all.
async fn until_sub(client: &Scripted, n: usize) {
    wait_for(&client.probe, &format!("sub-agent request {n}"), || client.sub_count() >= n).await;
}

/// `n` tools that wait (a reader `wait` or a held write) have started in all.
async fn until_tool(client: &Scripted, n: usize) {
    wait_for(&client.probe, &format!("waiting tool {n}"), || client.probe.tools.load(Ordering::SeqCst) >= n).await;
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

/// The ways one folder is written in a request: as given, in its canonical form (a temp dir may be a link),
/// with the `\\?\` prefix Windows puts on a canonical path and without it, and with `/` for `\`. Longest
/// first, so a form never leaves the tail of a longer one behind.
fn path_forms(paths: &[String]) -> Vec<String> {
    let mut forms = Vec::new();
    for p in paths {
        let bare = p.strip_prefix(r"\\?\").unwrap_or(p);
        for f in [bare.to_string(), format!(r"\\?\{bare}")] {
            forms.push(f.replace('\\', "/"));
            forms.push(f);
        }
    }
    forms.retain(|f| !f.is_empty());
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.dedup();
    forms
}

/// Every string inside `v` with each folder form written as `<cwd>`. This works on the values and not on
/// the JSON text: there a Windows path has its backslashes doubled and would not be found.
fn blank_paths(v: &mut Value, forms: &[String]) {
    match v {
        Value::String(s) => {
            for f in forms {
                if s.contains(f.as_str()) {
                    *s = s.replace(f.as_str(), "<cwd>");
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| blank_paths(x, forms)),
        Value::Object(o) => o.values_mut().for_each(|x| blank_paths(x, forms)),
        _ => {}
    }
}

/// The main agent's requests of a run as the provider receives them: every field, the run's own ids and
/// folders written out as words.
fn wire(reqs: &[ModelRequest], cwd: &Path) -> Value {
    let mut named = vec![cwd.to_string_lossy().to_string()];
    if let Ok(c) = cwd.canonicalize() {
        named.push(c.to_string_lossy().to_string());
    }
    let forms = path_forms(&named);
    let mut all = Vec::new();
    for r in reqs {
        let mut v = serde_json::to_value(r).unwrap();
        v["session_id"] = Value::Null;
        blank_paths(&mut v, &forms);
        all.push(v);
    }
    let mut s = serde_json::to_string_pretty(&all).unwrap();
    // the date and the machine are in the system prompt
    while let Some(a) = s.find("Date: 2") {
        let b = a + s[a..].find("\\nGit:").expect("the system prompt names the repository");
        s.replace_range(a..b, "Date: <D>\\nSystem: <S>");
    }
    // the only clock in a request: how long a job took ("after 16 ms")
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("after ") {
        let (head, tail) = rest.split_at(i + 6);
        out.push_str(head);
        let n = tail.chars().take_while(char::is_ascii_digit).count();
        if n > 0 {
            out.push_str("<T>");
        }
        rest = &tail[n..];
    }
    out.push_str(rest);
    serde_json::from_str(&out).unwrap()
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sub_events").join(name)
}

/// One scripted run with a waited-for reader that uses a tool, and a background reader whose end the main
/// agent is told of. Nobody writes to a sub-agent.
async fn the_scripted_run() -> (Value, Value) {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", task("explore", "look around"))),
            Reply::Events(call("t2", "task", bg("explore", "slow look"))),
            Reply::Events(text("started it")),
            Reply::Events(text("noted the report")),
        ],
        vec![
            ("look around", vec![Reply::Events(call("s1", "read", json!({}))), Reply::Events(text("reader report"))]),
            ("slow look", vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))]),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    release(d.path(), "go");
    r.turn().await;
    r.stop().await;
    (wire(&client.main_requests(), d.path()), wire(&client.sub_requests(), d.path()))
}

/// `got` is the recorded file `name`, field by field (so a failure names the request and the field).
fn matches_golden(got: &Value, name: &str) {
    let path = golden_path(name);
    if std::env::var_os("NEXO_BLESS").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(got).unwrap() + "\n").unwrap();
    }
    let want: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (g, w) = (got.as_array().unwrap(), want.as_array().unwrap());
    assert_eq!(g.len(), w.len(), "number of requests in {name}");
    for (i, (g, w)) in g.iter().zip(w).enumerate() {
        for (k, gv) in g.as_object().unwrap() {
            assert_eq!(gv, &w[k], "{name}: request {i}, field {k}");
        }
    }
    assert_eq!(got, &want);
}

// ---- the sub-agent called through its tool -------------------------------------------------------------

/// The `task` tool over a scripted model, without an agent loop around it (so a test holds the registry).
fn task_tool(client: &Arc<Scripted>) -> SubAgent {
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    let target = Target::new("mock", cfg, "m1");
    SubAgent::new(client.clone(), target, &tools(&client.probe), vec![], Arc::new(OnceLock::new()))
}

fn call_ctx(d: &Path, call_id: &str) -> ToolContext {
    let mut c = ToolContext::new(d.to_path_buf());
    c.call_id = call_id.into();
    c.session_id = "s".into();
    c
}

/// A reply that writes `said` in pieces, then calls `name`, and spends `usage`.
fn says_then_calls(said: &[&str], id: &str, name: &str, usage: Usage) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    evs.extend(said.iter().map(|s| StreamEvent::TextDelta((*s).into())));
    evs.push(StreamEvent::ToolCallStart { id: id.into(), name: name.into() });
    evs.push(StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args: json!({}) });
    evs.push(StreamEvent::Usage(usage));
    evs.push(StreamEvent::Finish(FinishReason::ToolUse));
    evs.push(StreamEvent::Done);
    evs
}

fn spends(input: u64, output: u64) -> Usage {
    Usage { input, output, ..Default::default() }
}

fn text_spending(s: &str, usage: Usage) -> Vec<StreamEvent> {
    let mut evs = text(s);
    evs.insert(2, StreamEvent::Usage(usage));
    evs
}

/// Everything the registry has said so far.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<SubChange>) -> Vec<SubChange> {
    let mut out = Vec::new();
    while let Ok(c) = rx.try_recv() {
        out.push(c);
    }
    out
}

/// Waits for a state change of the registry that `f` accepts.
async fn until_state(rx: &mut tokio::sync::broadcast::Receiver<SubChange>, f: impl Fn(&SubInfo) -> bool) -> SubInfo {
    loop {
        match tokio::time::timeout(PATIENCE, rx.recv()).await.expect("a state change in time") {
            Ok(SubChange::State(i)) if f(&i) => return i,
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(e) => panic!("the registry closed: {e}"),
        }
    }
}

fn items_of(evs: &[SubChange], id: &str) -> Vec<SubItem> {
    evs.iter()
        .filter_map(|c| match c {
            SubChange::Item { id: i, item } if i == id => Some(item.clone()),
            _ => None,
        })
        .collect()
}

// ---- through the agent loop ------------------------------------------------------------------------------

fn is_sub_state(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::SubState { .. })
}

fn sub_id_of(evs: &[AgentEvent]) -> String {
    evs.iter()
        .find_map(|e| match e {
            AgentEvent::SubState { info } => Some(info.id.clone()),
            _ => None,
        })
        .expect("a sub-agent state event")
}

/// A background sub-agent of the agent loop that holds in a tool; returns its run id.
async fn held_sub_agent(r: &mut Running, client: &Scripted) -> String {
    r.submit("go");
    until_tool(client, 1).await;
    let evs = r.until_all(&[&is_sub_state, &|e| matches!(e, AgentEvent::TurnEnd { .. })]).await;
    sub_id_of(&evs)
}

// ---- a view that falls behind, mail that never stops, a view that asks --------------------------------------

/// A reply of `n` pieces of text, one event each.
fn many_deltas(n: usize) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    evs.extend((0..n).map(|i| StreamEvent::TextDelta(format!("p{i} "))));
    evs.push(StreamEvent::Finish(FinishReason::Stop));
    evs.push(StreamEvent::Done);
    evs
}

/// Mail keeps arriving: after every reply a message is waiting.
async fn feed_mail_until_done(
    client: &Arc<Scripted>,
    subs: &nexo_core::subs::Subs,
    d: &Path,
    run: &tokio::task::JoinHandle<ToolOutput>,
) {
    let mut seen = 0;
    while !run.is_finished() {
        if client.sub_count() > seen {
            seen += 1;
            let _ = subs.submit("t1#1", "and another thing");
            release(d, &format!("r{}", seen - 1));
        } else {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

fn slow_replies(d: &Path, n: usize) -> Vec<Reply> {
    (0..n).map(|i| late(d, &format!("r{i}"), text(&format!("answer {i}")))).collect()
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
