use super::*;

/// A reply that stops at max_tokens: `events` with the finish said as Length.
fn cut(mut events: Vec<StreamEvent>) -> Vec<StreamEvent> {
    for e in &mut events {
        if let StreamEvent::Finish(f) = e {
            *f = FinishReason::Length;
        }
    }
    events
}

#[tokio::test]
async fn a_call_cut_by_max_tokens_does_not_run() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("f"), "x").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(call("c2", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    // the cut call never ran; the whole one did
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "c1")));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "c2")));
    assert!(
        notices(&evs).iter().any(|n| n.contains("cut at max_tokens while writing its calls")),
        "{:?}",
        notices(&evs)
    );
    // the model was told, as the result of its call
    let reqs = client.requests();
    let told = reqs[1].messages.last().unwrap();
    assert!(matches!(&told.content[0], Part::ToolResult { call_id, is_error: true, content, .. }
        if call_id == "c1" && nexo_core::message::result_text(content).contains("cut at max_tokens")));
    r.stop().await;
}

#[tokio::test]
async fn the_request_after_a_cut_call_leaves_it_out_and_is_sampled_warmer_once() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("f"), "x").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(call("c2", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.temperature = Some(0.0);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    let temps: Vec<_> = client.requests().iter().map(|q| q.temperature).collect();
    assert_eq!(temps, vec![Some(0.0), Some(0.6), Some(0.0)]);
    // the half-written call is not shown again: only that it was cut
    let reqs = client.requests();
    let sent = reqs[1].messages.iter().flat_map(|m| m.tool_calls()).find(|(id, _, _)| *id == "c1").unwrap();
    assert_eq!(sent.2, &json!({"left_out": "cut at max_tokens after 12 characters"}));
    r.stop().await;
}

#[tokio::test]
async fn two_replies_cut_in_a_row_stop_the_request() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(cut(call("c1", "read", json!({"path": "f"})))),
        Reply::Events(cut(call("c2", "read", json!({"path": "f"})))),
        Reply::Events(text("never")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read f".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(notices(&evs).iter().any(|n| n.contains("two replies in a row")), "{:?}", notices(&evs));
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn a_text_cut_by_max_tokens_is_said() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(cut(text("half an ans")))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "explain".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(notices(&evs).iter().any(|n| n.contains("cut at max_tokens")), "{:?}", notices(&evs));
    r.stop().await;
}
