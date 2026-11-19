//! A stop by the person holds the session: nothing starts by itself until they write again.

use super::*;

/// Fails when a turn starts within `ms`.
pub(super) async fn no_turn_within(r: &mut Running, ms: u64) {
    let hit = tokio::time::timeout(Duration::from_millis(ms), async {
        loop {
            if matches!(r.next().await, AgentEvent::TurnStart { .. }) {
                return;
            }
        }
    })
    .await;
    assert!(hit.is_err(), "a turn started by itself");
}

#[cfg(unix)]
pub(super) fn job_ended(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::Jobs { jobs } if jobs.iter().any(|j| j.id == 1 && !j.status.running()))
}

#[cfg(unix)]
pub(super) fn said(req: &ModelRequest) -> String {
    req.messages.iter().map(|m| m.text()).collect::<Vec<_>>().join("\n")
}

#[cfg(unix)]
#[tokio::test]
async fn a_job_that_ends_after_esc_starts_nothing_and_rides_the_next_request() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 1", "background": true}))),
        Reply::Hang(vec![StreamEvent::TextDelta("part".into())]),
        Reply::Events(text("ok")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    r.until(job_ended).await;
    no_turn_within(&mut r, 400).await;
    assert_eq!(client.requests().len(), 2, "the job's end did not start a request");
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(said(&reqs[2]).contains("[background jobs ended]"), "the person's request carries the note");
    r.stop().await;
}

#[tokio::test]
async fn a_check_in_asked_after_esc_makes_no_request() {
    let d = test_tmpdir().unwrap();
    // the request used a tool, so only the hold keeps the check-in back
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "a.rs"}))),
        Reply::Events(text("finished")),
        Reply::Events(text("DONE")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Interrupt);
    r.send(AgentCommand::CheckIn { evidence: "the session asked a question and nobody answered".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 2, "no check-in after a stop");
    r.stop().await;
}

#[tokio::test]
async fn mail_that_comes_after_esc_waits_for_the_person() {
    use nexo_core::mail::FromKind;
    let d = test_tmpdir().unwrap();
    let root = test_tmpdir().unwrap();
    let (api, web) = super::mail::sessions(root.path());
    let client = Scripted::new(vec![
        Reply::Hang(vec![StreamEvent::TextDelta("part".into())]),
        Reply::Events(text("second")),
        Reply::Events(text("third")),
    ]);
    let mut r = super::mail::start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Interrupt);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    let m = api.send(&web_id, "late news", None, FromKind::Agent).unwrap();
    r.send(AgentCommand::Deliver { mails: vec![m] });
    no_turn_within(&mut r, 400).await;
    assert_eq!(client.requests().len(), 1, "the mail started nothing");
    // it is still waiting: it goes after the person's next request
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::MailRead { .. })).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[2].messages.last().unwrap().text().contains("late news"));
    r.stop().await;
}

#[tokio::test]
async fn a_check_in_answered_continue_after_esc_starts_no_turn() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "c1",
            "todo",
            json!({"items": [{"text": "write it", "status": "done"}, {"text": "test it"}]}),
        )),
        Reply::Events(text("all done")),
    ]);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    let mut r = start_with(client.clone(), config(PermissionMode::Ask), None, dir.path(), todo_tools).await;
    // the person stops while the check-in is in flight; its CONTINUE comes after
    client.replies.lock().unwrap().push_back(Reply::Midway(
        r.handle.commands.clone(),
        Box::new(AgentCommand::Interrupt),
        text("CONTINUE run the test"),
    ));
    r.next().await;
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    r.turn().await;
    no_turn_within(&mut r, 800).await;
    assert_eq!(client.requests().len(), 3, "the todo call, the reply, the check-in: no resumed turn");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn once_the_person_writes_again_a_job_that_ends_starts_a_turn_as_before() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 1", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("saw it")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Interrupt);
    r.send(AgentCommand::Submit { text: "run it".into(), attachments: vec![] });
    r.turn().await;
    // the job's end starts a turn of its own
    let evs = r.turn().await;
    assert_eq!(text_of(&evs), "saw it");
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(said(&reqs[2]).contains("[background jobs ended]"));
    r.stop().await;
}

/// A first request that leaves a todo item open and gets its check-in (DONE), then `replies`.
async fn checked_once(replies: Vec<Reply>) -> (Arc<Scripted>, Running, tempfile::TempDir) {
    let (client, mut r, _, dir) = super::repeat_watch::unfinished(
        std::iter::once(Reply::Events(text("DONE, not needed"))).chain(replies).collect(),
        config(PermissionMode::Ask),
    )
    .await;
    r.until(|e| matches!(e, AgentEvent::Usage { .. })).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    (client, r, dir)
}

#[tokio::test]
async fn a_request_answered_with_words_only_gets_no_check_in_on_either_path() {
    let (client, mut r, _d) = checked_once(vec![Reply::Events(text("it means x")), Reply::Events(text("DONE"))]).await;
    assert_eq!(client.requests().len(), 3);
    r.send(AgentCommand::Submit { text: "what does that mean?".into(), attachments: vec![] });
    r.turn().await;
    // after the turn's end
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 4, "no check-in after the turn");
    // and when the supervisor asks
    r.send(AgentCommand::CheckIn { evidence: "older work is still open".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 4, "no check-in on request");
    r.stop().await;
}

#[tokio::test]
async fn a_request_that_used_tools_and_left_items_open_still_gets_its_one_check_in() {
    let (client, r, _d) = checked_once(vec![]).await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3, "the todo call, the reply, the check-in");
    assert!(reqs[2].messages.last().unwrap().text().contains("Everything OK?"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_resume_turn_with_a_block_in_front_of_its_mark_carries_no_tree_note() {
    let dir = test_tmpdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "x").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call(
            "c1",
            "todo",
            json!({"items": [{"text": "write it", "status": "done"}, {"text": "test it"}]}),
        )),
        Reply::Events(text("all done")),
        Reply::Events(text("running it")),
    ]);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    let mut r = start_with(client.clone(), config(PermissionMode::Ask), None, dir.path(), todo_tools).await;
    // a block that goes in front of the next request, queued while the check-in is in flight
    client.replies.lock().unwrap().insert(
        2,
        Reply::Midway(
            r.handle.commands.clone(),
            Box::new(AgentCommand::ChromeUse { id: "b1".into() }),
            text("CONTINUE run the test"),
        ),
    );
    r.next().await;
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    r.turn().await;
    // the folder changed since the model last saw it
    std::fs::write(dir.path().join("new.rs"), "").unwrap();
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4);
    let last = reqs[3].messages.iter().rev().find(|m| m.role == Role::User && m.text().contains("b1")).unwrap().text();
    assert!(last.contains(nexo_core::agent::SUPERVISOR_RESUME_MARK), "{last}");
    assert!(!last.contains("[project at the start of this request]"), "{last}");
    r.stop().await;
}

#[tokio::test]
async fn a_stop_typed_during_a_turn_gets_no_check_in() {
    let dir = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let todo_tools = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(dir.path().to_path_buf())));
    let mut r = start_with(client.clone(), config(PermissionMode::Ask), None, dir.path(), todo_tools).await;
    {
        // the person types "stop" while the first reply is on its way: it joins the turn after the todo call
        let mut replies = client.replies.lock().unwrap();
        replies.push_back(Reply::Midway(
            r.handle.commands.clone(),
            Box::new(AgentCommand::Submit { text: "stop".into(), attachments: vec![] }),
            call("c1", "todo", json!({"items": [{"text": "write it", "status": "done"}, {"text": "test it"}]})),
        ));
        replies.push_back(Reply::Events(text("OK, stopping")));
        replies.push_back(Reply::Events(text("CONTINUE")));
        replies.push_back(Reply::Events(text("resumed")));
    }
    r.next().await;
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    r.turn().await;
    no_turn_within(&mut r, 800).await;
    assert_eq!(client.requests().len(), 2, "the todo call and the answer to the stop: no check-in, no resumed turn");
    // nor when the supervisor asks: the model's words answered the person, there is no work of its own to check
    r.send(AgentCommand::CheckIn { evidence: "older work is still open".into() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 2, "no check-in on request either");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_job_that_ends_after_esc_and_a_rewind_starts_nothing() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 1", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("ok")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut r = start_with(client.clone(), config(PermissionMode::Auto), None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it".into(), attachments: vec![] });
    r.turn().await;
    // Esc, then Esc Esc to edit the message: the rewind keeps the stop
    r.send(AgentCommand::Interrupt);
    r.send(AgentCommand::Rewind { index: 0 });
    r.until(job_ended).await;
    no_turn_within(&mut r, 400).await;
    assert_eq!(client.requests().len(), 2, "the job's end did not start a request");
    r.send(AgentCommand::Submit { text: "next".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(said(&reqs[2]).contains("[background jobs ended]"), "the person's request carries the note");
    r.stop().await;
}

#[tokio::test]
async fn a_clear_after_esc_keeps_the_hold_so_mail_waits() {
    use nexo_core::mail::FromKind;
    let d = test_tmpdir().unwrap();
    let root = test_tmpdir().unwrap();
    let (api, web) = super::mail::sessions(root.path());
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut r = super::mail::start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.turn().await;
    // written while the old session is still the mailbox's: the clear moves the mailbox to the new one
    let m = api.send(&web_id, "late news", None, FromKind::Agent).unwrap();
    r.send(AgentCommand::Interrupt);
    r.send(AgentCommand::Clear);
    r.until(|e| matches!(e, AgentEvent::SessionChanged { .. })).await;
    r.send(AgentCommand::Deliver { mails: vec![m] });
    no_turn_within(&mut r, 400).await;
    assert_eq!(client.requests().len(), 1, "the mail started nothing after the clear");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_esc_as_the_turn_ends_done_keeps_the_queue_waiting() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![]);
    let marker = d.path().join("stopping");
    let mut cfg = config(PermissionMode::Auto);
    // the turn's last reply is in; the Stop hook keeps the turn open for a second
    cfg.hooks.push(hook_cfg("stop", None, &format!("touch '{}'; sleep 1", marker.display())));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    client.replies.lock().unwrap().push_back(Reply::Midway(
        r.handle.commands.clone(),
        Box::new(AgentCommand::Submit { text: "queued one".into(), attachments: vec![] }),
        text("done"),
    ));
    client.replies.lock().unwrap().push_back(Reply::Events(text("second")));
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    for _ in 0..250 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(marker.exists(), "the Stop hook ran");
    r.send(AgentCommand::Interrupt);
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done, "the turn was already past its last reply");
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("queued request(s) kept"))).await;
    no_turn_within(&mut r, 600).await;
    assert_eq!(client.requests().len(), 1, "the queued request did not run");
    // nor does a suggestion ask the model while the stop holds
    r.send(AgentCommand::Suggest);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.requests().len(), 1, "no suggestion request after a stop");
    r.send(AgentCommand::Submit { text: "now".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    // the request's own message: the kept one, then the new one, each on its own line under its time
    let sent = reqs[1].messages.last().map(|m| m.text()).unwrap_or_default();
    let (queued, now) = (sent.find("queued one"), sent.find("\nnow"));
    assert!(queued.is_some() && now.is_some() && queued < now, "the queued request goes first: {sent}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_held_session_reviews_no_running_job() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("b1", "bash", json!({"command": "sleep 3", "background": true}))),
        Reply::Events(text("started")),
        Reply::Events(text("CONTINUE")),
    ]);
    let set = tools().with(Arc::new(nexo_tools::bash::Bash));
    let mut cfg = config(PermissionMode::Auto);
    cfg.agent.job_review = 1;
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "run it".into(), attachments: vec![] });
    r.turn().await;
    // Esc while idle: the job runs on, and nothing asks the model about it until the person writes
    r.send(AgentCommand::Interrupt);
    r.until(job_ended).await;
    no_turn_within(&mut r, 500).await;
    assert_eq!(client.requests().len(), 2, "no review of the job, no turn for its end");
    r.stop().await;
}
