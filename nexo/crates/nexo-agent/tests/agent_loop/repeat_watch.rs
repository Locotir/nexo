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

pub(super) async fn unfinished(
    replies: Vec<Reply>,
    cfg: Config,
) -> (Arc<Scripted>, Running, Vec<AgentEvent>, tempfile::TempDir) {
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
    // a request with a tool call of its own: one with words only has nothing of its own to check
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("finished")),
        Reply::Events(text("DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    assert_eq!(client.requests().len(), 2, "nothing looks unfinished: no check-in on its own");
    r.send(AgentCommand::CheckIn { evidence: "the session asked a question and nobody answered".into() });
    r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    r.send(AgentCommand::CheckIn { evidence: "again".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3, "the second ask is let go");
    assert!(reqs[2].messages.last().unwrap().text().contains("nobody answered"));
    r.stop().await;
}

/// Every event that comes within `ms` (the agent is idle meanwhile, so nothing else is waited for).
async fn events_within(r: &mut Running, ms: u64) -> Vec<AgentEvent> {
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut out = Vec::new();
    while let Ok(ev) = tokio::time::timeout_at(end, r.next()).await {
        out.push(ev);
    }
    out
}

/// A session whose goal is met: one request with a `write` call, then the reply that says GOAL DONE. `more` are the
/// replies for whatever comes after.
async fn met_goal(more: Vec<Reply>, dir: &std::path::Path) -> (Arc<Scripted>, Running) {
    let mut all =
        vec![Reply::Events(call("c1", "write", json!({"path": "a.rs"}))), Reply::Events(text("all set\nGOAL DONE"))];
    all.extend(more);
    let client = Scripted::new(all);
    let mut r = start(client.clone(), config(PermissionMode::Auto), None, dir).await;
    r.until(|e| matches!(e, AgentEvent::ModelLimits { .. })).await;
    r.send(AgentCommand::SetGoal { text: Some("write a.rs".into()), attachments: vec![] });
    r.turn().await;
    r.until(|e| matches!(e, AgentEvent::Goal { state: GoalState::Done, .. })).await;
    (client, r)
}

#[tokio::test]
async fn a_supervisor_check_in_leaves_a_goal_alone() {
    let d = test_tmpdir().unwrap();
    // the last two replies are what a check-in would get, if one were sent
    let (client, r) =
        met_goal(vec![Reply::Events(text("CONTINUE keep going")), Reply::Events(text("on it"))], d.path()).await;
    assert_eq!(client.requests().len(), 2);
    r.send(AgentCommand::CheckIn { evidence: "tests still fail".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2, "the goal's own round is not checked on");
    let said: String = reqs.iter().flat_map(|q| q.messages.iter().map(|m| m.text())).collect::<Vec<_>>().join("\n");
    assert!(!said.contains(nexo_core::agent::SUPERVISOR_RESUME_MARK) && !said.contains("Everything OK?"), "{said}");
    r.stop().await;
}

#[tokio::test]
async fn a_met_goal_leaves_check_ins_for_later_requests() {
    let d = test_tmpdir().unwrap();
    // a later request of the person, with a tool call of its own and nothing unfinished in its reply
    let (client, mut r) = met_goal(
        vec![
            Reply::Events(call("c2", "write", json!({"path": "b.rs"}))),
            Reply::Events(text("b.rs is written")),
            Reply::Events(text("DONE")),
        ],
        d.path(),
    )
    .await;
    r.send(AgentCommand::Submit { text: "also write b.rs".into(), attachments: vec![] });
    r.turn().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 4, "nothing looks unfinished: no check-in on its own");
    r.send(AgentCommand::CheckIn { evidence: "tests still fail".into() });
    r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 5, "the check-in is sent: the goal is met and this request is not its round");
    assert!(reqs[4].messages.last().unwrap().text().contains("tests still fail"));
    r.stop().await;
}

#[tokio::test]
async fn a_supervisor_check_in_is_told_only_when_it_runs() {
    const TOLD: &str = "supervisor: checking in with this session";
    let told = |evs: &[AgentEvent]| notices(evs).iter().filter(|t| t.as_str() == TOLD).count();
    // no goal: the check-in runs, and the person is told once
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("finished")),
        Reply::Events(text("DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let turn = r.turn().await;
    assert_eq!(told(&turn), 0, "the automatic check-in found nothing to ask");
    r.send(AgentCommand::CheckIn { evidence: "the session asked a question and nobody answered".into() });
    let evs = events_within(&mut r, 300).await;
    assert_eq!(client.requests().len(), 3);
    assert_eq!(told(&evs), 1, "{:?}", notices(&evs));
    r.stop().await;
    // a met goal, in its own round: no check-in runs, so nothing is said
    let d = test_tmpdir().unwrap();
    let (client, mut r) = met_goal(vec![Reply::Events(text("DONE"))], d.path()).await;
    r.send(AgentCommand::CheckIn { evidence: "tests still fail".into() });
    let evs = events_within(&mut r, 300).await;
    assert_eq!(client.requests().len(), 2);
    assert_eq!(told(&evs), 0, "{:?}", notices(&evs));
    r.stop().await;
}
