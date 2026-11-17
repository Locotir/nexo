use super::*;

/// A tool that never looks at the cancel token, as a blocking call would not.
struct Stuck;

#[async_trait]
impl Tool for Stuck {
    fn name(&self) -> &str {
        "stuck"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: "stuck".into(), description: "never ends".into(), parameters: json!({"type": "object"}) }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn summary(&self, _args: &Value) -> String {
        "stuck".into()
    }
    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        tokio::time::sleep(Duration::from_secs(120)).await;
        ToolOutput::ok("late")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_turn_stuck_in_a_tool_does_not_stop_the_session_end_hook() {
    let d = test_tmpdir().unwrap();
    let marker = d.path().join("session-ended");
    let client = Scripted::new(vec![Reply::Events(call("c1", "stuck", json!({})))]);
    let mut cfg = config(PermissionMode::Auto);
    cfg.hooks.push(hook_cfg("session_end", None, &format!("touch '{}'", marker.display())));
    let mut r = start_with(client, cfg, None, d.path(), ToolSet::new().with(Arc::new(Stuck))).await;
    r.next().await;
    r.send(AgentCommand::Submit { text: "go".into(), attachments: vec![] });
    r.until(|e| matches!(e, AgentEvent::ToolStart { .. })).await;
    let started = std::time::Instant::now();
    r.send(AgentCommand::Shutdown);
    r.until(|e| matches!(e, AgentEvent::Shutdown)).await;
    r.task.await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(20), "the close waited for the tool: {:?}", started.elapsed());
    assert!(marker.exists(), "the session_end hook did not run");
}
