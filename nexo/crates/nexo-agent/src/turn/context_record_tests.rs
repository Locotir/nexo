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
