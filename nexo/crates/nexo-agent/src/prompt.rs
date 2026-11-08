//! The system prompt: who the agent is, where it works, what its tools are
//! for, and the project's own instructions (`NEXO.md`, `AGENTS.md`, `CLAUDE.md`).

use std::path::{Path, PathBuf};

use nexo_core::compat;
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
    if has("read") || has("grep") {
        lines.push("- Fewer steps: calls to `read`, `grep`, `glob`, `ls` and the web tools in one message run at the same time, so send every independent look together; several changes to one file go in one `edit` call (`edits`).");
    }
    if has("note") {
        lines.push("- `note`: keep each settled fact as `Finding: ...`, what is left as `Plan: ...`, the user's standing instructions as `Rule: ...` (scope `project` or `user` saves it for later sessions when the user corrects the same thing twice or states a lasting preference). Send it with your next tool call, never as a response of its own.");
    }
    if has("task") {
        lines.push("- `task`: use it when a question needs reading across many files, for several independent searches or checks (one call each, all in ONE message), for a long side job (`background: true`), and whenever the person asks for sub-agents or parallel work. Do it yourself when one or two reads or a single search will answer. The sub-agent sees nothing of this conversation: put all it needs in `prompt`.");
    }
    if has("skill") {
        lines.push("- `skill`: skills are folders of ready-made instructions; when a task matches one in the tool's list, load it before you start.");
    }
    if has("web_search") {
        lines
            .push("- `web_search`: check anything current or outside the repository instead of answering from memory.");
    }
    if has("web_fetch") {
        lines.push("- `web_fetch`: open the best results before relying on them and cite their URLs.");
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

/// The machine and the project in two lines: the system (with the Linux distribution, which says
/// apt, apk or dnf) and whether `cwd` is in a git repository. Both stay the same for a session, so
/// the cached prompt prefix does too.
fn environment(cwd: &Path) -> String {
    let distro = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| os_name(&t));
    let system = match distro {
        Some(d) => format!("{} ({d}), {}", std::env::consts::OS, std::env::consts::ARCH),
        None => format!("{}, {}", std::env::consts::OS, std::env::consts::ARCH),
    };
    format!("\nSystem: {system}\nGit: {}", git_line(cwd))
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
    match cwd.ancestors().find(|d| d.join(".git").exists()) {
        Some(root) if root == cwd => "this folder is a git repository".into(),
        Some(root) => format!("inside the git repository at {}", root.display()),
        None => "not a git repository".into(),
    }
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
    out.push_str(&environment(cwd));
    out.push_str(&tool_guidance(tools));
    if !cfg.hermetic {
        let home = compat::foreign_home();
        for i in compat::instructions(cwd, &paths::config_dir(), home.as_deref()) {
            out.push_str(&format!("\n\nInstructions from {}:\n{}", i.path.display(), i.text));
        }
        let nested = compat::nested_instructions(cwd);
        if !nested.is_empty() {
            // forward slashes on every system: the model reads one path style
            let list: Vec<String> =
                nested.iter().map(|p| format!("- {}", p.display().to_string().replace('\\', "/"))).collect();
            out.push_str(&format!(
                "\n\nMore instruction files, for folders you are not in (read one before you work there):\n{}",
                list.join("\n")
            ));
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
    fn names_the_request_gives_are_kept() {
        // configure-git-webserver: asked for user@server, the model pushed as the account it found
        assert!(built_in().contains("use them exactly. When one does not exist yet, create it"));
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
    fn the_web_tools_get_their_guidance_only_when_present() {
        let with = tool_guidance(&nexo_tools::builtin());
        assert!(with.contains("`web_search`: check anything current") && with.contains("cite their URLs"), "{with}");
        let off = nexo_core::config::WebConfig { enabled: false, ..Default::default() };
        let without = tool_guidance(&nexo_tools::builtin_with(&off));
        assert!(!without.contains("web_"), "{without}");
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

    /// 10565 characters (about 2600 tokens) when this budget was set; it may grow 10% and no more.
    /// A tool description says what is nexo's own (limits, errors, what saves steps), never what a
    /// model already knows about shells and search tools.
    #[test]
    fn the_tool_text_stays_within_its_budget() {
        let n = tool_text_chars();
        assert!(n <= 11_620, "tool descriptions and guidance take {n} characters; the budget is 11620");
    }

    #[test]
    fn the_guidance_names_what_saves_steps() {
        let g = tool_guidance(&nexo_tools::builtin());
        assert!(g.contains("run at the same time") && g.contains("`edits`"), "{g}");
    }
}
