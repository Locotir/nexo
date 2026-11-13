use super::*;

#[tokio::test]
async fn a_quiet_person_gets_the_report_as_the_tool_output_and_no_ended_note() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(call("t2", "task", task("general", "change a.txt"))),
            Reply::Events(text("done")),
        ],
        vec![
            ("look", vec![Reply::Events(text("reader report"))]),
            (
                "change a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt"}))),
                    Reply::Events(text("writer report")),
                ],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r.turn().await;
    // the old code made the output `{note}{report}{tail}`: no note unless background was asked, no tail
    // unless the sub-agent was isolated. A quiet person gets the report and nothing else.
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("reader report"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("writer report"));
    // the third request: the history holds both outputs, as it did, and nothing about ended jobs
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(
        dump(&reqs[2]),
        "User text: go\n\
         Assistant call t1 task {\"agent\":\"explore\",\"prompt\":\"look\"}\n\
         User result t1 error=false: reader report\n\
         Assistant call t2 task {\"agent\":\"general\",\"prompt\":\"change a.txt\"}\n\
         User result t2 error=false: writer report"
    );
    // and the writer's own tool ran as it always did
    assert_eq!(result_for(&requests_of(&client, "change a.txt"), "s1").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn an_isolated_writer_that_changed_nothing_ends_with_the_old_worktree_note() {
    let _one = TRUST.lock().await;
    data_dir();
    let (_keep, repo) = repo();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("iso", "work"))), Reply::Events(text("done"))],
        vec![Reply::Events(text("isolated report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), &repo).await;
    r.submit("go");
    let evs = r.turn().await;
    // `{report}{tail}` with the tail of `worktree_note` for a worktree without changes, as it was written before
    assert_eq!(
        done_of(&evs, "t1").as_deref(),
        Some("isolated report\n\n[worktree] the sub-agent changed nothing; its worktree was removed")
    );
    r.stop().await;
}

#[tokio::test]
async fn a_background_reader_keeps_the_old_started_text_and_the_old_ended_note() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("explore", "look"))),
            // the main agent's own tool keeps the round open while the reader ends
            Reply::Events(call("m1", "wait", json!({}))),
            Reply::Events(text("ok")),
        ],
        vec![("look", vec![Reply::Events(text("bg report"))])],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let evs = r.until_all(&[&is_done("t1"), &job_ended(1)]).await;
    // read-only with `background: true`: byte for byte what the model was told before
    assert_eq!(
        done_of(&evs, "t1").as_deref(),
        Some(
            "Started sub-agent job 1 (explore): \"look\". It works while you go on; you will be told when it ends. \
             Its report is the job's output: jobs {\"action\": \"output\", \"id\": 1} (add \"wait\" to wait for it), \
             stop it with {\"action\": \"kill\", \"id\": 1}."
        )
    );
    release(d.path(), "go");
    r.turn().await;
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 3);
    // the ended note of a job: the line, then the sentence that sends the model to the `jobs` tool
    let note = reqs[2].messages.iter().map(texts).find(|t| t.contains("[background jobs ended]")).unwrap_or_default();
    let note = note[note.find("[background jobs ended]").unwrap_or(0)..].to_string();
    assert!(
        note.starts_with("[background jobs ended]\n- job 1 sub-agent \"look\" done (reported) after ")
            && note
                .ends_with(" lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id)."),
        "{note}"
    );
    assert!(!note.contains("bg report"), "its report is read with the tool, as before: {note}");
    r.stop().await;
}

#[tokio::test]
async fn a_message_while_a_sub_agent_runs_moves_it_to_the_background_and_its_report_comes_as_a_note() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow look"))),
            Reply::Events(text("answering you")),
            // idle again: the sub-agent ends and a turn starts with its note
            Reply::Events(text("noted the report")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    r.submit("hello meanwhile");
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background as job") && out.contains("the person wrote while it ran"), "{out}");
    // the main agent answered before the sub-agent ended; the message came in the same turn, after the results
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("answering"))));
    let reqs = client.main_requests();
    assert_eq!(reqs.len(), 2);
    let last = reqs[1].messages.last().unwrap();
    assert!(texts(last).contains("hello meanwhile"), "{last:?}");
    assert_eq!(client.sub_count(), 1, "it has not ended");
    // it ends while idle: a turn starts with its report inline
    release(d.path(), "go");
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TextDelta { text } if text.contains("noted"))));
    let reqs = client.main_requests();
    let note = texts(reqs[2].messages.last().unwrap());
    assert!(note.contains("background jobs ended") && note.contains("late report"), "{note}");
    assert!(!note.contains("`jobs` tool"), "the report is inline, no jobs read is needed: {note}");
    r.stop().await;
}

#[tokio::test]
async fn a_detached_sub_agent_that_ends_while_the_main_agent_works_joins_at_the_next_round() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow look"))),
            Reply::Events(call("m1", "wait", json!({"file": "go2"}))),
            Reply::Events(text("all in")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("late report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    r.submit("hello");
    // the main agent now waits in its own tool; the sub-agent ends meanwhile
    r.until(|e| matches!(e, AgentEvent::ToolStart { id, .. } if id == "m1")).await;
    release(d.path(), "go");
    r.until(all_jobs_ended).await;
    release(d.path(), "go2");
    r.turn().await;
    let reqs = client.main_requests();
    let last = reqs.last().unwrap();
    let all: String = last.messages.iter().map(texts).collect::<Vec<_>>().join("\n");
    assert!(all.contains("late report"), "the note joined the round: {all}");
    r.stop().await;
}

#[tokio::test]
async fn the_key_sends_it_to_the_background_and_says_so() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![Reply::Events(call("t1", "task", task("explore", "slow"))), Reply::Events(text("free"))],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("r"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    r.send(AgentCommand::Detach);
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background") && out.contains("the person sent it to the background"), "{out}");
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn two_attached_sub_agents_both_detach_on_one_message() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(two_calls(("t1", "task", task("explore", "a")), ("t2", "task", task("explore", "b")))),
            Reply::Events(text("free")),
        ],
        vec![
            Reply::Events(call("w1", "wait", json!({}))),
            Reply::Events(call("w2", "wait", json!({}))),
            Reply::Events(text("ra")),
            Reply::Events(text("rb")),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 2).await;
    r.submit("hey");
    let evs = r.turn().await;
    for id in ["t1", "t2"] {
        let out = done_of(&evs, id).unwrap();
        assert!(out.contains("Moved to the background"), "{id}: {out}");
    }
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn with_detach_on_message_off_the_message_waits_for_the_round() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("t1", "task", task("explore", "slow"))),
            Reply::Events(text("round over")),
            Reply::Events(text("now your message")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("the report"))],
    );
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.detach_on_message = false;
    let mut r = start(client.clone(), cfg, d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    r.submit("hello");
    // (a negative: nothing may happen while the message waits)
    assert!(r.quiet(300, |e| matches!(e, AgentEvent::ToolDone { .. })).await, "the call is still waiting");
    release(d.path(), "go");
    let evs = r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("the report"));
    r.stop().await;
}

#[tokio::test]
async fn a_key_with_nothing_attached_leaves_no_signal_for_a_sub_agent_that_starts_later() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            // the model is still writing its reply when the key is pressed; the sub-agent starts after it
            late(d.path(), "g1", call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.send(AgentCommand::Detach);
    r.sync().await;
    release(d.path(), "g1");
    until_tool(&client, 1).await;
    release(d.path(), "go");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "it stayed attached: the key had nothing to act on");
    r.stop().await;
}

#[tokio::test]
async fn a_message_in_a_round_detaches_a_sub_agent_that_starts_later_in_the_same_round() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![late(d.path(), "g1", call("t1", "task", task("explore", "look"))), Reply::Events(text("done"))],
        vec![Reply::Events(call("w1", "wait", json!({}))), Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "g1");
    let evs = r.turn().await;
    let out = done_of(&evs, "t1").unwrap();
    assert!(out.contains("Moved to the background") && out.contains("the person wrote while it ran"), "{out}");
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn a_message_does_not_detach_a_sub_agent_of_a_later_round() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(
        vec![
            Reply::Events(call("m1", "wait", json!({"file": "go2"}))),
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_tool(&client, 1).await;
    // nothing is attached: the message is heard at the end of this round
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "go2");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "the next round's sub-agent is waited for");
    r.stop().await;
}

#[tokio::test]
async fn a_signal_does_not_survive_a_round_that_went_on_without_hearing_the_inbox() {
    let d = test_tmpdir().unwrap();
    let reasoning_only = vec![
        StreamEvent::Start { response_id: None, model: None },
        StreamEvent::ReasoningDelta("hmm".into()),
        StreamEvent::Finish(FinishReason::Stop),
        StreamEvent::Done,
    ];
    let client = Scripted::new(
        vec![
            // a reply of only reasoning: the agent asks the model to go on, no tools ran, the inbox was not read
            late(d.path(), "g1", reasoning_only),
            Reply::Events(call("t1", "task", task("explore", "look"))),
            Reply::Events(text("done")),
        ],
        vec![Reply::Events(text("report"))],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("delegate");
    until_main(&client, 1).await;
    r.submit("hello");
    r.until(|e| matches!(e, AgentEvent::Queued { .. })).await;
    release(d.path(), "g1");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t1").as_deref(), Some("report"), "the message belonged to the round before");
    r.stop().await;
}

#[tokio::test]
async fn a_background_writer_of_any_profile_is_a_job_and_its_claim_keeps_others_out_until_it_ends() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            // the main agent's own write of the same path is refused (it waits until the holder holds)
            late(d.path(), "m1go", call("m1", "write", json!({"path": "a.txt"}))),
            // a second writer (background too)
            Reply::Events(call("t2", "task", bg("general", "also a.txt"))),
            // the main agent's own tool keeps the round open until the second writer has given up
            Reply::Events(call("m4", "wait", json!({"file": "m4go"}))),
            Reply::Events(text("waiting")),
            // the first one ended: its note starts a turn, then the path is free
            Reply::Events(call("m2", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("written")),
        ],
        vec![
            (
                "hold a.txt",
                vec![
                    Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true}))),
                    Reply::Events(text("first done")),
                ],
            ),
            (
                "also a.txt",
                vec![
                    Reply::Events(call("s2", "write", json!({"path": "a.txt"}))),
                    Reply::Events(text("second gave up")),
                ],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    let first = r.until(is_done("t1")).await;
    let out = done_of(&first, "t1").unwrap();
    assert!(out.contains("Started sub-agent job"), "{out}");
    until_tool(&client, 1).await;
    release(d.path(), "m1go");
    let evs = r.until_all(&[&is_done("m1"), &is_done("t2"), &job_ended(2)]).await;
    let m1 = done_of(&evs, "m1").unwrap();
    assert!(m1.contains("being changed by a sub-agent"), "{m1}");
    // the second writer was refused: its own model read who holds the file
    let refusal = result_for(&requests_of(&client, "also a.txt"), "s2").unwrap();
    assert!(
        refusal.contains("`a.txt` is being changed by another sub-agent running at the same time (hold a.txt)"),
        "{refusal}"
    );
    release(d.path(), "m4go");
    r.turn().await;
    // the holder ends: the path is free
    release(d.path(), "go");
    let evs = r.turn().await;
    let m2 = done_of(&evs, "m2").unwrap();
    assert!(m2.contains("wrote a.txt"), "free after the holder ended: {m2}");
    r.stop().await;
}
