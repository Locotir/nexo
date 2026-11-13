//! Who may run what: the mode, the rules, and what to suggest when asking.
//!
//! A rule is `tool` (any call of it) or `tool(glob)`, where the glob is
//! matched against the call's *target*: the command for `bash`
//! (`bash(git *)`, `bash(cargo test*)`), the path for file tools
//! (`edit(src/**)`, `read(/etc/*)`), and the tool name for the rest
//! (`mcp__server__*`). `deny` beats `allow` beats the mode.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobMatcher};
use nexo_core::config::PermissionsConfig;
use nexo_core::permission::PermissionMode;
use nexo_core::tool::ToolKind;
use serde_json::Value;

#[cfg(test)]
mod flag_tests;
#[cfg(test)]
mod note_scope_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask,
}

#[derive(Debug, Clone, Default)]
pub struct Policy {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// Tools that touch nothing outside the conversation: never asked about (a `deny` rule still holds).
pub const ALWAYS_ALLOWED: &[&str] = &["todo", "note", "recall", "ask_user", "exit_plan"];

/// Tools that read by running the project's own code (a language server or compiler runs its build
/// scripts and macros): asked about like a command, never run where only reads may.
pub const RUNS_PROJECT_CODE: &[&str] = &["diagnostics"];

impl Policy {
    pub fn from_config(cfg: &PermissionsConfig) -> Policy {
        Policy { mode: cfg.mode, allow: cfg.allow.clone(), deny: cfg.deny.clone() }
    }

    pub fn decide(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> Decision {
        // a call of several files is judged file by file, as if each were asked for alone: one denied
        // file denies the call, one that needs asking asks for the call
        let many = match kind {
            ToolKind::Read | ToolKind::Search => nexo_core::tool::has_paths(args),
            ToolKind::Write => args.get("files").is_some_and(|f| f.as_array().is_some_and(|a| !a.is_empty())),
            _ => false,
        };
        let mut ask = false;
        if many {
            for p in nexo_core::tool::call_paths(args) {
                let mut one = args.clone();
                if let Some(o) = one.as_object_mut() {
                    o.remove("paths");
                    o.remove("files");
                    o.remove("check");
                    o.insert("path".into(), Value::String(p));
                }
                match self.decide_one(tool, kind, &one, cwd) {
                    Decision::Deny(why) => return Decision::Deny(why),
                    Decision::Ask => ask = true,
                    Decision::Allow => {}
                }
            }
        } else {
            match self.decide_one(tool, kind, args, cwd) {
                Decision::Deny(why) => return Decision::Deny(why),
                Decision::Ask => ask = true,
                Decision::Allow => {}
            }
        }
        // the command an edit or a write asks to run after it is judged as a command of its own
        if kind == ToolKind::Write {
            if let Some(c) = args.get("check").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
                match self.decide_one("bash", ToolKind::Exec, &serde_json::json!({"command": c}), cwd) {
                    Decision::Deny(why) => return Decision::Deny(format!("check command: {why}")),
                    Decision::Ask => ask = true,
                    Decision::Allow => {}
                }
            }
        }
        if ask {
            Decision::Ask
        } else {
            Decision::Allow
        }
    }

    fn decide_one(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path) -> Decision {
        // what would wipe the machine beyond doubt runs in no mode and by no rule
        if kind == ToolKind::Exec {
            if let Some(why) = target_of(kind, args).as_deref().and_then(crate::supervise::policy::catastrophic) {
                return Decision::Deny(format!("never run: {why}"));
            }
        }
        if let Some(r) = self.deny.iter().find(|r| rule_matches_as(r, tool, kind, args, cwd, true)) {
            return Decision::Deny(format!("denied by rule `{r}`"));
        }
        // a note that saves a rule is a write to the instructions file: decided like one
        if tool == "note" {
            let scope = args.get("scope").and_then(Value::as_str).unwrap_or("session");
            if let Some(file) = nexo_core::compat::rules_path(scope, cwd) {
                let file = serde_json::json!({"path": file.to_string_lossy()});
                return self.decide("edit", ToolKind::Write, &file, cwd);
            }
        }
        if ALWAYS_ALLOWED.contains(&tool) {
            return Decision::Allow;
        }
        // reading a web page changes nothing here: read-only mode still asks for it
        let reads = matches!(kind, ToolKind::Read | ToolKind::Search | ToolKind::Network);
        // a workflow is asked about as it is in every mode, also where only reads run: like `task`, what its
        // sub-agents do is decided call by call
        if self.mode.reads_only() && !reads && tool != "workflow" {
            return Decision::Deny(format!("{} mode", self.mode.as_str()));
        }
        // it reads, but by starting the project's own build tools (build scripts, macros, plugins),
        // which can do anything a command can: not where only reads run, asked about as a command is
        if RUNS_PROJECT_CODE.contains(&tool) {
            if self.mode.reads_only() {
                return Decision::Deny(format!("{} mode: {tool} starts the project's build tools", self.mode.as_str()));
            }
            if self.mode != PermissionMode::Auto && !self.allow.iter().any(|r| rule_matches(r, tool, kind, args, cwd)) {
                return Decision::Ask;
            }
        }
        // a command that outlives nexo is never covered by an allow rule or a mode: each one asks
        // (yolo, which asks about nothing, keeps its meaning)
        if kind == ToolKind::Exec && self.mode != PermissionMode::Auto && is_flag(args, "detach") {
            return Decision::Ask;
        }
        // writing into a program under the debugger, or attaching to a process, asks every time
        if kind == ToolKind::Exec
            && self.mode != PermissionMode::Auto
            && nexo_tools::bash::changes_a_program(tool, args)
        {
            return Decision::Ask;
        }
        // keys and credentials are read only when asked, or by a rule that names their path
        if matches!(kind, ToolKind::Read | ToolKind::Search)
            && self.mode != PermissionMode::Auto
            && target_of(kind, args).is_some_and(|p| is_sensitive(&p, cwd))
        {
            let named = self.allow.iter().any(|r| names_a_path(r) && rule_matches(r, tool, kind, args, cwd));
            return if named { Decision::Allow } else { Decision::Ask };
        }
        if self.allow.iter().any(|r| rule_matches(r, tool, kind, args, cwd)) {
            return Decision::Allow;
        }
        match (self.mode, kind) {
            (PermissionMode::Auto, _) => Decision::Allow,
            (_, ToolKind::Read | ToolKind::Search) => Decision::Allow,
            (PermissionMode::AcceptEdits, ToolKind::Write) if editable(args, cwd) => Decision::Allow,
            _ => Decision::Ask,
        }
    }

    /// `decide`, and with the OS sandbox on a command that asks to run outside it (`unsandboxed`)
    /// is never allowed by a rule or a mode: it asks. A `deny` still denies; yolo still allows.
    /// With the sandbox off the flag changes nothing, so it adds no question.
    pub fn decide_boxed(&self, tool: &str, kind: ToolKind, args: &Value, cwd: &Path, sandbox_on: bool) -> Decision {
        match self.decide(tool, kind, args, cwd) {
            Decision::Allow if sandbox_on && self.leaves_the_sandbox(kind, args) => Decision::Ask,
            d => d,
        }
    }

    fn leaves_the_sandbox(&self, kind: ToolKind, args: &Value) -> bool {
        kind == ToolKind::Exec && self.mode != PermissionMode::Auto && is_flag(args, "unsandboxed")
    }

    pub fn add_allow(&mut self, rule: &str) {
        if !self.allow.iter().any(|r| r == rule) {
            self.allow.push(rule.to_string());
        }
    }
}

/// A flag of a call as the bash tool reads it (`true`, or the text `true`, `yes`, `1`).
fn is_flag(args: &Value, key: &str) -> bool {
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

/// `name` and the pattern of `name(pattern)`.
fn split_rule(rule: &str) -> (&str, Option<&str>) {
    let rule = rule.trim();
    match rule.find('(') {
        Some(i) if rule.ends_with(')') => (rule[..i].trim(), Some(&rule[i + 1..rule.len() - 1])),
        _ => (rule, None),
    }
}

fn glob(pattern: &str) -> Option<GlobMatcher> {
    GlobBuilder::new(pattern).literal_separator(false).build().ok().map(|g: Glob| g.compile_matcher())
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

/// The path as the rules see it: relative to `cwd` when under it, else absolute; always with
/// `/` between the parts, as rules are written, on Windows too.
fn path_forms(p: &str, cwd: &Path) -> Vec<String> {
    let abs = lexical(p, cwd);
    let slash = |s: String| s.replace('\\', "/");
    let mut out = vec![slash(abs.to_string_lossy().to_string())];
    if let Ok(rel) = abs.strip_prefix(normalise(cwd)) {
        out.push(slash(rel.to_string_lossy().to_string()));
    }
    // as written, unless it holds `..`: a glob crosses it, so `src/../.env` would pass as `src/**`
    if !Path::new(p.trim()).components().any(|c| c == std::path::Component::ParentDir) {
        out.push(slash(p.trim().to_string()));
    }
    out
}

/// The forms of the path with its links resolved, relative to the resolved `cwd` when under it.
fn real_forms(p: &str, cwd: &Path) -> Vec<String> {
    let real = real_path(&lexical(p, cwd));
    let root = real_path(&normalise(cwd));
    let mut out = vec![rule_slash(&real)];
    if let Ok(rel) = real.strip_prefix(&root) {
        out.push(rule_slash(rel));
    }
    out
}

/// A path as rules are written: `/` between the parts, and without the `\\?\C:\...` (or
/// `\\?\UNC\host\...`) prefix Windows adds when it resolves one.
fn rule_slash(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    match s.strip_prefix("//?/") {
        Some(rest) => rest.strip_prefix("UNC/").map(|r| format!("//{r}")).unwrap_or_else(|| rest.to_string()),
        None => s,
    }
}

/// An absolute pattern with the fixed folder it starts with resolved (links, and on Windows
/// 8.3 short names such as `RUNNER~1`), for matching a path's resolved forms; `None` when that
/// changes nothing. Only the folder the rule itself names is resolved: a link under it still
/// does not carry the rule to where it points.
fn resolved_pattern(pattern: &str) -> Option<String> {
    let fixed = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    let cut = pattern[..fixed].rfind('/')?;
    let (dir, rest) = pattern.split_at(cut);
    if dir.is_empty() || !Path::new(dir).is_absolute() {
        return None;
    }
    let real = format!("{}{rest}", rule_slash(&real_path(Path::new(dir))));
    (real != pattern).then_some(real)
}

/// The path as the file tools resolve it: trimmed, `~` expanded, joined to `cwd`, `..` folded.
fn lexical(p: &str, cwd: &Path) -> PathBuf {
    let p = p.trim();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    let abs = match (p, home) {
        ("~", Some(h)) => h,
        (_, Some(h)) if p.starts_with("~/") => h.join(&p[2..]),
        _ if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => cwd.join(p),
    };
    normalise(&abs)
}

/// The path with links resolved as far as it exists, the rest as written.
fn real_path(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut base = p.to_path_buf();
    loop {
        if let Ok(c) = base.canonicalize() {
            return rest.iter().rev().fold(c, |acc, x: &std::ffi::OsString| acc.join(x));
        }
        match (base.file_name().map(|n| n.to_os_string()), base.parent().map(Path::to_path_buf)) {
            (Some(n), Some(parent)) => {
                rest.push(n);
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// A file that holds keys or credentials: anything under `.ssh`, `.gnupg`, `.aws`, `.kube` or
/// `.docker`, `.env` files, private keys and certificates, `.netrc` and its kind, and nexo's
/// own credential store. Checked as written and with links resolved.
pub fn is_sensitive(p: &str, cwd: &Path) -> bool {
    let lex = lexical(p, cwd);
    let real = real_path(&lex);
    [&lex, &real].iter().any(|f| nexo_core::paths::holds_keys(f))
}

/// Folders whose files run something here later or change what an agent may do.
const GUARDED_DIRS: &[&str] = &[".git", ".nexo", ".claude", ".codex", ".vscode", ".idea", ".husky"];
/// Shell start-up files and other single files with the same effect.
const GUARDED_FILES: &[&str] = &[
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".mcp.json",
    ".envrc",
    ".gitconfig",
];

/// accept-edits writes without asking only inside the project (links resolved), and never into
/// the guarded folders and files above or files that hold keys.
fn editable(args: &Value, cwd: &Path) -> bool {
    let Some(p) = target_of(ToolKind::Write, args) else { return false };
    let (lex, root) = (lexical(&p, cwd), normalise(cwd));
    let (real, real_root) = (real_path(&lex), real_path(&root));
    let (Ok(rel), Ok(real_rel)) = (lex.strip_prefix(&root), real.strip_prefix(&real_root)) else { return false };
    let guarded = |r: &Path| {
        r.components().any(|c| c.as_os_str().to_str().is_some_and(|n| GUARDED_DIRS.contains(&n)))
            || r.file_name().and_then(|n| n.to_str()).is_some_and(|n| GUARDED_FILES.contains(&n))
    };
    !rel.as_os_str().is_empty() && !guarded(rel) && !guarded(real_rel) && !is_sensitive(&p, cwd)
}

/// A rule with a pattern narrower than "everything" (`read(.env)`, not `read` or `read(**)`).
fn names_a_path(rule: &str) -> bool {
    split_rule(rule).1.map(str::trim).is_some_and(|p| !matches!(p, "" | "*" | "**" | "**/*" | "/**" | "/**/*"))
}

fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// True when no link inside the project bends the path: it sits at the same place under the
/// resolved project as under the project as written (only the project's own folder may be a link).
fn no_link_inside(p: &str, cwd: &Path) -> bool {
    let (lex, root) = (lexical(p, cwd), normalise(cwd));
    let (real, real_root) = (real_path(&lex), real_path(&root));
    matches!((lex.strip_prefix(&root), real.strip_prefix(&real_root)), (Ok(a), Ok(b)) if a == b)
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
                deny_forms(cmd, 0, &mut forms);
                return one(cmd) || forms.iter().any(|p| one(p));
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
            let g = if pattern.contains('\\') { glob(&pattern.replace('\\', "/")).unwrap_or(g) } else { g };
            let pattern = pattern.replace('\\', "/");
            let (written, real) = (path_forms(&p, cwd), real_forms(&p, cwd));
            let hit = |forms: &[String]| forms.iter().any(|f| g.is_match(f));
            // the resolved path also meets the rule with its own folder resolved
            let g_real = resolved_pattern(&pattern).and_then(|r| glob(&r));
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

/// The simple commands of a shell line, split on `;`, `&&`, `||`, `|`, `&`, newlines and parentheses
/// outside quotes, and whether it is more than the parser can vouch for: a substitution (`$(..)`,
/// backticks, `<(..)`) that runs something else, an ANSI-C string (`$'..'`), a `${..}` with an `@`
/// operator (`${x@P}` runs what it expands), or a redirection that writes a file other than
/// `/dev/null`. With `subst` the substitutions are split out as commands too (for deny rules).
fn simple_commands(cmd: &str, subst: bool) -> (Vec<String>, bool) {
    let chars: Vec<char> = cmd.chars().collect();
    let (mut parts, mut cur, mut complex) = (Vec::new(), String::new(), false);
    let (mut single, mut double, mut ansi) = (false, false, false);
    let mut i = 0;
    let cut = |cur: &mut String, parts: &mut Vec<String>| {
        let t = cur.trim();
        if !t.is_empty() {
            parts.push(t.to_string());
        }
        cur.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if single {
            single = c != '\'';
            cur.push(c);
        } else if ansi {
            // inside `$'..'` a backslash escapes the next character, `\'` included
            cur.push(c);
            if c == '\\' {
                if let Some(n) = next {
                    cur.push(n);
                    i += 1;
                }
            } else {
                ansi = c != '\'';
            }
        } else if !double && c == '$' && next == Some('\'') {
            ansi = true;
            complex = true;
            cur.push_str("$'");
            i += 1;
        } else if c == '$' && next == Some('{') {
            complex |= braced_has_at(&chars, i + 2);
            cur.push(c);
        } else if c == '\\' {
            cur.push(c);
            if let Some(n) = next {
                cur.push(n);
                i += 1;
            }
        } else if c == '`'
            || (c == '$' && next == Some('('))
            || (!double && matches!(c, '<' | '>') && next == Some('('))
        {
            complex = true;
            if subst {
                cut(&mut cur, &mut parts);
                i += usize::from(c != '`');
            } else {
                cur.push(c);
            }
        } else if double {
            double = c != '"';
            cur.push(c);
        } else if c == '\'' {
            single = true;
            cur.push(c);
        } else if c == '"' {
            double = true;
            cur.push(c);
        } else if c == '>' || (c == '<' && next == Some('>')) {
            complex |= !harmless_redirect(&chars, i);
            cur.push(c);
            if c == '>' && next == Some('|') {
                // `>|` is one operator, not a pipe
                cur.push('|');
                i += 1;
            }
        } else if matches!(c, ';' | '|' | '\n' | '(' | ')') {
            cut(&mut cur, &mut parts);
        } else if c == '&' {
            // `2>&1` and `&>file` are redirections
            let prev = i.checked_sub(1).map(|p| chars[p]);
            if matches!(prev, Some('>') | Some('<')) || next == Some('>') {
                cur.push(c);
            } else {
                cut(&mut cur, &mut parts);
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    cut(&mut cur, &mut parts);
    (parts, complex)
}

/// Whether the `${` whose inside starts at `from` holds an `@` operator (`${x@P}`, `${a[@]}`);
/// unclosed counts as holding one.
fn braced_has_at(chars: &[char], from: usize) -> bool {
    let mut depth = 1;
    for &c in chars.iter().skip(from) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            '@' => return true,
            _ => {}
        }
    }
    true
}

/// The redirection at `at` (`>`, `>>`, `>|`, `>&`, `<>`) only duplicates a descriptor (`2>&1`)
/// or writes to `/dev/null`.
fn harmless_redirect(chars: &[char], at: usize) -> bool {
    if chars[at] == '<' {
        return false;
    }
    let mut j = at + 1;
    if matches!(chars.get(j), Some('>' | '|')) {
        j += 1;
    }
    let dup = chars.get(j) == Some(&'&');
    j += usize::from(dup);
    while chars.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
        j += 1;
    }
    let target: String =
        chars[j.min(chars.len())..].iter().take_while(|c| !c.is_whitespace() && !";|&<>()".contains(**c)).collect();
    target == "/dev/null"
        || (dup && (target == "-" || (!target.is_empty() && target.chars().all(|c| c.is_ascii_digit()))))
}

/// The words of a simple command as the program receives them: quotes and backslashes removed,
/// `$'..'` decoded, redirections (`2>/dev/null`, `> out`) left out.
fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut i = 0;
    let end_word = |out: &mut Vec<String>, cur: &mut String, quoted: &mut bool| {
        if *quoted || !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
        *quoted = false;
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`' | '\n')) {
                        i += 1;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '$' if next == Some('\'') => {
                quoted = true;
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i = ansi_escape(&chars, i + 1, &mut cur);
                    } else {
                        cur.push(chars[i]);
                        i += 1;
                    }
                }
            }
            '\\' => {
                if let Some(n) = next {
                    if n != '\n' {
                        cur.push(n);
                    }
                    i += 1;
                }
            }
            '<' | '>' => {
                // a redirection: its descriptor number, the operator and its target are no words
                if quoted || !cur.chars().all(|d| d.is_ascii_digit()) {
                    end_word(&mut out, &mut cur, &mut quoted);
                }
                cur.clear();
                while i < chars.len() && matches!(chars[i], '<' | '>' | '&' | '|') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && !chars[i].is_whitespace() {
                    if let Some(q @ ('\'' | '"')) = chars.get(i).copied() {
                        i += 1;
                        while i < chars.len() && chars[i] != q {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                continue;
            }
            '&' if next == Some('>') => {
                end_word(&mut out, &mut cur, &mut quoted);
                i += 1;
                continue;
            }
            c if c.is_whitespace() => end_word(&mut out, &mut cur, &mut quoted),
            c => cur.push(c),
        }
        i += 1;
    }
    end_word(&mut out, &mut cur, &mut quoted);
    out
}

/// Decodes the escape of `$'..'` whose letter is at `at` into `out`; the index after it.
fn ansi_escape(chars: &[char], at: usize, out: &mut String) -> usize {
    let radix_run = |from: usize, radix: u32, max: usize| {
        let digits: String = chars[from..].iter().take(max).take_while(|c| c.is_digit(radix)).collect();
        let n = digits.len();
        (u32::from_str_radix(&digits, radix).ok().and_then(char::from_u32), n)
    };
    let simple = match chars[at] {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'e' | 'E' => Some('\x1b'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        '\\' | '\'' | '"' | '?' => Some(chars[at]),
        _ => None,
    };
    if let Some(c) = simple {
        out.push(c);
        return at + 1;
    }
    let (decoded, used) = match chars[at] {
        'x' => radix_run(at + 1, 16, 2),
        'u' => radix_run(at + 1, 16, 4),
        'U' => radix_run(at + 1, 16, 8),
        '0'..='7' => {
            let (c, n) = radix_run(at, 8, 3);
            (c, n.saturating_sub(1))
        }
        'c' if at + 1 < chars.len() => (char::from_u32(chars[at + 1] as u32 & 0x1f), 1),
        _ => (None, 0),
    };
    match decoded {
        Some(c) if used > 0 || chars[at].is_digit(8) => {
            out.push(c);
            at + 1 + used
        }
        _ => {
            out.push('\\');
            out.push(chars[at]);
            at + 1
        }
    }
}

/// `rm` of `/bin/rm`.
fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

/// `NAME=value`, a variable set for the command that follows.
fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The simple commands of `cmd` for deny rules: each as written, and as the program it runs sees it
/// (wrappers like `sudo`, `env` and `nice` skipped, `/bin/rm` as `rm`), and the scripts handed to a
/// shell (`bash -c '..'`, `eval`, `env -S`) taken apart the same way.
fn deny_forms(cmd: &str, depth: usize, out: &mut Vec<String>) {
    let (parts, _) = simple_commands(cmd, true);
    for p in parts {
        let (ws, scripts) = unwrap_command(words(&p));
        out.push(p);
        if !ws.is_empty() {
            out.push(ws.join(" "));
        }
        if depth < 4 {
            for s in scripts {
                deny_forms(&s, depth + 1, out);
            }
        }
    }
}

/// The command under its wrappers, its program as a bare name, and the scripts it hands a shell.
fn unwrap_command(ws: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut scripts = Vec::new();
    let mut i = 0;
    loop {
        while ws.get(i).is_some_and(|w| is_assignment(w)) {
            i += 1;
        }
        let Some(w) = ws.get(i) else { break };
        let name = basename(w);
        // options of each wrapper that take a value
        let takes: &[&str] = match name {
            "command" | "builtin" | "nohup" | "time" => &[],
            "exec" => &["-a"],
            "nice" => &["-n", "--adjustment"],
            "sudo" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "-U", "--user", "--group", "--chdir"],
            "doas" => &["-u", "-C"],
            "env" => &["-u", "-C", "--unset", "--chdir"],
            _ => break,
        };
        let env = name == "env";
        i += 1;
        while let Some(o) = ws.get(i) {
            if !o.starts_with('-') {
                break;
            }
            i += 1;
            if o == "--" {
                break;
            }
            if env && (o == "-S" || o == "--split-string") {
                scripts.extend(ws.get(i).cloned());
                i += 1;
            } else if let Some(s) = o.strip_prefix("--split-string=").or(o.strip_prefix("-S")).filter(|_| env) {
                scripts.push(s.to_string());
            } else if takes.contains(&o.as_str()) {
                i += 1;
            }
        }
    }
    let mut ws = ws.into_iter().skip(i).collect::<Vec<_>>();
    if let Some(first) = ws.first_mut() {
        *first = basename(first).to_string();
    }
    match ws.first().map(String::as_str) {
        Some("sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "fish") => {
            let mut c = false;
            for w in &ws[1..] {
                if w.starts_with('-') && !w.starts_with("--") {
                    c |= w.contains('c');
                } else if w.starts_with('-') || w.starts_with('+') {
                    continue;
                } else {
                    if c {
                        scripts.push(w.clone());
                    }
                    break;
                }
            }
        }
        Some("eval") => scripts.push(ws[1..].join(" ")),
        _ => {}
    }
    (ws, scripts)
}

/// A simple command whose options make its program run other code or delete what it finds, so an
/// allow rule for the program does not cover it: `git -c alias.x='!sh'`, `find -exec`, `xargs`,
/// `env -S`.
fn runs_code(part: &str) -> bool {
    let ws = words(part);
    let mut it = ws.iter().map(String::as_str).skip_while(|w| is_assignment(w));
    let Some(prog) = it.next() else { return false };
    let rest: Vec<&str> = it.collect();
    match basename(prog) {
        "git" => {
            // options before the subcommand: `-c` and `--config-env` set config, which can name programs
            let mut global = rest.iter();
            while let Some(w) = global.next() {
                if !w.starts_with('-') {
                    break;
                }
                if w.starts_with("-c") || w.starts_with("--config-env") {
                    return true;
                }
                if matches!(*w, "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix") {
                    global.next();
                }
            }
            const RUNS: &[&str] = &["--exec-path", "--upload-pack", "--receive-pack", "--exec", "--extcmd"];
            rest.iter().any(|w| RUNS.contains(&w.split('=').next().unwrap_or(w)))
        }
        "find" => rest.iter().any(|w| {
            matches!(
                *w,
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
            )
        }),
        "xargs" => true,
        "env" => rest.iter().take_while(|w| w.starts_with('-') || is_assignment(w)).any(|w| {
            w.starts_with("--split-string") || (w.starts_with('-') && !w.starts_with("--") && w.contains('S'))
        }),
        _ => false,
    }
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
