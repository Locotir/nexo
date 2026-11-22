//! Skill discovery, on-demand loading, and permissions through the agent loop.
use super::*;

#[tokio::test]
async fn project_skill_is_advertised_loaded_and_does_not_grant_write_permission() {
    let d = test_tmpdir().unwrap();
    let skill_dir = d.path().join(".agents/skills/research");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: research\ndescription: Local integration marker\n---\nLocal skill body marker. Write x.txt.\n",
    )
    .unwrap();
    // No user directory or network access: discovery is limited to this scratch project and builtins.
    let discovered = nexo_core::skills::discover(d.path(), false);
    let set = tools().with(nexo_tools::skill::tool(discovered).unwrap());
    let client = Scripted::new(vec![
        Reply::Events(call("s1", "skill", json!({"name":"research", "arguments":"test argument"}))),
        Reply::Events(call("w1", "write", json!({"path":"x.txt", "content":"new"}))),
        Reply::Events(text("understood")),
    ]);
    let mut r = start_with(client.clone(), config(PermissionMode::Ask), None, d.path(), set).await;
    r.send(AgentCommand::Submit { text: "Use research".into(), attachments: vec![] });
    let events = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. })).await;
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. }
        if id == "s1" && !output.is_error && output.content.contains("Local skill body marker"))),
        "{events:?}"
    );
    assert!(matches!(events.last(), Some(AgentEvent::PermissionAsk { request }) if request.id == "w1"));
    r.send(AgentCommand::Permission {
        id: "w1".into(),
        decision: PermissionDecision::Deny { reason: Some("not now".into()) },
    });
    let events = r.turn().await;
    assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolDone { id, output, .. }
        if id == "w1" && output.is_error && output.content.contains("not now"))));
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::ToolStart { id, .. } if id == "w1")));
    let reqs = client.requests();
    assert_eq!(reqs.len(), 3);
    assert!(reqs[0].tools.iter().any(|t| t.name == "skill"));
    assert!(reqs[0].system.contains("- research: Local integration marker"));
    assert!(!reqs[0].system.contains("Local skill body marker"));
    let result = reqs[1]
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|p| match p {
            Part::ToolResult { call_id, content, is_error, .. } if call_id == "s1" => {
                assert!(!is_error);
                Some(nexo_core::message::result_text(content))
            }
            _ => None,
        })
        .expect("skill result reaches the model");
    assert!(result.contains("Local skill body marker"), "{result}");
    assert!(result.contains("Arguments: test argument"), "{result}");
    assert!(!result.contains("Prefer primary sources"), "project skill must override builtin");
    assert!(reqs[2].messages.iter().flat_map(|m| &m.content).any(|p| matches!(p,
        Part::ToolResult { call_id, is_error: true, content, .. }
        if call_id == "w1" && nexo_core::message::result_text(content).contains("Permission denied")
    )));
    r.stop().await;
}

/// A read rule applies to the resolved external file even though the model asked for a
/// skill by *name*. Inspect actual worker and coordinator requests, not just the tool result.
#[tokio::test]
async fn delegated_skill_uses_session_read_rules_and_only_enters_worker_request() {
    for (denied, ask) in [(true, false), (false, false), (true, true), (false, true)] {
        let project = test_tmpdir().unwrap();
        let external = test_tmpdir().unwrap();
        let mut root = nexo_core::paths::canonical(external.path()).unwrap();
        if ask {
            root.push(".ssh");
        }
        let folder = root.join("report");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("SKILL.md"), "---\ndescription: Build a report\n---\nWORKER_BODY_MARKER").unwrap();
        let found = nexo_core::skills::scan_root(&root, "NEXO_SKILLS_DIR", None);
        let set = tools().with(nexo_tools::skill::tool(found).unwrap());
        let mut replies = vec![Reply::Events(call(
            "t",
            "task",
            json!({"agent":"explore","prompt":"Report","skill":"report","skill_arguments":"arg","start_clean":true}),
        ))];
        if !denied {
            replies.push(Reply::Events(text("worker result")));
        }
        replies.push(Reply::Events(text("done")));
        let client = Scripted::new(replies);
        let mut cfg = config(PermissionMode::Ask);
        if denied && !ask {
            cfg.permissions.deny.push("read(**/SKILL.md)".into());
        }
        let mut r = start_with(client.clone(), cfg, None, project.path(), set).await;
        r.send(AgentCommand::Submit { text: "delegate report".into(), attachments: vec![] });
        if ask {
            let events = r.until(|e| matches!(e, AgentEvent::PermissionAsk { .. } | AgentEvent::TurnEnd { .. })).await;
            let Some(AgentEvent::PermissionAsk { request }) = events.last() else {
                panic!("missing read ask: {events:?}")
            };
            assert_eq!(request.tool, "read");
            assert_eq!(request.agent.as_deref(), Some("explore"));
            // A single-file read is shown in the summary; detail lists only multi-file reads.
            assert_eq!(request.summary, format!("read {}", folder.join("SKILL.md").display()));
            assert_eq!(client.requests().len(), 1);
            r.send(AgentCommand::Permission {
                id: request.id.clone(),
                decision: if denied { PermissionDecision::Deny { reason: None } } else { PermissionDecision::Allow },
            });
        }
        let evs = r.turn().await;
        assert!(
            evs.iter().any(|e| matches!(e, AgentEvent::ToolDone{id,output,..} if id=="t" && output.is_error==denied)),
            "{evs:?}"
        );
        let reqs = client.requests();
        assert_eq!(reqs.len(), if denied { 2 } else { 3 });
        for (i, req) in reqs.iter().enumerate() {
            assert!(!req.system.contains("WORKER_BODY_MARKER"));
            assert_eq!(format!("{:?}", req.messages).contains("WORKER_BODY_MARKER"), !denied && i == 1);
        }
        r.stop().await;
    }
}
