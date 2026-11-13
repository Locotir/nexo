use super::*;

#[tokio::test]
async fn attachments_shell_model_and_clear() {
    if nexo_core::shell::shell().kind == "powershell" {
        eprintln!("the test writes POSIX shell; this system runs PowerShell: skipped");
        return;
    }
    let d = test_tmpdir().unwrap();
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
            assert!(t.starts_with("@note.md (attached in full, as it is now):\n```md\nremember this\n```"), "{t}");
            assert!(t.ends_with(&format!("```\n\n{}\nsee", nexo_core::agent::PERSON_MARK)), "{t}");
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

#[tokio::test]
async fn export_writes_the_conversation_as_markdown() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
async fn a_message_sent_during_a_turn_joins_it_before_the_next_call() {
    let d = test_tmpdir().unwrap();
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
async fn another_sessions_memory_goes_with_the_next_request_and_sessions_go_to_the_trash() {
    let d = test_tmpdir().unwrap();
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
    assert_eq!(sent, format!("{}\n\n{}\nfix it", told[0].text, nexo_core::agent::PERSON_MARK));
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
async fn a_suggestion_repeats_the_last_request_with_one_more_message() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
async fn a_cleared_conversation_does_not_count_the_failures_of_the_last_one() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
