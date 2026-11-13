//! The system prompt: who the agent is, where it works, what its tools are
//! for, and the project's own instructions (`NEXO.md`, `AGENTS.md`, `CLAUDE.md`).

use std::path::{Path, PathBuf};

use nexo_core::compat;
use nexo_core::config::AgentConfig;
use nexo_core::paths;
use nexo_core::tool::ToolSet;

#[cfg(test)]
mod size_budget;
#[cfg(test)]
mod tests;

/// The built-in prompt, kept as plain text in `prompt.md` next to this file so it can be edited as such.
const BUILT_IN: &str = include_str!("prompt.md");

/// The short built-in text, for a model with a small window.
const BUILT_IN_COMPACT: &str = include_str!("prompt_compact.md");

/// A model whose context window is at most this many tokens gets the compact prompt and compact tool text.
pub const COMPACT_WINDOW: u64 = 32_768;

/// Whether a model with context window `window` (tokens; `None` when unknown) gets the compact text.
pub fn is_compact(window: Option<u64>) -> bool {
    window.is_some_and(|w| w <= COMPACT_WINDOW)
}

/// A model whose window is at most this many tokens is not offered the tools in [`TINY_LEAVES_OUT`] either.
pub const TINY_WINDOW: u64 = 16_384;

/// The tools a window of [`COMPACT_WINDOW`] tokens or less is not offered: each costs a description in every
/// request and starts work (a second context, a list to keep, notes) that there is no room for.
const SMALL_LEAVES_OUT: [&str; 8] = ["task", "workflow", "todo", "note", "debug", "bin", "diagnostics", "browser"];

/// The tools a window of [`TINY_WINDOW`] tokens or less is not offered besides those: skills and the web.
const TINY_LEAVES_OUT: [&str; 3] = ["skill", "web_fetch", "web_search"];

/// Whether the tool `name` is left out for a model with context window `window` (`None`: unknown, nothing is).
pub fn left_out(window: Option<u64>, name: &str) -> bool {
    window.is_some_and(|w| {
        (w <= COMPACT_WINDOW && SMALL_LEAVES_OUT.contains(&name))
            || (w <= TINY_WINDOW && TINY_LEAVES_OUT.contains(&name))
    })
}

/// A prompt file whose first line is this is sent as it is: no tool guidance,
/// directory, date, project instructions or extra text appended.
pub const RAW_MARK: &str = "<!-- nexo: raw -->";

/// Said only with `agent.close_on_verified` on: without it the turn does not end on that reply, so the model
/// would write its answer twice.
const CLOSE_ON_VERIFIED: &str = "- When your last calls only confirm finished work (a test run, a check), put the answer for the person in that same message: if everything passes the turn ends there; if something fails you get the result and go on.";

/// The built-in text for a model with context window `window`, with the line for `agent.close_on_verified`
/// when that setting is on (the compact text does not have it).
fn base_for(window: Option<u64>, close_on_verified: bool) -> String {
    let mut out = built_in_for(window).to_string();
    if close_on_verified && !is_compact(window) {
        out.push('\n');
        out.push_str(CLOSE_ON_VERIFIED);
    }
    out
}

/// The built-in text: what every request starts with.
pub fn built_in() -> &'static str {
    BUILT_IN.trim()
}

/// The built-in text for a model with context window `window`.
fn built_in_for(window: Option<u64>) -> &'static str {
    if is_compact(window) {
        BUILT_IN_COMPACT.trim()
    } else {
        built_in()
    }
}

pub use nexo_core::paths::PREFS_TEMPLATE;

/// Whether a prompt file is a whole prompt of its own rather than preferences: one set in the
/// config, or one that starts like the built-in text (the copies earlier versions made).
fn replaces(cfg: &AgentConfig, text: &str) -> bool {
    let opening = built_in().split(',').next().unwrap_or_default();
    cfg.system_prompt_file.is_some() || text.starts_with(opening)
}

/// `text` without its `<!-- ... -->` comments.
fn without_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        rest = rest[i..].find("-->").map_or("", |j| &rest[i + j + 3..]);
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The file the base text comes from: `agent.system_prompt_file` when set,
/// else `~/.nexo/prompt.md` when it exists.
pub fn source_file(cfg: &AgentConfig) -> Option<PathBuf> {
    if let Some(f) = &cfg.system_prompt_file {
        return Some(f.clone());
    }
    if cfg.hermetic {
        return None;
    }
    let f = paths::prompt_file();
    f.is_file().then_some(f)
}

/// Whether the prompt file is sent exactly as written (it starts with [`RAW_MARK`]).
pub fn is_raw(cfg: &AgentConfig) -> bool {
    source_file(cfg).and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|t| t.trim_start().starts_with(RAW_MARK))
}

/// `system` with a context policy's directives at its end; a raw prompt stays as written.
pub fn with_directives(system: String, cfg: &AgentConfig, directives: Option<&str>) -> String {
    match directives {
        Some(d) if !is_raw(cfg) => format!("{system}\n\n{d}"),
        _ => system,
    }
}

/// Only what nexo itself writes of the prompt (`nexo prompt --builtin`): the built-in text, the guidance for
/// `tools` and the context policy's directives. No directory, date, project, instruction file or preference.
pub fn builtin_part(tools: &ToolSet, directives: Option<&str>) -> String {
    builtin_part_for(tools, directives, None, false)
}

/// [`builtin_part`] for a model with context window `window`, with `agent.close_on_verified` as given.
pub fn builtin_part_for(
    tools: &ToolSet,
    directives: Option<&str>,
    window: Option<u64>,
    close_on_verified: bool,
) -> String {
    let mut out = base_for(window, close_on_verified);
    out.push_str(&guidance_for(tools, window));
    if let Some(d) = directives {
        out.push_str(&format!("\n\n{d}"));
    }
    out
}

/// The tools as the model gets them (`nexo prompt --tools`): per tool a `=== name ===` line, its description
/// as it is, and its parameters as the JSON schema it is sent with.
pub fn tool_definitions(specs: &[nexo_core::provider::ToolSpec]) -> String {
    tool_definitions_for(specs, None)
}

/// [`tool_definitions`] for a model with context window `window`: the compact specs (`ToolSet::compact_specs`)
/// have their parameters on one line and no labels.
pub fn tool_definitions_for(specs: &[nexo_core::provider::ToolSpec], window: Option<u64>) -> String {
    let mut out = String::new();
    for s in specs {
        let params = if is_compact(window) {
            serde_json::to_string(&s.parameters)
        } else {
            serde_json::to_string_pretty(&s.parameters)
        }
        .unwrap_or_default();
        if is_compact(window) {
            out.push_str(&format!("=== {} ===\n{}\n{params}\n", s.name, s.description));
        } else {
            out.push_str(&format!("=== {} ===\ndescription:\n{}\nparameters:\n{params}\n\n", s.name, s.description));
        }
    }
    out.trim_end().to_string()
}

/// The guidance for the tools present, as the model with context window `window` gets it.
fn guidance_for(tools: &ToolSet, window: Option<u64>) -> String {
    if is_compact(window) {
        compact_guidance(tools)
    } else {
        tool_guidance(tools)
    }
}

/// Two short lines at most, for the tools present.
fn compact_guidance(tools: &ToolSet) -> String {
    let has = |n: &str| tools.get(n).is_some();
    let mut lines = Vec::new();
    if has("ls") || has("bash") {
        lines.push("- The Project block, with the project note a request may start with, is current: do not `ls`. A file the request names is attached.");
    }
    if has("bash") {
        lines.push("- Slow jobs: `background: true`.");
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\n\nTools:\n{}", lines.join("\n"))
}

/// The guidance for the tools present, by name.
fn tool_guidance(tools: &ToolSet) -> String {
    let mut lines = Vec::new();
    let has = |n: &str| tools.get(n).is_some();
    if has("ls") || has("bash") {
        lines.push("- The Project block, with the project note a request may start with, is current: use them, not `ls` or `find`.");
    }
    let mcp: Vec<&str> = tools.names().filter(|n| n.starts_with("mcp__")).collect();
    if !mcp.is_empty() {
        lines.push(
            "- Tools named `mcp__<server>__<tool>` come from connected servers; their descriptions say what they do.",
        );
    }
    let mut out = abilities(tools);
    if !lines.is_empty() {
        out.push_str(&format!("\n\nTools:\n{}", lines.join("\n")));
    }
    out
}

/// "You can do:" the abilities of the harness that save time, one line each, for the tools present only.
fn abilities(tools: &ToolSet) -> String {
    let has = |n: &str| tools.get(n).is_some();
    let looks = ["read", "grep", "glob", "ls", "bash", "web_search", "web_fetch"].iter().any(|n| has(n));
    let mut lines = Vec::new();
    if looks {
        lines.push("- Think in independent strands: independent calls go in ONE message and run at the same time (reads, searches, web, read-only and check commands). Steps that follow each other also go in ONE message when you do not need to see one result to write the next call (write a script and run it; edit, then run the check): they run one at a time in the order given, and when one fails the ones after it are not run. Never run in parallel what touches the same state.");
    }
    if has("bash") {
        lines.push("- Slow jobs (builds, tests, servers, downloads) use `background: true`; keep working, then `wait_until` a condition, not a fixed sleep.");
    }
    if has("edit") || has("write") {
        lines.push("- `edit` and `write` change many files in one call with its check result.");
    }
    if has("task") {
        lines.push("- `task` sub-agents take side work in parallel: several searches or checks (one call each, all in ONE message), a long job (`background: true`), anything the person asks to hand off. Not for one or two reads.");
    }
    if has("note") {
        lines.push("- `note`: a lasting preference, rule or fact the person states (\"always ...\", \"my .bashrc is at ...\"), or one you find that matters again (a non-default path or port, something not to touch), is saved as a `Rule: ...` line (one short line: the exact fact (path, value, name, command) and the rule, no filler) with `scope` `global` (all projects) or `project` (this repo). Not one-off details, guesses, secrets or what is saved. Say nothing about saving it.");
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\n\nYou can do:\n{}", lines.join("\n"))
}

/// The machine and the project: the system (with the Linux distribution, which says apt, apk or
/// dnf), whether `cwd` is in a git repository and the project map. A session builds this once (see
/// [`Volatile`]) and sends it unchanged for as long as it lives, so the cached prompt prefix stays.
fn environment(cwd: &Path) -> String {
    let distro = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| os_name(&t));
    let system = match distro {
        Some(d) => format!("{} ({d}), {}", std::env::consts::OS, std::env::consts::ARCH),
        None => format!("{}, {}", std::env::consts::OS, std::env::consts::ARCH),
    };
    format!("\nSystem: {system}\nGit: {}{}", git_line(cwd), project_block(cwd))
}

/// The most characters the Project block takes.
const PROJECT_CAP: usize = 2500;
/// The most tree entries the Project block lists.
const PROJECT_ENTRIES: usize = 60;

/// A bounded map of the project at `cwd`: its tree two levels down (directories with their file
/// counts, files in `.gitignore` left out), the manifests with their names, and the command that
/// checks it. Empty when the folder holds nothing worth listing.
fn project_block(cwd: &Path) -> String {
    project_block_with(cwd, &crate::missing::session_line(cwd))
}

/// [`project_block`] with `missing` (the "Not installed here" line, or empty) given.
fn project_block_with(cwd: &Path, missing: &str) -> String {
    project_block_head(cwd, missing, PROJECT_HEAD)
}

/// The head of the Project block in the system text.
const PROJECT_HEAD: &str = "Project (as when the session began; changes come in a note; use it, not ls or find)";

/// The project map alone, as the note at the start of a request shows it when it changed.
pub fn project_tree(cwd: &Path) -> String {
    // the check command is a line of the request note already, so the tree note leaves it out
    let block = project_block_head(cwd, "", "Project (changed since you last saw it)");
    block.trim_start().lines().filter(|l| !l.starts_with("check: ")).collect::<Vec<_>>().join("\n")
}

fn project_block_head(cwd: &Path, missing: &str, head: &str) -> String {
    use std::collections::BTreeMap;
    // the person's home is no project: its map would change with every file anything writes under it,
    // and each change would come back as a note in the middle of the session
    if compat::no_project(cwd) {
        return String::new();
    }
    let mut top: BTreeMap<String, (bool, usize)> = BTreeMap::new();
    let mut second: BTreeMap<String, (bool, usize)> = BTreeMap::new();
    let mut manifests = Vec::new();
    let walk = ignore::WalkBuilder::new(cwd).require_git(false).build();
    for e in walk.flatten().take(20_000) {
        let Ok(rel) = e.path().strip_prefix(cwd) else { continue };
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        if parts.is_empty() {
            continue;
        }
        let is_dir = e.file_type().is_some_and(|t| t.is_dir());
        if !is_dir && parts.len() <= 2 && MANIFESTS.contains(&parts[parts.len() - 1].as_str()) {
            manifests.push(manifest_line(&parts.join("/"), e.path()));
        }
        let files = usize::from(!is_dir);
        let t = top.entry(parts[0].clone()).or_insert((parts.len() > 1 || is_dir, 0));
        t.1 += files;
        if parts.len() >= 2 {
            let s = second.entry(format!("{}/{}", parts[0], parts[1])).or_insert((parts.len() > 2 || is_dir, 0));
            s.1 += files;
        }
    }
    let mut lines = Vec::new();
    let mut entries = 0;
    'tree: for (name, (dir, n)) in &top {
        if entries >= PROJECT_ENTRIES {
            break;
        }
        lines.push(if *dir { format!("- {name}/ ({n} files)") } else { format!("- {name}") });
        entries += 1;
        if !*dir {
            continue;
        }
        let prefix = format!("{name}/");
        for (sub, (sdir, sn)) in second.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)) {
            if entries >= PROJECT_ENTRIES {
                break 'tree;
            }
            lines.push(if *sdir { format!("  - {sub}/ ({sn} files)") } else { format!("  - {sub}") });
            entries += 1;
        }
    }
    let total = top.len() + second.len();
    if total > entries {
        lines.push(format!("- ... {} more entries", total - entries));
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut out = format!("\n{head}:\n{}", lines.join("\n"));
    manifests.sort();
    if !manifests.is_empty() {
        out.push_str(&format!("\nManifests: {}", manifests.join(", ")));
    }
    if let Some(check) = nexo_tools::project::check_command(cwd) {
        out.push_str(&format!("\ncheck: {}", check.command));
    }
    let room = PROJECT_CAP - if missing.is_empty() { 0 } else { missing.len() + 1 };
    if out.len() > room {
        let mut cut = room;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    if !missing.is_empty() {
        out.push('\n');
        out.push_str(missing);
    }
    out
}

const MANIFESTS: [&str; 5] = ["Cargo.toml", "package.json", "pyproject.toml", "go.mod", "setup.cfg"];

/// `Cargo.toml (name)`: the manifest's path with the project name it declares, when it has one.
fn manifest_line(shown: &str, path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let name = if shown.ends_with("package.json") {
        serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["name"].as_str().map(String::from))
    } else if shown.ends_with("go.mod") {
        text.lines().find_map(|l| l.strip_prefix("module ")).map(|m| m.trim().to_string())
    } else {
        text.lines().find_map(|l| {
            let v = l.trim().strip_prefix("name")?.trim_start().strip_prefix('=')?;
            Some(v.trim().trim_matches('"').trim_matches('\'').to_string())
        })
    };
    match name {
        Some(n) if !n.is_empty() => format!("{shown} ({n})"),
        _ => shown.to_string(),
    }
}

/// `PRETTY_NAME` of an os-release file, else `NAME VERSION_ID`.
fn os_name(text: &str) -> Option<String> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').trim().to_string())
            .filter(|v| !v.is_empty())
    };
    field("PRETTY_NAME").or_else(|| {
        let name = field("NAME")?;
        Some(match field("VERSION_ID") {
            Some(v) => format!("{name} {v}"),
            None => name,
        })
    })
}

fn git_line(cwd: &Path) -> String {
    // the project root, so a home folder with a `.git` is no repository the model works in
    let root = compat::project_root(cwd);
    match root.join(".git").exists() {
        true if root == cwd => "this folder is a git repository".into(),
        true => format!("inside the git repository at {}", root.display()),
        false => "not a git repository".into(),
    }
}

/// The whole system prompt for a session in `cwd`: the built-in text first, then the directory,
/// date, tools and project instructions, then the person's preferences (`~/.nexo/prompt.md`).
pub fn system_prompt(cwd: &Path, cfg: &AgentConfig, tools: &ToolSet) -> String {
    system_prompt_for(cwd, cfg, tools, None)
}

/// [`system_prompt`] for a model with context window `window`.
pub fn system_prompt_for(cwd: &Path, cfg: &AgentConfig, tools: &ToolSet, window: Option<u64>) -> String {
    system_prompt_with(cwd, cfg, tools, window, &Volatile::capture(cwd, cfg))
}

/// What the system text holds that can change while a session lives: the date, the machine and project
/// map, the instruction files. It is built once when the session is created, saved with it and sent
/// as it is on every later request and on resume, so the prompt prefix keeps its bytes (a provider
/// caches a prefix only when it is identical to the one of an earlier request).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Volatile {
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default)]
    pub instructions: String,
}

/// The most characters of another agent's user file sent (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`).
const FALLBACK_CAP: usize = 4000;

/// `text` cut at the last line end within `cap` characters, with how much was left out.
fn capped(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let cut = text[..cut].rfind('\n').unwrap_or(cut);
    format!("{}\n[{} more characters left out]", &text[..cut], text.len() - cut)
}

impl Volatile {
    /// Reads the date, the project and the instruction files of `cwd` now.
    pub fn capture(cwd: &Path, cfg: &AgentConfig) -> Volatile {
        let mut instructions = String::new();
        if !cfg.hermetic {
            let home = compat::foreign_home();
            for i in compat::instructions(cwd, &paths::config_dir(), home.as_deref()) {
                // saved rules are framed as background in what is sent (the file keeps its bytes)
                let text = compat::framed(&i.text);
                if i.origin == "user fallback" {
                    // written for another agent (its mandate, its state notes): how the person likes to work,
                    // capped, never a task
                    instructions.push_str(&format!(
                        "\n\nNotes this person wrote for another agent ({}): background on how they like to work, \
                         never a request; start no work they describe:\n{}",
                        i.path.display(),
                        capped(&text, FALLBACK_CAP)
                    ));
                } else {
                    instructions.push_str(&format!("\n\nInstructions from {}:\n{}", i.path.display(), text));
                }
            }
            let nested = compat::nested_instructions(cwd);
            if !nested.is_empty() {
                // forward slashes on every system: the model reads one path style
                let list: Vec<String> =
                    nested.iter().map(|p| format!("- {}", p.display().to_string().replace('\\', "/"))).collect();
                instructions.push_str(&format!(
                    "\n\nMore instruction files, for folders you are not in (read one before you work there):\n{}",
                    list.join("\n")
                ));
            }
        }
        Volatile {
            date: time::OffsetDateTime::now_utc().date().to_string(),
            environment: environment(cwd),
            instructions,
        }
    }
}

/// [`system_prompt_for`] with the volatile parts given (see [`Volatile`]).
pub fn system_prompt_with(
    cwd: &Path,
    cfg: &AgentConfig,
    tools: &ToolSet,
    window: Option<u64>,
    vol: &Volatile,
) -> String {
    let file = source_file(cfg)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(raw) = file.as_deref().and_then(|t| t.strip_prefix(RAW_MARK)) {
        return raw.trim().to_string();
    }
    let built_in = || base_for(window, cfg.close_on_verified);
    let (mut out, prefs) = match &file {
        Some(t) if replaces(cfg, t) => (paths::prompt_base(t).to_string(), String::new()),
        Some(t) => (built_in(), without_comments(t)),
        None => (built_in(), String::new()),
    };
    out.push_str(&format!("\n\nWorking directory: {}\nDate: {}", cwd.display(), vol.date));
    out.push_str(&vol.environment);
    out.push_str(&guidance_for(tools, window));
    out.push_str(&vol.instructions);
    if !prefs.is_empty() {
        out.push_str(&format!("\n\nThe person's preferences:\n{}", compat::framed(&prefs)));
    }
    if let Some(extra) = &cfg.system_prompt_extra {
        if !extra.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(extra.trim());
        }
    }
    out
}
