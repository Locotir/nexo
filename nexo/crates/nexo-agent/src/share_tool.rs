//! Tools for the shared, locked memory. Reads and writes are separate so read-only
//! sub-agents and permission rules cannot accidentally grant write access.
use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct ReadSharedMemory;
pub struct WriteSharedMemory;

#[async_trait]
impl Tool for ReadSharedMemory {
    fn name(&self) -> &str {
        "shared_memory_read"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name().into(), description: "Read shared memory entries after an epoch, oldest first. Continue with the last returned epoch to page through entries.".into(), parameters: json!({"type":"object", "properties":{"since":{"type":"integer","minimum":0,"description":"Last epoch seen (default 0)."}}}) }
    }
    fn summary(&self, args: &Value) -> String {
        format!("shared memory since {}", args.get("since").unwrap_or(&json!(0)))
    }
    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let since = match args.get("since") {
            None => 0,
            Some(v) => match v.as_u64() {
                Some(n) => n,
                None => return ToolOutput::error("since must be a non-negative integer"),
            },
        };
        match crate::share::read_since(since) {
            Ok(entries) if entries.is_empty() => ToolOutput::ok("No shared memory entries after this epoch."),
            Ok(entries) => ToolOutput::ok(crate::share::block(&entries)),
            Err(e) => ToolOutput::error(format!("shared memory: {e}")),
        }
    }
}

#[async_trait]
impl Tool for WriteSharedMemory {
    fn name(&self) -> &str {
        "shared_memory_write"
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Write
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name().into(), description: "Append a concise coordination note to shared memory, visible to other agents and sessions. Do not store secrets.".into(), parameters: json!({"type":"object", "properties":{"text":{"type":"string","description":"The note to share."}}, "required":["text"]}) }
    }
    fn summary(&self, args: &Value) -> String {
        format!(
            "share: {}",
            args.get("text").and_then(Value::as_str).unwrap_or("?").chars().take(80).collect::<String>()
        )
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(text) = args.get("text").and_then(Value::as_str) else {
            return ToolOutput::error("text must be a string");
        };
        let id = if ctx.session_id.is_empty() { "agent" } else { &ctx.session_id };
        match crate::share::append(id, text) {
            Ok(entry) => ToolOutput::ok(format!("Shared: {}", crate::share::block(&[entry]))),
            Err(e) => ToolOutput::error(format!("shared memory: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shared_memory_tools_round_trip() {
        // The child gets its own environment; no other parallel test sees this path.
        if std::env::var_os("NEXO_SHARE_TOOL_CHILD").is_none() {
            let dir = tempfile::tempdir().unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "share_tool::tests::shared_memory_tools_round_trip", "--nocapture"])
                .env("NEXO_SHARE_TOOL_CHILD", "1")
                .env("NEXO_SHARED_MEMORY", dir.path().join("memory"))
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let mut ctx = ToolContext::new(std::env::current_dir().unwrap());
        ctx.session_id = "session-1".into();
        assert_eq!(ReadSharedMemory.kind(), ToolKind::Read);
        assert_eq!(WriteSharedMemory.kind(), ToolKind::Write);
        assert!(WriteSharedMemory.run(json!({}), &ctx).await.is_error);
        assert!(WriteSharedMemory.run(json!({"text":"  "}), &ctx).await.is_error);
        assert!(ReadSharedMemory.run(json!({"since":-1}), &ctx).await.is_error);
        assert!(WriteSharedMemory.run(json!({"text":"hello"}), &ctx).await.content.contains("1 | session-1 | hello"));
        assert!(ReadSharedMemory.run(json!({}), &ctx).await.content.contains("1 | session-1 | hello"));
        assert!(ReadSharedMemory.run(json!({"since":1}), &ctx).await.content.contains("No shared memory"));
    }
}
