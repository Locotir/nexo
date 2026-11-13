use super::*;

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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
            AgentEvent::Notice { text, level, .. } if text.contains("not available") => Some((text.clone(), *level)),
            _ => None,
        })
        .expect("one plain line");
    assert_eq!(line.0, "this session last used mock/m2, which is not available now; staying on mock/m1");
    assert!(matches!(line.1, Level::Info), "no error");
    r.stop().await;
}

#[tokio::test]
async fn a_provider_that_is_no_longer_configured_falls_back_the_same_way() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
