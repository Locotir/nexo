use super::*;

#[tokio::test]
async fn a_message_sent_to_a_running_sub_agent_reaches_only_that_sub_agent() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "slow look"))),
            Reply::Events(text("started it")),
            Reply::Events(text("noted the report")),
        ],
        vec![
            Reply::Events(says_then_calls(&["SUB-THOUGHT-77"], "w1", "wait", spends(10, 5))),
            Reply::Events(text_spending("late report", spends(20, 7))),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    let id = held_sub_agent(&mut r, &client).await;
    r.send(AgentCommand::SubSubmit { id: id.clone(), text: "SECRET-NUDGE-42 check b too".into() });
    r.sync().await;
    release(d.path(), "go");
    // it ends while the agent is idle: its note starts a turn
    let evs = r.turn().await;
    let subs: Vec<&AgentEvent> = evs.iter().filter(|e| e.is_sub()).collect();
    assert!(
        subs.iter().any(|e| matches!(e, AgentEvent::Sub { id: i, item: SubItem::Person { text } } if *i == id && text.contains("SECRET-NUDGE-42"))),
        "{subs:?}"
    );
    // the sub-agent's next request holds the message as a user turn, after the tool result
    let reqs = requests_of(&client, "slow look");
    assert_eq!(reqs.len(), 2);
    let last = reqs[1].messages.last().unwrap();
    assert_eq!(last.role, nexo_core::message::Role::User);
    assert!(texts(last).contains("SECRET-NUDGE-42 check b too"), "{last:?}");
    // the main agent never saw it: not in any request, not in the end note
    for (i, req) in client.main_requests().iter().enumerate() {
        let all = serde_json::to_string(req).unwrap();
        assert!(!all.contains("SECRET-NUDGE-42") && !all.contains("SUB-THOUGHT-77"), "main request {i}");
    }
    assert_eq!(client.main_requests().len(), 3);
    // the ended state carries what the run spent
    let ended = evs
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::SubState { info } if info.state == SubState::Ok => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!((ended.tokens_in, ended.tokens_out), (30, 12));
    r.stop().await;
}

#[tokio::test]
async fn the_main_agent_s_message_to_its_running_sub_agent_joins_it_before_its_next_step_as_the_parent_s() {
    let d = test_tmpdir().unwrap();
    let home = test_tmpdir().unwrap();
    let mail = nexo_core::mailbox::Mailbox::open(
        &home.path().join("mail"),
        "aaaa1111",
        "api",
        "/w/api",
        false,
        nexo_core::mail::Inbound::Auto,
    )
    .unwrap();
    let note = json!({"to": "1", "text": "PARENT-NOTE-5 also check b.rs"});
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "slow look"))),
            late(d.path(), "now", call("m1", "send_message", note)),
            Reply::Events(text("told it")),
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start_mail(client.clone(), config(PermissionMode::Auto), d.path(), Some(mail.clone())).await;
    r.submit("go");
    // the sub-agent is inside its first step when the main agent writes to it
    until_tool(&client, 1).await;
    release(d.path(), "now");
    r.turn().await;
    let told = serde_json::to_string(&client.main_requests()[2]).unwrap();
    let sent = "Sent to sub-agent job 1 \\\"slow look\\\" (it reads it before its next step)";
    assert!(told.contains(sent), "{told}");
    // the person's screen shows it as a message sent to `job 1`
    let shown = mail.drain_events();
    assert!(
        shown.iter().any(|e| matches!(e, AgentEvent::MailSent { to_name, summary, .. }
            if to_name == "job 1" && summary == "PARENT-NOTE-5 also check b.rs")),
        "{shown:?}"
    );
    release(d.path(), "go");
    let evs = r.turn().await;
    assert!(
        evs.iter().any(
            |e| matches!(e, AgentEvent::Sub { item: SubItem::Parent { text }, .. } if text.starts_with("PARENT-NOTE-5"))
        ),
        "{evs:?}"
    );
    // its next request holds the message after the tool result, marked as the parent's
    let reqs = requests_of(&client, "slow look");
    assert_eq!(reqs.len(), 2);
    assert!(!serde_json::to_string(&reqs[0]).unwrap().contains("PARENT-NOTE-5"));
    let last = reqs[1].messages.last().unwrap();
    assert_eq!(last.role, nexo_core::message::Role::User);
    assert!(
        texts(last).contains(
            "[nexo: a message from the agent that started you, not from the person]\nPARENT-NOTE-5 also check b.rs"
        ),
        "{last:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_message_to_an_ended_or_unknown_sub_agent_says_it_was_not_delivered_and_queues_nothing() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "slow look"))),
            Reply::Events(text("started it")),
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    let id = held_sub_agent(&mut r, &client).await;
    release(d.path(), "go");
    r.turn().await;
    let sub_requests = client.sub_count();
    r.send(AgentCommand::SubSubmit { id: id.clone(), text: "are you there".into() });
    let evs = r.until(|e| matches!(e, AgentEvent::SubNotDelivered { .. })).await;
    assert!(
        matches!(evs.last(), Some(AgentEvent::SubNotDelivered { id: i, why, text }) if *i == id && why.contains("ended") && text == "are you there"),
        "{evs:?}"
    );
    r.send(AgentCommand::SubSubmit { id: "nobody#9".into(), text: "hello".into() });
    let evs = r.until(|e| matches!(e, AgentEvent::SubNotDelivered { .. })).await;
    assert!(
        matches!(evs.last(), Some(AgentEvent::SubNotDelivered { id, why, text }) if id == "nobody#9" && why.contains("no sub-agent") && text == "hello")
    );
    // nothing woke anything: no new request, no new turn
    assert!(r.quiet(300, |e| matches!(e, AgentEvent::TurnStart { .. } | AgentEvent::Sub { .. })).await);
    assert_eq!(client.sub_count(), sub_requests);
    assert_eq!(client.main_requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn sub_agent_events_are_not_written_to_the_session_and_what_a_run_spends_is_in_the_session_total() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "slow look"))),
            Reply::Events(text("started it")),
            Reply::Events(text("NOTED-REPORT-ONLY-IN-THE-SESSION-TEST")),
        ],
        vec![
            Reply::Events(says_then_calls(&["SUB-THOUGHT-77"], "w1", "wait", spends(100, 20))),
            Reply::Events(text_spending("late report", spends(100, 20))),
        ],
    );
    let mut cfg = config(PermissionMode::Auto);
    cfg.providers.get_mut("mock").unwrap().pricing.insert(
        "m1".into(),
        nexo_core::pricing::Pricing {
            input: 1_000_000.0,
            output: 2_000_000.0,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: 0.0,
        },
    );
    let mut r = start(client.clone(), cfg, d.path()).await;
    let id = held_sub_agent(&mut r, &client).await;
    r.send(AgentCommand::SubSubmit { id: id.clone(), text: "SECRET-NUDGE-42".into() });
    r.sync().await;
    r.send(AgentCommand::SubTranscript { id: id.clone(), request: 1 });
    r.until(|e| matches!(e, AgentEvent::SubTranscript { transcript: Some(_), .. })).await;
    release(d.path(), "go");
    let evs = r.turn().await;
    // two calls of 100 in and 20 out, at 1 and 2 dollars a token
    let ended = evs
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::SubState { info } if info.state == SubState::Ok => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ended.cost_usd, 280.0);
    let session_cost = evs
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Usage { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .unwrap();
    assert_eq!(session_cost, ended.cost_usd, "the run's figure is part of the total, not added to it");
    r.stop().await;
    // the session file holds the conversation of the main agent, none of the sub-agent's
    let (mut files, mut stack) = (Vec::new(), vec![PathBuf::from(std::env::var_os("NEXO_DATA_DIR").unwrap())]);
    while let Some(p) = stack.pop() {
        match std::fs::read_dir(&p) {
            Ok(rd) => stack.extend(rd.flatten().map(|e| e.path())),
            Err(_) => files.push(p),
        }
    }
    let mut seen_report = false;
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        assert!(!text.contains("SECRET-NUDGE-42") && !text.contains("SUB-THOUGHT-77"), "{}", f.display());
        seen_report |= text.contains("NOTED-REPORT-ONLY-IN-THE-SESSION-TEST");
    }
    assert!(seen_report, "the session file was found and holds the marker only this test writes");
}
