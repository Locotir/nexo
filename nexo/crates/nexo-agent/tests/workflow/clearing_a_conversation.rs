use super::*;

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
    let d = test_tmpdir().unwrap();
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
