use super::*;

async fn run_plan(approve: bool) {
    let d = test_tmpdir().unwrap();
    let mut replies = vec![Reply::Events(call(
        "plan1",
        "ultracode_execute",
        json!({
            "steps": [{"agent":"explore", "prompt":"find the exact parser", "provider":"mock", "model":"m2", "start_clean":true}]
        }),
    ))];
    if approve {
        replies.push(Reply::Events(text("parser report from subagent")));
    }
    replies.push(Reply::Events(text("main finished")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Ultracode { text: "find parser".into(), attachments: vec![] });
    let before = r.until(|e| matches!(e, AgentEvent::Question { .. } | AgentEvent::TurnEnd { .. })).await;
    let Some(AgentEvent::Question { question }) = before.last() else { panic!("expected approval: {before:?}") };
    let view = question.plan.as_ref().expect("approval view");
    assert!(view.contains("find the exact parser") && view.contains("m2") && view.contains("mock"), "{view}");
    assert_eq!(client.requests().len(), 1, "no subagent before approval");
    r.send(AgentCommand::Answer {
        id: question.id.clone(),
        answer: Some(if approve { "Approve agent plan" } else { "Reject" }.into()),
    });
    let events = r.turn().await;
    let output = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id, output, .. } if id == "plan1" => Some(output),
            _ => None,
        })
        .expect("plan tool result");
    if approve {
        assert!(!output.is_error, "{output:?}");
        assert!(output.content.contains("parser report from subagent"), "{output:?}");
        let reqs = client.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[1].model, "m2");
        assert!(reqs[1].system.contains("sub-agent"));
        assert!(reqs[1].messages.iter().any(|m| text_of_parts(&m.content).contains("find the exact parser")));
    } else {
        assert!(output.is_error && output.content.contains("not approved"), "{output:?}");
        assert_eq!(client.requests().len(), 2, "rejection must not start a subagent");
    }
    r.stop().await;
}

#[tokio::test]
async fn approved_command_executes_the_displayed_plan() {
    run_plan(true).await;
}

#[tokio::test]
async fn rejected_command_starts_no_subagent() {
    run_plan(false).await;
}

#[tokio::test]
async fn plan_mode_approval_transitions_and_executes() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "plan1",
            "ultracode_execute",
            json!({"steps":[{"agent":"explore","prompt":"find parser","provider":"mock","model":"m2","start_clean":true}]}),
        )),
        Reply::Events(text("report")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Plan);
    cfg.permissions.allow.push("task".into());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Ultracode { text: "find parser".into(), attachments: vec![] });
    let events = r.until(|e| matches!(e, AgentEvent::Question { .. } | AgentEvent::TurnEnd { .. })).await;
    let Some(AgentEvent::Question { question }) = events.last() else { panic!("expected approval: {events:?}") };
    assert_eq!(client.requests().len(), 1);
    r.send(AgentCommand::Answer { id: question.id.clone(), answer: Some("Approve agent plan".into()) });
    let events = r.turn().await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::PermissionModeChanged { mode: PermissionMode::Ask })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "plan1" && !output.is_error)),
        "{events:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn changing_mode_during_approval_invalidates_plan() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "plan1",
            "ultracode_execute",
            json!({"steps":[{"agent":"explore","prompt":"find parser","provider":"mock","model":"m2","start_clean":true}]}),
        )),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Plan), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Ultracode { text: "find parser".into(), attachments: vec![] });
    let events = r.until(|e| matches!(e, AgentEvent::Question { .. } | AgentEvent::TurnEnd { .. })).await;
    let Some(AgentEvent::Question { question }) = events.last() else { panic!("expected approval: {events:?}") };
    let id = question.id.clone();
    r.send(AgentCommand::SetPermissionMode(PermissionMode::ReadOnly));
    r.until(|e| matches!(e, AgentEvent::PermissionModeChanged { mode: PermissionMode::ReadOnly })).await;
    r.send(AgentCommand::Answer { id, answer: Some("Approve agent plan".into()) });
    let events = r.turn().await;
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "plan1" && output.is_error)),
        "{events:?}"
    );
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn auto_planning_blocks_unoffered_effects_but_allows_plan() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("write1", "write", json!({"path":"bad", "content":"bad"}))),
        Reply::Events(call(
            "plan1",
            "ultracode_execute",
            json!({"steps":[{"agent":"explore","prompt":"inspect","provider":"mock","model":"m2","start_clean":true}]}),
        )),
        Reply::Events(text("report")),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Ultracode { text: "inspect".into(), attachments: vec![] });
    let events = r.turn().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "write1" && output.is_error)),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "plan1" && !output.is_error)),
        "{events:?}"
    );
    let req = &client.requests()[0];
    assert!(!req.tools.iter().any(|t| t.name == "write" || t.name == "bash"));
    assert!(req.tools.iter().any(|t| t.name == "ultracode_execute"));
    r.stop().await;
}

#[tokio::test]
async fn changing_policy_during_first_step_stops_the_second() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.allow.push("task".into());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    *client.replies.lock().unwrap() = vec![
        Reply::Events(call(
            "plan1",
            "ultracode_execute",
            json!({"steps":[
                {"agent":"explore","prompt":"first","provider":"mock","model":"m2","start_clean":true},
                {"agent":"explore","prompt":"second","provider":"mock","model":"m2","start_clean":true}
            ]}),
        )),
        Reply::Midway(
            r.handle.commands.clone(),
            Box::new(AgentCommand::SetPermissionMode(PermissionMode::Auto)),
            text("first report"),
        ),
        Reply::Events(text("done")),
    ]
    .into();
    r.send(AgentCommand::Ultracode { text: "inspect".into(), attachments: vec![] });
    let events = r.until(|e| matches!(e, AgentEvent::Question { .. } | AgentEvent::TurnEnd { .. })).await;
    let Some(AgentEvent::Question { question }) = events.last() else { panic!("{events:?}") };
    r.send(AgentCommand::Answer { id: question.id.clone(), answer: Some("Approve agent plan".into()) });
    let events = r.turn().await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "plan1" && output.is_error && output.content.contains("authorization changed"))), "{events:?}");
    assert_eq!(client.requests().len(), 3, "second subagent must not start");
    r.stop().await;
}
