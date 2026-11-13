use super::*;

#[tokio::test]
async fn esc_stops_an_attached_isolated_writer_and_cleans_its_worktree() {
    let _one = TRUST.lock().await;
    data_dir();
    let (_keep, repo) = repo();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("iso", "work"))), Reply::Events(text("stopped"))],
        // the worktree has its own copy of the project: the sub-agent waits for `go` there
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Hang(text("never"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), &repo).await;
    r.submit("go");
    until_tool(&client, 1).await;
    let wts = repo.join(".nexo/worktrees");
    assert_eq!(std::fs::read_dir(&wts).unwrap().count(), 1, "the sub-agent works in a worktree");
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    // the call ends with "interrupted", or with what the sub-agent had said when Esc reached it (both end at
    // the same moment and either may be seen first); it never goes on to its next step
    assert!(out.contains("interrupted") || out.starts_with("never"), "{out}");
    // the worktree was finished before the call returned: it changed nothing, so it is gone
    assert_eq!(std::fs::read_dir(&wts).map(|d| d.count()).unwrap_or(0), 0, "no worktree is left");
    r.stop().await;
}

#[tokio::test]
async fn esc_leaves_a_detached_sub_agent_running() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow"))),
            Reply::Hang(text("thinking")),
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("survived"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    r.submit("meanwhile");
    r.until(|e| matches!(e, AgentEvent::ToolDone { id, .. } if id == "t1")).await;
    r.send(AgentCommand::Interrupt);
    r.turn().await;
    // it was not touched: it goes on and reports
    release(d.path(), "go");
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("noted"))));
    let reqs = client.main_requests();
    assert!(texts(reqs.last().unwrap().messages.last().unwrap()).contains("survived"));
    r.stop().await;
}

#[tokio::test]
async fn a_question_of_a_sub_agent_names_it_and_esc_leaves_a_detached_ones_open() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.allow.clear();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("general", "change a.txt"))), Reply::Hang(text("thinking"))],
        vec![Reply::Events(call("s1", "write", json!({"path": "a.txt"}))), Reply::Events(text("done"))],
    );
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    let evs = r.until(is_ask).await;
    let AgentEvent::PermissionAsk { request } = evs.last().unwrap() else { unreachable!() };
    assert_eq!(request.agent.as_deref(), Some("general"));
    let id = request.id.clone();
    // the person writes: the sub-agent goes to the background with its question open
    r.submit("meanwhile");
    r.until(is_done("t1")).await;
    r.send(AgentCommand::Interrupt);
    // the events of the turn that Esc ends: none of them resolves the question
    let turn = r.turn().await;
    assert!(!turn.iter().any(is_resolved(id.clone())), "Esc denied the question of a detached sub-agent: {turn:?}");
    assert!(
        r.quiet(300, is_resolved(id.clone())).await,
        "Esc did not deny the question of the sub-agent in the background"
    );
    // the question is alive: the answer lets the sub-agent go on to its end
    r.send(AgentCommand::Permission { id, decision: PermissionDecision::Allow });
    r.until(all_jobs_ended).await;
    r.stop().await;
}

#[tokio::test]
async fn a_question_of_a_sub_agent_started_in_the_background_stays_open_on_esc() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.permissions.allow.clear();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", bg("general", "change a.txt"))), Reply::Hang(text("thinking"))],
        vec![Reply::Events(call("s1", "write", json!({"path": "a.txt"}))), Reply::Events(text("done"))],
    );
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    // the call returns at once and the sub-agent asks, in either order
    let mut evs = r.until(is_done("t1")).await;
    if !evs.iter().any(is_ask) {
        evs.extend(r.until(is_ask).await);
    }
    let request = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionAsk { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(request.agent.as_deref(), Some("general"));
    let id = request.id.clone();
    r.send(AgentCommand::Interrupt);
    let turn = r.turn().await;
    assert!(!turn.iter().any(is_resolved(id.clone())), "Esc denied the question of a background sub-agent: {turn:?}");
    assert!(r.quiet(300, is_resolved(id.clone())).await, "Esc denied it a moment later");
    r.send(AgentCommand::Permission { id, decision: PermissionDecision::Allow });
    r.until(all_jobs_ended).await;
    r.stop().await;
}
