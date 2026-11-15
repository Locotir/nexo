//! The conversation as units: what each message or tool result says, for the ledger and the selections.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Person,
    Agent,
    Call,
    Result,
}

pub(super) struct Unit {
    pub(super) msg: usize,
    pub(super) kind: Kind,
    pub(super) tool: String,
    pub(super) path: Option<String>,
    /// The files of a `read` that names several in `paths`; `path` is then empty.
    pub(super) files: Vec<String>,
    pub(super) command: Option<String>,
    pub(super) text: String,
    pub(super) error: bool,
}

pub(super) fn str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty())
}

pub(super) fn result_text(content: &[Part]) -> String {
    let mut out = String::new();
    for p in content {
        if let Part::Text { text } = p {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// The units of `history`. A call whose result failed is marked `error` too.
pub(super) fn units(history: &[Message]) -> Vec<Unit> {
    let failed: BTreeSet<&str> = history
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|p| match p {
            Part::ToolResult { call_id, is_error: true, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let mut calls: BTreeMap<&str, (&str, &Value)> = BTreeMap::new();
    let mut out = vec![];
    for (mi, m) in history.iter().enumerate() {
        for p in &m.content {
            match p {
                Part::Text { text } if m.role == Role::User && is_request(text) => out.push(Unit {
                    msg: mi,
                    kind: Kind::Person,
                    tool: String::new(),
                    files: vec![],
                    path: None,
                    command: None,
                    text: text.clone(),
                    error: false,
                }),
                Part::Text { text } if m.role == Role::Assistant => out.push(Unit {
                    msg: mi,
                    kind: Kind::Agent,
                    tool: String::new(),
                    files: vec![],
                    path: None,
                    command: None,
                    text: text.clone(),
                    error: false,
                }),
                Part::ToolCall { id, name, args } => {
                    calls.insert(id, (name, args));
                    let text = match name.as_str() {
                        "edit" => str_arg(args, &["new_string"]).unwrap_or("").to_string(),
                        "write" => str_arg(args, &["content"]).unwrap_or("").to_string(),
                        _ => String::new(),
                    };
                    out.push(Unit {
                        msg: mi,
                        kind: Kind::Call,
                        tool: name.clone(),
                        files: if nexo_core::tool::has_paths(args) {
                            nexo_core::tool::call_paths(args)
                        } else {
                            vec![]
                        },
                        path: str_arg(args, &["path", "file_path"])
                            .filter(|_| !nexo_core::tool::has_paths(args))
                            .map(str::to_string),
                        command: str_arg(args, &["command", "cmd"]).map(str::to_string),
                        text,
                        error: failed.contains(id.as_str()),
                    });
                }
                Part::ToolResult { call_id, name, content, is_error } => {
                    // a read stub points at lines the sub-agent never saw
                    if crate::reads::is_unchanged_note(&result_text(content)) {
                        continue;
                    }
                    let (tool, args) = match calls.get(call_id.as_str()) {
                        Some((n, a)) => (n.to_string(), Some(*a)),
                        None => (name.clone().unwrap_or_default(), None),
                    };
                    out.push(Unit {
                        msg: mi,
                        kind: Kind::Result,
                        files: args
                            .filter(|a| nexo_core::tool::has_paths(a))
                            .map(nexo_core::tool::call_paths)
                            .unwrap_or_default(),
                        path: args
                            .filter(|a| !nexo_core::tool::has_paths(a))
                            .and_then(|a| str_arg(a, &["path", "file_path"]))
                            .map(str::to_string),
                        command: args.and_then(|a| str_arg(a, &["command", "cmd"])).map(str::to_string),
                        tool,
                        text: result_text(content),
                        error: *is_error,
                    });
                }
                _ => {}
            }
        }
    }
    out
}

pub(super) fn writes_file(tool: &str) -> bool {
    matches!(tool, "edit" | "write" | "multi_edit" | "apply_patch")
}

/// Number prefix of a file viewer's line: `    12│ ` or `12\t`.
pub(super) fn numbered(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let end = t.find(|c: char| !c.is_ascii_digit())?;
    if end == 0 {
        return None;
    }
    let n = t[..end].parse().ok()?;
    let rest = &t[end..];
    let rest = rest.strip_prefix('│').or_else(|| rest.strip_prefix('\t'))?;
    Some((n, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// Line ranges [start, end) cut at blank lines (line-number prefixes ignored).
pub(super) fn paragraphs(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut start = 0;
    for (i, line) in lines.iter().enumerate() {
        let body = numbered(line).map(|(_, r)| r).unwrap_or(line);
        let n = i + 1 - start;
        if (body.trim().is_empty() && n >= 4) || n >= MAX_CHUNK_LINES {
            out.push((start, i + 1));
            start = i + 1;
        }
    }
    if start < lines.len() {
        match out.last_mut() {
            Some(last) if lines.len() - start < 3 => last.1 = lines.len(),
            _ => out.push((start, lines.len())),
        }
    }
    out
}

pub(super) fn label(u: &Unit) -> String {
    match u.kind {
        Kind::Person => "the person".into(),
        Kind::Agent => "the main agent".into(),
        _ => {
            let what = u.path.clone().or_else(|| u.command.as_ref().map(|c| c.chars().take(80).collect()));
            match what {
                Some(w) => format!("{} {w}", u.tool),
                None => u.tool.clone(),
            }
        }
    }
}
