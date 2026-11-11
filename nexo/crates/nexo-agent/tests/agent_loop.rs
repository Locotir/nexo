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
}

impl Scripted {
    fn new(replies: Vec<Reply>) -> Arc<Scripted> {
        Arc::new(Scripted {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(Vec::new()),
            bases: Mutex::new(Vec::new()),
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

fn data_dir() -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let d = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_CONFIG_DIR", d.path());
    std::env::set_var("NEXO_CACHE_DIR", d.path().join("cache"));
    d.path().to_path_buf()
}

/// Tests that write the user's configuration file take turns.
fn config_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn user_config() -> Config {
    Config::parse(&std::fs::read_to_string(data_dir().join("config.toml")).unwrap_or_default()).unwrap()
}

/// What nexo puts in a message is told apart from the typed words where the two are put together, and
/// the display adds nothing to the request: the message sent is those parts around the typed words, byte
/// for byte.
#[cfg(unix)]
#[tokio::test]
async fn what_nexo_adds_to_a_message_is_told_apart_and_the_request_is_unchanged() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("note.md"), "remember this").unwrap();
    std::fs::write(d.path().join("small.rs"), "fn small() {}\n").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("ok")), Reply::Events(text("again"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.attach_budget = 4000;
    cfg.hooks.push(hook_cfg("UserPromptSubmit", None, "echo 'it is monday'"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit {
        text: "see".into(),
        attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("note.md") }],
    });
    let evs = r.turn().await;
    let at = evs.iter().position(|e| matches!(e, AgentEvent::History { index: 0, .. })).unwrap();
    let Some(AgentEvent::Added { index: 0, parts }) = evs.get(at + 1) else { panic!("{:?}", evs.get(at + 1)) };
    let kinds: Vec<AddedKind> = parts.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [AddedKind::Hook, AddedKind::StartNote, AddedKind::File, AddedKind::File], "{parts:?}");
    assert!(parts[0].text.starts_with(nexo_core::agent::HOOK_CONTEXT_START) && parts[0].text.contains("it is monday"));
    assert!(
        parts[1].text.starts_with("[project at the start of this request]\nattached: 1 files"),
        "{}",
        parts[1].text
    );
    assert_eq!(parts[2].label, "note.md");
    assert!(parts[2].text.contains("remember this") && parts[3].text.contains("fn small()"), "{parts:?}");
    assert!(parts.iter().all(|p| !p.request_only && p.tokens() > 0));
    // the request carries the same bytes as before: the parts in front, the typed words, the parts behind
    let sent = client.requests()[0].messages[0].text();
    assert_eq!(
        sent,
        format!("{}\n\nsee\n\n{}\n\n{}\n\n{}", parts[0].text, parts[1].text, parts[2].text, parts[3].text)
    );
    // `/sent` gives that request back in sections
    r.send(AgentCommand::ShowSent);
    let evs = r.until(|e| matches!(e, AgentEvent::Sent { .. })).await;
    let Some(AgentEvent::Sent { model, sections }) = evs.last() else { panic!() };
    assert_eq!(model, "m1");
    assert_eq!(sections[0].heading, "System prompt");
    let user = sections.iter().find(|s| s.heading == "#1 user").expect("the user message");
    assert_eq!(user.text, sent);
    // nothing added: nothing told
    r.send(AgentCommand::Submit { text: "plain words".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::History { .. })));
    let hook_only =
        evs.iter().find_map(|e| if let AgentEvent::Added { parts, .. } = e { Some(parts.clone()) } else { None });
    assert!(
        hook_only.is_some_and(|p| p.iter().all(|x| x.kind == AddedKind::Hook)),
        "only the hook adds to a plain message"
    );
    r.stop().await;
}

/// A file the model wrote in this session, unchanged since, is not attached again by the token budget at
/// the next request: the model has it. A file changed outside the model since is attached.
#[tokio::test]
async fn a_file_the_model_wrote_is_not_attached_again_at_the_next_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("w1", "write", json!({"path": "made.rs", "content": "fn made() {}\n"}))),
        Reply::Events(text("written")),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.attach_budget = 4000;
    let set = tools().with(Arc::new(nexo_tools::write::Write));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "make it".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(std::fs::read_to_string(d.path().join("made.rs")).unwrap(), "fn made() {}\n");
    // the end of the request is noted just after its last event
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(d.path().join("fresh.rs"), "fn fresh() {}\n").unwrap();
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let sent = reqs.last().unwrap().messages.last().unwrap().text();
    assert!(sent.contains("@fresh.rs "), "a file the model never met is attached: {sent}");
    assert!(!sent.contains("@made.rs "), "the file the model wrote is not attached again: {sent}");
    r.stop().await;
}

/// A file the model's own command made during a request (generated data) is not attached again by the token
/// budget at the next request; a file the person made between requests is.
#[cfg(unix)]
#[tokio::test]
async fn a_file_a_command_of_the_model_made_is_not_attached_at_the_next_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "seq 1 40 > gen.json"}))),
        Reply::Events(text("made")),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.attach_budget = 4000;
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "make data".into(), attachments: vec![] });
    r.turn().await;
    assert!(std::fs::read_to_string(d.path().join("gen.json")).unwrap().starts_with("1\n2\n"));
    // the end of the request is noted just after its last event
    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(d.path().join("fresh.rs"), "fn fresh() {}\n").unwrap();
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let sent = reqs.last().unwrap().messages.last().unwrap().text();
    assert!(sent.contains("@fresh.rs "), "a file the person made is attached: {sent}");
    assert!(!sent.contains("@gen.json "), "the data the model's command made is not attached: {sent}");
    r.stop().await;
}

/// The scratch folder lives as long as the session: kept over turns, gone after `/clear`, after a
/// resume of another session and at shutdown.
#[cfg(unix)]
#[tokio::test]
async fn the_scratch_folder_lasts_for_the_session() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo saved > \"$NEXO_TMP/s.sh\""}))),
        Reply::Events(text("one")),
        Reply::Events(call("b2", "bash", json!({"command": "cp \"$NEXO_TMP/s.sh\" seen.txt"}))),
        Reply::Events(text("two")),
        Reply::Events(call("b3", "bash", json!({"command": "echo again > \"$NEXO_TMP/t.sh\""}))),
        Reply::Events(text("three")),
        Reply::Events(call("b4", "bash", json!({"command": "echo third > \"$NEXO_TMP/u.sh\""}))),
        Reply::Events(text("four")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client, config(PermissionMode::Auto), None, d.path(), set).await;
    let sid1 = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        e => panic!("{e:?}"),
    };
    let dir1 = nexo_tools::bash::tmp_dir(&sid1);
    r.send(AgentCommand::Submit { text: "save a script".into(), attachments: vec![] });
    r.turn().await;
    assert!(dir1.join("s.sh").is_file(), "kept after the turn ended");
    r.send(AgentCommand::Submit { text: "use it".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(std::fs::read_to_string(d.path().join("seen.txt")).unwrap().trim(), "saved");
    // /clear starts another session: the folder of the first goes, the second gets its own
    r.send(AgentCommand::Clear);
    let sid2 = match r.until(|e| matches!(e, AgentEvent::SessionChanged { .. })).await.pop() {
        Some(AgentEvent::SessionChanged { session_id, .. }) => session_id,
        e => panic!("{e:?}"),
    };
    assert_ne!(sid1, sid2);
    assert!(!dir1.exists(), "gone after /clear");
    let dir2 = nexo_tools::bash::tmp_dir(&sid2);
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    assert!(dir2.join("t.sh").is_file());
    // resuming the first session replaces the second
    r.send(AgentCommand::Resume { id: sid1.clone() });
    r.until(|e| matches!(e, AgentEvent::SessionChanged { .. })).await;
    assert!(!dir2.exists(), "gone after the session is replaced");
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    assert!(dir1.join("u.sh").is_file());
    r.stop().await;
    assert!(!dir1.exists(), "gone at shutdown");
}

#[tokio::test]
async fn a_plain_message_is_told_as_nothing_added_and_sent_asks_before_any_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::ShowSent);
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("nothing was sent yet")));
    r.send(AgentCommand::Submit { text: "just words".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::History { index: 0, .. })));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Added { .. })), "nothing was added: {evs:?}");
    assert_eq!(client.requests()[0].messages[0].text(), "just words");
    r.stop().await;
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

#[tokio::test]
async fn a_text_reply_is_streamed_recorded_and_priced() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hello there"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let ready = r.next().await;
    assert!(matches!(ready, AgentEvent::Ready { ref provider, ref model, .. } if provider == "mock" && model == "m1"));
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 0, .. }));
    assert!(matches!(evs[1], AgentEvent::TurnStart { turn: 1 }));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Context { policy, messages_in_view: 1, .. } if policy == "plain")));
    assert_eq!(text_of(&evs), "hello there");
    let usage = evs
        .iter()
        .find_map(
            |e| if let AgentEvent::Usage { session, cost_usd, .. } = e { Some((*session, *cost_usd)) } else { None },
        )
        .unwrap();
    assert_eq!(usage.0.total_input(), 3000);
    assert!((usage.1 - (1000.0 * 1.0 + 100.0 * 2.0 + 2000.0 * 0.1) / 1e6).abs() < 1e-12);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::AssistantDone { message } if message.text() == "hello there")));
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let req = &client.requests()[0];
    assert!(req.system.starts_with("You are nexo"));
    // the four tools given, the read-only sub-agent built from them and the question; the plan's
    // approval only in plan mode
    assert_eq!(req.tools.len(), 6);
    assert!(req.tools.iter().any(|t| t.name == "task"));
    assert!(req.tools.iter().any(|t| t.name == "ask_user"));
    assert!(!req.tools.iter().any(|t| t.name == "exit_plan"));
    assert_eq!(req.messages.len(), 1);
    r.stop().await;
}

#[tokio::test]
async fn a_tool_round_runs_the_tool_and_feeds_the_result_back() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "a.rs"}))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolCall { id, name, summary, .. } if id == "c1" && name == "read" && summary == "read a.rs")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { id } if id == "c1")));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolProgress { id, line } if id == "c1" && line == "progress line")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "c1" && output.content == "read:a.rs" && !output.is_error)));
    assert_eq!(text_of(&evs), "done");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let last = reqs[1].messages.last().unwrap();
    assert!(last.is_tool_results());
    match &last.content[0] {
        Part::ToolResult { call_id, content, is_error, .. } => {
            assert_eq!(call_id, "c1");
            assert_eq!(nexo_core::message::result_text(content), "read:a.rs");
            assert!(!is_error);
        }
        _ => panic!(),
    }
    r.stop().await;
}

#[tokio::test]
async fn read_only_calls_run_in_parallel_and_others_in_order() {
    let d = tempfile::tempdir().unwrap();
    let two = vec![
        StreamEvent::ToolCallStart { id: "r1".into(), name: "read".into() },
        StreamEvent::ToolCallEnd { id: "r1".into(), name: "read".into(), args: json!({"path": "a"}) },
        StreamEvent::ToolCallStart { id: "r2".into(), name: "grep".into() },
        StreamEvent::ToolCallEnd { id: "r2".into(), name: "grep".into(), args: json!({"path": "b"}) },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![Reply::Events(two), Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done: Vec<&str> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::ToolDone { id, .. } = e { Some(id.as_str()) } else { None })
        .collect();
    assert_eq!(done.len(), 2);
    let reqs = client.requests();
    let results = &reqs[1].messages.last().unwrap().content;
    assert_eq!(results.len(), 2);
    assert!(
        matches!(&results[0], Part::ToolResult { call_id, .. } if call_id == "r1"),
        "results keep the calls' order"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_command_among_reads_does_not_make_the_reads_wait_for_each_other() {
    /// Two of these finish only when both run at once: one after the other, neither ends.
    struct Meet(Arc<tokio::sync::Barrier>);
    #[async_trait]
    impl Tool for Meet {
        fn name(&self) -> &str {
            "meet"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "meet".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Search
        }
        fn summary(&self, _: &Value) -> String {
            "meet".into()
        }
        async fn run(&self, _: Value, _: &ToolContext) -> ToolOutput {
            match tokio::time::timeout(Duration::from_secs(5), self.0.wait()).await {
                Ok(_) => ToolOutput::ok("met"),
                Err(_) => ToolOutput::error("ran alone"),
            }
        }
    }
    let d = tempfile::tempdir().unwrap();
    let mut evs = Vec::new();
    for (id, name, args) in [("w", "write", json!({"path": "x"})), ("m1", "meet", json!({})), ("m2", "meet", json!({}))]
    {
        evs.push(StreamEvent::ToolCallStart { id: id.into(), name: name.into() });
        evs.push(StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args });
    }
    evs.extend([StreamEvent::Finish(FinishReason::ToolUse), StreamEvent::Done]);
    let client = Scripted::new(vec![Reply::Events(evs), Reply::Events(text("ok"))]);
    let tools = tools().with(Arc::new(Meet(Arc::new(tokio::sync::Barrier::new(2)))));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let results = &reqs[1].messages.last().unwrap().content;
    let outs: Vec<String> = results
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { call_id, content, .. } => Some(format!("{call_id}={}", text_of_parts(content))),
            _ => None,
        })
        .collect();
    assert_eq!(outs, ["w=write:x", "m1=met", "m2=met"], "the write first, then both reads at once");
    r.stop().await;
}

/// A `bash` that sleeps and notes when each command ran: shows which calls overlap.
struct Sleepy {
    ms: u64,
    log: Arc<Mutex<Vec<(String, std::time::Instant, std::time::Instant)>>>,
}

#[async_trait]
impl Tool for Sleepy {
    fn name(&self) -> &str {
        "bash"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "bash".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Exec
    }
    fn summary(&self, _: &Value) -> String {
        "bash".into()
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("").to_string();
        let start = std::time::Instant::now();
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(self.ms)) => {}
            _ = ctx.cancel.cancelled() => return ToolOutput::error("cancelled"),
        }
        self.log.lock().unwrap().push((command.clone(), start, std::time::Instant::now()));
        ToolOutput::ok(format!("ran {command}"))
    }
}

/// Runs one message of `bash` calls through `Sleepy`; (seconds taken, result texts in order, what ran when).
async fn bash_calls(
    commands: &[&str],
    ms: u64,
) -> (f64, Vec<String>, Vec<(String, std::time::Instant, std::time::Instant)>) {
    let d = tempfile::tempdir().unwrap();
    let mut evs = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let id = format!("b{i}");
        evs.push(StreamEvent::ToolCallStart { id: id.clone(), name: "bash".into() });
        evs.push(StreamEvent::ToolCallEnd { id, name: "bash".into(), args: json!({"command": c}) });
    }
    evs.extend([StreamEvent::Finish(FinishReason::ToolUse), StreamEvent::Done]);
    let client = Scripted::new(vec![Reply::Events(evs), Reply::Events(text("ok"))]);
    let log = Arc::new(Mutex::new(Vec::new()));
    let tools = ToolSet::new().with(Arc::new(Sleepy { ms, log: log.clone() }));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let t = std::time::Instant::now();
    r.until(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == &format!("b{}", commands.len() - 1))).await;
    let took = t.elapsed().as_secs_f64();
    r.turn().await;
    let reqs = client.requests();
    let outs = reqs[1]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { call_id, content, .. } => Some(format!("{call_id}={}", text_of_parts(content))),
            _ => None,
        })
        .collect();
    r.stop().await;
    let ran = log.lock().unwrap().clone();
    (took, outs, ran)
}

#[tokio::test]
async fn two_pure_checks_in_one_message_run_at_the_same_time() {
    let (took, outs, _) = bash_calls(&["cargo test a", "git status"], 1000).await;
    assert!(took < 1.6, "the two commands took {took:.2} s: they ran one after the other");
    assert_eq!(outs, ["b0=ran cargo test a", "b1=ran git status"], "results keep the calls' order");
}

#[tokio::test]
async fn a_command_that_changes_something_between_checks_keeps_the_order() {
    let (took, outs, ran) = bash_calls(&["cargo test a", "rm -rf build", "cargo test b"], 300).await;
    assert!(took >= 0.9, "the three commands took {took:.2} s: they overlapped");
    assert_eq!(outs, ["b0=ran cargo test a", "b1=ran rm -rf build", "b2=ran cargo test b"]);
    assert_eq!(ran.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["cargo test a", "rm -rf build", "cargo test b"]);
    assert!(ran.windows(2).all(|w| w[0].2 <= w[1].1), "each command ended before the next began");
}

#[tokio::test]
async fn an_interrupt_stops_checks_that_run_together() {
    let d = tempfile::tempdir().unwrap();
    let mut evs = Vec::new();
    for (id, c) in [("b0", "cargo test a"), ("b1", "cargo test b")] {
        evs.push(StreamEvent::ToolCallStart { id: id.into(), name: "bash".into() });
        evs.push(StreamEvent::ToolCallEnd { id: id.into(), name: "bash".into(), args: json!({"command": c}) });
    }
    evs.extend([StreamEvent::Finish(FinishReason::ToolUse), StreamEvent::Done]);
    let client = Scripted::new(vec![Reply::Events(evs)]);
    let tools = ToolSet::new().with(Arc::new(Sleepy { ms: 30_000, log: Arc::new(Mutex::new(Vec::new())) }));
    let mut r = start_with(client, config(PermissionMode::Auto), None, d.path(), tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::ToolStart { id } if id == "b1")).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    let stopped = evs.iter().filter(|e| matches!(e, AgentEvent::ToolDone { output, .. } if output.is_error)).count();
    assert_eq!(stopped, 2, "both commands stopped");
    r.stop().await;
}

fn text_of_parts(parts: &[Part]) -> String {
    parts.iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect()
}

#[tokio::test]
async fn a_write_asks_and_a_denial_reaches_the_model() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("w1", "write", json!({"path": "x.txt", "content": "new"}))),
        Reply::Events(text("understood")),
        Reply::Events(call("w2", "write", json!({"path": "x.txt", "content": "new"}))),
        Reply::Events(text("written")),
        Reply::Events(call("w3", "write", json!({"path": "y.txt", "content": "new"}))),
        Reply::Events(text("again")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "write".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    let req = match evs.last().unwrap() {
        AgentEvent::PermissionAsk { request } => request.clone(),
        _ => unreachable!(),
    };
    assert_eq!(req.id, "w1");
    assert_eq!(req.kind, ToolKind::Write);
    assert!(req.suggested_rules.contains(&"write(x.txt)".to_string()), "{:?}", req.suggested_rules);
    assert!(matches!(req.detail, nexo_core::tool::ToolDisplay::Diff { ref new, .. } if new == "new"));
    r.send(AgentCommand::Permission {
        id: "w1".into(),
        decision: PermissionDecision::Deny { reason: Some("not now".into()) },
    });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::PermissionResolved { id, .. } if id == "w1")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "w1" && output.is_error && output.content.contains("not now"))));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { .. })), "a denied tool never starts");
    assert_eq!(text_of(&evs), "understood");
    let reqs = client.requests();
    match &reqs[1].messages.last().unwrap().content[0] {
        Part::ToolResult { is_error, content, .. } => {
            assert!(is_error);
            assert!(nexo_core::message::result_text(content).contains("Permission denied"));
        }
        _ => panic!(),
    }
    // allow always: no ask next time
    r.send(AgentCommand::Submit { text: "write again".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    r.send(AgentCommand::Permission {
        id: "w2".into(),
        decision: PermissionDecision::AllowAlways { rule: "write".into(), persist: false },
    });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "w2" && !output.is_error)));
    r.send(AgentCommand::Submit { text: "and again".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the rule holds");
    assert_eq!(text_of(&evs), "again");
    r.stop().await;
}

#[tokio::test]
async fn an_interrupt_keeps_the_partial_reply_and_the_queue_waits() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("part".into())]), Reply::Events(text("second"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Queued { pending: 1 })).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::AssistantDone { message } if message.text() == "part")));
    // the queued request does not run on its own; the next message does
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("kept"))));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TurnStart { turn: 2 })));
    assert_eq!(text_of(&evs), "second");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests()[1].messages.len(), 3, "the partial reply stays in the history");
    r.stop().await;
}

#[tokio::test]
async fn interrupt_run_stops_the_turn_and_runs_the_queued_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("part".into())]),
        Reply::Events(text("second")),
        Reply::Events(text("third")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Queued { pending: 1 })).await;
    r.send(AgentCommand::InterruptRun);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("kept"))));
    // no further message: the queued request starts by itself
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Queued { pending: 0 })));
    assert_eq!(text_of(&evs), "second");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs[1].messages.last().unwrap().text(), "two", "the queued text is the request");
    // the agent is idle again and takes the next message as usual
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    assert_eq!(text_of(&r.turn().await), "third");
    r.stop().await;
}

#[tokio::test]
async fn an_interrupt_cancels_a_running_tool() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo go; sleep 30"}))),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::ToolStart { .. })).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "b1" && output.is_error)));
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    let msgs = &client.requests()[1].messages;
    assert!(msgs[2].is_tool_results(), "the cancelled call still has its result");
    r.stop().await;
}

#[tokio::test]
async fn a_context_length_error_compacts_once_and_retries() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first answer")),
        Reply::Events(text("second answer")),
        Reply::Err(ProviderError::new(ErrorKind::ContextLength, "too long")),
        Reply::Events(text("SUMMARY OF ALL")),
        Reply::Events(text("third answer")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    for t in ["one", "two"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        r.turn().await;
    }
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("history compacted"))),
        "{evs:?}"
    );
    assert_eq!(text_of(&evs), "third answer");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(reqs[3].messages[0].text().contains("Transcript of the session"), "the summary call");
    assert_eq!(reqs[4].messages.len(), 1, "summary merged into the request in progress");
    let t = reqs[4].messages[0].text();
    assert!(t.starts_with("[summary]") && t.contains("SUMMARY OF ALL") && t.ends_with("three"), "{t}");
    r.stop().await;
}

fn big_call(id: &str, name: &str, path: &str) -> Vec<StreamEvent> {
    let mut evs = call(id, name, json!({"path": path}));
    evs[2] = StreamEvent::Usage(Usage { input: 5000, output: 5, ..Default::default() });
    evs
}

fn is_summary(r: &ModelRequest) -> bool {
    r.system == nexo_agent::compact::SUMMARY_SYSTEM && r.tools.is_empty()
}

#[tokio::test]
async fn compact_summarises_past_its_threshold_and_keeps_the_task_and_files() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events({
            // a long explanation with the first call: it is summarised away once a later call keeps the tail
            let mut evs = big_call("c1", "write", "b.rs");
            evs.insert(0, StreamEvent::TextDelta("x".repeat(20_000)));
            evs
        }),
        Reply::Events(big_call("c2", "read", "a.rs")),
        Reply::Events(text("SUMMARY ONE")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "fix the bug in a.rs".into(), attachments: vec![] });
    let evs = r.turn().await;
    let shown = text_of(&evs);
    assert!(shown.ends_with("xdone") && !shown.contains("SUMMARY"), "summaries are not shown as replies");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "one summary: the task alone was too little to summarise");
    assert!(!is_summary(&reqs[0]) && !is_summary(&reqs[1]) && is_summary(&reqs[2]) && !is_summary(&reqs[3]));
    // after the summary: the summary, then the last call with its result, nothing orphaned
    let m = &reqs[3].messages;
    assert_eq!(m.len(), 3, "{m:?}");
    let head = m[0].text();
    assert!(head.contains("[request] fix the bug in a.rs"), "{head}");
    assert!(head.contains("SUMMARY ONE") && head.contains("[files]") && head.contains("b.rs"), "{head}");
    assert!(m[1].content.iter().any(|p| matches!(p, Part::ToolCall { id, .. } if id == "c2")), "{m:?}");
    assert!(m[2].content.iter().any(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "c2")));
    let size = |i: usize| serde_json::to_string(&reqs[i].messages).unwrap().len();
    assert!(size(1) > 20_000 && size(3) < 2_000, "{} then {}", size(1), size(3));
    // the summary is billed like any request: 2 calls, 1 summary, 1 answer
    let session = evs
        .iter()
        .rev()
        .find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None })
        .unwrap();
    assert_eq!(session.total_input(), 2 * 5000 + 2 * 3000);
    // the summary's request is not the conversation's: it is spent but sized as no call
    let calls: Vec<(u64, u64)> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage { turn, call, .. } => Some((turn.total_input(), call.total_input())),
            _ => None,
        })
        .collect();
    assert_eq!(calls, vec![(5000, 5000), (5000, 5000), (3000, 0), (3000, 3000)]);
    r.stop().await;
}

#[tokio::test]
async fn a_retried_attempt_is_spent_but_does_not_count_in_the_size_of_the_request() {
    let d = tempfile::tempdir().unwrap();
    let mid = |id: &str, path: &str, used: u64| {
        let mut evs = call(id, "read", json!({"path": path}));
        evs[2] = StreamEvent::Usage(Usage { input: used, output: 5, ..Default::default() });
        evs
    };
    let client = Scripted::new(vec![
        Reply::Events({
            let mut evs = mid("c1", "a.rs", 1000);
            evs.insert(0, StreamEvent::TextDelta("x".repeat(20_000)));
            evs
        }),
        // the client retried this call once: the first attempt was billed 2500 and thrown away
        Reply::Events({
            let mut evs = mid("c2", "b.rs", 1000);
            evs.insert(0, StreamEvent::RetriedUsage(Usage { input: 2500, ..Default::default() }));
            evs
        }),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "fix the bug in a.rs".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), format!("{}done", "x".repeat(20_000)));
    let reqs = client.requests();
    assert!(reqs.iter().all(|q| !is_summary(q)), "the last request held 1000 tokens, not 3500: no summary");
    assert_eq!(reqs.len(), 3);
    let usages: Vec<(Usage, Usage)> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { turn, call, .. } = e { Some((*turn, *call)) } else { None })
        .collect();
    assert_eq!((usages[1].0.input, usages[1].1.input), (3500, 1000), "{usages:?}");
    let (session, cost) = evs
        .iter()
        .rev()
        .find_map(
            |e| if let AgentEvent::Usage { session, cost_usd, .. } = e { Some((*session, *cost_usd)) } else { None },
        )
        .unwrap();
    assert_eq!(session.input, 1000 + 3500 + 1000, "the retried attempt is spent");
    // 5500 in x 1 + 110 out x 2 + 2000 cached x 0.1, per million
    assert!((cost - 0.00592).abs() < 1e-9, "{cost}");
    r.stop().await;
}

#[tokio::test]
async fn compact_does_not_summarise_every_round_a_request_its_fixed_part_keeps_over_the_threshold() {
    let d = tempfile::tempdir().unwrap();
    // every request counts 5000 tokens (a long system prompt, many tools) while each round adds little
    let client = Scripted::new(vec![
        Reply::Events(big_call("c1", "read", "a.py")),
        Reply::Events(big_call("c2", "read", "b.py")),
        Reply::Events(big_call("c3", "read", "c.py")),
        Reply::Events(big_call("c4", "read", "d.py")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at the scripts".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(!reqs.iter().any(is_summary), "no summary of a summary each round");
    assert_eq!(reqs[4].messages.len(), 9, "the history is as it was");
    r.stop().await;
}

#[tokio::test]
async fn every_request_of_a_session_carries_its_id_also_after_a_resume() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs.iter().all(|q| q.session_id.as_deref() == Some(id.as_str())), "one id for the whole session");
    let client = Scripted::new(vec![Reply::Events(text("again"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert_eq!(client.requests()[0].session_id.as_deref(), Some(id.as_str()), "the same after a resume");
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let other = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert_ne!(other, id);
    assert_eq!(client.requests()[0].session_id.as_deref(), Some(other.as_str()), "another session, another id");
}

#[tokio::test]
async fn a_session_is_resumed_with_its_history_and_tool_outputs() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let metas = nexo_agent::session::list();
    let meta = metas.iter().find(|m| m.id == id).unwrap();
    assert_eq!(meta.title, "please read f");
    assert_eq!(meta.messages, 4);
    let client = Scripted::new(vec![Reply::Events(text("again"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    assert!(matches!(r.next().await, AgentEvent::Ready { session_id, .. } if session_id == id));
    let evs = r.until(|e| matches!(e, AgentEvent::Context { .. })).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, AgentEvent::Context { messages_total: 4, history_tokens, .. } if *history_tokens > 0)),
        "a resumed session shows its context before the first call"
    );
    let hist: Vec<usize> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::History { index, .. } = e { Some(*index) } else { None })
        .collect();
    assert_eq!(hist, vec![0, 1, 2, 3]);
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, elapsed_ms: 0 } if id == "c1" && output.content == "read:f")
    ));
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 4, .. }));
    assert_eq!(client.requests()[0].messages.len(), 5);
    r.stop().await;
}

#[tokio::test]
async fn max_turns_stops_a_loop_of_calls() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new((0..5).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect());
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 2;
    cfg.agent.on_limit = nexo_core::config::OnLimit::Ask;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "loop".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 2);
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("stopped after 2 tool rounds (agent.max_turns)"))),
        "the person is told why"
    );
    let asked = |q: &ModelRequest| serde_json::to_string(&q.messages).unwrap().contains("checkpoint");
    assert!(!client.requests().iter().any(asked), "no checkpoint is sent when the person is to be asked");
    r.stop().await;
}

#[tokio::test]
async fn loop_guard_stops_rounds_that_bring_nothing_new() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a"), "x").unwrap();
    let client =
        Scripted::new((0..8).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect());
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.guard_answer = false;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "loop".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert!(client.requests().len() < 8, "stopped before max_turns: {}", client.requests().len());
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("agent.loop_guard"))),
        "the person is told why"
    );
    r.stop().await;
}

#[tokio::test]
async fn stale_guard_stops_rounds_that_bring_no_line_not_seen_before() {
    // the loop guard off: only the rounds that bring nothing new stop the request
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new((0..8).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect());
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    cfg.agent.guard_answer = false;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 4, "one round with news, three without");
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("agent.stale_guard"))),
        "the person is told why"
    );
    r.stop().await;
}

#[tokio::test]
async fn stale_guard_does_not_count_a_reread_of_what_the_curator_left_out() {
    // two files of about 3000 tokens, a window that holds one of them: every read puts back the lines the last
    // view had dropped, so no round stalled, and the request goes on to its answer (with the whole history in the
    // view the same reads bring nothing new, and the guard stops them: the test above)
    struct Lines;
    #[async_trait]
    impl Tool for Lines {
        fn name(&self) -> &str {
            "read"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, args: &Value) -> String {
            format!("read {}", args.get("path").and_then(Value::as_str).unwrap_or(""))
        }
        async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
            let name = args.get("path").and_then(Value::as_str).unwrap_or("");
            ToolOutput::ok(
                (0..300).map(|k| format!("{name} line {k} holds its own value {}\n", k * 7)).collect::<String>(),
            )
        }
    }
    let d = tempfile::tempdir().unwrap();
    let mut replies: Vec<Reply> = (0..6)
        .map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": if i % 2 == 0 { "a" } else { "b" }}))))
        .collect();
    replies.push(Reply::Events(text("a and b read")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    cfg.context.curator_spec = "eoq=0;floor=0.6;target=2500;cap=5000;hard=60000;still=0".into();
    cfg.agent.max_turns = 20;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    let mut r = start_with(client.clone(), cfg, None, d.path(), ToolSet::new().with(Arc::new(Lines))).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read a and b".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "{evs:?}");
    assert_eq!(client.requests().len(), 7, "all six reads ran and the model answered");
    assert!(
        !evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("agent.stale_guard"))),
        "the guard did not stop a re-read of what the view had dropped"
    );
    r.stop().await;
}

#[tokio::test]
async fn the_curator_label_follows_what_each_call_moved() {
    struct Lines;
    #[async_trait]
    impl Tool for Lines {
        fn name(&self) -> &str {
            "read"
        }
        fn spec(&self) -> ToolSpec {
            ToolSpec { name: "read".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Read
        }
        fn summary(&self, args: &Value) -> String {
            format!("read {}", args.get("path").and_then(Value::as_str).unwrap_or(""))
        }
        async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
            let name = args.get("path").and_then(Value::as_str).unwrap_or("");
            ToolOutput::ok(
                (0..300).map(|k| format!("{name} line {k} holds its own value {}\n", k * 7)).collect::<String>(),
            )
        }
    }
    let d = tempfile::tempdir().unwrap();
    let mut replies: Vec<Reply> =
        (0..6).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": format!("f{i}")})))).collect();
    replies.push(Reply::Events(text("done")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    cfg.context.curator_spec = "eoq=0;floor=0.6;target=2500;cap=5000;hard=60000;still=0".into();
    cfg.agent.max_turns = 20;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    let mut r = start_with(client.clone(), cfg, None, d.path(), ToolSet::new().with(Arc::new(Lines))).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read the files".into(), attachments: vec![] });
    let evs = r.turn().await;
    let labels: Vec<String> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Context { label, .. } = e { Some(label.clone()) } else { None })
        .collect();
    assert_eq!(labels.len(), 7, "{labels:?}");
    // what the conversation appends is not the curator's doing: nothing here came back, so no call says "+"
    assert!(labels.iter().all(|l| !l.contains('+')), "{labels:?}");
    assert!(labels[..2].iter().all(|l| !l.contains("tok")), "no window pressure yet: {labels:?}");
    // once the window is full each call takes the oldest out, and the label says by how many tokens that
    // made the window in use smaller
    assert!(labels[2..].iter().all(|l| l.starts_with("curator -") && l.ends_with(" tok")), "{labels:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_guard_stop_asks_for_the_answer_without_tools() {
    let d = tempfile::tempdir().unwrap();
    let mut replies: Vec<Reply> =
        (0..4).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect();
    replies.push(Reply::Events(text("a is x")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "what is in a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "the request ends on an answer");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "one round with news, three without, then the answer");
    assert!(reqs[..4].iter().all(|q| !q.no_tool_call));
    assert!(
        !reqs[4].no_tool_call && reqs[4].tools == reqs[3].tools,
        "the answer is asked in words: the tools and the tool choice stay as they were, or the prompt cache misses"
    );
    let told =
        |q: &ModelRequest| serde_json::to_string(q.messages.last().unwrap()).unwrap().contains("Tool calls are off");
    assert!(told(&reqs[4]) && !reqs[..4].iter().any(told), "the model is told, next to the last outputs, to answer");
    let last = serde_json::to_string(reqs[4].messages.last().unwrap()).unwrap();
    assert!(
        last.contains("this request") && !last.contains("from here on"),
        "the stop holds for this request only, or the model keeps off the tools for the rest of the session"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_call_after_a_guard_stop_does_not_run() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new((0..8).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect());
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "loop".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 6, "four rounds, the call asked in words to answer, the call the protocol forbids tools");
    assert!(!reqs[4].no_tool_call && reqs[5].no_tool_call);
    let ran = |q: &ModelRequest| serde_json::to_string(q.messages.last().unwrap()).unwrap().contains("did not run");
    assert!(ran(&reqs[5]), "the call made after the stop did not run");
    r.stop().await;
}

#[tokio::test]
async fn a_call_after_a_guard_stop_gets_one_more_call_that_may_only_answer() {
    let d = tempfile::tempdir().unwrap();
    let mut replies: Vec<Reply> =
        (0..5).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a"})))).collect();
    replies.push(Reply::Events(text("a is x")));
    let client = Scripted::new(replies);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 3;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "what is in a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "the request still ends on an answer");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 6);
    assert!(reqs[..5].iter().all(|q| !q.no_tool_call) && reqs[5].no_tool_call);
    r.stop().await;
}

#[tokio::test]
async fn attachments_shell_model_and_clear() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("note.md"), "remember this").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit {
        text: "see".into(),
        attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("note.md") }],
    });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    match &evs[0] {
        AgentEvent::History { message, .. } => {
            let t = message.text();
            assert!(
                t.starts_with("see\n\n@note.md (attached in full, as it is now):\n```md\nremember this\n```"),
                "{t}"
            );
        }
        _ => panic!(),
    }
    r.send(AgentCommand::Shell { command: "echo hi; echo err >&2; exit 3".into() });
    let evs = r.until(|e| matches!(e, AgentEvent::ShellOutput { .. })).await;
    assert!(
        matches!(evs.last().unwrap(), AgentEvent::ShellOutput { output, exit_code: Some(3), .. } if output == "hi\nerr\n")
    );
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    assert!(matches!(r.next().await, AgentEvent::ModelChanged { model, .. } if model == "m2"));
    assert!(matches!(r.next().await, AgentEvent::ModelLimits { model, .. } if model == "m2"));
    assert!(matches!(r.next().await, AgentEvent::Effort { model, current: None, .. } if model == "m2"));
    r.send(AgentCommand::SetModel { provider: Some("nope".into()), model: "m2".into() });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("unknown provider")));
    r.send(AgentCommand::SetPermissionMode(PermissionMode::Auto));
    assert!(matches!(r.next().await, AgentEvent::PermissionModeChanged { mode: PermissionMode::Auto }));
    r.send(AgentCommand::Clear);
    let ev = r.next().await;
    assert!(matches!(&ev, AgentEvent::Ready { model, .. } if model == "m2"), "{ev:?}");
    assert!(matches!(r.next().await, AgentEvent::HistoryCleared));
    assert!(matches!(r.next().await, AgentEvent::SessionChanged { .. }));
    r.send(AgentCommand::Submit { text: "fresh".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 0, .. }), "a cleared history starts over");
    r.stop().await;
}

fn history_indexes(evs: &[AgentEvent]) -> Vec<usize> {
    evs.iter().filter_map(|e| if let AgentEvent::History { index, .. } = e { Some(*index) } else { None }).collect()
}

#[tokio::test]
async fn resume_in_place_replaces_the_conversation_and_its_file() {
    let d = tempfile::tempdir().unwrap();
    // session A: one request with a call
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // session B, fresh, then resume A in place
    let client = Scripted::new(vec![Reply::Events(text("b")), Reply::Events(text("c"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    assert_ne!(a, b);
    r.send(AgentCommand::Submit { text: "in b".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Resume { id: "nope".into() });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("cannot resume session nope")));
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Context { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Context { messages_total: 4, policy, .. }) if policy == "plain"));
    assert!(matches!(evs[0], AgentEvent::HistoryCleared));
    assert!(
        matches!(&evs[1], AgentEvent::SessionChanged { session_id, title } if *session_id == a && title == "please read f")
    );
    assert_eq!(history_indexes(&evs), vec![0, 1, 2, 3]);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "c1" && output.content == "read:f")));
    // the next request goes to A: index 4, and the model sees the five messages
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 4, .. }));
    assert_eq!(client.requests()[1].messages.len(), 5);
    r.stop().await;
    let metas = nexo_agent::session::list();
    assert_eq!(metas.iter().find(|m| m.id == a).unwrap().messages, 6);
    assert_eq!(metas.iter().find(|m| m.id == b).unwrap().messages, 2, "B kept what it had");
}

#[tokio::test]
async fn resume_and_rewind_wait_for_the_turn() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("half".into())])]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Resume { id });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("before resuming")));
    r.send(AgentCommand::Rewind { index: 0 });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("before rewinding")));
    r.send(AgentCommand::Interrupt);
    r.turn().await;
    r.stop().await;
}

#[tokio::test]
async fn a_session_another_nexo_holds_is_not_resumed() {
    let d = tempfile::tempdir().unwrap();
    let mut r = start(Scripted::new(vec![Reply::Events(text("a"))]), config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let file = nexo_agent::session::dir().join(format!("{a}.jsonl"));
    let lock = file.with_extension("jsonl.lock");
    std::fs::write(&lock, "1").unwrap(); // another nexo, still running, holds it
    let before = std::fs::read(&file).unwrap();
    // `/resume` from a nexo already running
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::HistoryCleared | AgentEvent::SessionChanged { .. })));
    assert!(matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("in use")), "{evs:?}");
    r.stop().await;
    assert_ne!(a, b);
    // `--resume` at startup
    let r = start(client.clone(), config(PermissionMode::Ask), Some(a.clone()), d.path()).await;
    let e = r.task.await.unwrap().unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    assert_eq!(std::fs::read(&file).unwrap(), before, "nothing written to the file another nexo holds");
    assert!(client.requests().is_empty(), "nothing spent");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "its lock stays");
    std::fs::remove_file(&lock).unwrap();
}

#[tokio::test]
async fn a_session_taken_over_is_opened_again_by_resume() {
    let d = tempfile::tempdir().unwrap();
    let mut r = start(
        Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]),
        config(PermissionMode::Ask),
        None,
        d.path(),
    )
    .await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    let lock = nexo_agent::session::dir().join(format!("{a}.jsonl.lock"));
    // another nexo takes it over: the next write finds the lock is not this one's
    std::fs::write(&lock, "1").unwrap();
    r.send(AgentCommand::Submit { text: "lost".into(), attachments: vec![] });
    r.turn().await;
    // it ends and lets go; the person opens the session again
    std::fs::remove_file(&lock).unwrap();
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. } | AgentEvent::SessionChanged { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::SessionChanged { .. })), "{evs:?}");
    assert!(lock.is_file(), "the session holds its lock again");
    r.stop().await;
}

#[tokio::test]
async fn resuming_the_session_in_use_keeps_its_lock() {
    let d = tempfile::tempdir().unwrap();
    let mut r = start(Scripted::new(vec![Reply::Events(text("a"))]), config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    let lock = nexo_agent::session::dir().join(format!("{a}.jsonl.lock"));
    assert!(lock.is_file());
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. } | AgentEvent::HistoryCleared)).await;
    assert!(lock.is_file(), "the session in use stays locked");
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::HistoryCleared)), "{evs:?}");
    r.stop().await;
}

#[tokio::test]
async fn rewind_cuts_before_the_calls_and_rewrites_the_file() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await; // history: user, assistant+call, results, assistant text
                    // index 2 lands between the call and its result: the cut moves before the call
    r.send(AgentCommand::Rewind { index: 2 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(matches!(evs[0], AgentEvent::HistoryCleared));
    assert!(matches!(&evs[1], AgentEvent::SessionChanged { session_id, .. } if *session_id == id));
    assert_eq!(history_indexes(&evs), vec![0]);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { .. })));
    assert!(
        matches!(evs.last().unwrap(), AgentEvent::Notice { text, .. } if text.contains("3 messages dropped, 1 kept"))
    );
    let on_disk = nexo_agent::Session::open(&id).unwrap();
    assert_eq!(on_disk.messages().len(), 1);
    assert!(on_disk.outputs().is_empty());
    assert_eq!(on_disk.meta().title, "please read f");
    assert!(on_disk.usage().input > 0, "spent tokens stay");
    // a second handle on the file would stop the next rewrite on Windows
    drop(on_disk);
    // the next request continues from the kept message
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 1, .. }));
    assert_eq!(client.requests()[2].messages.len(), 2);
    // an index past the end keeps everything; 0 drops everything
    r.send(AgentCommand::Rewind { index: 100 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert_eq!(history_indexes(&evs), vec![0, 1, 2]);
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(history_indexes(&evs).is_empty());
    assert_eq!(nexo_agent::Session::open(&id).unwrap().messages().len(), 0);
    r.stop().await;
}

#[tokio::test]
async fn export_writes_the_conversation_as_markdown() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    let out = d.path().join("out").join("conv.md");
    r.send(AgentCommand::Export { path: Some(out.clone()), format: None });
    let shown = out.display().to_string();
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text == format!("exported to {shown}")));
    let md = std::fs::read_to_string(&out).unwrap();
    assert!(md.starts_with("# please read f\n"));
    assert!(md.contains("## You\n\nplease read f"));
    assert!(md.contains("**read** read f\n\n```\nread:f\n```"));
    assert!(md.contains("## nexo\n\nread it"));
    assert!(md.contains("cost: $"));
    // no path: next to the session file; a relative path: in the working directory
    r.send(AgentCommand::Export { path: None, format: None });
    let default = data_dir().join("sessions").join(format!("{id}.md"));
    assert!(
        matches!(r.next().await, AgentEvent::Notice { text, .. } if text.ends_with(&default.display().to_string()))
    );
    assert!(default.is_file());
    r.send(AgentCommand::Export { path: Some(PathBuf::from("rel.md")), format: None });
    r.next().await;
    assert!(d.path().join("rel.md").is_file());
    r.stop().await;
}

#[tokio::test]
async fn queued_requests_are_counted_as_events() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("one".into())]),
        Reply::Events(text("two")),
        Reply::Events(text("three")),
    ]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "1".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Submit { text: "2".into(), attachments: vec![] });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 1 }));
    r.send(AgentCommand::Submit { text: "3".into(), attachments: vec![] });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 2 }));
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    // Esc: nothing queued runs on its own; only the next message starts a turn
    r.send(AgentCommand::Submit { text: "4".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("kept"))));
    assert_eq!(text_of(&evs), "two");
    r.stop().await;
}

#[tokio::test]
async fn unqueue_takes_a_waiting_request_out_by_text() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("one".into())]), Reply::Events(text("three"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "1".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Submit { text: "2".into(), attachments: vec![] });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 1 }));
    r.send(AgentCommand::Submit { text: "3".into(), attachments: vec![] });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 2 }));
    // a wrong index still finds the request by its text
    r.send(AgentCommand::Unqueue { index: 0, text: "2".into() });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 1 }));
    // one that is not waiting changes nothing
    r.send(AgentCommand::Unqueue { index: 0, text: "2".into() });
    assert!(matches!(r.next().await, AgentEvent::Queued { pending: 1 }));
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    // nothing runs by itself after Esc; the next message does, and "2" was taken out
    r.send(AgentCommand::Submit { text: "4".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "three");
    r.stop().await;
}

#[tokio::test]
async fn clear_tells_the_interface_to_start_over() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let first = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Clear);
    let second = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    assert_ne!(first, second);
    assert!(matches!(r.next().await, AgentEvent::HistoryCleared));
    assert!(
        matches!(r.next().await, AgentEvent::SessionChanged { session_id, title } if session_id == second && title.is_empty())
    );
    r.stop().await;
}

#[tokio::test]
async fn limits_are_reported_and_prices_can_be_set() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let evs = r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    match evs.last().unwrap() {
        AgentEvent::ModelLimits { pricing, source, context_window, .. } => {
            assert_eq!(pricing.unwrap().input, 1.0);
            assert_eq!(source, "config");
            assert_eq!(*context_window, None);
        }
        _ => unreachable!(),
    }
    let p = Pricing { input: 5.0, output: 10.0, cache_read: 0.5, cache_write: 0.0 };
    r.send(AgentCommand::SetPricing { pricing: p });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ModelLimits { source, pricing: Some(p), .. } if source == "config" && p.input == 5.0)
    ));
    assert_eq!(user_config().providers["mock"].pricing_for("m1").unwrap().output, 10.0);
    r.send(AgentCommand::Submit { text: "x".into(), attachments: vec![] });
    let evs = r.turn().await;
    let cost = evs
        .iter()
        .find_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None })
        .unwrap();
    // 1000 in × 5 + 100 out × 10 + 2000 cached × 0.5, per million
    assert!((cost - 0.007).abs() < 1e-9, "{cost}");
    r.send(AgentCommand::ShowModel);
    let evs = r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::ModelLimits { pricing: Some(p), .. }) if p.output == 10.0));
    r.stop().await;
}

#[tokio::test]
async fn a_max_output_error_caps_the_reply_and_retries_once() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Err(ProviderError::new(
            ErrorKind::BadRequest,
            "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
        )),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "fits now");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelLimits { max_output: Some(8192), source, .. } if source == "error")));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!((reqs[0].max_tokens, reqs[1].max_tokens), (16_384, 8_192));
    assert_eq!(user_config().providers["mock"].max_output["m1"], 8_192);
    r.stop().await;
}

#[tokio::test]
async fn a_context_error_with_a_number_records_the_window() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 131072 tokens. However, you requested 140000 tokens.",
        )),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ModelLimits { context_window: Some(131_072), source, .. } if source == "error")
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("context window of m1"))));
    assert_eq!(user_config().providers["mock"].context_window["m1"], 131_072);
    r.stop().await;
}

#[tokio::test]
async fn a_window_learned_smaller_gets_a_new_curated_view_before_a_compaction() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let too_long = || {
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 65536 tokens. However, you requested 70000 tokens.",
        ))
    };
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        too_long(),
        Reply::Events(text("after")),
        // the same window again: a new view would not help, the history is compacted
        too_long(),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("third")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert!(!notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(!is_summary(&reqs[2]) && reqs[2].messages == reqs[1].messages, "the history kept as it is");
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "third");
    assert!(notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 6);
    assert!(is_summary(&reqs[4]));
    r.stop().await;
}

#[tokio::test]
async fn each_curator_call_reports_what_it_added_right_after_its_map_and_the_plain_policy_does_not() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    let at = evs.iter().position(|e| matches!(e, AgentEvent::ContextMap { .. })).expect("a map");
    // a short conversation adds nothing, and the event still says so
    assert!(matches!(&evs[at + 1], AgentEvent::Curated { items } if items.is_empty()), "{evs:?}");
    r.stop().await;

    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Curated { .. })), "{evs:?}");
    r.stop().await;
}

#[tokio::test]
async fn an_unknown_model_is_reported_with_the_closest_names() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Err(
        ProviderError::new(ErrorKind::BadRequest, "The model `m3` does not exist or you do not have access to it.")
            .with_status(404),
    )]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetModel { provider: None, model: "m3".into() });
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("does not know the model `m3`") && text.contains("m1"))),
        "{evs:?}"
    );
    assert_eq!(client.requests().len(), 1, "no retry");
    r.stop().await;
}

// ------------------------------------------------------------ goals and todo

fn goal_of(evs: &[AgentEvent]) -> Option<(Option<String>, u32, u32)> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::Goal { text, round, max, .. } => Some((text.clone(), *round, *max)),
        _ => None,
    })
}

fn goal_state_of(evs: &[AgentEvent]) -> Option<GoalState> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::Goal { state, .. } => Some(state.clone()),
        _ => None,
    })
}

fn last_goal_log(evs: &[AgentEvent]) -> Option<nexo_core::agent::GoalLog> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::GoalLog(l) => Some(l.clone()),
        _ => None,
    })
}

fn user_texts(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::History { message, .. } if message.role == nexo_core::message::Role::User => {
                Some(message.text())
            }
            _ => None,
        })
        .collect()
}

const CONTINUE: &str = "[goal: continue]";

#[tokio::test]
async fn a_goal_starts_the_work_and_keeps_going_until_it_says_goal_done() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("working on it")),
        // the first continuation does real work: a call, then its reply
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("more work")),
        Reply::Events(call("c2", "read", json!({"path": "b.rs"}))),
        Reply::Events(text("all set\nGOAL DONE. Thanks!")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("  make the tests pass ".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("make the tests pass".into()), 0, 5)));
    // /goal starts the work by itself: the goal is the first request
    let t1 = r.turn().await;
    assert_eq!(user_texts(&t1), vec!["[goal set] make the tests pass"]);
    assert_eq!(text_of(&t1), "working on it");
    // the agent continues by itself, twice, then the model declares the goal met (with a closing sentence)
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()), "{:?}", user_texts(&t2));
    assert_eq!(goal_of(&t2).map(|g| g.1), Some(1));
    let t3 = r.turn().await;
    assert_eq!(goal_of(&t3).map(|g| g.1), Some(2));
    assert!(text_of(&t3).contains("GOAL DONE"));
    let after = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&after), Some((Some("make the tests pass".into()), 0, 5)));
    assert!(after.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("goal met"))));
    assert_eq!(goal_state_of(&after), Some(GoalState::Done));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(reqs.iter().all(|q| q.system.contains("Goal, in the person's words: make the tests pass\n")));
    r.stop().await;
}

#[tokio::test]
async fn the_agent_restates_the_goal_and_the_restatement_is_the_goal() {
    let dir = tempfile::tempdir().unwrap();
    let brief = json!({
        "want": "every test in the workspace passes",
        "why": "the release is blocked on red tests",
        "how": "fix the code, not the tests; keep the style",
        "done": "cargo test shows 0 failed"
    });
    let client =
        Scripted::new(vec![Reply::Events(call("g1", "goal_brief", brief)), Reply::Events(text("on it\nGOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("tests green pls".into()) });
    let t1 = r.turn().await;
    let want =
        "Want: every test in the workspace passes\nWhy: the release is blocked on red tests\nHow: fix the code, \
                not the tests; keep the style\nDone when: cargo test shows 0 failed";
    assert!(t1.iter().any(|e| matches!(e, AgentEvent::Goal { text: Some(t), .. } if t == want)));
    // the person's words are kept beside the restatement, with the call it took
    let log = last_goal_log(&t1).expect("the goal's log is sent");
    assert_eq!(log.original.as_deref(), Some("tests green pls"));
    assert_eq!(log.brief.as_deref(), Some(want));
    assert!(log.steps >= 1 && log.started_ms > 0, "{log:?}");
    let reqs = client.requests();
    assert!(reqs[0].system.contains("Goal, in the person's words: tests green pls\n"));
    assert!(reqs[0].tools.iter().any(|t| t.name == "goal_brief"));
    // from the next request on, the restatement stands in for the person's words
    assert!(reqs[1].system.contains(&format!("Goal, as the person means it:\n{want}\n")));
    assert!(!reqs[1].system.contains("tests green pls"));
    r.stop().await;
    // a resumed session keeps the restatement
    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs).and_then(|g| g.0), Some(want.to_string()));
    let log = last_goal_log(&evs).expect("a resumed goal sends its log");
    assert_eq!((log.original, log.brief.as_deref()), (None, Some(want)), "the restatement is all that was saved");
    r.stop().await;
}

#[tokio::test]
async fn without_a_goal_there_is_no_goal_brief_to_call() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    assert!(!client.requests()[0].tools.iter().any(|t| t.name == "goal_brief"));
    r.stop().await;
}

#[tokio::test]
async fn a_goal_set_while_a_turn_runs_only_pins_it() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.send(AgentCommand::SetGoal { text: Some("finish it".into()) });
    let t1 = r.turn().await;
    assert_eq!(user_texts(&t1), vec!["go"], "no second request while the turn runs");
    assert!(t1.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("from the next turn"))));
    // the goal is checked when that turn ends: the first continuation runs
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()));
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn a_goal_continuation_leaves_the_round_to_the_interface() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.send(AgentCommand::SetGoal { text: Some("finish it".into()) });
    r.turn().await;
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()));
    assert!(t2.iter().any(|e| matches!(e, AgentEvent::Goal { round: 1, .. })));
    assert!(
        !t2.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("goal: continuing"))),
        "the interface draws the line itself: {t2:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_continuation_without_tool_calls_ends_the_goal_loop() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("working")), Reply::Events(text("nothing more to do"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("tidy up".into()) });
    r.turn().await;
    // the first continuation always runs; it made no call, so the loop stops there
    let t2 = r.turn().await;
    assert_eq!(goal_of(&t2).map(|g| g.1), Some(1));
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("tidy up".into()), 0, 5)), "rounds reset");
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("the last round did nothing new".into())));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("goal paused: the last round"))));
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })));
    assert_eq!(client.requests().len(), 2);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_goal_stops_after_its_rounds_and_when_cleared() {
    let dir = tempfile::tempdir().unwrap();
    // every round calls a tool, so the loop only stops at the cap
    let mut replies = vec![Reply::Events(text("still at it"))];
    for i in 0..5 {
        replies.push(Reply::Events(call(&format!("c{i}"), "read", json!({"path": "a.rs"}))));
        replies.push(Reply::Events(text("still at it")));
    }
    replies.push(Reply::Events(text("again")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("never done".into()) });
    r.turn().await;
    for round in 1..=5u32 {
        let t = r.turn().await;
        assert_eq!(goal_of(&t).map(|g| g.1), Some(round), "round {round}");
    }
    let warned =
        r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("rounds without GOAL DONE"))).await;
    assert!(!warned.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })));
    let reset = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&reset).map(|g| g.1), Some(0), "rounds reset after the cap");
    assert_eq!(goal_state_of(&reset), Some(GoalState::Paused("5 rounds without GOAL DONE".into())));
    assert_eq!(client.requests().len(), 11, "the first turn and five continuations of two requests each");
    // clearing the goal: a turn ends and nothing follows
    r.send(AgentCommand::SetGoal { text: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((None, 0, 5)));
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. } | AgentEvent::Goal { .. })));
    assert_eq!(client.requests().len(), 12);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_resumed_session_brings_its_goal_back() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client, config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("keep it green".into()) });
    r.turn().await;
    r.stop().await;

    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("keep it green".into()), 0, 5)));
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done), "a met goal is still met after a resume");
    r.stop().await;
}

#[tokio::test]
async fn a_reply_that_asks_waits_for_the_answer_and_the_answer_resumes_the_goal() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("Two branches could take the fix.\n\n**Which one should I use, `main` or `next`?**")),
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("fixed on main")),
        Reply::Events(text("all green\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("fix the bug".into()) });
    r.turn().await;
    // the question stops the loop: no continuation answers it in the person's place
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Waiting));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("waiting for your answer"))));
    assert_eq!(client.requests().len(), 1);
    // the answer makes the goal active again, and the loop goes on after it
    r.send(AgentCommand::Submit { text: "main".into(), attachments: vec![] });
    let t2 = r.turn().await;
    assert_eq!(goal_state_of(&t2), Some(GoalState::Active));
    assert_eq!(user_texts(&t2), vec!["main"]);
    let t3 = r.turn().await;
    assert!(user_texts(&t3).contains(&CONTINUE.to_string()));
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 4);
    r.stop().await;
}

#[tokio::test]
async fn a_met_goal_is_not_restarted_by_the_next_message() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("done\nGOAL DONE")), Reply::Events(text("you're welcome"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    r.turn().await;
    r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    r.send(AgentCommand::Submit { text: "thanks".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no continuation after a met goal");
    assert_eq!(client.requests().len(), 2);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn esc_and_a_failed_turn_pause_the_goal_and_resume_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("half".into())]),
        Reply::Err(ProviderError::new(ErrorKind::BadRequest, "bad request")),
        Reply::Events(text("all set\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("tidy up".into()) });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    assert_eq!(end_of(&r.turn().await), TurnEnd::Interrupted);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("interrupted".into())));
    // resume starts a continuation at once; it fails, and the goal pauses again instead of looping
    r.send(AgentCommand::ResumeGoal);
    let t = r.turn().await;
    assert_eq!(goal_state_of(&t), Some(GoalState::Active));
    assert_eq!(user_texts(&t), vec![CONTINUE.to_string()]);
    assert_eq!(end_of(&t), TurnEnd::Error);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("the turn failed".into())));
    // each stop is kept with its reason and its time, the goal's own words are not touched
    let log = last_goal_log(&evs).expect("the goal's log is sent");
    let whys: Vec<&str> = log.stops.iter().map(|s| s.why.as_str()).collect();
    assert_eq!(whys, vec!["interrupted", "the turn failed"]);
    assert!(log.stops.iter().all(|s| s.at_ms > 0) && log.stops[0].at_ms <= log.stops[1].at_ms);
    assert_eq!(log.original.as_deref(), Some("tidy up"));
    assert_eq!(log.brief, None);
    r.send(AgentCommand::ResumeGoal);
    r.turn().await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn a_paused_goal_holds_through_messages_until_resume() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("I need one thing from you.\nGOAL WAIT")),
        Reply::Events(text("noted")),
        Reply::Events(text("finished\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("clean the logs".into()) });
    r.turn().await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Waiting), "GOAL WAIT waits as a question does");
    r.send(AgentCommand::PauseGoal);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("paused by you".into())));
    // a message while paused is answered, and nothing more starts
    r.send(AgentCommand::Submit { text: "note this".into(), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(goal_state_of(&t), None, "the message does not touch a paused goal");
    r.stop().await;
    assert_eq!(client.requests().len(), 2);

    // the pause is kept with the session; resume goes on
    let mut r = start(client.clone(), config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("paused by you".into())));
    r.send(AgentCommand::ResumeGoal);
    let t = r.turn().await;
    assert_eq!(user_texts(&t), vec![CONTINUE.to_string()]);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn a_todo_call_shows_the_list_and_the_replay_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let items = json!({"items": [{"text": "read", "status": "done"}, {"text": "fix", "status": "in_progress"}, {"text": "test"}]});
    let client = Scripted::new(vec![Reply::Events(call("c1", "todo", items)), Reply::Events(text("on it"))]);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    // the todo tool never asks, even in ask mode
    let mut r = start_with(client, config(PermissionMode::Ask), None, dir.path(), todo_tools).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { output, .. } => Some(output.clone()),
        _ => None,
    });
    let out = done.expect("the todo call ran");
    assert_eq!(out.content, "[x] read\n[>] fix\n[ ] test\n1/3 done");
    assert!(matches!(&out.display, nexo_core::ToolDisplay::Todo { items } if items.len() == 3));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolCall { summary, .. } if summary == "todo: 1/3 done")));
    r.stop().await;

    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Ask), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { output, .. } if matches!(output.display, nexo_core::ToolDisplay::Todo { .. }))));
    r.stop().await;
}

#[tokio::test]
async fn the_model_setup_checks_saves_switches_and_sets_the_effort() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        // the first try asks for a reasoning level; this server does not take one
        Reply::Err(ProviderError::new(ErrorKind::BadRequest, "Unsupported parameter: 'reasoning_effort'")),
        Reply::Events(call("c1", "ping", json!({}))),
        Reply::Events(text("on the new model")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    let probe = nexo_core::ServerProbe {
        kind: "openai-compatible".into(),
        protocol: "openai-chat".into(),
        base_url: "https://gw.example/v1".into(),
        models: vec![nexo_core::ModelInfo {
            id: "think-1".into(),
            context_window: Some(64_000),
            reasoning: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let cfg = ProviderConfig::new("openai-chat", "https://gw.example/v1", Some("GW_KEY"));
    r.send(AgentCommand::CheckModel { name: "gw".into(), probe, cfg: cfg.clone(), model: "think-1".into(), key: None });
    let evs = r.until(|e| matches!(e, AgentEvent::ModelChecked { .. })).await;
    let AgentEvent::ModelChecked { check, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(check.error.is_none(), "{check:?}");
    assert!(check.called_tool, "the model called the test tool");
    assert!(check.info.efforts.is_empty(), "the refused level leaves no levels: {check:?}");
    assert!(check.sources.iter().any(|s| s.contains("refused the reasoning level")), "{:?}", check.sources);
    let reqs = client.requests();
    assert!(reqs[0].reasoning.is_some() && reqs[1].reasoning.is_none());
    assert_eq!(reqs[1].tools[0].name, "ping");

    // save it with what the setup found, plus levels to choose from
    let mut save = cfg;
    save.context_window.insert("think-1".into(), 64_000);
    save.efforts.insert("think-1".into(), vec!["low".into(), "high".into()]);
    r.send(AgentCommand::SaveModel { name: "gw".into(), cfg: save, model: "think-1".into(), key: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelChanged { provider, model } if provider == "gw" && model == "think-1")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { context_window: Some(64_000), .. })));
    let saved = user_config();
    assert_eq!(saved.provider.as_deref(), Some("gw"));
    assert_eq!(saved.providers["gw"].api_key_env.as_deref(), Some("GW_KEY"));

    r.send(AgentCommand::SetEffort { effort: Some("medium".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("low, high"))));
    r.send(AgentCommand::SetEffort { effort: Some("high".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Effort { current: Some(c), .. }) if c == "high"));
    assert_eq!(user_config().providers["gw"].effort["think-1"], "high");
    r.send(AgentCommand::Submit { text: "x".into(), attachments: vec![] });
    r.turn().await;
    let last = client.requests().pop().unwrap();
    assert_eq!(last.model, "think-1");
    assert!(matches!(last.reasoning, Some(nexo_core::ReasoningConfig::Effort(nexo_core::Effort::High))));
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_puts_back_the_files_the_agent_wrote() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt"}))),
        Reply::Events(call("c2", "write", json!({"path": "new.txt"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "change them".into(), attachments: vec![] });
    r.turn().await;
    // what the (fake) tools would have done
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    std::fs::write(d.path().join("new.txt"), "created").unwrap();
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(text.contains("2 files put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "old");
    assert!(!d.path().join("new.txt").exists(), "a file the agent created is removed");
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_after_a_compaction_keeps_the_writes_the_summary_holds() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt"}))),
        Reply::Events(text("done")),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "change it".into(), attachments: vec![] });
    r.turn().await;
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    r.send(AgentCommand::Compact);
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("history compacted"))).await;
    r.send(AgentCommand::Submit { text: "something else".into(), attachments: vec![] });
    r.turn().await;
    // drop only the last request: the write before the compaction stays
    r.send(AgentCommand::Rewind { index: 1 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(!text.contains("put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "changed");
    r.stop().await;
}

#[tokio::test]
async fn an_attached_image_goes_as_an_image_part() {
    let d = tempfile::tempdir().unwrap();
    // the eight bytes every PNG starts with are enough for the attachment
    std::fs::write(d.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("a red square"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit {
        text: "what is in @shot.png".into(),
        attachments: vec![nexo_core::agent::Attachment { path: PathBuf::from("shot.png") }],
    });
    r.turn().await;
    let req = client.requests().pop().unwrap();
    let user = req.messages.iter().find(|m| m.role == nexo_core::message::Role::User).unwrap();
    assert!(user.text().contains("@shot.png: attached as an image"));
    assert!(user.content.iter().any(|p| matches!(
        p,
        Part::Image { source: nexo_core::message::ImageSource::Base64 { media_type, data } }
            if media_type == "image/png" && data == "iVBORw0KGgo="
    )));
    r.stop().await;
}

#[cfg(unix)]
#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_blocks_a_call_and_the_model_reads_why() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "write", json!({"path": "x"}))), Reply::Events(text("ok"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(nexo_core::config::HookConfig {
        event: "pre_tool".into(),
        matcher: Some("write|edit".into()),
        command: "echo 'writes are frozen today' >&2; exit 2".into(),
        ..Default::default()
    });
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "write it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { output, .. } if output.is_error && output.content.contains("writes are frozen today"))));
    let second = &client.requests()[1];
    let result = second.messages.iter().flat_map(|m| m.content.iter()).find_map(|p| match p {
        Part::ToolResult { content, .. } => content.first().cloned(),
        _ => None,
    });
    assert!(matches!(result, Some(Part::Text { text }) if text.contains("Blocked by a hook: writes are frozen today")));
    r.stop().await;
}

#[tokio::test]
async fn a_task_runs_a_read_only_sub_agent_whose_report_is_the_result() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        // the main model hands a question over
        Reply::Events(call(
            "t1",
            "task",
            json!({"description": "find the parser", "prompt": "where is parsing done?"}),
        )),
        // the sub-agent reads one file, tries to leave the project and to write, then reports
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "s1".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s1".into(), name: "read".into(), args: json!({"path": "src/parse.rs"}) },
            StreamEvent::ToolCallStart { id: "s2".into(), name: "read".into() },
            StreamEvent::ToolCallEnd {
                id: "s2".into(),
                name: "read".into(),
                args: json!({"path": "../../etc/passwd"}),
            },
            StreamEvent::ToolCallStart { id: "s3".into(), name: "write".into() },
            StreamEvent::ToolCallEnd { id: "s3".into(), name: "write".into(), args: json!({"path": "x"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("parsing is in src/parse.rs:12")),
        Reply::Events(text("it is in src/parse.rs")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where do we parse?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
        _ => None,
    });
    assert_eq!(done.map(|o| o.content).as_deref(), Some("parsing is in src/parse.rs:12"));
    let reqs = client.requests();
    // the sub-agent's own call has only the read-only tools and its own system prompt
    assert!(reqs[1].system.starts_with("You are a sub-agent"));
    let names: Vec<&str> = reqs[1].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"read") && names.contains(&"grep") && !names.contains(&"write") && !names.contains(&"task"),
        "{names:?}"
    );
    let back = reqs[2].messages.last().unwrap();
    let said: Vec<String> = back
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => {
                content.first().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() })
            }
            _ => None,
        })
        .collect();
    assert!(said[1].contains("outside the project") && said[2].contains("only read"), "{said:?}");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line.contains("read"))));
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_starts_with_a_briefing_of_what_the_session_read() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    let body: String = (1..=40)
        .map(|i| {
            if i == 21 {
                "fn parse_line(s: &str) -> Token { todo!() }\n".to_string()
            } else {
                format!("// filler {i}\n")
            }
        })
        .collect();
    std::fs::write(d.path().join("src/parser.rs"), body).unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "src/parser.rs"}))),
        Reply::Events(call(
            "t1",
            "task",
            json!({"description": "fix the parser", "prompt": "make parse_line in src/parser.rs return a Token"}),
        )),
        Reply::Events(text("parse_line is at src/parser.rs:21")),
        Reply::Events(text("done")),
    ]);
    let cfg = config(PermissionMode::Ask);
    let set = tools().with(Arc::new(nexo_tools::read::Read));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at the parser".into(), attachments: vec![] });
    let evs = r.turn().await;
    let reqs = client.requests();
    // the sub-agent's first request: the briefing, then the task
    let first = &reqs[2].messages[0].content;
    let texts: Vec<&str> =
        first.iter().filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].starts_with("<briefing") && texts[0].contains("fn parse_line"), "{}", texts[0]);
    assert!(texts[0].contains("src/parser.rs") && !texts[0].contains("filler 1\n"), "{}", texts[0]);
    assert!(texts[1].contains("make parse_line"));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line.starts_with("briefed:"))));
    r.stop().await;
}

#[tokio::test]
async fn the_spend_limit_stops_before_the_call_that_would_pass_it() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("third"))]);
    let mut cfg = config(PermissionMode::Ask);
    // one reply of the script costs $0.0014 at m1's prices
    cfg.agent.max_cost = Some(0.001);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Budget { session_limit: Some(l), session_cost, .. } if *l == 0.001 && *session_cost > 0.001)));
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("spend limit reached") && text.contains("this session"))));
    assert_eq!(client.requests().len(), 1, "the second request never reached the model");
    // a higher limit lets the work go on
    r.send(AgentCommand::SetBudget { limit: Some(1.0) });
    r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn a_message_sent_during_a_turn_joins_it_before_the_next_call() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("w1", "write", json!({"path": "x.txt"}))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "write it".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    r.send(AgentCommand::Submit { text: "and say hi".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Queued { pending: 1 })).await;
    r.send(AgentCommand::Permission { id: "w1".into(), decision: PermissionDecision::Allow });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Queued { pending: 0 })));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Notice { level: Level::Info, .. })), "no notice of it");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::History { message, .. } if message.text() == "and say hi")));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "no turn of its own");
    let last = reqs[1].messages.last().unwrap();
    assert_eq!(last.text(), "and say hi");
    r.stop().await;
}

#[tokio::test]
async fn reading_an_unchanged_file_again_sends_a_note() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "a.txt"}))),
        Reply::Events(call("r2", "read", json!({"path": "a.txt"}))),
        Reply::Events(text("ok")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let results: Vec<String> = client.requests()[2]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => Some(nexo_core::message::result_text(content)),
            _ => None,
        })
        .collect();
    assert_eq!(results[0], "read:a.txt");
    assert!(results[1].contains("not repeated"), "{results:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_general_sub_agent_asks_for_its_writes_and_its_spending_is_counted() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(text("all done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    let req = match evs.last().unwrap() {
        AgentEvent::PermissionAsk { request } => request.clone(),
        _ => unreachable!(),
    };
    assert_eq!((req.id.as_str(), req.tool.as_str()), ("t1.s1", "write"));
    r.send(AgentCommand::Permission { id: req.id, decision: PermissionDecision::Allow });
    let evs = r.turn().await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "t1" && output.content == "wrote x.txt")
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolProgress { id, line } if id == "t1" && line == "→ write x.txt")));
    assert_eq!(text_of(&evs), "all done");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    assert!(reqs[1].system.contains("sub-agent") && reqs[1].tools.iter().any(|t| t.name == "write"));
    assert!(!reqs[1].tools.iter().any(|t| t.name == "task"), "a sub-agent does not start its own");
    // both of the sub-agent's calls are on the bill: 10+1000 of theirs, 10+1000 of the main agent's
    let session =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None });
    assert_eq!(session.unwrap().input, 10 + 1000 + 10 + 1000);
    r.stop().await;
}

#[tokio::test]
async fn a_background_command_lets_the_turn_end_and_its_end_reaches_the_model() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo hi", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("saw it")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    let mut evs = Vec::new();
    loop {
        let ev = r.raw_within(Duration::from_secs(30)).await;
        let idle = matches!(ev, AgentEvent::Idle { running_jobs: 0 });
        evs.push(ev);
        if idle {
            break;
        }
    }
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "b1" && output.content.starts_with("Started background job 1"))));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && !j.status.running()))));
    // the model hears that it ended: with the tool results, or in a turn of its own
    let reqs = client.requests();
    let told = reqs.iter().skip(1).any(|q| {
        q.messages
            .iter()
            .any(|m| m.role == nexo_core::message::Role::User && m.text().contains("[background jobs ended]"))
    });
    assert!(told, "the ended job is reported to the model");
    assert!(text_of(&evs).ends_with("saw it") || reqs.len() == 2);
    r.stop().await;
}

#[tokio::test]
async fn plan_mode_refuses_writes_until_the_plan_is_approved() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("w0", "write", json!({"path": "x.txt", "content": "new"}))),
        Reply::Events(call("q1", "ask_user", json!({"question": "Which file?", "options": ["x.txt", "y.txt"]}))),
        Reply::Events(call("p1", "exit_plan", json!({"plan": "1. write x.txt"}))),
        Reply::Events(call("w1", "write", json!({"path": "x.txt", "content": "new"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetPermissionMode(PermissionMode::Plan));
    r.until(|e| matches!(e, AgentEvent::PermissionModeChanged { mode: PermissionMode::Plan })).await;
    r.send(AgentCommand::Submit { text: "plan it".into(), attachments: vec![] });
    // the question: the answer reaches the model
    let evs = r.until(|e| matches!(e, AgentEvent::Question { .. })).await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "w0" && output.content.contains("plan mode"))
    ));
    let Some(AgentEvent::Question { question }) = evs.last() else { unreachable!() };
    assert_eq!((question.id.as_str(), question.options.len(), question.plan.is_none()), ("q1", 2, true));
    r.send(AgentCommand::Answer { id: "q1".into(), answer: Some("x.txt".into()) });
    // the plan: approved, edits go through
    let evs = r.until(|e| matches!(e, AgentEvent::Question { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::QuestionResolved { id, .. } if id == "q1")));
    let Some(AgentEvent::Question { question }) = evs.last() else { unreachable!() };
    assert_eq!(question.plan.as_deref(), Some("1. write x.txt"));
    r.send(AgentCommand::Answer { id: "p1".into(), answer: Some(nexo_agent::ask::APPROVE_AUTO.into()) });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::PermissionModeChanged { mode: PermissionMode::AcceptEdits })));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "accept-edits: no ask");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "w1" && !output.is_error)));
    let reqs = client.requests();
    assert!(reqs[0].system.contains("Plan mode is on"));
    assert!(reqs[0].tools.iter().any(|t| t.name == "exit_plan"));
    let answered = |i: usize| match &reqs[i].messages.last().unwrap().content[0] {
        Part::ToolResult { content, .. } => nexo_core::message::result_text(content),
        _ => panic!(),
    };
    assert_eq!(answered(2), "The person answered: x.txt");
    assert!(answered(3).contains("approved"));
    assert!(!reqs[3].system.contains("Plan mode is on"), "the plan's directive goes with plan mode");
    assert!(!reqs[3].tools.iter().any(|t| t.name == "exit_plan"));
    r.stop().await;
}

#[tokio::test]
async fn an_interrupt_drops_the_question() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(call(
        "q1",
        "ask_user",
        json!({"question": "Which?", "options": ["a", "b"]}),
    ))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::Question { .. })).await;
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::QuestionResolved { id, answer: None } if id == "q1")));
    assert_eq!(end_of(&evs), TurnEnd::Interrupted);
    r.stop().await;
}

#[tokio::test]
async fn another_sessions_memory_goes_with_the_next_request_and_sessions_go_to_the_trash() {
    let d = tempfile::tempdir().unwrap();
    // session A: what it found
    let client = Scripted::new(vec![Reply::Events(text(
        "Finding: the tokenizer drops tab characters before the parser sees them.\nPlan: keep tabs in tokenize_line.",
    ))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "why does the tokenizer lose tabs?".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // session B takes what A knows
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::ShareMemory { id: Some(b.clone()), focus: None });
    let ev = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(matches!(ev.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("this session")), "{ev:?}");
    r.send(AgentCommand::ShareMemory { id: Some(a.clone()), focus: Some("tokenizer tabs".into()) });
    let ev = r.until(|e| matches!(e, AgentEvent::MemoryShared { .. })).await;
    assert!(matches!(ev.last(), Some(AgentEvent::MemoryShared { pending }) if pending.len() == 1 && pending[0].0 == a));
    r.send(AgentCommand::Submit { text: "fix it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::MemoryShared { pending } if pending.is_empty())));
    let sent = client.requests()[0].messages.last().unwrap().text();
    assert!(sent.starts_with(&format!("<memory from=\"{a}\"")), "{sent}");
    assert!(sent.contains("Finding: the tokenizer drops tab characters"), "{sent}");
    assert!(sent.ends_with("fix it"), "{sent}");
    // the interface is told what came from the other session apart from the typed words, and the message
    // sent is exactly that text in front of them
    let told =
        evs.iter().find_map(|e| if let AgentEvent::Added { parts, .. } = e { Some(parts.clone()) } else { None });
    let told = told.expect("an Added event");
    assert!(told.len() == 1 && told[0].kind == AddedKind::Memory && !told[0].request_only, "{told:?}");
    assert_eq!(sent, format!("{}\n\nfix it", told[0].text));
    // the one in use stays; another goes to the trash and comes back
    r.send(AgentCommand::DeleteSession { id: b.clone() });
    let ev = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(matches!(ev.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("in use")), "{ev:?}");
    r.send(AgentCommand::DeleteSession { id: a.clone() });
    let ev = r.until(|e| matches!(e, AgentEvent::SessionDeleted { .. })).await;
    assert!(matches!(ev.last(), Some(AgentEvent::SessionDeleted { id, .. }) if *id == a));
    assert!(!nexo_agent::session::list().iter().any(|m| m.id == a));
    r.send(AgentCommand::RestoreSession { id: a.clone() });
    let ev = r.until(|e| matches!(e, AgentEvent::SessionRestored { .. })).await;
    assert!(
        matches!(ev.last(), Some(AgentEvent::SessionRestored { id, title }) if *id == a && title.contains("tokenizer"))
    );
    assert!(nexo_agent::session::list().iter().any(|m| m.id == a));
    r.stop().await;
}

#[tokio::test]
async fn a_call_written_as_text_runs_as_a_call() {
    let d = tempfile::tempdir().unwrap();
    let written = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"read\">\n<｜｜DSML｜｜ parameter name=\"path\" string=\"true\">a</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
    let client = Scripted::new(vec![Reply::Events(text(written)), Reply::Events(text("a is x"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let AgentEvent::Ready { session_id: id, .. } = r.next().await else { panic!() };
    r.send(AgentCommand::Submit { text: "what is in a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "the markup is not the answer");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "the call ran and the model was asked again");
    let sent = serde_json::to_string(&reqs[1].messages).unwrap();
    assert!(sent.contains("read:a") && !sent.contains("DSML"), "the call and its output go back, not the markup");
    r.stop().await;
    // a resumed session counts its turns from 0 again: a new call written as text gets an id of its own
    let client = Scripted::new(vec![Reply::Events(text(&written.replace(">a<", ">b<"))), Reply::Events(text("b"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "and b".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let ids: Vec<String> =
        reqs[1].messages.iter().flat_map(|m| m.tool_calls().map(|(id, _, _)| id.to_string())).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "{ids:?}");
    r.stop().await;
}

#[tokio::test]
async fn recall_brings_back_a_message_exactly_and_only_the_curator_offers_it() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c2", "recall", json!({"message": "m00003"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    // the tool goes with the directives (an instruction the model gets only with the curator)
    cfg.context.curator_spec = "directives=1".into();
    let set = tools().with(Arc::new(nexo_tools::recall::Recall));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert!(reqs[0].tools.iter().any(|t| t.name == "recall"), "the curator with directives offers recall");
    let last = reqs[2].messages.last().unwrap();
    match &last.content[0] {
        Part::ToolResult { call_id, content, is_error, .. } => {
            assert_eq!(call_id, "c2");
            let got = nexo_core::message::result_text(content);
            assert_eq!(got, "m00003 (the output of read), as it was:\nread:a.rs", "{got}");
            assert!(!is_error);
        }
        _ => panic!(),
    }
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_out_of_rounds_still_reports_what_it_found() {
    let d = tempfile::tempdir().unwrap();
    let mut replies =
        vec![Reply::Events(call("t1", "task", json!({"description": "map it", "prompt": "where is x kept?"})))];
    // the explore kind has 16 rounds and spends every one on reading
    for i in 0..16 {
        let id = format!("s{i}");
        replies.push(Reply::Events(vec![
            StreamEvent::ToolCallStart { id: id.clone(), name: "read".into() },
            StreamEvent::ToolCallEnd { id, name: "read".into(), args: json!({"path": "a.rs"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]));
    }
    replies.push(Reply::Events(text("x is kept in a.rs:3; b.rs not checked")));
    replies.push(Reply::Events(text("done")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where is x?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
        _ => None,
    });
    let done = done.unwrap();
    assert!(!done.is_error && done.content == "x is kept in a.rs:3; b.rs not checked", "{done:?}");
    let reqs = client.requests();
    assert!(reqs[1].system.contains("You have 16 rounds"), "it knows its budget");
    let told = reqs[17]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .any(|p| matches!(p, Part::Text { text } if text.contains("No rounds left")));
    assert!(told, "the last request tells it to report");
    r.stop().await;
}

#[tokio::test]
async fn the_curator_offers_recall_and_recall_0_offers_the_tools_of_the_other_policies() {
    for (spec, offered) in [("", true), ("recall=0", false)] {
        let d = tempfile::tempdir().unwrap();
        let client = Scripted::new(vec![Reply::Events(text("done"))]);
        let mut cfg = config(PermissionMode::Ask);
        cfg.context.policy = "curator".into();
        cfg.context.curator_spec = spec.into();
        let set = tools().with(Arc::new(nexo_tools::recall::Recall));
        let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Done);
        let reqs = client.requests();
        assert_eq!(reqs[0].tools.iter().any(|t| t.name == "recall"), offered, "spec {spec:?}");
        r.stop().await;
    }
}

fn notices(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter().filter_map(|e| if let AgentEvent::Notice { text, .. } = e { Some(text.clone()) } else { None }).collect()
}

/// A reply that stops at max_tokens: `events` with the finish said as Length.
fn cut(mut events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    for e in &mut events {
        if let StreamEvent::Finish(f) = e {
            *f = FinishReason::Length;
        }
    }
    events
}

#[tokio::test]
async fn a_call_cut_by_max_tokens_does_not_run() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f"), "x").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(call("c2", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    // the cut call never ran; the whole one did
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "c1")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "c2")));
    assert!(
        notices(&evs).iter().any(|n| n.contains("cut at max_tokens while writing its calls")),
        "{:?}",
        notices(&evs)
    );
    // the model was told, as the result of its call
    let reqs = client.requests();
    let told = reqs[1].messages.last().unwrap();
    assert!(matches!(&told.content[0], Part::ToolResult { call_id, is_error: true, content, .. }
        if call_id == "c1" && nexo_core::message::result_text(content).contains("cut at max_tokens")));
    r.stop().await;
}

#[tokio::test]
async fn the_request_after_a_cut_call_leaves_it_out_and_is_sampled_warmer_once() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("f"), "x").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(call("c2", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.temperature = Some(0.0);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    let temps: Vec<_> = client.requests().iter().map(|q| q.temperature).collect();
    assert_eq!(temps, vec![Some(0.0), Some(0.6), Some(0.0)]);
    // the half-written call is not shown again: only that it was cut
    let reqs = client.requests();
    let sent = reqs[1].messages.iter().flat_map(|m| m.tool_calls()).find(|(id, _, _)| *id == "c1").unwrap();
    assert_eq!(sent.2, &json!({"left_out": "cut at max_tokens after 12 characters"}));
    r.stop().await;
}

#[tokio::test]
async fn two_replies_cut_in_a_row_stop_the_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(cut(call("c2", "read", json!({"path": "f"})))),
        Reply::Events(text("never")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(notices(&evs).iter().any(|n| n.contains("two replies in a row")), "{:?}", notices(&evs));
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn a_text_cut_by_max_tokens_is_said() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(cut(text("half an ans")))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "explain".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("cut at max_tokens")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_session_that_ended_between_a_call_and_its_result_goes_on() {
    let d = tempfile::tempdir().unwrap();
    data_dir();
    // the session was saved with the call and closed before the tool ran
    let mut s = nexo_agent::session::Session::create(d.path(), "mock", "m1").unwrap();
    s.append(&Message::user("read f")).unwrap();
    s.append(&Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", json!({"path": "f"}))])).unwrap();
    let id = s.id().to_string();
    drop(s);
    let client = Scripted::new(vec![Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    let sent = &reqs[0].messages;
    assert!(nexo_core::message::is_well_formed(sent), "{sent:?}");
    assert!(matches!(&sent[2].content[0], Part::ToolResult { call_id, is_error: true, content, .. }
        if call_id == "c1" && nexo_core::message::result_text(content).contains("the session ended before the tools ran")));
    r.stop().await;
    // the answer was saved: the session reads well formed from now on
    let s = nexo_agent::session::Session::open(&id).unwrap();
    assert!(nexo_core::message::is_well_formed(s.messages()));
}

/// The main agent's deny rules hold in the read-only sub-agent too, and it gets no `diagnostics`
/// (a language server may run the project's build scripts).
#[tokio::test]
async fn explore_respects_deny() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"description": "find the key", "prompt": "what is in .env?"}))),
        Reply::Events(vec![
            StreamEvent::ToolCallStart { id: "s1".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s1".into(), name: "read".into(), args: json!({"path": ".env"}) },
            StreamEvent::ToolCallStart { id: "s2".into(), name: "read".into() },
            StreamEvent::ToolCallEnd { id: "s2".into(), name: "read".into(), args: json!({"path": "src/a.rs"}) },
            StreamEvent::Finish(FinishReason::ToolUse),
            StreamEvent::Done,
        ]),
        Reply::Events(text("cannot read it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.deny = vec!["read(.env*)".into()];
    cfg.notices = vec!["a notice from loading".into()];
    let set = tools().with(Arc::new(nexo_tools::lsp::Diagnostics));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    let mut first = Vec::new();
    loop {
        let e = r.next().await;
        let ready = matches!(e, AgentEvent::ModelLimits { .. });
        first.push(e);
        if ready || first.len() > 8 {
            break;
        }
    }
    assert!(
        first.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text == "a notice from loading")),
        "{first:?}"
    );
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let names: Vec<&str> = reqs[1].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read") && !names.contains(&"diagnostics"), "{names:?}");
    let said: Vec<String> = reqs[2]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => {
                content.first().map(|c| if let Part::Text { text } = c { text.clone() } else { String::new() })
            }
            _ => None,
        })
        .collect();
    assert!(said[0].contains("Permission denied") && said[0].contains("read(.env*)"), "{said:?}");
    assert!(!said[1].contains("Permission denied"), "{said:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_puts_back_what_a_sub_agent_wrote() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "change a.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "a.txt"}))),
        Reply::Events(text("changed a.txt")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    r.turn().await;
    // what the (fake) write would have done
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(text.contains("1 file put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "old");
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_that_can_write_runs_in_the_background_when_asked() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "change a.txt", "background": true}))),
        Reply::Events(text("changed a.txt")),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client, config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.contains("Started sub-agent job"), "{out}");
    r.stop().await;
}

/// A write that takes a moment, so a sub-agent running beside another would let it in.
struct SlowWrite;

#[async_trait]
impl Tool for SlowWrite {
    fn name(&self) -> &str {
        "write"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "write".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn summary(&self, _args: &Value) -> String {
        "write".into()
    }
    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        tokio::time::sleep(Duration::from_millis(100)).await;
        ToolOutput::ok("written")
    }
}

#[tokio::test]
async fn sub_agents_that_can_write_run_one_after_the_other() {
    let d = tempfile::tempdir().unwrap();
    let two = vec![
        StreamEvent::ToolCallStart { id: "t1".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t1".into(),
            name: "task".into(),
            args: json!({"agent": "general", "prompt": "first"}),
        },
        StreamEvent::ToolCallStart { id: "t2".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t2".into(),
            name: "task".into(),
            args: json!({"agent": "general", "prompt": "second"}),
        },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![
        Reply::Events(two),
        // the first sub-agent writes, then reports; only then does the second start
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(text("first report")),
        Reply::Events(text("second report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.parallel_tasks = false;
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    let evs = r.turn().await;
    let report = |want: &str| {
        evs.iter().find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == want => Some(output.content.clone()),
            _ => None,
        })
    };
    assert_eq!(report("t1").as_deref(), Some("first report"));
    assert_eq!(report("t2").as_deref(), Some("second report"));
    let reqs = client.requests();
    // the second sub-agent's first request comes after the first one's two
    assert_eq!(reqs[3].messages.len(), 1, "{:?}", reqs[3].messages);
    assert!(reqs[3].messages[0].text().contains("second"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn moving_to_another_session_stops_the_jobs_of_the_one_left() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 30", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("in the fork")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Fork { index: None });
    r.until(|e| {
        matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && j.status == nexo_core::jobs::JobStatus::Killed))
    })
    .await;
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "in the fork");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3, "no turn of its own for the old session's job");
    assert!(
        !reqs[2].messages.iter().any(|m| m.text().contains("[background jobs ended]")),
        "the old session's job is not reported in the new one"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_goes_to_the_provider_and_model_asked_for_and_is_priced_there() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t0", "task", json!({"prompt": "look", "provider": "nope"}))),
        Reply::Events(call("t1", "task", json!({"prompt": "look", "provider": "Other", "model": "o1"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    let mut other = ProviderConfig::new("mock", "http://other", None);
    other.models = vec!["o1".into()];
    other.pricing.insert("o1".into(), Pricing { input: 10.0, output: 20.0, cache_read: 1.0, cache_write: 0.0 });
    cfg.providers.insert("other".into(), other);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look with o1 on other".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = |want: &str| {
        evs.iter()
            .find_map(|e| match e {
                AgentEvent::ToolDone { id, output, .. } if id == want => Some(output.clone()),
                _ => None,
            })
            .unwrap()
    };
    let refused = out("t0");
    assert!(refused.is_error && refused.content.contains("no provider `nope`"), "{refused:?}");
    assert!(refused.content.contains("mock, other"), "{refused:?}");
    let ran = out("t1");
    assert!(!ran.is_error && ran.content == "found it", "{ran:?}");
    let (reqs, bases) = (client.requests(), client.bases.lock().unwrap().clone());
    let task = reqs[0].tools.iter().find(|t| t.name == "task").unwrap();
    assert_eq!(task.parameters["properties"]["provider"]["enum"], json!(["mock", "other"]));
    let pd = task.parameters["properties"]["provider"]["description"].as_str().unwrap();
    assert!(!pd.contains("o1") && !pd.contains("Set up"), "{pd}");
    // the refused call asked nothing of any model
    assert_eq!(reqs.len(), 4);
    assert_eq!((reqs[2].model.as_str(), bases[2].as_str()), ("o1", "http://other"));
    assert_eq!((reqs[3].model.as_str(), bases[3].as_str()), ("m1", "http://mock"));
    let cost = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None })
        .next_back()
        .unwrap();
    // two rounds of calls and the answer at the main prices, the sub-agent at the other provider's
    let main = 2.0 * (10.0 * 1.0 + 5.0 * 2.0) + (1000.0 * 1.0 + 100.0 * 2.0 + 2000.0 * 0.1);
    let sub = 1000.0 * 10.0 + 100.0 * 20.0 + 2000.0 * 1.0;
    assert!((cost - (main + sub) / 1e6).abs() < 1e-12, "{cost}");
    r.stop().await;
}

/// Two sub-agents that change files, asked for in one message.
fn two_writers() -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: "t1".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t1".into(),
            name: "task".into(),
            args: json!({"agent": "general", "description": "first change", "prompt": "first"}),
        },
        StreamEvent::ToolCallStart { id: "t2".into(), name: "task".into() },
        StreamEvent::ToolCallEnd {
            id: "t2".into(),
            name: "task".into(),
            args: json!({"agent": "general", "description": "second change", "prompt": "second"}),
        },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

/// What each sub-agent's tool round got back, from its second request.
fn sub_results(client: &Scripted) -> Vec<String> {
    client
        .requests()
        .iter()
        .filter(|r| r.messages.len() == 3 && !r.messages[0].text().contains("two changes"))
        .map(|r| {
            r.messages[2]
                .content
                .iter()
                .filter_map(|p| match p {
                    Part::ToolResult { content, .. } => content.first().and_then(Part::as_text).map(str::to_string),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

#[tokio::test]
async fn sub_agents_that_can_write_run_at_the_same_time_on_their_own_files() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(two_writers()),
        // both start before either write is done: the writes take a moment
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(call("s2", "write", json!({"path": "y.txt"}))),
        Reply::Events(text("a report")),
        Reply::Events(text("a report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    // the second sub-agent asks for its first step while the first one is still writing
    assert_eq!((reqs[1].messages.len(), reqs[2].messages.len()), (1, 1), "{:?}", reqs[2].messages);
    assert_eq!(sub_results(&client), vec!["written", "written"]);
    r.stop().await;
}

#[tokio::test]
async fn a_file_one_sub_agent_is_changing_is_closed_to_another_at_the_same_time() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(two_writers()),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(call("s2", "write", json!({"path": "./sub/../x.txt"}))),
        Reply::Events(text("a report")),
        Reply::Events(text("a report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let set = tools().with(Arc::new(SlowWrite));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "two changes".into(), attachments: vec![] });
    r.turn().await;
    let mut got = sub_results(&client);
    got.sort();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].contains("is being changed by another sub-agent running at the same time"), "{got:?}");
    assert!(got[0].contains("first change") || got[0].contains("second change"), "{got:?}");
    assert_eq!(got[1], "written");
    r.stop().await;
}

/// A reply that only thinks.
fn thought(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta(s.into()),
        StreamEvent::Usage(Usage { input: 1000, output: 100, cache_read: 2000, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Length),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn a_reply_of_only_reasoning_is_asked_to_go_on() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(thought("now write it: first")), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("only reasoning")), "{:?}", notices(&evs));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let sent = &reqs[1].messages;
    assert!(nexo_core::message::is_well_formed(sent), "{sent:?}");
    assert!(sent.last().unwrap().text().contains("only reasoning"), "{sent:?}");
    // cut at max_tokens while thinking: more room only bought more reasoning, so the limit stays
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

#[tokio::test]
async fn replies_of_only_reasoning_stop_after_two_asks() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(thought("a")), Reply::Events(thought("b")), Reply::Events(thought("c"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    // the room stays at agent.max_tokens; the second ask says to build first
    let base = reqs[0].max_tokens;
    assert_eq!((reqs[1].max_tokens, reqs[2].max_tokens), (base, base));
    assert!(reqs[2].messages.last().unwrap().text().contains("Stop planning"));
    // the next request of the person starts from agent.max_tokens again
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(client.requests()[3].max_tokens, base);
    r.stop().await;
}

#[tokio::test]
async fn the_ask_to_go_on_carries_the_end_of_the_reasoning_the_provider_does_not_send_back() {
    let d = tempfile::tempdir().unwrap();
    let long = format!("{}PLAN: write ars.R with a log-concave hull, then test it", "x".repeat(20_000));
    let client = Scripted::new(vec![Reply::Events(thought(&long)), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let note = reqs[1].messages.last().unwrap().text();
    assert!(note.contains("PLAN: write ars.R with a log-concave hull, then test it"), "{note}");
    // only the end of it: a note as long as the thinking would cost what the cut already cost
    assert!(note.len() < 6_000, "{}", note.len());
    r.stop().await;
}

/// A reply that thinks `chars` characters in pieces, then says `after`, as the provider would stream it.
fn long_thought(chars: usize, after: Vec<StreamEvent>) -> Vec<StreamEvent> {
    let mut ev = vec![StreamEvent::Start { response_id: None, model: None }];
    for _ in 0..chars / 1000 {
        ev.push(StreamEvent::ReasoningDelta("x".repeat(1000)));
    }
    ev.extend(after);
    ev
}

fn then_text(s: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(s.into()),
        StreamEvent::Usage(Usage { input: 1000, output: 100, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn reasoning_past_the_budget_with_nothing_said_is_cut_and_asked_again() {
    let d = tempfile::tempdir().unwrap();
    // 40,000 characters are 10,000 tokens, past the 8,000 default; the text after them is never read
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")), "{:?}", notices(&evs));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    let note = reqs[1].messages.last().unwrap().text();
    assert!(note.contains("only reasoning") && note.contains("xxxx"), "{}", note.len());
    assert!(!reqs[1].messages.iter().any(|m| m.text().contains("never read")));
    // no more room: it would only buy more reasoning
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

#[tokio::test]
async fn after_two_cuts_the_next_call_runs_without_the_budget() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(long_thought(40_000, then_text("never read"))),
        Reply::Events(long_thought(40_000, then_text("the answer"))),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 3);
    let said: String = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(said.contains("the answer"), "{said}");
    assert!(!notices(&evs).iter().any(|n| n.contains("reasoned without answering")));
    r.stop().await;
}

#[tokio::test]
async fn a_reply_that_only_reasons_ends_with_a_notice() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning_budget = 0;
    let only = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta("thinking".into()),
        StreamEvent::Usage(Usage { input: 1000, output: 10, ..Default::default() }),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![Reply::Events(only.clone()), Reply::Events(only.clone()), Reply::Events(only)]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("reasoned without answering")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_budget_of_zero_lets_the_reasoning_run() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning_budget = 0;
    let client = Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("answer")))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn reasoning_after_text_is_not_cut_by_the_budget() {
    let d = tempfile::tempdir().unwrap();
    let mut ev = vec![StreamEvent::Start { response_id: None, model: None }, StreamEvent::TextDelta("first ".into())];
    ev.extend(long_thought(40_000, then_text("last")).into_iter().skip(1));
    let client = Scripted::new(vec![Reply::Events(ev)]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn reasoning_after_a_call_started_is_not_cut_by_the_budget() {
    let d = tempfile::tempdir().unwrap();
    let mut ev = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ToolCallStart { id: "c1".into(), name: "read".into() },
    ];
    ev.extend(long_thought(40_000, then_text("last")).into_iter().skip(1));
    let client = Scripted::new(vec![Reply::Events(ev), Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_reasoning_level_set_by_the_person_skips_the_budget() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.reasoning = Some("high".into());
    let client = Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("answer")))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 1);
    assert!(!notices(&evs).iter().any(|n| n.contains("agent.reasoning_budget")));
    r.stop().await;
}

#[tokio::test]
async fn a_cut_stream_is_billed_from_what_was_streamed() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    let billed: Vec<u64> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { call, .. } = e { Some(call.output) } else { None })
        .collect();
    assert!(billed.iter().any(|o| *o >= 8_000), "{billed:?}");
    r.stop().await;
}

#[tokio::test]
async fn reasoning_that_ended_on_its_own_keeps_the_room() {
    let d = tempfile::tempdir().unwrap();
    let mut done = thought("all thought out");
    done[3] = StreamEvent::Finish(FinishReason::Stop);
    let client = Scripted::new(vec![Reply::Events(done), Reply::Events(text("ok"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs[1].max_tokens, reqs[0].max_tokens);
    r.stop().await;
}

// ---------------------------------------------------------------- mail between sessions

async fn start_mail(client: Arc<Scripted>, cwd: &std::path::Path, mail: Arc<nexo_core::mailbox::Mailbox>) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client,
        resume: None,
        gate: None,
        mail: Some(mail),
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    Running { handle, task }
}

fn sessions(root: &std::path::Path) -> (Arc<nexo_core::mailbox::Mailbox>, Arc<nexo_core::mailbox::Mailbox>) {
    use nexo_core::mail::Inbound;
    use nexo_core::mailbox::Mailbox;
    let cwd = root.to_string_lossy().to_string();
    (
        Mailbox::open(root, "aaaa1111", "api", &cwd, false, Inbound::Auto).unwrap(),
        Mailbox::open(root, "bbbb2222", "web", &cwd, false, Inbound::Auto).unwrap(),
    )
}

#[tokio::test]
async fn a_mail_is_delivered_at_an_idle_point_as_a_marked_message_and_read_when_answered() {
    use nexo_core::mail::{FromKind, Stage};
    let d = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let (api, web) = sessions(root.path());
    let client = Scripted::new(vec![Reply::Events(text("on it"))]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    // a turn is running as far as the host knows: the mail arrives and waits
    web.set_idle(false);
    api.send(&web_id, "please run the parser tests", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert!(t.deliver.is_empty(), "never while busy");
    assert!(t.events.iter().any(|e| matches!(e, AgentEvent::MailArrived { from_name, .. } if from_name == "api")));
    assert!(client.requests().is_empty(), "the model heard nothing");
    // the agent says idle: the host takes the mail and hands it over
    web.set_idle(true);
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1);
    r.send(AgentCommand::Deliver { mails: t.deliver });
    let evs = r.turn().await;
    let pos = |f: &dyn Fn(&AgentEvent) -> bool| evs.iter().position(f).unwrap_or(usize::MAX);
    let delivered = pos(&|e| matches!(e, AgentEvent::MailDelivered { .. }));
    let started = pos(&|e| matches!(e, AgentEvent::TurnStart { .. }));
    let read = pos(&|e| matches!(e, AgentEvent::MailRead { .. }));
    assert!(delivered < started && started < read, "delivered, then the turn, then read: {evs:?}");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let req = &client.requests()[0];
    let last = req.messages.last().unwrap().text();
    assert!(last.contains("please run the parser tests") && last.contains("api"), "{last}");
    // the sender sees the ladder
    let mut seen = Vec::new();
    for _ in 0..3 {
        for e in api.tick().events {
            if let AgentEvent::MailStage { stage, .. } = e {
                seen.push(stage);
            }
        }
    }
    assert!(seen.contains(&Stage::Read), "{seen:?}");
    let names: Vec<String> = std::fs::read_dir(root.path().join("aaaa1111/receipts"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.iter().any(|n| n.ends_with(".delivered")) && names.iter().any(|n| n.ends_with(".read")), "{names:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_mail_handed_over_mid_turn_waits_for_the_turn_to_end() {
    use nexo_core::mail::FromKind;
    let d = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let (api, web) = sessions(root.path());
    let client =
        Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("part".into())]), Reply::Events(text("second"))]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    let m = api.send(&web_id, "late news", None, FromKind::Agent).unwrap();
    r.send(AgentCommand::Deliver { mails: vec![m] });
    // nothing reaches the model while the turn runs
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if let AgentEvent::MailDelivered { .. } = r.next().await {
                return;
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "no delivery mid-turn");
    assert_eq!(client.requests().len(), 1);
    r.send(AgentCommand::Interrupt);
    let evs = r.until(|e| matches!(e, AgentEvent::MailRead { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::MailDelivered { .. })));
    assert_eq!(client.requests().len(), 2);
    assert!(client.requests()[1].messages.last().unwrap().text().contains("late news"));
    r.stop().await;
}

#[tokio::test]
async fn the_agent_owns_the_idle_flag_a_fresh_session_takes_mail_and_a_running_turn_does_not() {
    use nexo_core::mail::FromKind;
    let d = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let (api, web) = sessions(root.path());
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        Reply::Hang(vec![StreamEvent::TextDelta("part".into())]),
        Reply::Events(text("third")),
    ]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    // nobody told the mailbox anything: a session that just started is between turns
    api.send(&web_id, "news before any turn", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1, "a fresh session is idle");
    r.send(AgentCommand::Deliver { mails: t.deliver });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    // after the turn it is idle again, and mail is handed over without anyone else saying so
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { .. }) {
            break;
        }
    }
    api.send(&web_id, "news after the turn", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1, "idle again after a turn");
    // that mail starts a turn that hangs: while it runs, nothing is handed over
    r.send(AgentCommand::Deliver { mails: t.deliver });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    api.send(&web_id, "news during the turn", None, FromKind::Agent).unwrap();
    assert!(web.tick().deliver.is_empty(), "never while a turn runs");
    r.send(AgentCommand::Interrupt);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { .. }) {
            break;
        }
    }
    assert_eq!(web.tick().deliver.len(), 1, "the mail that waited goes once the turn is over");
    r.stop().await;
}

#[tokio::test]
async fn asking_for_the_job_tails_answers_with_the_end_of_each_output() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo hi", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("saw it")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client, config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { running_jobs: 0 }) {
            break;
        }
    }
    r.send(AgentCommand::JobTails);
    loop {
        if let AgentEvent::JobTails { tails } = r.raw().await {
            assert_eq!(tails.len(), 1);
            assert_eq!(tails[0].id, 1);
            assert_eq!(tails[0].text, "hi");
            assert!(tails[0].ended_ago_ms.is_some());
            break;
        }
    }
    r.stop().await;
}

// ------------------------------------------------------------ checkpoints at agent.max_turns

/// A reply with text and then one call.
fn text_call(t: &str, id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }, StreamEvent::TextDelta(t.into())];
    evs.extend(call(id, name, args));
    evs
}

fn ck_config(max_turns: u32) -> Config {
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.max_turns = max_turns;
    // only the limit and the checkpoints stop these requests
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    cfg
}

const CONT: &str = "CHECKPOINT CONTINUE: more to do. NEXT: go on";

/// `max_turns = 2`: two calls, then a checkpoint, whose reply continues with a call, one more call, and so on
/// for `chunks` checkpoints; the reply to the last one is `last`. Each call writes a file of its own, so every
/// chunk makes progress.
fn writing_script(chunks: usize, last: Vec<StreamEvent>) -> Vec<Reply> {
    let mut out = Vec::new();
    for k in 0..(2 + 2 * chunks - 2) {
        let (id, args) = (format!("c{k}"), json!({"path": format!("f{k}")}));
        out.push(Reply::Events(if k >= 2 && k % 2 == 0 {
            text_call(CONT, &id, "write", args)
        } else {
            call(&id, "write", args)
        }));
    }
    out.push(Reply::Events(last));
    out
}

fn ck_notices(evs: &[AgentEvent]) -> Vec<(Level, String)> {
    evs.iter()
        .filter_map(|e| if let AgentEvent::Notice { level, text } = e { Some((*level, text.clone())) } else { None })
        .collect()
}

fn has_notice(evs: &[AgentEvent], needle: &str) -> bool {
    ck_notices(evs).iter().any(|(_, t)| t.contains(needle))
}

fn last_message_text(q: &ModelRequest) -> String {
    q.messages.last().unwrap().content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n")
}

async fn run_request(client: &Arc<Scripted>, cfg: Config, text: &str) -> Vec<AgentEvent> {
    let d = tempfile::tempdir().unwrap();
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: text.into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    evs
}

#[tokio::test]
async fn the_limit_sends_a_checkpoint_with_the_stats_header() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: all done\nThe work is finished.")),
    ]);
    let evs = run_request(&client, ck_config(2), "read a and b").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    let m = last_message_text(&reqs[2]);
    assert!(m.starts_with("[checkpoint 1/5] Session at this point: $"), "{m}");
    for want in [
        "tool calls",
        "steps",
        "cached",
        "Consider, based on all the progress",
        "Request: read a and b",
        "CHECKPOINT CONTINUE",
    ] {
        assert!(m.contains(want), "{want} in {m}");
    }
    assert!(m.chars().count() < 1500);
    assert!(has_notice(&evs, "checkpoint 1 ·") && has_notice(&evs, "stop: all done"));
}

#[tokio::test]
async fn continue_resets_the_counter_and_goes_on() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call("CHECKPOINT CONTINUE: x. NEXT: read c", "c2", "read", json!({"path": "c"}))),
        Reply::Events(call("c3", "read", json!({"path": "d"}))),
        Reply::Events(text("CHECKPOINT STOP: done\nSummary here.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "two rounds, the checkpoint's reply, one more round, the second checkpoint");
    assert!(has_notice(&evs, "continue: x"), "{:?}", ck_notices(&evs));
    assert!(last_message_text(&reqs[4]).starts_with("[checkpoint 2/5]"));
    let kept = serde_json::to_string(&reqs[3].messages).unwrap();
    assert!(kept.contains("[checkpoint 1/5]"), "the first exchange stays in the history");
}

#[tokio::test]
async fn stop_ends_the_request_with_the_summary_and_the_call_does_not_run() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call(
            "CHECKPOINT STOP: enough\nSummary: the parser works.",
            "c9",
            "write",
            json!({"path": "z"}),
        )),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(text_of(&evs).contains("Summary: the parser works."));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { id } if id == "c9")), "the call did not run");
    assert_eq!(client.requests().len(), 3);
    assert!(has_notice(&evs, "stop: enough"));
}

#[tokio::test]
async fn a_stop_without_a_summary_asks_for_it_once() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: done")),
        Reply::Events(text("Everything is finished.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    assert!(last_message_text(&reqs[3]).contains("final summary"));
    assert!(!reqs[3].no_tool_call && reqs[3].tools == reqs[2].tools, "the tools stay as they were: the cache is found");
    assert!(text_of(&evs).contains("Everything is finished."));
}

#[tokio::test]
async fn two_checkpoints_without_progress_stop_by_code() {
    // the same read over and over, the model always says continue
    let mut replies = vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "a"}))),
    ];
    for k in 2..12 {
        replies.push(Reply::Events(text_call(CONT, &format!("c{k}"), "read", json!({"path": "a"}))));
    }
    let client = Scripted::new(replies);
    let evs = run_request(&client, ck_config(2), "loop").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    // checkpoint 1 follows rounds with news, 2 follows rounds with none and is sent, 3 would be the second in a row
    assert_eq!(client.requests().len(), 6, "the model is not asked a third time");
    let reqs = client.requests();
    assert!(
        last_message_text(&reqs[4]).contains("[checkpoint 2/5]") && last_message_text(&reqs[4]).contains("no progress")
    );
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("no file written")));
}

#[tokio::test]
async fn progress_keeps_the_checkpoints_going() {
    let client = Scripted::new(writing_script(4, text("CHECKPOINT STOP: finished\nDone.")));
    let evs = run_request(&client, ck_config(2), "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 9);
    assert!(has_notice(&evs, "checkpoint 4 ·"));
}

#[tokio::test]
async fn without_a_spend_limit_the_checkpoints_are_capped_at_five() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let evs = run_request(&client, ck_config(2), "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 12, "two rounds, then five checkpoints of two rounds each");
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("agent.max_checkpoints")));
}

#[tokio::test]
async fn max_checkpoints_sets_the_cap() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_checkpoints = Some(2);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6);
    assert!(has_notice(&evs, "stopped after 2 checkpoints"));
}

#[tokio::test]
async fn a_spend_limit_makes_the_checkpoints_unlimited() {
    let client = Scripted::new(writing_script(7, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_cost = Some(1000.0);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "seven checkpoints, more than the default five");
    assert_eq!(client.requests().len(), 15);
}

#[tokio::test]
async fn the_spend_limit_wins_over_continue() {
    let client = Scripted::new(writing_script(3, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    // each call costs 0.00002: the limit is crossed by the reply that says to continue
    cfg.agent.max_cost = Some(0.00005);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert_eq!(client.requests().len(), 3);
    assert!(has_notice(&evs, "spend limit reached"));
}

#[tokio::test]
async fn ask_keeps_the_old_behaviour() {
    let client = Scripted::new(writing_script(3, text("never reached")));
    let mut cfg = ck_config(2);
    cfg.agent.on_limit = nexo_core::config::OnLimit::Ask;
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 2);
    assert!(has_notice(&evs, "stopped after 2 tool rounds (agent.max_turns)"));
    assert!(!client.requests().iter().any(|q| serde_json::to_string(&q.messages).unwrap().contains("checkpoint")));
}

#[tokio::test]
async fn a_checkpoint_is_appended_and_the_prefix_stays() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: ok\nDone.")),
    ]);
    run_request(&client, ck_config(2), "go").await;
    let reqs = client.requests();
    let (before, after) = (&reqs[1], &reqs[2]);
    assert_eq!(after.messages[..before.messages.len()], before.messages[..], "nothing before the new messages changes");
    assert_eq!(after.messages.len(), before.messages.len() + 2, "the reply and the tool results");
    assert_eq!((&after.system, &after.tools, after.no_tool_call), (&before.system, &before.tools, before.no_tool_call));
    let last = after.messages.last().unwrap();
    let (results, texts): (Vec<&Part>, Vec<&Part>) =
        last.content.iter().partition(|p| matches!(p, Part::ToolResult { .. }));
    assert_eq!((results.len(), texts.len()), (1, 1), "the checkpoint rides in the tool-results message");
}

#[tokio::test]
async fn a_stop_at_a_checkpoint_pauses_the_goal() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: blocked on a missing key\nI could not go on.")),
    ]);
    let mut r = start(client.clone(), ck_config(2), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(matches!(goal_state_of(&evs), Some(GoalState::Paused(w)) if w.starts_with("stopped at a checkpoint")));
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no continuation is submitted");
    assert_eq!(client.requests().len(), 3);
    r.task.await.unwrap().unwrap();
}

/// Rounds that repeat one call, the model saying to continue each time: the checkpoints see no progress.
fn repeating_script(path: impl Fn(usize) -> String, name: &str) -> Vec<Reply> {
    let mut replies = vec![
        Reply::Events(call("c0", name, json!({"path": path(0)}))),
        Reply::Events(call("c1", name, json!({"path": path(1)}))),
    ];
    for k in 2..12 {
        replies.push(Reply::Events(text_call(CONT, &format!("c{k}"), name, json!({"path": path(k)}))));
    }
    replies
}

#[tokio::test]
async fn writing_the_same_thing_again_is_not_progress() {
    let client = Scripted::new(repeating_script(|_| "same".into(), "write"));
    let evs = run_request(&client, ck_config(2), "rewrite").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6, "stopped by code at the third checkpoint, as for no output at all");
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("no file written")));
}

#[tokio::test]
async fn a_counter_in_the_output_is_not_a_new_line() {
    // the output differs round to round by a number only
    let client = Scripted::new(repeating_script(|k| format!("tick{k}"), "read"));
    let evs = run_request(&client, ck_config(2), "poll").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6);
}

#[tokio::test]
async fn a_spend_limit_that_cannot_be_reached_does_not_lift_the_cap() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_cost = Some(1000.0);
    // a model with no prices: its cost never grows, so the limit would never fire
    cfg.providers.get_mut("mock").unwrap().pricing.clear();
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 12, "the default cap of five applies");
}

#[tokio::test]
async fn a_stop_in_spanish_is_understood() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call("1. CHECKPOINT PARAR: ya esta\nResumen.", "c9", "write", json!({"path": "z"}))),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { id } if id == "c9")), "the call did not run");
    assert!(has_notice(&evs, "stop: ya esta"));
}

#[tokio::test]
async fn a_goal_stopped_for_lack_of_progress_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let client = Scripted::new(repeating_script(|_| "same".into(), "write"));
    let mut r = start(client.clone(), ck_config(2), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    let rest = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    let all: Vec<AgentEvent> = evs.into_iter().chain(rest).collect();
    assert!(
        matches!(goal_state_of(&all), Some(GoalState::Paused(w)) if w.contains("without progress")),
        "{:?}",
        goal_state_of(&all)
    );
    assert!(!all.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("tool-round limit"))));
    r.stop().await;
}

fn deltas(evs: &[AgentEvent]) -> String {
    evs.iter().filter_map(|e| if let AgentEvent::TextDelta { text } = e { Some(text.as_str()) } else { None }).collect()
}

/// A reply streamed in the given pieces.
fn pieces(parts: &[&str]) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    evs.extend(parts.iter().map(|p| StreamEvent::TextDelta((*p).into())));
    evs.extend([StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]);
    evs
}

#[tokio::test]
async fn the_decision_line_of_a_checkpoint_reply_is_not_streamed() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["CHECK", "POINT STOP: secret", " reason\n", "The work", " is finished."])),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(deltas(&evs), "The work is finished.");
    assert!(has_notice(&evs, "stop: secret reason"));
}

#[tokio::test]
async fn a_checkpoint_reply_that_is_only_the_marker_streams_nothing() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["checkpoint continue: why. NEXT: x"])),
        Reply::Events(text("Done.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "Done.");
}

#[tokio::test]
async fn a_checkpoint_reply_without_the_marker_streams_unchanged() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["Plain ", "words\nmore"])),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "Plain words\nmore");
}

#[tokio::test]
async fn text_starting_with_checkpoint_outside_a_checkpoint_is_untouched() {
    let client = Scripted::new(vec![Reply::Events(pieces(&["CHECKPOINT STOP: not a decision\n", "body"]))]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "CHECKPOINT STOP: not a decision\nbody");
}

#[tokio::test]
async fn the_history_keeps_the_full_checkpoint_reply() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["CHECKPOINT CONTINUE: keep me. NEXT: x\n", "going on"])),
        Reply::Events(text("Done.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "going onDone.");
    let kept = serde_json::to_string(&client.requests()[3].messages).unwrap();
    assert!(kept.contains("CHECKPOINT CONTINUE: keep me. NEXT: x\\ngoing on"), "{kept}");
}

/// A reply cut mid-stream by the server, after some of it came through.
fn broke_off(kind: ErrorKind) -> Vec<StreamEvent> {
    let mut e =
        ProviderError::new(kind, "server_error: non-completed responses with output cannot be synthesized safely");
    e.retry_after = Some(std::time::Duration::from_millis(1));
    vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::TextDelta("Now the plane".into()),
        StreamEvent::Error(e),
        StreamEvent::Done,
    ]
}

#[tokio::test]
async fn a_sub_agent_round_cut_by_the_server_is_asked_again_and_its_work_kept() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(broke_off(ErrorKind::Server)),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(text("all done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "t1" && output.content == "wrote x.txt")
    ));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    // the round asked again is the same request: the write before it is not done twice
    assert_eq!(reqs[2].messages.len(), reqs[3].messages.len());
    let writes = evs.iter().filter(|e| matches!(e, AgentEvent::ToolProgress { line, .. } if line == "→ write x.txt"));
    assert_eq!(writes.count(), 1);
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_that_fails_for_good_hands_back_what_it_did() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt"}))),
        Reply::Events(broke_off(ErrorKind::BadRequest)),
        Reply::Events(text("going on from x.txt")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "delegate".into(), attachments: vec![] });
    let evs = r.turn().await;
    let out = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.content.clone()),
            _ => None,
        })
        .unwrap();
    assert!(out.contains("in round 2/") && out.contains("- write x.txt") && out.contains("go on from there"), "{out}");
    assert_eq!(client.requests().len(), 4, "not asked again: the error is not one a retry mends");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_background_command_past_its_time_is_weighed_apart_and_stopped_when_the_review_says_stop() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo waiting; sleep 30", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("STOP")),
        Reply::Events(text("it hung, so it was stopped")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    // the job's end, told to the idle agent, starts a turn of its own
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "it hung, so it was stopped");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    let review = &reqs[2];
    assert!(review.tools.is_empty() && review.system.contains("CONTINUE or STOP"), "a tool-less request of its own");
    assert!(review.messages.len() == 1 && review.messages[0].text().contains("echo waiting; sleep 30"));
    assert!(review.messages[0].text().contains("run it in the background"));
    let told = reqs[3].messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n");
    assert!(told.contains("[background jobs ended]") && told.contains("stopped: a review judged it"), "{told}");
    assert!(
        !reqs[3].messages.iter().any(|m| m.text().contains("CONTINUE or STOP")),
        "the review stays out of the conversation"
    );
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_review_that_says_continue_leaves_the_command_running() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 3", "background": true}))),
        Reply::Events(text("started")),
        // every request from here on, the looks and the turn the job's end starts, hears CONTINUE
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
        Reply::Events(text("CONTINUE")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    r.turn().await;
    let reqs = client.requests();
    assert!(reqs.iter().any(|q| q.system.contains("CONTINUE or STOP")), "it was looked at");
    let told = reqs.last().unwrap().messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n");
    assert!(told.contains("done (exit 0)"), "{told}");
    r.stop().await;
}

#[tokio::test]
async fn a_suggestion_repeats_the_last_request_with_one_more_message() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("done")),
        // the first suggestion calls a tool: let go
        Reply::Events(call("c2", "read", json!({"path": "b.rs"}))),
        Reply::Events(text("\"now run the tests\"")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Suggestion { .. })), "nothing asks for it on its own");

    r.send(AgentCommand::Suggest);
    let evs = r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Suggestion { .. })), "a reply that calls a tool is dropped");

    r.send(AgentCommand::Suggest);
    let evs = r.until(|e| matches!(e, AgentEvent::Suggestion { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Suggestion { text }) if text == "now run the tests"));

    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    for side in &reqs[2..] {
        let last = &reqs[1];
        assert_eq!(side.system, last.system);
        assert_eq!(side.tools.len(), last.tools.len());
        assert_eq!(side.no_tool_call, last.no_tool_call);
        assert_eq!(side.model, last.model);
        // a guess of a dozen words: no thinking, a short cap
        assert_eq!(side.max_tokens, 200);
        assert!(matches!(side.reasoning, Some(nexo_core::ReasoningConfig::Off)));
        let n = last.messages.len();
        assert_eq!(side.messages.len(), n + 2);
        assert_eq!(side.messages[..n], last.messages[..], "the messages the last request sent come first");
        assert_eq!(side.messages[n].role, Role::Assistant);
        assert_eq!(side.messages[n].text(), "done", "then the reply it got");
        let ask = &side.messages[n + 1];
        assert_eq!(ask.role, Role::User);
        assert!(ask.text().starts_with("Suggest the next message the user is most likely to send"), "{}", ask.text());
    }
    r.stop().await;
}

#[tokio::test]
async fn a_suggestion_leaves_the_history_alone_runs_nothing_and_is_not_asked_when_off() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("done")),
        // the guess calls a tool: it is dropped, the tool never runs
        Reply::Events(call("c2", "read", json!({"path": "b.rs"}))),
        Reply::Events(text("still done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Suggest);
    let evs = r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { .. } | AgentEvent::PermissionAsk { .. })));
    // the next real request carries neither the ask nor the guess
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let sent = serde_json::to_string(&reqs.last().unwrap().messages).unwrap();
    assert!(!sent.contains("Suggest the next message"), "{sent}");
    r.stop().await;

    let client = Scripted::new(vec![Reply::Events(text("done")), Reply::Events(text("unasked"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.ui.suggest = false;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Suggest);
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Suggestion { .. })));
    assert_eq!(client.requests().len(), 2, "no side request when [ui] suggest is off");
    r.stop().await;
}
#[tokio::test]
async fn a_failure_that_comes_back_is_tagged_and_the_loop_guard_still_stops_the_loop() {
    // the guards weigh the outputs as they came: the line, new each time, must not hide the loop from them
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(
        (0..8).map(|i| Reply::Events(call(&format!("c{i}"), "read", json!({"path": "missing.txt"})))).collect(),
    );
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.max_turns = 8;
    cfg.agent.guard_answer = false;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "loop".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(client.requests().len() < 8, "stopped before max_turns: {}", client.requests().len());
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("agent.loop_guard"))));
    let sent = serde_json::to_string(&client.requests()[2].messages).unwrap();
    assert!(sent.contains("[the same failure on missing.txt (missing) for the second time"), "{sent}");
    r.stop().await;
}

#[tokio::test]
async fn a_cleared_conversation_does_not_count_the_failures_of_the_last_one() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "missing.txt"}))),
        Reply::Events(text("it is not there")),
        Reply::Events(call("c1", "read", json!({"path": "missing.txt"}))),
        Reply::Events(text("it is not there")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read it".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Clear);
    r.until(|e| matches!(e, AgentEvent::SessionChanged { .. })).await;
    r.send(AgentCommand::Submit { text: "read it".into(), attachments: vec![] });
    r.turn().await;
    let sent = serde_json::to_string(&client.requests()[3].messages).unwrap();
    assert!(sent.contains("No such file") && !sent.contains("[the same failure"), "{sent}");
    r.stop().await;
}

#[tokio::test]
async fn a_clear_during_a_turn_leaves_the_agent_idle() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo go; sleep 30"}))),
        Reply::Events(text("fresh answer")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::ToolStart { .. })).await;
    r.send(AgentCommand::Clear);
    // the cancelled turn never reaches its tail: the clear says the agent is idle (a headless run waits for it)
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { .. }) {
            break;
        }
    }
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "fresh answer");
    let sent = serde_json::to_string(&client.requests()[1].messages).unwrap();
    assert!(!sent.contains("background jobs ended"), "{sent}");
    r.stop().await;
}

#[tokio::test]
async fn a_sub_agent_reply_cut_at_the_output_limit_is_a_failure_not_a_report() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"description": "map it", "prompt": "where is x kept?"}))),
        Reply::Events(vec![
            StreamEvent::TextDelta("x is kept in a.rs and the".into()),
            StreamEvent::Finish(FinishReason::Length),
            StreamEvent::Done,
        ]),
        Reply::Events(text("noted")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "where is x?".into(), attachments: vec![] });
    let evs = r.turn().await;
    let done = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "t1" => Some(output.clone()),
            _ => None,
        })
        .unwrap();
    assert!(done.is_error && done.content.contains("cut off"), "{done:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_model_changed_during_a_request_does_not_change_who_is_billed() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let tx = r.handle.commands.clone();
    client.replies.lock().unwrap().push_back(Reply::Midway(
        tx,
        Box::new(AgentCommand::SetModel { provider: None, model: "m2".into() }),
        text("answer from m1"),
    ));
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let cost =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None });
    assert!(cost.is_some_and(|c| c > 0.0), "m1 has a price: {cost:?}");
    r.stop().await;
}

#[tokio::test]
async fn the_usage_of_a_failed_attempt_is_still_billed() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(vec![
            StreamEvent::Usage(Usage { input: 5000, ..Default::default() }),
            StreamEvent::Error(ProviderError::new(
                ErrorKind::BadRequest,
                "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
            )),
        ]),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let session =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None });
    assert_eq!(session.map(|u| u.input), Some(6000), "the failed attempt's 5000 count too");
    r.stop().await;
}

/// A tool that sends a command to the agent when it runs the second time.
struct Nudge {
    tx: CommandTx,
    runs: Mutex<u32>,
}

#[async_trait]
impl Tool for Nudge {
    fn name(&self) -> &str {
        "nudge"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "nudge".into(), description: "fake".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _args: &Value) -> String {
        "nudge".into()
    }
    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        let mut n = self.runs.lock().unwrap();
        *n += 1;
        if *n == 2 {
            self.tx.send(AgentCommand::Submit { text: "and now read b".into(), attachments: vec![] }).unwrap();
        }
        ToolOutput::ok("same output")
    }
}

#[tokio::test]
async fn a_message_that_joins_after_a_guard_stop_is_answered_with_tools_on() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("n1", "nudge", json!({}))),
        Reply::Events(call("n2", "nudge", json!({}))),
        Reply::Events(call("c3", "read", json!({"path": "b"}))),
        Reply::Events(text("read it")),
    ]);
    let (handle, ends) = channels();
    let tools = tools().with(Arc::new(Nudge { tx: handle.commands.clone(), runs: Mutex::new(0) }));
    data_dir();
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.loop_guard = 2;
    cfg.agent.guard_answer = true;
    let setup = AgentSetup {
        config: cfg,
        cwd: d.path().to_path_buf(),
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
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    r.next().await;
    r.send(AgentCommand::Submit { text: "start".into(), attachments: vec![] });
    let evs = r.turn().await;
    let read = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { id, output, .. } if id == "c3" => Some(output.clone()),
        _ => None,
    });
    assert!(read.as_ref().is_some_and(|o| !o.is_error), "the new message's call runs: {read:?}");
    r.stop().await;
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

/// The text of the tool results the model got in a request.
#[cfg(unix)]
fn results_in(req: &nexo_core::provider::ModelRequest) -> Vec<String> {
    req.messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => content.first().map(|c| text_of_parts(std::slice::from_ref(c))),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_json_deny_blocks_the_tool_and_updated_input_changes_it() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "denied.txt", "content": "a"}))),
        Reply::Events(call("c2", "write", json!({"path": "old.txt", "content": "a"}))),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "PreToolUse",
        Some("Write"),
        r#"p=$(cat); case "$p" in *denied.txt*) echo '{"decision":"deny","reason":"json says no"}';; *old.txt*) echo '{"updated_input":{"path":"new.txt","content":"b"}}';; esac; exit 0"#,
    ));
    // the Claude spelling of the event and of the tool is not read from nexo's own config: only the
    // canonical names match a matcher, so this one is spelled the nexo way
    cfg.hooks[0].matcher = Some("write".into());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    assert!(results_in(&client.requests()[1]).iter().any(|t| t.contains("Blocked by a hook: json says no")));
    let second = results_in(&client.requests()[2]);
    assert!(second.iter().any(|t| t == "write:new.txt"), "the input was replaced: {second:?}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_hook_sends_the_agent_back_to_work_three_times_at_most() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new((1..=6).map(|i| Reply::Events(text(&format!("answer {i}")))).collect());
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("stop", None, "echo 'run the tests first' >&2; exit 2"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "one answer and three goes back to work");
    let last = reqs[3].messages.last().unwrap();
    assert!(last.text().contains("[Stop hook] run the tests first"), "{}", last.text());
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn session_and_prompt_hooks_add_context_and_a_prompt_hook_can_stop_the_request() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("fine"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("SessionStart", None, r#"echo '{"additional_context":"the repo uses tabs"}'"#));
    cfg.hooks.push(hook_cfg("UserPromptSubmit", None, "cat > prompt.json; grep -q forbidden prompt.json && { echo 'no such words' >&2; exit 2; }; echo 'it is monday'"));
    cfg.hooks.push(hook_cfg("SessionEnd", None, "cat > end.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "a forbidden thing".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("a hook stopped it"))).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("no such words")));
    assert!(client.requests().is_empty(), "nothing was sent");
    r.send(AgentCommand::Submit { text: "a fine thing".into(), attachments: vec![] });
    r.turn().await;
    let first = client.requests().remove(0);
    let user = first.messages.iter().find(|m| m.role == nexo_core::message::Role::User).unwrap().text();
    assert!(
        user.contains("the repo uses tabs") && user.contains("it is monday") && user.contains("a fine thing"),
        "{user}"
    );
    r.stop().await;
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("end.json")).unwrap()).unwrap();
    assert_eq!(seen["hook_event_name"], "SessionEnd");
    assert!(seen["session_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[cfg(unix)]
#[tokio::test]
async fn tool_hooks_see_the_call_and_its_output_and_a_post_hook_reaches_the_model() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt", "content": "hello"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("pre_tool", Some("w.*"), "cat > pre.json"));
    cfg.hooks.push(hook_cfg("post_tool", Some("write"), "cat > post.json; echo 'formatted a.txt' >&2; exit 2"));
    cfg.hooks.push(hook_cfg("stop", None, "cat > stop.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let read =
        |n: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(d.path().join(n)).unwrap()).unwrap() };
    let pre = read("pre.json");
    assert_eq!((pre["tool_name"].as_str(), pre["tool_input"]["content"].as_str()), (Some("write"), Some("hello")));
    assert_eq!(pre["hook_event_name"], "PreToolUse");
    let post = read("post.json");
    assert_eq!(post["tool_name"], "write");
    assert!(post["tool_response"].as_str().is_some_and(|s| !s.is_empty()) && post["is_error"] == false);
    assert_eq!(read("stop.json")["last_assistant_message"], "done");
    assert!(results_in(&client.requests()[1]).iter().any(|t| t.contains("[hook] formatted a.txt")));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_can_answer_in_place_of_the_person() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "ok.txt", "content": "x"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg("permission_request", Some("write"), r#"cat > asked.json; echo '{"decision":"allow"}'"#));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the person was not asked");
    assert!(results_in(&client.requests()[1]).iter().any(|t| t == "write:ok.txt"));
    assert!(std::fs::read_to_string(d.path().join("asked.json")).unwrap().contains("permission_request"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_names_the_sub_agent_that_asks_and_not_the_main_agent() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt", "content": "x"}))),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(call("c2", "write", json!({"path": "y.txt", "content": "y"}))),
        Reply::Events(text("all done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg(
        "permission_request",
        Some("write"),
        r#"cat > "asked-$(date +%s%N).json"; echo '{"decision":"allow"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let mut seen: Vec<Value> = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("asked-"))
        .map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect();
    seen.sort_by_key(|v| v["tool_input"]["path"].as_str().unwrap_or_default().to_string());
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!((seen[0]["tool_input"]["path"].as_str(), seen[0]["agent"].as_str()), (Some("x.txt"), Some("general")));
    assert_eq!(seen[1]["tool_input"]["path"], "y.txt");
    assert!(seen[1].get("agent").is_none(), "the main agent carries no agent field: {:?}", seen[1]);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sub_agent_and_compaction_hooks_fire() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"description": "look", "prompt": "find it"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("the report says found")),
        Reply::Events(text("a summary")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("subagent_start", Some("general|explore"), "cat > sub-start.json"));
    cfg.hooks.push(hook_cfg("subagent_stop", None, "cat > sub-stop.json"));
    cfg.hooks.push(hook_cfg("pre_compact", None, "cat > compact.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Compact);
    r.until(|e| matches!(e, AgentEvent::Compacted { .. })).await;
    let read =
        |n: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(d.path().join(n)).unwrap()).unwrap() };
    assert_eq!(read("sub-start.json")["prompt"], "find it");
    assert_eq!(read("sub-stop.json")["report"], "found it");
    assert_eq!(read("compact.json")["hook_event_name"], "PreCompact");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_hooks_command_lists_what_is_loaded() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("notification", None, "true"));
    let mut r = start(Scripted::new(vec![]), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::ShowHooks);
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("hook(s)"))).await;
    assert!(
        matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("notification: true  <- nexo config"))
    );
    r.stop().await;
}

/// A main-agent script that sends a `general` sub-agent to write `path`, then lets both answer.
#[cfg(unix)]
fn sub_writes(path: &str) -> Arc<Scripted> {
    Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write it"}))),
        Reply::Events(call("s1", "write", json!({"path": path, "content": "a"}))),
        Reply::Events(text("the sub-agent is done")),
        Reply::Events(text("all done")),
    ])
}

#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_that_denies_write_blocks_the_sub_agents_write() {
    let d = tempfile::tempdir().unwrap();
    let client = sub_writes("denied.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "pre_tool",
        Some("write"),
        r#"cat > pre.json; echo '{"decision":"deny","reason":"no writes here"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let sub = results_in(&client.requests()[2]);
    assert!(sub.iter().any(|t| t.contains("Blocked by a hook: no writes here")), "{sub:?}");
    let pre: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("pre.json")).unwrap()).unwrap();
    assert_eq!((pre["tool_name"].as_str(), pre["agent"].as_str()), (Some("write"), Some("general")));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_post_tool_hook_sees_the_sub_agents_call_with_its_name_and_may_add_a_note() {
    let d = tempfile::tempdir().unwrap();
    let client = sub_writes("a.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "post_tool",
        Some("write"),
        r#"cat > post.json; echo '{"additional_context":"formatted a.txt"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let post: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("post.json")).unwrap()).unwrap();
    assert_eq!((post["tool_name"].as_str(), post["agent"].as_str()), (Some("write"), Some("general")));
    assert_eq!(post["tool_response"], "write:a.txt");
    let sub = results_in(&client.requests()[2]);
    assert!(sub.iter().any(|t| t.contains("write:a.txt") && t.contains("[hook] formatted a.txt")), "{sub:?}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_can_allow_or_rewrite_a_sub_agents_call() {
    let d = tempfile::tempdir().unwrap();
    let client = sub_writes("old.txt");
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg(
        "pre_tool",
        Some("write"),
        r#"echo '{"decision":"allow","updated_input":{"path":"new.txt","content":"b"}}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the person was not asked");
    assert!(results_in(&client.requests()[2]).iter().any(|t| t == "write:new.txt"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_sub_agents_tool_calls_reach_the_audit_log() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("audit.jsonl");
    let client = sub_writes("a.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.audit.enabled = true;
    cfg.audit.path = Some(log.display().to_string());
    cfg.hooks.push(hook_cfg("pre_tool", Some("write"), r#"echo '{"decision":"allow"}'"#));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let lines = |kind: &str| -> Vec<Value> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["kind"] == "tool" && v["tool"] == kind)
            .collect()
    };
    let write = lines("write");
    assert_eq!(write.len(), 1, "the sub-agent's write is logged once: {write:?}");
    assert_eq!((write[0]["status"].as_str(), write[0]["decision"].as_str()), (Some("ok"), Some("allowed")));
    assert_eq!(lines("task").len(), 1, "the main agent's task call is logged too");
}

#[tokio::test]
async fn fast_switches_to_the_twin_and_back_or_sets_the_request_option() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.providers.get_mut("mock").unwrap().models = vec!["m1".into(), "m1-Fast".into(), "m2".into()];
    let mut sub = ProviderConfig::new("mock", "http://sub", None);
    sub.models = vec!["c1".into()];
    sub.login = Some("codex".into());
    cfg.providers.insert("sub".into(), sub);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;

    // m1 -> m1-Fast (the provider's spelling) and back
    r.send(AgentCommand::SetFast { on: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Fast { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { model, .. } if model == "m1-Fast")));
    assert!(matches!(evs.last(), Some(AgentEvent::Fast { on: true })));
    r.send(AgentCommand::SetFast { on: Some(true) });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("already a fast version")));
    r.send(AgentCommand::SetFast { on: Some(false) });
    let evs = r.until(|e| matches!(e, AgentEvent::Fast { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { model, .. } if model == "m1")));
    assert!(matches!(evs.last(), Some(AgentEvent::Fast { on: false })));

    // no twin: one line, nothing changes
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text == "No fast version of m2 on mock."));
    r.send(AgentCommand::SetFast { on: Some(false) });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text == "fast is already off"));

    // a provider with a request option keeps its model and sends the option while it is on
    r.send(AgentCommand::SetModel { provider: Some("sub".into()), model: "c1".into() });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Fast { on: true }));
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::SetFast { on: None });
    assert!(matches!(r.next().await, AgentEvent::Fast { on: false }));
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs[0].model, "c1");
    assert_eq!(reqs[0].extra.get("service_tier"), Some(&json!("priority")));
    assert!(!reqs[1].extra.contains_key("service_tier"));
    r.stop().await;
}

#[tokio::test]
async fn add_dir_lets_a_write_there_pass_without_asking_and_tells_the_model() {
    let d = tempfile::tempdir().unwrap();
    let proj = d.path().join("proj");
    let other = d.path().join("other");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    // the folder as a model writes it: no `..`, and on Windows not the `\\?\` form canonicalize gives
    let target = other.join("x.txt").display().to_string();
    let client = Scripted::new(vec![
        Reply::Events(call("w1", "write", json!({"path": target, "content": "new"}))),
        Reply::Events(text("written")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, &proj).await;
    r.next().await;
    r.send(AgentCommand::AddDir("missing".into()));
    let ev = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await.pop().unwrap();
    assert!(matches!(&ev, AgentEvent::Notice { text, .. } if text.contains("not a folder")), "{ev:?}");
    r.send(AgentCommand::AddDir("../other".into()));
    let ev = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await.pop().unwrap();
    assert!(matches!(&ev, AgentEvent::Notice { text, .. } if text.starts_with("added ")), "{ev:?}");
    r.send(AgentCommand::Submit { text: "write there".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the added folder needs no asking");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "w1" && !output.is_error)));
    let reqs = client.requests();
    assert!(reqs[0].system.contains("You may also read and write in this folder: "), "{}", reqs[0].system);
    r.stop().await;
}

// ---- the supervisor's watch and check-in ----

const NOTICE: &str = "was called 3 times with the same arguments";

fn texts_of(req: &ModelRequest) -> String {
    req.messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n")
}

fn quiet(mut cfg: Config) -> Config {
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    cfg
}

fn history_texts(evs: &[AgentEvent]) -> String {
    evs.iter()
        .filter_map(|e| if let AgentEvent::History { message, .. } = e { Some(message.text()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn three_equal_reads(cfg: Config) -> (Arc<Scripted>, Vec<AgentEvent>) {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c2", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c3", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("looked")),
        Reply::Events(text("second answer")),
    ]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    let mut evs = r.turn().await;
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    evs.extend(r.turn().await);
    r.stop().await;
    (client, evs)
}

#[tokio::test]
async fn a_repeat_is_a_hidden_note_on_the_next_request_only_and_never_in_the_transcript() {
    let (client, evs) = three_equal_reads(quiet(config(PermissionMode::Ask))).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "no side request");
    for (i, r) in reqs.iter().enumerate() {
        let has = texts_of(r).contains(NOTICE);
        assert_eq!(has, i == 3, "request {i}: the note rides on the one after the third call only");
    }
    assert!(!history_texts(&evs).contains(NOTICE), "not in the conversation");
    assert!(!history_texts(&evs).contains("system-reminder"));
}

#[tokio::test]
async fn the_watch_can_be_switched_off() {
    let mut cfg = quiet(config(PermissionMode::Ask));
    cfg.agent.watch = false;
    let (client, _) = three_equal_reads(cfg).await;
    assert!(client.requests().iter().all(|r| !texts_of(r).contains(NOTICE)));
}

async fn unfinished(replies: Vec<Reply>, cfg: Config) -> (Arc<Scripted>, Running, Vec<AgentEvent>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut all = vec![
        Reply::Events(call(
            "c1",
            "todo",
            json!({"items": [{"text": "write it", "status": "done"}, {"text": "test it"}]}),
        )),
        Reply::Events(text("all done")),
    ];
    all.extend(replies);
    let client = Scripted::new(all);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    let mut r = start_with(client.clone(), cfg, None, dir.path(), todo_tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    let evs = r.turn().await;
    (client, r, evs, dir)
}

#[tokio::test]
async fn a_check_in_that_says_continue_resumes_the_session_with_the_answer_as_hidden_context() {
    let (client, mut r, mut evs, _d) = unfinished(
        vec![Reply::Events(text("CONTINUE run the missing test")), Reply::Events(text("running it"))],
        config(PermissionMode::Ask),
    )
    .await;
    evs.extend(r.turn().await);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "the todo call, the reply, the check-in, the resumed turn");
    let ask = reqs[2].messages.last().unwrap();
    assert_eq!(ask.role, Role::User);
    assert!(ask.text().contains("Everything OK?") && ask.text().contains("1 open item"), "{}", ask.text());
    assert_eq!(reqs[2].messages[reqs[2].messages.len() - 2].text(), "all done", "after the reply it got");
    assert!(reqs[3].messages.iter().any(|m| m.text().contains("answer was CONTINUE: run the missing test")));
    let said = history_texts(&evs);
    assert!(!said.contains("run the missing test") && !said.contains("Everything OK?"), "{said}");
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::History { message, .. } if message.text() == nexo_core::agent::SUPERVISOR_RESUME_MARK))
    );
    // the work is still open at the end of the resumed turn, but a request of the person gets one check-in
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 4, "no second check-in");
    r.stop().await;
}

#[tokio::test]
async fn a_check_in_that_says_done_changes_nothing() {
    let (client, mut r, evs, _d) =
        unfinished(vec![Reply::Events(text("DONE, the test is not needed"))], config(PermissionMode::Ask)).await;
    let more = r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 3);
    assert!(!more.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no new turn");
    assert!(!history_texts(&evs).contains("Everything OK?"));
    r.stop().await;
}

#[tokio::test]
async fn the_check_in_can_be_switched_off() {
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_in = false;
    let (client, r, _, _d) = unfinished(vec![Reply::Events(text("CONTINUE go"))], cfg).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn the_supervisor_asks_for_a_check_in_once_per_stopped_turn() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("finished")), Reply::Events(text("DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(client.requests().len(), 1, "nothing looks unfinished: no check-in on its own");
    r.send(AgentCommand::CheckIn { evidence: "the session asked a question and nobody answered".into() });
    r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    r.send(AgentCommand::CheckIn { evidence: "again".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "the second ask is let go");
    assert!(reqs[1].messages.last().unwrap().text().contains("nobody answered"));
    r.stop().await;
}

#[tokio::test]
async fn a_request_naming_an_existing_file_attaches_it_once() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    std::fs::write(d.path().join("big.txt"), "x".repeat(1_000_000)).unwrap();
    let client =
        Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two")), Reply::Events(text("three"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "make `answer` in src/lib.rs return 43".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "and keep src/lib.rs tidy".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "what is in big.txt and missing.rs?".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let first = reqs[0].messages[0].text();
    assert!(first.contains("@src/lib.rs (attached in full, as it is now):"), "{first}");
    assert!(first.contains("pub fn answer() -> u32 { 42 }"), "{first}");
    let second = reqs[1].messages.iter().filter(|m| m.role == Role::User).nth(1).unwrap().text();
    assert!(!second.contains("attached in full"), "{second}");
    let third = reqs[2].messages.last().unwrap().text();
    assert!(!third.contains("attached in full") && !third.contains("xxxx"), "{third}");
    r.stop().await;
}

/// A request that names a key file or a denied path does not send it to the provider unasked.
#[tokio::test]
async fn a_named_key_file_is_not_attached() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("config")).unwrap();
    std::fs::create_dir_all(d.path().join("secrets")).unwrap();
    std::fs::create_dir_all(d.path().join("sql")).unwrap();
    std::fs::write(d.path().join("config/.env"), "TOKEN=hunter2\n").unwrap();
    std::fs::write(d.path().join("secrets/app.key"), "PRIVATEKEYDATA\n").unwrap();
    std::fs::write(d.path().join("sql/t.sql"), "select denied_marker;\n").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.deny = vec!["read(sql/**)".into()];
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit {
        text: "never commit config/.env, rotate secrets/app.key, fix sql/t.sql".into(),
        attachments: vec![],
    });
    r.turn().await;
    let first = client.requests()[0].messages[0].text();
    assert!(!first.contains("attached in full"), "{first}");
    assert!(
        !first.contains("hunter2") && !first.contains("PRIVATEKEYDATA") && !first.contains("denied_marker"),
        "{first}"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_large_window_gets_the_largest_reply_from_the_first_request() {
    let _g = config_lock().lock().await;
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    let p = cfg.providers.get_mut("mock").unwrap();
    p.context_window.insert("m1".into(), 1_048_576);
    p.max_output.insert("m1".into(), 32_768);
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests()[0].max_tokens, 32_768);
    r.stop().await;
}

/// A tool that writes, edits or runs at once; a path starting with `missing` makes it fail.
fn quick_tools() -> ToolSet {
    ToolSet::new()
        .with(Arc::new(Fake { name: "read", kind: ToolKind::Read, slow: false }))
        .with(Arc::new(Fake { name: "write", kind: ToolKind::Write, slow: false }))
        .with(Arc::new(Fake { name: "edit", kind: ToolKind::Write, slow: false }))
        .with(Arc::new(Fake { name: "bash", kind: ToolKind::Exec, slow: false }))
}

/// Runs one message of calls (id, tool, args); the results the model gets back, as (id, is_error, text).
async fn one_message(calls: &[(&str, &str, Value)]) -> (Vec<(String, bool, String)>, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let mut evs = Vec::new();
    for (id, name, args) in calls {
        evs.push(StreamEvent::ToolCallStart { id: (*id).into(), name: (*name).into() });
        evs.push(StreamEvent::ToolCallEnd { id: (*id).into(), name: (*name).into(), args: args.clone() });
    }
    evs.extend([StreamEvent::Finish(FinishReason::ToolUse), StreamEvent::Done]);
    let client = Scripted::new(vec![Reply::Events(evs), Reply::Events(text("ok"))]);
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), quick_tools()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let closed = evs
        .iter()
        .filter_map(|e| if let AgentEvent::ToolDone { id, .. } = e { Some(id.clone()) } else { None })
        .collect();
    let reqs = client.requests();
    let outs = reqs[1]
        .messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { call_id, content, is_error, .. } => {
                Some((call_id.clone(), *is_error, text_of_parts(content)))
            }
            _ => None,
        })
        .collect();
    r.stop().await;
    (outs, closed)
}

#[tokio::test]
async fn a_write_then_a_command_in_one_message_both_run_in_order() {
    let (outs, closed) = one_message(&[
        ("w1", "write", json!({"path": "run.sh", "content": "echo hi"})),
        ("b1", "bash", json!({"command": "sh run.sh"})),
    ])
    .await;
    assert_eq!(outs.len(), 2);
    assert_eq!((outs[0].0.as_str(), outs[0].1), ("w1", false));
    assert_eq!((outs[1].0.as_str(), outs[1].1), ("b1", false));
    assert_eq!(outs[1].2, "bash:sh run.sh");
    assert_eq!(closed, ["w1", "b1"]);
}

#[tokio::test]
async fn a_failed_change_stops_the_calls_after_it_in_the_message() {
    let (outs, closed) = one_message(&[
        ("e1", "edit", json!({"path": "missing.rs"})),
        ("b1", "bash", json!({"command": "sh run.sh"})),
        ("r1", "read", json!({"path": "a"})),
    ])
    .await;
    assert_eq!(outs.len(), 3);
    assert_eq!((outs[0].0.as_str(), outs[0].1), ("e1", true));
    let want = "not run: the call before it in this message failed. Read its result, then send this call again if it is still wanted.";
    for (i, id) in [(1, "b1"), (2, "r1")] {
        assert_eq!((outs[i].0.as_str(), outs[i].1, outs[i].2.as_str()), (id, true, want));
    }
    assert_eq!(closed, ["e1", "b1", "r1"], "the skipped calls are closed for the screen");
}

#[tokio::test]
async fn a_failed_read_does_not_stop_the_reads_beside_it() {
    let (outs, _) =
        one_message(&[("r1", "read", json!({"path": "missing.txt"})), ("r2", "read", json!({"path": "a.txt"}))]).await;
    assert_eq!(outs.len(), 2);
    assert_eq!((outs[0].0.as_str(), outs[0].1), ("r1", true));
    assert_eq!((outs[1].0.as_str(), outs[1].1, outs[1].2.as_str()), ("r2", false, "read:a.txt"));
}

fn last_user_text(q: &ModelRequest) -> String {
    q.messages.iter().rev().find(|m| m.role == Role::User).map(|m| m.text()).unwrap_or_default()
}

/// What a session sends before its messages never changes while it lives, in the process or after a resume;
/// a file made between two messages reaches the model in the new message, once.
#[tokio::test]
async fn the_system_text_and_the_tools_stay_fixed_for_the_session_and_a_changed_tree_comes_in_the_new_message() {
    let d = tempfile::tempdir().unwrap();
    let with_diag = || tools().with(Arc::new(Fake { name: "diagnostics", kind: ToolKind::Read, slow: false }));
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    // process 1: the folder holds one file, so the Project block exists
    std::fs::write(d.path().join("README.md"), "x").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut r = start_with(client.clone(), cfg.clone(), None, d.path(), with_diag()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    // the model's work made files
    std::fs::write(d.path().join("inv.py"), "print(1)\n").unwrap();
    std::fs::write(d.path().join("test_inv.py"), "").unwrap();
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].tools.iter().any(|t| t.name == "diagnostics"));
    assert_eq!(reqs[1].system, reqs[0].system, "same process: the system text is the same after a file was made");
    assert_eq!(serde_json::to_string(&reqs[1].tools).unwrap(), serde_json::to_string(&reqs[0].tools).unwrap());
    assert!(reqs[0].system.contains("Project (as when the session began"), "{}", reqs[0].system);
    assert!(!reqs[0].system.contains("inv.py"));
    let note = last_user_text(&reqs[1]);
    assert!(note.contains("[project at the start of this request]"), "{note}");
    assert!(note.contains("inv.py") && note.contains("test_inv.py"), "{note}");
    assert!(!last_user_text(&reqs[0]).contains("[project at the start"), "no change, no note");
    // the old messages are the same bytes
    assert_eq!(
        serde_json::to_string(&reqs[0].messages[0]).unwrap(),
        serde_json::to_string(&reqs[1].messages[0]).unwrap()
    );
    // process 2 (`-c`): the folder holds Python files now and the process does not offer `diagnostics` of itself
    let client = Scripted::new(vec![Reply::Events(text("three")), Reply::Events(text("four"))]);
    let mut r = start_with(client.clone(), cfg.clone(), Some(id.clone()), d.path(), tools()).await;
    r.next().await;
    std::fs::write(d.path().join("inv.json"), "{}").unwrap();
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "fourth".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let later = client.requests();
    assert_eq!(later[0].system, reqs[0].system, "after a resume: the system text is the one the session began with");
    let names = |q: &ModelRequest| q.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&later[0]), names(&reqs[0]), "after a resume: the same tools");
    assert!(later[0].tools.iter().any(|t| t.name == "diagnostics"));
    let note = last_user_text(&later[0]);
    assert!(note.contains("inv.json"), "{note}");
    assert!(!note.contains("test_inv.py") || note.contains("Project (changed"), "{note}");
    assert!(!last_user_text(&later[1]).contains("[project at the start"), "nothing changed since: no note");
    assert_eq!(later[1].system, later[0].system);
    // process 3: the stored set has no `diagnostics` where it was started without it; a new process that has it drops it
    let client = Scripted::new(vec![Reply::Events(text("five"))]);
    let mut r = start_with(client.clone(), cfg.clone(), None, d.path(), tools()).await;
    let id2 = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "five".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let client2 = Scripted::new(vec![Reply::Events(text("six"))]);
    let mut r = start_with(client2.clone(), cfg, Some(id2), d.path(), with_diag()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "six".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert!(!client2.requests()[0].tools.iter().any(|t| t.name == "diagnostics"));
    assert_eq!(names(&client2.requests()[0]), names(&client.requests()[0]));
}

/// A session file saved before the prefix existed loads, gets one built and saved, and keeps it after.
#[tokio::test]
async fn a_session_saved_without_a_prefix_gets_one_once() {
    let d = tempfile::tempdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // an old file: the same lines without the prefix one
    let path = nexo_agent::session::dir().join(format!("{id}.jsonl"));
    let kept: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("{\"type\":\"prefix\""))
        .map(String::from)
        .collect();
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    assert!(nexo_agent::session::Session::open_path(&path).unwrap().prefix().is_none());
    std::fs::write(d.path().join("late.txt"), "x").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("two")), Reply::Events(text("three"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let saved = nexo_agent::session::Session::open_path(&path).unwrap();
    let p = saved.prefix().expect("built and saved on resume");
    assert!(p.volatile.environment.contains("late.txt"));
    std::fs::write(d.path().join("later.txt"), "x").unwrap();
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs[1].system, reqs[0].system, "the prefix built on the first resume is kept on the next");
    assert!(last_user_text(&reqs[1]).contains("later.txt"));
}

// ---- each session remembers its provider and model

async fn ready_id(r: &mut Running) -> String {
    match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        e => panic!("{e:?}"),
    }
}

/// One short turn, so the session holds something and stays on disk.
async fn say(r: &mut Running) {
    r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
    r.turn().await;
}

fn one_reply() -> Arc<Scripted> {
    Scripted::new(vec![Reply::Events(text("ok"))])
}

fn last_used(id: &str) -> (String, String) {
    let m = nexo_agent::Session::open(id).unwrap().meta().clone();
    (m.provider, m.model)
}

/// A session that ran on `m2` and was closed.
async fn session_on_m2(d: &std::path::Path) -> String {
    let mut r = start(one_reply(), config(PermissionMode::Ask), None, d).await;
    let id = ready_id(&mut r).await;
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    r.until(|e| matches!(e, AgentEvent::ModelChanged { .. })).await;
    say(&mut r).await;
    r.stop().await;
    id
}

/// What the agent says while it starts up: everything before the answer to `/sent`.
async fn startup_events(r: &mut Running) -> Vec<AgentEvent> {
    r.send(AgentCommand::ShowSent);
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("nothing was sent yet"))).await
}

#[tokio::test]
async fn a_session_records_its_model_at_the_start_and_at_each_change() {
    let d = tempfile::tempdir().unwrap();
    let mut r = start(one_reply(), config(PermissionMode::Ask), None, d.path()).await;
    let id = ready_id(&mut r).await;
    assert_eq!(last_used(&id), ("mock".into(), "m1".into()));
    r.send(AgentCommand::SetModel { provider: None, model: "m2".into() });
    r.until(|e| matches!(e, AgentEvent::ModelChanged { .. })).await;
    say(&mut r).await;
    assert_eq!(last_used(&id), ("mock".into(), "m2".into()));
    r.stop().await;
    assert_eq!(last_used(&id), ("mock".into(), "m2".into()), "the last one stays on disk");
}

#[tokio::test]
async fn a_resumed_session_goes_back_to_its_model() {
    let d = tempfile::tempdir().unwrap();
    let id = session_on_m2(d.path()).await;
    // the launch itself would start on m1
    let mut r = start(Scripted::new(vec![]), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    ready_id(&mut r).await;
    let evs = startup_events(&mut r).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, AgentEvent::ModelChanged { provider, model } if provider == "mock" && model == "m2")),
        "{evs:?}"
    );
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ModelLimits { model, .. } if model == "m2")));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("not available"))));
    r.stop().await;
    // a new session still starts on the default
    let mut r = start(one_reply(), config(PermissionMode::Ask), None, d.path()).await;
    let fresh = ready_id(&mut r).await;
    assert_eq!(last_used(&fresh), ("mock".into(), "m1".into()));
    let evs = startup_events(&mut r).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { .. })), "a new session starts as before");
    r.stop().await;
}

#[tokio::test]
async fn a_model_gone_from_the_list_keeps_the_current_one_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let id = session_on_m2(d.path()).await;
    let mut cfg = config(PermissionMode::Ask);
    cfg.providers.get_mut("mock").unwrap().models = vec!["m1".into()];
    let mut r = start(Scripted::new(vec![]), cfg, Some(id.clone()), d.path()).await;
    ready_id(&mut r).await;
    let evs = startup_events(&mut r).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { .. })), "{evs:?}");
    let line = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::Notice { text, level } if text.contains("not available") => Some((text.clone(), *level)),
            _ => None,
        })
        .expect("one plain line");
    assert_eq!(line.0, "this session last used mock/m2, which is not available now; staying on mock/m1");
    assert!(matches!(line.1, Level::Info), "no error");
    r.stop().await;
}

#[tokio::test]
async fn a_provider_that_is_no_longer_configured_falls_back_the_same_way() {
    let d = tempfile::tempdir().unwrap();
    let id = session_on_m2(d.path()).await;
    let mut cfg = config(PermissionMode::Ask);
    // the session's provider needs a key that is not there now
    cfg.providers.get_mut("mock").unwrap().api_key_env = Some("NEXO_TEST_NO_SUCH_KEY_6F1".into());
    let mut r = start(Scripted::new(vec![]), cfg, Some(id), d.path()).await;
    ready_id(&mut r).await;
    let evs = startup_events(&mut r).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("not available now"))));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { .. })));
    r.stop().await;
}

#[tokio::test]
async fn a_model_named_for_the_launch_wins_over_the_session() {
    let d = tempfile::tempdir().unwrap();
    let id = session_on_m2(d.path()).await;
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client: Scripted::new(vec![]),
        resume: Some(id.clone()),
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: true,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    ready_id(&mut r).await;
    let evs = startup_events(&mut r).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ModelChanged { .. })), "{evs:?}");
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("not available"))));
    r.stop().await;
    assert_eq!(last_used(&id), ("mock".into(), "m1".into()), "the session now records what it ran on");
}

#[tokio::test]
async fn a_session_file_without_the_record_opens_as_before() {
    let d = tempfile::tempdir().unwrap();
    let id = session_on_m2(d.path()).await;
    // an old file: the meta line and the messages, no `model` line (the model is the one it began with)
    let path = nexo_agent::session::dir().join(format!("{id}.jsonl"));
    let kept: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"type\":\"model\""))
        .map(String::from)
        .collect();
    assert!(kept[0].contains("\"m1\""), "{}", kept[0]);
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    let mut r = start(Scripted::new(vec![]), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    assert!(matches!(r.next().await, AgentEvent::Ready { session_id, model, .. } if session_id == id && model == "m1"));
    let evs = startup_events(&mut r).await;
    assert!(!evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelChanged { .. } | AgentEvent::Notice { level: Level::Error, .. })));
    r.stop().await;
}

#[tokio::test]
async fn two_sessions_keep_their_own_models() {
    let d = tempfile::tempdir().unwrap();
    let a = session_on_m2(d.path()).await;
    let mut r = start(one_reply(), config(PermissionMode::Ask), None, d.path()).await;
    let b = ready_id(&mut r).await;
    say(&mut r).await;
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::ModelChanged { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::ModelChanged { model, .. }) if model == "m2"));
    // B was left on m1 and goes back to it, whatever A did in between
    r.send(AgentCommand::Resume { id: b.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::ModelChanged { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::ModelChanged { model, .. }) if model == "m1"), "{evs:?}");
    r.stop().await;
    assert_eq!(last_used(&a), ("mock".into(), "m2".into()));
    assert_eq!(last_used(&b), ("mock".into(), "m1".into()));
}

#[tokio::test]
async fn an_empty_reply_is_not_called_reasoning() {
    let d = tempfile::tempdir().unwrap();
    let empty = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::Usage(Usage { input: 1000, output: 0, ..Default::default() }),
        StreamEvent::Finish(FinishReason::ContentFilter),
        StreamEvent::Done,
    ];
    let client = Scripted::new(vec![Reply::Events(empty)]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!notices(&evs).iter().any(|n| n.contains("reasoned without answering")), "{:?}", notices(&evs));
    r.stop().await;
}

#[tokio::test]
async fn a_cut_reasoning_call_is_billed_its_input_too() {
    let d = tempfile::tempdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(long_thought(40_000, then_text("never read"))), Reply::Events(text("done"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "implement it".into(), attachments: vec![] });
    let evs = r.turn().await;
    let first = evs
        .iter()
        .find_map(|e| if let AgentEvent::Usage { call, .. } = e { Some(*call) } else { None })
        .expect("a usage event for the cut call");
    assert!(first.total_input() > 0, "the cut call sent a whole request and was billed none of it: {first:?}");
    // an estimate: not far above what was sent (4 characters a token, the nexo estimate)
    let sent = client.requests()[0].clone();
    let size = serde_json::to_string(&sent).unwrap().len() as u64 / 4;
    assert!(first.total_input() <= size * 2, "estimate {} for a request of about {size} tokens", first.total_input());
    r.stop().await;
}
