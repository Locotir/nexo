//! The `workflow` tool, end to end with a scripted model: no network, no real provider. The sub-agents are the
//! ones the `task` tool starts; the mock counts how many are at work at once and what each was asked.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nexo_agent::subagent::SubAgent;
use nexo_agent::workflow::file::subject;
use nexo_agent::workflow::Workflow;
use nexo_agent::{AgentSetup, ModelClient, Target};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::permission::{PermissionDecision, PermissionMode};
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::subs::{FlowInfo, FlowState, StepState, SubChange, SubState};
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

mod clearing_a_conversation;
mod permissions_and_writers;
mod progress_and_stopping;
mod prompts_and_answers;
mod replacing_a_conversation;
mod runs_and_limits;
mod start_by_name;
mod tool_hooks;

const PATIENCE: Duration = Duration::from_secs(30);

/// Waits for `f` at most [`PATIENCE`]: a rule that is broken fails the test with a message, it does not hang it.
async fn bounded<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, f).await.unwrap_or_else(|_| panic!("{what} did not end in time"))
}

/// What a sub-agent answers.
#[derive(Clone)]
enum Say {
    /// A report, spending this many tokens (input, output).
    Text(String, u64, u64),
    /// Never answers.
    Hang,
    /// One of these for each call, in turn.
    Cycle(Vec<String>),
    /// A report once the test lets one sub-agent through its gate (`State::gate`).
    Gated(String),
    /// The model refuses the request.
    Fail,
    /// A report with no usage in the stream.
    Bare(String),
    /// Calls this tool once, then reports (the second request holds the tool result).
    CallThen(&'static str, String),
}

fn says(s: &str) -> Say {
    Say::Text(s.into(), 10, 5)
}

struct State {
    tasks: Mutex<Vec<String>>,
    tools_seen: Mutex<Vec<Vec<String>>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
    /// (true: began, false: ended, the task).
    log: Mutex<Vec<(bool, String)>>,
    notify: tokio::sync::Notify,
    /// Sub-agents that were gated finish one for each permit.
    gate: tokio::sync::Semaphore,
    ended: AtomicUsize,
}

impl Default for State {
    fn default() -> State {
        State {
            tasks: Mutex::default(),
            tools_seen: Mutex::default(),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            log: Mutex::default(),
            notify: tokio::sync::Notify::new(),
            gate: tokio::sync::Semaphore::new(0),
            ended: AtomicUsize::new(0),
        }
    }
}

struct Mock {
    /// (a word of the task, the answer): the first whose word the task holds.
    rules: Vec<(String, Say)>,
    main: Mutex<VecDeque<Vec<StreamEvent>>>,
    turns: AtomicUsize,
    st: Arc<State>,
    /// The last message of every request the main agent made.
    main_msgs: Mutex<Vec<String>>,
}

impl Mock {
    fn new(rules: Vec<(&str, Say)>) -> Arc<Mock> {
        Mock::with_main(rules, vec![])
    }

    fn with_main(rules: Vec<(&str, Say)>, main: Vec<Vec<StreamEvent>>) -> Arc<Mock> {
        Arc::new(Mock {
            rules: rules.into_iter().map(|(k, s)| (k.to_string(), s)).collect(),
            main: Mutex::new(main.into()),
            turns: AtomicUsize::new(0),
            st: Arc::new(State::default()),
            main_msgs: Mutex::default(),
        })
    }

    fn tasks(&self) -> Vec<String> {
        self.st.tasks.lock().unwrap().clone()
    }

    /// How many tasks hold `word`.
    fn count(&self, word: &str) -> usize {
        self.tasks().iter().filter(|t| t.contains(word)).count()
    }

    async fn until_ended(&self, n: usize) {
        let end = tokio::time::Instant::now() + PATIENCE;
        while self.st.ended.load(Ordering::SeqCst) < n {
            assert!(tokio::time::Instant::now() < end, "{n} sub-agents never ended");
            let _ = tokio::time::timeout(Duration::from_millis(20), self.st.notify.notified()).await;
        }
    }

    async fn until_started(&self, n: usize) {
        let end = tokio::time::Instant::now() + PATIENCE;
        while self.st.started.load(Ordering::SeqCst) < n {
            assert!(tokio::time::Instant::now() < end, "{n} sub-agents were never started");
            let _ = tokio::time::timeout(Duration::from_millis(20), self.st.notify.notified()).await;
        }
    }
}

fn task_of(req: &ModelRequest) -> String {
    let t = req.messages.first().map(|m| m.text()).unwrap_or_default();
    match t.find("</briefing>") {
        Some(i) => t[i + "</briefing>".len()..].to_string(),
        None => t,
    }
}

fn text_events(s: &str, input: u64, output: u64) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta(s.into()),
        StreamEvent::Usage(Usage { input, output, ..Default::default() }),
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

#[async_trait]
impl ModelClient for Mock {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
        // the main agent is the one that has the workflow tool
        if names.iter().any(|n| n == "workflow") {
            self.main_msgs.lock().unwrap().push(req.messages.last().map(|m| m.text()).unwrap_or_default());
            let evs = self.main.lock().unwrap().pop_front().unwrap_or_else(|| text_events("ok", 1, 1));
            return Ok(Box::pin(futures::stream::iter(evs)));
        }
        let task = task_of(req);
        // the second request of a sub-agent that called a tool: it holds the result, and it is the same task
        let again = req.messages.len() > 1;
        let say = self.rules.iter().find(|(k, _)| task.contains(k.as_str())).map(|(_, s)| s.clone());
        let st = self.st.clone();
        if let Some(Say::CallThen(tool, text)) = &say {
            if again {
                // what came back to it, for the tests that look at a refused call
                st.log.lock().unwrap().push((false, format!("RESULT {:?}", req.messages.last())));
                return Ok(Box::pin(futures::stream::iter(text_events(text, 10, 5))));
            }
            let evs = call("c1", tool, json!({}));
            st.tasks.lock().unwrap().push(task);
            st.tools_seen.lock().unwrap().push(names);
            return Ok(Box::pin(futures::stream::iter(evs)));
        }
        st.tasks.lock().unwrap().push(task.clone());
        st.tools_seen.lock().unwrap().push(names);
        let now = st.active.fetch_add(1, Ordering::SeqCst) + 1;
        st.peak.fetch_max(now, Ordering::SeqCst);
        st.log.lock().unwrap().push((true, task.clone()));
        st.started.fetch_add(1, Ordering::SeqCst);
        st.notify.notify_waiters();
        let say = match say.unwrap_or_else(|| says("done")) {
            Say::Cycle(all) => says(&all[self.turns.fetch_add(1, Ordering::SeqCst) % all.len()]),
            other => other,
        };
        match say {
            Say::Cycle(_) | Say::CallThen(..) => unreachable!(),
            Say::Fail => Err(ProviderError::new(ErrorKind::BadRequest, "the model said no")),
            Say::Bare(s) => {
                let evs: Vec<StreamEvent> =
                    text_events(&s, 0, 0).into_iter().filter(|e| !matches!(e, StreamEvent::Usage(_))).collect();
                st.active.fetch_sub(1, Ordering::SeqCst);
                Ok(Box::pin(futures::stream::iter(evs)))
            }
            Say::Gated(s) => {
                // held until the test lets one through: no number of yields decides who is at work
                let work = async move {
                    st.gate.acquire().await.unwrap().forget();
                    st.active.fetch_sub(1, Ordering::SeqCst);
                    st.log.lock().unwrap().push((false, task));
                    st.ended.fetch_add(1, Ordering::SeqCst);
                    st.notify.notify_waiters();
                };
                let evs = text_events(&s, 10, 5);
                Ok(Box::pin(futures::stream::once(work).flat_map(move |_| futures::stream::iter(evs.clone()))))
            }
            Say::Hang => Ok(Box::pin(futures::stream::pending())),
            Say::Text(s, input, output) => {
                // others get to start while this one is at work
                let work = async move {
                    for _ in 0..25 {
                        tokio::task::yield_now().await;
                    }
                    st.active.fetch_sub(1, Ordering::SeqCst);
                    st.log.lock().unwrap().push((false, task));
                };
                let evs = text_events(&s, input, output);
                Ok(Box::pin(futures::stream::once(work).flat_map(move |_| futures::stream::iter(evs.clone()))))
            }
        }
    }
}

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

/// A tool of that name that does nothing.
struct Named(&'static str);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.0.into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn summary(&self, _: &Value) -> String {
        self.0.into()
    }
    async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
        ToolOutput::ok("x")
    }
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

fn sub_agent(client: &Arc<Mock>, set: ToolSet) -> SubAgent {
    sub_agent_with(client, set, vec![])
}

fn sub_agent_with(client: &Arc<Mock>, set: ToolSet, profiles: Vec<nexo_agent::subagent::Profile>) -> SubAgent {
    data_dir();
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    SubAgent::new(client.clone(), Target::new("mock", cfg, "m1"), &set, profiles, Arc::new(OnceLock::new()))
}

fn tool(client: &Arc<Mock>, cwd: &Path) -> Workflow {
    Workflow::new(Arc::new(sub_agent(client, ToolSet::new().with(Arc::new(Read)))), cwd.to_path_buf())
}

/// A tool set where the `general` kind can change things (`edit` is neither a read nor a search), so it is a
/// writer for the `task` tool and for the runner; `explore` stays read-only.
fn writer_tool(client: &Arc<Mock>, cwd: &Path, parallel: bool) -> Workflow {
    let set = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
    Workflow::new(Arc::new(sub_agent(client, set).with_parallel(parallel)), cwd.to_path_buf())
}

fn ctx(d: &Path) -> ToolContext {
    let mut c = ToolContext::new(d.to_path_buf());
    c.call_id = "w".into();
    c.session_id = "s".into();
    c
}

async fn go(client: &Arc<Mock>, d: &Path, toml: &str, args: &str) -> (ToolOutput, ToolContext) {
    let c = ctx(d);
    let out = tool(client, d).run(json!({"inline": toml, "args": args}), &c).await;
    (out, c)
}

fn steps_line<'a>(out: &'a ToolOutput, id: &str) -> &'a str {
    out.content
        .lines()
        .find(|l| l.starts_with(&format!("- {id}:")))
        .unwrap_or_else(|| panic!("no line for {id}: {}", out.content))
}

#[tokio::test]
async fn never_more_than_max_parallel_at_work_and_phases_do_not_overlap() {
    let d = test_tmpdir().unwrap();
    // every sub-agent waits at a gate the test opens one permit at a time: no count of yields decides who is at work
    let client = Mock::new(vec![("P", Say::Gated("ok".into()))]);
    let toml = r#"
max_parallel = 2
[[phase]]
title = "One"
  [[phase.step]]
  id = "s1"
  prompt = "P1 a"
  [[phase.step]]
  id = "s2"
  prompt = "P1 b"
  [[phase.step]]
  id = "s3"
  prompt = "P1 c"
  [[phase.step]]
  id = "s4"
  prompt = "P1 d"
  [[phase.step]]
  id = "s5"
  prompt = "P1 e"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "last"
  prompt = "P2 end"
"#;
    let c = ctx(d.path());
    let wf = tool(&client, d.path());
    let driver = async {
        client.until_started(2).await;
        // the runner has had every chance to start a third by now
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(client.st.started.load(Ordering::SeqCst), 2, "a third began while two were at work");
        for n in 1..=6 {
            client.st.gate.add_permits(1);
            client.until_ended(n).await;
        }
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": toml}), &c), driver);
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.tasks().len(), 6);
    assert_eq!(client.st.peak.load(Ordering::SeqCst), 2, "two at work, never three");
    let log = client.st.log.lock().unwrap().clone();
    let last_p1_end = log.iter().rposition(|(start, t)| !start && t.contains("P1")).unwrap();
    let first_p2_start = log.iter().position(|(start, t)| *start && t.contains("P2")).unwrap();
    assert!(last_p1_end < first_p2_start, "phase two began while phase one still worked: {log:?}");
    // each sub-agent is in the registry under its label, ended with a report
    let list = c.jobs.subs().list();
    assert_eq!(list.len(), 6);
    assert!(list.iter().all(|i| i.state == SubState::Ok), "{list:?}");
    let mut titles: Vec<String> = list.iter().map(|i| i.title.clone()).collect();
    titles.sort();
    assert_eq!(titles[0], "inline · One · s1");
    assert_eq!(titles[5], "inline · Two · last");
    assert!(out.content.contains("== last =="), "{}", out.content);
}

/// `file` with the `answers` of a vote step.
fn answering(file: &str) -> String {
    file.replace("votes = 2", "votes = 2\n  answers = [\"REAL\", \"NOT REAL\"]")
}

const CHECKED: &str = r#"
[[phase]]
title = "Find"
  [[phase.step]]
  id = "find"
  prompt = "FIND in {{args}}"
  returns = "list"
[[phase]]
title = "Check"
  [[phase.step]]
  id = "check"
  each = "find"
  votes = 2
  prompt = "CHECK {{item}}"
"#;

// ---- the question, through the agent loop ----

fn config(mode: PermissionMode) -> Config {
    let mut cfg = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    cfg.providers.insert("mock".into(), p);
    cfg.permissions.mode = mode;
    cfg.context.policy = "plain".into();
    cfg.agent.attach_budget = 0;
    cfg.agent.check_at_start = false;
    cfg
}

struct Running {
    handle: AgentHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Running {
    fn send(&self, c: AgentCommand) {
        self.handle.commands.send(c).unwrap();
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

    /// Events until both of two have come, in either order.
    async fn until_both(
        &mut self,
        a: impl Fn(&AgentEvent) -> bool,
        b: impl Fn(&AgentEvent) -> bool,
    ) -> Vec<AgentEvent> {
        let (mut got_a, mut got_b) = (false, false);
        let mut out = Vec::new();
        while !(got_a && got_b) {
            let e = self.next().await;
            got_a |= a(&e);
            got_b |= b(&e);
            out.push(e);
        }
        out
    }

    async fn turn(&mut self) -> Vec<AgentEvent> {
        self.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await
    }

    async fn ask(&mut self) -> nexo_core::permission::PermissionRequest {
        let evs = self.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
        match evs.last().unwrap() {
            AgentEvent::PermissionAsk { request } => request.clone(),
            _ => unreachable!(),
        }
    }

    fn submit(&self, t: &str) {
        self.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
    }
}

fn start(client: &Arc<Mock>, d: &Path, mode: PermissionMode, tools: ToolSet) -> Running {
    start_with(client, d, config(mode), tools)
}

fn start_with(client: &Arc<Mock>, d: &Path, config: Config, tools: ToolSet) -> Running {
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config,
        cwd: d.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools,
        client: client.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    Running { handle, task: tokio::spawn(nexo_agent::run(ends, setup)) }
}

/// A trusted project folder with the workflow `name` saved in it.
fn project_with(name: &str, text: &str) -> tempfile::TempDir {
    data_dir();
    let d = test_tmpdir().unwrap();
    nexo_core::config::trust_project(d.path(), &nexo_core::paths::trusted_projects_file()).unwrap();
    std::fs::create_dir_all(d.path().join(".nexo/workflows")).unwrap();
    std::fs::write(d.path().join(format!(".nexo/workflows/{name}.toml")), text).unwrap();
    d
}

// ---- writers, failures, modes ----

/// A phase of four steps of the sub-agent kind `agent`, each held at the gate.
fn four(agent: &str) -> String {
    let steps: String = (1..=4)
        .map(|n| format!("  [[phase.step]]\n  id = \"s{n}\"\n  agent = \"{agent}\"\n  prompt = \"GATE {n}\"\n"))
        .collect();
    format!("[[phase]]\ntitle = \"Only\"\n{steps}")
}

/// Time the runner gets to start one more sub-agent that it should not start.
async fn grace() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

// ---- live state, stops, a run started by name ----

type Changes = tokio::sync::broadcast::Receiver<SubChange>;

/// The workflow snapshots the registry has sent so far.
fn flow_events(rx: &mut Changes) -> Vec<FlowInfo> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(SubChange::Flow(i)) => out.push(i),
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return out,
        }
    }
}

/// Waits for a snapshot that `f` accepts.
async fn until_flow(rx: &mut Changes, f: impl Fn(&FlowInfo) -> bool) -> FlowInfo {
    loop {
        match tokio::time::timeout(PATIENCE, rx.recv()).await.expect("a snapshot in time") {
            Ok(SubChange::Flow(i)) if f(&i) => return i,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("the registry is gone"),
            _ => {}
        }
    }
}

fn fstep<'a>(i: &'a FlowInfo, id: &str) -> &'a nexo_core::subs::FlowStep {
    i.phases.iter().flat_map(|p| &p.steps).find(|s| s.id == id).unwrap_or_else(|| panic!("no step {id}"))
}

/// The states a step went through, a repeat of the one before left out.
fn path_of(seen: &[FlowInfo], id: &str) -> Vec<StepState> {
    let mut out: Vec<StepState> = Vec::new();
    for i in seen {
        let s = fstep(i, id).state;
        if out.last() != Some(&s) {
            out.push(s);
        }
    }
    out
}

fn running(i: &FlowInfo, id: &str) -> bool {
    fstep(i, id).state == StepState::Running
}

const THREE: &str = r#"
[[phase]]
title = "Look"
  [[phase.step]]
  id = "a"
  prompt = "ONE"
  [[phase.step]]
  id = "b"
  prompt = "TWO"
[[phase]]
title = "Join"
  [[phase.step]]
  id = "c"
  prompt = "THREE {{steps.a.report}}"
"#;

fn files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// A saved workflow with one step that holds at the gate.
const BYNAME: &str = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"GATE {{args}}\"\n";

fn notice_has(e: &AgentEvent, word: &str) -> bool {
    matches!(e, AgentEvent::Notice { text, .. } if text.contains(word))
}

/// Every event that is already on its way, until the agent is quiet.
async fn drain(r: &mut Running) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(Some(e)) = tokio::time::timeout(Duration::from_millis(150), r.handle.events.recv()).await {
        out.push(e);
    }
    out
}

fn flows_in(evs: &[AgentEvent]) -> Vec<&FlowInfo> {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::FlowRun { info } => Some(info),
            _ => None,
        })
        .collect()
}

/// A gate that holds each question until the test answers it, whatever becomes of the conversation meanwhile.
struct HeldGate {
    asked: tokio::sync::mpsc::UnboundedSender<nexo_core::permission::PermissionRequest>,
    answers: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PermissionDecision>>,
}

#[async_trait]
impl nexo_core::permission::PermissionGate for HeldGate {
    async fn ask(&self, req: nexo_core::permission::PermissionRequest) -> PermissionDecision {
        let _ = self.asked.send(req);
        self.answers.lock().await.recv().await.unwrap_or(PermissionDecision::Deny { reason: None })
    }
}

/// A saved workflow whose one step ends at once.
#[cfg(unix)]
fn quick() -> String {
    BYNAME.replace("GATE", "DONE")
}

#[cfg(unix)]
fn with_hook(event: &str, matcher: &str, command: &str) -> Config {
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(nexo_core::config::HookConfig {
        event: event.into(),
        matcher: Some(matcher.into()),
        command: command.into(),
        ..Default::default()
    });
    cfg
}

async fn stop_agent(mut r: Running) {
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

/// How the conversation is replaced in the tests that follow.
#[derive(Clone, Copy, Debug)]
enum Replace {
    Clear,
    Fork,
    Resume,
}

/// The session the agent starts in (one turn done, so it is recorded) and the agent.
async fn recorded(client: &Arc<Mock>, d: &Path) -> (Running, String) {
    let mut r = start(client, d, PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    let sid = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        e => panic!("{e:?}"),
    };
    r.submit("hello");
    r.turn().await;
    (r, sid)
}

/// Replaces the conversation the way `how` says; `first` is the session recorded at the start.
async fn replace(r: &mut Running, how: Replace, first: &str) -> Vec<AgentEvent> {
    r.send(match how {
        Replace::Clear => AgentCommand::Clear,
        Replace::Fork => AgentCommand::Fork { index: None },
        Replace::Resume => AgentCommand::Resume { id: first.into() },
    });
    r.until(|e| matches!(e, AgentEvent::SessionChanged { .. })).await
}

const EACH_STOP: &str = r#"
max_parallel = 1
[[phase]]
title = "Find"
  [[phase.step]]
  id = "find"
  prompt = "FIND"
  returns = "list"
[[phase]]
title = "Check"
  [[phase.step]]
  id = "check"
  each = "find"
  prompt = "CHECK {{item}}"
"#;

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
