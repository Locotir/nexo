//! A goal whose work changes nothing stops, and a goal round reads an earlier message of the person as answered.

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

const LONG_GOAL: &str = "add a store many sessions can write at once, with a lock, a /shared-memory command that \
                         lists it, tests for both, and a line in the docs";

/// The case of a 42-minute run: "hola" answered, then `/goal` with a long text, a long first turn and goal rounds.
/// Every goal request says the goal is the task in progress and that what came before `[goal set]` is answered.
#[tokio::test]
async fn a_greeting_before_the_goal_reads_as_answered_on_every_goal_round() {
    let dir = test_tmpdir().unwrap();
    let mut replies = vec![Reply::Events(text("Hola."))];
    replies.extend(reads(0, 8));
    replies.push(Reply::Events(call("w1", "write", json!({"path": "src/store.rs", "content": "x"}))));
    replies.push(Reply::Events(text("store written")));
    replies.push(Reply::Events(call("w2", "write", json!({"path": "src/cmd.rs", "content": "y"}))));
    replies.push(Reply::Events(text("all of it\nGOAL DONE")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::Submit { text: "hola".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::SetGoal { text: Some(LONG_GOAL.into()) });
    r.turn().await;
    let t2 = r.turn().await;
    assert!(text_of(&t2).contains("GOAL DONE"));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 13);
    assert!(!reqs[0].system.contains("Goal,"), "no goal before /goal");
    for (k, q) in reqs.iter().enumerate().skip(1) {
        // the rules of the goal say which message is the task: the greeting before `[goal set]` is answered
        assert!(q.system.contains("The goal is the task in progress"), "request {k}");
        assert!(q.system.contains("before `[goal set]` was answered already, so do not answer it again"), "{k}");
        assert!(!q.system.contains("A message from the person comes before the goal"), "request {k}");
        // the greeting comes with its answer, and the goal's request after them
        let texts: Vec<String> = q.messages.iter().map(Message::text).collect();
        assert_eq!((texts[0].as_str(), texts[1].as_str()), ("hola", "Hola."), "request {k}");
        assert!(texts[2].starts_with(&format!("[goal set] {LONG_GOAL}")), "request {k}: {}", texts[2]);
        let newest = q.messages.iter().rposition(|m| m.is_person()).unwrap();
        assert!(newest >= 2, "request {k}: the greeting is never the newest request");
    }
    // on the continuation round the newest request is the loop's own mark, not the person
    let last = reqs[12].messages.iter().rfind(|m| m.is_person()).unwrap();
    assert_eq!(last.text(), "[goal: continue]");
    r.stop().await;
}

/// A continuation round that only reads changed nothing: the goal pauses there instead of asking again.
#[tokio::test]
async fn a_goal_round_of_reads_alone_pauses_the_goal() {
    let dir = test_tmpdir().unwrap();
    let mut replies = reads(0, 2);
    replies.push(Reply::Events(text("looked at it")));
    replies.extend(reads(2, 3));
    replies.push(Reply::Events(text("looked again")));
    let client = Scripted::new(replies);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()) });
    r.turn().await;
    r.turn().await;
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    let why = paused_why(&after).unwrap();
    // the steps count from the last change, across the goal's rounds: 2 in the first turn, 3 in the round
    assert_eq!(why, "the last round changed nothing (5 steps without a change)");
    assert_eq!(client.requests().len(), 7, "no third round");
    r.stop().await;
}

/// Reads go on after two supervisor notes with no change: the reading tools go away for the next request, and a
/// step that still changes nothing ends the turn and pauses the goal.
#[tokio::test]
async fn ignored_notes_take_the_reading_tools_away_then_pause_the_goal() {
    let dir = test_tmpdir().unwrap();
    // 24 reads raise two notes (one each 12 reads in a row); the 25th call is a read all the same
    let client = Scripted::new(reads(0, 25));
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("make the store safe".into()) });
    let t = r.turn().await;
    assert_eq!(end_of(&t), TurnEnd::MaxTurns);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 25);
    assert!(reqs[..24].iter().all(|q| offers(q, "read") && offers(q, "grep")));
    let narrowed = &reqs[24];
    assert!(!offers(narrowed, "read") && !offers(narrowed, "grep"), "reading and search are off");
    assert!(offers(narrowed, "write") && offers(narrowed, "bash"), "acting is not");
    let note = text_of_parts(&narrowed.messages.last().unwrap().content);
    assert!(note.contains("24 steps since the last change and 2 notes about it"), "{note}");
    assert!(notices(&t).iter().any(|n| n.contains("reading tools are off")), "{:?}", notices(&t));
    let after = r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Paused(_), .. })).await;
    assert_eq!(paused_why(&after).as_deref(), Some("25 steps without a change"));
    assert!(notices(&after).iter().any(|n| n.starts_with("goal paused: 25 steps without a change")));
    r.stop().await;
}

/// A goal that reads a lot and then edits is not narrowed: the edit resets the count, and it ends met.
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
