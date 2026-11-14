//! `send_message` and `list_sessions`: talk to the other nexo sessions running on this machine. A message
//! lands in the other session's mailbox and reaches its model at its next idle point, as a note that says
//! it came from a session, not from the person. `send_message` also reaches this session's own running
//! sub-agents by job number, in process, before their next step. Not part of `local()`: they exist only
//! where a mailbox does.

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
                "Write to another nexo session running on this machine (see `list_sessions`), or to one of this session's \
                 own running sub-agents by its job number. The text reaches a session's model \
                 when that session is idle, marked as coming from a session and not from the person; it cannot approve \
                 permissions or change settings there. Use it to hand over a finding or ask for one, not to chat: at most \
                 {MAIL_MAX_BYTES} bytes, a few messages a minute, so batch what you have. Do not paste secrets."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "The session: its name, its id or the start of it, or `name [ref]` when two share a name; or a sub-agent's job number."
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
        let (to, owned, summary_owned) = (to.to_string(), text.to_string(), summary.map(String::from));
        let r =
            tokio::task::spawn_blocking(move || mailbox.send(&to, &owned, summary_owned.as_deref(), FromKind::Agent))
                .await;
        match r {
            Ok(Ok(m)) => {
                let name = self.mailbox.peers().into_iter().find(|p| p.id == m.to).map_or(m.to.clone(), |p| p.name);
                ToolOutput::ok(format!("Sent {} to {name} (delivered when it is idle)", m.id))
            }
            Ok(Err(e)) if e.starts_with("Not sent: no live session") => {
                let (word, named) = jobs_named(to_name, &ctx.jobs);
                if let [j] = named.as_slice() {
                    if let Some(out) = self.to_sub_agent(j, text, summary, &ctx.jobs) {
                        return out;
                    }
                }
                ToolOutput::error(match job_fact(&word, &named) {
                    Some(f) => format!("{e}\n{f}"),
                    None => e,
                })
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("Not sent: {e}")),
        }
    }
}

impl SendMessage {
    /// `text` into the inbox of sub-agent job `j`, read before its next step and marked as the parent's. None
    /// when `j` is no sub-agent run (a command, a workflow's own job): the caller says what it is instead.
    fn to_sub_agent(
        &self,
        j: &nexo_core::jobs::JobInfo,
        text: &str,
        summary: Option<&str>,
        jobs: &nexo_core::jobs::Jobs,
    ) -> Option<ToolOutput> {
        if j.kind != nexo_core::jobs::JobKind::Agent {
            return None;
        }
        let who = format!("sub-agent job {} \"{}\"", j.id, j.label);
        let ended = || {
            ToolOutput::error(format!(
                "Not sent: {who} has already ended, so it reads no more messages; its output comes with `jobs output {}`.",
                j.id
            ))
        };
        if !j.status.running() {
            return Some(ended());
        }
        let subs = jobs.subs();
        let n = j.id.to_string();
        // a sub-agent's run id ends with its job number
        let run = subs.list().into_iter().find(|s| s.id.rsplit('#').next() == Some(n.as_str()))?;
        Some(match subs.submit_from_parent(&run.id, text) {
            Ok(()) => {
                self.mailbox.sent_to_job(j.id, text, summary);
                ToolOutput::ok(format!("Sent to {who} (it reads it before its next step)"))
            }
            Err(why) if why == "that sub-agent has already ended" => ended(),
            Err(why) => ToolOutput::error(format!("Not sent to {who}: {why}.")),
        })
    }
}

/// The jobs of this session `to` names: its number (`3`, `#3`), a sub-agent's description or its name. The
/// word comes back as it was matched.
fn jobs_named(to: &str, jobs: &nexo_core::jobs::Jobs) -> (String, Vec<nexo_core::jobs::JobInfo>) {
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
    let named = ids.iter().filter_map(|id| list.iter().find(|j| j.id == *id)).cloned().collect();
    (word.to_string(), named)
}

/// What `word` is when it names jobs of this session that a message does not reach: a command, a workflow's
/// own job, or more than one job. The way to any of them is its output.
fn job_fact(word: &str, named: &[nexo_core::jobs::JobInfo]) -> Option<String> {
    use nexo_core::jobs::JobKind;
    match named {
        [] => None,
        [j] => {
            let (what, how) = match j.kind {
                JobKind::Agent => (format!("sub-agent \"{}\"", j.label), "it takes no message; "),
                JobKind::Shell => (format!("command `{}`", j.label), ""),
            };
            Some(format!(
                "\"{word}\" is your background job {} ({what}, {}): {how}its output comes with `jobs output {}`.",
                j.id,
                j.status.plain(),
                j.id
            ))
        }
        many => {
            let nums: Vec<String> = many.iter().take(5).map(|j| j.id.to_string()).collect();
            Some(format!(
                "\"{word}\" fits your background jobs {}: name one by its number; its output comes with `jobs output <number>`.",
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
    async fn a_running_sub_agent_gets_the_message_by_job_number_name_or_description() {
        use nexo_core::agent::AgentEvent;
        use nexo_core::jobs::JobKind;
        use nexo_core::subs::SubItem;
        let (_d, a, _b) = pair();
        let ts = tools(a.clone());
        let send = ts.get("send_message").unwrap();
        let c = ctx();
        let _shell = c.jobs.start(JobKind::Shell, "npm test");
        let agent = c.jobs.start(JobKind::Agent, "review the parser");
        let sub = c.jobs.subs().begin(&format!("call#{}", agent.id()), "explore", "review the parser", true);
        let say = |to: &str, text: &str| json!({"to": to, "text": text});

        let out = send.run(say("2", "also check lexer.rs\nline two"), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "Sent to sub-agent job 2 \"review the parser\" (it reads it before its next step)");
        assert_eq!(sub.take_inbox(), vec![SubItem::Parent { text: "also check lexer.rs\nline two".into() }]);
        let shown: Vec<(String, String)> = a
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::MailSent { to_name, summary, .. } => Some((to_name, summary)),
                _ => None,
            })
            .collect();
        assert_eq!(shown, [("job 2".to_string(), "also check lexer.rs".to_string())]);

        for to in ["#2", "Explore", "review the parser"] {
            assert!(send.run(say(to, "hi"), &c).await.content.starts_with("Sent to sub-agent job 2"), "{to}");
        }
        assert_eq!(sub.take_inbox().len(), 3);
    }

    #[tokio::test]
    async fn an_ended_sub_agent_a_command_and_an_unknown_job_say_what_they_are() {
        use nexo_core::jobs::JobKind;
        use nexo_core::subs::SubState;
        let (_d, a, _b) = pair();
        let ts = tools(a.clone());
        let send = ts.get("send_message").unwrap();
        let c = ctx();
        let _shell = c.jobs.start(JobKind::Shell, "npm test");
        let agent = c.jobs.start(JobKind::Agent, "review the parser");
        let sub = c.jobs.subs().begin(&format!("call#{}", agent.id()), "explore", "review the parser", true);
        let say = |to: &str| json!({"to": to, "text": "hi"});

        // the run ended but its job is not marked finished yet: the same fact
        sub.end(SubState::Ok);
        let ended = "Not sent: sub-agent job 2 \"review the parser\" has already ended, so it reads no more messages; \
                     its output comes with `jobs output 2`.";
        let out = send.run(say("2"), &c).await;
        assert!(out.is_error);
        assert_eq!(out.content, ended);
        agent.finish(true, "reported");
        assert_eq!(send.run(say("2"), &c).await.content, ended);

        let shell = send.run(say("#1"), &c).await;
        assert!(shell.is_error);
        let lines: Vec<&str> = shell.content.lines().collect();
        assert_eq!(
            lines,
            [
                "Not sent: no live session \"#1\". Live sessions: web.",
                "\"1\" is your background job 1 (command `npm test`, running): its output comes with `jobs output 1`."
            ]
        );
        let none = send.run(say("99"), &c).await;
        assert_eq!(none.content, "Not sent: no live session \"99\". Live sessions: web.");
        assert!(!a.drain_events().iter().any(|e| matches!(e, nexo_core::agent::AgentEvent::MailSent { .. })));
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
