//! Model control requests are handled by the session loop, which remains responsive during a turn.
use super::*;
use async_trait::async_trait;
use nexo_core::provider::ToolSpec;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

pub(super) struct Request {
    name: &'static str,
    args: Value,
    session_id: String,
    cancel: tokio_util::sync::CancellationToken,
    reply: oneshot::Sender<ToolOutput>,
}

pub(super) struct ControlTool {
    name: &'static str,
    tx: mpsc::UnboundedSender<Request>,
}
impl ControlTool {
    pub(super) fn new(name: &'static str, tx: mpsc::UnboundedSender<Request>) -> Self {
        Self { name, tx }
    }
}
#[async_trait]
impl Tool for ControlTool {
    fn name(&self) -> &str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        let (description, properties) = if self.name == "goal" {
            ("Manage this session's goal. Setting or resuming a goal may start further model turns. Future tool calls require their own permissions.", json!({"action":{"type":"string","enum":["set","clear","pause","resume","show"]},"text":{"type":"string","description":"Goal text for set."}}))
        } else {
            ("Manage session-local recurring prompts. A new loop runs as soon as idle, then at its interval; stopping nexo or switching sessions removes it. Future tool calls require their own permissions.", json!({"action":{"type":"string","enum":["create","list","cancel"]},"interval":{"type":"string","description":"60s to 30d, e.g. 5m."},"prompt":{"type":"string"},"id":{"type":"integer","minimum":1}}))
        };
        ToolSpec {
            name: self.name.into(),
            description: description.into(),
            parameters: json!({"type":"object","properties":properties,"required":["action"]}),
        }
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }
    fn call_kind(&self, args: &Value) -> ToolKind {
        if matches!(args.get("action").and_then(Value::as_str), Some("show" | "list")) {
            ToolKind::Read
        } else {
            ToolKind::Other
        }
    }
    fn summary(&self, args: &Value) -> String {
        format!("{}: {}", self.name, args)
    }
    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Request {
                name: self.name,
                args,
                session_id: ctx.session_id.clone(),
                cancel: ctx.cancel.clone(),
                reply,
            })
            .is_err()
        {
            return ToolOutput::error("session closed");
        }
        tokio::select! { result = rx => result.unwrap_or_else(|_| ToolOutput::error("session closed")), _ = ctx.cancel.cancelled() => ToolOutput::error("interrupted") }
    }
}

pub(super) async fn handle(rt: &mut Runtime, req: Request) {
    // Do not perform an approved action if its turn was cancelled while awaiting dispatch.
    if req.reply.is_closed() || req.cancel.is_cancelled() {
        return;
    }
    if rt.state.lock().await.session.id() != req.session_id {
        let _ = req.reply.send(ToolOutput::error("session changed"));
        return;
    }
    let action = req.args.get("action").and_then(Value::as_str).unwrap_or("");
    let out = if req.name == "loop" {
        let input = match action {
            "list" => Some("list".to_string()),
            "cancel" => req.args.get("id").and_then(Value::as_u64).map(|id| format!("cancel {id}")),
            "create" => req
                .args
                .get("interval")
                .and_then(Value::as_str)
                .zip(req.args.get("prompt").and_then(Value::as_str))
                .map(|(i, p)| format!("{i} {p}")),
            _ => None,
        };
        match input {
            Some(input) => scheduled::command(rt, &input, false),
            None => ToolOutput::error("invalid loop action or missing arguments"),
        }
    } else {
        match action {
            "set" => {
                match req.args.get("text").and_then(Value::as_str).filter(|s| !s.trim().is_empty() && s.len() <= 16000)
                {
                    Some(text) => {
                        dispatch::on_set_goal(rt, Some(text.to_string()), vec![]).await;
                        ToolOutput::ok("goal set; its request will run after this turn")
                    }
                    None => ToolOutput::error("goal text must be 1–16000 bytes"),
                }
            }
            "clear" => {
                dispatch::on_set_goal(rt, None, vec![]).await;
                ToolOutput::ok("goal cleared")
            }
            "pause" => {
                pause_goal(&rt.state, true).await;
                ToolOutput::ok("goal pause requested")
            }
            "resume" => {
                resume_goal(&rt.state, true).await;
                ToolOutput::ok("goal resume requested")
            }
            "show" => {
                let st = rt.state.lock().await;
                ToolOutput::ok(format!("goal: {:?}; state: {:?}", st.goal, st.goal_state))
            }
            _ => ToolOutput::error("invalid goal action"),
        }
    };
    let _ = req.reply.send(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{Decision, Policy};
    use nexo_tools::todo::Todo;

    #[test]
    fn control_and_todo_mutations_ask_unless_auto_or_explicitly_allowed() {
        let (tx, _) = mpsc::unbounded_channel();
        let goal = ControlTool::new("goal", tx.clone());
        let loops = ControlTool::new("loop", tx);
        let todo = Todo::new();
        let normal = Policy { mode: PermissionMode::Ask, ..Policy::default() };
        let auto = Policy { mode: PermissionMode::Auto, ..Policy::default() };
        let cwd = Path::new("/work");
        for (tool, args) in [
            (&goal as &dyn Tool, json!({"action":"set","text":"work"})),
            (&loops as &dyn Tool, json!({"action":"create","interval":"1m","prompt":"work"})),
            (&todo as &dyn Tool, json!({"items":[]})),
        ] {
            assert_eq!(normal.decide(tool.name(), tool.call_kind(&args), &args, cwd), Decision::Ask);
            assert_eq!(auto.decide(tool.name(), tool.call_kind(&args), &args, cwd), Decision::Allow);
            let deny = Policy { deny: vec![tool.name().into()], ..auto.clone() };
            assert!(matches!(deny.decide(tool.name(), tool.call_kind(&args), &args, cwd), Decision::Deny(_)));
        }
        for (tool, args) in [
            (&goal as &dyn Tool, json!({"action":"show"})),
            (&loops as &dyn Tool, json!({"action":"list"})),
            (&todo as &dyn Tool, json!({})),
        ] {
            assert_eq!(normal.decide(tool.name(), tool.call_kind(&args), &args, cwd), Decision::Allow);
        }
    }
}
