use super::*;

#[tokio::test]
async fn a_second_sub_agent_writes_the_path_after_the_holder_ended_normally() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // the holder's note starts this turn
            Reply::Events(call("t2", "task", task("general", "second writer"))),
            Reply::Events(text("both done")),
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
                "second writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("second done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    release(d.path(), "go");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("second done"));
    assert_eq!(result_for(&requests_of(&client, "second writer"), "s2").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn killing_the_holder_frees_its_file_for_the_main_agent_and_for_a_new_sub_agent() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // the next turn: the write waits until the test says the kill is over
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(call("t2", "task", task("general", "new writer"))),
            Reply::Events(text("all written")),
        ],
        vec![
            ("hold a.txt", vec![Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true})))]),
            (
                "new writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("new done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    r.send(AgentCommand::KillJob { id: Some(1) });
    r.until(job_ended(1)).await;
    // let the stopped run finish its own cleanup (a current-thread runtime runs it when this task yields)
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    r.submit("again");
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert!(done_of(&evs, "m1").unwrap().contains("wrote a.txt"), "{:?}", done_of(&evs, "m1"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("new done"));
    assert_eq!(result_for(&requests_of(&client, "new writer"), "s2").as_deref(), Some("wrote a.txt"));
    r.stop().await;
}

#[tokio::test]
async fn clear_frees_the_file_of_a_background_writer_too() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold a.txt"))),
            Reply::Events(text("started")),
            // after /clear, in the new conversation
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(call("t2", "task", task("general", "new writer"))),
            Reply::Events(text("all written")),
        ],
        vec![
            ("hold a.txt", vec![Reply::Events(call("s1", "write", json!({"path": "a.txt", "hold": true})))]),
            (
                "new writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("new done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    r.turn().await;
    until_tool(&client, 1).await;
    r.send(AgentCommand::Clear);
    r.until(job_ended(1)).await;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    r.submit("again");
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert!(done_of(&evs, "m1").unwrap().contains("wrote a.txt"), "{:?}", done_of(&evs, "m1"));
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("new done"));
    r.stop().await;
}

#[tokio::test]
async fn an_edit_of_several_files_claims_each_and_a_refused_call_gives_back_what_it_took() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("t1", "task", bg("general", "hold files"))),
            late(d.path(), "g2", call("t2", "task", task("general", "take b"))),
            late(d.path(), "g3", call("t3", "task", task("general", "take c"))),
            Reply::Events(text("done")),
        ],
        vec![
            (
                "hold files",
                vec![
                    Reply::Events(call(
                        "s1",
                        "write",
                        json!({"files": [{"path": "a.txt"}, {"path": "b.txt"}], "hold": true}),
                    )),
                    Reply::Events(text("held")),
                ],
            ),
            (
                "take b",
                vec![
                    // c.txt is free, b.txt is the holder's: the whole call is refused and c.txt is not kept
                    Reply::Events(call("s2", "write", json!({"paths": ["c.txt", "b.txt"]}))),
                    Reply::Events(text("gave up")),
                ],
            ),
            (
                "take c",
                vec![Reply::Events(call("s3", "write", json!({"path": "c.txt"}))), Reply::Events(text("c done"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    until_tool(&client, 1).await;
    release(d.path(), "g2");
    let evs = r.until(is_done("t2")).await;
    assert_eq!(done_of(&evs, "t2").as_deref(), Some("gave up"));
    let refusal = result_for(&requests_of(&client, "take b"), "s2").unwrap();
    assert!(
        refusal.contains("`b.txt` is being changed by another sub-agent running at the same time (hold files)"),
        "{refusal}"
    );
    release(d.path(), "g3");
    let evs = r.turn().await;
    assert_eq!(done_of(&evs, "t3").as_deref(), Some("c done"));
    assert_eq!(result_for(&requests_of(&client, "take c"), "s3").as_deref(), Some("wrote c.txt"));
    release(d.path(), "go");
    r.stop().await;
}

#[tokio::test]
async fn a_call_id_that_comes_again_does_not_share_the_claims_of_the_first_writer() {
    let d = test_tmpdir().unwrap();
    // a provider that sends no ids makes `call_0` in every turn
    let client = Scripted::with_subs(
        vec![
            Reply::Events(call("call_0", "task", bg("general", "hold a.txt"))),
            late(d.path(), "g2", call("call_0", "task", task("general", "second writer"))),
            late(d.path(), "g3", call("m1", "write", json!({"path": "a.txt"}))),
            Reply::Events(text("done")),
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
                "second writer",
                vec![Reply::Events(call("s2", "write", json!({"path": "a.txt"}))), Reply::Events(text("gave up"))],
            ),
        ],
    );
    let mut r = start(client.clone(), config(PermissionMode::Auto), d.path()).await;
    r.submit("go");
    until_tool(&client, 1).await;
    release(d.path(), "g2");
    // the holder's request, the second writer's two
    until_sub(&client, 3).await;
    release(d.path(), "g3");
    let evs = r.turn().await;
    let refusal = result_for(&requests_of(&client, "second writer"), "s2").unwrap();
    assert!(refusal.contains("is being changed by another sub-agent running at the same time"), "{refusal}");
    // and the second writer's end did not free the holder's file
    let m1 = done_of(&evs, "m1").unwrap();
    assert!(m1.contains("being changed by a sub-agent"), "{m1}");
    release(d.path(), "go");
    r.stop().await;
}
