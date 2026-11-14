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
    r.send(AgentCommand::SetGoal { text: Some("find why X fails and tell me".into()) });
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
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()) });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::MaxTurns);
    let reqs = client.requests();
    assert!(reqs.iter().all(|q| offers(q, "read") && offers(q, "grep")), "no tool is taken away");
    let tail = |k: usize| text_of_parts(&reqs[k].messages.last().unwrap().content);
    assert!(reqs.iter().all(|q| !text_of_parts(&q.messages.last().unwrap().content).contains("automatic check")));
    assert!(!tail(4).contains("goal work:"), "{}", tail(4));
    assert!(
        tail(5).contains("[goal work: no progress for 2 steps, same result again: store.rs ×1, lock.rs ×1]"),
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
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()) });
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
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()) });
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
