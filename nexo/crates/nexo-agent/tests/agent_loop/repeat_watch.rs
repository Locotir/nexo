use super::*;

#[tokio::test]
async fn reading_an_unchanged_file_again_sends_a_note() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("r1", "read", json!({"path": "a.txt"}))),
        Reply::Events(call("r2", "read", json!({"path": "a.txt"}))),
        Reply::Events(text("ok")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let results: Vec<String> = client.requests()[2]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => Some(nexo_core::message::result_text(content)),
            _ => None,
        })
        .collect();
    assert_eq!(results[0], "read:a.txt");
    assert!(results[1].contains("not repeated"), "{results:?}");
    r.stop().await;
}

const NOTICE: &str = "was called 3 times with the same arguments";

fn texts_of(req: &ModelRequest) -> String {
    req.messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n")
}

fn quiet(mut cfg: Config) -> Config {
    cfg.agent.loop_guard = 0;
    cfg.agent.stale_guard = 0;
    cfg
}

fn history_texts(evs: &[AgentEvent]) -> String {
    evs.iter()
        .filter_map(|e| if let AgentEvent::History { message, .. } = e { Some(message.text()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn three_equal_reads(cfg: Config) -> (Arc<Scripted>, Vec<AgentEvent>) {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c2", "read", json!({"path": "a.rs"}))),
        Reply::Events(call("c3", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("looked")),
        Reply::Events(text("second answer")),
    ]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "look".into(), attachments: vec![] });
    let mut evs = r.turn().await;
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    evs.extend(r.turn().await);
    r.stop().await;
    (client, evs)
}

#[tokio::test]
async fn a_repeat_is_a_hidden_note_on_the_next_request_only_and_never_in_the_transcript() {
    let (client, evs) = three_equal_reads(quiet(config(PermissionMode::Ask))).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "no side request");
    for (i, r) in reqs.iter().enumerate() {
        let has = texts_of(r).contains(NOTICE);
        assert_eq!(has, i == 3, "request {i}: the note rides on the one after the third call only");
    }
    assert!(!history_texts(&evs).contains(NOTICE), "not in the conversation");
    assert!(!history_texts(&evs).contains("system-reminder"));
}

#[tokio::test]
async fn the_watch_can_be_switched_off() {
    let mut cfg = quiet(config(PermissionMode::Ask));
    cfg.agent.watch = false;
    let (client, _) = three_equal_reads(cfg).await;
    assert!(client.requests().iter().all(|r| !texts_of(r).contains(NOTICE)));
}

async fn unfinished(replies: Vec<Reply>, cfg: Config) -> (Arc<Scripted>, Running, Vec<AgentEvent>, tempfile::TempDir) {
    let dir = test_tmpdir().unwrap();
    let mut all = vec![
        Reply::Events(call(
            "c1",
            "todo",
            json!({"items": [{"text": "write it", "status": "done"}, {"text": "test it"}]}),
        )),
        Reply::Events(text("all done")),
    ];
    all.extend(replies);
    let client = Scripted::new(all);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    let mut r = start_with(client.clone(), cfg, None, dir.path(), todo_tools).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    let evs = r.turn().await;
    (client, r, evs, dir)
}

#[tokio::test]
async fn a_check_in_that_says_continue_resumes_the_session_with_the_answer_as_hidden_context() {
    let (client, mut r, mut evs, _d) = unfinished(
        vec![Reply::Events(text("CONTINUE run the missing test")), Reply::Events(text("running it"))],
        config(PermissionMode::Ask),
    )
    .await;
    evs.extend(r.turn().await);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "the todo call, the reply, the check-in, the resumed turn");
    let ask = reqs[2].messages.last().unwrap();
    assert_eq!(ask.role, Role::User);
    assert!(ask.text().contains("Everything OK?") && ask.text().contains("1 open item"), "{}", ask.text());
    assert_eq!(reqs[2].messages[reqs[2].messages.len() - 2].text(), "all done", "after the reply it got");
    assert!(reqs[3].messages.iter().any(|m| m.text().contains("answer was CONTINUE: run the missing test")));
    let said = history_texts(&evs);
    assert!(!said.contains("run the missing test") && !said.contains("Everything OK?"), "{said}");
    assert!(
        evs.iter().any(|e| matches!(e, AgentEvent::History { message, .. } if message.text() == nexo_core::agent::SUPERVISOR_RESUME_MARK))
    );
    // the work is still open at the end of the resumed turn, but a request of the person gets one check-in
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 4, "no second check-in");
    r.stop().await;
}

#[tokio::test]
async fn a_check_in_that_says_done_changes_nothing() {
    let (client, mut r, evs, _d) =
        unfinished(vec![Reply::Events(text("DONE, the test is not needed"))], config(PermissionMode::Ask)).await;
    let more = r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 3);
    assert!(!more.iter().any(|e| matches!(e, AgentEvent::TurnStart { .. })), "no new turn");
    assert!(!history_texts(&evs).contains("Everything OK?"));
    r.stop().await;
}

#[tokio::test]
async fn the_check_in_can_be_switched_off() {
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_in = false;
    let (client, r, _, _d) = unfinished(vec![Reply::Events(text("CONTINUE go"))], cfg).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 2);
    r.stop().await;
}

#[tokio::test]
async fn the_supervisor_asks_for_a_check_in_once_per_stopped_turn() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("finished")), Reply::Events(text("DONE"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(client.requests().len(), 1, "nothing looks unfinished: no check-in on its own");
    r.send(AgentCommand::CheckIn { evidence: "the session asked a question and nobody answered".into() });
    r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    r.send(AgentCommand::CheckIn { evidence: "again".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "the second ask is let go");
    assert!(reqs[1].messages.last().unwrap().text().contains("nobody answered"));
    r.stop().await;
}
