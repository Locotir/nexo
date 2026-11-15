//! Hooks and tool names of the other tools' settings, as nexo reads them.

use std::path::Path;

use super::*;

/// A tool name of another tool's agent file as nexo calls it; none for one nexo has no use for.
pub fn map_tool(t: &str) -> Option<String> {
    let m = match t {
        "Read" => "read",
        "Write" => "write",
        "Edit" | "MultiEdit" | "NotebookEdit" => "edit",
        "Bash" | "BashOutput" => "bash",
        "Glob" => "glob",
        "Grep" => "grep",
        "LS" => "ls",
        "WebFetch" => "web_fetch",
        "WebSearch" => "web_search",
        "TodoWrite" => "todo",
        t if t.starts_with("mcp__") => t,
        t if !t.is_empty() && t.chars().all(|c| c.is_ascii_lowercase() || c == '_') => t,
        _ => return None,
    };
    Some(m.to_string())
}

/// A hook matcher of another tool's settings as nexo reads it: each plain tool name between `|`
/// is mapped like an agent's tool list (`Edit|Write` becomes `edit|write`, `Task` becomes `task`),
/// anything with regex syntax stays as it is. None when it matches every tool.
pub fn map_matcher(m: &str) -> Option<String> {
    let m = m.trim();
    if m.is_empty() || m == "*" {
        return None;
    }
    let plain = m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|');
    if !plain {
        return Some(m.to_string());
    }
    let alts: Vec<String> = m
        .split('|')
        .map(|a| if a == "Task" { "task".to_string() } else { map_tool(a).unwrap_or_else(|| a.to_string()) })
        .collect();
    Some(alts.join("|"))
}

/// The hooks of a `{"PreToolUse": [{"matcher": ..., "hooks": [{"command": ...}]}]}` table as nexo's
/// hooks: `command` hooks only, events nexo has no point for left out. With `root`, the plugin root
/// variables in the commands are replaced by that folder.
pub fn hooks_from_events(
    events: &serde_json::Map<String, serde_json::Value>,
    source: &str,
    root: Option<&Path>,
) -> Vec<crate::config::HookConfig> {
    let mut out = Vec::new();
    for (event, groups) in events {
        let Some(name) = crate::config::canonical_event(event) else { continue };
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group.get("matcher").and_then(|m| m.as_str()).and_then(map_matcher);
            for h in group.get("hooks").and_then(|h| h.as_array()).into_iter().flatten() {
                let kind = h.get("type").and_then(|t| t.as_str()).unwrap_or("command");
                let Some(command) = h.get("command").and_then(|c| c.as_str()).filter(|_| kind == "command") else {
                    continue;
                };
                let command = match root {
                    Some(r) => crate::plugins::expand_root(command, r),
                    None => command.to_string(),
                };
                out.push(crate::config::HookConfig {
                    event: name.to_string(),
                    matcher: matcher.clone(),
                    command,
                    timeout: h.get("timeout").and_then(|t| t.as_u64()),
                    source: source.to_string(),
                });
            }
        }
    }
    out
}

/// The hooks of the project's `.claude/settings.json` and `.claude/settings.local.json`
/// (`{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "...",
/// "timeout": 10}]}]}}`), only when the project is trusted, like nexo's own project file. Events
/// nexo has no point for are left out. The second value says what was left out for lack of trust.
/// A project at the home folder has none: its `.claude/settings.json` is Claude Code's own user file, and
/// its hooks are written for Claude Code sessions, not for this one.
pub fn claude_hooks(cwd: &Path, trust_file: &Path) -> (Vec<crate::config::HookConfig>, Option<String>) {
    if !claude_code() {
        return (Vec::new(), None);
    }
    claude_hooks_in(cwd, trust_file, dirs::home_dir().as_deref())
}

pub(super) fn claude_hooks_in(
    cwd: &Path,
    trust_file: &Path,
    home: Option<&Path>,
) -> (Vec<crate::config::HookConfig>, Option<String>) {
    // the settings in the home folder are the person's own for Claude Code, never a project's hooks
    let root = project_root_with(cwd, home);
    if crate::paths::is_home(&root, home) {
        return (Vec::new(), None);
    }
    let trusted = crate::config::is_trusted(&root, trust_file);
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for file in ["settings.json", "settings.local.json"] {
        let path = root.join(".claude").join(file);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(events) = v.get("hooks").and_then(|h| h.as_object()) else { continue };
        let shown = format!("project .claude/{file}");
        let found = hooks_from_events(events, &shown, None);
        if trusted {
            out.extend(found);
        } else if !found.is_empty() {
            skipped.push(shown);
        }
    }
    skipped.dedup();
    let note = (!skipped.is_empty()).then(|| {
        format!(
            "hooks in {} are ignored until you trust this project: nexo config trust \"{}\"",
            skipped.join(" and "),
            root.display()
        )
    });
    (out, note)
}
