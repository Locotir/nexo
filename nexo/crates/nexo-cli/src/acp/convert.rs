//! nexo's agent events and tool outputs as ACP `session/update` bodies, and the
//! client's content blocks and permission answers as nexo's inputs. Pure
//! functions on JSON, so the wire shapes are tested without a process.

use std::path::{Path, PathBuf};

use nexo_core::agent::AgentEvent;
use nexo_core::{
    PermissionDecision, PermissionMode, PermissionRequest, Question, Role, TodoStatus, ToolDisplay, ToolKind,
    ToolOutput, ToolSet,
};
use serde_json::{json, Value};

/// The most characters of one tool result sent to the editor (the model got all of it).
const OUTPUT_CHARS: usize = 20_000;

/// The permission modes the editor can pick from.
pub fn modes() -> Vec<(PermissionMode, &'static str)> {
    vec![
        (PermissionMode::Ask, "Ask before edits and commands"),
        (PermissionMode::AcceptEdits, "Edits go through; ask before commands"),
        (PermissionMode::Auto, "Never ask"),
        (PermissionMode::ReadOnly, "Only read and search"),
        (PermissionMode::Plan, "Read and search, then propose a plan"),
    ]
}

pub fn modes_state(current: PermissionMode) -> Value {
    let available: Vec<Value> =
        modes().into_iter().map(|(m, d)| json!({"id": m.as_str(), "name": mode_name(m), "description": d})).collect();
    json!({"currentModeId": current.as_str(), "availableModes": available})
}

fn mode_name(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Ask => "Ask",
        PermissionMode::AcceptEdits => "Accept edits",
        PermissionMode::Auto => "Auto",
        PermissionMode::ReadOnly => "Read-only",
        PermissionMode::Plan => "Plan",
    }
}

/// A mode id the editor sent; only the ids `modes_state` offers.
pub fn parse_mode(id: &str) -> Option<PermissionMode> {
    modes().into_iter().map(|(m, _)| m).find(|m| m.as_str() == id)
}

pub fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

pub fn mode_update(mode: PermissionMode) -> Value {
    json!({"sessionUpdate": "current_mode_update", "currentModeId": mode.as_str()})
}

/// The ACP kind of a call, from its tool.
pub fn tool_kind(tools: &ToolSet, name: &str, args: &Value) -> &'static str {
    if name == "exit_plan" {
        return "switch_mode";
    }
    let kind = tools.get(name).map(|t| t.call_kind(args)).unwrap_or(ToolKind::Other);
    kind_name(kind)
}

fn kind_name(k: ToolKind) -> &'static str {
    match k {
        ToolKind::Read => "read",
        ToolKind::Search => "search",
        ToolKind::Write => "edit",
        ToolKind::Exec => "execute",
        ToolKind::Network => "fetch",
        ToolKind::Other => "other",
    }
}

/// The file a call works on, as an absolute path.
pub fn call_path(cwd: &Path, args: &Value) -> Option<PathBuf> {
    let p = args.get("path").or_else(|| args.get("file_path")).and_then(Value::as_str)?;
    Some(nexo_tools::util::resolve(cwd, p))
}

fn locations(cwd: &Path, args: &Value) -> Value {
    match call_path(cwd, args) {
        Some(p) => {
            let mut loc = json!({"path": p.display().to_string()});
            if let Some(line) = args.get("offset").and_then(Value::as_u64) {
                loc["line"] = json!(line);
            }
            json!([loc])
        }
        None => json!([]),
    }
}

/// The `tool_call` update for a call the model asked for.
pub fn tool_call(tools: &ToolSet, cwd: &Path, id: &str, name: &str, args: &Value, summary: &str) -> Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": id,
        "title": summary,
        "kind": tool_kind(tools, name, args),
        "status": "pending",
        "locations": locations(cwd, args),
        "rawInput": args,
    })
}

pub fn tool_status(id: &str, status: &str) -> Value {
    json!({"sessionUpdate": "tool_call_update", "toolCallId": id, "status": status})
}

fn diff_content(path: &str, old: &str, new: &str) -> Value {
    json!({"type": "diff", "path": path, "oldText": if old.is_empty() { Value::Null } else { json!(old) }, "newText": new})
}

fn text_content(text: &str) -> Value {
    json!({"type": "content", "content": {"type": "text", "text": text}})
}

fn cap(s: &str) -> String {
    nexo_tools::util::cap_text(s, OUTPUT_CHARS)
}

/// The `tool_call_update` that ends a call: its status, and what it did as diff or text.
/// `path` is the absolute file the call worked on (the output's own path may carry a note);
/// `terminal` the editor terminal the call ran in.
pub fn tool_done(id: &str, out: &ToolOutput, path: Option<&Path>, terminal: Option<&str>) -> Value {
    let content = match &out.display {
        // the editor shows the output in its terminal; the call keeps it there
        _ if terminal.is_some() => vec![json!({"type": "terminal", "terminalId": terminal})],
        ToolDisplay::Diff { path: shown, old, new, .. } if !out.is_error => {
            let p = path.map(|p| p.display().to_string()).unwrap_or_else(|| shown.clone());
            vec![diff_content(&p, old, new)]
        }
        ToolDisplay::Todo { .. } => vec![text_content("task list updated")],
        _ => vec![text_content(&cap(&out.content))],
    };
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": id,
        "status": if out.is_error { "failed" } else { "completed" },
        "content": content,
        "rawOutput": {"content": cap(&out.content), "isError": out.is_error},
    })
}

/// The `plan` update for the model's task list.
pub fn plan(out: &ToolOutput) -> Option<Value> {
    let ToolDisplay::Todo { items } = &out.display else { return None };
    let entries: Vec<Value> = items
        .iter()
        .map(|i| {
            let status = match i.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Done => "completed",
            };
            json!({"content": i.text, "priority": "medium", "status": status})
        })
        .collect();
    Some(json!({"sessionUpdate": "plan", "entries": entries}))
}

/// The text the model reads for the content blocks of a prompt.
pub fn prompt_text(blocks: &[Value]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => parts.extend(b.get("text").and_then(Value::as_str).map(String::from)),
            Some("resource_link") => {
                let uri = b.get("uri").and_then(Value::as_str).unwrap_or("");
                parts.push(format!("[{}]({})", b.get("name").and_then(Value::as_str).unwrap_or(uri), uri));
            }
            Some("resource") => {
                let r = &b["resource"];
                let uri = r.get("uri").and_then(Value::as_str).unwrap_or("");
                match r.get("text").and_then(Value::as_str) {
                    Some(t) => parts.push(format!("<file uri=\"{uri}\">\n{t}\n</file>")),
                    None => parts.push(format!("[binary resource {uri}]")),
                }
            }
            Some(other) => parts.push(format!("[a {other} block was attached; nexo acp reads text only]")),
            None => {}
        }
    }
    parts.join("\n\n")
}

/// The options of a permission request: allow, allow always (when nexo has a rule for it), reject.
pub fn permission_options(req: &PermissionRequest) -> Value {
    let mut o = vec![json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"})];
    if let Some(rule) = req.suggested_rules.first() {
        o.push(json!({"optionId": "always", "name": format!("Always allow {rule}"), "kind": "allow_always"}));
    }
    o.push(json!({"optionId": "reject", "name": "Reject", "kind": "reject_once"}));
    Value::Array(o)
}

/// The `toolCall` part of a permission request: what is about to run, with its diff or command.
pub fn permission_tool_call(tools: &ToolSet, cwd: &Path, req: &PermissionRequest, args: Option<&Value>) -> Value {
    let content = match &req.detail {
        ToolDisplay::Diff { path, old, new, .. } => {
            let abs = nexo_tools::util::resolve(cwd, path).display().to_string();
            vec![diff_content(&abs, old, new)]
        }
        ToolDisplay::Shell { command, .. } => vec![text_content(command)],
        ToolDisplay::List { items } => vec![text_content(&items.join("\n"))],
        _ => vec![],
    };
    let none = Value::Null;
    json!({
        "toolCallId": req.id,
        "title": req.summary,
        "kind": tool_kind(tools, &req.tool, args.unwrap_or(&none)),
        "status": "pending",
        "content": content,
        "locations": locations(cwd, args.unwrap_or(&none)),
    })
}

/// What the person's answer to `session/request_permission` means to nexo.
pub fn decision(result: &Value, req: &PermissionRequest) -> PermissionDecision {
    let out = &result["outcome"];
    if out["outcome"] != "selected" {
        return PermissionDecision::Deny { reason: Some("cancelled in the editor".into()) };
    }
    match out["optionId"].as_str() {
        Some("allow") => PermissionDecision::Allow,
        Some("always") => match req.suggested_rules.first() {
            Some(rule) => PermissionDecision::AllowAlways { rule: rule.clone(), persist: false },
            None => PermissionDecision::Allow,
        },
        _ => PermissionDecision::Deny { reason: Some("rejected in the editor".into()) },
    }
}

/// The `toolCall` and options that put a question of the agent (a plan to approve, a choice) to the person.
pub fn question_request(q: &Question) -> Value {
    let options: Vec<Value> = q
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| json!({"optionId": format!("q{i}"), "name": o.label, "kind": "allow_once"}))
        .collect();
    let mut content = Vec::new();
    if let Some(p) = &q.plan {
        content.push(text_content(p));
    }
    let title = if q.header.is_empty() { q.question.clone() } else { format!("{}: {}", q.header, q.question) };
    json!({
        "toolCall": {"toolCallId": q.id, "title": title, "kind": "other", "status": "pending", "content": content},
        "options": options,
    })
}

/// The label picked for a question, None when it was cancelled.
pub fn answer(result: &Value, q: &Question) -> Option<String> {
    let out = &result["outcome"];
    if out["outcome"] != "selected" {
        return None;
    }
    let id = out["optionId"].as_str()?;
    let i: usize = id.strip_prefix('q')?.parse().ok()?;
    q.options.get(i).map(|o| o.label.clone())
}

/// The updates that replay a recorded conversation to the editor (`session/load`).
pub fn replay(events: &[AgentEvent], tools: &ToolSet, cwd: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let mut paths = std::collections::HashMap::new();
    for ev in events {
        match ev {
            AgentEvent::History { message, .. } => {
                if message.is_tool_results() {
                    continue;
                }
                let text = message.text();
                if text.is_empty() {
                    continue;
                }
                let kind = if message.role == Role::User { "user_message_chunk" } else { "agent_message_chunk" };
                out.push(chunk(kind, &text));
            }
            AgentEvent::ToolCall { id, name, args, summary } => {
                if let Some(p) = call_path(cwd, args) {
                    paths.insert(id.clone(), p);
                }
                out.push(tool_call(tools, cwd, id, name, args, summary));
            }
            AgentEvent::ToolDone { id, output, .. } => {
                out.push(tool_done(id, output, paths.get(id).map(PathBuf::as_path), None));
                out.extend(plan(output));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::TodoItem;

    #[test]
    fn a_write_ends_as_a_diff_with_no_old_text_for_a_new_file() {
        let out = ToolOutput::ok("created a.rs").with_display(ToolDisplay::Diff {
            path: "a.rs (outside the project)".into(),
            old: String::new(),
            new: "fn main() {}\n".into(),
            start_line: 1,
        });
        let u = tool_done("c1", &out, Some(Path::new("/w/a.rs")), None);
        assert_eq!(u["status"], "completed");
        assert_eq!(u["content"][0]["type"], "diff");
        assert_eq!(u["content"][0]["path"], "/w/a.rs");
        assert!(u["content"][0]["oldText"].is_null());
    }

    #[test]
    fn a_failed_call_is_failed_with_its_message() {
        let u = tool_done("c1", &ToolOutput::error("edit: not found"), None, None);
        assert_eq!(u["status"], "failed");
        assert_eq!(u["content"][0]["content"]["text"], "edit: not found");
    }

    #[test]
    fn a_call_run_in_a_terminal_ends_with_the_terminal() {
        let u = tool_done("c1", &ToolOutput::ok("hi"), None, Some("t9"));
        assert_eq!(u["content"], json!([{"type": "terminal", "terminalId": "t9"}]));
        assert_eq!(u["rawOutput"]["content"], "hi");
    }

    #[test]
    fn the_todo_list_becomes_a_plan() {
        let out = ToolOutput::ok("ok").with_display(ToolDisplay::Todo {
            items: vec![
                TodoItem { text: "a".into(), status: TodoStatus::Done },
                TodoItem { text: "b".into(), status: TodoStatus::InProgress },
            ],
        });
        let p = plan(&out).unwrap();
        assert_eq!(p["entries"][0]["status"], "completed");
        assert_eq!(p["entries"][1]["status"], "in_progress");
    }

    #[test]
    fn prompt_blocks_read_as_text() {
        let t = prompt_text(&[
            json!({"type": "text", "text": "fix this"}),
            json!({"type": "resource", "resource": {"uri": "file:///w/a.rs", "text": "let x;"}}),
            json!({"type": "image", "data": "", "mimeType": "image/png"}),
        ]);
        assert!(t.starts_with("fix this\n\n<file uri=\"file:///w/a.rs\">\nlet x;\n</file>"), "{t}");
        assert!(t.contains("image block"), "{t}");
    }

    #[test]
    fn modes_round_trip_and_unknown_ones_are_refused() {
        for (m, _) in modes() {
            assert_eq!(parse_mode(m.as_str()), Some(m));
        }
        assert_eq!(parse_mode("yolo"), None);
    }

    #[test]
    fn answers_map_to_decisions() {
        let req = PermissionRequest {
            id: "c1".into(),
            tool: "bash".into(),
            kind: ToolKind::Exec,
            summary: "bash: ls".into(),
            detail: ToolDisplay::Plain,
            suggested_rules: vec!["bash(ls *)".into()],
        };
        let sel = |id: &str| json!({"outcome": {"outcome": "selected", "optionId": id}});
        assert_eq!(decision(&sel("allow"), &req), PermissionDecision::Allow);
        assert_eq!(
            decision(&sel("always"), &req),
            PermissionDecision::AllowAlways { rule: "bash(ls *)".into(), persist: false }
        );
        assert!(matches!(decision(&sel("reject"), &req), PermissionDecision::Deny { .. }));
        assert!(matches!(
            decision(&json!({"outcome": {"outcome": "cancelled"}}), &req),
            PermissionDecision::Deny { .. }
        ));
    }
}
