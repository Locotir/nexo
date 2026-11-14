//! What the person sees when asked: flag words, rules to adopt, and saving one.

use std::path::Path;

use nexo_core::tool::ToolKind;
use serde_json::Value;

use super::paths::path_forms;
use super::rules::target_of;

/// A flag of a call as the bash tool reads it (`true`, or the text `true`, `yes`, `1`).
pub(super) fn is_flag(args: &Value, key: &str) -> bool {
    nexo_tools::util::bool_arg(args, key)
}

/// What a call does beyond what its command says, in words for the person: it runs outside the
/// sandbox (only said while the sandbox is on) or keeps running after nexo exits. Empty for a plain call.
pub fn flag_words(kind: ToolKind, args: &Value, sandbox_on: bool) -> Vec<&'static str> {
    let mut out = Vec::new();
    if kind == ToolKind::Exec {
        if sandbox_on && is_flag(args, "unsandboxed") {
            out.push("outside the sandbox");
        }
        if is_flag(args, "detach") {
            out.push("detached: keeps running after nexo exits");
        }
    }
    out
}

/// The summary of a request with the flag words after the tool name, where a narrow window still shows
/// them: `bash (outside the sandbox): cargo x`. Unchanged when there are none.
pub fn with_flags(summary: String, tool: &str, words: &[&str]) -> String {
    if words.is_empty() {
        return summary;
    }
    let rest = summary.strip_prefix(tool).map(|r| r.trim_start_matches(':').trim_start()).unwrap_or(&summary);
    format!("{tool} ({}): {rest}", words.join(", "))
}

/// Rules the person may adopt instead of being asked again for this call.
pub fn suggested_rules(tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if tool == "workflow" {
        // for one saved file as it is now (name and hash); none for text the model wrote, and none when the
        // name does not load
        return match args.get(crate::workflow::tool::PIN).and_then(Value::as_str) {
            Some(s) if args.get("inline").and_then(Value::as_str).is_none_or(|t| t.trim().is_empty()) => {
                vec![format!("workflow({s})")]
            }
            _ => out,
        };
    }
    match (kind, target_of(kind, args)) {
        (ToolKind::Exec, Some(cmd)) => {
            let words: Vec<&str> = cmd.split_whitespace().collect();
            if let Some(w) = words.first() {
                out.push(format!("{tool}({w} *)"));
            }
            if words.len() >= 2 {
                out.push(format!("{tool}({} {} *)", words[0], words[1]));
            }
        }
        (ToolKind::Read | ToolKind::Write | ToolKind::Search, Some(p)) => {
            let forms = path_forms(&p, cwd);
            let shown = forms.get(1).cloned().unwrap_or_else(|| forms[0].clone());
            out.push(format!("{tool}({shown})"));
            let dir = Path::new(&shown).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            out.push(if dir.is_empty() { format!("{tool}(**)") } else { format!("{tool}({dir}/**)") });
        }
        (ToolKind::Network, Some(host)) => {
            out.push(format!("{tool}({host})"));
        }
        _ => {}
    }
    out.push(tool.to_string());
    out.dedup();
    out
}

/// Adds `rule` to `[permissions] allow` of `<project_dir>/config.toml`, keeping
/// the rest of the file as it is.
pub fn persist_rule(project_dir: &Path, rule: &str) -> anyhow::Result<()> {
    crate::config_edit::push(&project_dir.join("config.toml"), &["permissions"], "allow", rule)
}
