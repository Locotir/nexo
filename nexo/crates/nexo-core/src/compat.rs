//! Day-one compatibility with the files a team already keeps for other coding agents:
//! `CLAUDE.md`, `CLAUDE.local.md` and nested `AGENTS.md` instructions with `@path` imports,
//! commands in `.claude/commands` and `.codex/prompts`, and sub-agents in `.claude/agents`.
//!
//! Everything here takes its folders as arguments, so tests use temporary ones. nexo's own files
//! (`NEXO.md`, `.nexo/commands`, `.nexo/agents`) win over the foreign ones on a clash, and a
//! project's foreign commands and agents are left out until the project is trusted, like nexo's.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether Claude Code's files are read at all (`[compat] claude_code`): `CLAUDE.md`, `CLAUDE.local.md`,
/// `.claude/` in the project and `~/.claude` in the home. Process wide, set once when the configuration is loaded.
static CLAUDE_CODE: AtomicBool = AtomicBool::new(true);

/// Turns the reading of Claude Code's files on or off for this process (`[compat] claude_code`).
pub fn set_claude_code(on: bool) {
    CLAUDE_CODE.store(on, Ordering::Relaxed);
}

/// Whether Claude Code's files are read (on unless `[compat] claude_code = false`).
pub fn claude_code() -> bool {
    CLAUDE_CODE.load(Ordering::Relaxed)
}

/// How many imports may be nested in one another.
const IMPORT_DEPTH: usize = 5;
/// The most nested instruction files listed.
const NESTED_MAX: usize = 20;

/// The nearest ancestor of `cwd` with a `.git` entry below the person's home, else `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    project_root_with(cwd, dirs::home_dir().as_deref())
}

/// Where scratch folders go: `dir` (on disk, under a build's target dir) unless it lies inside a git
/// repository, else the system temp dir. A test folder inside the checkout it was built from would be taken
/// for that project: its git line, its worktrees, its instruction files.
pub fn scratch_base(dir: &Path) -> PathBuf {
    match project_root(dir).join(".git").exists() {
        true => std::env::temp_dir(),
        false => dir.to_path_buf(),
    }
}

/// [`project_root`] below the person's home `own_home`.
fn project_root_with(cwd: &Path, own_home: Option<&Path>) -> PathBuf {
    for d in cwd.ancestors() {
        if home_stop(d, cwd, own_home) {
            break;
        }
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

/// Whether the walk up from `cwd` stops at `d` as the person's home (`own_home`): it does, kept in git or
/// not (as for skills), unless the person works in the home itself and it is a real repository (a
/// container whose home is the project); a stray `.git` folder makes none.
fn home_stop(d: &Path, cwd: &Path, own_home: Option<&Path>) -> bool {
    let repo = d.join(".git").is_file() || d.join(".git/HEAD").is_file();
    crate::paths::is_home(d, own_home) && !(d == cwd && repo)
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
    instructions_with(cwd, config_dir, home, claude_code(), dirs::home_dir().as_deref())
}

/// The folders whose instruction files are the project's, nearest first: from `cwd` up to the repository
/// root. The person's home (`own_home`) is none (see [`home_stop`]): a file there speaks to every tool run
/// in any folder below it (another tool's state notes, say), and nexo's own user file lives in its config
/// folder.
fn project_dirs<'a>(cwd: &'a Path, own_home: Option<&Path>) -> Vec<&'a Path> {
    let mut out = Vec::new();
    for d in cwd.ancestors() {
        if home_stop(d, cwd, own_home) {
            break;
        }
        out.push(d);
        if d.join(".git").exists() {
            break;
        }
    }
    out
}

/// Whether `cwd` is in no project: the person's home (see [`home_stop`]). nexo maps no tree there and
/// reads no instruction file of it, so the notes and folders of everything kept under it stay out.
pub fn no_project(cwd: &Path) -> bool {
    project_dirs(cwd, dirs::home_dir().as_deref()).is_empty()
}

/// [`instructions`] with Claude Code's files read (`claude`) or left out, below the person's home `own_home`.
fn instructions_with(
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
    claude: bool,
    own_home: Option<&Path>,
) -> Vec<Instruction> {
    let mut out = Vec::new();
    let own: Vec<PathBuf> =
        ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).filter(|p| p.is_file()).collect();
    if own.is_empty() {
        if let Some(h) = home {
            for rel in fallbacks(claude) {
                push(&mut out, h.join(rel), "user fallback");
            }
        }
    } else {
        for p in own {
            push(&mut out, p, "user");
        }
    }
    for d in project_dirs(cwd, own_home) {
        push(&mut out, d.join("NEXO.md"), "project");
        let agents = d.join("AGENTS.md");
        let had_agents = agents.is_file();
        push(&mut out, agents, "project");
        if !had_agents && claude {
            if d.join("CLAUDE.md").is_file() {
                push(&mut out, d.join("CLAUDE.md"), "project");
            } else {
                push(&mut out, d.join(".claude").join("CLAUDE.md"), "project");
            }
        }
        if claude {
            push(&mut out, d.join("CLAUDE.local.md"), "project (local)");
        }
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
/// to the repository root, or `AGENTS.md` in `cwd` when there is none yet. In the person's home, which is
/// no project, it is nexo's own user file.
pub fn project_file(cwd: &Path) -> PathBuf {
    project_file_with(cwd, claude_code(), dirs::home_dir().as_deref(), &crate::paths::config_dir())
}

fn project_file_with(cwd: &Path, claude: bool, own_home: Option<&Path>, config_dir: &Path) -> PathBuf {
    let dirs = project_dirs(cwd, own_home);
    for d in &dirs {
        for name in ["NEXO.md", "AGENTS.md", "CLAUDE.md", ".claude/CLAUDE.md"] {
            if (claude || !name.contains("CLAUDE")) && d.join(name).is_file() {
                return d.join(name);
            }
        }
    }
    if dirs.is_empty() {
        return user_file_with(config_dir, None, claude).0;
    }
    cwd.join("AGENTS.md")
}

/// The user file `/memory user` opens: nexo's own in `config_dir`, and when it does not exist yet,
/// the text to start it with: what the other tools' files under `home` say, which nexo reads in its
/// place until it exists and never after.
pub fn user_file(config_dir: &Path, home: Option<&Path>) -> (PathBuf, Option<String>) {
    user_file_with(config_dir, home, claude_code())
}

fn user_file_with(config_dir: &Path, home: Option<&Path>, claude: bool) -> (PathBuf, Option<String>) {
    if let Some(p) = ["NEXO.md", "AGENTS.md"].iter().map(|n| config_dir.join(n)).find(|p| p.is_file()) {
        return (p, None);
    }
    let mut fallback = Vec::new();
    if let Some(h) = home {
        for rel in fallbacks(claude) {
            push(&mut fallback, h.join(rel), "user fallback");
        }
    }
    let seed = fallback.iter().map(|i| i.text.trim_end()).collect::<Vec<_>>().join("\n\n");
    (config_dir.join("AGENTS.md"), (!seed.is_empty()).then(|| format!("{seed}\n")))
}

/// The other tools' user files read in place of nexo's own, under the home folder.
fn fallbacks(claude: bool) -> &'static [&'static str] {
    if claude {
        &[".codex/AGENTS.md", ".claude/CLAUDE.md"]
    } else {
        &[".codex/AGENTS.md"]
    }
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
    nested_instructions_with(cwd, claude_code(), dirs::home_dir().as_deref())
}

fn nested_instructions_with(cwd: &Path, claude: bool, own_home: Option<&Path>) -> Vec<PathBuf> {
    // the person's home is no project: the other folders' files under it are theirs, not a part of this one
    if project_dirs(cwd, own_home).is_empty() {
        return Vec::new();
    }
    let root = project_root_with(cwd, own_home);
    let loaded: Vec<&Path> = cwd.ancestors().take_while(|d| d.starts_with(&root)).collect();
    let mut found = Vec::new();
    let walk = ignore::WalkBuilder::new(&root).max_depth(Some(5)).build();
    for (seen, e) in walk.flatten().enumerate() {
        if seen > 20_000 || found.len() >= NESTED_MAX {
            break;
        }
        let name = e.file_name().to_string_lossy();
        let wanted = match name.as_ref() {
            "AGENTS.md" => true,
            "CLAUDE.md" | "CLAUDE.local.md" => claude,
            _ => false,
        };
        if !wanted || !e.path().is_file() {
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
    let claude_sub = claude_sub.filter(|_| claude_code());
    sources_with(cwd, config_dir, trust_file, home, nexo_sub, claude_sub, codex_sub)
}

fn sources_with(
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
    if !claude_code() {
        return (Vec::new(), None);
    }
    claude_hooks_in(cwd, trust_file, dirs::home_dir().as_deref())
}

fn claude_hooks_in(
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
    rules_file_with(scope, cwd, config_dir, home, dirs::home_dir().as_deref())
}

/// [`rules_file`] below the person's home `own_home`: a project rule asked for there is the user's.
fn rules_file_with(
    scope: &str,
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
    own_home: Option<&Path>,
) -> Option<(PathBuf, Option<String>)> {
    match scope {
        // no seed: a note is no reason to copy another agent's user file into nexo's own
        "project" if project_dirs(cwd, own_home).is_empty() => Some((user_file(config_dir, home).0, None)),
        "project" => {
            let file = project_file_with(cwd, claude_code(), own_home, config_dir);
            let file = if file.is_file() { file } else { project_root_with(cwd, own_home).join("AGENTS.md") };
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

/// What the model reads on the line under that heading, so saved rules are taken as background and not as
/// something the person just said. It goes into the text that is sent, never into the file.
pub const REMEMBERED_FRAME: &str = "(Saved by nexo in earlier sessions. Take it as background to apply when it \
                                    matters: the person did not just say it, so do not answer it or bring it up.)";

/// `text` with [`REMEMBERED_FRAME`] on the line after its `## Remembered by nexo` heading; `text` as it
/// is when it has no such heading.
pub fn framed(text: &str) -> std::borrow::Cow<'_, str> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        at += line.len();
        if line.trim().eq_ignore_ascii_case(REMEMBERED) {
            let nl = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let gap = if line.ends_with('\n') { "" } else { nl };
            return std::borrow::Cow::Owned(format!("{}{gap}{REMEMBERED_FRAME}{nl}{}", &text[..at], &text[at..]));
        }
    }
    std::borrow::Cow::Borrowed(text)
}

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

/// What `save_rule` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    /// Written as a new line.
    New,
    /// A line with the same words was there: nothing written.
    There,
    /// Nothing written: this line under `## Remembered by nexo` (without its list marker) says nearly the same.
    Near(String),
}

/// Telling words a rule and a saved line need to share at least before they can count as nearly the same.
const NEAR_WORDS: usize = 3;

/// Words that join a sentence and are in every line, so two different facts share them.
const JOINING: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "be", "it", "its", "on", "in", "of", "to", "at", "by", "as", "for", "and",
    "or", "with", "that", "this", "if",
];

/// The words of a line that tell what it says: all but the joining ones.
fn telling(line: &str) -> std::collections::BTreeSet<String> {
    rule_key(line).split(' ').filter(|w| !JOINING.contains(w)).map(str::to_string).collect()
}

/// The lines of the `## Remembered by nexo` section of `text` (up to the next heading), each with the byte
/// where it starts and its line end.
fn remembered_lines(text: &str) -> Vec<(usize, &str)> {
    let (mut at, mut inside, mut out) = (0, false, Vec::new());
    for line in text.split_inclusive('\n') {
        if !inside {
            inside = line.trim().eq_ignore_ascii_case(REMEMBERED);
        } else if line.starts_with('#') {
            break;
        } else {
            out.push((at, line));
        }
        at += line.len();
    }
    out
}

/// The one of `lines` that says nearly what `rule` says (see `near_rule`).
fn nearest<'a>(lines: &[(usize, &'a str)], rule: &str) -> Option<(usize, &'a str)> {
    let mine = telling(rule);
    let mut best: Option<(usize, (usize, &str))> = None;
    for l in lines {
        let theirs = telling(l.1);
        let shared = mine.intersection(&theirs).count();
        if shared >= NEAR_WORDS
            && shared * 10 >= mine.len().min(theirs.len()) * 6
            && best.is_none_or(|(n, _)| shared > n)
        {
            best = Some((shared, *l));
        }
    }
    best.map(|(_, l)| l)
}

/// A saved line as it is shown: without its line end and its list marker.
fn bare(line: &str) -> String {
    line.trim().trim_start_matches(['-', '*']).trim().to_string()
}

/// The line under `## Remembered by nexo` that says nearly what `rule` says, without its list marker: at
/// least `NEAR_WORDS` telling words in common, and six in ten of those of the shorter of the two. A model saves
/// a fact again in other words once the message that showed the first save has left its context, and the
/// section grows with the same fact over and over. Only nexo's own section is compared: what the person
/// wrote elsewhere in the file is theirs, and saying it again as a rule is the caller's choice. Two different
/// facts in like words can match too, so this is a question for the caller (`save_rule` with `anyway`), not a verdict.
pub fn near_rule(text: &str, rule: &str) -> Option<String> {
    nearest(&remembered_lines(text), rule).map(|(_, l)| bare(l))
}

/// The saved line that `old` names: the list line of `lines` with its words (the marker and the date do not
/// count), else the only one that says nearly the same on both sides (`NEAR_WORDS` telling words shared, six
/// in ten of those of the longer of the two). A vague quote, or one that fits two lines, names none: the wrong
/// line rewritten is worse than none. A line the person wrote in the section without a list marker is theirs.
fn named<'a>(lines: &[(usize, &'a str)], old: &str) -> Option<(usize, &'a str)> {
    let items = || lines.iter().copied().filter(|(_, l)| l.trim_start().starts_with(['-', '*']));
    let want = rule_key(old);
    if want.is_empty() {
        return None;
    }
    if let Some(l) = items().find(|(_, l)| rule_key(l) == want) {
        return Some(l);
    }
    let mine = telling(old);
    let mut like = items().filter(|(_, l)| {
        let theirs = telling(l);
        let shared = mine.intersection(&theirs).count();
        shared >= NEAR_WORDS && shared * 10 >= mine.len().max(theirs.len()) * 6
    });
    match (like.next(), like.next()) {
        (Some(l), None) => Some(l),
        _ => None,
    }
}

/// `text` with the line of its `## Remembered by nexo` section that `old` names (see `named`) put as `rule`,
/// dated `date`, in the same place; and that line as it was, without its list marker. `None` when the
/// section has no such line, or when a line of `text` already says `rule`. Every other byte of `text`
/// stays: the person's own lines are never the ones changed.
pub fn with_replaced(text: &str, old: &str, rule: &str, date: &str) -> Option<(String, String)> {
    let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = rule_key(&rule);
    if key.is_empty() || text.lines().any(|l| rule_key(l) == key) {
        return None;
    }
    let (at, line) = named(&remembered_lines(text), old)?;
    let end = &line[line.trim_end_matches(['\r', '\n']).len()..];
    Some((format!("{}- {date}: {rule}{end}{}", &text[..at], &text[at + line.len()..]), bare(line)))
}

/// What `replace_rule` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replaced {
    /// The named line was rewritten; this is what it said, without its list marker.
    Done(String),
    /// Nothing written: no saved line is the one named.
    NoLine,
    /// Nothing written: a line of the file already says the rule.
    There,
}

/// Puts `rule` in the place of the saved line of `file` that `old` names (see `with_replaced`, dated today).
pub fn replace_rule(file: &Path, old: &str, rule: &str) -> std::io::Result<Replaced> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replaced::NoLine),
        Err(e) => return Err(e),
    };
    let key = rule_key(rule);
    if !key.is_empty() && text.lines().any(|l| rule_key(l) == key) {
        return Ok(Replaced::There);
    }
    let today = time::OffsetDateTime::now_utc().date();
    let Some((new, was)) = with_replaced(&text, old, rule, &today.to_string()) else { return Ok(Replaced::NoLine) };
    std::fs::write(file, new)?;
    Ok(Replaced::Done(was))
}

/// Saves `rule` into `file` (see `with_rule`, dated today), starting from `seed` when the file is new.
/// A rule the remembered section nearly says already (`near_rule`) is not written unless `anyway`.
pub fn save_rule(file: &Path, seed: Option<&str>, rule: &str, anyway: bool) -> std::io::Result<Saved> {
    let old = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => seed.unwrap_or("").to_string(),
        Err(e) => return Err(e),
    };
    let today = time::OffsetDateTime::now_utc().date();
    let Some(new) = with_rule(&old, rule, &today.to_string()) else { return Ok(Saved::There) };
    if !anyway {
        if let Some(line) = near_rule(&old, rule) {
            return Ok(Saved::Near(line));
        }
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, new)?;
    Ok(Saved::New)
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
    fn the_persons_home_is_no_project_seen_from_below_and_one_only_as_a_real_repository() {
        // a home holding another tool's state notes, and a folder below it that is no repository
        let (home, cfg, other) =
            (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let h = home.path();
        w(h.join("AGENTS.md"), "Read this first: the lanes and their state.");
        w(h.join("CLAUDE.md"), "Act for the boss, unasked.");
        w(h.join("work/AGENTS.md"), "Work rules.");
        w(other.path().join(".claude/CLAUDE.md"), "Another agent's mandate.");
        let sub = h.join("Downloads");
        std::fs::create_dir_all(&sub).unwrap();
        let none_from = |cwd: &Path| {
            let got = instructions_with(cwd, cfg.path(), None, true, Some(h));
            assert!(got.is_empty(), "{cwd:?}: {got:?}");
            assert!(nested_instructions_with(cwd, true, Some(h)).is_empty(), "{cwd:?}");
        };
        // no `.git`, then a stray `.git` folder that is no repository: the same
        for stray in [false, true] {
            if stray {
                std::fs::create_dir_all(h.join(".git/info")).unwrap();
            }
            none_from(h);
            none_from(&sub);
            assert!(project_dirs(h, Some(h)).is_empty() && project_dirs(&sub, Some(h)) == [sub.as_path()]);
            assert_eq!(project_root_with(&sub, Some(h)), sub);
            // what /memory opens and where a project rule goes: nexo's own user file in the home (never seeded
            // from another agent's), a new file below it
            assert_eq!(project_file_with(h, true, Some(h), cfg.path()), cfg.path().join("AGENTS.md"));
            assert_eq!(project_file_with(&sub, true, Some(h), cfg.path()), sub.join("AGENTS.md"));
            let (f, seed) = rules_file_with("project", h, cfg.path(), Some(other.path()), Some(h)).unwrap();
            assert_eq!((f, seed), (cfg.path().join("AGENTS.md"), None));
            assert_eq!(rules_file_with("project", &sub, cfg.path(), None, Some(h)).unwrap().0, sub.join("AGENTS.md"));
        }
        // a repository below the home is a project
        let work = h.join("work");
        std::fs::create_dir_all(work.join(".git")).unwrap();
        let got: Vec<String> =
            instructions_with(&work, cfg.path(), None, true, Some(h)).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["Work rules."]);
        // a home that is a real repository (a container whose home is the project) is one in the home itself,
        // never from a folder below it
        w(h.join(".git/HEAD"), "ref: refs/heads/main\n");
        let got: Vec<String> =
            instructions_with(h, cfg.path(), None, true, Some(h)).into_iter().map(|i| i.text).collect();
        assert_eq!(got, ["Read this first: the lanes and their state."]);
        none_from(&sub);
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
    fn with_claude_code_off_no_file_of_it_is_read() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        w(home.path().join(".claude/CLAUDE.md"), "c-user");
        w(home.path().join(".codex/AGENTS.md"), "x-user");
        w(p.path().join("CLAUDE.md"), "claude");
        w(p.path().join("CLAUDE.local.md"), "mine");
        w(p.path().join("web/CLAUDE.md"), "web");
        w(p.path().join("web/AGENTS.md"), "web agents");
        let texts = |claude| -> Vec<String> {
            instructions_with(p.path(), cfg.path(), Some(home.path()), claude, None)
                .into_iter()
                .map(|i| i.text)
                .collect()
        };
        assert_eq!(texts(true), ["x-user", "c-user", "claude", "mine"]);
        assert_eq!(texts(false), ["x-user"], "the codex file and nexo's own stay");
        w(p.path().join("AGENTS.md"), "agents");
        assert_eq!(texts(false), ["x-user", "agents"]);
        assert_eq!(
            nested_instructions_with(p.path(), true, None),
            [PathBuf::from("web/AGENTS.md"), PathBuf::from("web/CLAUDE.md")]
        );
        assert_eq!(nested_instructions_with(p.path(), false, None), [PathBuf::from("web/AGENTS.md")]);

        let bare = repo();
        w(bare.path().join("CLAUDE.md"), "Root rules.");
        assert_eq!(project_file_with(bare.path(), true, None, bare.path()), bare.path().join("CLAUDE.md"));
        assert_eq!(project_file_with(bare.path(), false, None, bare.path()), bare.path().join("AGENTS.md"));
        let (path, seed) = user_file_with(cfg.path(), Some(home.path()), false);
        assert_eq!((path, seed.as_deref()), (cfg.path().join("AGENTS.md"), Some("x-user\n")));
        let (_, seed) = user_file_with(cfg.path(), Some(home.path()), true);
        assert_eq!(seed.as_deref(), Some("x-user\n\nc-user\n"));
    }

    #[test]
    fn with_claude_code_off_its_command_and_agent_folders_are_not_sources() {
        let (p, cfg, home) = (repo(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let trust = cfg.path().join("trusted");
        std::fs::write(&trust, format!("{}\n", p.path().canonicalize().unwrap().display())).unwrap();
        let dirs = |claude_sub| -> Vec<String> {
            sources_with(p.path(), cfg.path(), &trust, Some(home.path()), "commands", claude_sub, Some("prompts"))
                .into_iter()
                .map(|s| s.origin)
                .collect()
        };
        assert!(dirs(Some("commands")).iter().any(|o| o.contains(".claude")));
        let off = dirs(None);
        assert!(off.iter().all(|o| !o.contains(".claude")), "{off:?}");
        assert!(off.iter().any(|o| o.contains(".codex")), "{off:?}");
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
    fn the_text_sent_says_under_the_heading_that_saved_rules_are_background() {
        let f = REMEMBERED_FRAME;
        assert_eq!(framed("# P\n\nBuild with make.\n"), "# P\n\nBuild with make.\n");
        assert!(matches!(framed("no heading"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(
            framed("# P\n\n## Remembered by nexo\n- 2026-11-01: A.\n\n## Other\nx\n"),
            format!("# P\n\n## Remembered by nexo\n{f}\n- 2026-11-01: A.\n\n## Other\nx\n")
        );
        // the heading as the last line, with and without its line end, and Windows line ends
        assert_eq!(framed("## Remembered by nexo"), format!("## Remembered by nexo\n{f}\n"));
        assert_eq!(framed("## Remembered by nexo\n"), format!("## Remembered by nexo\n{f}\n"));
        assert_eq!(
            framed("A\r\n## remembered by nexo\r\n- x\r\n"),
            format!("A\r\n## remembered by nexo\r\n{f}\r\n- x\r\n")
        );
        // the frame is one line and the file never gets it: a rule saved into framed text is not the way in
        assert!(!f.contains('\n') && f.starts_with('(') && f.ends_with(')'));
        assert_eq!(
            with_rule("## Remembered by nexo\n- 2026-11-01: B.", "A.", "2026-11-09").unwrap().matches(f).count(),
            0
        );
    }

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
        assert_eq!(save_rule(&f, None, "Run `make fmt` first.", false).unwrap(), Saved::New);
        assert_eq!(save_rule(&f, None, "run `make fmt`  first.", false).unwrap(), Saved::There);
        let today = time::OffsetDateTime::now_utc().date();
        let one = format!("# Project\n\nBuild with make.\n\n## Remembered by nexo\n- {today}: Run `make fmt` first.\n");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), one);
        // nearly the same words: nothing is written and the line that is there comes back
        let near = save_rule(&f, None, "Always run `make fmt` first, before a commit.", false).unwrap();
        assert_eq!(near, Saved::Near(format!("{today}: Run `make fmt` first.")));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), one);
        // the caller can insist; the very same words stay once all the same
        assert_eq!(save_rule(&f, None, "Always run `make fmt` first, before a commit.", true).unwrap(), Saved::New);
        assert_eq!(save_rule(&f, None, "Run `make fmt` first.", true).unwrap(), Saved::There);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            format!("{one}- {today}: Always run `make fmt` first, before a commit.\n")
        );
    }

    #[test]
    fn a_rule_the_remembered_section_nearly_says_is_found_and_the_persons_own_text_is_not_compared() {
        let a = r"Acme install folder is always %LOCALAPPDATA%\Acme; if it already exists, only the config file is overwritten (Launch.c:50 APP_DIRECTORY, :1349 installed flag, :1519 returns TRUE). No per-game names.";
        let b = r"Acme install folder always %LOCALAPPDATA%\Acme; if it already exists only config.json is rewritten (Launch.c:50 APP_DIRECTORY, :1519 installed branch), confirmed 2026-11-10.";
        let text = format!(
            "# Project\n\nThe wallet binary lives in /opt/acme/bin and is owned by root.\n\n## Remembered by nexo\n- 2026-11-11: {a}\n- 2026-11-11: tests run with `make test`\n\n## Other\n- deploys always go through the ship script on fridays\n"
        );
        assert_eq!(near_rule(&text, b), Some(format!("2026-11-11: {a}")));
        // a short rule the section holds whole
        assert_eq!(near_rule(&text, "Tests run with make").as_deref(), Some("2026-11-11: tests run with `make test`"));
        // another fact, a rule too short to judge, and lines outside nexo's own section
        assert_eq!(near_rule(&text, "the relay keeps events for seven days"), None);
        assert_eq!(near_rule(&text, "run make"), None);
        assert_eq!(near_rule(&text, "the wallet binary lives in /opt/acme/bin, owned by root"), None);
        assert_eq!(near_rule(&text, "deploys always go through the ship script"), None);
        assert_eq!(near_rule("", b), None);
        // two facts that share their joining words and one noun are two facts; the same one in more words is not
        let ports = "## Remembered by nexo\n- 2026-11-11: the production database is on port 5432\n";
        assert_eq!(near_rule(ports, "the staging database is on port 5433"), None);
        assert_eq!(
            near_rule(ports, "production database listens on port 5432 (not the default)").as_deref(),
            Some("2026-11-11: the production database is on port 5432")
        );
    }

    #[test]
    fn a_saved_line_is_corrected_in_place_and_nothing_else_in_the_file_changes() {
        const D: &str = "2026-11-12";
        let text = "# Project\r\n\r\n- the staging database is on port 5433\r\n\r\n## Remembered by nexo\r\n- 2026-11-01: tests run with `make test`\r\n* 2026-11-02: the staging database is on port 5433 of db-stage\r\n- 2026-11-03: never touch vendor/\r\n\r\n## Other\r\n- 2026-11-02: the staging database is on port 5433 of db-stage\r\n";
        let fixed = "the staging database is on port 5434 of db-stage";
        let want = text.replacen(
            "* 2026-11-02: the staging database is on port 5433 of db-stage",
            "- 2026-11-12: the staging database is on port 5434 of db-stage",
            1,
        );
        let was = "2026-11-02: the staging database is on port 5433 of db-stage".to_string();
        // named by its words, with or without the date and the marker, or nearly
        for old in [
            "the staging database is on port 5433 of db-stage",
            "- 2026-11-02: The staging database is on port 5433 of db-stage.",
            "staging database, port 5433 on db-stage",
        ] {
            assert_eq!(with_replaced(text, old, fixed, D), Some((want.clone(), was.clone())), "{old}");
        }
        // no such line in nexo's section, nothing to name, the same words outside any such section, and a rule
        // the file already has (the same line takes another rule)
        let old = "the staging database is on port 5433 of db-stage";
        assert_eq!(with_replaced(text, "the relay keeps events for seven days", fixed, D), None);
        assert_eq!(with_replaced(text, "", fixed, D), None);
        assert_eq!(
            with_replaced("# P\n- 2026-11-02: the staging database is on port 5433 of db-stage\n", old, fixed, D),
            None
        );
        assert!(with_replaced(text, "tests run with make test", "tests run with make check", D).is_some());
        assert_eq!(with_replaced(text, "tests run with make test", "Never touch vendor", D), None);
        // a line the person wrote in the section without a list marker is theirs; a vague quote names no line,
        // and neither does one that fits two
        let mixed = "## Remembered by nexo\nBackups: nightly at 03:00 to the big disk\n- 2026-11-01: the staging database is on port 5433 of db-stage, user shop\n- 2026-11-02: the staging replica is on port 5434 of db-stage-ro, user shop\n";
        assert_eq!(with_replaced(mixed, "Backups: nightly at 03:00 to the big disk", fixed, D), None);
        assert_eq!(with_replaced(mixed, "staging database port", fixed, D), None);
        assert_eq!(with_replaced(mixed, "staging on port of db-stage, user shop", fixed, D), None);
        let (now, was) =
            with_replaced(mixed, "the staging database is on port 5433 of db-stage (user shop)", fixed, D).unwrap();
        assert_eq!(was, "2026-11-01: the staging database is on port 5433 of db-stage, user shop");
        assert_eq!(
            now,
            mixed.replace(
                "- 2026-11-01: the staging database is on port 5433 of db-stage, user shop",
                "- 2026-11-12: the staging database is on port 5434 of db-stage"
            )
        );
        // the last line of a file without a final line end
        assert_eq!(
            with_replaced("## Remembered by nexo\n- 2026-11-01: port is 5433", "port is 5433", "port is 5434", D),
            Some((
                "## Remembered by nexo\n- 2026-11-12: port is 5434".to_string(),
                "2026-11-01: port is 5433".to_string()
            ))
        );
        // on disk: the file changes only when a line is named, and a missing file is not made
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("AGENTS.md");
        assert_eq!(replace_rule(&f, "port is 5433", "port is 5434").unwrap(), Replaced::NoLine);
        assert!(!f.exists());
        std::fs::write(&f, "## Remembered by nexo\n- 2026-11-01: port is 5433\n").unwrap();
        assert_eq!(replace_rule(&f, "something else entirely", "port is 5434").unwrap(), Replaced::NoLine);
        assert_eq!(replace_rule(&f, "something else entirely", "Port is 5433.").unwrap(), Replaced::There);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "## Remembered by nexo\n- 2026-11-01: port is 5433\n");
        assert_eq!(
            replace_rule(&f, "port is 5433", "port is 5434").unwrap(),
            Replaced::Done("2026-11-01: port is 5433".to_string())
        );
        let now = std::fs::read_to_string(&f).unwrap();
        assert!(
            now.starts_with("## Remembered by nexo\n- ") && now.ends_with(": port is 5434\n") && !now.contains("5433"),
            "{now}"
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
        assert_eq!(save_rule(&f, seed.as_deref(), "Answer in English.", false).unwrap(), Saved::New);
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with(crate::paths::PREFS_TEMPLATE), "{text}");
        assert!(text.contains("## Remembered by nexo\n- "), "{text}");
        // from a sub-folder the project file is at the repository root, created when missing
        let (p, _) = rules_file("project", &proj.join("src/deep"), &cfg, None).unwrap();
        assert_eq!(p, proj.join("AGENTS.md"));
        assert_eq!(save_rule(&p, None, "Never edit vendor/.", false).unwrap(), Saved::New);
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
