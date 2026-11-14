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
