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
