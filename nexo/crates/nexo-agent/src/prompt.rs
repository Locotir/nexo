//! The system prompt: who the agent is, where it works, what its tools are
//! for, and the project's own instructions (`NEXO.md`, `AGENTS.md`, `CLAUDE.md`).

use std::path::{Path, PathBuf};

use nexo_core::compat;
use nexo_core::config::AgentConfig;
use nexo_core::paths;
use nexo_core::tool::ToolSet;

/// The built-in prompt, kept as plain text in `prompt.md` next to this file so it can be edited as such.
const BUILT_IN: &str = include_str!("prompt.md");

/// The short built-in text, for a model with a small window.
const BUILT_IN_COMPACT: &str = include_str!("prompt_compact.md");

/// A model whose context window is at most this many tokens gets the compact prompt and compact tool text.
pub const COMPACT_WINDOW: u64 = 32_000;

/// Whether a model with context window `window` (tokens; `None` when unknown) gets the compact text.
pub fn is_compact(window: Option<u64>) -> bool {
    window.is_some_and(|w| w <= COMPACT_WINDOW)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The prompt reads `prompt.md` from `NEXO_CONFIG_DIR`, one variable for the whole process:
    /// the tests that point it somewhere take turns, so one never sees another's file.
    static CONFIG_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn environment_lists_the_tree_the_manifests_and_the_check_command() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "").unwrap();
        let e = environment(d.path());
        assert!(e.contains("- src/ (1 files)"), "{e}");
        assert!(e.contains("  - src/lib.rs"), "{e}");
        assert!(e.contains("Manifests: Cargo.toml (demo)"), "{e}");
        assert!(e.contains("\ncheck: cargo test -q"), "{e}");
    }

    #[test]
    fn the_missing_commands_line_ends_the_block_and_nothing_missing_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "").unwrap();
        let plain = project_block_with(d.path(), "");
        assert!(!plain.contains("Not installed"), "{plain}");
        let with = project_block_with(d.path(), "Not installed here: curl, file, xxd.");
        assert_eq!(with, format!("{plain}\nNot installed here: curl, file, xxd."));
        assert_eq!(project_block_with(d.path(), ""), plain);
    }

    #[test]
    fn the_line_fits_inside_the_cap() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..500 {
            std::fs::write(d.path().join(format!("file_with_a_long_name_{i}.txt")), "").unwrap();
        }
        let b = project_block_with(d.path(), "Not installed here: curl, file, xxd.");
        assert!(b.len() <= PROJECT_CAP && b.ends_with("xxd."), "{}", b.len());
    }

    #[test]
    fn environment_block_stays_under_its_cap() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("data")).unwrap();
        for i in 0..500 {
            std::fs::write(d.path().join(format!("file_with_a_long_name_{i}.txt")), "").unwrap();
            std::fs::write(d.path().join(format!("data/another_long_file_name_{i}.txt")), "").unwrap();
        }
        let b = project_block(d.path());
        assert!(b.len() <= PROJECT_CAP, "{}", b.len());
        assert!(b.matches("\n- ").count() + b.matches("\n  - ").count() <= PROJECT_ENTRIES + 1, "{b}");
        assert!(b.contains("more entries"), "{b}");
    }

    #[test]
    fn names_the_request_gives_are_kept() {
        // configure-git-webserver: asked for user@server, the model pushed as the account it found
        assert!(built_in().contains("use them exactly. When one does not exist yet, create it"));
    }

    #[test]
    fn the_strands_line_says_steps_in_order_go_in_one_message_and_a_failure_stops_the_rest() {
        let all = tool_guidance(&nexo_tools::builtin());
        for want in [
            "Steps that follow each other also go in ONE message",
            "write a script and run it; edit, then run the check",
            "they run one at a time in the order given, and when one fails the ones after it are not run",
        ] {
            assert!(all.contains(want), "missing {want}: {all}");
        }
        assert!(!all.contains("a change runs alone"), "{all}");
        assert!(BUILT_IN_COMPACT.contains("a failure stops the rest"));
    }

    #[test]
    fn the_you_can_do_section_lists_only_enabled_abilities() {
        let all = tool_guidance(&nexo_tools::builtin());
        for want in [
            "You can do:",
            "independent strands",
            "ONE message",
            "run at the same time",
            "`wait_until`",
            "touches the same state",
            "many files",
            "`note`: a lasting preference",
        ] {
            assert!(all.contains(want), "missing {want}: {all}");
        }
        struct Named(&'static str);
        #[async_trait::async_trait]
        impl nexo_core::tool::Tool for Named {
            fn name(&self) -> &str {
                self.0
            }
            fn spec(&self) -> nexo_core::provider::ToolSpec {
                nexo_core::provider::ToolSpec {
                    name: self.0.into(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                }
            }
            fn kind(&self) -> nexo_core::tool::ToolKind {
                nexo_core::tool::ToolKind::Read
            }
            fn summary(&self, _: &serde_json::Value) -> String {
                String::new()
            }
            async fn run(&self, _: serde_json::Value, _: &nexo_core::tool::ToolContext) -> nexo_core::tool::ToolOutput {
                nexo_core::tool::ToolOutput::ok("")
            }
        }
        let with_task = tool_guidance(&nexo_tools::builtin().with(std::sync::Arc::new(Named("task"))));
        assert!(with_task.contains("`task` sub-agents"), "{with_task}");
        let mut few = ToolSet::new();
        for n in ["read", "bash"] {
            few = few.with(nexo_tools::builtin().get(n).unwrap().clone());
        }
        let g = tool_guidance(&few);
        assert!(g.contains("independent strands") && g.contains("`background: true`"), "{g}");
        assert!(
            !g.contains("`edit`") && !g.contains("`task`") && !g.contains("`note`") && !g.contains("`recall`"),
            "{g}"
        );
        assert_eq!(tool_guidance(&ToolSet::new()), "");
    }

    #[test]
    fn the_distribution_comes_from_os_release() {
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\n";
        assert_eq!(os_name(debian).as_deref(), Some("Debian GNU/Linux 12 (bookworm)"));
        assert_eq!(os_name("NAME=Alpine Linux\nVERSION_ID=3.20.3\n").as_deref(), Some("Alpine Linux 3.20.3"));
        assert_eq!(os_name("PRETTY_NAME=\"\"\nNAME=Arch\n").as_deref(), Some("Arch"));
        assert_eq!(os_name("ID=x\n"), None);
        let none = tempfile::tempdir().unwrap();
        assert_eq!(git_line(none.path()), "not a git repository");
    }

    #[test]
    fn another_agents_user_file_is_background_capped_and_never_the_request() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let (cfg, home, d) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::env::set_var("NEXO_CONFIG_DIR", cfg.path());
        std::env::set_var("NEXO_COMPAT_HOME", home.path());
        let line = "Act for the boss: keep going until it is done, unasked.\n";
        let file = home.path().join(".claude/CLAUDE.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, line.repeat(200)).unwrap();
        let p = system_prompt(d.path(), &AgentConfig::default(), &ToolSet::new());
        std::env::remove_var("NEXO_COMPAT_HOME");
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert!(
            p.contains(&format!("Notes this person wrote for another agent ({}): background", file.display())),
            "{p}"
        );
        assert!(!p.contains(&format!("Instructions from {}", file.display())), "{p}");
        let kept = p.matches(line.trim_end()).count();
        assert!(kept > 0 && kept * line.len() <= FALLBACK_CAP, "{kept}");
        assert!(p.contains(&format!("[{} more characters left out]", line.len() * (200 - kept))), "{p}");
        assert!(p.contains("they are never the request"), "{p}");
    }

    #[test]
    fn carries_directory_instructions_and_extra() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("NEXO.md"), "Use tabs.").unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cfg = AgentConfig { system_prompt_extra: Some("Answer in Spanish.".into()), ..Default::default() };
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(p.starts_with("You are nexo"));
        assert!(p.contains(&format!("Working directory: {}", d.path().display())));
        assert!(p.contains(&format!("\nSystem: {}", std::env::consts::OS)), "{p}");
        assert!(p.contains("\nGit: this folder is a git repository"), "{p}");
        let sub = d.path().join("src");
        std::fs::create_dir(&sub).unwrap();
        let inner = system_prompt(&sub, &AgentConfig::default(), &ToolSet::new());
        assert!(inner.contains(&format!("Git: inside the git repository at {}", d.path().display())), "{inner}");
        assert!(p.contains("Use tabs."));
        assert!(p.ends_with("Answer in Spanish."));
        let f = d.path().join("sys.txt");
        std::fs::write(&f, "Custom.").unwrap();
        let cfg = AgentConfig { system_prompt_file: Some(f), ..Default::default() };
        assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("Custom."));
        std::env::remove_var("NEXO_CONFIG_DIR");
    }

    #[test]
    fn a_hermetic_prompt_leaves_out_the_machines_instructions_and_preferences() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        std::fs::write(home.path().join("prompt.md"), "Reply in Spanish.\n").unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "Private notes.\n").unwrap();
        let mut cfg = AgentConfig::default();
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(p.contains("Private notes.") && p.contains("Reply in Spanish."), "{p}");
        cfg.hermetic = true;
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert!(p.starts_with(built_in()), "{p}");
        assert!(
            !p.contains("Private notes.") && !p.contains("Reply in Spanish.") && !p.contains("Instructions from"),
            "{p}"
        );
    }

    #[test]
    fn the_persons_prompt_file_adds_preferences_and_a_whole_copy_replaces_the_text() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        let cfg = AgentConfig::default();
        assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("You are nexo"));
        // preferences: the built-in text stays first, they come last, the comments are not sent
        std::fs::write(home.path().join("prompt.md"), format!("{PREFS_TEMPLATE}Mine.\n<!-- note -->Short answers.\n"))
            .unwrap();
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(p.starts_with(built_in()), "{p}");
        assert!(p.ends_with("The person's preferences:\nMine.\nShort answers."), "{p}");
        assert!(!p.contains("<!--"), "{p}");
        // only the template: nothing is added
        std::fs::write(home.path().join("prompt.md"), PREFS_TEMPLATE).unwrap();
        assert!(!system_prompt(d.path(), &cfg, &ToolSet::new()).contains("preferences:"));
        // a whole prompt saved by an earlier version replaces the built-in text, without its stale
        // directory, date and tools
        std::fs::write(
            home.path().join("prompt.md"),
            "You are nexo, mine.\n\nWorking directory: /old\nDate: 2020-01-01\n\nTools:\n- `read`: x\n",
        )
        .unwrap();
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(
            p.starts_with("You are nexo, mine.\n\nWorking directory:")
                && !p.contains("/old")
                && !p.contains("2020-01-01"),
            "{p}"
        );
        std::fs::write(home.path().join("prompt.md"), format!("{RAW_MARK}\nOnly this.\n")).unwrap();
        assert_eq!(system_prompt(d.path(), &cfg, &ToolSet::new()), "Only this.");
        let f = d.path().join("sys.txt");
        std::fs::write(&f, "Configured.").unwrap();
        let cfg = AgentConfig { system_prompt_file: Some(f), ..Default::default() };
        assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("Configured.\n\nWorking"));
        std::env::remove_var("NEXO_CONFIG_DIR");
    }

    #[test]
    fn the_prompt_tells_the_model_what_to_remember_and_where_only_when_it_has_note() {
        let all = tool_guidance(&nexo_tools::builtin());
        for want in [
            "lasting preference, rule or fact",
            "\"always ...\"",
            "`global`",
            "`project`",
            "one-off details",
            "secrets",
            "one short line",
            "the exact fact (path, value, name, command) and the rule, no filler",
            // saving is silent: the screen shows it, and a line of reply about it is what the person does not want
            "or what is saved.",
            "Say nothing about saving it.",
        ] {
            assert!(all.contains(want), "missing {want}: {all}");
        }
        assert!(!tool_guidance(&ToolSet::new()).contains("lasting preference, rule or fact"));
        assert!(!all.contains("Tell the person what you saved"), "{all}");
    }

    /// What the model saves with `note` is in the prompt of the next session (and the current one keeps its prompt, so
    /// the cached prefix stays the same: the call itself stays in the context for it).
    #[test]
    fn a_saved_rule_is_in_the_prompt_of_the_next_session_and_not_of_this_one() {
        use nexo_core::tool::{Tool, ToolContext};
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let (cwd, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir(cwd.path().join(".git")).unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        let cfg = AgentConfig::default();
        let tools = nexo_tools::builtin();
        let before = system_prompt(cwd.path(), &cfg, &tools);
        let cx = ToolContext::new(cwd.path().to_path_buf());
        let note = nexo_tools::note::Note;
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let run = |args: serde_json::Value| rt.block_on(note.run(args, &cx));
        let g = run(serde_json::json!({"text": "Rule: my .bashrc is at ~/dotfiles/bashrc", "scope": "global"}));
        let p = run(serde_json::json!({"text": "Rule: the dev server runs on port 4100", "scope": "project"}));
        let after = system_prompt(cwd.path(), &cfg, &tools);
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert!(!g.is_error && !p.is_error, "{} / {}", g.content, p.content);
        assert!(!before.contains("dotfiles") && !before.contains("4100"));
        // the model is told, right under each heading, that this is background and not the person speaking now
        let f = compat::REMEMBERED_FRAME;
        assert!(after.contains(&format!("The person's preferences:\n## Remembered by nexo\n{f}\n- ")), "{after}");
        assert_eq!(after.matches(f).count(), 2, "{after}");
        // and neither file holds that line
        let agents = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("## Remembered by nexo\n- ") && !agents.contains(f), "{agents}");
        assert!(after.contains("my .bashrc is at ~/dotfiles/bashrc"), "{after}");
        assert!(after.contains("the dev server runs on port 4100"), "{after}");
        assert!(after.contains(&format!("Instructions from {}", cwd.path().join("AGENTS.md").display())), "{after}");
    }

    /// When to search and to cite is said once, in the description of `web_search` (a sub-agent gets it too), and
    /// not a second time in the system text.
    #[test]
    fn the_web_tools_carry_their_own_guidance() {
        let tools = nexo_tools::builtin();
        let search = tools.get("web_search").unwrap().spec().description;
        for want in
            ["anything current or outside the repository", "`web_fetch` before relying on them", "cite their URLs"]
        {
            assert!(search.contains(want), "missing {want}: {search}");
        }
        let off = nexo_core::config::WebConfig { enabled: false, ..Default::default() };
        for set in [tools, nexo_tools::builtin_with(&off)] {
            let g = tool_guidance(&set);
            assert!(!g.contains("web_"), "{g}");
        }
    }

    /// The model is told to answer in the reply that holds its last checks only when that reply ends the turn
    /// (`agent.close_on_verified`); told so with the setting off, it writes the answer twice.
    #[test]
    fn the_line_about_answering_with_the_last_checks_is_sent_only_when_that_ends_the_turn() {
        let d = tempfile::tempdir().unwrap();
        let tools = nexo_tools::builtin();
        let off = AgentConfig { hermetic: true, ..Default::default() };
        let on = AgentConfig { close_on_verified: true, ..off.clone() };
        let line = "put the answer for the person in that same message";
        assert!(!off.close_on_verified && !system_prompt(d.path(), &off, &tools).contains(line));
        let with = system_prompt(d.path(), &on, &tools);
        assert!(with.contains(&format!("\n{CLOSE_ON_VERIFIED}\n\nWorking directory: ")), "{with}");
        assert!(with.starts_with(built_in()), "{with}");
        assert!(!system_prompt_for(d.path(), &on, &tools, Some(8_000)).contains(line));
        // `nexo prompt --builtin` prints what the session gets
        assert!(builtin_part_for(&tools, None, None, true).contains(line));
        assert!(!builtin_part(&tools, None).contains(line));
    }

    #[test]
    fn claude_md_and_nested_files_reach_the_prompt() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join("CLAUDE.md"), "Use tabs. @style.md").unwrap();
        std::fs::write(d.path().join("style.md"), "Tabs are four wide.").unwrap();
        std::fs::create_dir(d.path().join("web")).unwrap();
        std::fs::write(d.path().join("web/AGENTS.md"), "Web rules.").unwrap();
        let p = system_prompt(d.path(), &AgentConfig::default(), &ToolSet::new());
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert!(p.contains("Use tabs.") && p.contains("Tabs are four wide."), "{p}");
        assert!(p.contains("- web/AGENTS.md") && !p.contains("Web rules."), "{p}");
    }
}

#[cfg(test)]
mod size_budget {
    use super::*;

    /// Characters of every built-in tool spec (name, description, parameter schema) plus the tool guidance.
    fn tool_text_chars() -> usize {
        let set = nexo_tools::builtin();
        let specs: usize = set
            .specs()
            .iter()
            .map(|s| s.name.len() + s.description.len() + serde_json::to_string(&s.parameters).unwrap().len())
            .sum();
        specs + tool_guidance(&set).len()
    }

    /// 10135 characters (about 2500 tokens) when this budget was set, after the text said twice was taken out
    /// (the budget before it was 12150); it may grow 5% and no more (10650).
    /// A tool description says what is nexo's own (limits, errors, what saves steps), never what a
    /// model already knows about shells and search tools, and a parameter gets a description only for what its
    /// name and the tool description do not say.
    #[test]
    fn the_tool_text_stays_within_its_budget() {
        let n = tool_text_chars();
        // the `bash` description names the shell it found: on Windows that phrase is up to 43 characters longer than `sh -c`
        let budget = if cfg!(windows) { 10_700 } else { 10_650 };
        assert!(n <= budget, "tool descriptions and guidance take {n} characters; the budget is {budget}");
    }

    /// nexo writes parts of its own into the person's messages (ended jobs, the project note, hook output, files
    /// it attached, facts and lines it brought back, the supervisor's mark). Bare, a model answers them as if the
    /// person had just said them; the full prompt names each one as nexo's and the compact one names the kinds.
    #[test]
    fn both_prompts_name_every_part_nexo_adds_to_the_persons_messages() {
        let attached = crate::reads::ATTACHED.trim_end_matches(':');
        let marks = [
            "[background jobs ended]",
            "[project at the start of this request]",
            nexo_core::agent::SUPERVISOR_RESUME_MARK,
            nexo_core::agent::HOOK_CONTEXT_START,
            attached,
            "`Standing:`",
            "`Recall`",
        ];
        let line = |text: &'static str| {
            text.lines().find(|l| l.contains("the person did not type them")).unwrap_or_else(|| panic!("{text}"))
        };
        let full = line(BUILT_IN);
        for m in marks {
            assert!(full.contains(m), "{m} is not named in: {full}");
        }
        assert!(full.contains("do not reply to them as if the person had just said them"), "{full}");
        // the compact text has 2 600 characters in all: it names the kinds, not each label
        // and both say where the person's own words are
        assert!(
            full.contains(nexo_core::agent::PERSON_MARK)
                && line(BUILT_IN_COMPACT).contains(nexo_core::agent::PERSON_MARK)
        );
        let compact = line(BUILT_IN_COMPACT);
        for m in ["`[label]`", nexo_core::agent::HOOK_CONTEXT_START, "`@path (attached", "`Standing:`", "`Recall`"] {
            assert!(compact.contains(m), "{m} is not named in: {compact}");
        }
        assert!(compact.contains("do not reply to them"), "{compact}");
    }

    #[test]
    fn both_prompts_say_to_answer_at_once_when_the_proving_check_passes() {
        for text in [BUILT_IN, BUILT_IN_COMPACT] {
            assert!(text.contains("run the one check that proves it; when it passes, answer at once"), "{text}");
        }
    }

    #[test]
    fn the_guidance_names_what_saves_steps() {
        let g = tool_guidance(&nexo_tools::builtin());
        assert!(g.contains("run at the same time") && g.contains("many files"), "{g}");
    }

    #[test]
    fn the_built_in_part_has_neither_the_machine_nor_the_files() {
        let tools = nexo_tools::builtin();
        let b = builtin_part(&tools, Some("Directive text."));
        assert!(b.starts_with(built_in()) && b.contains("You can do:") && b.ends_with("Directive text."), "{b}");
        for not in ["Working directory", "Date:", "Instructions from", "The person's preferences", "System:"] {
            assert!(!b.contains(not), "{not}: {b}");
        }
        assert_eq!(builtin_part(&ToolSet::new(), None), built_in());
    }

    #[test]
    fn tool_definitions_show_name_description_and_parameters_of_each_tool() {
        let tools = nexo_tools::builtin();
        let specs = tools.specs();
        let text = tool_definitions(&specs);
        assert_eq!(text.matches("\n=== ").count() + 1, specs.len());
        for s in &specs {
            assert!(
                text.contains(&format!("=== {} ===\ndescription:\n{}\nparameters:\n{{", s.name, s.description)),
                "{}",
                s.name
            );
        }
        assert!(text.contains("\"type\": \"object\""), "{text}");
        assert_eq!(tool_definitions(&[]), "");
    }

    /// The compact specs of the built-in tools, as `nexo prompt --tools --window N` prints them.
    fn compact_tools_text() -> String {
        tool_definitions_for(&nexo_tools::builtin().compact_specs(), Some(8_000))
    }

    #[test]
    fn a_large_window_gets_the_full_text_unchanged() {
        let tools = nexo_tools::builtin();
        for w in [None, Some(200_000), Some(COMPACT_WINDOW + 1)] {
            assert_eq!(builtin_part_for(&tools, Some("D."), w, false), builtin_part(&tools, Some("D.")), "{w:?}");
            assert_eq!(tool_definitions_for(&tools.specs(), w), tool_definitions(&tools.specs()), "{w:?}");
        }
        assert!(!is_compact(None) && !is_compact(Some(COMPACT_WINDOW + 1)) && is_compact(Some(COMPACT_WINDOW)));
        let d = tempfile::tempdir().unwrap();
        let cfg = AgentConfig { hermetic: true, ..Default::default() };
        let full = system_prompt(d.path(), &cfg, &tools);
        assert_eq!(system_prompt_for(d.path(), &cfg, &tools, Some(200_000)), full);
        assert!(full.contains("How to write code:"));
    }

    #[test]
    fn a_small_window_gets_the_compact_text_within_its_caps() {
        let tools = nexo_tools::builtin();
        for w in [8_000, COMPACT_WINDOW] {
            let b = builtin_part_for(&tools, None, Some(w), false);
            assert!(b.len() <= 2_600, "{w}: the compact built-in part takes {} characters", b.len());
            assert!(!b.contains("How to write code:") && !b.contains("You can do:"), "{b}");
            let t = tool_definitions_for(&tools.compact_specs(), Some(w));
            assert!(t.len() <= 6_000, "{w}: the compact tools take {} characters", t.len());
            let json = serde_json::to_string(&tools.compact_specs()).unwrap();
            assert!(json.len() <= 6_000, "{w}: the compact tool JSON takes {} characters", json.len());
        }
        assert!(BUILT_IN_COMPACT.len() <= 2_400, "{}", BUILT_IN_COMPACT.len());
        assert!(compact_guidance(&tools).lines().filter(|l| l.starts_with("- ")).count() <= 2);
    }

    #[test]
    fn the_compact_prompt_keeps_the_safety_rules() {
        let b = builtin_part_for(&nexo_tools::builtin(), None, Some(8_000), false).to_lowercase();
        for want in ["secrets", "destructive", "only the person", "explicit request", "information"] {
            assert!(b.contains(want), "missing {want}: {b}");
        }
        assert!(b.starts_with("you are nexo,"), "{b}");
    }

    #[test]
    fn a_compact_tool_keeps_its_parameter_names_types_and_required_list() {
        let tools = nexo_tools::builtin();
        let (full, compact) = (tools.specs(), tools.compact_specs());
        assert_eq!(full.len(), compact.len());
        fn shape(v: &serde_json::Value, out: &mut Vec<String>, at: &str) {
            let Some(o) = v.as_object() else { return };
            if let Some(t) = o.get("type").and_then(|t| t.as_str()) {
                out.push(format!("{at}:{t}"));
            }
            out.push(format!("{at}:required={}", o.get("required").map_or("-".into(), |r| r.to_string())));
            if let Some(p) = o.get("properties").and_then(|p| p.as_object()) {
                for (k, v) in p {
                    shape(v, out, &format!("{at}.{k}"));
                }
            }
            if let Some(i) = o.get("items") {
                shape(i, out, &format!("{at}[]"));
            }
        }
        for (f, c) in full.iter().zip(&compact) {
            assert_eq!(f.name, c.name);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            shape(&f.parameters, &mut a, "");
            shape(&c.parameters, &mut b, "");
            assert_eq!(a, b, "{}", f.name);
            assert!(c.description.len() < f.description.len() && c.description.len() <= 200, "{}", f.name);
        }
        assert!(compact_tools_text().contains("=== bash ==="));
    }
}
