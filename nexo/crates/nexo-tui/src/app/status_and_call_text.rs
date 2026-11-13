//! Status lifetimes, tool-call summaries, pricing and branch parsing the state relies on.

use super::*;

/// What `/init` asks the agent.
pub const INIT_PROMPT: &str = "Study this project and write (or update) AGENTS.md at its root: a short guide for an \
agent working here. Cover what the project is, how it is laid out, how to build, test, lint and run it (the exact \
commands, which you should check by running them), the conventions the code follows, and anything surprising. \
Keep it under 150 lines; prefer facts you verified to guesses.";

/// How long a flash message stays; it fades out over its last `STATUS_FADE_MS`.
pub const STATUS_MS: u64 = 2500;
pub const STATUS_FADE_MS: u64 = 900;
/// An error stays longer: it is not a confirmation to miss.
pub const STATUS_ERR_MS: u64 = 8000;

/// How long a flash of this level stays.
pub fn status_life(level: Level) -> u64 {
    if level == Level::Error {
        STATUS_ERR_MS
    } else {
        STATUS_MS
    }
}

/// `+n −m` (or `+n -m`) in a tool's text, as an edit tool reports it.
pub fn parse_counts(text: &str) -> Option<(usize, usize)> {
    let i = text.find('+')?;
    let rest = &text[i + 1..];
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = rest[n.len()..].trim_start();
    let after = after.strip_prefix('−').or_else(|| after.strip_prefix('-'))?;
    let m: String = after.chars().take_while(char::is_ascii_digit).collect();
    if n.is_empty() || m.is_empty() {
        return None;
    }
    Some((n.parse().ok()?, m.parse().ok()?))
}

pub(super) fn status_of(o: &ToolOutput) -> ToolStatus {
    if !o.is_error {
        ToolStatus::Done
    } else if o.content.to_lowercase().starts_with("permission denied")
        || o.content.to_lowercase().starts_with("denied")
    {
        ToolStatus::Denied
    } else {
        ToolStatus::Failed
    }
}

pub(super) fn mode_hint(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Ask => "ask before writes and commands",
        PermissionMode::AcceptEdits => "writes go through; ask before commands",
        PermissionMode::Auto => "never ask",
        PermissionMode::ReadOnly => "only reads and searches",
        PermissionMode::Plan => "reads, then a plan to approve before any write",
    }
}

/// `HH:MM` now, local time when the offset is known.
pub(crate) fn clock() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!("{:02}:{:02}", now.hour(), now.minute())
}

/// A line that starts with a path (`/home/u/a.rs has a bug`), not a command: its first word has a second
/// `/`, a `\`, or a `.`.
pub(super) fn starts_with_path(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|w| w[1..].contains(['/', '\\', '.']))
}

/// The branch of the repository that holds `cwd` (a short hash when detached).
pub fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let head = d.join(".git").join("HEAD");
        if let Ok(text) = std::fs::read_to_string(&head) {
            let text = text.trim();
            return Some(match text.strip_prefix("ref: ") {
                Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_string(),
                None => text.chars().take(7).collect(),
            });
        }
        if d.join(".git").is_file() {
            // a worktree: `.git` names the real directory
            if let Ok(text) = std::fs::read_to_string(d.join(".git")) {
                if let Some(p) = text.trim().strip_prefix("gitdir: ") {
                    if let Ok(h) = std::fs::read_to_string(std::path::Path::new(p).join("HEAD")) {
                        let h = h.trim();
                        return Some(match h.strip_prefix("ref: ") {
                            Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_string(),
                            None => h.chars().take(7).collect(),
                        });
                    }
                }
            }
        }
        dir = d.parent();
    }
    None
}

/// `in=0.28 out=0.42 cache=0.028 write=0`: prices per million tokens.
pub fn parse_pricing(args: &str) -> Result<Pricing, String> {
    let mut p = Pricing::default();
    let mut any = false;
    for tok in args.split([' ', ',']).filter(|t| !t.is_empty()) {
        let (k, v) = tok.split_once('=').ok_or_else(|| format!("'{tok}' is not key=value"))?;
        let v: f64 = v.trim_start_matches('$').parse().map_err(|_| format!("'{v}' is not a number"))?;
        if v < 0.0 {
            return Err(format!("{k} cannot be negative"));
        }
        match k.to_ascii_lowercase().as_str() {
            "in" | "input" | "prompt" => p.input = v,
            "out" | "output" | "completion" => p.output = v,
            "cache" | "cached" | "cache_read" | "read" => p.cache_read = v,
            "write" | "cache_write" => p.cache_write = v,
            other => return Err(format!("unknown key '{other}' (in, out, cache, write)")),
        }
        any = true;
    }
    if !any {
        return Err("give at least one of in=, out=, cache=, write=".into());
    }
    Ok(p)
}

/// `key value` pairs of an object of plain values (`action output, id 3`); None for anything else.
pub(super) fn pairs(args: &serde_json::Value) -> Option<String> {
    let o = args.as_object().filter(|o| !o.is_empty())?;
    let mut out = Vec::new();
    for (k, v) in o {
        let v = match v {
            serde_json::Value::String(s) => s.trim().lines().next().unwrap_or("").to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => continue,
            _ => v.to_string(),
        };
        out.push(format!("{k} {v}"));
    }
    Some(out.join(", "))
}

/// One line for a call replayed from the history (the agent's summary is not recorded).
pub fn summarise(name: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::trim);
    match name {
        "web_search" => {
            if let Some(q) = s("query") {
                return format!("web_search: {}", q.lines().next().unwrap_or(""));
            }
        }
        "read" if nexo_core::tool::has_paths(args) => {
            return nexo_core::tool::paths_label(&nexo_core::tool::call_paths(args));
        }
        "web_fetch" => {
            if let Some(u) = s("url") {
                // host and path: the scheme and the query say little on a card
                let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
                let shown = rest.split(['?', '#']).next().unwrap_or(rest).trim_end_matches('/');
                return format!("web_fetch: {shown}");
            }
        }
        _ => {}
    }
    for k in ["path", "file_path", "command", "pattern", "query", "url"] {
        if let Some(v) = args.get(k).and_then(|v| v.as_str()) {
            return v.lines().next().unwrap_or("").to_string();
        }
    }
    let _ = name;
    let s = pairs(args).unwrap_or_else(|| args.to_string());
    if s.len() > 80 {
        // cut on a character boundary: the JSON keeps non-ASCII text as is
        let cut = (0..=79).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        format!("{}…", &s[..cut])
    } else {
        s
    }
}

/// The body of the last fenced code block in markdown text.
pub fn last_code_block(text: &str) -> Option<String> {
    let mut found = None;
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let t = line.trim_start();
        let fence: String = t.chars().take_while(|c| *c == '`' || *c == '~').collect();
        match open.as_mut() {
            Some((f, body)) => {
                if fence.len() >= f.len() && fence.starts_with(&f[..1]) && t.trim_end() == fence {
                    found = Some(body.join("\n"));
                    open = None;
                } else {
                    body.push(line);
                }
            }
            None if fence.len() >= 3 && fence.chars().all(|c| c == fence.chars().next().unwrap()) => {
                open = Some((fence, Vec::new()));
            }
            None => {}
        }
    }
    found
}
