use super::*;

async fn start_mail(client: Arc<Scripted>, cwd: &std::path::Path, mail: Arc<nexo_core::mailbox::Mailbox>) -> Running {
    data_dir();
    let (handle, ends) = channels();
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: cwd.to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client,
        resume: None,
        gate: None,
        mail: Some(mail),
        mcp: None,
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    Running { handle, task }
}

fn sessions(root: &std::path::Path) -> (Arc<nexo_core::mailbox::Mailbox>, Arc<nexo_core::mailbox::Mailbox>) {
    use nexo_core::mail::Inbound;
    use nexo_core::mailbox::Mailbox;
    let cwd = root.to_string_lossy().to_string();
    (
        Mailbox::open(root, "aaaa1111", "api", &cwd, false, Inbound::Auto).unwrap(),
        Mailbox::open(root, "bbbb2222", "web", &cwd, false, Inbound::Auto).unwrap(),
    )
}

#[tokio::test]
async fn a_mail_is_delivered_at_an_idle_point_as_a_marked_message_and_read_when_answered() {
    use nexo_core::mail::{FromKind, Stage};
    let d = test_tmpdir().unwrap();
    let root = test_tmpdir().unwrap();
    let (api, web) = sessions(root.path());
    let client = Scripted::new(vec![Reply::Events(text("on it"))]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    // a turn is running as far as the host knows: the mail arrives and waits
    web.set_idle(false);
    api.send(&web_id, "please run the parser tests", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert!(t.deliver.is_empty(), "never while busy");
    assert!(t.events.iter().any(|e| matches!(e, AgentEvent::MailArrived { from_name, .. } if from_name == "api")));
    assert!(client.requests().is_empty(), "the model heard nothing");
    // the agent says idle: the host takes the mail and hands it over
    web.set_idle(true);
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1);
    r.send(AgentCommand::Deliver { mails: t.deliver });
    let evs = r.turn().await;
    let pos = |f: &dyn Fn(&AgentEvent) -> bool| evs.iter().position(f).unwrap_or(usize::MAX);
    let delivered = pos(&|e| matches!(e, AgentEvent::MailDelivered { .. }));
    let started = pos(&|e| matches!(e, AgentEvent::TurnStart { .. }));
    let read = pos(&|e| matches!(e, AgentEvent::MailRead { .. }));
    assert!(delivered < started && started < read, "delivered, then the turn, then read: {evs:?}");
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let req = &client.requests()[0];
    let last = req.messages.last().unwrap().text();
    assert!(last.contains("please run the parser tests") && last.contains("api"), "{last}");
    // the sender sees the ladder
    let mut seen = Vec::new();
    for _ in 0..3 {
        for e in api.tick().events {
            if let AgentEvent::MailStage { stage, .. } = e {
                seen.push(stage);
            }
        }
    }
    assert!(seen.contains(&Stage::Read), "{seen:?}");
    let names: Vec<String> = std::fs::read_dir(root.path().join("aaaa1111/receipts"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(names.iter().any(|n| n.ends_with(".delivered")) && names.iter().any(|n| n.ends_with(".read")), "{names:?}");
    r.stop().await;
}

#[tokio::test]
async fn a_mail_handed_over_mid_turn_waits_for_the_turn_to_end() {
    use nexo_core::mail::FromKind;
    let d = test_tmpdir().unwrap();
    let root = test_tmpdir().unwrap();
    let (api, web) = sessions(root.path());
    let client =
        Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("part".into())]), Reply::Events(text("second"))]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    r.send(AgentCommand::Submit { text: "one".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    let m = api.send(&web_id, "late news", None, FromKind::Agent).unwrap();
    r.send(AgentCommand::Deliver { mails: vec![m] });
    // nothing reaches the model while the turn runs
    let quiet = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if let AgentEvent::MailDelivered { .. } = r.next().await {
                return;
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "no delivery mid-turn");
    assert_eq!(client.requests().len(), 1);
    r.send(AgentCommand::Interrupt);
    let evs = r.until(|e| matches!(e, AgentEvent::MailRead { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::MailDelivered { .. })));
    assert_eq!(client.requests().len(), 2);
    assert!(client.requests()[1].messages.last().unwrap().text().contains("late news"));
    r.stop().await;
}

#[tokio::test]
async fn the_agent_owns_the_idle_flag_a_fresh_session_takes_mail_and_a_running_turn_does_not() {
    use nexo_core::mail::FromKind;
    let d = test_tmpdir().unwrap();
    let root = test_tmpdir().unwrap();
    let (api, web) = sessions(root.path());
    let client = Scripted::new(vec![
        Reply::Events(text("first")),
        Reply::Hang(vec![StreamEvent::TextDelta("part".into())]),
        Reply::Events(text("third")),
    ]);
    let mut r = start_mail(client.clone(), d.path(), web.clone()).await;
    let AgentEvent::Ready { session_id: web_id, .. } = r.next().await else { panic!("no Ready") };
    // nobody told the mailbox anything: a session that just started is between turns
    api.send(&web_id, "news before any turn", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1, "a fresh session is idle");
    r.send(AgentCommand::Deliver { mails: t.deliver });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    // after the turn it is idle again, and mail is handed over without anyone else saying so
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { .. }) {
            break;
        }
    }
    api.send(&web_id, "news after the turn", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1, "idle again after a turn");
    // that mail starts a turn that hangs: while it runs, nothing is handed over
    r.send(AgentCommand::Deliver { mails: t.deliver });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    api.send(&web_id, "news during the turn", None, FromKind::Agent).unwrap();
    assert!(web.tick().deliver.is_empty(), "never while a turn runs");
    r.send(AgentCommand::Interrupt);
    r.until(|e| matches!(e, AgentEvent::TurnEnd { .. })).await;
    loop {
        if matches!(r.raw().await, AgentEvent::Idle { .. }) {
            break;
        }
    }
    assert_eq!(web.tick().deliver.len(), 1, "the mail that waited goes once the turn is over");
    r.stop().await;
}
