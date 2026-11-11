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

fn data_dir() {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_CONFIG_DIR", d.path());
    std::env::set_var("NEXO_CACHE_DIR", d.path().join("cache"));
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
    let d = tempfile::tempdir().unwrap();
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

#[tokio::test]
async fn three_items_with_two_votes_start_six_sub_agents_each_with_its_item_and_the_count_line() {
    let d = tempfile::tempdir().unwrap();
    let checked = answering(CHECKED);
    let client = Mock::new(vec![
        ("FIND", says("Three.\n[\"alpha\", \"beta\", \"gamma\"]")),
        ("CHECK alpha", says("REAL: it is there")),
        ("CHECK beta", says("NOT REAL: it is fine")),
        ("CHECK gamma", says("Hmm, who knows")),
    ]);
    let (out, c) = go(&client, d.path(), &checked, "src").await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.tasks().len(), 7);
    for item in ["alpha", "beta", "gamma"] {
        assert_eq!(client.count(&format!("CHECK {item}")), 2, "{item}");
    }
    // the closing line for a list is added to the step that returns one, not to the others
    assert!(client.tasks()[0].contains("JSON array of strings"));
    assert!(!client.tasks().iter().any(|t| t.starts_with("CHECK") && t.contains("JSON array")));
    assert!(client.tasks()[0].contains("FIND in src"));
    // the count line only where every copy began with one of the answers
    assert!(out.content.contains("=> item 1: REAL 2 of 2"), "{}", out.content);
    assert!(out.content.contains("=> item 2: NOT REAL 2 of 2"), "{}", out.content);
    assert!(!out.content.contains("item 3: HMM") && !out.content.contains("=> item 3"), "{}", out.content);
    assert!(out.content.contains("--- check #1 (item 1: alpha) (copy 1) ---"), "{}", out.content);
    let titles: Vec<String> = c.jobs.subs().list().into_iter().map(|i| i.title).collect();
    assert!(titles.contains(&"inline · Check · check #6".to_string()), "{titles:?}");
    assert!(titles.contains(&"inline · Find · find".to_string()), "{titles:?}");
}

#[tokio::test]
async fn the_count_line_needs_every_copy_to_begin_with_one_of_the_answers() {
    let d = tempfile::tempdir().unwrap();
    let one = answering(CHECKED).replace("votes = 2", "votes = 3");
    // two answers among three copies: the count is said for both
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL yes".into(), "not real, sorry".into(), "Real again".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(out.content.contains("=> item 1: REAL 2 of 3, NOT REAL 1 of 3"), "{}", out.content);
    // one copy gave none of the answers: just the reports
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL".into(), "FAKE".into(), "REAL".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(!out.content.contains("=>"), "{}", out.content);
    assert!(out.content.contains("--- check #3 (item 1: a) (copy 3) ---"), "{}", out.content);
    // without `answers` a first word that is the same is no reason to count
    let client = Mock::new(vec![("FIND", says("[\"a\"]")), ("CHECK", says("REAL, I think"))]);
    let (out, _) = go(&client, d.path(), &CHECKED.replace("votes = 2", "votes = 3"), "").await;
    assert!(!out.content.contains("=>"), "{}", out.content);
}

#[tokio::test]
async fn a_report_with_no_array_fails_the_step_and_skips_what_needs_it_but_not_the_rest_of_the_phase() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says("I found nothing to list")), ("SIDE", says("side report"))]);
    let toml = r#"
[[phase]]
title = "Find"
  [[phase.step]]
  id = "find"
  prompt = "FIND"
  returns = "list"
  [[phase.step]]
  id = "side"
  prompt = "SIDE"
[[phase]]
title = "Check"
  [[phase.step]]
  id = "check"
  each = "find"
  prompt = "CHECK {{item}}"
  [[phase.step]]
  id = "uses"
  prompt = "USES {{steps.find.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE"
"#;
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "find").contains("failed: the report has no JSON array of strings"), "{}", out.content);
    assert!(steps_line(&out, "side").contains(": ok"));
    assert!(steps_line(&out, "check").contains("skipped: step `find` failed"), "{}", out.content);
    assert!(steps_line(&out, "uses").contains("skipped: step `find` failed"));
    assert!(steps_line(&out, "free").contains(": ok"));
    assert_eq!(client.count("CHECK"), 0);
    assert_eq!(client.count("USES"), 0);
    assert_eq!(client.count("FREE"), 1);
}

#[tokio::test]
async fn a_phase_where_every_step_failed_ends_the_run() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[1, 2, 3]"))]);
    let toml = format!("{CHECKED}\n[[phase]]\ntitle = \"Never\"\n[[phase.step]]\nid = \"never\"\nprompt = \"NEVER\"\n");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    // the only usable thing would be a report: there is none
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("Ended early: every step of phase \"Find\" failed"), "{}", out.content);
    assert!(steps_line(&out, "find").contains("not one of strings"));
    assert!(steps_line(&out, "never").contains("not run"));
    assert_eq!(client.tasks().len(), 1);
}

#[tokio::test]
async fn text_that_arrives_through_a_placeholder_is_never_expanded_again() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"{{args}} and {{steps.find.report}}\"]")), ("CHECK", says("fine"))]);
    let (out, _) = go(&client, d.path(), CHECKED, "THE-ARGS").await;
    assert!(!out.is_error, "{}", out.content);
    let check = client.tasks().into_iter().find(|t| t.starts_with("CHECK")).unwrap();
    assert!(check.contains("{{args}} and {{steps.find.report}}"), "{check}");
    assert!(!check.contains("THE-ARGS"), "{check}");
}

#[tokio::test]
async fn max_agents_stops_new_starts_and_is_named() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"a\", \"b\", \"c\", \"d\", \"e\"]"))]);
    let toml = format!("max_agents = 3\nmax_parallel = 1\n{CHECKED}\n[[phase]]\ntitle = \"After\"\n[[phase.step]]\nid = \"after\"\nprompt = \"AFTER\"\n");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(client.tasks().len(), 3, "{}", out.content);
    assert!(out.content.contains("Stopped: max_agents (3) was reached"), "{}", out.content);
    assert!(steps_line(&out, "check").contains("partial, 8 without a report"), "{}", out.content);
    assert!(steps_line(&out, "after").contains("not run: max_agents (3) was reached"), "{}", out.content);
    assert_eq!(client.count("AFTER"), 0);
}

#[tokio::test]
async fn max_tokens_stops_new_starts_and_is_named() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![
        ("FIND", Say::Text("[\"a\", \"b\", \"c\"]".into(), 500, 100)),
        ("CHECK", Say::Text("fine".into(), 500, 100)),
    ]);
    let toml = format!("max_tokens = 1000\nmax_parallel = 1\n{CHECKED}").replace("votes = 2", "votes = 1");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    // 600 spent by `find`, 1200 after the first check: the second check does not start
    assert_eq!(client.tasks().len(), 2, "{}", out.content);
    assert!(out.content.contains("Stopped: max_tokens (1000) was reached"), "{}", out.content);
    assert!(out.content.contains("1200 tokens"), "{}", out.content);
    assert!(steps_line(&out, "check").contains("partial, 2 without a report"), "{}", out.content);
}

#[tokio::test]
async fn the_interrupt_stops_the_whole_run_and_nothing_starts_after_it() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let toml = r#"
max_parallel = 2
[[phase]]
title = "One"
  [[phase.step]]
  id = "a"
  prompt = "HANG a"
  [[phase.step]]
  id = "b"
  prompt = "HANG b"
  [[phase.step]]
  id = "c"
  prompt = "HANG c"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "z"
  prompt = "AFTER"
"#;
    let c = ctx(d.path());
    let wf = tool(&client, d.path());
    let stopper = async {
        client.until_started(2).await;
        let at_cancel = client.tasks().len();
        c.cancel.cancel();
        at_cancel
    };
    let (out, at_cancel) = tokio::join!(wf.run(json!({"inline": toml}), &c), stopper);
    assert_eq!(at_cancel, 2);
    assert_eq!(client.tasks().len(), 2, "a call was made after the interrupt");
    assert!(out.content.contains("Interrupted by the person"), "{}", out.content);
    assert!(steps_line(&out, "c").contains("not run"), "{}", out.content);
    assert!(steps_line(&out, "z").contains("not run"), "{}", out.content);
    let list = c.jobs.subs().list();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|i| i.state == SubState::Killed), "{list:?}");
    assert_eq!(c.jobs.running(), 0);
}

#[tokio::test]
async fn a_file_that_fails_a_check_or_names_an_unknown_kind_starts_nothing() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![]);
    let bad = "[[phase]]\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\nvotes = 9\nagent = \"nobody\"\n";
    let (out, _) = go(&client, d.path(), bad, "").await;
    assert!(
        out.is_error && out.content.contains("nothing was started") && out.content.contains("line"),
        "{}",
        out.content
    );
    assert!(out.content.contains("`votes` is 9"), "{}", out.content);
    let kind = "[[phase]]\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\nagent = \"nobody\"\n";
    let (out, _) = go(&client, d.path(), kind, "").await;
    assert!(out.is_error && out.content.contains("no sub-agent kind `nobody`"), "{}", out.content);
    assert!(out.content.contains("explore"), "the kinds there are are listed: {}", out.content);
    let wf = tool(&client, d.path());
    let c = ctx(d.path());
    for args in [json!({}), json!({"name": "ghost"}), json!({"name": "a", "inline": "b"})] {
        assert!(wf.run(args, &c).await.is_error);
    }
    assert!(client.tasks().is_empty());
}

#[tokio::test]
async fn the_answer_is_bounded_per_report_and_every_report_keeps_its_share_of_the_total() {
    let d = tempfile::tempdir().unwrap();
    let big = "x".repeat(7000);
    let client = Mock::new(vec![("BIG", says(&big))]);
    // one report: 6000 characters, and the line says how much was cut
    let one = "[[phase]]\ntitle = \"Only\"\n[[phase.step]]\nid = \"s1\"\nprompt = \"BIG 1\"\n";
    let (out, _) = go(&client, d.path(), one, "").await;
    assert_eq!(out.content.matches('x').count(), 6000);
    assert!(out.content.contains("[cut: 1000 more characters]"), "{}", out.content);
    // five of them: each has a fifth of 24000, so none comes first and none vanishes
    let steps: String = (1..=5).map(|n| format!("[[phase.step]]\nid = \"s{n}\"\nprompt = \"BIG {n}\"\n")).collect();
    let toml = format!("[[phase]]\ntitle = \"Only\"\n{steps}");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(
        out.content.matches('x').count(),
        24000,
        "{}",
        out.content.lines().filter(|l| !l.starts_with("xxx")).collect::<Vec<_>>().join("|")
    );
    assert_eq!(out.content.matches("[cut: 2200 more characters]").count(), 5);
    for n in 1..=5 {
        assert!(out.content.contains(&format!("== s{n} ==")), "s{n} vanished");
    }
    assert!(
        !out.content.contains("left out"),
        "{}",
        out.content.lines().filter(|l| !l.starts_with("xxx")).collect::<Vec<_>>().join("|")
    );
}

#[tokio::test]
async fn in_the_answer_every_vote_keeps_its_header_and_its_count_line_even_with_no_text_left() {
    let d = tempfile::tempdir().unwrap();
    // 5 items with 5 votes: 25 reports share 24000, 960 characters each
    let items = (1..=5).map(|n| format!("\"it{n}\"")).collect::<Vec<_>>().join(", ");
    let client =
        Mock::new(vec![("FIND", says(&format!("[{items}]"))), ("CHECK", says(&format!("REAL {}", "x".repeat(2000))))]);
    let toml = answering(CHECKED).replace("votes = 2", "votes = 5");
    let toml = toml.replace("[[phase]]\ntitle = \"Find\"", "max_agents = 50\n[[phase]]\ntitle = \"Find\"");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert_eq!(out.content.matches("--- check #").count(), 25, "{}", out.content);
    assert_eq!(out.content.matches("=> item ").count(), 5);
    assert!(out.content.contains("=> item 5: REAL 5 of 5"), "{}", out.content);
    assert!(out.content.contains("--- check #25 (item 5: it5) (copy 5) ---"), "{}", out.content);
    assert!(out.content.matches('x').count() <= 24000);
}

#[tokio::test]
async fn a_workflow_cannot_start_a_workflow_and_a_sub_agent_is_not_offered_the_tool() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![]);
    let set = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("workflow")));
    let wf = Workflow::new(Arc::new(sub_agent(&client, set)), d.path().to_path_buf());
    let out = wf
        .run(json!({"inline": "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\nagent=\"general\""}), &ctx(d.path()))
        .await;
    assert!(!out.is_error, "{}", out.content);
    let seen = client.st.tools_seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains(&"read".to_string()), "{seen:?}");
    assert!(!seen[0].contains(&"workflow".to_string()) && !seen[0].contains(&"task".to_string()), "{seen:?}");
}

#[tokio::test]
async fn the_description_lists_the_saved_workflows_bounded_and_the_question_says_what_it_costs() {
    data_dir();
    let d = tempfile::tempdir().unwrap();
    nexo_core::config::trust_project(d.path(), &nexo_core::paths::trusted_projects_file()).unwrap();
    let dir = d.path().join(".nexo/workflows");
    std::fs::create_dir_all(&dir).unwrap();
    let body = |desc: &str| {
        format!("description = \"{desc}\"\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\n")
    };
    for n in 0..25 {
        std::fs::write(dir.join(format!("w{n:02}.toml")), body(&"long ".repeat(40))).unwrap();
    }
    std::fs::write(dir.join("w00.toml"), body("the first one")).unwrap();
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    let spec = wf.spec();
    assert!(spec.description.contains("- w00: the first one"), "{}", spec.description);
    assert!(spec.description.contains("- w19:"), "{}", spec.description);
    assert!(!spec.description.contains("- w20"), "{}", spec.description);
    assert!(spec.description.contains("and 5 more"));
    let line = spec.description.lines().find(|l| l.starts_with("- w01")).unwrap();
    assert!(line.chars().count() <= 80 + 6, "{line}");
    assert_eq!(wf.kind(), ToolKind::Other);
    // the question
    let summary = wf.summary(&json!({"name": "w00"}));
    assert_eq!(summary, "workflow `w00`: up to 1 sub-agents (at least 1), 400000 tokens at most");
    let (s, detail) = nexo_agent::workflow::tool::question(&json!({"inline": CHECKED}), d.path());
    assert_eq!(s, "inline workflow: up to 12 sub-agents (at least 1), 400000 tokens at most");
    assert_eq!(detail, vec!["Find: find".to_string(), "Check: check (each of find, 2 votes)".to_string()]);
    // more than 12 phases: 12 lines, then how many more
    let phases: String = std::iter::once("max_agents = 20\n".to_string())
        .chain(
            (1..=15).map(|n| format!("[[phase]]\ntitle = \"P{n}\"\n[[phase.step]]\nid = \"s{n}\"\nprompt = \"p\"\n")),
        )
        .collect();
    let (_, detail) = nexo_agent::workflow::tool::question(&json!({"inline": phases}), d.path());
    assert_eq!(detail.len(), 13, "{detail:?}");
    assert_eq!(detail[11], "P12: s12");
    assert_eq!(detail[12], "+3 more");
}

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
    let d = tempfile::tempdir().unwrap();
    nexo_core::config::trust_project(d.path(), &nexo_core::paths::trusted_projects_file()).unwrap();
    std::fs::create_dir_all(d.path().join(".nexo/workflows")).unwrap();
    std::fs::write(d.path().join(format!(".nexo/workflows/{name}.toml")), text).unwrap();
    d
}

#[tokio::test]
async fn leave_is_asked_once_a_denial_starts_nothing_and_always_allow_holds_for_one_file_as_it_is() {
    let saved = "description = \"two reads\"\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"SAVED a\"\n[[phase.step]]\nid = \"b\"\nprompt = \"SAVED b\"\n";
    let d = project_with("saved", saved);
    let inline = "[[phase]]\ntitle = \"I\"\n[[phase.step]]\nid = \"i\"\nprompt = \"INLINE i\"\n";
    let client = Mock::with_main(
        vec![],
        vec![
            call("w1", "workflow", json!({"name": "saved"})),
            text_events("understood", 1, 1),
            call("w2", "workflow", json!({"name": "saved"})),
            text_events("ran", 1, 1),
            call("w3", "workflow", json!({"name": "saved"})),
            text_events("ran again", 1, 1),
            call("w5", "workflow", json!({"name": "saved"})),
            text_events("changed", 1, 1),
            call("w4", "workflow", json!({"inline": inline})),
            text_events("denied", 1, 1),
        ],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;

    // 1: asked, denied: nothing starts
    r.submit("run it");
    let req = r.ask().await;
    assert_eq!((req.tool.as_str(), req.kind), ("workflow", ToolKind::Other));
    assert_eq!(req.summary, "workflow `saved`: up to 2 sub-agents (at least 2), 400000 tokens at most");
    // the rule that is offered holds the name and the start of the hash of the file
    let rule = format!("workflow({})", subject("saved", saved));
    assert_eq!(req.suggested_rules, vec![rule.clone()]);
    assert!(rule.len() == "workflow(saved@".len() + 32 + 1, "the pin is 32 hex: {rule}");
    r.send(AgentCommand::Permission {
        id: "w1".into(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { .. })), "a denied tool never starts");
    assert!(client.tasks().is_empty(), "{:?}", client.tasks());

    // 2: allowed always with the rule it offered: one question, two sub-agents
    r.submit("again");
    r.ask().await;
    r.send(AgentCommand::Permission {
        id: "w2".into(),
        decision: PermissionDecision::AllowAlways { rule: rule.clone(), persist: false },
    });
    let evs = r.turn().await;
    assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::PermissionAsk { .. })).count(), 0);
    assert_eq!(client.tasks().len(), 2);

    // 3: the rule covers it: no question at all
    r.submit("and again");
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "{evs:?}");
    assert_eq!(client.tasks().len(), 4);

    // 3b: the file changed: the rule was for the other one, so it is asked again, with a rule of its own
    let changed = saved.replace("SAVED b", "SAVED b, and now more");
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), &changed).unwrap();
    r.submit("after the change");
    let req = r.ask().await;
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("saved", &changed))]);
    assert_ne!(req.suggested_rules[0], rule);
    r.send(AgentCommand::Permission { id: "w5".into(), decision: PermissionDecision::Deny { reason: None } });
    r.turn().await;
    assert_eq!(client.tasks().len(), 4, "a changed file started on the old leave");

    // 4: text the model wrote is asked every time, and says its phases and steps
    r.submit("write one");
    let req = r.ask().await;
    assert!(req.summary.starts_with("inline workflow: up to 1 sub-agents"), "{}", req.summary);
    assert!(req.suggested_rules.is_empty(), "{:?}", req.suggested_rules);
    assert!(
        matches!(&req.detail, nexo_core::tool::ToolDisplay::List { items } if items == &vec!["I: i".to_string()]),
        "{:?}",
        req.detail
    );
    r.send(AgentCommand::Permission { id: "w4".into(), decision: PermissionDecision::Deny { reason: None } });
    r.turn().await;
    assert_eq!(client.tasks().len(), 4);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
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

#[tokio::test]
async fn sub_agents_that_can_change_files_run_one_at_a_time_as_with_the_task_tool() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("done".into()))]);
    let wf = writer_tool(&client, d.path(), false);
    let c = ctx(d.path());
    let driver = async {
        for n in 1..=4 {
            client.until_started(n).await;
            grace().await;
            assert_eq!(client.st.started.load(Ordering::SeqCst), n, "a writer began while another was at work");
            client.st.gate.add_permits(1);
            client.until_ended(n).await;
        }
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": four("general")}), &c), driver);
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.st.peak.load(Ordering::SeqCst), 1, "{}", out.content);
    assert_eq!(client.tasks().len(), 4);
}

#[tokio::test]
async fn read_only_kinds_run_together_and_so_do_writers_when_the_setting_lets_task_sub_agents_write_side_by_side() {
    for (agent, parallel) in [("explore", false), ("general", true)] {
        let d = tempfile::tempdir().unwrap();
        let client = Mock::new(vec![("GATE", Say::Gated("done".into()))]);
        let wf = writer_tool(&client, d.path(), parallel);
        let c = ctx(d.path());
        let driver = async {
            // all four at work at once, with the gate shut
            client.until_started(4).await;
            assert_eq!(client.st.active.load(Ordering::SeqCst), 4, "{agent} {parallel}");
            client.st.gate.add_permits(4);
            client.until_ended(4).await;
        };
        let (out, ()) = tokio::join!(wf.run(json!({"inline": four(agent)}), &c), driver);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(client.st.peak.load(Ordering::SeqCst), 4, "{agent} {parallel}");
    }
}

#[tokio::test]
async fn an_error_a_cut_and_a_refused_start_each_fail_their_own_step_and_skip_what_needs_it() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![
        ("BOOM", Say::Fail),
        ("CUT", Say::Hang),
        ("FINE", says("all good")),
        ("FREE", says("free report")),
        ("NEEDSFINE", says("used the good one")),
    ]);
    // a kind that works in a worktree of its own, in a folder that is no repository: it cannot be started
    let mut profiles = nexo_agent::subagent::builtin();
    profiles.push(nexo_agent::subagent::Profile {
        name: "iso".into(),
        description: "works in a worktree".into(),
        tools: None,
        read_only: false,
        model: None,
        prompt: "Work.".into(),
        rounds: 4,
        source: None,
        soft_model: false,
        isolation: true,
    });
    let set = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
    let wf = Workflow::new(Arc::new(sub_agent_with(&client, set, profiles)), d.path().to_path_buf());
    let toml = r#"
[[phase]]
title = "Work"
  [[phase.step]]
  id = "boom"
  prompt = "BOOM"
  [[phase.step]]
  id = "cutoff"
  prompt = "CUT"
  [[phase.step]]
  id = "refused"
  agent = "iso"
  prompt = "ISO"
  [[phase.step]]
  id = "fine"
  prompt = "FINE"
[[phase]]
title = "After"
  [[phase.step]]
  id = "needs_boom"
  prompt = "NEEDS {{steps.boom.report}}"
  [[phase.step]]
  id = "needs_cut"
  prompt = "NEEDS {{steps.cutoff.report}}"
  [[phase.step]]
  id = "needs_refused"
  prompt = "NEEDS {{steps.refused.report}}"
  [[phase.step]]
  id = "needs_fine"
  prompt = "NEEDSFINE {{steps.fine.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE"
"#;
    let c = ctx(d.path());
    let cutter = async {
        client.until_started(2).await;
        // the person stops that one sub-agent from the job list: the run goes on
        let job = loop {
            if let Some(j) = c.jobs.list().into_iter().find(|j| j.label.contains("cutoff")) {
                break j;
            }
            tokio::task::yield_now().await;
        };
        assert!(c.jobs.kill(job.id));
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": toml}), &c), cutter);
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "boom").contains("failed: the sub-agent's model call failed"), "{}", out.content);
    assert!(steps_line(&out, "cutoff").contains("failed: the sub-agent was stopped"), "{}", out.content);
    assert!(steps_line(&out, "refused").contains("failed: could not make the sub-agent a worktree"), "{}", out.content);
    assert!(steps_line(&out, "fine").contains(": ok"), "{}", out.content);
    for (id, by) in [("needs_boom", "boom"), ("needs_cut", "cutoff"), ("needs_refused", "refused")] {
        assert!(steps_line(&out, id).contains(&format!("skipped: step `{by}` failed")), "{}", out.content);
    }
    assert!(steps_line(&out, "needs_fine").contains(": ok") && steps_line(&out, "free").contains(": ok"));
    assert_eq!(client.count("NEEDS "), 0);
    assert_eq!(client.count("NEEDSFINE"), 1);
    assert_eq!(client.count("FREE"), 1);
    assert_eq!(client.count("ISO"), 0, "the refused one never ran");
}

#[tokio::test]
async fn a_tool_a_sub_agent_is_refused_goes_back_to_it_and_does_not_by_itself_fail_its_step() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("DENY", Say::CallThen("edit", "the edit was refused, nothing changed".into()))]);
    let wf = writer_tool(&client, d.path(), false);
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"deny\"\nagent = \"general\"\nprompt = \"DENY\"\n";
    let out = wf.run(json!({"inline": toml}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "deny").contains(": ok"), "{}", out.content);
    assert!(out.content.contains("the edit was refused, nothing changed"), "{}", out.content);
    let log = client.st.log.lock().unwrap().clone();
    let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("the sub-agent got a result back");
    assert!(result.1.contains("Permission denied"), "{}", result.1);
}

#[tokio::test]
async fn read_only_and_plan_mode_ask_about_a_workflow_as_ever_and_the_mode_refuses_what_a_sub_agent_changes() {
    for mode in [PermissionMode::ReadOnly, PermissionMode::Plan] {
        let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"look\"\nprompt = \"LOOK\"\n[[phase.step]]\nid = \"change\"\nagent = \"general\"\nprompt = \"CHANGE\"\n";
        let d = project_with("saved", text);
        let client = Mock::with_main(
            vec![("LOOK", says("looked")), ("CHANGE", Say::CallThen("edit", "the mode said no".into()))],
            vec![call("w1", "workflow", json!({"name": "saved"})), text_events("done", 1, 1)],
        );
        let tools = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
        let mut r = start(&client, d.path(), mode, tools);
        r.next().await;
        r.submit("run it");
        // asked, not denied by the mode (a workflow is judged call by call inside)
        let req = r.ask().await;
        assert_eq!(req.tool, "workflow", "{mode:?}");
        r.send(AgentCommand::Permission { id: "w1".into(), decision: PermissionDecision::Allow });
        let evs = r.turn().await;
        assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "{mode:?}: asked twice");
        // the read ran; the sub-agent that tried to change something was refused by the mode
        assert_eq!(client.count("LOOK"), 1, "{mode:?}");
        assert_eq!(client.count("CHANGE"), 1, "{mode:?}");
        let log = client.st.log.lock().unwrap().clone();
        let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("a result came back");
        assert!(result.1.contains("Permission denied") && result.1.contains(mode.as_str()), "{mode:?}: {}", result.1);
        r.send(AgentCommand::Shutdown);
        r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        r.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn a_run_that_reports_no_usage_counts_as_zero_tokens_and_the_result_says_so() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("BARE", Say::Bare("no meter".into())), ("METERED", says("metered"))]);
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"bare\"\nprompt = \"BARE\"\n[[phase.step]]\nid = \"metered\"\nprompt = \"METERED\"\n";
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "bare").contains("1 sub-agent, 0 tokens"), "{}", out.content);
    assert!(steps_line(&out, "metered").contains("1 sub-agent, 15 tokens"), "{}", out.content);
    assert!(
        out.content.contains("note: token use was not reported for 1 sub-agent; it is counted as 0 tokens"),
        "{}",
        out.content
    );
    assert_eq!(out.content.matches("token use was not reported").count(), 1, "only the one that did not report");
}

#[tokio::test]
async fn a_list_cut_at_fifty_items_and_equal_items_are_said_in_the_result() {
    let d = tempfile::tempdir().unwrap();
    let many = (0..60).map(|n| format!("\"i{n}\"")).collect::<Vec<_>>().join(", ");
    let client = Mock::new(vec![("FIND", says(&format!("[{many}]")))]);
    let toml = format!(
        "max_agents = 50\nmax_parallel = 8\n{}",
        CHECKED.replace("votes = 2", "votes = 1").replace("{{item}}", "{{item}}!")
    );
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(
        out.content.contains("note: it returned 60 items; the first 50 are used and the rest are left out"),
        "{}",
        out.content
    );
    // 50 items were used (49 could start under max_agents), not 60
    assert!(steps_line(&out, "check").contains("partial, 1 without a report"), "{}", out.content);
    assert_eq!(client.count("CHECK i50!"), 0);
    assert_eq!(client.count("CHECK i48!"), 1);

    let client = Mock::new(vec![("FIND", says("[\"a\", \"a\", \"b\", \"a\"]"))]);
    let toml = CHECKED.replace("votes = 2", "votes = 1").replace("{{item}}", "{{item}}!");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(out.content.contains("note: 2 equal items merged"), "{}", out.content);
    assert_eq!(client.count("CHECK a!"), 1);
    assert_eq!(client.count("CHECK b!"), 1);
}

#[tokio::test]
async fn what_a_later_prompt_gets_of_a_step_with_each_is_the_joined_reports_under_one_line_headers_and_capped() {
    let d = tempfile::tempdir().unwrap();
    let big = "y".repeat(7000);
    let client = Mock::new(vec![
        ("FIND", says("[\"alpha\", \"beta\"]")),
        ("CHECK alpha", says("report alpha")),
        ("CHECK beta", says(&big)),
    ]);
    let toml = format!(
        "{}\n[[phase]]\ntitle = \"Sum\"\n[[phase.step]]\nid = \"sum\"\nprompt = \"SUM {{{{steps.check.report}}}}\"\n",
        CHECKED.replace("votes = 2", "votes = 1")
    );
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    let sum = client.tasks().into_iter().find(|t| t.starts_with("SUM")).expect("the last step ran");
    let want = format!(
        "SUM --- check #1 (item 1: alpha) ---\nreport alpha\n\n--- check #2 (item 2: beta) ---\n{}\n[cut: 1000 more characters]",
        "y".repeat(6000)
    );
    assert_eq!(sum, want);
}

#[tokio::test]
async fn an_item_longer_than_two_thousand_characters_is_cut_with_a_mark_in_the_prompt() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says(&format!("[\"{}\"]", "z".repeat(3000)))), ("CHECK", says("fine"))]);
    let (out, _) = go(&client, d.path(), &CHECKED.replace("votes = 2", "votes = 1"), "").await;
    assert!(!out.is_error, "{}", out.content);
    let check = client.tasks().into_iter().find(|t| t.starts_with("CHECK")).unwrap();
    assert_eq!(check, format!("CHECK {}\n[cut: 1000 more characters]", "z".repeat(2000)));
}

// ---- the file that was asked about is the file that runs ----

#[tokio::test]
async fn a_saved_file_changed_after_the_question_is_refused_and_nothing_starts() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"OLD\"\n";
    let d = project_with("saved", text);
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    // the turn stamps the subject of the file as it is when the person is asked
    let mut args = json!({"name": "saved", "pinned": "saved@00000000000000000000000000000000"});
    nexo_agent::workflow::tool::pin(&mut args, d.path());
    assert_eq!(args["pinned"], json!(subject("saved", text)), "what the model put there is replaced");
    // the file is swapped between the question and the run
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), text.replace("OLD", "NEW")).unwrap();
    let out = wf.run(args.clone(), &ctx(d.path())).await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("changed after you were asked about it, so nothing was started"), "{}", out.content);
    assert!(client.tasks().is_empty());
    // asked about again, it runs the new one
    nexo_agent::workflow::tool::pin(&mut args, d.path());
    let out = wf.run(args, &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.count("NEW"), 1);
}

#[tokio::test]
async fn a_pin_made_with_the_old_short_hash_is_refused_so_nothing_starts_on_it() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"P\"\n";
    let d = project_with("saved", text);
    let client = Mock::new(vec![]);
    let wf = tool(&client, d.path());
    let long = subject("saved", text);
    let short = long[.."saved@".len() + 8].to_string();
    let out = wf.run(json!({"name": "saved", "pinned": short}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("changed after you were asked about it"), "{}", out.content);
    assert!(client.tasks().is_empty());
    let out = wf.run(json!({"name": "saved", "pinned": long}), &ctx(d.path())).await;
    assert!(!out.is_error, "{}", out.content);
}

#[tokio::test]
async fn no_subject_for_inline_text_or_a_name_that_does_not_load_and_a_bad_name_is_refused_first() {
    let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"P\"\n";
    let d = project_with("saved", text);
    std::fs::write(d.path().join(".nexo/workflows/broken.toml"), "this is = = not toml").unwrap();
    let pinned = |mut a: Value| {
        nexo_agent::workflow::tool::pin(&mut a, d.path());
        a.get("pinned").cloned()
    };
    assert_eq!(pinned(json!({"name": "saved"})), Some(json!(subject("saved", text))));
    assert_eq!(pinned(json!({"inline": text, "pinned": "saved@00000000000000000000000000000000"})), None);
    assert_eq!(pinned(json!({"name": "broken", "pinned": "x"})), None);
    assert_eq!(pinned(json!({"name": "ghost"})), None);
    assert_eq!(pinned(json!({"name": "../saved"})), None);
    let client = Mock::new(vec![]);
    let out = tool(&client, d.path()).run(json!({"name": "../../etc/passwd"}), &ctx(d.path())).await;
    assert!(out.is_error && out.content.contains("is not a workflow name"), "{}", out.content);
    assert!(client.tasks().is_empty());
}

#[tokio::test]
async fn the_unreported_note_counts_only_sub_agents_that_reported_without_usage() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![
        ("BOOM", Say::Fail),
        ("CUT", Say::Hang),
        ("BARE", Say::Bare("no meter".into())),
        ("FINE", says("fine")),
    ]);
    let wf = tool(&client, d.path());
    // a sub-agent whose first call fails, and one the person stops: neither had a report to give usage with
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"boom\"\nprompt = \"BOOM\"\n[[phase.step]]\nid = \"cutoff\"\nprompt = \"CUT\"\n[[phase.step]]\nid = \"bare\"\nprompt = \"BARE\"\n[[phase.step]]\nid = \"fine\"\nprompt = \"FINE\"\n";
    let c = ctx(d.path());
    let cutter = async {
        client.until_started(2).await;
        let job = loop {
            if let Some(j) = c.jobs.list().into_iter().find(|j| j.label.contains("cutoff")) {
                break j;
            }
            tokio::task::yield_now().await;
        };
        assert!(c.jobs.kill(job.id));
    };
    let (out, ()) = tokio::join!(wf.run(json!({"inline": toml}), &c), cutter);
    assert!(steps_line(&out, "boom").contains("failed"), "{}", out.content);
    assert!(steps_line(&out, "cutoff").contains("failed"), "{}", out.content);
    // only the one that reported with no usage is named
    assert_eq!(out.content.matches("token use was not reported").count(), 1, "{}", out.content);
    let note_after = |id: &str| {
        let at = out.content.find(&format!("- {id}:")).unwrap();
        let rest = &out.content[at..];
        let end = rest[1..].find("\n- ").map_or(rest.len(), |i| i + 1);
        rest[..end].contains("token use was not reported")
    };
    assert!(note_after("bare"), "{}", out.content);
    for id in ["boom", "cutoff", "fine"] {
        assert!(!note_after(id), "{id}: {}", out.content);
    }
}

#[tokio::test]
async fn a_step_report_of_five_or_more_parts_in_a_later_prompt_holds_to_the_group_cap_part_by_part() {
    let d = tempfile::tempdir().unwrap();
    let items = (1..=6).map(|n| format!("\"i{n}\"")).collect::<Vec<_>>().join(", ");
    let client = Mock::new(vec![
        ("FIND", says(&format!("[{items}]"))),
        ("AFTER", says("done")),
        ("BIG", says(&"x".repeat(7000))),
    ]);
    let toml = "[[phase]]\ntitle = \"Find\"\n[[phase.step]]\nid = \"find\"\nprompt = \"FIND\"\nreturns = \"list\"\n[[phase]]\ntitle = \"Work\"\n[[phase.step]]\nid = \"work\"\neach = \"find\"\nprompt = \"BIG {{item}}\"\n[[phase]]\ntitle = \"Last\"\n[[phase.step]]\nid = \"last\"\nprompt = \"AFTER {{steps.work.report}}\"\n";
    let (out, _) = go(&client, d.path(), toml, "").await;
    assert!(!out.is_error, "{}", out.content);
    let after = client.tasks().into_iter().find(|t| t.starts_with("AFTER")).expect("the last step started");
    // 6 parts share 24000: 4000 each (the per-report cap of 6000 alone would let 36000 in)
    assert_eq!(after.matches('x').count(), 24000);
    assert_eq!(after.matches("[cut: 3000 more characters]").count(), 6, "{after}");
    for n in 1..=6 {
        assert!(after.contains(&format!("i{n}")), "part {n} vanished");
    }
}

#[tokio::test]
async fn text_of_many_bytes_per_character_is_cut_in_the_answer_on_a_character_boundary_with_the_cut_line() {
    let d = tempfile::tempdir().unwrap();
    // a sub-agent's own report is held to about 8 KB, so the 6000-character cap is reached only by text that is
    // mostly one byte per character: ASCII first, then characters of 2 and 3 bytes where the cut falls
    for (ch, ascii, multi, kept) in [("é", 5200, 1000, 800), ("日", 5300, 800, 700)] {
        let text = format!("{}{}", "x".repeat(ascii), ch.repeat(multi));
        let client = Mock::new(vec![("BIG", says(&text))]);
        let one = "[[phase]]\ntitle = \"Only\"\n[[phase.step]]\nid = \"s1\"\nprompt = \"BIG 1\"\n";
        let (out, _) = go(&client, d.path(), one, "").await;
        let head: String = out.content.chars().take(300).collect();
        assert_eq!(out.content.matches(ch).count(), kept, "{ch}: {head}");
        assert_eq!(out.content.matches('x').count(), ascii, "{ch}");
        assert!(
            out.content.contains(&format!("{}\n[cut: {} more characters]", ch.repeat(kept), ascii + multi - 6000)),
            "{ch}"
        );
        assert!(!out.content.contains('\u{fffd}'), "{ch}");
    }
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

#[tokio::test]
async fn a_run_reports_each_step_going_waiting_running_ok_in_order_with_its_sub_agents_and_ends() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("ONE", says("one")), ("TWO", says("two")), ("THREE", says("three"))]);
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let out = tool(&client, d.path()).run(json!({"inline": THREE}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let seen = flow_events(&mut rx);

    // the first snapshot: every step waits
    let first = &seen[0];
    assert_eq!((first.id.as_str(), first.name.as_str(), first.state), ("wf#1", "inline", FlowState::Running));
    assert_eq!(first.phases.iter().map(|p| p.title.as_str()).collect::<Vec<_>>(), ["Look", "Join"]);
    assert!(first.phases.iter().flat_map(|p| &p.steps).all(|s| s.state == StepState::Waiting && s.subs.is_empty()));
    for id in ["a", "b", "c"] {
        assert_eq!(path_of(&seen, id), [StepState::Waiting, StepState::Running, StepState::Ok], "step {id}");
    }
    // the second phase begins only once the first has ended
    let at = |f: &dyn Fn(&FlowInfo) -> bool| seen.iter().position(f).unwrap();
    let c_runs = at(&|i| running(i, "c"));
    assert!(c_runs > at(&|i| fstep(i, "a").state == StepState::Ok), "c started before a ended");
    assert!(c_runs > at(&|i| fstep(i, "b").state == StepState::Ok), "c started before b ended");
    // a sub-agent's id is in the snapshot that says its step runs, and it is the id the registry lists
    let listed: Vec<String> = c.jobs.subs().list().into_iter().map(|i| i.id).collect();
    for id in ["a", "b", "c"] {
        let s = seen.iter().map(|i| fstep(i, id)).find(|s| s.state == StepState::Running).unwrap();
        assert_eq!(s.subs.len(), 1, "step {id}");
        assert!(listed.contains(&s.subs[0]), "{} is not in {listed:?}", s.subs[0]);
        assert!(s.started_ms.is_some() && s.ended_ms.is_none());
    }
    // the last snapshot says the run ended, and what each step spent
    let last = seen.last().unwrap();
    assert_eq!(last.state, FlowState::Ok);
    assert!(last.ended_ms.is_some());
    assert!(last
        .phases
        .iter()
        .flat_map(|p| &p.steps)
        .all(|s| s.tokens == 15 && s.ended_ms.is_some() && s.why.is_none()));
    assert_eq!(c.jobs.subs().flows(), vec![last.clone()], "the registry keeps the last snapshot");
}

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

#[tokio::test]
async fn snapshots_reach_the_interface_but_are_not_in_the_session_file() {
    let saved = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"SAVED a\"\n";
    let d = project_with("saved", saved);
    let client = Mock::with_main(
        vec![],
        vec![call("w1", "workflow", json!({"name": "saved"})), text_events("all done qzx", 1, 1)],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.submit("run it qzx");
    r.ask().await;
    r.send(AgentCommand::Permission { id: "w1".into(), decision: PermissionDecision::Allow });
    let evs = r.turn().await;
    let flows: Vec<&FlowInfo> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::FlowRun { info } => Some(info),
            _ => None,
        })
        .collect();
    assert!(flows.len() >= 3, "the interface heard {} snapshots", flows.len());
    assert_eq!(flows.last().unwrap().state, FlowState::Ok);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();

    // nothing of it was written down
    let mut files = Vec::new();
    files_under(&nexo_core::paths::sessions_dir(), &mut files);
    // only this test's own session file: the one that holds its marker (the folder is shared by every test here)
    let texts: Vec<String> =
        files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).filter(|t| t.contains("run it qzx")).collect();
    assert_eq!(texts.len(), 1, "this test's session file was not found once: {}", texts.len());
    assert!(texts[0].contains("all done qzx"), "the session file is not the whole conversation");
    assert!(!texts[0].contains("flow_run") && !texts[0].contains("wf#"), "a snapshot was written to the session file");
}

#[tokio::test]
async fn stopping_a_step_cancels_its_agent_fails_it_with_the_reason_skips_what_needs_it_and_the_rest_goes_on() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let toml = r#"
[[phase]]
title = "One"
  [[phase.step]]
  id = "a"
  prompt = "HANG a"
  [[phase.step]]
  id = "b"
  prompt = "B works"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "dep"
  prompt = "DEP {{steps.a.report}}"
  [[phase.step]]
  id = "free"
  prompt = "FREE {{steps.b.report}}"
"#;
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let mut all = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        let i = until_flow(&mut rx, |i| running(i, "a")).await;
        // what the agent does with `FlowStopStep`: the registry names the jobs, and each is cancelled
        let jobs = c.jobs.subs().stop_flow_step(&i.id, "a").unwrap();
        assert_eq!(jobs.len(), 1);
        for j in jobs {
            assert!(c.jobs.kill(j));
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(!out.is_error, "{}", out.content);
    assert!(steps_line(&out, "a").contains("failed: stopped by the person"), "{}", out.content);
    assert!(steps_line(&out, "b").contains(": ok"), "{}", out.content);
    assert!(steps_line(&out, "dep").contains("skipped: step `a` failed: stopped by the person"), "{}", out.content);
    assert!(steps_line(&out, "free").contains(": ok"), "{}", out.content);
    assert_eq!((client.count("DEP"), client.count("FREE")), (0, 1));
    let last = flow_events(&mut all).pop().unwrap();
    assert_eq!(last.state, FlowState::Problems);
    assert_eq!(fstep(&last, "a").state, StepState::Failed);
    assert_eq!(fstep(&last, "a").why.as_deref(), Some("stopped by the person"));
    assert_eq!(fstep(&last, "dep").state, StepState::Skipped);
    assert_eq!(fstep(&last, "free").state, StepState::Ok);
    // the sub-agent was cancelled, not left at work
    assert!(c.jobs.subs().list().iter().any(|i| i.state == SubState::Killed));
    assert_eq!(c.jobs.running(), 0);
    // a step that ended cannot be stopped, and neither can an unknown one
    assert!(c.jobs.subs().stop_flow_step("wf#1", "b").is_err());
    assert!(c.jobs.subs().stop_flow_step("wf#1", "nope").is_err());
}

#[tokio::test]
async fn stopping_the_run_starts_nothing_more_and_the_result_lists_what_ended_and_what_was_stopped() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang), ("DONE", says("finished"))]);
    let toml = r#"
max_parallel = 2
[[phase]]
title = "One"
  [[phase.step]]
  id = "done"
  prompt = "DONE first"
  [[phase.step]]
  id = "a"
  prompt = "HANG a"
  [[phase.step]]
  id = "b"
  prompt = "HANG b"
  [[phase.step]]
  id = "c"
  prompt = "HANG c"
[[phase]]
title = "Two"
  [[phase.step]]
  id = "z"
  prompt = "AFTER"
"#;
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let mut all = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        let i = until_flow(&mut rx, |i| running(i, "a") && running(i, "b") && fstep(i, "done").state == StepState::Ok)
            .await;
        let jobs = c.jobs.subs().stop_flow(&i.id).unwrap();
        assert_eq!(jobs.len(), 2, "a and b are at work");
        for j in jobs {
            c.jobs.kill(j);
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(out.content.contains("Stopped by the person"), "{}", out.content);
    assert!(out.content.contains("Ended: done. Stopped: a, b. Not started: c, z."), "{}", out.content);
    assert_eq!(client.tasks().len(), 3, "something started after the stop: {:?}", client.tasks());
    assert!(steps_line(&out, "z").contains("not run"), "{}", out.content);
    let last = flow_events(&mut all).pop().unwrap();
    assert_eq!(last.state, FlowState::Stopped);
    assert_eq!(c.jobs.running(), 0);
}

#[tokio::test]
async fn one_run_at_a_time_a_second_is_refused_in_one_sentence_by_the_tool_too_and_allowed_once_the_first_ends() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let first = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"GATE a\"\n";
    let c = ctx(d.path());
    let wf = tool(&client, d.path());
    let second = async {
        client.until_started(1).await;
        let refused = wf.run(json!({"inline": first}), &c).await;
        client.st.gate.add_permits(1);
        refused
    };
    let (out, refused) = tokio::join!(bounded("the first run", wf.run(json!({"inline": first}), &c)), second);
    assert!(!out.is_error, "{}", out.content);
    assert!(refused.is_error);
    assert_eq!(refused.content, "A workflow is already running (wf#1); wait for it to end or stop it first.");
    assert_eq!(client.tasks().len(), 1, "the refused one started a sub-agent");
    // once the first has ended, the next goes
    client.st.gate.add_permits(1);
    let again = bounded("the next run", wf.run(json!({"inline": first}), &c)).await;
    assert!(!again.is_error, "{}", again.content);
    assert_eq!(c.jobs.subs().flows().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["wf#1", "wf#2"]);
}

/// A saved workflow with one step that holds at the gate.
const BYNAME: &str = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"GATE {{args}}\"\n";

fn notice_has(e: &AgentEvent, word: &str) -> bool {
    matches!(e, AgentEvent::Notice { text, .. } if text.contains(word))
}

#[tokio::test]
async fn a_workflow_started_by_name_asks_with_the_pinned_subject_and_a_denial_runs_nothing() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    assert_eq!((req.tool.as_str(), req.kind), ("workflow", ToolKind::Other));
    assert!(req.id.starts_with("workflow-start-"), "{}", req.id);
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("saved", BYNAME))]);
    r.send(AgentCommand::Permission {
        id: req.id.clone(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let evs = r.until(|e| notice_has(e, "was not started")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::FlowRun { .. })), "a denied run reported itself");
    assert!(client.tasks().is_empty());
    // a name that is not saved is refused with no question
    r.send(AgentCommand::FlowStart { name: "nope".into(), args: String::new() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("workflow:"))).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_allowed_start_by_name_runs_in_the_background_the_person_keeps_talking_and_nexo_writes_the_result() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "hello".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.count("GATE hello"), 1, "the argument text reached the step");

    // the main agent is not busy with it: a turn starts and ends while the sub-agent is held
    r.submit("meanwhile qzx");
    let evs = r.turn().await;
    assert!(evs.iter().all(|e| !matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running)));
    assert_eq!(client.st.ended.load(Ordering::SeqCst), 0, "the run ended before it was let through");
    assert_eq!(client.main_msgs.lock().unwrap().len(), 1);

    // let it end: the result arrives as a turn the note starts
    client.st.gate.add_permits(1);
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    r.until_both(ended, |e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let note = client.main_msgs.lock().unwrap().last().cloned().unwrap();
    assert!(note.contains("nexo ran the saved workflow `saved`"), "{note}");
    assert!(note.contains("not from the person"), "{note}");
    assert!(note.contains("- a: ok"), "the report is in the note: {note}");
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_second_start_by_name_while_one_runs_is_refused_before_any_question() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "one".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "two".into() });
    let evs = r.until(|e| notice_has(e, "already running")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "asked about the second");
    client.st.gate.add_permits(1);
    r.turn().await;
    assert_eq!(client.tasks().len(), 1);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_file_swapped_between_the_question_and_the_answer_is_refused_and_nothing_starts() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), BYNAME.replace("GATE", "GATE swapped")).unwrap();
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| notice_has(e, "changed after you were asked about it, so nothing was started")).await;
    assert!(client.tasks().is_empty(), "{:?}", client.tasks());
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn read_only_and_plan_mode_treat_a_start_by_name_as_the_tool_does() {
    for mode in [PermissionMode::ReadOnly, PermissionMode::Plan] {
        let text = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"look\"\nprompt = \"LOOK\"\n[[phase.step]]\nid = \"change\"\nagent = \"general\"\nprompt = \"CHANGE\"\n";
        let d = project_with("saved", text);
        let client =
            Mock::new(vec![("LOOK", says("looked")), ("CHANGE", Say::CallThen("edit", "the mode said no".into()))]);
        let tools = ToolSet::new().with(Arc::new(Read)).with(Arc::new(Named("edit")));
        let mut r = start(&client, d.path(), mode, tools);
        r.next().await;
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: String::new() });
        let req = r.ask().await;
        assert_eq!(req.tool, "workflow", "{mode:?}: asked as the tool is");
        r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
        r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running)).await;
        assert_eq!((client.count("LOOK"), client.count("CHANGE")), (1, 1), "{mode:?}");
        let log = client.st.log.lock().unwrap().clone();
        let result = log.iter().find(|(_, t)| t.starts_with("RESULT")).expect("a result came back");
        assert!(result.1.contains("Permission denied") && result.1.contains(mode.as_str()), "{mode:?}: {}", result.1);
        r.send(AgentCommand::Shutdown);
        r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
        r.task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn the_commands_stop_a_step_and_the_run_through_the_agent_and_the_interrupt_key_is_unchanged() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::with_main(
        vec![("HANG", Say::Hang)],
        vec![
            text_events("noted", 1, 1),
            call("w1", "workflow", json!({"name": "saved", "args": "t"})),
            text_events("after", 1, 1),
        ],
    );
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;

    // 1: by name, then stop the step: the run ends with the step failed
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.phases[0].steps[0].state == StepState::Running)).await;
    r.send(AgentCommand::FlowStopStep { run: "wf#1".into(), step: "a".into() });
    let evs = r
        .until_both(
            |e| matches!(e, AgentEvent::FlowRun { info } if info.state != FlowState::Running),
            |e| matches!(e, AgentEvent::TurnEnd { .. }),
        )
        .await;
    let end = evs.iter().rev().find_map(|e| match e {
        AgentEvent::FlowRun { info } => Some(info.clone()),
        _ => None,
    });
    let end = end.unwrap();
    assert_eq!((end.state, end.phases[0].steps[0].state), (FlowState::Problems, StepState::Failed));
    assert_eq!(end.phases[0].steps[0].why.as_deref(), Some("stopped by the person"));

    // 2: an unknown run is said to the person in one sentence
    r.send(AgentCommand::FlowStop { run: "wf#9".into() });
    r.until(|e| notice_has(e, "there is no workflow run wf#9")).await;

    // 3: Esc (the interrupt) in a turn that waits for a workflow ends the turn and the run, as before
    r.submit("run it");
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if info.id == "wf#2" && running(info, "a"))).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    let end = evs.iter().rev().find_map(|e| match e {
        AgentEvent::FlowRun { info } => Some(info.clone()),
        _ => None,
    });
    assert_eq!(end.unwrap().state, FlowState::Stopped);
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
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

#[tokio::test]
async fn clear_during_a_run_stops_it_and_none_of_its_events_reach_the_new_conversation() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
    r.send(AgentCommand::Clear);
    let mut evs = r.until(|e| matches!(e, AgentEvent::HistoryCleared)).await;
    // the new conversation goes on with the count (an old id is never given twice), and the old run says nothing
    // more: the first snapshot it hears is the new run beginning
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "again".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    evs.extend(r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await);
    // the old run has had its time to end: nothing it did is told
    evs.extend(drain(&mut r).await);
    let flows = flows_in(&evs);
    let first = flows[0];
    assert_eq!((first.id.as_str(), first.state), ("wf#2", FlowState::Running), "{flows:?}");
    assert!(flows.iter().all(|i| i.id != "wf#1"), "the old run spoke into the new conversation: {flows:?}");
    assert!(flows.iter().all(|i| i.state == FlowState::Running), "an old end reached the new one: {flows:?}");
    assert!(first.phases[0].steps.iter().all(|s| s.state == StepState::Waiting), "{first:?}");
    let named: Vec<&AgentEvent> = evs.iter().filter(|e| notice_has(e, "saved")).collect();
    assert!(named.is_empty(), "the new conversation was told of the old run: {named:?}");
    assert!(client.main_msgs.lock().unwrap().is_empty(), "the main agent got a note of the old run");
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
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

#[tokio::test]
async fn an_always_allow_answered_after_a_clear_adds_no_rule_to_the_new_conversation() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let (asked, mut asked_rx) = tokio::sync::mpsc::unbounded_channel();
    let (answer, answers) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(HeldGate { asked, answers: tokio::sync::Mutex::new(answers) });
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new().with(Arc::new(Read)),
        client: client.clone(),
        resume: None,
        gate: Some(gate),
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let mut r = Running { handle, task: tokio::spawn(nexo_agent::run(ends, setup)) };
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = bounded("the question", asked_rx.recv()).await.unwrap();
    // the conversation is replaced while the question is open (this gate does not deny it), then it is answered
    r.send(AgentCommand::Clear);
    r.until(|e| matches!(e, AgentEvent::HistoryCleared)).await;
    answer.send(PermissionDecision::AllowAlways { rule: req.suggested_rules[0].clone(), persist: false }).unwrap();
    // the old start ends without running; until it lets its slot go, a new start is refused and is sent again
    let start_again = |r: &Running| r.send(AgentCommand::FlowStart { name: "saved".into(), args: "y".into() });
    start_again(&r);
    let mut again = None;
    while again.is_none() {
        tokio::select! {
            q = asked_rx.recv() => again = q,
            ev = r.next() => match ev {
                AgentEvent::FlowRun { .. } => panic!("the always-allow of the old conversation holds in the new one"),
                e if notice_has(&e, "already starting") => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    start_again(&r);
                }
                _ => {}
            },
        }
    }
    let again = again.expect("the new conversation asks again");
    assert_eq!(again.suggested_rules, req.suggested_rules);
    answer.send(PermissionDecision::Deny { reason: Some("no".into()) }).unwrap();
    r.until(|e| notice_has(e, "was not started")).await;
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_session_with_no_saved_workflow_sends_a_request_that_says_nothing_of_workflows() {
    #[derive(Default)]
    struct Record(Mutex<Vec<String>>);
    #[async_trait]
    impl ModelClient for Record {
        async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
            self.0.lock().unwrap().push(format!("{req:?}").to_lowercase());
            Ok(Box::pin(futures::stream::iter(text_events("hi", 1, 1))))
        }
    }
    data_dir();
    let d = tempfile::tempdir().unwrap();
    let rec = Arc::new(Record::default());
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new().with(Arc::new(Read)),
        client: rec.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let mut r = Running { handle, task: tokio::spawn(nexo_agent::run(ends, setup)) };
    r.next().await;
    r.submit("hello");
    r.turn().await;
    let sent = rec.0.lock().unwrap().clone();
    assert!(!sent.is_empty());
    for req in &sent {
        assert!(!req.contains("workflow") && !req.contains("flow_"), "the request names workflows: {req}");
    }
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
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

#[cfg(unix)]
#[tokio::test]
async fn a_start_by_name_runs_the_pre_tool_hooks_a_block_says_its_words_and_nothing_is_asked() {
    let d = project_with("saved", &quick());
    let client = Mock::new(vec![]);
    let cfg = with_hook("pre_tool", "workflow", "cat > pre.json; echo 'not on fridays' >&2; exit 2");
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let evs = r.until(|e| notice_has(e, "was not started")).await;
    assert!(notice_has(evs.last().unwrap(), "not on fridays"), "{:?}", evs.last());
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. } | AgentEvent::FlowRun { .. })));
    assert!(client.tasks().is_empty());
    // the hook saw the call as it sees the tool's
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("pre.json")).unwrap()).unwrap();
    assert_eq!(seen["tool_name"], "workflow");
    assert_eq!(seen["tool_input"]["name"], "saved");
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_hook_decision_reaches_the_permission_of_a_start_by_name() {
    let d = project_with("saved", &quick());
    let client = Mock::new(vec![]);
    let cfg = with_hook(
        "pre_tool",
        "workflow",
        r#"case "$(cat)" in *allowme*) echo '{"decision":"allow"}';; *denyme*) echo '{"decision":"deny","reason":"the hook says no"}';; esac"#,
    );
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    // allow: it runs with no question, though the mode asks
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "allowme".into() });
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    let evs = r.until(|e| ended(e) || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(ended(evs.last().unwrap()), "it asked though the hook allowed it: {:?}", evs.last());
    r.turn().await;
    // deny: refused with the hook's reason and no question
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "denyme".into() });
    let evs = r.until(|e| notice_has(e, "was not started") || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(notice_has(evs.last().unwrap(), "the hook says no"), "{:?}", evs.last());
    assert_eq!(client.count("DONE"), 1);
    // no decision: the person is asked, as before
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "other".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_hook_that_replaces_the_input_of_a_start_by_name_gets_the_new_file_pinned_and_asked() {
    let other = "[[phase]]\ntitle = \"B\"\n[[phase.step]]\nid = \"b\"\nprompt = \"GATE swapped {{args}}\"\n";
    let d = project_with("saved", BYNAME);
    std::fs::write(d.path().join(".nexo/workflows/other.toml"), other).unwrap();
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let cfg = with_hook(
        "pre_tool",
        "workflow",
        r#"case "$(cat)" in *first*) echo '{"updated_input":{"name":"other","args":"now"}}';; esac"#,
    );
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "first".into() });
    let req = r.ask().await;
    // the question is about what will run: the other file, pinned as it is now
    assert_eq!(req.suggested_rules, vec![format!("workflow({})", subject("other", other))]);
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.tasks(), vec!["GATE swapped now".to_string()]);
    client.st.gate.add_permits(1);
    r.turn().await;
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_post_tool_hook_of_a_start_by_name_fires_and_its_words_reach_the_main_agent_in_the_note() {
    let d = project_with("saved", &quick());
    let client = Mock::new(vec![]);
    let cfg = with_hook("post_tool", "workflow", "cat > post.json; echo 'formatted it' >&2; exit 2");
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "x".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    let ended = |e: &AgentEvent| matches!(e, AgentEvent::FlowRun { info } if info.state == FlowState::Ok);
    r.until_both(ended, |e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let note = client.main_msgs.lock().unwrap().last().cloned().unwrap();
    assert!(note.contains("[hook] formatted it"), "{note}");
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("post.json")).unwrap()).unwrap();
    assert_eq!(seen["tool_name"], "workflow");
    stop_agent(r).await;
}

#[tokio::test]
async fn a_second_start_while_the_first_is_asking_is_refused_in_one_sentence_and_a_denial_frees_the_slot() {
    let d = project_with("saved", BYNAME);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "one".into() });
    let first = r.ask().await;
    // the question is open and no run exists yet: the slot is what refuses
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "two".into() });
    let evs = r.until(|e| notice_has(e, "already starting") || matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(notice_has(evs.last().unwrap(), "already starting"), "a second question was asked: {:?}", evs.last());
    r.send(AgentCommand::Permission { id: first.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    // a denial frees it: the next start is asked
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "three".into() });
    let again = r.ask().await;
    r.send(AgentCommand::Permission { id: again.id, decision: PermissionDecision::Allow });
    client.until_started(1).await;
    assert_eq!(client.tasks(), vec!["GATE three".to_string()]);
    // and the slot is held until the run ends: a start while it runs is refused before any question
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "four".into() });
    let evs = r.until(|e| notice_has(e, "already")).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    client.st.gate.add_permits(1);
    r.turn().await;
    // the end frees it too
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "five".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Deny { reason: None } });
    r.until(|e| notice_has(e, "was not started")).await;
    stop_agent(r).await;
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

#[tokio::test]
async fn a_question_open_when_the_conversation_is_replaced_is_denied_silently_and_no_rule_is_kept() {
    for how in [Replace::Clear, Replace::Fork, Replace::Resume] {
        let d = project_with("saved", BYNAME);
        let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
        let (mut r, sid1) = recorded(&client, d.path()).await;
        if matches!(how, Replace::Resume) {
            // a second session, so that the first can be resumed
            replace(&mut r, Replace::Clear, &sid1).await;
        }
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: "q".into() });
        let req = r.ask().await;
        replace(&mut r, how, &sid1).await;
        // the answer of the person comes late and belongs to the old conversation: it keeps no rule
        r.send(AgentCommand::Permission {
            id: req.id.clone(),
            decision: PermissionDecision::AllowAlways { rule: req.suggested_rules[0].clone(), persist: false },
        });
        let evs = drain(&mut r).await;
        assert!(!evs.iter().any(|e| notice_has(e, "was not started")), "{how:?}: the new session was told: {evs:?}");
        assert!(flows_in(&evs).is_empty(), "{how:?}");
        // the slot is free and the same start is asked again
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: "q".into() });
        let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. } | AgentEvent::FlowRun { .. })).await;
        assert!(
            matches!(evs.last(), Some(AgentEvent::PermissionAsk { .. })),
            "{how:?}: the rule of an old conversation allowed it: {:?}",
            evs.last()
        );
        assert!(client.tasks().is_empty(), "{how:?}");
        stop_agent(r).await;
    }
}

#[tokio::test]
async fn the_run_count_goes_on_after_clear_fork_and_resume_and_each_replaces_the_run_before() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let (mut r, sid1) = recorded(&client, d.path()).await;
    let mut n = 0;
    for how in [None, Some(Replace::Clear), Some(Replace::Fork), Some(Replace::Resume)] {
        if let Some(how) = how {
            replace(&mut r, how, &sid1).await;
        }
        n += 1;
        r.send(AgentCommand::FlowStart { name: "saved".into(), args: n.to_string() });
        let req = r.ask().await;
        r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
        let evs = r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
        let id = format!("wf#{n}");
        assert!(flows_in(&evs).iter().all(|i| i.id == id), "{how:?}: {:?}", flows_in(&evs));
    }
    stop_agent(r).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_that_ends_after_a_clear_fires_no_hook_and_tells_nothing_to_the_new_conversation() {
    let d = project_with("saved", BYNAME.replace("GATE", "HANG").as_str());
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    let cfg = with_hook("post_tool", "workflow", "echo ran > post-ran.txt");
    let mut r = start_with(&client, d.path(), cfg, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: "s".into() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    r.until(|e| matches!(e, AgentEvent::FlowRun { info } if running(info, "a"))).await;
    r.send(AgentCommand::Clear);
    let mut evs = r.until(|e| matches!(e, AgentEvent::HistoryCleared)).await;
    // the old run ends in the cleared session; the hooks of the new one are not run for it
    evs.extend(drain(&mut r).await);
    evs.extend(drain(&mut r).await);
    assert!(!d.path().join("post-ran.txt").exists(), "a hook ran for a run of a session that is gone");
    assert!(!evs.iter().any(|e| notice_has(e, "saved")), "{evs:?}");
    assert!(client.main_msgs.lock().unwrap().is_empty());
    stop_agent(r).await;
}

#[tokio::test]
async fn a_sub_agent_that_ends_while_the_main_agent_is_idle_starts_no_turn_and_the_run_makes_one_note() {
    let two = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"DONE a\"\n[[phase.step]]\nid = \"b\"\nprompt = \"GATE b\"\n";
    let d = project_with("saved", two);
    let client = Mock::new(vec![("GATE", Say::Gated("opened".into()))]);
    let mut r = start(&client, d.path(), PermissionMode::Ask, ToolSet::new().with(Arc::new(Read)));
    r.next().await;
    r.send(AgentCommand::FlowStart { name: "saved".into(), args: String::new() });
    let req = r.ask().await;
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    // one step ended and the other is held: the run goes on, and the main agent has been told nothing
    let mut evs = r
        .until(|e| matches!(e, AgentEvent::FlowRun { info } if fstep(info, "a").state == StepState::Ok && running(info, "b")))
        .await;
    evs.extend(drain(&mut r).await);
    assert!(client.main_msgs.lock().unwrap().is_empty(), "a turn started: {:?}", client.main_msgs.lock().unwrap());
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "{evs:?}");
    // the end of the run is one note, once
    client.st.gate.add_permits(1);
    r.turn().await;
    evs = drain(&mut r).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "a second turn: {evs:?}");
    assert_eq!(client.main_msgs.lock().unwrap().len(), 1);
    assert!(client.main_msgs.lock().unwrap()[0].contains("nexo ran the saved workflow `saved`"));
    stop_agent(r).await;
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

#[tokio::test]
async fn a_stopped_step_does_not_start_the_items_that_waited_for_a_place_and_ends_failed_with_the_reason() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("FIND", says("[\"DONE one\", \"HANG two\", \"HANG three\"]")), ("HANG", Say::Hang)]);
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let wf = tool(&client, d.path());
    let stopper = async {
        // find, the first item (ends at once) and the second (held): the third waits for a place
        client.until_started(3).await;
        let i = until_flow(&mut rx, |i| running(i, "check")).await;
        let jobs = c.jobs.subs().stop_flow_step(&i.id, "check").unwrap();
        assert_eq!(jobs.len(), 1, "only the second item is at work");
        for j in jobs {
            assert!(c.jobs.kill(j));
        }
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": EACH_STOP}), &c)), stopper);
    assert!(steps_line(&out, "check").contains("failed: stopped by the person"), "{}", out.content);
    assert_eq!(client.count("HANG three"), 0, "an item of a stopped step started: {:?}", client.tasks());
    assert_eq!(c.jobs.running(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_that_comes_while_a_sub_agent_is_starting_reaches_it_as_soon_as_it_has_started() {
    let d = tempfile::tempdir().unwrap();
    let client = Mock::new(vec![("HANG", Say::Hang)]);
    // the hook of the sub-agent's start holds the start until the test lets it go
    let hold = nexo_core::config::HookConfig {
        event: "subagent_start".into(),
        command: "touch starting; while [ ! -f go ]; do sleep 0.02; done".into(),
        ..Default::default()
    };
    let sub = sub_agent(&client, ToolSet::new().with(Arc::new(Read))).with_hooks(vec![hold]);
    let wf = Workflow::new(Arc::new(sub), d.path().to_path_buf());
    let toml = "[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"HANG a\"\n";
    let c = ctx(d.path());
    let mut rx = c.jobs.subs().changes();
    let stopper = async {
        let i = until_flow(&mut rx, |_| true).await;
        let end = tokio::time::Instant::now() + PATIENCE;
        while !d.path().join("starting").exists() {
            assert!(tokio::time::Instant::now() < end, "the sub-agent never began to start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // no sub-agent is at work yet, so the stop has nobody to cancel
        assert!(c.jobs.subs().stop_flow_step(&i.id, "a").unwrap().is_empty());
        std::fs::write(d.path().join("go"), "").unwrap();
    };
    let (out, ()) = tokio::join!(bounded("the run", wf.run(json!({"inline": toml}), &c)), stopper);
    assert!(steps_line(&out, "a").contains("failed: stopped by the person"), "{}", out.content);
    assert_eq!(c.jobs.running(), 0);
}
