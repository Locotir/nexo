use super::*;

#[cfg(unix)]
#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_blocks_a_call_and_the_model_reads_why() {
    let d = test_tmpdir().unwrap();
    let client =
        Scripted::new(vec![Reply::Events(call("c1", "write", json!({"path": "x"}))), Reply::Events(text("ok"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(nexo_core::config::HookConfig {
        event: "pre_tool".into(),
        matcher: Some("write|edit".into()),
        command: "echo 'writes are frozen today' >&2; exit 2".into(),
        ..Default::default()
    });
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "write it".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolDone { output, .. } if output.is_error && output.content.contains("writes are frozen today"))));
    let second = &client.requests()[1];
    let result = second.messages.iter().flat_map(|m| m.content.iter()).find_map(|p| match p {
        Part::ToolResult { content, .. } => content.first().cloned(),
        _ => None,
    });
    assert!(matches!(result, Some(Part::Text { text }) if text.contains("Blocked by a hook: writes are frozen today")));
    r.stop().await;
}

/// The text of the tool results the model got in a request.
#[cfg(unix)]
fn results_in(req: &nexo_core::provider::ModelRequest) -> Vec<String> {
    req.messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| match p {
            Part::ToolResult { content, .. } => content.first().map(|c| text_of_parts(std::slice::from_ref(c))),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_json_deny_blocks_the_tool_and_updated_input_changes_it() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "denied.txt", "content": "a"}))),
        Reply::Events(call("c2", "write", json!({"path": "old.txt", "content": "a"}))),
        Reply::Events(text("ok")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "PreToolUse",
        Some("Write"),
        r#"p=$(cat); case "$p" in *denied.txt*) echo '{"decision":"deny","reason":"json says no"}';; *old.txt*) echo '{"updated_input":{"path":"new.txt","content":"b"}}';; esac; exit 0"#,
    ));
    // the Claude spelling of the event and of the tool is not read from nexo's own config: only the
    // canonical names match a matcher, so this one is spelled the nexo way
    cfg.hooks[0].matcher = Some("write".into());
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    assert!(results_in(&client.requests()[1]).iter().any(|t| t.contains("Blocked by a hook: json says no")));
    let second = results_in(&client.requests()[2]);
    assert!(second.iter().any(|t| t == "write:new.txt"), "the input was replaced: {second:?}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_stop_hook_sends_the_agent_back_to_work_three_times_at_most() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new((1..=6).map(|i| Reply::Events(text(&format!("answer {i}")))).collect());
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("stop", None, "echo 'run the tests first' >&2; exit 2"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert_eq!(end_of(&evs), TurnEnd::Done);
    let reqs = client.requests();
    assert_eq!(reqs.len(), 4, "one answer and three goes back to work");
    let last = reqs[3].messages.last().unwrap();
    assert!(last.text().contains("[Stop hook] run the tests first"), "{}", last.text());
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn session_and_prompt_hooks_add_context_and_a_prompt_hook_can_stop_the_request() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![Reply::Events(text("fine"))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("SessionStart", None, r#"echo '{"additional_context":"the repo uses tabs"}'"#));
    cfg.hooks.push(hook_cfg("UserPromptSubmit", None, "cat > prompt.json; grep -q forbidden prompt.json && { echo 'no such words' >&2; exit 2; }; echo 'it is monday'"));
    cfg.hooks.push(hook_cfg("SessionEnd", None, "cat > end.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "a forbidden thing".into(), attachments: vec![] });
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("a hook stopped it"))).await;
    assert!(matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("no such words")));
    assert!(client.requests().is_empty(), "nothing was sent");
    r.send(AgentCommand::Submit { text: "a fine thing".into(), attachments: vec![] });
    r.turn().await;
    let first = client.requests().remove(0);
    let user = first.messages.iter().find(|m| m.role == nexo_core::message::Role::User).unwrap().text();
    assert!(
        user.contains("the repo uses tabs") && user.contains("it is monday") && user.contains("a fine thing"),
        "{user}"
    );
    r.stop().await;
    let seen: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("end.json")).unwrap()).unwrap();
    assert_eq!(seen["hook_event_name"], "SessionEnd");
    assert!(seen["session_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[cfg(unix)]
#[tokio::test]
async fn tool_hooks_see_the_call_and_its_output_and_a_post_hook_reaches_the_model() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "a.txt", "content": "hello"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("pre_tool", Some("w.*"), "cat > pre.json"));
    cfg.hooks.push(hook_cfg("post_tool", Some("write"), "cat > post.json; echo 'formatted a.txt' >&2; exit 2"));
    cfg.hooks.push(hook_cfg("stop", None, "cat > stop.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let read =
        |n: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(d.path().join(n)).unwrap()).unwrap() };
    let pre = read("pre.json");
    assert_eq!((pre["tool_name"].as_str(), pre["tool_input"]["content"].as_str()), (Some("write"), Some("hello")));
    assert_eq!(pre["hook_event_name"], "PreToolUse");
    let post = read("post.json");
    assert_eq!(post["tool_name"], "write");
    assert!(post["tool_response"].as_str().is_some_and(|s| !s.is_empty()) && post["is_error"] == false);
    assert_eq!(read("stop.json")["last_assistant_message"], "done");
    assert!(results_in(&client.requests()[1]).iter().any(|t| t.contains("[hook] formatted a.txt")));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_can_answer_in_place_of_the_person() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("c1", "write", json!({"path": "ok.txt", "content": "x"}))),
        Reply::Events(text("done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg("permission_request", Some("write"), r#"cat > asked.json; echo '{"decision":"allow"}'"#));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the person was not asked");
    assert!(results_in(&client.requests()[1]).iter().any(|t| t == "write:ok.txt"));
    assert!(std::fs::read_to_string(d.path().join("asked.json")).unwrap().contains("permission_request"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_permission_request_hook_names_the_sub_agent_that_asks_and_not_the_main_agent() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write x.txt"}))),
        Reply::Events(call("s1", "write", json!({"path": "x.txt", "content": "x"}))),
        Reply::Events(text("wrote x.txt")),
        Reply::Events(call("c2", "write", json!({"path": "y.txt", "content": "y"}))),
        Reply::Events(text("all done")),
    ]);
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg(
        "permission_request",
        Some("write"),
        r#"cat > "asked-$(date +%s%N).json"; echo '{"decision":"allow"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let mut seen: Vec<Value> = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("asked-"))
        .map(|e| serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap())
        .collect();
    seen.sort_by_key(|v| v["tool_input"]["path"].as_str().unwrap_or_default().to_string());
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!((seen[0]["tool_input"]["path"].as_str(), seen[0]["agent"].as_str()), (Some("x.txt"), Some("general")));
    assert_eq!(seen[1]["tool_input"]["path"], "y.txt");
    assert!(seen[1].get("agent").is_none(), "the main agent carries no agent field: {:?}", seen[1]);
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn sub_agent_and_compaction_hooks_fire() {
    let d = test_tmpdir().unwrap();
    let client = Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"description": "look", "prompt": "find it"}))),
        Reply::Events(text("found it")),
        Reply::Events(text("the report says found")),
        Reply::Events(text("a summary")),
    ]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("subagent_start", Some("general|explore"), "cat > sub-start.json"));
    cfg.hooks.push(hook_cfg("subagent_stop", None, "cat > sub-stop.json"));
    cfg.hooks.push(hook_cfg("pre_compact", None, "cat > compact.json"));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.send(AgentCommand::Compact);
    r.until(|e| matches!(e, AgentEvent::Compacted { .. })).await;
    let read =
        |n: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(d.path().join(n)).unwrap()).unwrap() };
    assert_eq!(read("sub-start.json")["prompt"], "find it");
    assert_eq!(read("sub-stop.json")["report"], "found it");
    assert_eq!(read("compact.json")["hook_event_name"], "PreCompact");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_hooks_command_lists_what_is_loaded() {
    let d = test_tmpdir().unwrap();
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("notification", None, "true"));
    let mut r = start(Scripted::new(vec![]), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::ShowHooks);
    let evs = r.until(|e| matches!(e, AgentEvent::Notice { text, .. } if text.contains("hook(s)"))).await;
    assert!(
        matches!(evs.last(), Some(AgentEvent::Notice { text, .. }) if text.contains("notification: true  <- nexo config"))
    );
    r.stop().await;
}

/// A main-agent script that sends a `general` sub-agent to write `path`, then lets both answer.
#[cfg(unix)]
fn sub_writes(path: &str) -> Arc<Scripted> {
    Scripted::new(vec![
        Reply::Events(call("t1", "task", json!({"agent": "general", "prompt": "write it"}))),
        Reply::Events(call("s1", "write", json!({"path": path, "content": "a"}))),
        Reply::Events(text("the sub-agent is done")),
        Reply::Events(text("all done")),
    ])
}

#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_that_denies_write_blocks_the_sub_agents_write() {
    let d = test_tmpdir().unwrap();
    let client = sub_writes("denied.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "pre_tool",
        Some("write"),
        r#"cat > pre.json; echo '{"decision":"deny","reason":"no writes here"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let sub = results_in(&client.requests()[2]);
    assert!(sub.iter().any(|t| t.contains("Blocked by a hook: no writes here")), "{sub:?}");
    let pre: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("pre.json")).unwrap()).unwrap();
    assert_eq!((pre["tool_name"].as_str(), pre["agent"].as_str()), (Some("write"), Some("general")));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_post_tool_hook_sees_the_sub_agents_call_with_its_name_and_may_add_a_note() {
    let d = test_tmpdir().unwrap();
    let client = sub_writes("a.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg(
        "post_tool",
        Some("write"),
        r#"cat > post.json; echo '{"additional_context":"formatted a.txt"}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    let post: Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("post.json")).unwrap()).unwrap();
    assert_eq!((post["tool_name"].as_str(), post["agent"].as_str()), (Some("write"), Some("general")));
    assert_eq!(post["tool_response"], "write:a.txt");
    let sub = results_in(&client.requests()[2]);
    assert!(sub.iter().any(|t| t.contains("write:a.txt") && t.contains("[hook] formatted a.txt")), "{sub:?}");
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_pre_tool_hook_can_allow_or_rewrite_a_sub_agents_call() {
    let d = test_tmpdir().unwrap();
    let client = sub_writes("old.txt");
    let mut cfg = config(PermissionMode::Ask);
    cfg.hooks.push(hook_cfg(
        "pre_tool",
        Some("write"),
        r#"echo '{"decision":"allow","updated_input":{"path":"new.txt","content":"b"}}'"#,
    ));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    let evs = r.turn().await;
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::PermissionAsk { .. })), "the person was not asked");
    assert!(results_in(&client.requests()[2]).iter().any(|t| t == "write:new.txt"));
    r.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_sub_agents_tool_calls_reach_the_audit_log() {
    let d = test_tmpdir().unwrap();
    let log = d.path().join("audit.jsonl");
    let client = sub_writes("a.txt");
    let mut cfg = config(PermissionMode::Auto);
    cfg.audit.enabled = true;
    cfg.audit.path = Some(log.display().to_string());
    cfg.hooks.push(hook_cfg("pre_tool", Some("write"), r#"echo '{"decision":"allow"}'"#));
    let mut r = start(client.clone(), cfg, None, d.path()).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.turn().await;
    r.stop().await;
    let lines = |kind: &str| -> Vec<Value> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["kind"] == "tool" && v["tool"] == kind)
            .collect()
    };
    let write = lines("write");
    assert_eq!(write.len(), 1, "the sub-agent's write is logged once: {write:?}");
    assert_eq!((write[0]["status"].as_str(), write[0]["decision"].as_str()), (Some("ok"), Some("allowed")));
    assert_eq!(lines("task").len(), 1, "the main agent's task call is logged too");
}
