use super::*;

#[tokio::test]
async fn two_sub_agents_at_once_never_cross_items_or_messages() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![],
        vec![
            (
                "alpha",
                vec![Reply::Events(call("a1", "wait", json!({"file": "go-alpha"}))), Reply::Events(text("alpha done"))],
            ),
            (
                "beta",
                vec![Reply::Events(call("b1", "wait", json!({"file": "go-beta"}))), Reply::Events(text("beta done"))],
            ),
        ],
    );
    let agent = task_tool(&client);
    let (ca, mut cb) = (call_ctx(d.path(), "ta"), call_ctx(d.path(), "tb"));
    cb.jobs = ca.jobs.clone();
    let subs = ca.jobs.subs();
    let mut rx = subs.changes();
    agent.run(bg("explore", "alpha task"), &ca).await;
    agent.run(bg("explore", "beta task"), &cb).await;
    until_tool(&client, 2).await;
    let ids: Vec<String> = subs.list().into_iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), 2);
    let (a, b) = (ids[0].clone(), ids[1].clone());
    assert!(subs.submit(&a, "only for alpha").is_ok());
    release(d.path(), "go-alpha");
    release(d.path(), "go-beta");
    until_state(&mut rx, |i| i.id == a && i.state == SubState::Ok).await;
    until_state(&mut rx, |i| i.id == b && i.state == SubState::Ok).await;
    let (ta, tb) = (subs.transcript(&a).unwrap().items, subs.transcript(&b).unwrap().items);
    let said = |t: &[SubItem], s: &str| {
        t.iter().any(|i| matches!(i, SubItem::Text { text } | SubItem::Person { text } if text == s))
    };
    assert!(said(&ta, "alpha done") && said(&ta, "only for alpha") && !said(&ta, "beta done"));
    assert!(said(&tb, "beta done") && !said(&tb, "only for alpha") && !said(&tb, "alpha done"));
    assert!(ta.iter().all(|i| !matches!(i, SubItem::Call { id, .. } | SubItem::Result { id, .. } if id == "b1")));
    // and the one that was written to read it as a user turn, the other did not see it
    let alpha = requests_of(&client, "alpha task");
    let beta = requests_of(&client, "beta task");
    assert!(texts(alpha.last().unwrap().messages.last().unwrap()).contains("only for alpha"));
    assert!(beta.iter().all(|r| r.messages.iter().all(|m| !texts(m).contains("only for alpha"))));
}

#[tokio::test]
async fn failed_killed_and_interrupted_runs_end_in_the_right_state() {
    let d = test_tmpdir().unwrap();
    // failed: the model answers nothing
    let client = Scripted::new(vec![], vec![Reply::Events(text(""))]);
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "f1"));
    let out = agent.run(task("explore", "say nothing"), &ctx).await;
    assert!(out.is_error);
    assert_eq!(ctx.jobs.subs().list()[0].state, SubState::Failed);

    // killed: a background run is stopped with the job
    let client = Scripted::new(vec![], vec![Reply::Events(call("w1", "wait", json!({})))]);
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "k1"));
    let subs = ctx.jobs.subs();
    let mut rx = subs.changes();
    agent.run(bg("explore", "hold on"), &ctx).await;
    until_tool(&client, 1).await;
    assert_eq!(subs.list()[0].state, SubState::Background);
    ctx.jobs.kill(1);
    let i = until_state(&mut rx, |i| !i.state.running()).await;
    assert_eq!(i.state, SubState::Killed);
    assert!(subs.submit(&i.id, "too late").is_err(), "nothing is queued for a dead run");
    assert!(!subs.list()[0].state.running());

    // interrupted: Esc on the call that waits for it
    let client = Scripted::new(vec![], vec![Reply::Events(call("w1", "wait", json!({"file": "never"})))]);
    let agent = Arc::new(task_tool(&client));
    let ctx = call_ctx(d.path(), "i1");
    let (subs, cancel) = (ctx.jobs.subs(), ctx.cancel.clone());
    let run = {
        let (agent, ctx) = (agent.clone(), ctx.clone());
        tokio::spawn(async move { agent.run(task("explore", "wait here"), &ctx).await })
    };
    until_tool(&client, 1).await;
    assert_eq!(subs.list()[0].state, SubState::Attached);
    cancel.cancel();
    let out = run.await.unwrap();
    assert!(out.is_error && out.content.contains("interrupted"), "{}", out.content);
    assert_eq!(subs.list()[0].state, SubState::Killed);
}

#[tokio::test]
async fn nobody_listening_changes_nothing_for_the_sub_agent_or_the_registry() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![Reply::Events(says_then_calls(&["x"], "s1", "read", spends(1, 1))), Reply::Events(text("fine"))],
    );
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "t1"));
    let subs = ctx.jobs.subs();
    // a view that looked and went away
    drop(subs.changes());
    let out = agent.run(task("explore", "go"), &ctx).await;
    assert_eq!(out.content, "fine");
    assert_eq!(subs.list()[0].state, SubState::Ok);
    assert_eq!(subs.transcript("t1#1").unwrap().items.len(), 5);
}

#[tokio::test]
async fn a_message_that_arrives_while_the_last_reply_is_written_is_read_before_it_ends() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![],
        vec![late(d.path(), "first", text("first answer")), Reply::Events(text("second answer"))],
    );
    let (agent, ctx) = (task_tool(&client), call_ctx(d.path(), "t1"));
    let subs = ctx.jobs.subs();
    let run = {
        let ctx = ctx.clone();
        let agent = Arc::new(agent);
        tokio::spawn(async move { agent.run(task("explore", "answer"), &ctx).await })
    };
    until_sub(&client, 1).await;
    subs.submit("t1#1", "and one more thing").unwrap();
    release(d.path(), "first");
    let out = run.await.unwrap();
    assert_eq!(out.content, "second answer");
    let reqs = client.sub_requests();
    assert_eq!(reqs.len(), 2);
    let roles: Vec<String> = reqs[1].messages.iter().map(|m| format!("{:?}: {}", m.role, texts(m))).collect();
    assert_eq!(roles, ["User: answer", "Assistant: first answer", "User: and one more thing"]);
    let t = subs.transcript("t1#1").unwrap().items;
    assert!(t.contains(&SubItem::Person { text: "and one more thing".into() }));
}
