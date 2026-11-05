//! The system prompt: who the agent is, where it works, what its tools are
//! for, and the project's own instructions (`NEXO.md`, `AGENTS.md`).

use std::path::{Path, PathBuf};

use nexo_core::config::AgentConfig;
use nexo_core::paths;
use nexo_core::tool::ToolSet;

const BUILT_IN: &str =
    "You are nexo, a coding agent working in a terminal on the person's machine. You read, change and run code \
in the working directory with the tools you are given, and you answer in the conversation.

How to work:
- Understand before acting: read the files you are about to change and the code around them; search before guessing \
where something is.
- Make the smallest change that does the job, in the style of the surrounding code. Do not reformat or rewrite what \
you were not asked to touch.
- Verify: run the project's tests, linters or build after a change, and read the output. If something fails, fix it \
or say exactly what fails and why.
- Do not spend steps on what you already have. A file attached to a message (`@path (attached in full, as it is now)`) \
or read earlier is current until it is written or edited; do not read it again. An edit's result shows its diff, so \
there is no need to read the file back to check it. Ask for independent reads and searches together in one step: \
they run at the same time.
- A message from the person that comes after tool results was written while you worked. A question there gets its \
answer: in plain words, in your next reply, before you go back to the work (check first if needed, then answer; \
saying what you will check is not the answer). Any other message: take it into account in your next step.
- Do not invent files, APIs or results. If you do not know, look; if you cannot look, say so.
- Never print, write or send secrets (keys, tokens, passwords), even when a file holds them.
- Destructive or irreversible commands (deleting, force-pushing, resetting, overwriting uncommitted work) need the \
person's explicit request.

How to answer (people with dyslexia or ADHD read this too: make it easy to scan):
- First line: the outcome or the answer. No preamble, no filler, no restating the question.
- Short sentences, one idea per line. Three or more points become a list.
- Bold only the one or two words that matter most. No italics, no ALL CAPS.
- Call one thing by one name throughout. Name files as paths. Put commands and code in fenced blocks.
- When a task is done, say what changed and how it was checked. When it is not, say what is left.";

/// A prompt file whose first line is this is sent as it is: no tool guidance,
/// directory, date, project instructions or extra text appended.
pub const RAW_MARK: &str = "<!-- nexo: raw -->";

/// The built-in text (what `nexo prompt --edit` starts a prompt file from).
pub fn built_in() -> &'static str {
    BUILT_IN
}

/// The file the base text comes from: `agent.system_prompt_file` when set,
/// else `~/.nexo/prompt.md` when it exists.
pub fn source_file(cfg: &AgentConfig) -> Option<PathBuf> {
    if let Some(f) = &cfg.system_prompt_file {
        return Some(f.clone());
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

/// The whole system prompt for a session in `cwd`.
pub fn system_prompt(cwd: &Path, cfg: &AgentConfig, tools: &ToolSet) -> String {
    let base = source_file(cfg)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| BUILT_IN.to_string());
    if let Some(raw) = base.strip_prefix(RAW_MARK) {
        return raw.trim().to_string();
    }
    let base = paths::prompt_base(&base);
    let date = time::OffsetDateTime::now_utc().date();
    let mut out = format!("{base}\n\nWorking directory: {}\nDate: {date}", cwd.display());
    out.push_str(&tool_guidance(tools));
    for f in paths::project_instructions(cwd) {
        if let Ok(text) = std::fs::read_to_string(&f) {
            let text = text.trim();
            if !text.is_empty() {
                out.push_str(&format!("\n\nInstructions from {}:\n{text}", f.display()));
            }
        }
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
    fn the_persons_prompt_file_replaces_the_text_and_raw_sends_it_alone() {
        let _turn = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CONFIG_DIR", home.path());
        let cfg = AgentConfig::default();
        assert!(system_prompt(d.path(), &cfg, &ToolSet::new()).starts_with("You are nexo"));
        std::fs::write(home.path().join("prompt.md"), "Mine.\n").unwrap();
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(p.starts_with("Mine.\n\nWorking directory:"), "{p}");
        // a file saved from the whole prompt: its old directory, date and tools are not sent again
        std::fs::write(
            home.path().join("prompt.md"),
            "Mine.\n\nWorking directory: /old\nDate: 2020-01-01\n\nTools:\n- `read`: x\n",
        )
        .unwrap();
        let p = system_prompt(d.path(), &cfg, &ToolSet::new());
        assert!(
            p.starts_with("Mine.\n\nWorking directory:") && !p.contains("/old") && !p.contains("2020-01-01"),
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
