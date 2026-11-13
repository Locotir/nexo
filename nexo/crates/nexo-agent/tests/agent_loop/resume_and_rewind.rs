use super::*;

#[tokio::test]
async fn every_request_of_a_session_carries_its_id_also_after_a_resume() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs.iter().all(|q| q.session_id.as_deref() == Some(id.as_str())), "one id for the whole session");
    let client = Scripted::new(vec![Reply::Events(text("again"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert_eq!(client.requests()[0].session_id.as_deref(), Some(id.as_str()), "the same after a resume");
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let other = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert_ne!(other, id);
    assert_eq!(client.requests()[0].session_id.as_deref(), Some(other.as_str()), "another session, another id");
}

#[tokio::test]
async fn a_session_is_resumed_with_its_history_and_tool_outputs() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let metas = nexo_agent::session::list();
    let meta = metas.iter().find(|m| m.id == id).unwrap();
    assert_eq!(meta.title, "please read f");
    assert_eq!(meta.messages, 4);
    let client = Scripted::new(vec![Reply::Events(text("again"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    assert!(matches!(r.next().await, AgentEvent::Ready { session_id, .. } if session_id == id));
    let evs = r.until(|e| matches!(e, AgentEvent::Context { .. })).await;
    assert!(
        evs.iter()
            .any(|e| matches!(e, AgentEvent::Context { messages_total: 4, history_tokens, .. } if *history_tokens > 0)),
        "a resumed session shows its context before the first call"
    );
    let hist: Vec<usize> = evs
        .iter()
        .filter_map(|e| if let AgentEvent::History { index, .. } = e { Some(*index) } else { None })
        .collect();
    assert_eq!(hist, vec![0, 1, 2, 3]);
    assert!(evs.iter().any(
        |e| matches!(e, AgentEvent::ToolDone { id, output, elapsed_ms: 0 } if id == "c1" && output.content == "read:f")
    ));
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 4, .. }));
    assert_eq!(client.requests()[0].messages.len(), 5);
    r.stop().await;
}

fn history_indexes(evs: &[AgentEvent]) -> Vec<usize> {
    evs.iter().filter_map(|e| if let AgentEvent::History { index, .. } = e { Some(*index) } else { None }).collect()
}

#[tokio::test]
async fn resume_in_place_replaces_the_conversation_and_its_file() {
    let d = test_tmpdir().unwrap();
    // session A: one request with a call
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "read", json!({"path": "f"}))), Reply::Events(text("read it"))]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // session B, fresh, then resume A in place
    let client = Scripted::new(vec![Reply::Events(text("b")), Reply::Events(text("c"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    assert_ne!(a, b);
    r.send(AgentCommand::Submit { text: "in b".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Resume { id: "nope".into() });
    assert!(
        matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("Could not resume session nope"))
    );
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Context { .. })).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Context { messages_total: 4, policy, .. }) if policy == "plain"));
    assert!(matches!(evs[0], AgentEvent::HistoryCleared));
    assert!(
        matches!(&evs[1], AgentEvent::SessionChanged { session_id, title } if *session_id == a && title == "please read f")
    );
    assert_eq!(history_indexes(&evs), vec![0, 1, 2, 3]);
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. } if id == "c1" && output.content == "read:f")));
    // the next request goes to A: index 4, and the model sees the five messages
    r.send(AgentCommand::Submit { text: "more".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 4, .. }));
    assert_eq!(client.requests()[1].messages.len(), 5);
    r.stop().await;
    let metas = nexo_agent::session::list();
    assert_eq!(metas.iter().find(|m| m.id == a).unwrap().messages, 6);
    assert_eq!(metas.iter().find(|m| m.id == b).unwrap().messages, 2, "B kept what it had");
}

#[tokio::test]
async fn resume_and_rewind_wait_for_the_turn() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Hang(vec![StreamEvent::TextDelta("half".into())])]);
    let mut r = start(client, config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::TextDelta { .. })).await;
    r.send(AgentCommand::Resume { id });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("before resuming")));
    r.send(AgentCommand::Rewind { index: 0 });
    assert!(matches!(r.next().await, AgentEvent::Notice { text, .. } if text.contains("before rewinding")));
    r.send(AgentCommand::Interrupt);
    r.turn().await;
    r.stop().await;
}

#[tokio::test]
async fn a_session_another_nexo_holds_is_not_resumed() {
    let d = test_tmpdir().unwrap();
    let mut r = start(Scripted::new(vec![Reply::Events(text("a"))]), config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let file = nexo_agent::session::dir().join(format!("{a}.jsonl"));
    let lock = file.with_extension("jsonl.lock");
    std::fs::write(&lock, "1").unwrap(); // another nexo, still running, holds it
    let before = std::fs::read(&file).unwrap();
    // `/resume` from a nexo already running
    let client = Scripted::new(vec![]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::HistoryCleared | AgentEvent::SessionChanged { .. })));
    assert!(matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("in use")), "{evs:?}");
    r.stop().await;
    assert_ne!(a, b);
    // `--resume` at startup
    let r = start(client.clone(), config(PermissionMode::Ask), Some(a.clone()), d.path()).await;
    let e = r.task.await.unwrap().unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    assert_eq!(std::fs::read(&file).unwrap(), before, "nothing written to the file another nexo holds");
    assert!(client.requests().is_empty(), "nothing spent");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1", "its lock stays");
    std::fs::remove_file(&lock).unwrap();
}

#[tokio::test]
async fn a_session_taken_over_is_opened_again_by_resume() {
    let d = test_tmpdir().unwrap();
    let mut r = start(
        Scripted::new(vec![Reply::Events(text("a")), Reply::Events(text("b"))]),
        config(PermissionMode::Ask),
        None,
        d.path(),
    )
    .await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    let lock = nexo_agent::session::dir().join(format!("{a}.jsonl.lock"));
    // another nexo takes it over: the next write finds the lock is not this one's
    std::fs::write(&lock, "1").unwrap();
    r.send(AgentCommand::Submit { text: "lost".into(), attachments: vec![] });
    r.turn().await;
    // it ends and lets go; the person opens the session again
    std::fs::remove_file(&lock).unwrap();
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. } | AgentEvent::SessionChanged { .. })).await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::SessionChanged { .. })), "{evs:?}");
    assert!(lock.is_file(), "the session holds its lock again");
    r.stop().await;
}

#[tokio::test]
async fn resuming_the_session_in_use_keeps_its_lock() {
    let d = test_tmpdir().unwrap();
    let mut r = start(Scripted::new(vec![Reply::Events(text("a"))]), config(PermissionMode::Ask), None, d.path()).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "in a".into(), attachments: vec![] });
    r.turn().await;
    let lock = nexo_agent::session::dir().join(format!("{a}.jsonl.lock"));
    assert!(lock.is_file());
    r.send(AgentCommand::Resume { id: a.clone() });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. } | AgentEvent::HistoryCleared)).await;
    assert!(lock.is_file(), "the session in use stays locked");
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::HistoryCleared)), "{evs:?}");
    r.stop().await;
}

#[tokio::test]
async fn rewind_cuts_before_the_calls_and_rewrites_the_file() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "read", json!({"path": "f"}))),
        Reply::Events(text("read it")),
        Reply::Events(text("after")),
    ]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "please read f".into(), attachments: vec![] });
    r.turn().await; // history: user, assistant+call, results, assistant text
                    // index 2 lands between the call and its result: the cut moves before the call
    r.send(AgentCommand::Rewind { index: 2 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(matches!(evs[0], AgentEvent::HistoryCleared));
    assert!(matches!(&evs[1], AgentEvent::SessionChanged { session_id, .. } if *session_id == id));
    assert_eq!(history_indexes(&evs), vec![0]);
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { .. })));
    assert!(
        matches!(evs.last().unwrap(), AgentEvent::Notice { text, .. } if text.contains("3 messages dropped, 1 kept"))
    );
    let on_disk = nexo_agent::Session::open(&id).unwrap();
    assert_eq!(on_disk.messages().len(), 1);
    assert!(on_disk.outputs().is_empty());
    assert_eq!(on_disk.meta().title, "please read f");
    assert!(on_disk.usage().input > 0, "spent tokens stay");
    // a second handle on the file would stop the next rewrite on Windows
    drop(on_disk);
    // the next request continues from the kept message
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    let evs = r.turn().await;
    let evs: Vec<AgentEvent> = evs
        .into_iter()
        .filter(|e| {
            !matches!(
                e,
                AgentEvent::ModelLimits { .. }
                    | AgentEvent::Effort { .. }
                    | AgentEvent::ContextMap { .. }
                    | AgentEvent::Curated { .. }
            )
        })
        .collect();
    assert!(matches!(evs[0], AgentEvent::History { index: 1, .. }));
    assert_eq!(client.requests()[2].messages.len(), 2);
    // an index past the end keeps everything; 0 drops everything
    r.send(AgentCommand::Rewind { index: 100 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert_eq!(history_indexes(&evs), vec![0, 1, 2]);
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { .. })).await;
    assert!(history_indexes(&evs).is_empty());
    assert_eq!(nexo_agent::Session::open(&id).unwrap().messages().len(), 0);
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_puts_back_the_files_the_agent_wrote() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt"}))),
        Reply::Events(call("c2", "write", json!({"path": "new.txt"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "change them".into(), attachments: vec![] });
    r.turn().await;
    // what the (fake) tools would have done
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    std::fs::write(d.path().join("new.txt"), "created").unwrap();
    r.send(AgentCommand::Rewind { index: 0 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(text.contains("2 files put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "old");
    assert!(!d.path().join("new.txt").exists(), "a file the agent created is removed");
    r.stop().await;
}

#[tokio::test]
async fn a_rewind_after_a_compaction_keeps_the_writes_the_summary_holds() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("a.txt"), "old").unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt"}))),
        Reply::Events(text("done")),
        Reply::Events(text("SUMMARY")),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.permissions.allow.push("write".into());
    let mut r = start(client, cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "change it".into(), attachments: vec![] });
    r.turn().await;
    std::fs::write(d.path().join("a.txt"), "changed").unwrap();
    r.send(AgentCommand::Compact);
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("history compacted"))).await;
    r.send(AgentCommand::Submit { text: "something else".into(), attachments: vec![] });
    r.turn().await;
    // drop only the last request: the write before the compaction stays
    r.send(AgentCommand::Rewind { index: 1 });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.starts_with("rewound"))).await;
    let AgentEvent::Notice { text, .. } = evs.last().unwrap() else { unreachable!() };
    assert!(!text.contains("put back"), "{text}");
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "changed");
    r.stop().await;
}
