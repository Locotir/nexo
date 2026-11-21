//! Real model tool calls through the session runtime, with a scripted provider.
use super::*;

fn done<'a>(events: &'a [AgentEvent], id: &str) -> &'a ToolOutput {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolDone { id: i, output, .. } if i == id => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing {id}: {events:?}"))
}

#[tokio::test]
async fn model_creates_lists_and_cancels_loop_in_auto() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("create", "loop", json!({"action":"create", "interval":"1m", "prompt":"check"}))),
        Reply::Events(call("list", "loop", json!({"action":"list"}))),
        Reply::Events(call("cancel", "loop", json!({"action":"cancel", "id":1}))),
        Reply::Events(call("empty", "loop", json!({"action":"list"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "manage loops".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    assert!(done(&evs, "create").content.contains("loop #1 scheduled"));
    assert!(done(&evs, "list").content.contains("loop #1 every 60s: check"));
    assert!(done(&evs, "cancel").content.contains("cancelled"));
    assert!(done(&evs, "empty").content.contains("no active loops"));
    assert!(client.requests()[0].tools.iter().any(|t| t.name == "loop"));
    r.stop().await;
}

#[tokio::test]
async fn model_sets_shows_and_clears_goal_in_auto() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("set", "goal", json!({"action":"set", "text":"ship it"}))),
        Reply::Events(call("show", "goal", json!({"action":"show"}))),
        Reply::Events(call("clear", "goal", json!({"action":"clear"}))),
        Reply::Events(call("empty", "goal", json!({"action":"show"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "manage goal".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    assert!(!done(&evs, "set").is_error);
    assert!(done(&evs, "show").content.contains("ship it"));
    assert!(!done(&evs, "clear").is_error);
    assert!(done(&evs, "empty").content.contains("None"));
    assert!(client.requests()[0].tools.iter().any(|t| t.name == "goal"));
    r.stop().await;
}

#[tokio::test]
async fn normal_mode_asks_for_mutation_and_rejection_has_no_effect() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("list", "loop", json!({"action":"list"}))),
        Reply::Events(call("create", "loop", json!({"action":"create", "interval":"1m", "prompt":"check"}))),
        Reply::Events(call("set", "goal", json!({"action":"set", "text":"ship it"}))),
        Reply::Events(call("show", "goal", json!({"action":"show"}))),
        Reply::Events(call("empty", "loop", json!({"action":"list"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "manage".into(), attachments: vec![] });
    let before = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(done(&before, "list").content.contains("no active loops"));
    r.send(AgentCommand::Permission {
        id: "create".into(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    r.send(AgentCommand::Permission {
        id: "set".into(),
        decision: PermissionDecision::Deny { reason: Some("no".into()) },
    });
    let after = r.turn().await;
    assert!(done(&after, "show").content.contains("None"));
    assert!(done(&after, "empty").content.contains("no active loops"));
    assert!(!after.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    r.stop().await;
}

#[tokio::test]
async fn model_created_loop_disappears_on_clear_during_turn() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("create", "loop", json!({"action":"create", "interval":"1m", "prompt":"check"}))),
        Reply::Hang(vec![StreamEvent::TextDelta("holding".into())]),
        Reply::Events(call("list", "loop", json!({"action":"list"}))),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client, config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "create".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::TextDelta { text } if text == "holding")).await;
    assert!(done(&evs, "create").content.contains("scheduled"));
    r.send(AgentCommand::Clear);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { reason: TurnEnd::Interrupted })).await;
    r.send(AgentCommand::Submit { text: "list".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(done(&evs, "list").content.contains("no active loops"));
    r.stop().await;
}

#[tokio::test]
async fn model_goal_set_starts_its_own_turn_after_current_turn() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("set", "goal", json!({"action":"set", "text":"ship it"}))),
        Reply::Events(text("setting goal")),
        Reply::Events(text("GOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "set goal".into(), attachments: vec![] });
    let first = r.turn().await;
    assert!(!done(&first, "set").is_error);
    let second = r.turn().await;
    assert!(second
        .iter()
        .any(|e| matches!(e, AgentEvent::History { message, .. } if message.text().contains("[goal set] ship it"))));
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn model_loop_prompt_is_not_presented_as_person_words() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "create",
            "loop",
            json!({"action":"create", "interval":"1m", "prompt":"model generated work"}),
        )),
        Reply::Events(text("created")),
        Reply::Events(text("loop finished")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, d.path()).await;
    r.send(AgentCommand::Submit { text: "create a loop".into(), attachments: vec![] });
    r.turn().await;
    let events = r.turn().await;
    let msg = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::History { message, .. } if message.text().contains("model generated work") => {
                Some(message.text())
            }
            _ => None,
        })
        .expect("scheduled history");
    assert!(!msg.contains(nexo_core::agent::PERSON_MARK), "{msg}");
    r.stop().await;
}
