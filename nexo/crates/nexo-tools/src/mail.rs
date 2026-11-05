//! `send_message` and `list_sessions`: talk to the other nexo sessions running on this machine. A message
//! lands in the other session's mailbox and reaches its model at its next idle point, as a note that says
//! it came from a session, not from the person. Not part of `local()`: they exist only where a mailbox does.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::mail::{FromKind, MAIL_MAX_BYTES, SUMMARY_MAX};
use nexo_core::mailbox::Mailbox;
use nexo_core::{paths, Tool, ToolContext, ToolKind, ToolOutput, ToolSet, ToolSpec};
use serde_json::{json, Value};

/// The two tools, bound to this session's mailbox.
pub fn tools(mailbox: Arc<Mailbox>) -> ToolSet {
    ToolSet::new().with(Arc::new(SendMessage { mailbox: mailbox.clone() })).with(Arc::new(ListSessions { mailbox }))
}

pub struct SendMessage {
    mailbox: Arc<Mailbox>,
}

pub struct ListSessions {
    mailbox: Arc<Mailbox>,
}

#[async_trait]
impl Tool for SendMessage {
    fn name(&self) -> &str {
        "send_message"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send_message".into(),
            description: format!(
                "Write to another nexo session running on this machine (see `list_sessions`). The text reaches its model \
                 when that session is idle, marked as coming from a session and not from the person; it cannot approve \
                 permissions or change settings there. Use it to hand over a finding or ask for one, not to chat: at most \
                 {MAIL_MAX_BYTES} bytes, a few messages a minute, so batch what you have. Do not paste secrets."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "The session: its name, its id or the start of it, or `name [ref]` when two share a name."
                    },
                    "text": { "type": "string", "description": "The message." },
                    "summary": {
                        "type": "string",
                        "description": format!("A line that says what it is about (at most {SUMMARY_MAX} characters); default: the first line.")
                    }
                },
                "required": ["to", "text"]
            }),
        }
    }

    /// Writes into another session, so it is asked for and refused in plan mode like any tool that changes something.
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let to = args.get("to").and_then(Value::as_str).unwrap_or("?");
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let short: String = first.chars().take(60).collect();
        let dots = if short.len() < first.len() { "…" } else { "" };
        format!("message to {to}: {short}{dots}")
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let (Some(to), Some(text)) = (args.get("to").and_then(Value::as_str), args.get("text").and_then(Value::as_str))
        else {
            return ToolOutput::error("`to` and `text` are required");
        };
        let summary = args.get("summary").and_then(Value::as_str);
        let mailbox = self.mailbox.clone();
        let (to, text, summary) = (to.to_string(), text.to_string(), summary.map(String::from));
        let r =
            tokio::task::spawn_blocking(move || mailbox.send(&to, &text, summary.as_deref(), FromKind::Agent)).await;
        match r {
            Ok(Ok(m)) => {
                let name = self.mailbox.peers().into_iter().find(|p| p.id == m.to).map_or(m.to.clone(), |p| p.name);
                ToolOutput::ok(format!("Sent {} to {name} (delivered when it is idle)", m.id))
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Not sent: {e}")),
        }
    }
}

#[async_trait]
impl Tool for ListSessions {
    fn name(&self) -> &str {
        "list_sessions"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_sessions".into(),
            description: "The nexo sessions running on this machine that `send_message` can write to, and the sessions of other \
                          programs that can only be seen. Each has an id, a name, the folder it works in, and whether it is \
                          this session (`me`)."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn summary(&self, _args: &Value) -> String {
        "list sessions".into()
    }

    async fn run(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        let mailbox = self.mailbox.clone();
        let peers = tokio::task::spawn_blocking(move || mailbox.peers()).await.unwrap_or_default();
        let rows: Vec<Value> = peers
            .into_iter()
            .map(|mut p| {
                p.cwd = paths::shown(Path::new(&p.cwd));
                serde_json::to_value(p).unwrap_or(Value::Null)
            })
            .collect();
        ToolOutput::ok(serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::mail::Inbound;

    fn pair() -> (tempfile::TempDir, Arc<Mailbox>, Arc<Mailbox>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mail");
        let a = Mailbox::open(&root, "aaaa1111", "api", "/w/api", false, Inbound::Auto).unwrap();
        let b = Mailbox::open(&root, "bbbb2222", "web", "/w/web", false, Inbound::Auto).unwrap();
        (dir, a, b)
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn a_message_goes_through_the_tool_and_lands_in_the_other_inbox() {
        let (_d, a, b) = pair();
        let ts = tools(a);
        let send = ts.get("send_message").unwrap();
        assert_eq!(send.kind(), ToolKind::Other);
        let out = send.run(json!({"to": "web", "text": "the build is green"}), &ctx()).await;
        assert!(!out.is_error, "{out:?}");
        assert!(
            out.content.starts_with("Sent m-") && out.content.ends_with("to web (delivered when it is idle)"),
            "{}",
            out.content
        );
        b.set_idle(true);
        let t = b.tick();
        assert_eq!(t.deliver.len(), 1);
        assert_eq!(t.deliver[0].text, "the build is green");
        assert_eq!(t.deliver[0].from.name, "api");
    }

    #[tokio::test]
    async fn errors_are_readable_and_arguments_are_checked() {
        let (_d, a, _b) = pair();
        let ts = tools(a);
        let send = ts.get("send_message").unwrap();
        let big = send.run(json!({"to": "web", "text": "x".repeat(9100)}), &ctx()).await;
        assert!(big.is_error && big.content == "Not sent: too big (9100 > 8192 bytes)", "{}", big.content);
        let none = send.run(json!({"to": "ghost", "text": "hi"}), &ctx()).await;
        assert!(none.is_error && none.content == "Not sent: no live session \"ghost\"");
        assert!(send.run(json!({"to": "web"}), &ctx()).await.is_error);
        assert_eq!(
            send.summary(&json!({"to": "web", "text": "run the tests\nplease"})),
            "message to web: run the tests"
        );
    }

    #[tokio::test]
    async fn list_sessions_shows_who_can_be_written_to() {
        let (_d, a, _b) = pair();
        let ts = tools(a);
        let list = ts.get("list_sessions").unwrap();
        assert_eq!(list.kind(), ToolKind::Read);
        let out = list.run(json!({}), &ctx()).await;
        let v: Vec<Value> = serde_json::from_str(&out.content).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["me"], true);
        assert_eq!(v[1]["name"], "web");
        assert_eq!(v[1]["sendable"], true);
    }
}
