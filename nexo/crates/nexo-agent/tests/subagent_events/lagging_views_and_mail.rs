use super::*;

#[tokio::test]
async fn a_view_that_falls_behind_is_sent_the_state_of_every_run_and_the_forwarder_goes_on() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "burst"))),
            Reply::Events(text("started it")),
            Reply::Events(text("noted it")),
            Reply::Events(text("noted again")),
        ],
        // far more events than the channel holds, written without a pause
        vec![Reply::Events(many_deltas(9000))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r
        .until_all(&[&|e| matches!(e, AgentEvent::SubState { info } if info.state == SubState::Ok), &|e| {
            matches!(e, AgentEvent::Sub { item: SubItem::Text { .. }, .. })
        }])
        .await;
    let at = |f: &dyn Fn(&AgentEvent) -> bool| evs.iter().position(f);
    let task_at = at(&|e| matches!(e, AgentEvent::Sub { item: SubItem::Task { .. }, .. })).unwrap();
    let first = at(&|e| matches!(e, AgentEvent::SubDelta { .. })).expect("the newest pieces of text arrived");
    let deltas = evs.iter().filter(|e| matches!(e, AgentEvent::SubDelta { .. })).count();
    assert!(deltas < 9000, "the channel did not overflow, so nothing was tested ({deltas} deltas)");
    // a state event after the task and before the text: the run began before them, so only the catch-up
    // sent when the lag was seen can be there
    assert!(evs[task_at..first].iter().any(is_sub_state), "no state event after the lag");
    // the forwarder goes on: an answer made by the registry still reaches the view
    r.send(AgentCommand::SubSubmit { id: "nobody#9".into(), text: "hello".into() });
    r.until(|e| matches!(e, AgentEvent::SubNotDelivered { .. })).await;
    r.stop().await;
}

#[tokio::test]
async fn mail_that_never_stops_ends_at_the_rounds_of_the_profile() {
    let d = test_tmpdir().unwrap();
    // the explore profile has 16 rounds: 17 requests at most, whatever arrives
    let client = Scripted::new(vec![], slow_replies(d.path(), 40));
    let (agent, ctx) = (Arc::new(task_tool(&client)), call_ctx(d.path(), "t1"));
    let subs = ctx.jobs.subs();
    let run = {
        let ctx = ctx.clone();
        tokio::spawn(async move { agent.run(task("explore", "answer"), &ctx).await })
    };
    tokio::time::timeout(PATIENCE, feed_mail_until_done(&client, &subs, d.path(), &run)).await.expect("it ends");
    let out = run.await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(client.sub_requests().len(), 17);
    assert!(subs.list()[0].state == SubState::Ok);
}

#[tokio::test]
async fn mail_that_never_stops_ends_when_the_person_cancels() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![], slow_replies(d.path(), 40));
    let (agent, ctx) = (Arc::new(task_tool(&client)), call_ctx(d.path(), "t1"));
    let subs = ctx.jobs.subs();
    let run = {
        let ctx = ctx.clone();
        tokio::spawn(async move { agent.run(task("explore", "answer"), &ctx).await })
    };
    let mut seen = 0;
    while seen < 3 {
        if client.sub_count() > seen {
            seen += 1;
            subs.submit("t1#1", "and another thing").unwrap();
            release(d.path(), &format!("r{}", seen - 1));
        } else {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    // the fourth reply is on its way when the person stops it
    until_sub(&client, 4).await;
    subs.submit("t1#1", "and another thing").unwrap();
    ctx.cancel.cancel();
    release(d.path(), "r3");
    let out = tokio::time::timeout(PATIENCE, run).await.expect("it ends").unwrap();
    assert!(out.is_error, "{}", out.content);
    assert_eq!(client.sub_requests().len(), 4, "no round after the cancel");
    assert_eq!(subs.list()[0].state, SubState::Killed);
}

#[tokio::test]
async fn a_view_can_ask_for_the_conversation_of_a_run() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "slow look"))),
            Reply::Events(text("started it")),
            Reply::Events(text("noted the report")),
        ],
        vec![
            Reply::Events(says_then_calls(&["ASKED-THOUGHT-5"], "w1", "wait", spends(10, 5))),
            Reply::Events(text_spending("late report", spends(20, 7))),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    let id = held_sub_agent(&mut r, &client).await;
    r.send(AgentCommand::SubTranscript { id: id.clone() });
    r.send(AgentCommand::SubTranscript { id: "nobody#9".into() });
    let evs = r.until_all(&[&|e| matches!(e, AgentEvent::SubTranscript { id, .. } if id == "nobody#9")]).await;
    let answers: Vec<&AgentEvent> = evs.iter().filter(|e| matches!(e, AgentEvent::SubTranscript { .. })).collect();
    assert_eq!(answers.len(), 2, "one answer for each question: {answers:?}");
    let AgentEvent::SubTranscript { id: got, transcript: Some(t) } = answers[0] else { panic!("{answers:?}") };
    assert_eq!(got, &id);
    assert_eq!(t.dropped, 0);
    assert!(matches!(&t.items[0], SubItem::Task { text } if text == "slow look"), "{:?}", t.items);
    assert!(t.items.iter().any(|i| matches!(i, SubItem::Text { text } if text == "ASKED-THOUGHT-5")), "{:?}", t.items);
    assert!(matches!(answers[1], AgentEvent::SubTranscript { transcript: None, .. }));
    // it is the registry's own answer: the same as reading it
    assert!(evs.iter().all(|e| e.is_sub() || !matches!(e, AgentEvent::Notice { .. })));
    release(d.path(), "go");
    r.turn().await;
    // the event is for the interface only: not in the session file
    r.stop().await;
    let mut files = Vec::new();
    let mut stack = vec![PathBuf::from(std::env::var_os("NEXO_DATA_DIR").unwrap())];
    while let Some(p) = stack.pop() {
        match std::fs::read_dir(&p) {
            Ok(rd) => stack.extend(rd.flatten().map(|e| e.path())),
            Err(_) => files.push(p),
        }
    }
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        assert!(!text.contains("ASKED-THOUGHT-5"), "{}", f.display());
    }
}

#[tokio::test]
async fn a_rewind_announces_the_live_runs_again() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", bg("explore", "slow look"))), Reply::Events(text("started it"))],
        vec![Reply::Events(call("w1", "wait", json!({"file": "go"}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    let id = held_sub_agent(&mut r, &client).await;
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::SubState { info } if info.id == id)).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::HistoryCleared)), "the state comes after the clear: {evs:?}");
    release(d.path(), "go");
    r.stop().await;
}
