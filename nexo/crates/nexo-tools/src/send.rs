//! `send_to_session`: type a message into another live session, as its person would, Enter included. A
//! Claude Code session gets it through its inbox socket, a program inside tmux through its pane, and a nexo
//! session through its mailbox. Every call is asked for, since it types into another program.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::mail::FromKind;
use nexo_core::mailbox::Mailbox;
use nexo_core::reach::{self, Reach};
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

pub struct SendToSession {
    mailbox: Option<Arc<Mailbox>>,
    reach: Reach,
}

impl SendToSession {
    pub fn new(mailbox: Option<Arc<Mailbox>>, reach: Reach) -> SendToSession {
        SendToSession { mailbox, reach }
    }
}

/// The sessions of other programs that can be typed into now, as one line for an error to end with.
fn reachable_line(reach: &Reach) -> String {
    let found = reach.discover();
    if found.is_empty() {
        return "No session of another program can be typed into right now.".into();
    }
    let list: Vec<String> = found
        .iter()
        .take(12)
        .map(|r| {
            if r.cwd.is_empty() {
                format!("{} ({})", r.address, r.program)
            } else {
                format!("{} ({}, {})", r.address, r.program, nexo_core::paths::shown(std::path::Path::new(&r.cwd)))
            }
        })
        .collect();
    format!("Reachable now: {}.", list.join(", "))
}

#[async_trait]
impl Tool for SendToSession {
    fn name(&self) -> &str {
        "send_to_session"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send_to_session".into(),
            description: "Type a message into another live session on this machine, as if its person typed it and pressed \
                          Enter: it becomes that session's next prompt. `to` is `claude:<pid>` for a Claude Code session, \
                          `tmux:<pane>` for a program running in tmux (Codex, a shell…), `pid:<n>` for a process (its \
                          inbox, else its tmux pane), or the name of a nexo session (it goes to its mailbox). Line breaks \
                          become spaces in a terminal. Do not paste secrets."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "claude:<pid>, tmux:<pane>, pid:<n>, or a nexo session's name." },
                    "text": { "type": "string", "description": "What to type." }
                },
                "required": ["to", "text"]
            }),
        }
    }

    /// It types into another program, so it is always asked for, and refused in plan mode.
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let to = args.get("to").and_then(Value::as_str).unwrap_or("?");
        let text = reach::one_line(args.get("text").and_then(Value::as_str).unwrap_or(""));
        let short: String = text.chars().take(60).collect();
        let dots = if short.len() < text.len() { "…" } else { "" };
        format!("type into {to}: {short}{dots}")
    }

    async fn run(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let (Some(to), Some(text)) = (args.get("to").and_then(Value::as_str), args.get("text").and_then(Value::as_str))
        else {
            return ToolOutput::error("`to` and `text` are required");
        };
        let (to, text, reach) = (to.to_string(), text.to_string(), self.reach.clone());
        match reach::parse(&to) {
            Some(Err(e)) => ToolOutput::error(format!("{e}. {}", reachable_line(&reach))),
            Some(Ok(target)) => {
                let r = tokio::task::spawn_blocking(move || {
                    reach.send(&target, &text).map_err(|e| format!("{e} {}", reachable_line(&reach)))
                })
                .await;
                match r {
                    Ok(Ok(who)) => ToolOutput::ok(format!("Typed into {who} and pressed Enter")),
                    Ok(Err(e)) => ToolOutput::error(e),
                    Err(e) => ToolOutput::error(format!("Not sent: {e}")),
                }
            }
            None => {
                let Some(mailbox) = self.mailbox.clone() else {
                    return ToolOutput::error(format!(
                        "Not sent: \"{to}\" is not an address of another program, and this session has no mailbox for \
                         nexo sessions. {}",
                        reachable_line(&reach)
                    ));
                };
                let r = tokio::task::spawn_blocking(move || {
                    let m = mailbox
                        .send(&to, &text, None, FromKind::Agent)
                        .map_err(|e| format!("{e}. {}", reachable_line(&reach)))?;
                    let name = mailbox.peers().into_iter().find(|p| p.id == m.to).map_or(m.to.clone(), |p| p.name);
                    Ok::<_, String>((m.id, name))
                })
                .await;
                match r {
                    Ok(Ok((id, name))) => {
                        ToolOutput::ok(format!("Sent {id} to nexo session {name} (delivered when it is idle)"))
                    }
                    Ok(Err(e)) => ToolOutput::error(e),
                    Err(e) => ToolOutput::error(format!("Not sent: {e}")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::mail::Inbound;

    fn reach(dir: &std::path::Path) -> Reach {
        Reach {
            cc_socks: Some(dir.join("cc-socks")),
            claude_home: Some(dir.join("claude")),
            tmux_name: Some(format!("nexo-send-tool-{}", std::process::id())),
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn a_nexo_name_goes_to_the_mailbox_and_an_unknown_address_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("mail");
        let a = Mailbox::open(&root, "aaaa1111", "api", "/w/api", false, Inbound::Auto).unwrap();
        let b = Mailbox::open(&root, "bbbb2222", "web", "/w/web", false, Inbound::Auto).unwrap();
        let t = SendToSession::new(Some(a), reach(dir.path()));
        assert_eq!(t.kind(), ToolKind::Other);
        let out = t.run(json!({"to": "web", "text": "the build is green"}), &ctx()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.ends_with("to nexo session web (delivered when it is idle)"), "{}", out.content);
        b.set_idle(true);
        assert_eq!(b.tick().deliver[0].text, "the build is green");

        let out = t.run(json!({"to": "fax:abc", "text": "hi"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("not an address nexo can type into"), "{}", out.content);
        assert!(out.content.ends_with("No session of another program can be typed into right now."), "{}", out.content);
        let out = t.run(json!({"to": "pid:999999999", "text": "hi"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("does not run inside tmux"), "{}", out.content);
        let lone = SendToSession::new(None, reach(dir.path()));
        let out = lone.run(json!({"to": "web", "text": "hi"}), &ctx()).await;
        assert!(out.is_error && out.content.contains("no mailbox"), "{}", out.content);
        assert_eq!(
            t.summary(&json!({"to": "claude:12", "text": "run the tests\nplease"})),
            "type into claude:12: run the tests please"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_claude_code_session_gets_a_prompt() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cc-socks")).unwrap();
        let pid = std::process::id();
        let listener =
            std::os::unix::net::UnixListener::bind(dir.path().join("cc-socks").join(format!("{pid}.sock"))).unwrap();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut got = String::new();
            c.read_to_string(&mut got).unwrap();
            got
        });
        let t = SendToSession::new(None, reach(dir.path()));
        let out = t.run(json!({"to": format!("claude:{pid}"), "text": "hi bro"}), &ctx()).await;
        assert_eq!(out.content, format!("Typed into Claude Code session {pid} and pressed Enter"));
        let got = server.join().unwrap();
        assert!(got.ends_with('\n') && got.lines().count() == 1, "{got}");
        let v: Value = serde_json::from_str(got.trim()).unwrap();
        assert_eq!(v, json!({"type": "user", "message": {"role": "user", "content": "hi bro"}}));
    }
}
