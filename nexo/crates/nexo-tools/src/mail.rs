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

/// The tools, bound to this session's mailbox: `send_to_session` also types into sessions of other programs.
pub fn tools(mailbox: Arc<Mailbox>) -> ToolSet {
    ToolSet::new()
        .with(Arc::new(SendMessage { mailbox: mailbox.clone() }))
        .with(Arc::new(crate::send::SendToSession::new(Some(mailbox.clone()), nexo_core::reach::Reach::default())))
        .with(Arc::new(ListSessions { mailbox }))
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

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let (Some(to), Some(text)) = (args.get("to").and_then(Value::as_str), args.get("text").and_then(Value::as_str))
        else {
            return ToolOutput::error("`to` and `text` are required");
        };
        let summary = args.get("summary").and_then(Value::as_str);
        let mailbox = self.mailbox.clone();
        let to_name = to;
        let (to, text, summary) = (to.to_string(), text.to_string(), summary.map(String::from));
        let r =
            tokio::task::spawn_blocking(move || mailbox.send(&to, &text, summary.as_deref(), FromKind::Agent)).await;
        match r {
            Ok(Ok(m)) => {
                let name = self.mailbox.peers().into_iter().find(|p| p.id == m.to).map_or(m.to.clone(), |p| p.name);
                ToolOutput::ok(format!("Sent {} to {name} (delivered when it is idle)", m.id))
            }
            Ok(Err(e)) => {
                let fact = if e.starts_with("Not sent: no live session") { job_fact(to_name, &ctx.jobs) } else { None };
                ToolOutput::error(match fact {
                    Some(f) => format!("{e}\n{f}"),
                    None => e,
                })
            }
            Err(e) => ToolOutput::error(format!("Not sent: {e}")),
        }
    }
}

/// What `to` is when it is not a session but a job of this one: its number, the sub-agent's name or its description.
/// A sub-agent takes no message from the model (only the person writes to it), so the way to it is its output.
fn job_fact(to: &str, jobs: &nexo_core::jobs::Jobs) -> Option<String> {
    use nexo_core::jobs::JobKind;
    let list = jobs.list();
    let word = to.trim().trim_start_matches('#').trim();
    let mut ids: Vec<u32> = match word.parse::<u32>() {
        Ok(n) => list.iter().filter(|j| j.id == n).map(|j| j.id).collect(),
        Err(_) => list
            .iter()
            .filter(|j| j.kind == JobKind::Agent && j.label.eq_ignore_ascii_case(word))
            .map(|j| j.id)
            .collect(),
    };
    if ids.is_empty() && word.parse::<u32>().is_err() {
        // a sub-agent's name: its run id ends with the job number
        for s in jobs.subs().list() {
            if s.profile.eq_ignore_ascii_case(word) {
                ids.extend(s.id.rsplit('#').next().and_then(|n| n.parse::<u32>().ok()));
            }
        }
        ids.sort_unstable();
        ids.dedup();
    }
    let jobs_of: Vec<_> = ids.iter().filter_map(|id| list.iter().find(|j| j.id == *id)).collect();
    match jobs_of.as_slice() {
        [] => None,
        [j] => {
            let what = match j.kind {
                JobKind::Agent => format!("sub-agent \"{}\"", j.label),
                JobKind::Shell => format!("command `{}`", j.label),
            };
            let how = match j.kind {
                JobKind::Agent => format!("it takes no message from you; its output comes with `jobs output {}`", j.id),
                JobKind::Shell => format!("its output comes with `jobs output {}`", j.id),
            };
            Some(format!("\"{word}\" is your background job {} ({what}, {}): {how}.", j.id, j.status.plain()))
        }
        many => {
            let nums: Vec<String> = many.iter().take(5).map(|j| j.id.to_string()).collect();
            Some(format!(
                "\"{word}\" fits your background jobs {}: a sub-agent takes no message from you; its output comes with `jobs output <number>`.",
                nums.join(", ")
            ))
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
        assert!(
            none.is_error && none.content == "Not sent: no live session \"ghost\". Live sessions: web.",
            "{}",
            none.content
        );
        assert!(send.run(json!({"to": "web"}), &ctx()).await.is_error);
        assert_eq!(
            send.summary(&json!({"to": "web", "text": "run the tests\nplease"})),
            "message to web: run the tests"
        );
    }

    #[tokio::test]
    async fn a_message_to_itself_says_so_and_names_who_else_is_live() {
        let (_d, a, _b) = pair();
        let ts = tools(a);
        let out = ts.get("send_message").unwrap().run(json!({"to": "api", "text": "hi"}), &ctx()).await;
        assert!(
            out.is_error
                && out.content
                    == "Not sent: \"api\" is this session, the message would go to itself. Live sessions: web.",
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn a_job_number_or_sub_agent_is_named_with_the_way_to_read_it() {
        use nexo_core::jobs::JobKind;
        let (_d, a, _b) = pair();
        let ts = tools(a);
        let send = ts.get("send_message").unwrap();
        let c = ctx();
        let _shell = c.jobs.start(JobKind::Shell, "npm test");
        let agent = c.jobs.start(JobKind::Agent, "review the parser");
        let _sub = c.jobs.subs().begin(&format!("call#{}", agent.id()), "explore", "review the parser", true);
        let say = |to: &str| json!({"to": to, "text": "hi"});

        let out = send.run(say("2"), &c).await;
        assert!(out.is_error, "{}", out.content);
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.len(), 2, "{}", out.content);
        assert_eq!(lines[0], "Not sent: no live session \"2\". Live sessions: web.");
        assert_eq!(
            lines[1],
            "\"2\" is your background job 2 (sub-agent \"review the parser\", running): it takes no message from you; \
             its output comes with `jobs output 2`."
        );

        let shell = send.run(say("#1"), &c).await;
        assert!(
            shell.content.ends_with(
                "\"1\" is your background job 1 (command `npm test`, running): its output comes with `jobs output 1`."
            ),
            "{}",
            shell.content
        );
        let by_name = send.run(say("Explore"), &c).await;
        assert!(by_name.content.contains("background job 2 (sub-agent"), "{}", by_name.content);
        let by_text = send.run(say("review the parser"), &c).await;
        assert!(by_text.content.contains("`jobs output 2`"), "{}", by_text.content);
        let none = send.run(say("99"), &c).await;
        assert_eq!(none.content.lines().count(), 1, "{}", none.content);
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
