//! Rule matching: the `tool(glob)` syntax, a call's target, and `rule_matches`.

use std::path::Path;

use globset::{Glob, GlobBuilder, GlobMatcher};
use nexo_core::tool::ToolKind;
use serde_json::Value;

use super::bash::{deny_forms, runs_code, simple_commands};
use super::paths::{no_link_inside, path_forms, real_forms, resolved_pattern};

/// `name` and the pattern of `name(pattern)`.
pub(super) fn split_rule(rule: &str) -> (&str, Option<&str>) {
    let rule = rule.trim();
    match rule.find('(') {
        Some(i) if rule.ends_with(')') => (rule[..i].trim(), Some(&rule[i + 1..rule.len() - 1])),
        _ => (rule, None),
    }
}

pub(super) fn glob(pattern: &str) -> Option<GlobMatcher> {
    build_glob(pattern, false)
}

/// A glob over file names: on macOS the volume folds case, so `.GIT` is `.git` and the rule must see it. Command
/// patterns never fold: `git branch -d *` must not cover `-D`.
pub(super) fn path_glob(pattern: &str) -> Option<GlobMatcher> {
    build_glob(pattern, cfg!(target_os = "macos"))
}

fn build_glob(pattern: &str, fold: bool) -> Option<GlobMatcher> {
    GlobBuilder::new(pattern)
        .literal_separator(false)
        .case_insensitive(fold)
        .build()
        .ok()
        .map(|g: Glob| g.compile_matcher())
}

/// What a call acts on: the command for exec tools, the path for file tools,
/// the host for network tools, nothing for the rest.
pub fn target_of(kind: ToolKind, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    match kind {
        ToolKind::Exec => s("command").or_else(|| s("cmd")).or_else(|| s("script")),
        ToolKind::Read | ToolKind::Write | ToolKind::Search => s("path")
            .or_else(|| s("file_path"))
            .or_else(|| s("file"))
            .or_else(|| s("directory"))
            .or_else(|| s("dir"))
            .or_else(|| nexo_core::tool::call_paths(args).into_iter().next()),
        ToolKind::Network => s("url").or_else(|| s("uri")).and_then(|u| host_of(&u)),
        _ => None,
    }
}

/// `docs.rs` out of `https://user@docs.rs:443/tokio?x`, lower case; `None` for what has no host.
pub fn host_of(url: &str) -> Option<String> {
    // parsed as web_fetch parses it, so a rule and the fetch agree on the host
    let host = url::Url::parse(url.trim()).ok()?.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// A rule with a pattern narrower than "everything" (`read(.env)`, not `read` or `read(**)`).
pub(super) fn names_a_path(rule: &str) -> bool {
    split_rule(rule).1.map(str::trim).is_some_and(|p| !matches!(p, "" | "*" | "**" | "**/*" | "/**" | "/**/*"))
}

pub fn rule_matches(rule: &str, tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> bool {
    rule_matches_as(rule, tool, kind, args, cwd, false)
}

/// `deny`: a command matches when any simple command in it does; otherwise (an allow rule) every one must.
pub fn rule_matches_as(rule: &str, tool: &str, kind: ToolKind, args: &Value, cwd: &Path, deny: bool) -> bool {
    let (name, pattern) = split_rule(rule);
    let name_ok = name == tool || (name.contains('*') && glob(name).map(|g| g.is_match(tool)).unwrap_or(false));
    if !name_ok {
        return false;
    }
    // a rule for the workflow tool covers saved workflows, never text the model wrote: that asks every time
    if tool == "workflow" && !deny && args.get("inline").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty()) {
        return false;
    }
    let Some(pattern) = pattern else { return true };
    let pattern = pattern.trim();
    // `workflow(find-bugs@<32 hex>)`: one saved workflow as its file is now. The turn put that subject in the call
    // before asking; a call without one (the file does not load) matches no pattern but `*`.
    if tool == "workflow" {
        let subject = args.get(crate::workflow::tool::PIN).and_then(Value::as_str);
        let hit = |c: &str| glob(pattern).is_some_and(|g| g.is_match(c));
        // a rule that denies also covers the name alone
        return pattern == "*"
            || pattern.is_empty()
            || subject.is_some_and(hit)
            || (deny && hit(args.get("name").and_then(Value::as_str).unwrap_or("")));
    }
    if pattern == "*" || pattern.is_empty() {
        return true;
    }
    let Some(g) = glob(pattern) else { return false };
    match (kind, target_of(kind, args)) {
        (ToolKind::Exec, Some(cmd)) => {
            let one = |c: &str| g.is_match(c) || g.is_match(c.split_whitespace().collect::<Vec<_>>().join(" "));
            let cmd = cmd.trim();
            if deny {
                // each simple command also as the program would see it: quotes gone, wrappers
                // (`sudo`, `env`, ...) skipped, `/bin/rm` as `rm`, and `bash -c '...'` read inside
                let mut forms = Vec::new();
                let unknown = deny_forms(cmd, 0, &mut forms);
                return unknown || one(cmd) || forms.iter().any(|p| one(p));
            }
            let (parts, complex) = simple_commands(cmd, false);
            if pattern.contains([';', '&', '|', '\n', '`', '$']) {
                // the rule spells the compound out itself
                one(cmd)
            } else {
                !complex && !parts.is_empty() && parts.iter().all(|p| one(p) && !runs_code(p))
            }
        }
        (ToolKind::Read | ToolKind::Write | ToolKind::Search, Some(p)) => {
            // a rule written with `\` on Windows reads as one with `/`
            let pattern = pattern.replace('\\', "/");
            let g = path_glob(&pattern).unwrap_or(g);
            let (written, real) = (path_forms(&p, cwd), real_forms(&p, cwd));
            let hit = |forms: &[String]| forms.iter().any(|f| g.is_match(f));
            // the resolved path also meets the rule with its own folder resolved
            let g_real = resolved_pattern(&pattern).and_then(|r| path_glob(&r));
            let hit_real = hit(&real) || g_real.is_some_and(|gr| real.iter().any(|f| gr.is_match(f)));
            // a link does not carry an allow rule to where it points; a deny rule sees both
            if deny {
                hit(&written) || hit_real
            } else {
                hit(&written) && (hit_real || no_link_inside(&p, cwd))
            }
        }
        // `web_fetch(*.github.com)` also covers `github.com` itself
        (ToolKind::Network, Some(host)) => {
            g.is_match(&host) || pattern.strip_prefix("*.").is_some_and(|bare| bare.eq_ignore_ascii_case(&host))
        }
        (ToolKind::Network, None) => false,
        (_, _) => g.is_match(tool),
    }
}
