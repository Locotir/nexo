use super::*;

#[tokio::test]
async fn the_spend_limit_stops_before_the_call_that_would_pass_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("first")), Reply::Events(text("third"))]);
    let mut cfg = config(PermissionMode::Ask);
    // one reply of the script costs $0.0014 at m1's prices
    cfg.agent.max_cost = Some(0.001);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Budget { session_limit: Some(l), session_cost, .. } if *l == 0.001 && *session_cost > 0.001)));
    r.send(AgentCommand::Submit { text: "two".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("spend limit reached") && text.contains("this session"))));
    assert_eq!(client.requests().len(), 1, "the second request never reached the model");
    // a higher limit lets the work go on
    r.send(AgentCommand::SetBudget { limit: Some(1.0) });
    r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    r.send(AgentCommand::Submit { text: "three".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

/// A reply with text and then one call.
fn text_call(t: &str, id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }, StreamEvent::TextDelta(t.into())];
    evs.extend(call(id, name, args));
    evs
}

fn ck_config(max_turns: u32) -> Config {
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.max_turns = max_turns;
    // only the limit and the checkpoints stop these requests
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    cfg
}

const CONT: &str = "CHECKPOINT CONTINUE: more to do. NEXT: go on";

/// `max_turns = 2`: two calls, then a checkpoint, whose reply continues with a call, one more call, and so on
/// for `chunks` checkpoints; the reply to the last one is `last`. Each call writes a file of its own, so every
/// chunk makes progress.
fn writing_script(chunks: usize, last: Vec<StreamEvent>) -> Vec<Reply> {
    let mut out = Vec::new();
    for k in 0..(2 + 2 * chunks - 2) {
        let (id, args) = (format!("c{k}"), json!({"path": format!("f{k}")}));
        out.push(Reply::Events(if k >= 2 && k % 2 == 0 {
            text_call(CONT, &id, "write", args)
        } else {
            call(&id, "write", args)
        }));
    }
    out.push(Reply::Events(last));
    out
}

fn ck_notices(evs: &[AgentEvent]) -> Vec<(Level, String)> {
    evs.iter()
        .filter_map(
            |e| if let AgentEvent::Notice { level, text, .. } = e { Some((*level, text.clone())) } else { None },
        )
        .collect()
}

fn has_notice(evs: &[AgentEvent], needle: &str) -> bool {
    ck_notices(evs).iter().any(|(_, t)| t.contains(needle))
}

fn last_message_text(q: &ModelRequest) -> String {
    q.messages.last().unwrap().content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n")
}

async fn run_request(client: &Arc<Scripted>, cfg: Config, text: &str) -> Vec<AgentEvent> {
    let d = test_tmpdir().unwrap();
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: text.into(), attachments: vec![] });
    let evs = r.turn().await;
    r.stop().await;
    evs
}

#[tokio::test]
async fn the_limit_sends_a_checkpoint_with_the_stats_header() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: all done\nThe work is finished.")),
    ]);
    let evs = run_request(&client, ck_config(2), "read a and b").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    let m = last_message_text(&reqs[2]);
    assert!(m.starts_with("[checkpoint 1/5] Session at this point: $"), "{m}");
    for want in [
        "tool calls",
        "steps",
        "cached",
        "Consider, based on all the progress",
        "Request: read a and b",
        "CHECKPOINT CONTINUE",
    ] {
        assert!(m.contains(want), "{want} in {m}");
    }
    assert!(m.chars().count() < 1500);
    assert!(has_notice(&evs, "checkpoint 1 ·") && has_notice(&evs, "stop: all done"));
}

#[tokio::test]
async fn continue_resets_the_counter_and_goes_on() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call("CHECKPOINT CONTINUE: x. NEXT: read c", "c2", "read", json!({"path": "c"}))),
        Reply::Events(call("c3", "read", json!({"path": "d"}))),
        Reply::Events(text("CHECKPOINT STOP: done\nSummary here.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "two rounds, the checkpoint's reply, one more round, the second checkpoint");
    assert!(has_notice(&evs, "continue: x"), "{:?}", ck_notices(&evs));
    assert!(last_message_text(&reqs[4]).starts_with("[checkpoint 2/5]"));
    let kept = serde_json::to_string(&reqs[3].messages).unwrap();
    assert!(kept.contains("[checkpoint 1/5]"), "the first exchange stays in the history");
}

#[tokio::test]
async fn stop_ends_the_request_with_the_summary_and_the_call_does_not_run() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call(
            "CHECKPOINT STOP: enough\nSummary: the parser works.",
            "c9",
            "write",
            json!({"path": "z"}),
        )),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(text_of(&evs).contains("Summary: the parser works."));
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { id } if id == "c9")), "the call did not run");
    assert_eq!(client.requests().len(), 3);
    assert!(has_notice(&evs, "stop: enough"));
}

#[tokio::test]
async fn a_stop_without_a_summary_asks_for_it_once() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: done")),
        Reply::Events(text("Everything is finished.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    assert!(last_message_text(&reqs[3]).contains("final summary"));
    assert!(!reqs[3].no_tool_call && reqs[3].tools == reqs[2].tools, "the tools stay as they were: the cache is found");
    assert!(text_of(&evs).contains("Everything is finished."));
}

#[tokio::test]
async fn two_checkpoints_without_progress_stop_by_code() {
    // the same read over and over, the model always says continue
    let mut replies = vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "a"}))),
    ];
    for k in 2..12 {
        replies.push(Reply::Events(text_call(CONT, &format!("c{k}"), "read", json!({"path": "a"}))));
    }
    let client = Scripted::new(replies);
    let evs = run_request(&client, ck_config(2), "loop").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    // checkpoint 1 follows rounds with news, 2 follows rounds with none and is sent, 3 would be the second in a row
    assert_eq!(client.requests().len(), 6, "the model is not asked a third time");
    let reqs = client.requests();
    assert!(
        last_message_text(&reqs[4]).contains("[checkpoint 2/5]") && last_message_text(&reqs[4]).contains("no progress")
    );
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("no file written")));
}

#[tokio::test]
async fn progress_keeps_the_checkpoints_going() {
    let client = Scripted::new(writing_script(4, text("CHECKPOINT STOP: finished\nDone.")));
    let evs = run_request(&client, ck_config(2), "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(client.requests().len(), 9);
    assert!(has_notice(&evs, "checkpoint 4 ·"));
}

#[tokio::test]
async fn without_a_spend_limit_the_checkpoints_are_capped_at_five() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let evs = run_request(&client, ck_config(2), "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 12, "two rounds, then five checkpoints of two rounds each");
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("agent.max_checkpoints")));
}

#[tokio::test]
async fn max_checkpoints_sets_the_cap() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_checkpoints = Some(2);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6);
    assert!(has_notice(&evs, "stopped after 2 checkpoints"));
}

#[tokio::test]
async fn a_spend_limit_makes_the_checkpoints_unlimited() {
    let client = Scripted::new(writing_script(7, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_cost = Some(1000.0);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "seven checkpoints, more than the default five");
    assert_eq!(client.requests().len(), 15);
}

#[tokio::test]
async fn the_spend_limit_wins_over_continue() {
    let client = Scripted::new(writing_script(3, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    // each call costs 0.00002: the limit is crossed by the reply that says to continue
    cfg.agent.max_cost = Some(0.00005);
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::Error);
    assert_eq!(client.requests().len(), 3);
    assert!(has_notice(&evs, "spend limit reached"));
}

#[tokio::test]
async fn ask_keeps_the_old_behaviour() {
    let client = Scripted::new(writing_script(3, text("never reached")));
    let mut cfg = ck_config(2);
    cfg.agent.on_limit = nexo_core::config::OnLimit::Ask;
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 2);
    assert!(has_notice(&evs, "stopped after 2 tool rounds (agent.max_turns)"));
    assert!(!client.requests().iter().any(|q| serde_json::to_string(&q.messages).unwrap().contains("checkpoint")));
}

#[tokio::test]
async fn a_checkpoint_is_appended_and_the_prefix_stays() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: ok\nDone.")),
    ]);
    run_request(&client, ck_config(2), "go").await;
    let reqs = client.requests();
    let (before, after) = (&reqs[1], &reqs[2]);
    assert_eq!(after.messages[..before.messages.len()], before.messages[..], "nothing before the new messages changes");
    assert_eq!(after.messages.len(), before.messages.len() + 2, "the reply and the tool results");
    assert_eq!((&after.system, &after.tools, after.no_tool_call), (&before.system, &before.tools, before.no_tool_call));
    let last = after.messages.last().unwrap();
    let (results, texts): (Vec<&Part>, Vec<&Part>) =
        last.content.iter().partition(|p| matches!(p, Part::ToolResult { .. }));
    assert_eq!((results.len(), texts.len()), (1, 1), "the checkpoint rides in the tool-results message");
}

#[tokio::test]
async fn a_stop_at_a_checkpoint_pauses_the_goal() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text("CHECKPOINT STOP: blocked on a missing key\nI could not go on.")),
    ]);
    let mut r = start(client.clone(), ck_config(2), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(matches!(goal_state_of(&evs), Some(GoalState::Paused(w)) if w.starts_with("stopped at a checkpoint")));
    r.send(AgentCommand::Shutdown);
    let rest = r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    assert!(!rest.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no continuation is submitted");
    assert_eq!(client.requests().len(), 3);
    r.task.await.unwrap().unwrap();
}

/// Rounds that repeat one call, the model saying to continue each time: the checkpoints see no progress.
fn repeating_script(path: impl Fn(usize) -> String, name: &str) -> Vec<Reply> {
    let mut replies = vec![
        Reply::Events(call("c0", name, json!({"path": path(0)}))),
        Reply::Events(call("c1", name, json!({"path": path(1)}))),
    ];
    for k in 2..12 {
        replies.push(Reply::Events(text_call(CONT, &format!("c{k}"), name, json!({"path": path(k)}))));
    }
    replies
}

#[tokio::test]
async fn writing_the_same_thing_again_is_not_progress() {
    let client = Scripted::new(repeating_script(|_| "same".into(), "write"));
    let evs = run_request(&client, ck_config(2), "rewrite").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6, "stopped by code at the third checkpoint, as for no output at all");
    assert!(ck_notices(&evs).iter().any(|(l, t)| *l == Level::Warn && t.contains("no file written")));
}

#[tokio::test]
async fn a_counter_in_the_output_is_not_a_new_line() {
    // the output differs round to round by a number only
    let client = Scripted::new(repeating_script(|k| format!("tick{k}"), "read"));
    let evs = run_request(&client, ck_config(2), "poll").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 6);
}

#[tokio::test]
async fn a_spend_limit_that_cannot_be_reached_does_not_lift_the_cap() {
    let client = Scripted::new(writing_script(8, text("CHECKPOINT STOP: finished\nDone.")));
    let mut cfg = ck_config(2);
    cfg.agent.max_cost = Some(1000.0);
    // a model with no prices: its cost never grows, so the limit would never fire
    cfg.providers.get_mut("mock").unwrap().pricing.clear();
    let evs = run_request(&client, cfg, "write files").await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    assert_eq!(client.requests().len(), 12, "the default cap of five applies");
}

#[tokio::test]
async fn a_stop_in_spanish_is_understood() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(text_call("1. CHECKPOINT PARAR: ya esta\nResumen.", "c9", "write", json!({"path": "z"}))),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolStart { id } if id == "c9")), "the call did not run");
    assert!(has_notice(&evs, "stop: ya esta"));
}

#[tokio::test]
async fn a_goal_stopped_for_lack_of_progress_says_so() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(repeating_script(|_| "same".into(), "write"));
    let mut r = start(client.clone(), ck_config(2), None, dir.path()).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("ship it".into()) });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::MaxTurns);
    let rest = r.until(|e| matches!(e, AgentEvent::Goal { .. })).await;
    let all: Vec<AgentEvent> = evs.into_iter().chain(rest).collect();
    assert!(
        matches!(goal_state_of(&all), Some(GoalState::Paused(w)) if w.contains("without progress")),
        "{:?}",
        goal_state_of(&all)
    );
    assert!(!all.iter().any(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("tool-round limit"))));
    r.stop().await;
}

fn deltas(evs: &[AgentEvent]) -> String {
    evs.iter().filter_map(|e| if let AgentEvent::TextDelta { text } = e { Some(text.as_str()) } else { None }).collect()
}

/// A reply streamed in the given pieces.
fn pieces(parts: &[&str]) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::Start { response_id: None, model: None }];
    evs.extend(parts.iter().map(|p| StreamEvent::TextDelta((*p).into())));
    evs.extend([StreamEvent::Finish(FinishReason::Stop), StreamEvent::Done]);
    evs
}

#[tokio::test]
async fn the_decision_line_of_a_checkpoint_reply_is_not_streamed() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["CHECK", "POINT STOP: secret", " reason\n", "The work", " is finished."])),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    assert_eq!(deltas(&evs), "The work is finished.");
    assert!(has_notice(&evs, "stop: secret reason"));
}

#[tokio::test]
async fn a_checkpoint_reply_that_is_only_the_marker_streams_nothing() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["checkpoint continue: why. NEXT: x"])),
        Reply::Events(text("Done.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "Done.");
}

#[tokio::test]
async fn a_checkpoint_reply_without_the_marker_streams_unchanged() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["Plain ", "words\nmore"])),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "Plain words\nmore");
}

#[tokio::test]
async fn text_starting_with_checkpoint_outside_a_checkpoint_is_untouched() {
    let client = Scripted::new(vec![Reply::Events(pieces(&["CHECKPOINT STOP: not a decision\n", "body"]))]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "CHECKPOINT STOP: not a decision\nbody");
}

#[tokio::test]
async fn the_history_keeps_the_full_checkpoint_reply() {
    let client = Scripted::new(vec![
        Reply::Events(call("c0", "read", json!({"path": "a"}))),
        Reply::Events(call("c1", "read", json!({"path": "b"}))),
        Reply::Events(pieces(&["CHECKPOINT CONTINUE: keep me. NEXT: x\n", "going on"])),
        Reply::Events(text("Done.")),
    ]);
    let evs = run_request(&client, ck_config(2), "go").await;
    assert_eq!(deltas(&evs), "going onDone.");
    let kept = serde_json::to_string(&client.requests()[3].messages).unwrap();
    assert!(kept.contains("CHECKPOINT CONTINUE: keep me. NEXT: x\\ngoing on"), "{kept}");
}
