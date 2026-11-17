use super::*;
use nexo_core::{Effort, ReasoningConfig};

/// The `mock` model lists `levels`; the person's session level is `xhigh` where the list has it.
fn listing(mut cfg: Config, levels: &[&str]) -> Config {
    let levels = levels.iter().map(|l| l.to_string()).collect();
    cfg.providers.get_mut("mock").unwrap().efforts.insert("m1".into(), levels);
    cfg
}

fn is_summary(r: &ModelRequest) -> bool {
    r.messages.first().is_some_and(|m| m.text().contains("Transcript of the session"))
}

#[tokio::test]
async fn the_summary_asks_for_the_lowest_level_the_model_lists_not_off() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first answer")),
        Reply::Events(text("second answer")),
        Reply::Err(ProviderError::new(ErrorKind::ContextLength, "too long")),
        Reply::Events(text("SUMMARY OF ALL")),
        Reply::Events(text("third answer")),
    ]);
    // no `off` in the list: the lowest level it has
    let cfg = listing(config(PermissionMode::Ask), &["medium", "high", "xhigh"]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetEffort { effort: Some("xhigh".into()) });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    for t in ["one", "two", "three"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        r.turn().await;
    }
    let reqs = client.requests();
    let sum = reqs.iter().find(|q| is_summary(q)).expect("a summary request");
    assert_eq!(sum.reasoning, Some(ReasoningConfig::Effort(Effort::Medium)));
    assert!(reqs[0].reasoning == Some(ReasoningConfig::Effort(Effort::Xhigh)), "the turn keeps the session level");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_job_review_asks_for_the_lowest_level_the_model_lists() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "echo waiting; sleep 30", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("STOP")),
        Reply::Events(text("it hung, so it was stopped")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = listing(config(PermissionMode::Auto), &["low", "medium", "high"]);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::SetEffort { effort: Some("high".into()) });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::Submit { text: "run it in the background".into(), attachments: vec![] });
    r.turn().await;
    r.turn().await;
    let reqs = client.requests();
    let review = reqs.iter().find(|q| q.system.contains("CONTINUE or STOP")).expect("a review request");
    assert_eq!(review.reasoning, Some(ReasoningConfig::Effort(Effort::Low)));
    r.stop().await;
}

#[tokio::test]
async fn the_reader_asks_for_the_lowest_level_the_model_lists() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("Finding: a. Conclusion: unknown")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = listing(config(PermissionMode::Ask), &["minimal", "low", "high"]);
    cfg.context.reader_at = 1;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::SetEffort { effort: Some("high".into()) });
    r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    let read = reqs.iter().find(|q| q.system.starts_with("You read material")).expect("a reader request");
    assert_eq!(read.reasoning, Some(ReasoningConfig::Effort(Effort::Minimal)));
    r.stop().await;
}

#[tokio::test]
async fn off_on_a_model_that_cannot_turn_thinking_off_is_sent_as_low_with_one_notice() {
    let _g = config_lock().lock().await;
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]);
    // what the Anthropic provider lists for Opus 5.5: no `off`
    let mut cfg = listing(config(PermissionMode::Ask), &["low", "medium", "high", "xhigh", "max"]);
    cfg.providers.get_mut("mock").unwrap().effort.insert("m1".into(), "off".into());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    let clamps = |evs: &[AgentEvent]| {
        evs.iter()
            .filter(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("does not take effort `off`")))
            .count()
    };
    // the start says it once, and shows the level really used
    let evs = r.until(|e| matches!(e, AgentEvent::Effort { .. })).await;
    assert_eq!(clamps(&evs), 1, "{evs:?}");
    assert!(matches!(evs.last(), Some(AgentEvent::Effort { current: Some(c), .. }) if c == "low"), "{evs:?}");
    for t in ["x", "y"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        let evs = r.turn().await;
        assert_eq!(clamps(&evs), 0, "the notice is not repeated on every turn");
    }
    for q in client.requests() {
        assert_eq!(q.reasoning, Some(ReasoningConfig::Effort(Effort::Low)));
    }
    r.stop().await;
}
