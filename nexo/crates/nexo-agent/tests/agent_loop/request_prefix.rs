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
