use super::*;

#[tokio::test]
async fn plan_mode_refuses_writes_until_the_plan_is_approved() {
    let d = test_tmpdir().unwrap();
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
    let d = test_tmpdir().unwrap();
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
async fn add_dir_lets_a_write_there_pass_without_asking_and_tells_the_model() {
    let d = test_tmpdir().unwrap();
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
