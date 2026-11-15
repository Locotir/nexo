//! MCP servers connect while the interface starts; the first request still carries their tools.
#![cfg(unix)]

use super::*;

/// A server whose `initialize` answer takes two seconds; it has one tool, `t`.
const SLOW_SERVER: &str = r#"while IFS= read -r l; do
  id=$(printf '%s' "$l" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$l" in
    *'"method":"initialize"'*) sleep 2; printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"toy"}}}\n' "$id" ;;
    *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
  esac
done
"#;

#[tokio::test]
async fn the_agent_is_ready_before_a_slow_server_answers_and_the_first_request_lists_its_tools() {
    let d = test_tmpdir().unwrap();
    let mut servers = std::collections::BTreeMap::new();
    servers.insert(
        "slow".to_string(),
        nexo_core::config::McpServerConfig {
            command: "sh".into(),
            args: vec!["-c".into(), SLOW_SERVER.into()],
            ..Default::default()
        },
    );
    let hub = nexo_tools::mcp_hub::McpHub::start_in_background(&servers, &d.path().join("auth.json"), None);
    data_dir();
    let (handle, ends) = channels();
    let client = Scripted::new(vec![Reply::Events(text("hi"))]);
    let setup = AgentSetup {
        config: config(PermissionMode::Ask),
        cwd: d.path().to_path_buf(),
        provider: "mock".into(),
        model: "m1".into(),
        tools: tools(),
        client: client.clone(),
        resume: None,
        gate: None,
        mail: None,
        mcp: Some(hub.clone()),
        model_pinned: false,
    };
    let task = tokio::spawn(nexo_agent::run(ends, setup));
    let mut r = Running { handle, task };
    assert!(matches!(r.next().await, AgentEvent::Ready { .. }));
    assert_eq!(hub.generation(), 0, "the agent was ready while the server had not answered");
    r.send(AgentCommand::Submit { text: "hello".into(), attachments: vec![] });
    r.turn().await;
    let reqs = client.requests();
    assert_eq!(reqs.len(), 1);
    assert!(reqs[0].tools.iter().any(|t| t.name == "mcp__slow__t"), "{:?}", reqs[0].tools);
    r.stop().await;
    hub.close().await;
}
