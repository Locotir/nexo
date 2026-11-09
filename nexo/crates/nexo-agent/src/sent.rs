//! `/sent`: the last request as it was sent, cut into sections for the person to read: the system prompt
//! with its parts, the tools offered, then every message part by part. What the context curator and the
//! supervisor put in is tagged. Display only: nothing here changes what is sent.

use nexo_core::agent::SentSection;
use nexo_core::message::{result_text, Part, Role};
use nexo_core::provider::ModelRequest;

/// Marks the text the context curator put in.
const CURATOR: &str = "[curator]";
/// Where a hidden supervisor note begins.
const SUPERVISOR: &str = "<system-reminder>\nAn automatic check on this session";

/// About how many tokens `text` takes (4 characters a token).
fn tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

fn section(heading: impl Into<String>, depth: u8, tag: Option<&str>, text: String, collapse: bool) -> SentSection {
    SentSection { heading: heading.into(), depth, tag: tag.map(str::to_string), tokens: tokens(&text), text, collapse }
}

/// The sections of `req`, in the order they were sent.
pub fn sections(req: &ModelRequest) -> Vec<SentSection> {
    let mut out = Vec::new();
    out.push(SentSection {
        heading: "System prompt".into(),
        depth: 0,
        tag: None,
        tokens: tokens(&req.system),
        text: String::new(),
        collapse: false,
    });
    for (heading, tag, text) in system_parts(&req.system) {
        out.push(section(heading, 1, tag, text.to_string(), false));
    }
    if !req.tools.is_empty() {
        let sizes: Vec<usize> = req
            .tools
            .iter()
            .map(|t| t.name.len() + t.description.len() + serde_json::to_string(&t.parameters).map_or(0, |s| s.len()))
            .collect();
        let list: Vec<String> =
            req.tools.iter().zip(&sizes).map(|(t, n)| format!("{} · ~{} tokens", t.name, n.div_ceil(4))).collect();
        let total: usize = sizes.iter().sum();
        out.push(SentSection {
            heading: format!("Tools offered · {}", req.tools.len()),
            depth: 0,
            tag: None,
            tokens: total.div_ceil(4) as u64,
            text: list.join("\n"),
            collapse: true,
        });
    }
    // the name of a call, to head its result with it
    let mut calls: Vec<(&str, &str)> = Vec::new();
    for m in &req.messages {
        for p in &m.content {
            if let Part::ToolCall { id, name, .. } = p {
                calls.push((id, name));
            }
        }
    }
    for (i, m) in req.messages.iter().enumerate() {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let n = i + 1;
        for p in &m.content {
            out.push(match p {
                Part::Text { text } => {
                    let (what, tag) = classify(text);
                    let heading = match what {
                        Some(w) => format!("#{n} {role} · {w}"),
                        None => format!("#{n} {role}"),
                    };
                    section(heading, 0, tag, text.clone(), false)
                }
                Part::ToolCall { name, args, .. } => {
                    section(format!("#{n} {role} · call {name}"), 0, None, args.to_string(), false)
                }
                Part::ToolResult { call_id, content, is_error, .. } => {
                    let name = calls.iter().find(|(id, _)| id == call_id).map_or("tool", |(_, n)| n);
                    let text = result_text(content);
                    let tag = text.trim_start().starts_with(CURATOR).then_some("curator");
                    let err = if *is_error { " (error)" } else { "" };
                    section(format!("#{n} {role} · result of {name}{err}"), 0, tag, text, true)
                }
                Part::Reasoning { text, .. } => {
                    section(format!("#{n} {role} · reasoning"), 0, None, text.clone(), true)
                }
                Part::RedactedReasoning { .. } => {
                    section(format!("#{n} {role} · reasoning (withheld)"), 0, None, String::new(), false)
                }
                Part::Image { .. } => section(format!("#{n} {role} · image"), 0, None, "[image]".into(), false),
            });
        }
    }
    out
}

/// What a text part of a message is, when the curator or the supervisor wrote it: a label and the tag.
fn classify(text: &str) -> (Option<&'static str>, Option<&'static str>) {
    let t = text.trim_start();
    if t.starts_with(SUPERVISOR) {
        return (Some("supervisor note (this request only)"), Some("supervisor"));
    }
    let Some(rest) = t.strip_prefix(CURATOR) else { return (None, None) };
    let rest = rest.trim_start();
    let what = [
        ("Messages 2 to", "anchor"),
        ("Standing instructions", "standing rules"),
        ("This conversation has other topics", "topic cards"),
        ("Facts from the messages", "fixed facts"),
        ("Files you wrote", "fixed facts: files written"),
        ("What you established", "fixed facts: notes"),
        ("Files the request names", "fixed facts: files named"),
        ("Brought back from earlier", "recalled outputs"),
        ("Before the next step", "NUDGE"),
    ]
    .into_iter()
    .find(|(head, _)| rest.starts_with(head))
    .map_or("note", |(_, w)| w);
    (Some(what), Some("curator"))
}

/// The system prompt in the parts it is made of, found by the lines that open each: `(heading, tag, text)`.
/// The first part is everything before the first of them (the built-in text, directory, date, environment
/// and tool guidance). A part not in this prompt (no goal, no instruction files) is left out.
fn system_parts(system: &str) -> Vec<(&'static str, Option<&'static str>, &str)> {
    let marks: [(&str, &str, Option<&str>); 5] = [
        ("\n\nInstructions from ", "instruction files", None),
        ("\n\nThe person's preferences:\n", "preferences", None),
        ("\n\n[curator] Working efficiently", "curator directives", Some("curator")),
        ("\n\nPlan mode is on.", "plan mode directive", None),
        ("\n\nGoal, ", "goal", None),
    ];
    let mut cuts: Vec<(usize, &str, Option<&str>)> = Vec::new();
    let mut from = 0;
    for (needle, heading, tag) in marks {
        if let Some(at) = system[from..].find(needle) {
            cuts.push((from + at, heading, tag));
            from += at + needle.len();
        }
    }
    // a prompt with the nested-instructions list but no instruction file of its own
    if !cuts.iter().any(|c| c.1 == "instruction files") {
        let before = cuts.first().map_or(system.len(), |c| c.0);
        if let Some(at) = system[..before].find("\n\nMore instruction files, ") {
            cuts.insert(0, (at, "instruction files", None));
        }
    }
    let first = cuts.first().map_or(system.len(), |c| c.0);
    let mut out = vec![("built-in text, directory, date, environment, tools", None, system[..first].trim())];
    for (k, (at, heading, tag)) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).map_or(system.len(), |c| c.0);
        out.push((*heading, *tag, system[*at..end].trim()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::message::Message;
    use nexo_core::provider::ToolSpec;
    use serde_json::json;

    fn request() -> ModelRequest {
        let system = format!(
            "You are nexo.\n\nWorking directory: /w\nDate: 2026-11-09\n\nInstructions from /w/AGENTS.md:\nUse tabs.\n\nMore \
             lines of the file.\n\nThe person's preferences:\nReply short.\n\n{}\n\nGoal, in the person's words: ship it",
            crate::context::CURATOR_DIRECTIVES
        );
        let mut req = ModelRequest::new("m", system, Vec::new());
        req.tools = vec![ToolSpec { name: "read".into(), description: "Reads a file.".into(), parameters: json!({}) }];
        let mut first = Message::user("fix the bug in a.rs");
        first
            .content
            .push(Part::text("[curator] Standing instructions the user gave during this session. They hold:\n- tabs"));
        first
            .content
            .push(Part::text("[curator] Before the next step, keep what these results settled with the `note` tool"));
        req.messages = vec![
            first,
            Message::new(
                Role::Assistant,
                vec![Part::ToolCall { id: "c1".into(), name: "read".into(), args: json!({"path": "a.rs"}) }],
            ),
            Message::new(
                Role::User,
                vec![
                    Part::ToolResult {
                        call_id: "c1".into(),
                        name: None,
                        content: vec![Part::text("line 1\nline 2\nline 3")],
                        is_error: false,
                    },
                    Part::text(crate::supervise::watch::hidden_block(&["the same file was read 3 times".to_string()])),
                ],
            ),
        ];
        req
    }

    #[test]
    fn the_system_prompt_is_cut_in_the_parts_it_is_made_of() {
        let s = sections(&request());
        assert_eq!(s[0].heading, "System prompt");
        let heads: Vec<&str> = s.iter().take_while(|x| x.depth <= 1).map(|x| x.heading.as_str()).collect();
        assert_eq!(
            &heads[..6],
            [
                "System prompt",
                "built-in text, directory, date, environment, tools",
                "instruction files",
                "preferences",
                "curator directives",
                "goal"
            ]
        );
        let files = s.iter().find(|x| x.heading == "instruction files").unwrap();
        assert!(files.text.contains("Use tabs.") && files.text.contains("More lines of the file."), "{}", files.text);
        assert!(!files.text.contains("Reply short."));
        let dir = s.iter().find(|x| x.heading == "curator directives").unwrap();
        assert_eq!(dir.tag.as_deref(), Some("curator"));
        assert!(dir.text.starts_with("[curator] Working efficiently") && !dir.text.contains("Goal,"));
        assert!(s.iter().find(|x| x.heading == "goal").unwrap().text.ends_with("ship it"));
        // the parts together hold the whole prompt, and the head says its size
        assert!(s[0].tokens >= s[1..6].iter().map(|x| x.tokens).sum::<u64>() - 6);
    }

    #[test]
    fn messages_come_part_by_part_with_the_curators_and_the_supervisors_tagged() {
        let s = sections(&request());
        let find = |h: &str| s.iter().find(|x| x.heading.contains(h)).unwrap_or_else(|| panic!("no {h}: {s:?}"));
        assert_eq!(find("#1 user").tag, None);
        assert_eq!(find("#1 user").text, "fix the bug in a.rs");
        assert_eq!(find("standing rules").tag.as_deref(), Some("curator"));
        assert_eq!(find("NUDGE").tag.as_deref(), Some("curator"));
        assert_eq!(find("call read").text, r#"{"path":"a.rs"}"#);
        let r = find("result of read");
        assert!(r.collapse && r.text.starts_with("line 1"));
        assert_eq!(find("supervisor note").tag.as_deref(), Some("supervisor"));
        assert!(find("Tools offered").text.starts_with("read · ~"));
    }

    #[test]
    fn no_goal_and_no_instruction_files_leave_those_parts_out() {
        let req = ModelRequest::new("m", "You are nexo.\n\nWorking directory: /w", vec![Message::user("hi")]);
        let s = sections(&req);
        let heads: Vec<&str> = s.iter().map(|x| x.heading.as_str()).collect();
        assert_eq!(heads, ["System prompt", "built-in text, directory, date, environment, tools", "#1 user"]);
    }
}
