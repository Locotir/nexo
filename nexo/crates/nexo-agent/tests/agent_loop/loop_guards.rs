use super::*;

#[tokio::test]
async fn max_turns_stops_a_loop_of_calls() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
        last.contains("until you answer")
            && last.contains("on again after your answer")
            && !last.contains("from here on"),
        "the stop holds until the answer only, or the model keeps off the tools for the rest of the session"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_call_after_a_guard_stop_does_not_run() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
async fn a_failure_that_comes_back_is_tagged_and_the_loop_guard_still_stops_the_loop() {
    // the guards weigh the outputs as they came: the line, new each time, must not hide the loop from them
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
