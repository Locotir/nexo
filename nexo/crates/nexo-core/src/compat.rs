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
mod tests;

#[cfg(test)]
mod rule_tests;
