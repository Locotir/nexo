use super::*;

fn goal_of(evs: &[AgentEvent]) -> Option<(Option<String>, u32, u32)> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::Goal { text, round, max, .. } => Some((text.clone(), *round, *max)),
        _ => None,
    })
}

fn last_goal_log(evs: &[AgentEvent]) -> Option<nexo_core::agent::GoalLog> {
    evs.iter().rev().find_map(|e| match e {
        AgentEvent::GoalLog(l) => Some(l.clone()),
        _ => None,
    })
}

fn user_texts(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::History { message, .. } if message.role == nexo_core::message::Role::User => {
                Some(message.text())
            }
            _ => None,
        })
        .collect()
}

const CONTINUE: &str = "[goal: continue]";

#[tokio::test]
async fn a_goal_starts_the_work_and_keeps_going_until_it_says_goal_done() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("working on it")),
        // the first continuation does real work: a call, then its reply
        Reply::Events(call("c1", "write", json!({"path": "a.rs"}))),
        Reply::Events(text("more work")),
        Reply::Events(call("c2", "write", json!({"path": "b.rs"}))),
        Reply::Events(text("all set\nGOAL DONE. Thanks!")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("  make the tests pass ".into()) });
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("make the tests pass".into()), 0, 5)));
    // /goal starts the work by itself: the goal is the first request
    let t1 = r.turn().await;
    assert_eq!(user_texts(&t1), vec!["[goal set] make the tests pass"]);
    assert_eq!(text_of(&t1), "working on it");
    // the agent continues by itself, twice, then the model declares the goal met (with a closing sentence)
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()), "{:?}", user_texts(&t2));
    assert_eq!(goal_of(&t2).map(|g| g.1), Some(1));
    let t3 = r.turn().await;
    assert_eq!(goal_of(&t3).map(|g| g.1), Some(2));
    assert!(text_of(&t3).contains("GOAL DONE"));
    let after = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&after), Some((Some("make the tests pass".into()), 0, 5)));
    assert!(after.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("goal met"))));
    assert_eq!(goal_state_of(&after), Some(GoalState::Done));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5);
    assert!(reqs.iter().all(|q| q.system.contains("Goal, in the person's words: make the tests pass\n")));
    r.stop().await;
}

#[tokio::test]
async fn the_agent_restates_the_goal_and_the_restatement_is_the_goal() {
    let dir = test_tmpdir().unwrap();
    let brief = json!({
        "want": "every test in the workspace passes",
        "why": "the release is blocked on red tests",
        "how": "fix the code, not the tests; keep the style",
        "done": "cargo test shows 0 failed"
    });
    let client =
        Scripted::new(vec![Reply::Events(call("g1", "goal_brief", brief)), Reply::Events(text("on it\nGOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("tests green pls".into()) });
    let t1 = r.turn().await;
    let want =
        "Want: every test in the workspace passes\nWhy: the release is blocked on red tests\nHow: fix the code, \
                not the tests; keep the style\nDone when: cargo test shows 0 failed";
    assert!(t1.iter().any(|e| matches!(e, AgentEvent::Goal { text: Some(t), .. } if t == want)));
    // the person's words are kept beside the restatement, with the call it took
    let log = last_goal_log(&t1).expect("the goal's log is sent");
    assert_eq!(log.original.as_deref(), Some("tests green pls"));
    assert_eq!(log.brief.as_deref(), Some(want));
    assert!(log.steps >= 1 && log.started_ms > 0, "{log:?}");
    let reqs = client.requests();
    assert!(reqs[0].system.contains("Goal, in the person's words: tests green pls\n"));
    assert!(reqs[0].tools.iter().any(|t| t.name == "goal_brief"));
    // from the next request on, the restatement stands in for the person's words
    assert!(reqs[1].system.contains(&format!("Goal, as the person means it:\n{want}\n")));
    assert!(!reqs[1].system.contains("tests green pls"));
    r.stop().await;
    // a resumed session keeps the restatement
    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs).and_then(|g| g.0), Some(want.to_string()));
    let log = last_goal_log(&evs).expect("a resumed goal sends its log");
    assert_eq!((log.original, log.brief.as_deref()), (None, Some(want)), "the restatement is all that was saved");
    r.stop().await;
}

#[tokio::test]
async fn without_a_goal_there_is_no_goal_brief_to_call() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    assert!(!client.requests()[0].tools.iter().any(|t| t.name == "goal_brief"));
    r.stop().await;
}

#[tokio::test]
async fn a_goal_set_while_a_turn_runs_only_pins_it() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.send(AgentCommand::SetGoal { text: Some("finish it".into()) });
    let t1 = r.turn().await;
    assert_eq!(user_texts(&t1), vec!["go"], "no second request while the turn runs");
    assert!(t1.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("from the next turn"))));
    // the goal is checked when that turn ends: the first continuation runs
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()));
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn a_goal_continuation_leaves_the_round_to_the_interface() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.send(AgentCommand::SetGoal { text: Some("finish it".into()) });
    r.turn().await;
    let t2 = r.turn().await;
    assert!(user_texts(&t2).contains(&CONTINUE.to_string()));
    assert!(t2.iter().any(|e| matches!(e, AgentEvent::Goal { round: 1, .. })));
    assert!(
        !t2.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("goal: continuing"))),
        "the interface draws the line itself: {t2:?}"
    );
    r.stop().await;
}

#[tokio::test]
async fn a_continuation_without_tool_calls_ends_the_goal_loop() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("working")), Reply::Events(text("nothing more to do"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("tidy up".into()) });
    r.turn().await;
    // the first continuation always runs; it made no call, so the loop stops there
    let t2 = r.turn().await;
    assert_eq!(goal_of(&t2).map(|g| g.1), Some(1));
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("tidy up".into()), 0, 5)), "rounds reset");
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("the last round did nothing new".into())));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("goal paused: the last round"))));
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })));
    assert_eq!(client.requests().len(), 2);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_goal_stops_after_its_rounds_and_when_cleared() {
    let dir = test_tmpdir().unwrap();
    // every round changes a file, so the loop only stops at the cap
    let mut replies = vec![Reply::Events(text("still at it"))];
    for i in 0..5 {
        replies.push(Reply::Events(call(&format!("c{i}"), "write", json!({"path": "a.rs"}))));
        replies.push(Reply::Events(text("still at it")));
    }
    replies.push(Reply::Events(text("again")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("never done".into()) });
    r.turn().await;
    for round in 1..=5u32 {
        let t = r.turn().await;
        assert_eq!(goal_of(&t).map(|g| g.1), Some(round), "round {round}");
    }
    let warned =
        r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("rounds without GOAL DONE"))).await;
    assert!(!warned.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })));
    let reset = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&reset).map(|g| g.1), Some(0), "rounds reset after the cap");
    assert_eq!(goal_state_of(&reset), Some(GoalState::Paused("5 rounds without GOAL DONE".into())));
    assert_eq!(client.requests().len(), 11, "the first turn and five continuations of two requests each");
    // clearing the goal: a turn ends and nothing follows
    r.send(AgentCommand::SetGoal { text: None });
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((None, 0, 5)));
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. } | AgentEvent::Goal { .. })));
    assert_eq!(client.requests().len(), 12);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_resumed_session_brings_its_goal_back() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("GOAL DONE"))]);
    let mut r = start(client, config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("keep it green".into()) });
    r.turn().await;
    r.stop().await;

    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_of(&evs), Some((Some("keep it green".into()), 0, 5)));
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done), "a met goal is still met after a resume");
    r.stop().await;
}

#[tokio::test]
async fn a_reply_that_asks_waits_for_the_answer_and_the_answer_resumes_the_goal() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("Two branches could take the fix.\n\n**Which one should I use, `main` or `next`?**")),
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("fixed on main")),
        Reply::Events(text("all green\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("fix the bug".into()) });
    r.turn().await;
    // the question stops the loop: no continuation answers it in the person's place
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Waiting));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("waiting for your answer"))));
    assert_eq!(client.requests().len(), 1);
    // the answer makes the goal active again, and the loop goes on after it
    r.send(AgentCommand::Submit { text: "main".into(), attachments: vec![] });
    let t2 = r.turn().await;
    assert_eq!(goal_state_of(&t2), Some(GoalState::Active));
    assert_eq!(user_texts(&t2), vec!["main"]);
    let t3 = r.turn().await;
    assert!(user_texts(&t3).contains(&CONTINUE.to_string()));
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 4);
    r.stop().await;
}

#[tokio::test]
async fn a_met_goal_is_not_restarted_by_the_next_message() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("done\nGOAL DONE")), Reply::Events(text("you're welcome"))]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    r.turn().await;
    r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    r.send(AgentCommand::Submit { text: "thanks".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no continuation after a met goal");
    assert_eq!(client.requests().len(), 2);
    r.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn esc_and_a_failed_turn_pause_the_goal_and_resume_goes_on() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("half".into())]),
        Reply::Err(ProviderError::new(ErrorKind::BadRequest, "bad request")),
        Reply::Events(text("all set\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("tidy up".into()) });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    assert_eq!(end_of(&r.turn().await), TurnEnd::Interrupted);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("interrupted".into())));
    // resume starts a continuation at once; it fails, and the goal pauses again instead of looping
    r.send(AgentCommand::ResumeGoal);
    let t = r.turn().await;
    assert_eq!(goal_state_of(&t), Some(GoalState::Active));
    assert_eq!(user_texts(&t), vec![CONTINUE.to_string()]);
    assert_eq!(end_of(&t), TurnEnd::Error);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("the turn failed".into())));
    // each stop is kept with its reason and its time, the goal's own words are not touched
    let log = last_goal_log(&evs).expect("the goal's log is sent");
    let whys: Vec<&str> = log.stops.iter().map(|s| s.why.as_str()).collect();
    assert_eq!(whys, vec!["interrupted", "the turn failed"]);
    assert!(log.stops.iter().all(|s| s.at_ms > 0) && log.stops[0].at_ms <= log.stops[1].at_ms);
    assert_eq!(log.original.as_deref(), Some("tidy up"));
    assert_eq!(log.brief, None);
    r.send(AgentCommand::ResumeGoal);
    r.turn().await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn a_paused_goal_holds_through_messages_until_resume() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("I need one thing from you.\nGOAL WAIT")),
        Reply::Events(text("noted")),
        Reply::Events(text("finished\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::SetGoal { text: Some("clean the logs".into()) });
    r.turn().await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Waiting), "GOAL WAIT waits as a question does");
    r.send(AgentCommand::PauseGoal);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("paused by you".into())));
    // a message while paused is answered, and nothing more starts
    r.send(AgentCommand::Submit { text: "note this".into(), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(goal_state_of(&t), None, "the message does not touch a paused goal");
    r.stop().await;
    assert_eq!(client.requests().len(), 2);

    // the pause is kept with the session; resume goes on
    let mut r = start(client.clone(), config(PermissionMode::Auto), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Paused("paused by you".into())));
    r.send(AgentCommand::ResumeGoal);
    let t = r.turn().await;
    assert_eq!(user_texts(&t), vec![CONTINUE.to_string()]);
    let evs = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    assert_eq!(goal_state_of(&evs), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 3);
    r.stop().await;
}

#[tokio::test]
async fn a_todo_call_shows_the_list_and_the_replay_keeps_it() {
    let dir = test_tmpdir().unwrap();
    let items = json!({"items": [{"text": "read", "status": "done"}, {"text": "fix", "status": "in_progress"}, {"text": "test"}]});
    let client = Scripted::new(vec![Reply::Events(call("c1", "todo", items)), Reply::Events(text("on it"))]);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    // the todo tool never asks, even in ask mode
    let mut r = start_with(client, config(PermissionMode::Ask), None, dir.path(), todo_tools).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })));
    let done = evs.iter().find_map(|e| match e {
        AgentEvent::ToolDone { output, .. } => Some(output.clone()),
        _ => None,
    });
    let out = done.expect("the todo call ran");
    assert_eq!(out.content, "[x] read\n[>] fix\n[ ] test\n1/3 done");
    assert!(matches!(&out.display, nexo_core::ToolDisplay::Todo { items } if items.len() == 3));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolCall { summary, .. } if summary == "todo: 1/3 done")));
    r.stop().await;

    let client = Scripted::new(vec![]);
    let mut r = start(client, config(PermissionMode::Ask), Some(id), dir.path()).await;
    let evs = r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { output, .. } if matches!(output.display, nexo_core::ToolDisplay::Todo { .. }))));
    r.stop().await;
}
