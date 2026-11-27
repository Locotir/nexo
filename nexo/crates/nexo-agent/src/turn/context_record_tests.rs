//! A call whose view failed says so on its usage line, and only that one (0 $, no network).

use super::start::view_now;
use super::*;
use crate::context::Plain;
use nexo_core::agent::channels;
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::error::{ErrorKind, ProviderError};
use nexo_core::message::Message;
use nexo_core::provider::{EventStream, ModelRequest};

/// A model nothing is asked of.
struct Idle;

#[async_trait::async_trait]
impl ModelClient for Idle {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        Err(ProviderError::new(ErrorKind::Other, "not called"))
    }
}

struct SummaryReply;

#[async_trait::async_trait]
impl ModelClient for SummaryReply {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        use nexo_core::stream::{FinishReason, StreamEvent};
        Ok(Box::pin(futures::stream::iter(vec![
            StreamEvent::TextDelta("summary".into()),
            StreamEvent::Usage(Usage { input: 17, output: 3, ..Default::default() }),
            StreamEvent::Finish(FinishReason::Stop),
            StreamEvent::Done,
        ])))
    }
}

/// A policy whose view always fails.
struct Failing;

#[async_trait::async_trait]
impl ContextPolicy for Failing {
    fn name(&self) -> &str {
        "failing"
    }
    async fn view(&mut self, _system_tokens: u64, _history: &[Message]) -> anyhow::Result<View> {
        anyhow::bail!("no view today")
    }
    fn reset(&mut self) {}
}

fn usage_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with(r#"{"type":"usage""#))
        .map(String::from)
        .collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_failed_view_marks_the_usage_line_of_that_call_only() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    std::env::set_var("NEXO_DATA_DIR", d.path());
    std::env::set_var("NEXO_MACHINE_MAP", "0");
    let mut config = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    config.providers.insert("mock".into(), p);
    config.context.policy = "plain".into();
    config.agent.attach_budget = 0;
    config.agent.check_at_start = false;
    let cwd = d.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let (_handle, ends) = channels();
    let setup = crate::AgentSetup {
        config,
        cwd,
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new(),
        client: Arc::new(Idle),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    let rt = Runtime::build(ends.events, setup).await.unwrap();
    let mut st = rt.state.lock().await;
    let call = Usage { input: 10, output: 1, ..Default::default() };
    st.policy = Box::new(Failing);
    view_now(&mut st, 0).await;
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    st.policy = Box::new(Plain::default());
    view_now(&mut st, 0).await;
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    let lines = usage_lines(st.session.path());
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains(r#""fallback":true"#), "{}", lines[0]);
    assert!(!lines[1].contains("fallback"), "{}", lines[1]);
    // the policy that ran is on the record too, once
    let text = std::fs::read_to_string(st.session.path()).unwrap();
    assert_eq!(text.matches(r#"{"type":"context""#).count(), 1);
    std::env::remove_var("NEXO_DATA_DIR");
}

async fn runtime_in(d: &Path) -> Runtime {
    std::env::set_var("NEXO_DATA_DIR", d);
    std::env::set_var("NEXO_MACHINE_MAP", "0");
    let mut config = Config::default();
    let mut p = ProviderConfig::new("mock", "http://mock", None);
    p.models = vec!["m1".into()];
    config.providers.insert("mock".into(), p);
    config.context.policy = "plain".into();
    config.agent.attach_budget = 0;
    config.agent.check_at_start = false;
    let cwd = d.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let (_handle, ends) = channels();
    let setup = crate::AgentSetup {
        config,
        cwd,
        provider: "mock".into(),
        model: "m1".into(),
        tools: ToolSet::new(),
        client: Arc::new(Idle),
        resume: None,
        gate: None,
        mail: None,
        mcp: None,
        model_pinned: false,
    };
    Runtime::build(ends.events, setup).await.unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_persistence_failure_keeps_original_image_tail_and_positions() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let (mut handle, ends) = channels();
    let mut st = rt.state.lock().await;
    st.client = Arc::new(SummaryReply);
    st.events = ends.events;
    let tail = Message::new(
        Role::User,
        vec![
            Part::text("current"),
            Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/tail.png".into() },
            },
        ],
    );
    let history = vec![Message::user("earlier task"), Message::assistant("earlier work"), tail];
    for m in &history {
        st.session.append(m).unwrap();
    }
    st.history.replace(history.clone());
    st.checkpoints.push((2, PathBuf::from("test"), None));
    st.turn_start = 2;
    st.person_at = 2;
    let path = st.session.path().to_path_buf();
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(!super::compaction::compact_locked(&mut st, 2, &CancellationToken::new()).await);
    assert_eq!(st.history.to_vec(), history);
    assert_eq!(st.session.messages(), history);
    assert_eq!(crate::session::Session::open_path(&path).unwrap().messages(), history);
    assert_eq!(st.checkpoints[0].0, 2);
    assert_eq!((st.turn_start, st.person_at), (2, 2));
    assert_eq!(st.session.usage().input, 17);
    let mut events = Vec::new();
    while let Ok(event) = handle.events.try_recv() {
        events.push(event);
    }
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::Compacted { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Notice { level: Level::Error, .. })));
    std::fs::remove_dir(tmp).unwrap();
    assert!(super::compaction::compact_locked(&mut st, 2, &CancellationToken::new()).await);
    assert_eq!(st.history.to_vec(), st.session.messages());
    assert!(st.history.to_vec().last().unwrap().content.ends_with(&history.last().unwrap().content));
    assert_eq!(crate::session::Session::open_path(&path).unwrap().messages(), st.history.to_vec());
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_reopen_failure_keeps_committed_history_and_recovers_saving() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let (mut handle, ends) = channels();
    let mut st = rt.state.lock().await;
    st.client = Arc::new(SummaryReply);
    st.events = ends.events;
    let tail = Message::new(
        Role::User,
        vec![Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/tail.png".into() },
        }],
    );
    let history = vec![Message::user("earlier task"), Message::assistant("earlier work"), tail.clone()];
    for m in &history {
        st.session.append(m).unwrap();
    }
    st.history.replace(history);
    crate::session::FAIL_REOPEN.with(|f| f.set(true));
    let done = super::compaction::compact_locked(&mut st, 2, &CancellationToken::new()).await;
    crate::session::FAIL_REOPEN.with(|f| f.set(false));
    assert!(done, "rename committed despite failed reopen");
    assert_eq!(st.history.to_vec(), st.session.messages());
    assert!(st.history.to_vec().last().unwrap().content.ends_with(&tail.content));
    assert_eq!(crate::session::Session::open_path(st.session.path()).unwrap().messages(), st.history.to_vec());
    let mut events = Vec::new();
    while let Ok(event) = handle.events.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Compacted { .. })));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Notice { level: Level::Error, .. })));
    st.session.append(&Message::assistant("after")).unwrap();
    assert!(!st.session.is_lost());
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_request_the_curator_fitted_under_plain_says_so_with_what_it_held() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.limits.context_window = Some(8000);
    st.system = "You are a test.".into();
    st.policy = Box::new(Plain::default());
    let big = "0123456789abcdef0123456789abcdef0123456\n".repeat(1600);
    assert_eq!(big.chars().count(), 64_000);
    st.history.replace(vec![
        Message::user("look at the file big.rs"),
        Message::new(
            Role::Assistant,
            vec![Part::text("reading it"), Part::tool_call("c1", "read", serde_json::json!({"path": "big.rs"}))],
        ),
        Message::tool_results(vec![Part::tool_result("c1", big.clone(), false)]),
        Message::assistant("read it"),
        Message::user("now say hi"),
    ]);
    let call = Usage { input: 10, output: 1, ..Default::default() };
    let view = view_now(&mut st, 0).await;
    let (_, _, model) = st.target.get();
    let mut req = ModelRequest::new(model, st.system.clone(), view.messages);
    req.max_tokens = reply_tokens(&st);
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    // the curator cut the request: the result does not go whole, and the request takes under the window
    let sent = serde_json::to_string(&req.messages).unwrap();
    assert!(!sent.contains(&big), "the 64 000-char result went whole");
    let tokens = nexo_core::message::estimate_tokens(&req.messages);
    assert!(tokens < 8000, "{tokens} tokens");
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    // a history that fits goes as it is and says nothing
    st.history.replace(vec![Message::user("hi")]);
    let view = view_now(&mut st, 0).await;
    let (_, _, model) = st.target.get();
    let mut req = ModelRequest::new(model, st.system.clone(), view.messages);
    req.max_tokens = reply_tokens(&st);
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    host::add_usage(&mut st, &rt.events, "m1", call, call, UsageKind::Main);
    let lines = usage_lines(st.session.path());
    assert_eq!(lines.len(), 2, "{lines:?}");
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(first["fitted"], true, "{}", lines[0]);
    assert!(first["held"].as_u64().unwrap() > 10_000, "{}", lines[0]);
    let second: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert!(second.get("fitted").is_none(), "{}", lines[1]);
    assert_eq!(second["held"], 0, "{}", lines[1]);
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn prepared_map_reflects_the_fitted_request() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let (mut handle, ends) = channels();
    let big = "0123456789abcdef".repeat(4000);
    {
        let mut st = rt.state.lock().await;
        st.events = ends.events;
        st.limits.context_window = Some(8000);
        st.system = "test".into();
        st.history.replace(vec![
            Message::user("read"),
            Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({}))]),
            Message::tool_results(vec![Part::tool_result("c1", big.clone(), false)]),
            Message::assistant("done"),
            Message::user("hello"),
        ]);
    }
    let (req, _, _, _) = super::start::prepare(&rt.state, &CancellationToken::new(), false).await.unwrap();
    assert!(!serde_json::to_string(&req.messages).unwrap().contains(&big));
    let mut last = None;
    while let Ok(event) = handle.events.try_recv() {
        if let AgentEvent::ContextMap { calls, .. } = event {
            last = Some(calls);
        }
    }
    let calls = last.expect("prepared request emitted a map");
    assert_ne!(calls.iter().find(|(id, _)| id == "c1").unwrap().1, nexo_core::CallMark::Sent);
    assert_ne!(calls.iter().find(|(id, _)| id == "c1").unwrap().1, nexo_core::CallMark::Cached);
    std::env::remove_var("NEXO_DATA_DIR");
}

/// Return a known inventory so the real prepare path, not just its goal helper, is exercised.
struct GoalInventory(Option<context::Memory>);

#[async_trait::async_trait]
impl ContextPolicy for GoalInventory {
    fn name(&self) -> &str {
        "goal-inventory"
    }
    async fn view(&mut self, _: u64, history: &[Message]) -> anyhow::Result<View> {
        Ok(View {
            messages: history.to_vec(),
            tokens: nexo_core::message::estimate_tokens(history),
            note: None,
            memory: self.0.clone(),
        })
    }
    fn reset(&mut self) {}
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn prepare_checks_goal_inventory_before_extracting_memory() {
    use nexo_core::{CuratedItem, CuratedKind, CuratedState};
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let (mut handle, ends) = channels();
    rt.state.lock().await.events = ends.events;
    let item = CuratedItem { kind: CuratedKind::Goal, label: "goal".into(), tokens: 1, state: CuratedState::In };
    // Either kind of inventory must survive; empty or absent inventory still permits deduplication.
    for (curated, relation_quotes, has_memory, preserve) in [
        (vec![], Some(1), true, true),
        (vec![item], None, true, true),
        (vec![], Some(0), true, false),
        (vec![], None, false, false),
    ] {
        let original = Message::user("[goal set] hello");
        {
            let mut st = rt.state.lock().await;
            st.system = "test".into();
            st.goal = Some("hello".into());
            st.history.replace(vec![original.clone()]);
            st.policy = Box::new(GoalInventory(has_memory.then(|| context::Memory {
                zones: vec![],
                cells: vec![],
                curated: curated.clone(),
                relation_quotes,
            })));
        }
        let (req, _, _, _) = super::start::prepare(&rt.state, &CancellationToken::new(), false).await.unwrap();
        // Request-only goal notes may follow this part, but must not replace its carrier.
        assert_eq!(req.messages[0].content[0] == original.content[0], preserve);
        let mut inventory = None;
        while let Ok(event) = handle.events.try_recv() {
            if let AgentEvent::Curated { items, relation_quotes, .. } = event {
                inventory = Some((items, relation_quotes));
            }
        }
        let expected = if preserve { (curated, relation_quotes) } else { (vec![], None) };
        assert_eq!(inventory.expect("prepare announces final inventory"), expected);
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

/// Drive the last guard directly, after policy selection, so neither a helpful fallback nor a mock
/// provider can mask what the actual outbound request contains.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn window_guard_preserves_the_whole_turn_or_refuses_the_cut() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1000);
    st.system.clear();
    let ask = Message::user("keep my original request");
    let queued = Message::user("also check the second case");
    let job = Message::user("[background jobs ended] 7");
    let old = Message::assistant("x".repeat(8000));
    st.history.replace(vec![Message::user("earlier"), old.clone(), ask.clone(), queued.clone(), job.clone()]);
    st.turn_start = 2;
    st.person_turn_version = Some(st.history.version());
    let request = |mut messages: Vec<Message>| {
        attach_hidden(&mut messages, "only on this request".into());
        let mut req = ModelRequest::new("m1", "", messages);
        req.max_tokens = 256;
        req
    };
    let note = Some("only on this request");
    // The curated prefix no longer has the same length as the history prefix.
    let mut req = request(vec![old.clone(), ask.clone(), queued.clone(), job.clone()]);
    super::window_fit::fit(&mut st, &mut req, note, 0).await.unwrap();
    assert_eq!(req.messages, request(vec![ask.clone(), queued.clone(), job.clone()]).messages);
    // An unrecognisable first or queued request must not be dropped in favour of the exact job note.
    for tail in [vec![queued.clone(), job.clone()], vec![ask.clone(), job.clone()]] {
        let mut req = request([vec![old.clone()], tail].concat());
        assert!(super::window_fit::fit(&mut st, &mut req, note, 0).await.is_err());
    }
    // A supervisor resume can itself be the first user-role message of a turn.
    st.history.replace(vec![ask, old, Message::user(SUPERVISOR_RESUME_MARK), queued]);
    st.person_turn_version = None;
    let mut req = request(st.history.to_vec());
    assert!(super::window_fit::fit(&mut st, &mut req, note, 0).await.is_err());
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn window_guard_omitted_read_can_be_read_again_at_a_smaller_range() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1000);
    st.system.clear();
    let body = "one long line\n".repeat(1000);
    let path = d.path().join("a.txt");
    std::fs::write(&path, &body).unwrap();
    assert!(st.reads.after_read(&path, "a.txt", None, 1000, "c1", usize::MAX).is_none());
    st.history.replace(vec![
        Message::user("read a.txt"),
        Message::new(Role::Assistant, vec![Part::tool_call("c1", "read", serde_json::json!({"path":"a.txt"}))]),
        Message::tool_results(vec![Part::tool_result("c1", body.clone(), false)]),
    ]);
    let mut req = ModelRequest::new("m1", "", st.history.to_vec());
    req.max_tokens = 256;
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    assert_ne!(req.messages.last(), st.history.last(), "the guard actually omitted the read");
    assert!(st.reads.after_read(&path, "a.txt", Some(99), 1, "c2", usize::MAX).is_none(), "line 99 was not sent");
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_image_recovery_preserves_text_and_fails_closed_after_repair_or_history_change() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1000);
    st.system.clear();
    let ask = Message::user("identical human words");
    let old = Message::new(
        Role::User,
        vec![
            Part::text("historical instruction: preserve these exact words"),
            Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
            },
        ],
    );
    let historical_text = Message::new(
        Role::User,
        vec![
            Part::text("historical instruction: preserve these exact words"),
            Part::text("[Historical image content omitted to fit the context window.]"),
        ],
    );
    st.history.replace(vec![ask.clone(), old.clone(), ask.clone()]);
    st.turn_start = 2;
    st.person_turn_version = Some(st.history.version());
    // Simulate all prior transforms including a misleading duplicate in the prefix and
    // an altered current request. The fresh render must take history[2], not that duplicate.
    let merged = Message::user(format!("{} transformed current tail", "x".repeat(8000)));
    let request = || {
        let mut req = ModelRequest::new("m1", "", vec![merged.clone()]);
        req.max_tokens = 256;
        req
    };
    let mut req = request();
    super::window_fit::fit(&mut st, &mut req, Some("request-only note"), 0).await.unwrap();
    let mut expected = vec![ask.clone(), historical_text.clone(), ask.clone()];
    attach_hidden(&mut expected, "request-only note".into());
    assert_eq!(req.messages, expected);
    assert_eq!(st.history[2], ask, "outgoing recovery never mutates the transcript");

    st.person_turn_version = None;
    assert!(super::window_fit::fit(&mut st, &mut request(), None, 0).await.is_err());
    st.person_turn_version = Some(st.history.version());
    st.history.replace(vec![old.clone(), ask.clone()]); // stale history version: rewind/rewrite
    assert!(super::window_fit::fit(&mut st, &mut request(), None, 0).await.is_err());
    st.turn_start = 1;
    st.person_turn_version = Some(st.history.version());
    let mut req = request();
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    assert_eq!(req.messages, vec![historical_text, ask.clone()], "fresh history frontier works after rewind");

    st.history.push(Message::user("queued ask must not be omitted"));
    let mut queued_req = request();
    super::window_fit::fit(&mut st, &mut queued_req, None, 0).await.unwrap();
    assert_eq!(queued_req.messages.last(), st.history.last(), "queued text is preserved, not classified or dropped");
    st.history.replace(vec![
        old,
        Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({}))]),
        ask,
    ]);
    st.turn_start = 2;
    st.person_turn_version = Some(st.history.version());
    assert!(
        super::window_fit::fit(&mut st, &mut request(), None, 0).await.is_err(),
        "repair inserts a result: boundary invalidated, do not cut"
    );
    // An image-only historical message keeps its position with a clear omission marker.
    st.history.replace(vec![
        Message::new(
            Role::User,
            vec![Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/only.png".into() },
            }],
        ),
        Message::user("current request"),
    ]);
    st.turn_start = 1;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("m1", "", st.history.to_vec());
    req.max_tokens = 256;
    let original = st.history.to_vec();
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    assert_eq!(
        req.messages,
        vec![Message::user("[Historical image content omitted to fit the context window.]"), original[1].clone(),]
    );
    assert_eq!(st.history.to_vec(), original);
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_image_only_history_preserves_roles_and_tool_structure() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1000);
    st.system.clear();
    let image =
        Part::Image { source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/only.png".into() } };
    let marker = Part::text("[Historical image content omitted to fit the context window.]");
    for role in [Role::User, Role::Assistant] {
        let original = vec![
            Message::user("Keep this instruction verbatim."),
            Message::new(role, vec![image.clone(), image.clone()]),
            Message::new(
                Role::Assistant,
                vec![Part::tool_call("image-call", "read", serde_json::json!({"path":"shot.png"}))],
            ),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "image-call".into(),
                name: Some("read".into()),
                content: vec![
                    Part::text("before"),
                    image.clone(),
                    Part::text("between"),
                    image.clone(),
                    Part::text("after"),
                ],
                is_error: true,
            }]),
            Message::user("current request"),
        ];
        st.history.replace(original.clone());
        st.turn_start = 4;
        st.person_turn_version = Some(st.history.version());
        let mut req = ModelRequest::new("m1", "", original.clone());
        req.max_tokens = 256;
        super::window_fit::fit(&mut st, &mut req, Some("request-only note"), 0).await.unwrap();
        let mut expected = original.clone();
        expected[1].content = vec![marker.clone()];
        let Part::ToolResult { content, .. } = &mut expected[3].content[0] else { unreachable!() };
        *content =
            vec![Part::text("before"), marker.clone(), Part::text("between"), marker.clone(), Part::text("after")];
        attach_hidden(&mut expected, "request-only note".into());
        assert_eq!(req.messages, expected, "{role:?}: only image bodies may change");
        assert!(req.messages.iter().all(|m| !m.content.is_empty()));
        assert!(nexo_core::message::is_well_formed(&req.messages));
        assert_eq!(st.history.to_vec(), original, "raw image payloads remain persisted");
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_image_recovery_counts_all_opaque_bytes_and_preserves_mixed_continuations() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(2000);
    st.system.clear();
    st.tok_ratio = Some(0.01); // a learned prose ratio must not discount opaque bytes
    for large in [false, true] {
        for kind in ["signed", "encrypted", "redacted", "nested"] {
            let payload = if large { "\"\\\n🦀".repeat(1000) } else { "small".into() };
            let opaque = match kind {
                "signed" | "nested" => Part::Reasoning {
                    text: "think".into(),
                    signature: Some(payload),
                    encrypted: None,
                    model: Some("m1".into()),
                },
                "encrypted" => Part::Reasoning {
                    text: "think".into(),
                    signature: None,
                    encrypted: Some(payload),
                    model: Some("m1".into()),
                },
                _ => Part::RedactedReasoning { data: payload, model: Some("m1".into()) },
            };
            let mut old = Message::user("Never delete files.");
            old.content.extend((0..6).map(|_| Part::Image {
                source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
            }));
            let mut history = vec![old];
            if kind == "nested" {
                history
                    .push(Message::new(Role::Assistant, vec![Part::tool_call("prior", "read", serde_json::json!({}))]));
                history.push(Message::tool_results(vec![Part::ToolResult {
                    call_id: "prior".into(),
                    name: Some("read".into()),
                    content: vec![Part::text("keep this result"), opaque],
                    is_error: false,
                }]));
            } else {
                history.push(Message::new(Role::Assistant, vec![opaque, Part::text("prior answer")]));
            }
            st.turn_start = history.len();
            history.extend([
                Message::user("current human instruction"),
                Message::new(
                    Role::Assistant,
                    vec![Part::tool_call("c", "read", serde_json::json!({"path":"policy.txt"}))],
                ),
                Message::new(
                    Role::User,
                    vec![
                        Part::tool_result("c", "entire result", false),
                        Part::text("internal instruction: summarize now"),
                    ],
                ),
                Message::user("queued human request must survive as well"),
            ]);
            st.history.replace(history.clone());
            st.person_turn_version = Some(st.history.version());
            // Simulate an oversized policy render; recovery must use source provenance.
            let mut req = ModelRequest::new("m2", "", vec![Message::user("x".repeat(1_000_000))]);
            req.max_tokens = 256;
            let result = super::window_fit::fit(&mut st, &mut req, None, 0).await;
            if large {
                assert!(result.is_err(), "{kind}: escaped/multibyte opaque payload must not be discounted");
            } else {
                result.unwrap();
                history[0].content.retain(|p| !matches!(p, Part::Image { .. }));
                history[0].content.push(Part::text("[Historical image content omitted to fit the context window.]"));
                assert_eq!(req.messages, history, "{kind}: all non-image parts and full turn retained");
                assert!(nexo_core::message::is_well_formed(&req.messages));
                assert!(
                    st.est_sent > nexo_core::message::estimate_tokens(&req.messages),
                    "reserve includes opaque bytes even in historical/nested blocks"
                );
            }
            assert_eq!(st.history[0].content.len(), 7, "raw images remain persisted");
        }
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_images_are_not_discounted_by_low_text_ratio() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(2000);
    st.limits.max_output = None;
    st.system.clear();
    st.tok_ratio = Some(0.01);
    let image = Part::Image {
        source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/image.png".into() },
    };
    for nested in [false, true] {
        let part = if nested {
            Part::ToolResult {
                call_id: "c".into(),
                name: Some("read".into()),
                content: vec![image.clone()],
                is_error: false,
            }
        } else {
            image.clone()
        };
        let message = Message::new(Role::User, vec![part.clone(), part]);
        let mut req = ModelRequest::new("model", "", vec![message.clone()]);
        req.max_tokens = 256;
        assert!(!super::window_fit::side_fits(&st, &mut req), "nested={nested}");
        st.history.replace(vec![message]);
        st.turn_start = 0;
        assert!(super::window_fit::fit(&mut st, &mut req, None, 0).await.is_err(), "nested={nested}");
    }
    // Text-only requests retain the learned ratio.
    let mut req = ModelRequest::new("model", "", vec![Message::user("x".repeat(8000))]);
    req.max_tokens = 256;
    assert!(super::window_fit::side_fits(&st, &mut req));
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_recovered_current_images_keep_their_reserve() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(2000);
    st.limits.max_output = None;
    st.system.clear();
    st.tok_ratio = Some(0.01);
    let image = Part::Image {
        source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/image.png".into() },
    };
    let history = vec![
        Message::new(Role::User, vec![Part::text("old"), image.clone()]),
        Message::new(Role::Assistant, vec![Part::text("old answer")]),
        Message::new(Role::User, vec![Part::text("current"), image.clone(), image]),
    ];
    assert!(nexo_core::message::is_well_formed(&history));
    st.history.replace(history.clone());
    st.turn_start = 2;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("model", "", vec![Message::user("x".repeat(1_000_000))]);
    req.max_tokens = 256;
    assert!(super::window_fit::fit(&mut st, &mut req, None, 0).await.is_err());
    assert_eq!(
        req.messages[0].content,
        vec![Part::text("old"), Part::text("[Historical image content omitted to fit the context window.]")],
        "only historical images are removed"
    );
    assert_eq!(req.messages[2], history[2], "current images must survive recovery");
    assert!(nexo_core::message::is_well_formed(&req.messages));
    assert_eq!(st.history.to_vec(), history, "recovery must not modify stored history");
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_nested_historical_images_are_replaced_in_place() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1000);
    st.system.clear();
    let image =
        Part::Image { source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() } };
    let marker = Part::text("[Historical image content omitted to fit the context window.]");
    let nested = |content| Part::ToolResult {
        call_id: "inner".into(),
        name: Some("nested-read".into()),
        is_error: true,
        content,
    };
    let outer = |content| Part::ToolResult { call_id: "c".into(), name: Some("read".into()), is_error: false, content };
    let history = vec![
        Message::user("read"),
        Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({"path":"old"}))]),
        Message::tool_results(vec![outer(vec![
            Part::text("outer before"),
            image.clone(),
            nested(vec![
                Part::text("inner before"),
                image.clone(),
                image.clone(),
                Part::text("inner after"),
                image.clone(),
            ]),
            image.clone(),
            Part::text("outer after"),
        ])]),
        Message::user("current request"),
    ];
    assert!(nexo_core::message::is_well_formed(&history));
    st.history.replace(history.clone());
    st.turn_start = 3;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("m1", "", history.clone());
    req.max_tokens = 256;
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    let mut expected = history.clone();
    expected[2] = Message::tool_results(vec![outer(vec![
        Part::text("outer before"),
        marker.clone(),
        nested(vec![Part::text("inner before"), marker.clone(), Part::text("inner after"), marker.clone()]),
        marker,
        Part::text("outer after"),
    ])]);
    assert_eq!(req.messages, expected, "only historical image runs are replaced, at each nesting level");
    assert!(nexo_core::message::is_well_formed(&req.messages));
    assert_eq!(st.history.to_vec(), history, "the raw transcript remains intact");
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_nested_images_in_paired_result_keep_their_reserve() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(2000);
    st.limits.max_output = None;
    st.system.clear();
    st.tok_ratio = Some(0.01);
    let image = Part::Image {
        source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/image.png".into() },
    };
    let history = vec![
        Message::user("read"),
        Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({}))]),
        Message::tool_results(vec![Part::ToolResult {
            call_id: "c".into(),
            name: Some("read".into()),
            is_error: false,
            content: vec![
                Part::text("mixed"),
                Part::ToolResult {
                    call_id: "inner".into(),
                    name: Some("read".into()),
                    is_error: false,
                    content: vec![Part::text("nested"), image.clone(), image],
                },
            ],
        }]),
    ];
    assert!(nexo_core::message::is_well_formed(&history));
    st.history.replace(history.clone());
    st.turn_start = 0;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("model", "", history.clone());
    req.max_tokens = 256;
    assert!(!super::window_fit::side_fits(&st, &mut req));
    assert!(super::window_fit::fit(&mut st, &mut req, None, 0).await.is_err());
    assert_eq!(req.messages, history);
    assert_eq!(st.history.to_vec(), history);
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn side_admission_respects_model_limit_and_discards_stale_ceiling() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    let mut req = ModelRequest::new("model", "", vec![Message::user("hello")]);
    req.max_tokens = 200;
    st.limits.max_output = Some(100);
    for window in [Some(10_000), None] {
        st.limits.context_window = window;
        req.output_ceiling = Some(1);
        assert!(!super::window_fit::side_fits(&st, &mut req));
        assert_eq!(req.output_ceiling, Some(100));
        st.limits.max_output = Some(300);
        assert!(super::window_fit::side_fits(&st, &mut req));
        assert_eq!(req.output_ceiling, Some(300));
        st.limits.max_output = Some(100);
    }
    st.limits.context_window = None;
    st.limits.max_output = None;
    req.output_ceiling = Some(1);
    assert!(super::window_fit::side_fits(&st, &mut req));
    assert_eq!(req.output_ceiling, None);
    std::env::remove_var("NEXO_DATA_DIR");
}

/// Optional calls use the configured protocol and request model (not the active
/// target model), and low learned prose ratios cannot discount retained reserves.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_opaque_side_fits_exception_boundaries() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.limits.context_window = Some(2000);
    st.limits.max_output = Some(512);
    st.tok_ratio = Some(0.01);
    for protocol in ["anthropic", "openai-responses", "openai"] {
        let (name, mut provider, _) = st.target.get();
        provider.protocol = protocol.into();
        st.target.set(name, provider, "active-model-not-request-model");
        for kind in ["foreign", "foreign-redacted", "own", "legacy", "nested-foreign", "mixed"] {
            for large in [false, true] {
                let payload = "opaque_".repeat(if large { 400 } else { 1 });
                let foreign = Part::Reasoning {
                    text: "foreign text".into(),
                    signature: Some(payload.clone()),
                    encrypted: None,
                    model: Some("foreign-model".into()),
                };
                let own = Part::Reasoning {
                    text: "own text".into(),
                    signature: Some(payload.clone()),
                    encrypted: None,
                    model: (kind != "legacy").then(|| "request-model".into()),
                };
                let parts = match kind {
                    "foreign" => vec![foreign],
                    "foreign-redacted" => {
                        vec![Part::RedactedReasoning { data: payload, model: Some("foreign-model".into()) }]
                    }
                    "nested-foreign" => vec![Part::ToolResult {
                        call_id: "nested".into(),
                        name: Some("read".into()),
                        content: vec![foreign],
                        is_error: false,
                    }],
                    "mixed" => vec![foreign, own],
                    _ => vec![own],
                };
                let mut req = ModelRequest::new("request-model", "", vec![Message::new(Role::Assistant, parts)]);
                req.max_tokens = 256;
                let before = req.messages.clone();
                let exempt = matches!(protocol, "anthropic" | "openai-responses")
                    && matches!(kind, "foreign" | "foreign-redacted");
                let expected = !large || exempt;
                assert_eq!(super::window_fit::side_fits(&st, &mut req), expected, "{protocol}/{kind}/{large}");
                assert_eq!(req.messages, before, "admission must not mutate optional messages");
                assert_eq!(req.max_tokens, 256);
                if expected {
                    assert_eq!(req.output_ceiling, Some(512));
                } else {
                    assert_eq!(req.output_ceiling, Some(0));
                }
            }
        }
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_image_recovery_trims_only_current_results() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(1500);
    st.system.clear();
    let mut old = Message::user("Never delete files.");
    old.content.push(Part::Image {
        source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/old.png".into() },
    });
    let original = vec![
        old,
        Message::new(Role::Assistant, vec![Part::tool_call("prior", "read", serde_json::json!({}))]),
        Message::tool_results(vec![Part::tool_result("prior", "historical output must survive".repeat(12), false)]),
        Message::user("Read current data; keep instructions."),
        Message::new(Role::Assistant, vec![Part::tool_call("current", "read", serde_json::json!({"path":"data"}))]),
        Message::new(
            Role::User,
            vec![
                Part::tool_result("current", "x".repeat(20_000), true),
                Part::text("Internal instruction must survive."),
            ],
        ),
        Message::user("Queued instruction must survive."),
    ];
    st.history.replace(original.clone());
    st.turn_start = 3;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("m1", "", original.clone());
    req.max_tokens = 256;
    super::window_fit::fit(&mut st, &mut req, Some("request note"), 0).await.unwrap();
    let mut expected = original.clone();
    expected[0].content.pop();
    expected[0].content.push(Part::text("[Historical image content omitted to fit the context window.]"));
    let Part::ToolResult { content, .. } = &mut expected[5].content[0] else { unreachable!() };
    *content =
        vec![Part::text("[curator] Tool output omitted to fit the context window. Request a smaller range if needed.")];
    attach_hidden(&mut expected, "request note".into());
    assert_eq!(req.messages, expected);
    assert_eq!(st.history.to_vec(), original);
    assert!(nexo_core::message::is_well_formed(&req.messages));
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_current_foreign_signature_is_not_transmitted() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(2000);
    st.system.clear();
    for protocol in ["anthropic", "openai-responses", "openai"] {
        let (name, mut provider, _) = st.target.get();
        provider.protocol = protocol.into();
        st.target.set(name, provider, "m1");
        for foreign in [true, false] {
            let original = vec![
                Message::user("Preserve instructions."),
                Message::new(
                    Role::Assistant,
                    vec![
                        Part::Reasoning {
                            text: "thinking".into(),
                            signature: Some("s".repeat(40_000)),
                            encrypted: None,
                            model: Some(if foreign { "other" } else { "m1" }.into()),
                        },
                        Part::tool_call("c", "read", serde_json::json!({})),
                    ],
                ),
                Message::tool_results(vec![Part::tool_result("c", "short result", false)]),
            ];
            st.history.replace(original.clone());
            st.turn_start = 0;
            st.person_turn_version = Some(st.history.version());
            let mut req = ModelRequest::new("m1", "", original.clone());
            req.max_tokens = 256;
            let allowed = foreign && protocol != "openai";
            assert_eq!(
                super::window_fit::fit(&mut st, &mut req, None, 0).await.is_ok(),
                allowed,
                "{protocol}/{foreign}"
            );
            assert_eq!(super::window_fit::side_fits(&st, &mut req), allowed, "side {protocol}/{foreign}");
            assert_eq!(req.messages, original);
            assert_eq!(st.history.to_vec(), original);
        }
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_mixed_tool_result_trims_only_text_and_fails_if_opaque_still_overflows() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(4000);
    st.system.clear();
    for large in [false, true] {
        let image = Part::Image {
            source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/current.png".into() },
        };
        let opaque = Part::RedactedReasoning {
            data: if large { "opaque".repeat(1000) } else { "small".into() },
            model: Some("m1".into()),
        };
        let original = vec![
            Message::user("inspect"),
            Message::new(Role::Assistant, vec![Part::tool_call("c", "read", serde_json::json!({}))]),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "c".into(),
                name: Some("read".into()),
                content: vec![Part::text("a".repeat(12000)), image.clone(), Part::text("tail"), opaque.clone()],
                is_error: true,
            }]),
        ];
        st.history.replace(original.clone());
        st.turn_start = 0;
        st.person_turn_version = Some(st.history.version());
        let mut req = ModelRequest::new("m1", "", original.clone());
        req.max_tokens = 256;
        let result = super::window_fit::fit(&mut st, &mut req, None, 0).await;
        assert_eq!(result.is_err(), large, "{result:?}");
        if large {
            assert!(result.unwrap_err().to_string().contains("request over the window"));
        }
        let mut expected = original.clone();
        let Part::ToolResult { content, .. } = &mut expected[2].content[0] else { unreachable!() };
        *content = vec![
            Part::text("[curator] Tool output omitted to fit the context window. Request a smaller range if needed."),
            image,
            opaque,
        ];
        assert_eq!(req.messages, expected);
        assert_eq!(st.history.to_vec(), original);
    }
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admission_multiple_mixed_results_incrementally_fit_same_message() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    st.limits.context_window = Some(7000);
    st.system.clear();
    let (name, mut provider, _) = st.target.get();
    provider.protocol = "openai".into();
    st.target.set(name, provider, "m1");
    let image = Part::Image {
        source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/current.png".into() },
    };
    let opaque = Part::RedactedReasoning { data: "opaque".into(), model: Some("m1".into()) };
    let original = vec![
        Message::user("inspect"),
        Message::new(
            Role::Assistant,
            (0..3).map(|i| Part::tool_call(format!("c{i}"), "read", serde_json::json!({}))).collect(),
        ),
        Message::tool_results(vec![
            Part::ToolResult {
                call_id: "c0".into(),
                name: Some("read".into()),
                content: vec![image.clone(), Part::text("a".repeat(12000)), opaque.clone(), Part::text("tail")],
                is_error: false,
            },
            Part::ToolResult {
                call_id: "c1".into(),
                name: Some("read".into()),
                content: vec![opaque.clone(), Part::text("b".repeat(10000)), image.clone(), Part::text("tail")],
                is_error: true,
            },
            Part::tool_result("c2", "c".repeat(3000), false),
        ]),
    ];
    st.history.replace(original.clone());
    st.turn_start = 0;
    st.person_turn_version = Some(st.history.version());
    let mut req = ModelRequest::new("m1", "", original.clone());
    req.max_tokens = 256;
    super::window_fit::fit(&mut st, &mut req, None, 0).await.unwrap();
    let marker =
        Part::text("[curator] Tool output omitted to fit the context window. Request a smaller range if needed.");
    let mut expected = original.clone();
    let Part::ToolResult { content, .. } = &mut expected[2].content[0] else { unreachable!() };
    *content = vec![image.clone(), marker.clone(), opaque.clone()];
    let Part::ToolResult { content, .. } = &mut expected[2].content[1] else { unreachable!() };
    *content = vec![opaque.clone(), marker, image];
    assert_eq!(req.messages, expected);
    assert_eq!(st.history.to_vec(), original);
    assert_eq!(
        st.est_sent,
        nexo_core::message::estimate_tokens(&expected) + 2 * serde_json::to_vec(&opaque).unwrap().len() as u64
    );
    assert!(super::window_fit::provider_tokens(st.est_sent, Some(4.0 / 3.0)) + u64::from(req.max_tokens) <= 7000);
    assert_eq!(req.max_tokens, 256);
    assert!(nexo_core::message::is_well_formed(&req.messages));
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_recovery_survives_interruption_and_late_old_writer() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.client = Arc::new(SummaryClient);
    st.policy = Box::new(crate::context::BuiltIn::new("").unwrap());
    let old = vec![
        Message::user("Never delete the logs."),
        Message::assistant("Understood."),
        Message::user("Continue work."),
        Message::assistant("Ok."),
    ];
    for m in &old {
        st.session.append(m).unwrap();
    }
    st.history.replace(old.clone());
    st.policy.view(0, &old).await.unwrap();
    let old_snapshot = st.policy.save().unwrap()();
    let path = st.session.path().to_path_buf();
    assert!(compact_locked(&mut st, 2, &CancellationToken::new()).await);
    let replacement = Session::open_path(&path).unwrap().messages().to_vec();
    // Simulate a delayed background save and a restart immediately after the session rename.
    crate::context::saved::write(&path, Box::new(move || old_snapshot)).unwrap();
    st.history.replace(replacement.clone());
    st.policy = Box::new(crate::context::BuiltIn::new("").unwrap());
    super::saved_state::load(&mut st);
    let view = st.policy.view(0, &replacement).await.unwrap();
    assert!(view.messages.iter().any(|m| m.text().contains("Never delete the logs.")));
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_snapshot_write_failure_does_not_replace_session() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old =
        vec![Message::user("Never delete the logs."), Message::assistant("Understood."), Message::user("Continue.")];
    // A first, identical compaction in a scratch session reveals the recovery slot's name, which depends
    // only on the history (the same summary gives the same one).
    let slot_suffix = {
        let d = tempfile::tempdir().unwrap();
        let rt = runtime_in(d.path()).await;
        let mut st = rt.state.lock().await;
        st.client = Arc::new(SummaryClient);
        st.policy = Box::new(crate::context::BuiltIn::new("").unwrap());
        for m in &old {
            st.session.append(m).unwrap();
        }
        st.history.replace(old.clone());
        st.policy.view(0, &old).await.unwrap();
        let path = st.session.path().to_path_buf();
        assert!(compact_locked(&mut st, 2, &CancellationToken::new()).await);
        let prefix = format!("{}.curator.compact.", path.file_name().unwrap().to_str().unwrap());
        let slot = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with(&prefix) && !n.ends_with(".tmp"))
            .expect("the compaction left no recovery slot");
        slot[path.file_name().unwrap().to_str().unwrap().len() - "jsonl".len()..].to_string()
    };
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.client = Arc::new(SummaryClient);
    st.policy = Box::new(crate::context::BuiltIn::new("").unwrap());
    for m in &old {
        st.session.append(m).unwrap();
    }
    st.history.replace(old.clone());
    st.policy.view(0, &old).await.unwrap();
    let path = st.session.path().to_path_buf();
    // A non-empty directory in the recovery slot's place makes committing it fail on every platform
    // (renaming a directory that holds open files is refused on Windows, so the parent is left alone).
    let slot = path.with_extension(&slot_suffix);
    std::fs::create_dir(&slot).unwrap();
    std::fs::write(slot.join("occupied"), b"x").unwrap();
    assert!(!compact_locked(&mut st, 2, &CancellationToken::new()).await);
    assert_eq!(st.history.to_vec(), old);
    assert_eq!(st.session.messages(), old);
    std::fs::remove_dir_all(&slot).unwrap();
    assert_eq!(Session::open_path(&path).unwrap().messages(), old);
    std::env::remove_var("NEXO_DATA_DIR");
}

struct SummaryClient;

#[async_trait::async_trait]
impl ModelClient for SummaryClient {
    async fn stream(&self, _: &ProviderConfig, _: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(futures::stream::iter(vec![StreamEvent::TextDelta("summary".into())])))
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_rewrite_io_failure_keeps_history_and_indices() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.client = Arc::new(SummaryClient);
    let history = vec![Message::user("task"), Message::assistant("work"), Message::user("next")];
    for message in &history {
        st.session.append(message).unwrap();
    }
    st.history.replace(history.clone());
    st.checkpoints.push((2, PathBuf::from("test"), None));
    st.turn_start = 2;
    st.person_at = 2;
    let path = st.session.path().to_path_buf();
    let tmp = path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(!compact_locked(&mut st, 2, &CancellationToken::new()).await);
    assert_eq!(st.history.to_vec(), history);
    assert_eq!(st.session.messages(), history);
    assert_eq!(st.checkpoints[0].0, 2);
    assert_eq!((st.turn_start, st.person_at), (2, 2));
    assert_eq!(Session::open_path(&path).unwrap().messages(), history);
    std::fs::remove_dir(tmp).unwrap();
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn compact_reopen_failure_keeps_replaced_file_and_memory_aligned() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    let (mut handle, ends) = channels();
    st.events = ends.events;
    st.client = Arc::new(SummaryClient);
    let history = vec![Message::user("task"), Message::assistant("work"), Message::user("next")];
    for message in &history {
        st.session.append(message).unwrap();
    }
    st.session.hold_output("held-call", &nexo_core::tool::ToolOutput::ok("held output"));
    st.history.replace(history.clone());
    st.turn_start = 2;
    st.person_at = 2;
    let path = st.session.path().to_path_buf();
    crate::session::FAIL_REOPEN.with(|f| f.set(true));
    let result = compact_locked(&mut st, 2, &CancellationToken::new()).await;
    crate::session::FAIL_REOPEN.with(|f| f.set(false));
    assert!(result);
    assert_ne!(st.history.to_vec(), history);
    assert_eq!(st.session.messages(), st.history.to_vec());
    assert_eq!(Session::open_path(&path).unwrap().messages(), st.history.to_vec());
    assert_eq!((st.turn_start, st.person_at), (0, 0));
    assert!(std::iter::from_fn(|| handle.events.try_recv().ok()).any(|e| matches!(e, AgentEvent::Compacted { .. })));

    // Losing our lock after the rename must not make our own replacement look like another writer's.
    std::fs::remove_file(path.with_extension("jsonl.lock")).unwrap();
    let next = Message::tool_results(vec![Part::tool_result("held-call", "held output", false)]);
    st.session.append(&next).unwrap();
    assert!(!st.session.is_lost());
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches(r#"{"type":"tool_output""#).count(), 1, "{text}");
    assert_eq!(Session::open_path(&path).unwrap().messages().last(), Some(&next));
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn rewind_reopen_failure_keeps_history_aligned_and_recovers_after_lock_loss() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let (mut handle, ends) = channels();
    let history = vec![Message::user("task"), Message::assistant("work"), Message::user("next")];
    let path = {
        let mut st = rt.state.lock().await;
        st.events = ends.events;
        for message in &history {
            st.session.append(message).unwrap();
        }
        st.history.replace(history.clone());
        st.turn_start = 2;
        st.person_at = 2;
        st.session.path().to_path_buf()
    };
    crate::session::FAIL_REOPEN.with(|f| f.set(true));
    super::commands::rewind(&rt.state, 2).await;
    crate::session::FAIL_REOPEN.with(|f| f.set(false));
    let mut st = rt.state.lock().await;
    assert_eq!(st.history.to_vec(), history[..2]);
    assert_eq!(st.session.messages(), &history[..2]);
    assert_eq!(Session::open_path(&path).unwrap().messages(), &history[..2]);
    assert_eq!((st.turn_start, st.person_at), (0, 0));
    let events: Vec<_> = std::iter::from_fn(|| handle.events.try_recv().ok()).collect();
    assert!(events.iter().any(|e| matches!(e, AgentEvent::Notice { level: Level::Warn, text, .. } if text.contains("rewound") && text.contains("reopen"))));
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { level: Level::Error, text, .. } if text.contains("cannot rewind"))));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::HistoryCleared)));

    std::fs::remove_file(path.with_extension("jsonl.lock")).unwrap();
    let next = Message::user("after rewind");
    st.session.append(&next).unwrap();
    assert!(!st.session.is_lost());
    let mut expected = history[..2].to_vec();
    expected.push(next);
    assert_eq!(Session::open_path(&path).unwrap().messages(), expected);
    std::env::remove_var("NEXO_DATA_DIR");
}

/// The curator's own view is measured in its estimate; admission counts images and opaque reasoning stricter, so a
/// view can pass the window by a little. With images in the history the request is not refused then: the fitting
/// curator cuts it again under the window (the view that went over is the request as built here).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_curator_view_that_passes_the_window_by_a_little_is_cut_again_not_refused() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    st.policy = Box::new(context::BuiltIn::lenient(""));
    assert!(st.policy.fits_window());
    st.limits.context_window = Some(8000);
    st.limits.max_output = None;
    st.system = "You are a test.".into();
    let image =
        Part::Image { source: nexo_core::message::ImageSource::Url { url: "https://example.invalid/shot.png".into() } };
    let big = "fn main() { println!(\"line\"); }\n".repeat(1200);
    st.history.replace(vec![
        Message::new(Role::User, vec![Part::text("look at this screenshot"), image]),
        Message::assistant("I see a login form."),
        Message::user("read big.rs"),
        Message::new(
            Role::Assistant,
            vec![Part::text("reading it"), Part::tool_call("c1", "read", serde_json::json!({"path": "big.rs"}))],
        ),
        Message::tool_results(vec![Part::tool_result("c1", big.clone(), false)]),
        Message::assistant("read it"),
        Message::user("now say hi"),
    ]);
    st.turn_start = 6;
    let (_, _, model) = st.target.get();
    let mut req = ModelRequest::new(model, st.system.clone(), st.history.to_vec());
    req.max_tokens = 256;
    super::window_fit::fit(&mut st, &mut req, None, 0).await.expect("cut again, not refused");
    let sent = serde_json::to_string(&req.messages).unwrap();
    assert!(!sent.contains(&big), "the 38 000-char result went whole");
    assert!(sent.contains("now say hi"), "the person's newest words go as written");
    assert!(st.est_sent < 8000, "{} estimated tokens", st.est_sent);
    std::env::remove_var("NEXO_DATA_DIR");
}

/// Offline diagnostic; takes a scratch copy, never the live session directory.
#[tokio::test]
#[ignore]
#[allow(clippy::await_holding_lock)]
async fn copied_session_admission_diagnostic() {
    let _g = crate::DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = std::env::var("NEXO_REPRO_SESSION").expect("scratch session path");
    assert!(!path.contains("/.nexo/"));
    let lines: Vec<serde_json::Value> =
        std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let messages: Vec<Message> = lines
        .iter()
        .filter(|v| v["type"] == "message")
        .map(|v| serde_json::from_value(v["message"].clone()).unwrap())
        .collect();
    let d = tempfile::tempdir().unwrap();
    let rt = runtime_in(d.path()).await;
    let mut st = rt.state.lock().await;
    let mut cfg = ProviderConfig::new("openai-responses", "http://offline.invalid", None);
    cfg.models = vec!["gpt-6.2-astra".into()];
    st.target = Target::new("codex", cfg.clone(), "gpt-6.2-astra");
    st.limits.context_window = Some(272_000);
    st.history.replace(messages.clone());
    st.policy = Box::new(context::BuiltIn::lenient("relations=1"));
    st.system = "s".repeat(4_799 * 4);
    st.tools = nexo_tools::builtin();
    let view = start::view_now(&mut st, 0).await;
    println!("RAW messages={} latest_person={:?} estimate={} CURATOR messages={} latest_person={:?} estimate={} view_tokens={}", messages.len(), nexo_core::message::latest_person(&messages), nexo_core::message::estimate_tokens(&messages), view.messages.len(), nexo_core::message::latest_person(&view.messages), nexo_core::message::estimate_tokens(&view.messages), view.tokens);
    let mut req = ModelRequest::new("gpt-6.2-astra", st.system.clone(), view.messages);
    req.tools = st.tools.specs();
    req.max_tokens = 256;
    let old_opaque: u64 = req
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|p| {
            matches!(p, Part::Reasoning { .. } | Part::RedactedReasoning { .. }) && !p.is_foreign_reasoning(&req.model)
        })
        .map(|p| serde_json::to_vec(p).unwrap().len() as u64)
        .sum();
    println!(
        "OLD_ALGORITHM conversation={} opaque_reserve={} total_with_reported_fixed_and_reply={}",
        nexo_core::message::estimate_tokens(&req.messages) + old_opaque,
        old_opaque,
        nexo_core::message::estimate_tokens(&req.messages) + old_opaque + 4_799 + 13_908 + 256
    );
    let body = nexo_providers::openai_responses::OpenAiResponses::body(&cfg, &req);
    println!(
        "WIRE bytes={} input_bytes={} reasoning_items={}",
        serde_json::to_vec(&body).unwrap().len(),
        serde_json::to_vec(&body["input"]).unwrap().len(),
        body["input"].as_array().unwrap().iter().filter(|p| p["type"] == "reasoning").count()
    );
    for (i, m) in req.messages.iter().enumerate() {
        let parts: Vec<_> = m
            .content
            .iter()
            .map(|p| {
                let v = serde_json::to_value(p).unwrap();
                (v["type"].as_str().unwrap().to_string(), serde_json::to_vec(p).unwrap().len())
            })
            .collect();
        println!(
            "MESSAGE {} {:?} generic={} opaque={} parts={:?}",
            i + 1,
            m.role,
            nexo_core::message::message_tokens(m),
            nexo_core::message::reasoning_tokens(m),
            parts
        );
    }
    let result = super::window_fit::fit(&mut st, &mut req, None, 0).await;
    println!(
        "FIT result={result:?} est_sent={} system={} tools={} conversation={} reply={}",
        st.est_sent,
        req.system.len() / 4,
        serde_json::to_vec(&req.tools).unwrap().len() / 4,
        st.est_sent.saturating_sub(start::fixed_tokens(&req.system, &req.tools)),
        req.max_tokens
    );
    std::env::remove_var("NEXO_DATA_DIR");
}

#[tokio::test]
async fn curator_preserves_person_frontier_after_results_for_admission_and_wire() {
    let opaque = Part::Reasoning {
        text: "summary".into(),
        signature: None,
        encrypted: Some("E".repeat(300_000)),
        model: Some("m".into()),
    };
    let history = vec![
        Message::user("Inspect the report."),
        Message::new(Role::Assistant, vec![opaque, Part::tool_call("c", "read", serde_json::json!({}))]),
        Message::tool_results(vec![Part::tool_result("c", "report", false)]),
        Message::user("Continue with the report."),
    ];
    let mut curator = context::BuiltIn::lenient("relations=1");
    let view = curator.view(0, &history).await.unwrap();
    assert_eq!(nexo_core::message::latest_person(&view.messages), Some(view.messages.len() - 1));
    let req = ModelRequest::new("m", "", view.messages);
    let cfg = ProviderConfig::new("openai-responses", "http://offline.invalid", None);
    let body = nexo_providers::openai_responses::OpenAiResponses::body(&cfg, &req);
    assert!(!serde_json::to_string(&body).unwrap().contains("encrypted_content"));
    assert!(nexo_core::message::estimate_tokens(&req.messages) < 1000);
}
