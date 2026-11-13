use super::*;

#[tokio::test]
async fn recall_brings_back_a_message_exactly_and_only_the_curator_offers_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c2", "recall", json!({"message": "m00003"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    // the tool goes with the directives (an instruction the model gets only with the curator)
    cfg.context.curator_spec = "directives=1".into();
    let set = tools().with(Arc::new(nexo_tools::recall::Recall));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "read a".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert!(reqs[0].tools.iter().any(|t| t.name == "recall"), "the curator with directives offers recall");
    let last = reqs[2].messages.last().unwrap();
    match &last.content[0] {
        Part::ToolResult { call_id, content, is_error, .. } => {
            assert_eq!(call_id, "c2");
            let got = nexo_core::message::result_text(content);
            assert_eq!(got, "m00003 (the output of read), as it was:\nread:a.rs", "{got}");
            assert!(!is_error);
        }
        _ => panic!(),
    }
    r.stop().await;
}

#[tokio::test]
async fn the_curator_offers_recall_and_recall_0_offers_the_tools_of_the_other_policies() {
    for (spec, offered) in [("", true), ("recall=0", false)] {
        let d = test_tmpdir().unwrap();
        let client = Scripted::new(vec![Reply::Events(text("done"))]);
        let mut cfg = config(PermissionMode::Ask);
        cfg.context.policy = "curator".into();
        cfg.context.curator_spec = spec.into();
        let set = tools().with(Arc::new(nexo_tools::recall::Recall));
        let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "hi".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Done);
        let reqs = client.requests();
        assert_eq!(reqs[0].tools.iter().any(|t| t.name == "recall"), offered, "spec {spec:?}");
        r.stop().await;
    }
}
