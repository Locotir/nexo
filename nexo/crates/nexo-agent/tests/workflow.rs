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
use nexo_agent::workflow::Workflow;
use nexo_agent::{AgentSetup, ModelClient, Target};
use nexo_core::agent::{channels, AgentCommand, AgentEvent, AgentHandle};
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::ProviderError;
use nexo_core::permission::{PermissionDecision, PermissionMode};
use nexo_core::provider::{EventStream, ModelRequest, ToolSpec};
use nexo_core::stream::{FinishReason, StreamEvent, Usage};
use nexo_core::subs::SubState;
use nexo_core::tool::{Tool, ToolContext, ToolKind, ToolOutput, ToolSet};
use serde_json::{json, Value};

const PATIENCE: Duration = Duration::from_secs(30);

/// What a sub-agent answers.
#[derive(Clone)]
enum Say {
    /// A report, spending this many tokens (input, output).
    Text(String, u64, u64),
    /// Never answers.
    Hang,
    /// One of these for each call, in turn.
    Cycle(Vec<String>),
}

fn says(s: &str) -> Say {
    Say::Text(s.into(), 10, 5)
}

#[derive(Default)]
struct State {
    tasks: Mutex<Vec<String>>,
    tools_seen: Mutex<Vec<Vec<String>>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
    /// (true: began, false: ended, the task).
    log: Mutex<Vec<(bool, String)>>,
    notify: tokio::sync::Notify,
}

struct Mock {
    /// (a word of the task, the answer): the first whose word the task holds.
    rules: Vec<(String, Say)>,
    main: Mutex<VecDeque<Vec<StreamEvent>>>,
    turns: AtomicUsize,
    st: Arc<State>,
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
        })
    }

    fn tasks(&self) -> Vec<String> {
        self.st.tasks.lock().unwrap().clone()
    }

    /// How many tasks hold `word`.
    fn count(&self, word: &str) -> usize {
        self.tasks().iter().filter(|t| t.contains(word)).count()
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
            let evs = self.main.lock().unwrap().pop_front().unwrap_or_else(|| text_events("ok", 1, 1));
            return Ok(Box::pin(futures::stream::iter(evs)));
        }
        let task = task_of(req);
        let say = self.rules.iter().find(|(k, _)| task.contains(k.as_str())).map(|(_, s)| s.clone());
        let st = self.st.clone();
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
            Say::Cycle(_) => unreachable!(),
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
    data_dir();
    let mut cfg = ProviderConfig::new("mock", "http://mock", None);
    cfg.models = vec!["m1".into()];
    SubAgent::new(client.clone(), Target::new("mock", cfg, "m1"), &set, vec![], Arc::new(OnceLock::new()))
}

fn tool(client: &Arc<Mock>, cwd: &Path) -> Workflow {
    Workflow::new(Arc::new(sub_agent(client, ToolSet::new().with(Arc::new(Read)))), cwd.to_path_buf())
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
    let client = Mock::new(vec![]);
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
    let (out, c) = go(&client, d.path(), toml, "").await;
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
    let client = Mock::new(vec![
        ("FIND", says("Three.\n[\"alpha\", \"beta\", \"gamma\"]")),
        ("CHECK alpha", says("REAL: it is there")),
        ("CHECK beta", says("NOT REAL: it is fine")),
        ("CHECK gamma", says("Hmm, who knows")),
    ]);
    let (out, c) = go(&client, d.path(), CHECKED, "src").await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.tasks().len(), 7);
    for item in ["alpha", "beta", "gamma"] {
        assert_eq!(client.count(&format!("CHECK {item}")), 2, "{item}");
    }
    // the closing line for a list is added to the step that returns one, not to the others
    assert!(client.tasks()[0].contains("JSON array of strings"));
    assert!(!client.tasks().iter().any(|t| t.starts_with("CHECK") && t.contains("JSON array")));
    assert!(client.tasks()[0].contains("FIND in src"));
    // the count line only where every copy began with the same word
    assert!(out.content.contains("=> item 1: REAL 2 of 2"), "{}", out.content);
    assert!(out.content.contains("=> item 2: NOT 2 of 2"), "{}", out.content);
    assert!(out.content.contains("=> item 3: HMM 2 of 2"), "{}", out.content);
    assert!(out.content.contains("--- check #1 (item 1: alpha) (copy 1) ---"), "{}", out.content);
    let titles: Vec<String> = c.jobs.subs().list().into_iter().map(|i| i.title).collect();
    assert!(titles.contains(&"inline · Check · check #6".to_string()), "{titles:?}");
    assert!(titles.contains(&"inline · Find · find".to_string()), "{titles:?}");
}

#[tokio::test]
async fn the_count_line_needs_every_copy_to_begin_with_one_of_two_words() {
    let d = tempfile::tempdir().unwrap();
    let one = CHECKED.replace("votes = 2", "votes = 3");
    // two words among three copies: the count is the majority
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL yes".into(), "NOT REAL".into(), "REAL again".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(out.content.contains("=> item 1: REAL 2 of 3"), "{}", out.content);
    // three different words: just the reports
    let client = Mock::new(vec![
        ("FIND", says("[\"a\"]")),
        ("CHECK", Say::Cycle(vec!["REAL".into(), "FAKE".into(), "MAYBE".into()])),
    ]);
    let (out, _) = go(&client, d.path(), &one, "").await;
    assert!(!out.content.contains("=>"), "{}", out.content);
    assert!(out.content.contains("--- check #3 (item 1: a) (copy 3) ---"), "{}", out.content);
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
async fn the_answer_is_bounded_per_report_and_in_all() {
    let d = tempfile::tempdir().unwrap();
    let big = "x".repeat(7000);
    let client = Mock::new(vec![("BIG", says(&big))]);
    let steps: String = (1..=5).map(|n| format!("[[phase.step]]\nid = \"s{n}\"\nprompt = \"BIG {n}\"\n")).collect();
    let toml = format!("[[phase]]\ntitle = \"Only\"\n{steps}");
    let (out, _) = go(&client, d.path(), &toml, "").await;
    assert!(out.content.contains("[cut: 1000 more characters]"), "{}", &out.content[..600.min(out.content.len())]);
    assert!(out.content.contains("[1 more report(s) left out: 24000 characters is the most]"));
    let xs = out.content.matches('x').count();
    let lens: Vec<usize> = out.content.split("== s").map(|p| p.matches('x').count()).collect();
    assert_eq!(
        xs,
        24000,
        "{lens:?} {}",
        out.content.lines().filter(|l| !l.starts_with("xxx")).collect::<Vec<_>>().join("|")
    );
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

#[tokio::test]
async fn leave_is_asked_once_a_denial_starts_nothing_and_always_allow_covers_saved_names_only() {
    data_dir();
    let d = tempfile::tempdir().unwrap();
    nexo_core::config::trust_project(d.path(), &nexo_core::paths::trusted_projects_file()).unwrap();
    std::fs::create_dir_all(d.path().join(".nexo/workflows")).unwrap();
    let saved = "description = \"two reads\"\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"SAVED a\"\n[[phase.step]]\nid = \"b\"\nprompt = \"SAVED b\"\n";
    std::fs::write(d.path().join(".nexo/workflows/saved.toml"), saved).unwrap();
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
            call("w4", "workflow", json!({"inline": inline})),
            text_events("denied", 1, 1),
        ],
    );
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new().with(Arc::new(Read)),
        client: client.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    r.next().await;

    // 1: asked, denied: nothing starts
    r.submit("run it");
    let req = r.ask().await;
    assert_eq!((req.tool.as_str(), req.kind), ("workflow", ToolKind::Other));
    assert_eq!(req.summary, "workflow `saved`: up to 2 sub-agents (at least 2), 400000 tokens at most");
    assert_eq!(req.suggested_rules, vec!["workflow(saved)".to_string()]);
    r.send(AgentCommand::Permission {
        id: "w1".into(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { .. })), "a denied tool never starts");
    assert!(client.tasks().is_empty(), "{:?}", client.tasks());

    // 2: allowed always by name: one question, two sub-agents
    r.submit("again");
    r.ask().await;
    r.send(AgentCommand::Permission {
        id: "w2".into(),
        decision: PermissionDecision::AllowAlways { rule: "workflow".into(), persist: false },
    });
    let evs = r.turn().await;
    assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::PermissionAsk { .. })).count(), 0);
    assert_eq!(client.tasks().len(), 2);

    // 3: the rule covers it: no question at all
    r.submit("and again");
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "{evs:?}");
    assert_eq!(client.tasks().len(), 4);

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
