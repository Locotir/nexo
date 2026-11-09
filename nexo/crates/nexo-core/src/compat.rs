//! Day-one compatibility with the files a team already keeps for other coding agents:
//! `CLAUDE.md`, `CLAUDE.local.md` and nested `AGENTS.md` instructions with `@path` imports,
//! commands in `.claude/commands` and `.codex/prompts`, and sub-agents in `.claude/agents`.
//!
//! Everything here takes its folders as arguments, so tests use temporary ones. nexo's own files
//! (`NEXO.md`, `.nexo/commands`, `.nexo/agents`) win over the foreign ones on a clash, and a
//! project's foreign commands and agents are left out until the project is trusted, like nexo's.

use std::path::{Path, PathBuf};

/// How many imports may be nested in one another.
const IMPORT_DEPTH: usize = 5;
/// The most nested instruction files listed.
const NESTED_MAX: usize = 20;

/// The nearest ancestor of `cwd` with a `.git` entry, else `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd).to_path_buf()
}

/// Where `~/.claude` and `~/.codex` are looked for: `NEXO_COMPAT_HOME` when set (empty turns the
/// lookup off), else the person's home. A custom `NEXO_CONFIG_DIR` marks an isolated setup (tests,
/// containers): then the other tools' user folders are not read unless `NEXO_COMPAT_HOME` says so.
pub fn foreign_home() -> Option<PathBuf> {
    match std::env::var_os("NEXO_COMPAT_HOME") {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None if std::env::var_os("NEXO_CONFIG_DIR").is_some_and(|v| !v.is_empty()) => None,
        None => dirs::home_dir(),
    }
}

/// One instructions file, its `@path` imports already placed after the line that names them.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub path: PathBuf,
    pub text: String,
    /// Where it comes from, in a few words (`project`, `user`, `user fallback`).
    pub origin: &'static str,
}

fn read(p: &Path) -> Option<String> {
    let t = std::fs::read_to_string(p).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// The instruction files that apply in `cwd`: the user-level ones first, then the project's from the
/// nearest folder up to the repository root, as nexo always did. In one folder: `NEXO.md`, then `AGENTS.md`, then `CLAUDE.md` (also
/// `.claude/CLAUDE.md`) unless `AGENTS.md` is there, then `CLAUDE.local.md`.
///
/// User level: `NEXO.md` or `AGENTS.md` in `config_dir`; only when neither exists, the other
/// tools' `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` (under `home`).
pub fn instructions(cwd: &Path, config_dir: &Path, home: Option<&Path>) -> Vec<Instruction> {
    let mut out = Vec::new();
    let own: Vec<PathBuf> =
        ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).filter(|p| p.is_file()).collect();
    if own.is_empty() {
        if let Some(h) = home {
            for rel in [".codex/AGENTS.md", ".claude/CLAUDE.md"] {
                push(&mut out, h.join(rel), "user fallback");
            }
        }
    } else {
        for p in own {
            push(&mut out, p, "user");
        }
    }
    let mut p = Some(cwd);
    while let Some(d) = p {
        push(&mut out, d.join("NEXO.md"), "project");
        let agents = d.join("AGENTS.md");
        let had_agents = agents.is_file();
        push(&mut out, agents, "project");
        if !had_agents {
            if d.join("CLAUDE.md").is_file() {
                push(&mut out, d.join("CLAUDE.md"), "project");
            } else {
                push(&mut out, d.join(".claude").join("CLAUDE.md"), "project");
            }
        }
        push(&mut out, d.join("CLAUDE.local.md"), "project (local)");
        if d.join(".git").exists() {
            break;
        }
        p = d.parent();
    }
    // a home kept in git: `~/.claude/CLAUDE.md` read as the user's must not come again as the project's
    let mut seen: Vec<PathBuf> = Vec::new();
    out.retain(|i| {
        let real = i.path.canonicalize().unwrap_or_else(|_| i.path.clone());
        !seen.contains(&real) && {
            seen.push(real);
            true
        }
    });
    out
}

/// The project file `/memory` opens in `cwd`: the nearest instructions file nexo reads, from `cwd` up
/// to the repository root, or `AGENTS.md` in `cwd` when there is none yet.
pub fn project_file(cwd: &Path) -> PathBuf {
    let mut p = Some(cwd);
    while let Some(d) = p {
        for name in ["NEXO.md", "AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md"] {
            if d.join(name).is_file() {
                return d.join(name);
            }
        }
        if d.join(".git").exists() {
            break;
        }
        p = d.parent();
    }
    cwd.join("AGENTS.md")
}

/// The user file `/memory user` opens: nexo's own in `config_dir`, and when it does not exist yet,
/// the text to start it with: what the other tools' files under `home` say, which nexo reads in its
/// place until it exists and never after.
pub fn user_file(config_dir: &Path, home: Option<&Path>) -> (PathBuf, Option<String>) {
    if let Some(p) = ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).find(|p| p.is_file()) {
        return (p, None);
    }
    let mut fallback = Vec::new();
    if let Some(h) = home {
        for rel in [".codex/AGENTS.md", ".claude/CLAUDE.md"] {
            push(&mut fallback, h.join(rel), "user fallback");
        }
    }
    let seed = fallback.iter().map(|i| i.text.trim_end()).collect::<Vec<_>>().join("\n\n");
    (config_dir.join("AGENTS.md"), (!seed.is_empty()).then(|| format!("{seed}\n")))
}

fn push(out: &mut Vec<Instruction>, path: PathBuf, origin: &'static str) {
    if !path.is_file() {
        return;
    }
    let mut stack = vec![path.canonicalize().unwrap_or_else(|_| path.clone())];
    if let Some(text) = read(&path).map(|t| expand_imports(&t, path.parent().unwrap_or(Path::new(".")), 0, &mut stack))
    {
        out.push(Instruction { path, text, origin });
    }
}

/// `text` with the files named by `@relative/path` words placed after the line that names them.
/// Fenced code and words that are not an existing file stay as they are. Depth is limited and a
/// file already being expanded is not entered again. Files that hold keys are never imported.
fn expand_imports(text: &str, dir: &Path, depth: usize, stack: &mut Vec<PathBuf>) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        out.push_str(line);
        out.push('\n');
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced || depth >= IMPORT_DEPTH {
            continue;
        }
        let mut prev_space = true;
        let mut word = String::new();
        let mut words = Vec::new();
        for c in line.chars().chain(std::iter::once(' ')) {
            if c.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                prev_space = true;
            } else if prev_space && c == '@' && word.is_empty() {
                word.push('@');
                prev_space = false;
            } else if word.starts_with('@') {
                word.push(c);
            } else {
                prev_space = false;
            }
        }
        for w in words {
            let rel = w[1..].trim_end_matches(['.', ',', ';', ':', ')', '!', '?']);
            if rel.is_empty() || rel.starts_with('/') || rel.starts_with('~') || Path::new(rel).is_absolute() {
                continue;
            }
            let file = dir.join(rel);
            let Ok(real) = file.canonicalize() else { continue };
            if !real.is_file()
                || stack.contains(&real)
                || crate::paths::holds_keys(&file)
                || crate::paths::holds_keys(&real)
            {
                continue;
            }
            let Some(body) = read(&real) else { continue };
            stack.push(real.clone());
            let inner = expand_imports(&body, real.parent().unwrap_or(dir), depth + 1, stack);
            stack.pop();
            out.push_str(&format!("\n[imported from {rel}]\n{}\n[end of {rel}]\n\n", inner.trim()));
        }
    }
    out.trim_end().to_string()
}

/// Instruction files below the project root that are not loaded (those in a folder the agent
/// is not working in), as paths relative to the root; the agent reads one when it works there.
pub fn nested_instructions(cwd: &Path) -> Vec<PathBuf> {
    let root = project_root(cwd);
    let loaded: Vec<&Path> = cwd.ancestors().take_while(|d| d.starts_with(&root)).collect();
    let mut found = Vec::new();
    let walk = ignore::WalkBuilder::new(&root).max_depth(Some(5)).build();
    for (seen, e) in walk.flatten().enumerate() {
        if seen > 20_000 || found.len() >= NESTED_MAX {
            break;
        }
        let name = e.file_name().to_string_lossy();
        if !matches!(name.as_ref(), "AGENTS.md" | "CLAUDE.md" | "CLAUDE.local.md") || !e.path().is_file() {
            continue;
        }
        let Some(parent) = e.path().parent() else { continue };
        if loaded.contains(&parent) {
            continue;
        }
        found.push(e.path().strip_prefix(&root).unwrap_or(e.path()).to_path_buf());
    }
    found.sort();
    found
}

/// A front matter (`key: value` lines between `---` lines) and the text after it.
pub fn front_matter(text: &str) -> (Vec<(String, String)>, String) {
    let text = text.trim_start_matches('\u{feff}');
    let norm;
    let text = if text.contains('\r') {
        norm = text.replace("\r\n", "\n");
        norm.as_str()
    } else {
        text
    };
    if let Some(rest) = text.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let keys = rest[..end]
                .lines()
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| {
                    (k.trim().to_ascii_lowercase(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                })
                .collect();
            let body = rest[end + 4..].trim_start_matches(|c| c != '\n').trim_start_matches('\n').to_string();
            return (keys, body);
        }
    }
    (Vec::new(), text.to_string())
}

/// One folder a kind of file is read from, lowest priority first in [`sources`].
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub dir: PathBuf,
    /// Short and plain: `project .claude/commands`, `user .nexo/agents`.
    pub origin: String,
    pub foreign: bool,
    /// The plugin this folder belongs to: its items are named `plugin:item`.
    pub plugin: Option<String>,
}

/// The folders to read `nexo_sub` (`commands`) from, and the other tools' equivalents
/// (`claude_sub`, `codex_sub`), lowest priority first: the other tools' user folders, their
/// project folders, then nexo's own user folder and nexo's project folder. A project's folders
/// are left out until the project is trusted.
pub fn sources(
    cwd: &Path,
    config_dir: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    nexo_sub: &str,
    claude_sub: Option<&str>,
    codex_sub: Option<&str>,
) -> Vec<Source> {
    let mut out = Vec::new();
    // installed plugins come first: everything of the person's own overrides them
    for p in crate::plugins::enabled(config_dir).0 {
        let dirs = match nexo_sub {
            "commands" => p.command_dirs(),
            "agents" => p.agent_dirs(),
            _ => Vec::new(),
        };
        for dir in dirs {
            let origin = format!("plugin {}", p.name);
            out.push(Source { dir, origin, foreign: true, plugin: Some(p.name.clone()) });
        }
    }
    let mut add = |dir: PathBuf, scope: &str, shown: String, foreign: bool| {
        out.push(Source { dir, origin: format!("{scope} {shown}"), foreign, plugin: None });
    };
    let root = project_root(cwd);
    let trusted = crate::config::is_trusted(&root, trust_file);
    if let Some(h) = home {
        if let Some(s) = codex_sub {
            add(h.join(".codex").join(s), "user", format!(".codex/{s}"), true);
        }
        if let Some(s) = claude_sub {
            add(h.join(".claude").join(s), "user", format!(".claude/{s}"), true);
        }
    }
    // a home kept in git: its `.claude` and `.codex` are the user folders above, not a project's
    if trusted && !crate::paths::is_home(&root, home) {
        if let Some(s) = codex_sub {
            add(root.join(".codex").join(s), "project", format!(".codex/{s}"), true);
        }
        if let Some(s) = claude_sub {
            add(root.join(".claude").join(s), "project", format!(".claude/{s}"), true);
        }
    }
    add(config_dir.join(nexo_sub), "user", format!(".nexo/{nexo_sub}"), false);
    if let Some(p) = crate::paths::project_dir(cwd) {
        if p.parent().is_some_and(|r| crate::config::is_trusted(r, trust_file)) {
            add(p.join(nexo_sub), "project", format!(".nexo/{nexo_sub}"), false);
        }
    }
    out
}

/// A command file: `name` is the path below the folder without `.md`, folders joined with `:`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandFile {
    pub name: String,
    pub description: String,
    pub hint: String,
    pub allowed_tools: String,
    pub body: String,
    pub path: PathBuf,
    pub origin: String,
}

/// All command files, a later source replacing an earlier one with the same name. Names for
/// which `reserved` says true (the built-in commands) are left out.
pub fn commands(
    cwd: &Path,
    config_dir: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    reserved: &dyn Fn(&str) -> bool,
) -> Vec<CommandFile> {
    let mut out: Vec<CommandFile> = Vec::new();
    let policy = crate::managed::current();
    for s in sources(cwd, config_dir, trust_file, home, "commands", Some("commands"), Some("prompts")) {
        // a managed policy can switch off the person's commands and the project's (origin: `user ...`, `project ...`)
        let scope = s.origin.split(' ').next().unwrap_or("");
        if policy.as_ref().is_some_and(|p| !p.extension_allowed(scope, "commands")) {
            continue;
        }
        let mut files = Vec::new();
        collect_md(&s.dir, &s.dir, 0, &mut files);
        files.sort();
        for (name, f) in files {
            let name = match &s.plugin {
                Some(p) => format!("{}:{}", p.to_ascii_lowercase(), name.to_ascii_lowercase()),
                None => name.to_ascii_lowercase(),
            };
            if name.is_empty() || name.contains(char::is_whitespace) || reserved(&name) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let (keys, body) = front_matter(&text);
            let get = |k: &str| keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            let description = get("description").filter(|d| !d.is_empty()).unwrap_or_else(|| first_line(&body));
            out.retain(|c| c.name != name);
            out.push(CommandFile {
                name,
                description,
                hint: get("argument-hint").unwrap_or_default(),
                allowed_tools: get("allowed-tools").unwrap_or_default(),
                body,
                path: f,
                origin: s.origin.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// `(name, path)` of the `.md` files below `dir`, up to three folders deep, hidden ones skipped.
pub(crate) fn collect_md(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            if depth < 3 {
                collect_md(root, &p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()) == Some("md") {
            let rel = p.strip_prefix(root).unwrap_or(&p).with_extension("");
            let name: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            out.push((name.join(":"), p));
        }
    }
}

fn first_line(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_start_matches('#').trim();
    l.chars().take(70).collect()
}

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
    claude_hooks_in(cwd, trust_file, dirs::home_dir().as_deref())
}

fn claude_hooks_in(
    cwd: &Path,
    trust_file: &Path,
    home: Option<&Path>,
) -> (Vec<crate::config::HookConfig>, Option<String>) {
    let root = project_root(cwd);
    if Some(root.as_path()) == home {
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

/// The file a lasting rule is saved to and the text to start it with when it is new: the project's
/// instructions file (`project`: the nearest one nexo reads, else `AGENTS.md` at the repository root),
/// nexo's own prompt file (`global`: `prompt.md` in `config_dir`) or the user's instructions file
/// (`user`, the older name for the same wish); `None` for any other scope.
pub fn rules_file(
    scope: &str,
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
) -> Option<(PathBuf, Option<String>)> {
    match scope {
        "project" => {
            let file = project_file(cwd);
            let file = if file.is_file() { file } else { project_root(cwd).join("AGENTS.md") };
            Some((file, None))
        }
        "global" => Some((config_dir.join("prompt.md"), Some(crate::paths::PREFS_TEMPLATE.to_string()))),
        "user" => Some(user_file(config_dir, home)),
        _ => None,
    }
}

/// The file a lasting rule of `scope` goes to on this machine (see `rules_file`).
pub fn rules_path(scope: &str, cwd: &Path) -> Option<PathBuf> {
    let home = foreign_home();
    rules_file(scope, cwd, &crate::paths::config_dir(), home.as_deref()).map(|(p, _)| p)
}

/// The heading the rules nexo saves go under.
pub const REMEMBERED: &str = "## Remembered by nexo";

/// The words of a line, lower case, without the list marker, a leading `YYYY-MM-DD:` date and the
/// punctuation: two lines with the same key say the same thing.
fn rule_key(line: &str) -> String {
    let l = line.trim().trim_start_matches(['-', '*']).trim();
    let b = l.as_bytes();
    let dated = b.len() > 11
        && b[..10].iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && b[10] == b':';
    let l = if dated { &l[11..] } else { l };
    l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

/// `text` with `rule` added as a dated list line (`- 2026-11-09: the rule`) at the end of its
/// `## Remembered by nexo` section; the section is created at the end when missing. Nothing already
/// in `text` changes, byte for byte. `None` when a line with the same words is already there.
pub fn with_rule(text: &str, rule: &str, date: &str) -> Option<String> {
    let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = rule_key(&rule);
    if key.is_empty() || text.lines().any(|l| rule_key(l) == key) {
        return None;
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let item = format!("- {date}: {rule}{nl}");
    // the section: from the end of its heading line to the next heading (or the end of the text)
    let (mut at, mut start, mut end) = (0, None, text.len());
    for line in text.split_inclusive('\n') {
        if start.is_none() && line.trim().eq_ignore_ascii_case(REMEMBERED) {
            start = Some(at + line.len());
        } else if start.is_some() && line.starts_with('#') {
            end = at;
            break;
        }
        at += line.len();
    }
    let Some(start) = start else {
        let gap = match text {
            "" => "",
            t if t.ends_with('\n') => nl,
            _ => &format!("{nl}{nl}"),
        };
        return Some(format!("{text}{gap}{REMEMBERED}{nl}{item}"));
    };
    // after the last line of the section that has words, so blank lines before the next heading stay
    let body = &text[start..end];
    let used = body.trim_end().len();
    let (cut, lead) = match body[used..].find('\n') {
        _ if used == 0 => (start, if text[..start].ends_with('\n') { "" } else { nl }),
        Some(q) => (start + used + q + 1, ""),
        None => (end, nl),
    };
    Some(format!("{}{lead}{item}{}", &text[..cut], &text[cut..]))
}

/// Saves `rule` into `file` (see `with_rule`, dated today), starting from `seed` when the file is
/// new. `Ok(false)` when it was there already.
pub fn save_rule(file: &Path, seed: Option<&str>, rule: &str) -> std::io::Result<bool> {
    let old = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => seed.unwrap_or("").to_string(),
        Err(e) => return Err(e),
    };
    let today = time::OffsetDateTime::now_utc().date();
    let Some(new) = with_rule(&old, rule, &today.to_string()) else { return Ok(false) };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, new)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(p: PathBuf, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".git")).unwrap();
        d
    }

    const CLAUDE_HOOKS: &str = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash|Edit", "hooks": [{"type": "command", "command": "check.sh", "timeout": 5}, {"type": "prompt", "command": "x"}]}], "Stop": [{"hooks": [{"command": "done.sh"}]}], "Whatever": [{"hooks": [{"command": "no.sh"}]}]}}"#;

    #[test]
    fn claude_settings_hooks_load_only_in_a_trusted_project_with_nexo_tool_names() {
        let (p, t) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join(".claude/settings.json"), CLAUDE_HOOKS);
        let trust = t.path().join("trusted");
        let (none, note) = claude_hooks(p.path(), &trust);
        assert!(none.is_empty() && note.unwrap().contains("nexo config trust"));
        crate::config::trust_project(p.path(), &trust).unwrap();
        let (got, note) = claude_hooks(p.path(), &trust);
        assert!(note.is_none());
        let seen: Vec<(String, Option<String>, String, Option<u64>)> =
            got.iter().map(|h| (h.event.clone(), h.matcher.clone(), h.command.clone(), h.timeout)).collect();
        assert_eq!(
            seen,
            [
                ("pre_tool".into(), Some("bash|edit".into()), "check.sh".into(), Some(5)),
                ("stop".into(), None, "done.sh".into(), None),
            ]
        );
        assert_eq!(got[0].source, "project .claude/settings.json");
    }

    #[test]
    fn a_project_at_the_home_folder_brings_no_claude_hooks() {
        // there `.claude/settings.json` is Claude Code's own user file, trusted or not
        let (p, t) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join(".claude/settings.json"), CLAUDE_HOOKS);
        let trust = t.path().join("trusted");
        crate::config::trust_project(p.path(), &trust).unwrap();
        let below = p.path().join("work/dir");
        std::fs::create_dir_all(&below).unwrap();
        for cwd in [p.path(), below.as_path()] {
            assert_eq!(claude_hooks_in(cwd, &trust, Some(p.path())), (Vec::new(), None));
            assert_eq!(claude_hooks_in(cwd, &trust, Some(t.path())).0.len(), 2);
        }
    }

    #[test]
    fn claude_matchers_become_nexo_tool_names() {
        assert_eq!(map_matcher("Bash").as_deref(), Some("bash"));
        assert_eq!(map_matcher("Edit|Write").as_deref(), Some("edit|write"));
        assert_eq!(map_matcher("*"), None);
        assert_eq!(map_matcher(""), None);
    }

    #[test]
    fn agents_md_wins_over_claude_md_and_the_local_file_adds() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        w(p.path().join("AGENTS.md"), "agents");
        w(p.path().join("CLAUDE.md"), "claude");
        w(p.path().join("CLAUDE.local.md"), "mine");
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["agents", "mine"]);
        std::fs::remove_file(p.path().join("AGENTS.md")).unwrap();
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["claude", "mine"]);
    }

    #[test]
    fn user_level_fallbacks_only_when_nexo_has_none() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        w(home.path().join(".claude/CLAUDE.md"), "c-user");
        w(home.path().join(".codex/AGENTS.md"), "x-user");
        let got: Vec<String> =
            instructions(p.path(), cfg.path(), Some(home.path())).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["x-user", "c-user"]);
        w(cfg.path().join("AGENTS.md"), "own");
        let got = instructions(p.path(), cfg.path(), Some(home.path()));
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].text.as_str(), got[0].origin), ("own", "user"));
    }

    #[test]
    fn memory_opens_the_nearest_project_file_and_starts_the_users_own_from_the_fallback() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        let sub = p.path().join("web");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(project_file(&sub), sub.join("AGENTS.md"), "none yet: a new one where the person is");
        w(p.path().join("CLAUDE.md"), "Root rules.");
        assert_eq!(project_file(&sub), p.path().join("CLAUDE.md"));
        w(sub.join("NEXO.md"), "Web rules.");
        assert_eq!(project_file(&sub), sub.join("NEXO.md"));

        let home = tempfile::tempdir().unwrap();
        assert_eq!(user_file(cfg.path(), Some(home.path())), (cfg.path().join("AGENTS.md"), None));
        w(home.path().join(".claude/CLAUDE.md"), "Answer in Spanish.");
        let (path, seed) = user_file(cfg.path(), Some(home.path()));
        assert_eq!(path, cfg.path().join("AGENTS.md"));
        assert_eq!(seed.as_deref(), Some("Answer in Spanish.\n"), "what nexo read stays");
        w(cfg.path().join("NEXO.md"), "Mine.");
        assert_eq!(user_file(cfg.path(), Some(home.path())), (cfg.path().join("NEXO.md"), None));
    }
    #[test]
    fn imports_are_relative_limited_and_never_cycle() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(
            p.path().join("AGENTS.md"),
            "Top\nSee @docs/a.md for more.\n```\n@docs/a.md\n```\nmail me@docs/a.md and @types/node",
        );
        w(p.path().join("docs/a.md"), "A text\n@b.md\n@../AGENTS.md");
        w(p.path().join("docs/b.md"), "B text");
        let t = &instructions(p.path(), cfg.path(), None)[0].text;
        assert!(t.contains("A text") && t.contains("B text"), "{t}");
        assert_eq!(t.matches("A text").count(), 1, "the fenced mention and the mid-word one do not import: {t}");
        assert_eq!(t.matches("Top").count(), 1, "the cycle back to the top file is skipped: {t}");
        assert!(t.contains("@types/node"));
        // depth: a chain of files stops after 5 hops
        for i in 0..8 {
            w(p.path().join(format!("c{i}.md")), &format!("level{i}\n@c{}.md", i + 1));
        }
        w(p.path().join("AGENTS.md"), "@c0.md");
        let t = &instructions(p.path(), cfg.path(), None)[0].text;
        assert!(t.contains("level4") && !t.contains("level5"), "{t}");
    }

    #[test]
    fn a_key_file_is_never_imported() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join(".env"), "SECRET=1");
        w(p.path().join("AGENTS.md"), "x\n@.env");
        assert!(!instructions(p.path(), cfg.path(), None)[0].text.contains("SECRET"));
    }

    #[test]
    fn nested_files_are_listed_but_those_on_the_way_are_loaded() {
        let (p, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(p.path().join("AGENTS.md"), "root");
        w(p.path().join("web/CLAUDE.md"), "web");
        w(p.path().join("api/AGENTS.md"), "api");
        w(p.path().join("api/src/CLAUDE.md"), "deep");
        let cwd = p.path().join("api");
        let got: Vec<String> = instructions(&cwd, cfg.path(), None).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["api", "root"]);
        let nested = nested_instructions(&cwd);
        assert_eq!(nested, [PathBuf::from("api/src/CLAUDE.md"), PathBuf::from("web/CLAUDE.md")]);
    }

    #[test]
    fn commands_nest_with_colons_and_nexo_wins() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let trust = cfg.path().join("trusted-projects");
        w(home.path().join(".codex/prompts/ship.md"), "codex ship");
        w(
            p.path().join(".claude/commands/ship.md"),
            "---\ndescription: Ship it\nargument-hint: [env]\nallowed-tools: Bash(git:*)\n---\nclaude ship",
        );
        w(p.path().join(".claude/commands/git/sync.md"), "# Sync\nsync");
        w(cfg.path().join("commands/ship.md"), "nexo ship");
        let none = |_: &str| false;
        let before = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let names: Vec<&str> = before.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ship"], "an untrusted project's commands are left out");
        assert_eq!(before[0].body, "nexo ship");
        crate::config::trust_project(p.path(), &trust).unwrap();
        let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["git:sync", "ship"]);
        assert_eq!(all[1].origin, "user .nexo/commands");
        assert_eq!(all[0].origin, "project .claude/commands");
        assert_eq!(all[0].description, "Sync");
        std::fs::remove_file(cfg.path().join("commands/ship.md")).unwrap();
        let all = commands(p.path(), cfg.path(), &trust, Some(home.path()), &none);
        let ship = all.iter().find(|c| c.name == "ship").unwrap();
        assert_eq!(
            (ship.description.as_str(), ship.hint.as_str(), ship.allowed_tools.as_str()),
            ("Ship it", "[env]", "Bash(git:*)")
        );
        assert_eq!(ship.body, "claude ship", "the project's own folder beats the user's");
    }

    #[test]
    fn a_home_kept_in_git_reads_its_dot_folders_once_as_the_persons_own() {
        let (home, cfg) = (repo(), tempfile::tempdir().unwrap());
        w(home.path().join(".claude/CLAUDE.md"), "c-user");
        w(home.path().join(".claude/commands/mine.md"), "Mine.");
        let sub = home.path().join("work");
        std::fs::create_dir_all(&sub).unwrap();
        let got = instructions(&sub, cfg.path(), Some(home.path()));
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].origin, "user fallback");
        let trust = cfg.path().join("trusted-projects");
        crate::config::trust_project(home.path(), &trust).unwrap();
        let src = sources(&sub, cfg.path(), &trust, Some(home.path()), "commands", Some("commands"), None);
        let claude = home.path().join(".claude/commands");
        assert_eq!(src.iter().filter(|s| s.dir == claude).count(), 1, "{src:?}");
        assert!(src.iter().all(|s| !s.origin.starts_with("project .claude")), "{src:?}");
        // another repository still has its own project folders
        let other = repo();
        crate::config::trust_project(other.path(), &trust).unwrap();
        let src = sources(other.path(), cfg.path(), &trust, Some(home.path()), "commands", Some("commands"), None);
        assert!(src.iter().any(|s| s.origin.starts_with("project")), "{src:?}");
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    const D: &str = "2026-11-09";

    #[test]
    fn a_rule_goes_under_the_remembered_heading_and_nothing_else_moves() {
        let before = "# Project\n\nBuild with make.\n\n## Rules\n- Use tabs.\n\n## Remembered by nexo\n- 2026-11-01: Old one.\n\n## Other\ntext\n";
        let t = with_rule(before, "Run `make fmt` first.", D).unwrap();
        assert_eq!(
            t,
            "# Project\n\nBuild with make.\n\n## Rules\n- Use tabs.\n\n## Remembered by nexo\n- 2026-11-01: Old one.\n- 2026-11-09: Run `make fmt` first.\n\n## Other\ntext\n"
        );
        // near-exact copies (case, spacing, punctuation, the date) and lines of the person's own are skipped
        assert!(with_rule(&t, "run `make fmt`  first", "2030-01-01").is_none());
        assert!(with_rule(&t, "old one", D).is_none());
        assert!(with_rule(&t, "Use tabs!", D).is_none());
        // a file without the heading gets it at the end, with a blank line before it
        assert_eq!(with_rule("Mine.", "A.", D).unwrap(), "Mine.\n\n## Remembered by nexo\n- 2026-11-09: A.\n");
        assert_eq!(with_rule("Mine.\n", "A.", D).unwrap(), "Mine.\n\n## Remembered by nexo\n- 2026-11-09: A.\n");
        assert_eq!(with_rule("", "A.", D).unwrap(), "## Remembered by nexo\n- 2026-11-09: A.\n");
        // an empty section, at the very end or without a final newline
        assert_eq!(with_rule("## Remembered by nexo\n", "A.", D).unwrap(), "## Remembered by nexo\n- 2026-11-09: A.\n");
        assert_eq!(
            with_rule("## Remembered by nexo\n- 2026-11-01: B.", "A.", D).unwrap(),
            "## Remembered by nexo\n- 2026-11-01: B.\n- 2026-11-09: A.\n"
        );
        // Windows line endings stay Windows line endings
        assert_eq!(
            with_rule("Mine.\r\nMore.\r\n", "A.", D).unwrap(),
            "Mine.\r\nMore.\r\n\r\n## Remembered by nexo\r\n- 2026-11-09: A.\r\n"
        );
    }

    #[test]
    fn save_rule_appends_dated_and_dedupes() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("AGENTS.md");
        std::fs::write(&f, "# Project\n\nBuild with make.\n").unwrap();
        assert!(save_rule(&f, None, "Run `make fmt` first.").unwrap());
        assert!(!save_rule(&f, None, "run `make fmt`  first.").unwrap());
        let t = std::fs::read_to_string(&f).unwrap();
        let today = time::OffsetDateTime::now_utc().date();
        assert_eq!(
            t,
            format!("# Project\n\nBuild with make.\n\n## Remembered by nexo\n- {today}: Run `make fmt` first.\n")
        );
    }

    #[test]
    fn each_scope_has_its_file_and_the_next_session_reads_it() {
        let d = tempfile::tempdir().unwrap();
        let (cfg, proj) = (d.path().join("cfg"), d.path().join("proj"));
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join("src/deep")).unwrap();
        let (f, seed) = rules_file("global", &proj, &cfg, None).unwrap();
        assert_eq!(f, cfg.join("prompt.md"));
        assert!(save_rule(&f, seed.as_deref(), "Answer in English.").unwrap());
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with(crate::paths::PREFS_TEMPLATE), "{text}");
        assert!(text.contains("## Remembered by nexo\n- "), "{text}");
        // from a sub-folder the project file is at the repository root, created when missing
        let (p, _) = rules_file("project", &proj.join("src/deep"), &cfg, None).unwrap();
        assert_eq!(p, proj.join("AGENTS.md"));
        assert!(save_rule(&p, None, "Never edit vendor/.").unwrap());
        let all = instructions(&proj, &cfg, None);
        assert!(all.iter().any(|i| i.text.contains("Never edit vendor/.")), "{all:?}");
        // an instructions file nexo already reads is the one that grows
        std::fs::remove_file(&p).unwrap();
        std::fs::write(proj.join("CLAUDE.md"), "Mine.\n").unwrap();
        assert_eq!(rules_file("project", &proj, &cfg, None).unwrap().0, proj.join("CLAUDE.md"));
        assert_eq!(rules_file("user", &proj, &cfg, None).unwrap().0, cfg.join("AGENTS.md"));
        assert!(rules_file("session", &proj, &cfg, None).is_none());
    }
}
