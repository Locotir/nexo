use super::*;

#[tokio::test]
async fn a_text_reply_is_streamed_recorded_and_priced() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let (_, outs, ran) = bash_calls(&["cargo test a", "git status"], 1000).await;
    // Whether they overlapped, not how long the whole took: a loaded machine (a full gate runs hundreds of tests at
    // once) can stretch the wall time past two sleeps even when both ran together.
    assert_eq!(ran.len(), 2, "{ran:?}");
    let (a, b) = (&ran[0], &ran[1]);
    assert!(a.1 < b.2 && b.1 < a.2, "the two commands ran one after the other: {ran:?}");
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
    let d = test_tmpdir().unwrap();
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

#[tokio::test]
async fn a_write_asks_and_a_denial_reaches_the_model() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
async fn a_call_written_as_text_runs_as_a_call() {
    let d = test_tmpdir().unwrap();
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
async fn a_session_that_ended_between_a_call_and_its_result_goes_on() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
