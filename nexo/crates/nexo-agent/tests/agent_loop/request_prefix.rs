use super::*;

fn last_user_text(q: &ModelRequest) -> String {
    q.messages.iter().rev().find(|m| m.role == Role::User).map(|m| m.text()).unwrap_or_default()
}

/// What a session sends before its messages never changes while it lives, in the process or after a resume;
/// a file made between two messages reaches the model in the new message, once.
#[tokio::test]
async fn the_system_text_and_the_tools_stay_fixed_for_the_session_and_a_changed_tree_comes_in_the_new_message() {
    let d = test_tmpdir().unwrap();
    let with_diag = || tools().with(Arc::new(Fake { name: "diagnostics", kind: ToolKind::Read, slow: false }));
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    // process 1: the folder holds one file, so the Project block exists
    std::fs::write(d.path().join("README.md"), "x").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one")), Reply::Events(text("two"))]);
    let mut r = start_with(client.clone(), cfg.clone(), None, d.path(), with_diag()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    // the model's work made files
    std::fs::write(d.path().join("inv.py"), "print(1)\n").unwrap();
    std::fs::write(d.path().join("test_inv.py"), "").unwrap();
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].tools.iter().any(|t| t.name == "diagnostics"));
    assert_eq!(reqs[1].system, reqs[0].system, "same process: the system text is the same after a file was made");
    assert_eq!(serde_json::to_string(&reqs[1].tools).unwrap(), serde_json::to_string(&reqs[0].tools).unwrap());
    assert!(reqs[0].system.contains("Project (as when the session began"), "{}", reqs[0].system);
    assert!(!reqs[0].system.contains("inv.py"));
    let note = last_user_text(&reqs[1]);
    assert!(note.contains("[project at the start of this request]"), "{note}");
    assert!(note.contains("inv.py") && note.contains("test_inv.py"), "{note}");
    assert!(!last_user_text(&reqs[0]).contains("[project at the start"), "no change, no note");
    // the old messages are the same bytes
    assert_eq!(
        serde_json::to_string(&reqs[0].messages[0]).unwrap(),
        serde_json::to_string(&reqs[1].messages[0]).unwrap()
    );
    // process 2 (`-c`): the folder holds Python files now and the process does not offer `diagnostics` of itself
    let client = Scripted::new(vec![Reply::Events(text("three")), Reply::Events(text("four"))]);
    let mut r = start_with(client.clone(), cfg.clone(), Some(id.clone()), d.path(), tools()).await;
    r.next().await;
    std::fs::write(d.path().join("inv.json"), "{}").unwrap();
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "fourth".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let later = client.requests();
    assert_eq!(later[0].system, reqs[0].system, "after a resume: the system text is the one the session began with");
    let names = |q: &ModelRequest| q.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&later[0]), names(&reqs[0]), "after a resume: the same tools");
    assert!(later[0].tools.iter().any(|t| t.name == "diagnostics"));
    let note = last_user_text(&later[0]);
    assert!(note.contains("inv.json"), "{note}");
    assert!(!note.contains("test_inv.py") || note.contains("Project (changed"), "{note}");
    assert!(!last_user_text(&later[1]).contains("[project at the start"), "nothing changed since: no note");
    assert_eq!(later[1].system, later[0].system);
    // process 3: the stored set has no `diagnostics` where it was started without it; a new process that has it drops it
    let client = Scripted::new(vec![Reply::Events(text("five"))]);
    let mut r = start_with(client.clone(), cfg.clone(), None, d.path(), tools()).await;
    let id2 = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "five".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let client2 = Scripted::new(vec![Reply::Events(text("six"))]);
    let mut r = start_with(client2.clone(), cfg, Some(id2), d.path(), with_diag()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "six".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert!(!client2.requests()[0].tools.iter().any(|t| t.name == "diagnostics"));
    assert_eq!(names(&client2.requests()[0]), names(&client.requests()[0]));
}

/// A session file saved before the prefix existed loads, gets one built and saved, and keeps it after.
#[tokio::test]
async fn a_session_saved_without_a_prefix_gets_one_once() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // an old file: the same lines without the prefix one
    let path = nexo_agent::session::dir().join(format!("{id}.jsonl"));
    let kept: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("{\"type\":\"prefix\""))
        .map(String::from)
        .collect();
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();
    assert!(nexo_agent::session::Session::open_path(&path).unwrap().prefix().is_none());
    std::fs::write(d.path().join("late.txt"), "x").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("two")), Reply::Events(text("three"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let saved = nexo_agent::session::Session::open_path(&path).unwrap();
    let p = saved.prefix().expect("built and saved on resume");
    assert!(p.volatile.environment.contains("late.txt"));
    std::fs::write(d.path().join("later.txt"), "x").unwrap();
    let mut r = start(client.clone(), config(PermissionMode::Ask), Some(id), d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    assert_eq!(reqs[1].system, reqs[0].system, "the prefix built on the first resume is kept on the next");
    assert!(last_user_text(&reqs[1]).contains("later.txt"));
}

/// Instruction files that changed while a session was closed reach it on resume; unchanged, the
/// system text is the same bytes as before.
#[tokio::test]
async fn a_resumed_session_gets_its_instruction_files_as_they_are_now() {
    let d = test_tmpdir().unwrap();
    std::fs::write(d.path().join("AGENTS.md"), "Use tabs.").unwrap();
    let client = Scripted::new(vec![Reply::Events(text("one"))]);
    let mut r = start(client.clone(), config(PermissionMode::Ask), None, d.path()).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        _ => panic!(),
    };
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    std::fs::write(d.path().join("AGENTS.md"), "Use spaces.").unwrap();
    let later = Scripted::new(vec![Reply::Events(text("two")), Reply::Events(text("three"))]);
    for (n, reply) in ["second", "third"].into_iter().enumerate() {
        let mut r = start(later.clone(), config(PermissionMode::Ask), Some(id.clone()), d.path()).await;
        r.next().await;
        r.send(AgentCommand::Submit { text: reply.into(), attachments: vec![] });
        r.turn().await;
        r.stop().await;
        let sys = &later.requests()[n].system;
        assert!(sys.contains("Use spaces.") && !sys.contains("Use tabs."), "{sys}");
    }
    let (first, reqs) = (client.requests(), later.requests());
    assert!(first[0].system.contains("Use tabs."));
    assert_eq!(reqs[1].system, reqs[0].system, "nothing changed since: the same bytes");
}

// ---- each session remembers its provider and model

/// Check a real tool round and a following user turn, not just two isolated prompts.
/// Reported usage is simulated: a stable prefix does not guarantee a provider cache hit.
#[tokio::test]
async fn tool_round_and_next_turn_preserve_prefix_and_account_for_cache_usage() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    let client = Scripted::new(vec![
        Reply::Events(call("read-1", "read", json!({"path": "src/main.rs"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("done")),
    ]);
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "inspect the file".into(), attachments: vec![] });
    let first = r.turn().await;
    assert_eq!(end_of(&first), TurnEnd::Done);
    r.send(AgentCommand::Submit { text: "summarise it".into(), attachments: vec![] });
    let second = r.turn().await;
    assert_eq!(end_of(&second), TurnEnd::Done);
    r.stop().await;

    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    for pair in reqs.windows(2) {
        let (before, after) = (&pair[0], &pair[1]);
        assert_eq!(before.system, after.system, "system instructions changed");
        assert_eq!(before.tools, after.tools, "tool schemas or their order changed");
        assert_eq!(
            (before.model.as_str(), before.cache, &before.session_id),
            (after.model.as_str(), after.cache, &after.session_id)
        );
        assert!(after.messages.len() > before.messages.len());
        assert_eq!(&after.messages[..before.messages.len()], &before.messages[..], "previous messages changed");
    }
    assert!(reqs[2].messages.iter().any(|m| m.content.iter().any(|p| matches!(p, Part::ToolResult { .. }))));
    let usage = second
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Usage { session, .. } => Some(*session),
            _ => None,
        })
        .expect("session usage");
    // First tool call: 10 uncached; the two scripted answers: 1000 uncached + 2000 cached each.
    assert_eq!((usage.input, usage.cache_read, usage.cache_write), (2010, 4000, 0));
    assert_eq!(usage.total_input(), 6010);
    assert!((usage.cache_read as f64 / usage.total_input() as f64 - 4000.0 / 6010.0).abs() < 1e-12);
}

fn with_todo(d: &std::path::Path) -> nexo_core::ToolSet {
    tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(d.to_path_buf())))
}

/// The task list changes between two requests, and what the cache holds in front of the conversation does not:
/// the list rides with each request in its trailing text part, never in the system text.
#[tokio::test]
async fn a_task_list_change_leaves_the_system_text_alone_and_the_list_rides_with_the_request() {
    let d = test_tmpdir().unwrap();
    let items = json!({"items": [{"text": "read the code", "status": "done"}, {"text": "fix the bug", "status": "in_progress"}]});
    let client = Scripted::new(vec![
        Reply::Events(text("one")),
        Reply::Events(call("c1", "todo", items)),
        Reply::Events(text("two")),
        Reply::Events(text("three")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    let mut r = start_with(client.clone(), cfg, None, d.path(), with_todo(d.path())).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "first".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "second".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Submit { text: "third".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let seen: Vec<String> = reqs
        .iter()
        .map(|q| {
            let last: String = last_user_text(q).chars().take(120).collect();
            format!(
                "system {} chars, {} messages, max_tokens {}, last user text {last:?}",
                q.system.len(),
                q.messages.len(),
                q.max_tokens
            )
        })
        .collect();
    assert_eq!(reqs.len(), 4, "the requests were:\n{}", seen.join("\n"));
    for q in &reqs[1..] {
        assert_eq!(q.system, reqs[0].system, "a todo call does not change the system text");
    }
    assert!(!reqs[0].system.contains("todo-list"));
    // before the call there is no list; the request after it, inside the same turn, and the next turn's carry it
    assert!(!last_user_text(&reqs[0]).contains("<todo-list>") && !last_user_text(&reqs[1]).contains("<todo-list>"));
    for q in &reqs[2..] {
        let note = last_user_text(q);
        assert!(note.contains("<todo-list>\n[x] read the code\n[>] fix the bug\n1/2 done\n</todo-list>"), "{note}");
    }
    // it is not kept in the conversation: the saved messages of the earlier requests stay the same bytes
    assert_eq!(
        serde_json::to_string(&reqs[3].messages[0]).unwrap(),
        serde_json::to_string(&reqs[0].messages[0]).unwrap()
    );
    assert!(!reqs[3].messages[..reqs[3].messages.len() - 1].iter().any(|m| m.text().contains("<todo-list>")));
}

/// A resumed session has its saved list on its first request, and one that was left does not leak into the next.
#[tokio::test]
async fn a_resumed_session_carries_its_saved_task_list_and_not_the_one_left_behind() {
    let d = test_tmpdir().unwrap();
    // session A, with no list
    let client = Scripted::new(vec![Reply::Events(text("a"))]);
    let mut r = start_with(client, config(PermissionMode::Ask), None, d.path(), with_todo(d.path())).await;
    let a = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::Submit { text: "plain".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // session B makes a list
    let items = json!({"items": [{"text": "step one"}]});
    let client = Scripted::new(vec![Reply::Events(call("c1", "todo", items)), Reply::Events(text("b"))]);
    let mut r = start_with(client, config(PermissionMode::Ask), None, d.path(), with_todo(d.path())).await;
    let b = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    r.send(AgentCommand::Submit { text: "plan".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    // a new process resumes B: its first request carries the list
    let client = Scripted::new(vec![Reply::Events(text("again")), Reply::Events(text("on A"))]);
    let mut r = start_with(client.clone(), config(PermissionMode::Ask), Some(b), d.path(), with_todo(d.path())).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    r.turn().await;
    assert!(
        last_user_text(&client.requests()[0]).contains("[ ] step one"),
        "{}",
        last_user_text(&client.requests()[0])
    );
    // and moving to A in the same process leaves B's list behind
    r.send(AgentCommand::Resume { id: a.clone() });
    r.until(|e| matches!(e, AgentEvent::SessionChanged { session_id, .. } if *session_id == a)).await;
    r.send(AgentCommand::Submit { text: "now A".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    assert!(!last_user_text(&client.requests()[1]).contains("step one"), "{}", last_user_text(&client.requests()[1]));
}

/// The list the model carries is the one its messages left: a fork cut after the first list has that list and not
/// the later one, and one cut before any list has none.
#[tokio::test]
async fn a_shorter_fork_carries_the_task_list_of_the_messages_it_kept() {
    let d = test_tmpdir().unwrap();
    let a = json!({"items": [{"text": "first plan"}]});
    let b = json!({"items": [{"text": "second plan", "status": "done"}]});
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "todo", a)),
        Reply::Events(text("two")),
        Reply::Events(call("c2", "todo", b)),
        Reply::Events(text("four")),
        Reply::Events(text("in the first fork")),
        Reply::Events(text("in the second fork")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    let mut r = start_with(client.clone(), cfg, None, d.path(), with_todo(d.path())).await;
    r.next().await;
    for t in ["plan", "replan"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        r.turn().await;
    }
    // history: plan, call, result, two, replan, call, result, four
    r.send(AgentCommand::Fork { index: Some(4) });
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("forked"))).await;
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Fork { index: Some(1) });
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("forked"))).await;
    r.send(AgentCommand::Submit { text: "again".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let note = last_user_text(&reqs[4]);
    assert!(note.contains("[ ] first plan") && !note.contains("second plan"), "{note}");
    assert!(!last_user_text(&reqs[5]).contains("<todo-list>"), "{}", last_user_text(&reqs[5]));
}

/// A rewind puts the list back to what the kept messages say, as the panel shows it, and the saved one with it.
#[tokio::test]
async fn a_rewind_takes_the_task_list_back_to_the_one_the_kept_messages_left() {
    let d = test_tmpdir().unwrap();
    let a = json!({"items": [{"text": "first plan"}]});
    let b = json!({"items": [{"text": "second plan", "status": "done"}]});
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "todo", a)),
        Reply::Events(text("two")),
        Reply::Events(call("c2", "todo", b)),
        Reply::Events(text("four")),
        Reply::Events(text("after the first rewind")),
        Reply::Events(text("after the second rewind")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.agent.check_at_start = false;
    // the list is kept where the session is, as it is outside a test
    let set = tools().with(Arc::new(nexo_tools::todo::Todo::with_dir(nexo_agent::session::dir())));
    let mut r = start_with(client.clone(), cfg, None, d.path(), set).await;
    let id = match r.next().await {
        AgentEvent::Ready { session_id, .. } => session_id,
        other => panic!("{other:?}"),
    };
    for t in ["plan", "replan"] {
        r.send(AgentCommand::Submit { text: t.into(), attachments: vec![] });
        r.turn().await;
    }
    r.send(AgentCommand::Rewind { index: 4 });
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("rewound"))).await;
    let saved = nexo_tools::todo::file_in(&nexo_agent::session::dir(), &id);
    assert!(std::fs::read_to_string(&saved).unwrap().contains("first plan"));
    r.send(AgentCommand::Submit { text: "go on".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Rewind { index: 0 });
    r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("rewound"))).await;
    r.send(AgentCommand::Submit { text: "from scratch".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let reqs = client.requests();
    let note = last_user_text(&reqs[4]);
    assert!(note.contains("[ ] first plan") && !note.contains("second plan"), "{note}");
    assert!(!last_user_text(&reqs[5]).contains("<todo-list>"), "{}", last_user_text(&reqs[5]));
    assert!(!saved.exists(), "no list is left saved for a session that has none");
}
