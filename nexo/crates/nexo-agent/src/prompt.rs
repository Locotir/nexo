//! The system prompt: who the agent is, where it works, what its tools are
//! for, and the project's own instructions (`NEXO.md`, `AGENTS.md`).

use std::path::{Path, PathBuf};

use nexo_core::config::AgentConfig;
use nexo_core::paths;
use nexo_core::tool::ToolSet;

/// The built-in prompt, kept as plain text in `prompt.md` next to this file so it can be edited as such.
const BUILT_IN: &str = include_str!("prompt.md");

/// A prompt file whose first line is this is sent as it is: no tool guidance,
/// directory, date, project instructions or extra text appended.
pub const RAW_MARK: &str = "<!-- nexo: raw -->";

/// The built-in text: what every request starts with.
pub fn built_in() -> &'static str {
    BUILT_IN.trim()
}

/// What a new `~/.nexo/prompt.md` holds: the file adds the person's own preferences after the built-in text.
pub const PREFS_TEMPLATE: &str = "<!-- Your preferences: added after nexo's built-in prompt, which stays first (tools, subagents, how to work).
     Write who you are, your machine and stack, how you like answers. Comments like this one are not sent.
     A file that starts with \"You are nexo,\" replaces the built-in prompt instead (README: Your own system prompt). -->
";

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

/// The guidance for the tools present, by name.
fn tool_guidance(tools: &ToolSet) -> String {
    let mut lines = Vec::new();
    let has = |n: &str| tools.get(n).is_some();
    if has("read") {
        lines.push("- `read`: read a file (numbered lines; `offset`/`limit` for a part of a long file). Read a file before editing it; a file you already have and that has not changed comes back as a short note, not again.");
    }
    if has("edit") {
        lines.push("- `edit`: replace one exact occurrence of `old_string` with `new_string` in a file (`replace_all` for every occurrence). The old text must match exactly, whitespace included; include enough context to be unique.");
    }
    if has("write") {
        lines.push("- `write`: create or overwrite a whole file. Prefer `edit` for existing files.");
    }
    if has("bash") {
        lines.push("- `bash`: run a shell command in the working directory (tests, builds, git, scripts). Quote paths. Avoid interactive commands; set `timeout` (seconds) for slow commands. For anything that runs long or does not end on its own (a dev server, a watcher, a full test suite or build while you do other work), set `background: true`: it starts a job and returns at once.");
    }
    if has("jobs") {
        lines.push("- `jobs`: the background jobs (commands and sub-agents). You are told when one ends; read its output with action `output` (add `wait` seconds to wait for it), stop it with `kill`. Do not poll in a loop: go on with other work, and wait only when you need the result.");
    }
    if has("glob") {
        lines.push("- `glob`: find files by name pattern (`src/**/*.rs`).");
    }
    if has("grep") {
        lines.push("- `grep`: search file contents with a regular expression; narrow with `path` or `glob`.");
    }
    if has("ls") {
        lines.push("- `ls`: list a directory.");
    }
    if has("todo") {
        lines.push("- `todo`: for work with three or more steps, keep the list with `todo`: write it first, mark the step in progress before starting it and done when verified; keep it short.");
    }
    if has("note") {
        lines.push("- `note`: when a result settles a fact (a value, a formula, an address, a format, the cause of an error), keep it with `note` as one line `Finding: ...`; keep what is left as one line `Plan: ...` and the user's standing instructions as `Rule: ...`. Older messages leave the context; the notes stay with the first message.");
    }
    if has("diagnostics") {
        lines.push("- `diagnostics`: after you edit a source file, ask its language server what is wrong with it (errors and warnings with their lines) before you call the change done.");
    }
    if has("task") {
        lines.push("- `task`: hand self-contained work to a sub-agent; only its short report comes back, so your own context stays small. Use it when a question needs reading across many files or a whole directory (`explore`, the default), when there are several independent searches or checks (one task call each, all in ONE message, so they run at the same time), for a long side job while you go on (`background: true`), and whenever the person asks for sub-agents or parallel work. `general` can also edit and run commands. Do it yourself when one or two reads or a single search will answer. The sub-agent sees nothing of this conversation: put all it needs in `prompt` (the goal, the files, what is ruled out, what to report).");
    }
    if has("ask_user") {
        lines.push("- `ask_user`: when a choice is the person's to make (an approach, a name, which of several things), ask with a few options, the one you recommend first. Not for what you can find out yourself.");
    }
    if has("web_search") {
        lines.push("- `web_search`: search the web for anything current or outside the repository (library docs and versions, error messages, APIs). Do not answer such questions from memory when you can check.");
    }
    if has("web_fetch") {
        lines.push("- `web_fetch`: read a page as Markdown. Open the best results before relying on them, read on with `start` when a page is cut, and cite the URLs you used.");
    }
    let mcp: Vec<&str> = tools.names().filter(|n| n.starts_with("mcp__")).collect();
    if !mcp.is_empty() {
        lines.push(
            "- Tools named `mcp__<server>__<tool>` come from connected servers; their descriptions say what they do.",
        );
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("\n\nTools:\n{}", lines.join("\n"))
}

/// The whole system prompt for a session in `cwd`: the built-in text first, then the directory,
/// date, tools and project instructions, then the person's preferences (`~/.nexo/prompt.md`).
pub fn system_prompt(cwd: &Path, cfg: &AgentConfig, tools: &ToolSet) -> String {
    let file = source_file(cfg)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(raw) = file.as_deref().and_then(|t| t.strip_prefix(RAW_MARK)) {
        return raw.trim().to_string();
    }
    let (base, prefs) = match &file {
        Some(t) if replaces(cfg, t) => (paths::prompt_base(t), String::new()),
        Some(t) => (built_in(), without_comments(t)),
        None => (built_in(), String::new()),
    };
    let date = time::OffsetDateTime::now_utc().date();
    let mut out = format!("{base}\n\nWorking directory: {}\nDate: {date}", cwd.display());
    out.push_str(&tool_guidance(tools));
    let instructions = if cfg.hermetic { Vec::new() } else { paths::project_instructions(cwd) };
    for f in instructions {
        if let Ok(text) = std::fs::read_to_string(&f) {
            let text = text.trim();
            if !text.is_empty() {
                out.push_str(&format!("\n\nInstructions from {}:\n{text}", f.display()));
            }
        }
    }
    if !prefs.is_empty() {
        out.push_str(&format!("\n\nThe person's preferences:\n{prefs}"));
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
    fn the_web_tools_get_their_guidance_only_when_present() {
        let with = tool_guidance(&nexo_tools::builtin());
        assert!(with.contains("`web_search`: search the web") && with.contains("cite the URLs"), "{with}");
        let off = nexo_core::config::WebConfig { enabled: false, ..Default::default() };
        let without = tool_guidance(&nexo_tools::builtin_with(&off));
        assert!(!without.contains("web_"), "{without}");
    }
}
