//! A ChatGPT subscription login against a local server standing in for the backend: the stored login is
//! read, its headers and the subscription body go out, and the streamed text and tool call come back.
//! One test only: it points the credential store at a temp file through the environment.

use std::collections::BTreeMap;

use futures::StreamExt;
use httpmock::prelude::*;
use nexo_core::auth::{self, Credential};
use nexo_core::{Collector, Message, ModelRequest, ProviderConfig, ToolSpec};
use nexo_providers::{registry, Client};
use serde_json::json;

const SSE: &str = include_str!("fixtures/openai_responses.sse");

#[tokio::test]
async fn a_stored_codex_login_streams_text_and_a_tool_call_from_the_subscription_backend() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("auth.json");
    std::env::set_var("NEXO_AUTH_FILE", &store);
    let mut metadata = BTreeMap::new();
    metadata.insert("flow".to_string(), "codex".to_string());
    metadata.insert("account_id".to_string(), "acct-7".to_string());
    let cred = Credential::Oauth {
        access: "synthetic-access".into(),
        refresh: "synthetic-refresh".into(),
        expires: auth::now() + 3600,
        metadata,
    };
    auth::set_in(&store, "codex", cred).unwrap();

    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/backend-api/codex/responses")
                .header("authorization", "Bearer synthetic-access")
                .header("chatgpt-account-id", "acct-7")
                .header("originator", "nexo")
                .json_body_includes(r#"{"stream": true, "store": false}"#)
                .is_true(|r| {
                    let b: serde_json::Value = serde_json::from_str(&r.body_string()).unwrap();
                    b.get("max_output_tokens").is_none()
                        && b.get("temperature").is_none()
                        && b["instructions"] == "be brief"
                        && b["tools"][0]["name"] == "read"
                        && b["tools"][0]["type"] == "function"
                        && b["tools"][0]["strict"] == false
                });
            then.status(200).header("content-type", "text/event-stream").body(SSE);
        })
        .await;

    let mut cfg = ProviderConfig::new("openai-responses", &format!("{}/backend-api/codex", server.base_url()), None);
    cfg.login = Some("codex".into());
    cfg.auth = Some("codex".into());
    cfg.extra.insert("omit_max_tokens".into(), toml::Value::Boolean(true));
    let mut req = ModelRequest::new("gpt-5", "be brief", vec![Message::user("read main")]);
    req.temperature = Some(0.2);
    req.tools = vec![ToolSpec {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    }];

    let client = Client::new(registry());
    let mut s = client.stream(&cfg, &req, 3).await.unwrap();
    let mut c = Collector::new();
    while let Some(ev) = s.next().await {
        c.push(ev);
    }
    m.assert_async().await;
    let msg = c.message();
    assert!(msg.text().contains("I'll read it."), "{}", msg.text());
    let calls: Vec<_> = msg.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "read");
    assert_eq!(calls[0].2["path"], "src/main.rs");
}
