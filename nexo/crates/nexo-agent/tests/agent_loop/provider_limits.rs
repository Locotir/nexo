use super::*;

#[tokio::test]
async fn a_max_output_error_caps_the_reply_and_retries_once() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Err(ProviderError::new(
            ErrorKind::BadRequest,
            "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
        )),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "fits now");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ModelLimits { max_output: Some(8192), source, .. } if source == "error")));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!((reqs[0].max_tokens, reqs[1].max_tokens), (16_384, 8_192));
    assert_eq!(user_config().providers["mock"].max_output["m1"], 8_192);
    r.stop().await;
}

#[tokio::test]
async fn a_context_error_with_a_number_records_the_window() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 131072 tokens. However, you requested 140000 tokens.",
        )),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ModelLimits { context_window: Some(131_072), source, .. } if source == "error")
    ));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("context window of m1"))));
    assert_eq!(user_config().providers["mock"].context_window["m1"], 131_072);
    r.stop().await;
}

#[tokio::test]
async fn a_window_learned_smaller_gets_a_new_curated_view_before_a_compaction() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let too_long = || {
        Reply::Err(ProviderError::new(
            ErrorKind::ContextLength,
            "This model's maximum context length is 65536 tokens. However, you requested 70000 tokens.",
        ))
    };
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        too_long(),
        Reply::Events(text("after")),
        // the same window again: a new view would not help, the history is compacted
        too_long(),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("third")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "after");
    assert!(!notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(!is_summary(&reqs[2]) && reqs[2].messages == reqs[1].messages, "the history kept as it is");
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "third");
    assert!(notices(&evs).iter().any(|n| n.starts_with("history compacted")), "{evs:?}");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 6);
    assert!(is_summary(&reqs[4]));
    r.stop().await;
}

#[tokio::test]
async fn each_curator_call_reports_what_it_added_right_after_its_map_and_the_plain_policy_does_not() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.context.policy = "curator".into();
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    let at = evs.iter().position(|e| matches!(e, AgentEvent::ContextMap { .. })).expect("a map");
    // a short conversation adds nothing, and the event still says so
    assert!(matches!(&evs[at + 1], AgentEvent::Curated { items } if items.is_empty()), "{evs:?}");
    r.stop().await;

    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Curated { .. })), "{evs:?}");
    r.stop().await;
}

#[tokio::test]
async fn an_unknown_model_is_reported_with_the_closest_names() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Err(
        ProviderError::new(ErrorKind::BadRequest, "The model `m3` does not exist or you do not have access to it.")
            .with_status(404),
    )]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetModel { provider: None, model: "m3".into() });
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("does not know the model `m3`") && text.contains("m1"))),
        "{evs:?}"
    );
    assert_eq!(client.requests().len(), 1, "no retry");
    r.stop().await;
}

#[tokio::test]
async fn a_failed_request_comes_as_a_notice_with_a_problem_that_says_what_to_do() {
    for (status, body, title, step) in [
        (
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-proj-a1B2c3D4e5F6g7H8i9J0k1L2m3N4"}}"#,
            "rejected the key",
            "nexo auth login",
        ),
        (402, r#"{"error":{"message":"Insufficient Balance"}}"#, "has no credit left", "billing page"),
    ] {
        let d = test_tmpdir().unwrap();
        let client = Scripted::new(vec![Reply::Err(ProviderError::from_http(status, body))]);
        let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(end_of(&evs), TurnEnd::Error);
        let (text, p) = evs
            .iter()
            .find_map(|e| match e {
                AgentEvent::Notice { level: Level::Error, text, problem: Some(p) } => Some((text.clone(), p.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{status}: an error notice with a problem: {evs:?}"));
        assert!(p.title.contains(title), "{p:?}");
        assert!(p.steps[0].contains(step), "{p:?}");
        assert!(p.detail.as_deref().is_some_and(|d| d.contains(&status.to_string())), "{p:?}");
        assert_eq!(text, p.plain(), "the text stays the full plain form for readers that know no card");
        assert!(!text.contains("a1B2c3D4"), "no key in the text: {text}");
        r.stop().await;
    }
}

// ------------------------------------------------------------ goals and todo

#[tokio::test]
async fn a_model_changed_during_a_request_does_not_change_who_is_billed() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let tx = r.handle.commands.clone();
    client.replies.lock().unwrap().push_back(Reply::Midway(
        tx,
        Box::new(AgentCommand::SetModel { provider: None, model: "m2".into() }),
        text("answer from m1"),
    ));
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let cost =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { cost_usd, .. } = e { Some(*cost_usd) } else { None });
    assert!(cost.is_some_and(|c| c > 0.0), "m1 has a price: {cost:?}");
    r.stop().await;
}

#[tokio::test]
async fn the_usage_of_a_failed_attempt_is_still_billed() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(vec![
            StreamEvent::Usage(Usage { input: 5000, ..Default::default() }),
            StreamEvent::Error(ProviderError::new(
                ErrorKind::BadRequest,
                "max_tokens: 65536 > 8192, which is the maximum allowed number of output tokens for m1",
            )),
        ]),
        Reply::Events(text("fits now")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    let session =
        evs.iter().rev().find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None });
    assert_eq!(session.map(|u| u.input), Some(6000), "the failed attempt's 5000 count too");
    r.stop().await;
}

#[tokio::test]
async fn a_large_window_gets_the_largest_reply_from_the_first_request() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    let p = cfg.providers.get_mut("mock").unwrap();
    p.context_window.insert("m1".into(), 1_048_576);
    p.max_output.insert("m1".into(), 32_768);
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests()[0].max_tokens, 32_768);
    r.stop().await;
}
