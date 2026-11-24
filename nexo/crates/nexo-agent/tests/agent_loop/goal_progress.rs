//! A goal whose work brings nothing new is told the plain facts of it, then stops with them; reading new files is progress.

use super::*;

fn reads(from: usize, n: usize) -> Vec<Reply> {
    (from..from + n)
        .map(|i| Reply::Events(call(&format!("r{i}"), "read", json!({"path": format!("src/f{i}.rs")}))))
        .collect()
}

fn offers(q: &ModelRequest, name: &str) -> bool {
    q.tools.iter().any(|t| t.name == name)
}

fn paused_why(evs: &[AgentEvent]) -> Option<String> {
    match goal_state_of(evs) {
        Some(GoalState::Paused(why)) => Some(why),
        _ => None,
    }
}

fn reads_of(paths: &[&str], from: usize) -> Vec<Reply> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| Reply::Events(call(&format!("q{}", from + i), "read", json!({"path": p}))))
        .collect()
}

/// A research goal reads forty different files and reports: each read brings something new, so it never pauses.
#[tokio::test]
async fn a_research_goal_that_reads_forty_files_and_reports_never_pauses() {
    let dir = test_tmpdir().unwrap();
    let mut replies = reads(0, 40);
    replies.push(Reply::Events(text("X fails because f7 drops the lock\nGOAL DONE")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("find why X fails and tell me".into()), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::Done);
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(goal_state_of(&after), Some(GoalState::Done));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 41);
    assert!(reqs.iter().all(|q| !text_of_parts(&q.messages.last().unwrap().content).contains("goal work:")));
    r.stop().await;
}

/// A goal that reads the same three unchanged files over and over: from the second repeat on, the facts ride on each
/// request (since when, how many steps, what came back the same), no tool is taken away and no generic note is
/// added; when the work stops on it, the goal pauses with those facts for the person.
#[tokio::test]
async fn a_goal_that_reads_the_same_three_files_again_pauses_with_the_facts() {
    let dir = test_tmpdir().unwrap();
    // an order that never repeats a run of rounds (a square-free word over three letters): the loop guard stays out
    let mut order = vec![0usize];
    while order.len() < 39 {
        order = order.iter().flat_map(|&x| [vec![0, 1, 2], vec![0, 2], vec![1]][x].clone()).collect();
    }
    let three = ["src/store.rs", "src/commands.rs", "src/lock.rs"];
    let replies: Vec<Reply> = order[..39]
        .iter()
        .enumerate()
        .map(|(i, &f)| Reply::Events(call(&format!("q{i}"), "read", json!({"path": three[f]}))))
        .collect();
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::MaxTurns);
    let reqs = client.requests();
    assert!(reqs.iter().all(|q| offers(q, "read") && offers(q, "grep")), "no tool is taken away");
    let tail = |k: usize| text_of_parts(&reqs[k].messages.last().unwrap().content);
    assert!(reqs.iter().all(|q| !text_of_parts(&q.messages.last().unwrap().content).contains("automatic check")));
    assert!(!tail(4).contains("goal work:"), "{}", tail(4));
    assert!(
        tail(5).contains(
            "[nexo, not the person: goal work: no progress for 2 steps, same result again: store.rs ×1, lock.rs ×1]"
        ),
        "{}",
        tail(5)
    );
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    let why = paused_why(&after).unwrap();
    assert!(why.starts_with("no progress for "), "{why}");
    assert!(why.contains(" steps, same result again: store.rs ×"), "{why}");
    assert!(notices(&after).iter().any(|n| n.starts_with(&format!("goal paused: {why}"))));
    r.stop().await;
}

/// A continuation round that only reads again what it read: the goal pauses there instead of asking again.
#[tokio::test]
async fn a_goal_round_that_only_reads_again_pauses_the_goal() {
    let dir = test_tmpdir().unwrap();
    let two = ["src/a.rs", "src/b.rs"];
    let mut replies = reads_of(&two, 0);
    replies.push(Reply::Events(text("looked at it")));
    replies.extend(reads_of(&two, 2));
    replies.push(Reply::Events(text("looked again")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()), attachments: vec![] });
    r.turn().await;
    r.turn().await;
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    let why = paused_why(&after).unwrap();
    assert_eq!(why, "the last round made no progress (no progress for 2 steps, same result again: a.rs ×1, b.rs ×1)");
    assert_eq!(client.requests().len(), 6, "no third round");
    r.stop().await;
}

/// A goal that reads a lot and then edits keeps its tools, and it ends met.
#[tokio::test]
async fn a_goal_that_reads_then_edits_keeps_its_tools() {
    let dir = test_tmpdir().unwrap();
    let mut replies = reads(0, 20);
    replies.push(Reply::Events(call("w1", "write", json!({"path": "src/store.rs", "content": "x"}))));
    replies.extend(reads(20, 20));
    replies.push(Reply::Events(call("w2", "write", json!({"path": "src/cmd.rs", "content": "y"}))));
    replies.push(Reply::Events(text("done\nGOAL DONE")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::Done);
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(goal_state_of(&after), Some(GoalState::Done));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 43);
    assert!(reqs.iter().all(|q| offers(q, "read")), "the reading tools stay");
    r.stop().await;
}

/// Without a goal, a long question answered from reads alone is never cut short.
#[tokio::test]
async fn a_read_heavy_question_without_a_goal_is_not_stopped() {
    let dir = test_tmpdir().unwrap();
    let mut replies = reads(0, 40);
    replies.push(Reply::Events(text("here is how it works")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "how does the store work?".into(), attachments: vec![] });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::Done);
    assert_eq!(text_of(&t), "here is how it works");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 41);
    assert!(reqs.iter().all(|q| offers(q, "read")));
    r.stop().await;
}

/// What the model reads of the last message of request `k`: the continuation mark or the tool results, and the
/// facts that ride on that request only.
fn tail_of(client: &Scripted, k: usize) -> String {
    text_of_parts(&client.requests()[k].messages.last().unwrap().content)
}

fn files_in(dir: &std::path::Path, names: &[&str]) {
    for n in names {
        std::fs::write(dir.join(n), "fn f() {}\n").unwrap();
    }
}

/// A goal run states its own counts on each request: calls, distinct files read, files edited, the reads since the
/// last edit (back to zero at an edit) and the commands with the failures; the line comes with the continuation of
/// the second round, as the person's own request and the saved conversation never carry it.
#[cfg(unix)]
#[tokio::test]
async fn a_goal_run_shows_the_model_its_own_counts() {
    let dir = test_tmpdir().unwrap();
    files_in(dir.path(), &["a.rs", "b.rs", "c.rs", "d.rs"]);
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("r2", "read", json!({"path": "b.rs"}))),
        Reply::Events(call("r3", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("w1", "write", json!({"path": "out.rs", "content": "fn out() {}\n"}))),
        Reply::Events(call("r4", "read", json!({"path": "c.rs"}))),
        Reply::Events(call("b1", "bash", json!({"command": "exit 3"}))),
        Reply::Events(text("checked it, going on")),
        Reply::Events(call("r5", "read", json!({"path": "d.rs"}))),
        Reply::Events(text("all done\nGOAL DONE")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("find the leak".into()), attachments: vec![] });
    let first = r.turn().await;
    assert_eq!(end_of(&first), TurnEnd::Done);
    let second = r.turn().await;
    assert_eq!(end_of(&second), TurnEnd::Done);
    r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(client.requests().len(), 9);
    // nothing to count before the first call
    assert!(!tail_of(&client, 0).contains("goal so far"), "{}", tail_of(&client, 0));
    // an edit sends the reads since the last one back to zero
    assert!(
        tail_of(&client, 3)
            .contains("[nexo, not the person: goal so far: 0 min, round 1, 3 tool calls; read 2 files, edited 0; ran 0 commands, 0 failed.]"),
        "{}",
        tail_of(&client, 3)
    );
    assert!(
        tail_of(&client, 4).contains(
            "[nexo, not the person: goal so far: 0 min, round 1, 4 tool calls; read 2 files (0 since the last edit), edited 1; ran 0 commands, 0 failed.]"
        ),
        "{}",
        tail_of(&client, 4)
    );
    assert!(tail_of(&client, 5).contains("read 3 files (1 since the last edit), edited 1; ran 0 commands"));
    // the second round: the continuation, then the line with the failed command counted
    let line = "[nexo, not the person: goal so far: 0 min, round 2, 6 tool calls; read 3 files (1 since the last edit), edited 1; ran 1 command, 1 failed.]";
    assert!(tail_of(&client, 7).starts_with("[goal: continue]"), "{}", tail_of(&client, 7));
    assert!(tail_of(&client, 7).contains(line), "{}", tail_of(&client, 7));
    // nexo wrote it, not the person: the trailing line says so
    let t7 = tail_of(&client, 7);
    let counts = &t7[t7.find("[nexo").unwrap_or(0)..];
    assert!(counts.starts_with("[nexo, not the person: goal so far:"), "{t7}");
    assert!(counts.ends_with(']'), "{t7}");
    assert!(tail_of(&client, 8).contains("round 2, 7 tool calls; read 4 files (2 since the last edit)"));
    // it rides on the request only: the saved conversation holds the continuation mark alone
    let saved: Vec<String> = second
        .iter()
        .filter_map(|e| match e {
            AgentEvent::History { message, .. } if message.role == nexo_core::message::Role::User => {
                Some(message.text())
            }
            _ => None,
        })
        .collect();
    assert_eq!(saved.first().map(String::as_str), Some("[goal: continue]"));
    assert!(saved.iter().all(|t| !t.contains("goal so far")), "{saved:?}");
    r.stop().await;
}

/// A goal resumed after a stop counts again from its own start: what it read before the stop is not in the line.
#[tokio::test]
async fn a_resumed_goal_starts_its_counts_over() {
    let dir = test_tmpdir().unwrap();
    files_in(dir.path(), &["a.rs", "b.rs"]);
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "a.rs"}))),
        Reply::Hang(vec![StreamEvent::TextDelta("half".into())]),
        Reply::Events(call("r2", "read", json!({"path": "b.rs"}))),
        Reply::Events(text("all set\nGOAL DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("find the leak".into()), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    assert_eq!(end_of(&r.turn().await), TurnEnd::Interrupted);
    // the request that hung had the first read counted
    assert!(tail_of(&client, 1).contains("round 1, 1 tool call; read 1 file, edited 0"), "{}", tail_of(&client, 1));
    r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    r.send(AgentCommand::ResumeGoal);
    assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    let after = tail_of(&client, 3);
    assert!(
        after.contains("[nexo, not the person: goal so far: 0 min, round 1, 1 tool call; read 1 file, edited 0; ran 0 commands, 0 failed.]"),
        "{after}"
    );
    r.stop().await;
}

/// A supervisor note and the counts of a goal ride on the same request: they go as one trailing text part, so a
/// provider's cache breakpoint before it lands where the next request, without them, still finds its prefix.
#[cfg(unix)]
#[tokio::test]
async fn a_supervisor_note_and_the_goal_counts_ride_as_one_text_part() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "exit 127"}))),
        Reply::Events(call("b2", "bash", json!({"command": "exit 127"}))),
        Reply::Events(text("giving up\nGOAL DONE")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    let mut r = start_with(client.clone(), cfg, None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("read the file".into()), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    let last = client.requests()[2].messages.last().unwrap().clone();
    let texts: Vec<&str> = last
        .content
        .iter()
        .filter_map(|p| if let Part::Text { text, .. } = p { Some(text.as_str()) } else { None })
        .collect();
    let tail = texts.last().copied().unwrap_or("");
    assert!(tail.contains("was run again") && tail.contains("goal so far"), "{texts:?}");
    assert!(tail.find("was run again") < tail.find("goal so far"), "the supervisor note comes first: {tail}");
    assert!(
        texts[..texts.len() - 1].iter().all(|t| !t.contains("goal so far") && !t.contains("was run again")),
        "{texts:?}"
    );
    // the person is still told of the supervisor note
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::Added { parts, .. } if parts.iter().any(|p| p.kind == AddedKind::Supervisor))
    ));
    r.stop().await;
}

/// Novel successful read results renew the window too: research does not have to edit files.
#[tokio::test]
async fn a_research_goal_continues_past_five_rounds_then_finishes() {
    let dir = test_tmpdir().unwrap();
    let mut replies = vec![Reply::Events(text("starting"))];
    for i in 0..7 {
        replies.extend(reads(i, 1));
        replies.push(Reply::Events(text(if i == 6 { "reported\nGOAL DONE" } else { "more to check" })));
    }
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("research the code".into()), attachments: vec![] });
    for _ in 0..8 {
        assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    }
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    assert_eq!(goal_state_of(&after), Some(GoalState::Done));
    assert_eq!(client.requests().len(), 15);
    r.stop().await;
}

/// A failed command is not progress, even if its arguments differ from earlier calls.
#[cfg(unix)]
#[tokio::test]
async fn a_goal_continuation_with_only_errors_pauses() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(text("starting")),
        Reply::Events(call("b1", "bash", json!({"command": "exit 3"}))),
        Reply::Events(call("b2", "bash", json!({"command": "exit 4"}))),
        Reply::Events(text("still failing")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("fix the command".into()), attachments: vec![] });
    r.turn().await;
    r.turn().await;
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    assert_eq!(paused_why(&after).as_deref(), Some("the last round made no progress (2 steps)"));
    assert_eq!(client.requests().len(), 4, "errors must not start another continuation");
    r.stop().await;
}

/// Exec permission is not proof of a mutation. Supply an initial command and twelve identical
/// successful continuations; stop after the first repeated result, before renewing two windows.
#[cfg(unix)]
#[tokio::test]
async fn a_goal_repeated_unknown_successful_command_pauses_instead_of_renewing() {
    let dir = test_tmpdir().unwrap();
    // A shell function queries constant data but is deliberately outside the read-only allowlist.
    let args = json!({"command": "query() { printf stable; }; query"});
    assert_eq!(nexo_core::Tool::call_kind(&nexo_tools::bash::Bash, &args), nexo_core::ToolKind::Exec);
    let mut replies = Vec::new();
    for i in 0..13 {
        replies.push(Reply::Events(call(&format!("b{i}"), "bash", args.clone())));
        replies.push(Reply::Events(text("still investigating")));
    }
    let client = Scripted::new(replies);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("investigate the data".into()), attachments: vec![] });
    for _ in 0..2 {
        assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    }
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    assert!(paused_why(&after).unwrap().starts_with("the last round made no progress"));
    assert_eq!(client.requests().len(), 4, "only one continuation; unused script must not run");
    r.stop().await;
}

/// The same command can accompany genuine research across two window renewals. Its remembered
/// result must survive those productive rounds, so once the new reads stop it cannot sustain work.
#[cfg(unix)]
#[tokio::test]
async fn a_goal_repeated_command_with_new_reads_continues_then_pauses_across_windows() {
    let dir = test_tmpdir().unwrap();
    let args = json!({"command": "query() { printf stable; }; query"});
    let mut replies = vec![Reply::Events(text("starting"))];
    for i in 0..12 {
        replies.push(Reply::Events(call(&format!("b{i}"), "bash", args.clone())));
        if i < 11 {
            replies.extend(reads(i, 1));
        }
        replies.push(Reply::Events(text("still investigating")));
    }
    let client = Scripted::new(replies);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("investigate the code".into()), attachments: vec![] });
    for _ in 0..13 {
        assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    }
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    assert!(paused_why(&after).unwrap().starts_with("the last round made no progress"));
    assert_eq!(client.requests().len(), 36, "initial turn, eleven productive continuations, one repeat-only");
    r.stop().await;
}

/// Identical command arguments with genuinely changing output remain observable progress.
#[cfg(unix)]
#[tokio::test]
async fn a_goal_repeated_command_with_changed_output_continues_then_pauses() {
    let dir = test_tmpdir().unwrap();
    let args = json!({"command": "n=$(cat count); if [ \"$n\" -lt 11 ]; then n=$((n + 1)); printf '%s' \"$n\" > count; fi; cat count"});
    std::fs::write(dir.path().join("count"), "0").unwrap();
    let mut replies = vec![Reply::Events(text("starting"))];
    for i in 0..12 {
        replies.push(Reply::Events(call(&format!("b{i}"), "bash", args.clone())));
        replies.push(Reply::Events(text("still working")));
    }
    let client = Scripted::new(replies);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, dir.path(), set).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("advance the counter".into()), attachments: vec![] });
    for _ in 0..13 {
        assert_eq!(end_of(&r.turn().await), TurnEnd::Done);
    }
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    assert!(paused_why(&after).unwrap().starts_with("the last round made no progress"));
    assert_eq!(std::fs::read_to_string(dir.path().join("count")).unwrap(), "11");
    assert_eq!(client.requests().len(), 25);
    r.stop().await;
}
