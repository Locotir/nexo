//! Plan profile acceptance: scripted provider, production tools and real files.
//! No model quality claim: this checks the task/runtime contract offline.
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use nexo_agent::{subagent::SubAgent, ModelClient, Target};
use nexo_core::{
    config::ProviderConfig,
    error::ProviderError,
    message::Part,
    provider::{EventStream, ModelRequest},
    stream::{FinishReason, StreamEvent},
    subs::SubState,
    tool::{Tool, ToolContext},
};
use serde_json::{json, Value};

struct Planner {
    requests: Mutex<Vec<ModelRequest>>,
}

fn call(id: &str, name: &str, args: Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart { id: id.into(), name: name.into() },
        StreamEvent::ToolCallEnd { id: id.into(), name: name.into(), args },
        StreamEvent::Finish(FinishReason::ToolUse),
        StreamEvent::Done,
    ]
}

#[async_trait]
impl ModelClient for Planner {
    async fn stream(&self, _: &ProviderConfig, req: &ModelRequest, _: u32) -> Result<EventStream, ProviderError> {
        let mut requests = self.requests.lock().unwrap();
        let step = requests.len();
        requests.push(req.clone());
        let events = match step {
            0 => call("read1", "read", json!({"path":"src/lib.rs"})),
            1 => call("write1", "write", json!({"path":"src/lib.rs", "content":"destroyed"})),
            2 => call("escape1", "read", json!({"path":"../secret.txt"})),
            3 => call("shell1", "bash", json!({"command":"touch forbidden-marker"})),
            4 => vec![
                StreamEvent::Start { response_id: None, model: None },
                StreamEvent::TextDelta(
                    "Plan: src/lib.rs:1 returns 7. Add an acceptance test before changing it; no changes made.".into(),
                ),
                StreamEvent::Finish(FinishReason::Stop),
                StreamEvent::Done,
            ],
            _ => panic!("unexpected planner round"),
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn plan_reads_real_source_rejects_mutations_and_returns_its_report() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    let source = b"pub fn answer() -> u32 { 7 }\n";
    std::fs::write(project.join("src/lib.rs"), source).unwrap();
    std::fs::write(root.path().join("secret.txt"), "PRIVATE_SENTINEL").unwrap();
    let client = Arc::new(Planner { requests: Mutex::new(Vec::new()) });
    let mut config = ProviderConfig::new("mock", "http://unused.invalid", None);
    config.models = vec!["planner-model".into()];
    let task = SubAgent::new(
        client.clone(),
        Target::new("mock", config, "planner-model"),
        &nexo_tools::builtin(),
        vec![],
        Arc::new(OnceLock::new()),
    );
    let mut ctx = ToolContext::new(project.clone());
    ctx.call_id = "planner-acceptance".into();
    ctx.session_id = "offline-planner".into();
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        task.run(json!({"agent":"plan", "prompt":"Inspect src/lib.rs and plan a test; do not change files."}), &ctx),
    )
    .await
    .expect("planner completes without permission prompts or shell execution");
    assert!(!out.is_error, "{out:?}");
    assert!(out.content.contains("src/lib.rs:1 returns 7"), "{out:?}");
    assert_eq!(std::fs::read(project.join("src/lib.rs")).unwrap(), source);
    assert!(!project.join("forbidden-marker").exists());
    assert_eq!(std::fs::read_to_string(root.path().join("secret.txt")).unwrap(), "PRIVATE_SENTINEL");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    for request in requests.iter() {
        assert_eq!(request.model, "planner-model");
        assert!(request.system.contains("smallest viable implementation plan"));
        assert!(request.tools.iter().any(|t| t.name == "read"));
        for denied in ["write", "edit", "bash", "browser", "task"] {
            assert!(!request.tools.iter().any(|t| t.name == denied), "{denied}");
        }
        assert!(!request.messages.iter().any(|m| m.text().contains("PRIVATE_SENTINEL")));
    }
    for (round, id, error, expected) in [
        (1, "read1", false, "pub fn answer() -> u32 { 7 }"),
        (2, "write1", true, "has no tool `write`"),
        (3, "escape1", true, "outside the project"),
        (4, "shell1", true, "has no tool `bash`"),
    ] {
        let result = requests[round]
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|part| match part {
                Part::ToolResult { call_id, content, is_error, .. } if call_id == id => {
                    Some((*is_error, nexo_core::message::result_text(content)))
                }
                _ => None,
            })
            .expect("tool result reaches subsequent planner request");
        assert_eq!(result.0, error, "{id}: {}", result.1);
        assert!(result.1.contains(expected), "{id}: {}", result.1);
    }
    let info = ctx.jobs.subs().list().pop().unwrap();
    assert_eq!(info.profile, "plan");
    assert_eq!(info.state, SubState::Ok);
}
