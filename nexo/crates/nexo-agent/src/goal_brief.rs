//! `goal_brief`: the agent restates the goal the way the person means it (what, why, how, when it is
//! done), and the restatement takes the place of the person's words. The tool only says how the call
//! reads: the agent loop answers it, since the goal lives in the loop's state.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct GoalBrief;

/// The fields of a call, in the order the brief shows them: (key, label).
pub const FIELDS: [(&str, &str); 4] = [("want", "Want"), ("why", "Why"), ("how", "How"), ("done", "Done when")];

/// The brief a call gives, or the first field it lacks.
pub fn brief(args: &Value) -> Result<String, &'static str> {
    let mut lines = Vec::new();
    for (key, label) in FIELDS {
        let v = args.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default();
        if v.is_empty() {
            return Err(key);
        }
        lines.push(format!("{label}: {v}"));
    }
    Ok(lines.join("\n"))
}

#[async_trait]
impl Tool for GoalBrief {
    fn name(&self) -> &str {
        "goal_brief"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "goal_brief".into(),
            description: "Restate the goal as the person really means it; it replaces their words from then on. \
                          Put yourself in their place: they wrote it quickly. Fill gaps from the conversation and \
                          the repository. Call it again when they change what they want."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "want": {"type": "string", "description": "What they want, concrete, in one or two sentences."},
                    "why": {"type": "string", "description": "Why they want it: the need behind it."},
                    "how": {"type": "string", "description": "How they expect it done: the quality bar, conventions and limits they would take for granted."},
                    "done": {"type": "string", "description": "When it counts as done: checks anyone could run or see."}
                },
                "required": ["want", "why", "how", "done"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let want = args.get("want").and_then(Value::as_str).unwrap_or("?");
        format!("goal: {}", want.chars().take(60).collect::<String>())
    }

    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::error("goal_brief is answered by the agent loop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brief_needs_every_field() {
        let full = json!({"want": "a", "why": "b", "how": "c", "done": " d "});
        assert_eq!(brief(&full).unwrap(), "Want: a\nWhy: b\nHow: c\nDone when: d");
        assert!(brief(&full).unwrap().starts_with(nexo_core::agent::GOAL_BRIEF_HEAD));
        assert_eq!(brief(&json!({"want": "a", "why": "b", "how": " "})), Err("how"));
    }
}
