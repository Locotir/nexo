use super::*;

#[tokio::test]
async fn a_context_length_error_compacts_once_and_retries() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("first answer")),
        Reply::Events(text("second answer")),
        Reply::Err(ProviderError::new(ErrorKind::ContextLength, "too long")),
        Reply::Events(text("SUMMARY OF ALL")),
        Reply::Events(text("third answer")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    for t in ["one", "two"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        r.turn().await;
    }
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("history compacted"))),
        "{evs:?}"
    );
    assert_eq!(text_of(&evs), "third answer");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(reqs[3].messages[0].text().contains("Transcript of the session"), "the summary call");
    assert_eq!(reqs[4].messages.len(), 1, "summary merged into the request in progress");
    let t = reqs[4].messages[0].text();
    assert!(t.starts_with("[summary]") && t.contains("SUMMARY OF ALL") && t.ends_with("three"), "{t}");
    r.stop().await;
}

fn big_call(id: &str, name: &str, path: &str) -> Vec<StreamEvent> {
    let mut evs = call(id, name, json!({"path": path}));
    evs[2] = StreamEvent::Usage(Usage { input: 5000, output: 5, ..Default::default() });
    evs
}

#[tokio::test]
async fn compact_summarises_past_its_threshold_and_keeps_the_task_and_files() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events({
            // a long explanation with the first call: it is summarised away once a later call keeps the tail
            let mut evs = big_call("c1", "write", "b.rs");
            evs.insert(0, StreamEvent::TextDelta("x".repeat(20_000)));
            evs
        }),
        Reply::Events(big_call("c2", "read", "a.rs")),
        Reply::Events(text("SUMMARY ONE")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "fix the bug in a.rs".into(), attachments: vec![] });
    let evs = r.turn().await;
    let shown = text_of(&evs);
    assert!(shown.ends_with("xdone") && !shown.contains("SUMMARY"), "summaries are not shown as replies");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "one summary: the task alone was too little to summarise");
    assert!(!is_summary(&reqs[0]) && !is_summary(&reqs[1]) && is_summary(&reqs[2]) && !is_summary(&reqs[3]));
    // after the summary: the summary, then the last call with its result, nothing orphaned
    let m = &reqs[3].messages;
    assert_eq!(m.len(), 3, "{m:?}");
    let head = m[0].text();
    assert!(head.contains("[request] fix the bug in a.rs"), "{head}");
    assert!(head.contains("SUMMARY ONE") && head.contains("[files]") && head.contains("b.rs"), "{head}");
    assert!(m[1].content.iter().any(|p| matches!(p, Part::ToolCall { id, .. } if id == "c2")), "{m:?}");
    assert!(m[2].content.iter().any(|p| matches!(p, Part::ToolResult { call_id, .. } if call_id == "c2")));
    let size = |i: usize| serde_json::to_string(&reqs[i].messages).unwrap().len();
    assert!(size(1) > 20_000 && size(3) < 2_000, "{} then {}", size(1), size(3));
    // the summary is billed like any request: 2 calls, 1 summary, 1 answer
    let session = evs
        .iter()
        .rev()
        .find_map(|e| if let AgentEvent::Usage { session, .. } = e { Some(*session) } else { None })
        .unwrap();
    assert_eq!(session.total_input(), 2 * 5000 + 2 * 3000);
    // the summary's request is not the conversation's: it is spent but sized as no call
    let calls: Vec<(u64, u64)> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Usage { turn, call, .. } => Some((turn.total_input(), call.total_input())),
            _ => None,
        })
        .collect();
    assert_eq!(calls, vec![(5000, 5000), (5000, 5000), (3000, 0), (3000, 3000)]);
    r.stop().await;
}

#[tokio::test]
async fn a_retried_attempt_is_spent_but_does_not_count_in_the_size_of_the_request() {
    let d = test_tmpdir().unwrap();
    let mid = |id: &str, path: &str, used: u64| {
        let mut evs = call(id, "read", json!({"path": path}));
        evs[2] = StreamEvent::Usage(Usage { input: used, output: 5, ..Default::default() });
        evs
    };
    let client = Scripted::new(vec![
        Reply::Events({
            let mut evs = mid("c1", "a.rs", 1000);
            evs.insert(0, StreamEvent::TextDelta("x".repeat(20_000)));
            evs
        }),
        // the client retried this call once: the first attempt was billed 2500 and thrown away
        Reply::Events({
            let mut evs = mid("c2", "b.rs", 1000);
            evs.insert(0, StreamEvent::RetriedUsage(Usage { input: 2500, ..Default::default() }));
            evs
        }),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "fix the bug in a.rs".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), format!("{}done", "x".repeat(20_000)));
    let reqs = client.requests();
    assert!(reqs.iter().all(|q| !is_summary(q)), "the last request held 1000 tokens, not 3500: no summary");
    assert_eq!(reqs.len(), 3);
    let usages: Vec<(Usage, Usage)> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::Usage { turn, call, .. } = e { Some((*turn, *call)) } else { None })
        .collect();
    assert_eq!((usages[1].0.input, usages[1].1.input), (3500, 1000), "{usages:?}");
    let (session, cost) = evs
        .iter()
        .rev()
        .find_map(
            |e| if let AgentEvent::Usage { session, cost_usd, .. } = e { Some((*session, *cost_usd)) } else { None },
        )
        .unwrap();
    assert_eq!(session.input, 1000 + 3500 + 1000, "the retried attempt is spent");
    // 5500 in x 1 + 110 out x 2 + 2000 cached x 0.1, per million
    assert!((cost - 0.00592).abs() < 1e-9, "{cost}");
    r.stop().await;
}

#[tokio::test]
async fn compact_does_not_summarise_every_round_a_request_its_fixed_part_keeps_over_the_threshold() {
    let d = test_tmpdir().unwrap();
    // every request counts 5000 tokens (a long system prompt, many tools) while each round adds little
    let client = Scripted::new(vec![
        Reply::Events(big_call("c1", "read", "a.py")),
        Reply::Events(big_call("c2", "read", "b.py")),
        Reply::Events(big_call("c3", "read", "c.py")),
        Reply::Events(big_call("c4", "read", "d.py")),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.context.policy = "compact".into();
    cfg.context.compact_at = 3000;
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look at the scripts".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(!reqs.iter().any(is_summary), "no summary of a summary each round");
    assert_eq!(reqs[4].messages.len(), 9, "the history is as it was");
    r.stop().await;
}
