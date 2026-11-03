//! `recall`: a message the context left out, back as it was. The context curator names each message it
//! leaves out `m#####` (its place in the conversation, from 1); the session answers the call from the
//! whole history it keeps, so what comes back is exact. The tool itself only says how the call reads:
//! the agent loop answers it (`nexo_agent::recall`).

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct Recall;

#[async_trait]
impl Tool for Recall {
    fn name(&self) -> &str {
        "recall"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "recall".into(),
            description: "Bring back, exactly as it was, a message this context left out: give the name it was \
                          left out under (`m00042`). Use it instead of running a command again when you need what \
                          it printed. `from` and `to` (lines, from 1) take part of a long message."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "The message's name, like `m00042`."},
                    "from": {"type": "integer", "description": "First line to bring back (from 1)."},
                    "to": {"type": "integer", "description": "Last line to bring back."}
                },
                "required": ["message"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        format!("recall {}", args.get("message").and_then(Value::as_str).unwrap_or("?"))
    }

    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::error("recall is answered by the session: this session keeps no history to answer it from")
    }
}
