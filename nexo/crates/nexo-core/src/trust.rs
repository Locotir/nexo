//! What trusting the project of the working directory would switch on, for the question the
//! interactive start asks (`Do you trust the files in this folder?`). The answer is recorded as
//! `nexo config trust` records it. Everything that trust gates is read from the same places the
//! loading reads it from, so the question is asked only when there is something to gate.

use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::managed::ManagedPolicy;

/// A project that is not trusted yet and holds something trust gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The folder `nexo config trust` would list (resolved).
    pub root: PathBuf,
    /// What would run: `hooks`, `MCP servers`.
    pub runs: Vec<&'static str>,
    /// What would be loaded: `commands`, `agents`, `settings`.
    pub loads: Vec<&'static str>,
    /// The folder is the person's home: trusting it trusts all of it.
    pub home: bool,
}

fn list(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

impl Gate {
    /// One short line on what a yes enables.
    pub fn enables(&self) -> String {
        match (self.runs.is_empty(), self.loads.is_empty()) {
            (false, true) => format!("Project {} will run.", list(&self.runs)),
            (true, _) => format!("Project {} will load.", list(&self.loads)),
            (false, false) => format!("Project {} will run and its {} will load.", list(&self.runs), list(&self.loads)),
        }
    }

    /// A line to set apart when the folder is the home folder.
    pub fn warning(&self) -> Option<String> {
        self.home.then(|| {
            "This is your home folder: trusting it trusts every folder below it that is not a project of its own."
                .to_string()
        })
    }

    /// Lists the folder in `trust_file`, as `nexo config trust` does; true when it was not there yet.
    pub fn record(&self, trust_file: &Path) -> std::io::Result<bool> {
        config::trust_project(&self.root, trust_file)
    }
}

/// Whether the start of the interface asks: no prompt (`-p`), no subcommand, and a real terminal
/// at both ends. Anything else keeps the notice the loading prints.
pub fn asks(has_prompt: bool, has_subcommand: bool, stdin_tty: bool, stdout_tty: bool) -> bool {
    !has_prompt && !has_subcommand && stdin_tty && stdout_tty
}

fn has_md(dir: &Path, depth: usize) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            depth < 4 && has_md(&p, depth + 1)
        } else {
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md"))
        }
    })
}

/// What trusting the project of `cwd` would enable, or `None` when it is trusted already, there is
/// no project, or nothing in it is gated (or the managed `policy` forbids all of it). `home` is the
/// person's home folder.
///
/// Two folders can be the one to trust: the root of `.nexo/config.toml` (what `.mcp.json` and the
/// project's `.nexo` folders hang from) and the nearest `.git` folder, where the other tools'
/// `.claude` and `.codex` files are read from and the warning at loading points. They are often the
/// same; the person's home kept in git has only the second. The first with something to enable is asked.
pub fn gate(
    cwd: &Path,
    user_file: &Path,
    trust_file: &Path,
    home: Option<&Path>,
    policy: Option<&ManagedPolicy>,
) -> Option<Gate> {
    let git_root = {
        let r = crate::compat::project_root(cwd);
        r.canonicalize().unwrap_or(r)
    };
    let nexo = crate::paths::project_config_file(cwd).map(|f| (config::project_root(&f), f));
    let mut targets: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    if let Some((root, file)) = &nexo {
        targets.push((root.clone(), Some(file.clone())));
    }
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    // a home kept in git: its `.claude` and `.codex` are the person's own folders, read as theirs
    // already (and the loading ignores them as a project's), so they are nothing to trust
    let is_home = |root: &Path| home.is_some_and(|h| real(h) == root);
    if nexo.as_ref().is_none_or(|(root, _)| *root != git_root) && !is_home(&git_root) {
        targets.push((git_root.clone(), None));
    }
    targets.into_iter().find_map(|(root, file)| {
        if config::is_trusted(&root, trust_file) {
            return None;
        }
        let foreign = root == git_root && !is_home(&root);
        let (runs, loads) = scan(cwd, &root, file.as_deref(), foreign, user_file, trust_file, policy)?;
        let home = is_home(&root);
        Some(Gate { root, runs, loads, home })
    })
}

/// What is gated under `root`: `file` is its `.nexo/config.toml` (none: the root has no `.nexo`
/// project), `foreign` whether the other tools' files are read from it.
fn scan(
    cwd: &Path,
    root: &Path,
    file: Option<&Path>,
    foreign: bool,
    user_file: &Path,
    trust_file: &Path,
    policy: Option<&ManagedPolicy>,
) -> Option<(Vec<&'static str>, Vec<&'static str>)> {
    let (mut runs, mut loads) = (Vec::new(), Vec::new());
    let hooks_ok = policy.is_none_or(|p| p.project_hooks_allowed());
    let mcp_ok = |name: &str, s: &config::McpServerConfig| policy.is_none_or(|p| p.check_mcp(name, s).is_ok());
    let commands_ok = policy.is_none_or(|p| p.extension_allowed("project", "commands"));
    let (mut hooks, mut mcp, mut commands, mut agents) = (false, false, false, false);

    if let Some(file) = file {
        // `<root>/.nexo/config.toml`: its hooks, servers and the other settings an untrusted project loses
        if let Some(mut part) = std::fs::read_to_string(file).ok().and_then(|t| Config::parse(&t).ok()) {
            hooks |= hooks_ok && !part.hooks.is_empty();
            mcp |= part.mcp.iter().any(|(n, s)| mcp_ok(n, s));
            let user = Config::load_files(user_file, None, trust_file).unwrap_or_default();
            let left = part.strip_untrusted(&user);
            if left.iter().any(|k| k != "hooks" && !k.starts_with("mcp.")) {
                loads.push("settings");
            }
        }
        // `<root>/.mcp.json`
        if let Some(found) =
            std::fs::read_to_string(root.join(".mcp.json")).ok().and_then(|t| crate::mcp_json::parse(&t).ok())
        {
            mcp |= found.iter().any(|(n, s)| mcp_ok(n, s));
        }
        commands |= commands_ok && has_md(&root.join(".nexo/commands"), 0);
        agents |= has_md(&root.join(".nexo/agents"), 0);
    }
    if foreign {
        // the same reading as the notice of the loading (`compat::claude_hooks`)
        hooks |= hooks_ok && crate::compat::claude_hooks(cwd, trust_file).1.is_some();
        let claude = crate::compat::claude_code();
        commands |= commands_ok
            && ((claude && has_md(&root.join(".claude/commands"), 0)) || has_md(&root.join(".codex/prompts"), 0));
        agents |= claude && has_md(&root.join(".claude/agents"), 0);
    }
    if hooks {
        runs.push("hooks");
    }
    if mcp {
        runs.push("MCP servers");
    }
    if commands {
        loads.insert(0, "commands");
    }
    if agents {
        let at = loads.iter().position(|w| *w == "settings").unwrap_or(loads.len());
        loads.insert(at, "agents");
    }
    (!runs.is_empty() || !loads.is_empty()).then_some((runs, loads))
}

/// [`gate`] with the person's own files and home folder.
pub fn gate_here(cwd: &Path, policy: Option<&ManagedPolicy>) -> Option<Gate> {
    gate(
        cwd,
        &crate::paths::user_config_file(),
        &crate::paths::trusted_projects_file(),
        dirs::home_dir().as_deref(),
        policy,
    )
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

    const HOOK: &str = "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"check\"\n";
    const MCP_JSON: &str = r#"{"mcpServers": {"files": {"command": "npx", "args": ["srv"]}}}"#;

    fn ask(p: &Path, cfg: &Path, policy: Option<&ManagedPolicy>) -> Option<Gate> {
        gate(p, &cfg.join("config.toml"), &cfg.join("trusted-projects"), None, policy)
    }

    #[test]
    fn a_folder_without_a_project_or_with_nothing_gated_is_not_asked_about() {
        let (cfg, bare) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        assert_eq!(ask(bare.path(), cfg.path(), None), None, "no .git or .nexo above");
        let p = repo();
        assert_eq!(ask(p.path(), cfg.path(), None), None, "a repository with no config");
        w(p.path().join(".nexo/config.toml"), "[ui]\ntheme = \"light\"\n");
        assert_eq!(ask(p.path(), cfg.path(), None), None, "a theme is not gated");
    }

    #[test]
    fn hooks_servers_and_commands_name_what_a_yes_enables() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".nexo/config.toml"), HOOK);
        let g = ask(p.path(), cfg.path(), None).unwrap();
        assert_eq!((g.runs.as_slice(), g.loads.as_slice()), (&["hooks"][..], &[][..]));
        assert_eq!(g.enables(), "Project hooks will run.");
        w(p.path().join(".mcp.json"), MCP_JSON);
        let g = ask(p.path(), cfg.path(), None).unwrap();
        assert_eq!(g.enables(), "Project hooks and MCP servers will run.");
        w(p.path().join(".nexo/commands/ship.md"), "Ship it.");
        w(p.path().join(".claude/agents/docs.md"), "Docs.");
        let g = ask(p.path(), cfg.path(), None).unwrap();
        assert_eq!(g.enables(), "Project hooks and MCP servers will run and its commands and agents will load.");
        assert!(g.warning().is_none());
    }

    #[test]
    fn claude_hooks_and_an_mcp_json_alone_are_enough() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".claude/settings.json"), r#"{"hooks": {"Stop": [{"hooks": [{"command": "done.sh"}]}]}}"#);
        assert_eq!(ask(p.path(), cfg.path(), None).unwrap().runs, ["hooks"]);
        let q = repo();
        w(q.path().join(".mcp.json"), MCP_JSON);
        assert_eq!(ask(q.path(), cfg.path(), None).unwrap().runs, ["MCP servers"]);
    }

    #[test]
    fn a_trusted_project_is_not_asked_about_and_a_yes_is_recorded_like_config_trust() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".nexo/config.toml"), HOOK);
        let g = ask(p.path(), cfg.path(), None).unwrap();
        let list = cfg.path().join("trusted-projects");
        assert!(g.record(&list).unwrap());
        assert!(config::is_trusted(p.path(), &list));
        assert_eq!(ask(p.path(), cfg.path(), None), None);
        assert!(!g.record(&list).unwrap(), "listed once");
    }

    #[test]
    fn a_sub_folder_asks_about_the_project_root() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".nexo/config.toml"), HOOK);
        std::fs::create_dir_all(p.path().join("src/deep")).unwrap();
        let g = ask(&p.path().join("src/deep"), cfg.path(), None).unwrap();
        assert_eq!(g.root, p.path().canonicalize().unwrap());
    }

    #[test]
    fn the_managed_policy_leaves_out_what_it_forbids() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".nexo/config.toml"), HOOK);
        w(p.path().join(".mcp.json"), MCP_JSON);
        w(p.path().join(".nexo/commands/ship.md"), "Ship it.");
        let policy = |t: &str| ManagedPolicy::parse(t).unwrap();
        let hooks_off = policy("allow_project_hooks = false");
        assert_eq!(ask(p.path(), cfg.path(), Some(&hooks_off)).unwrap().runs, ["MCP servers"]);
        let all_off =
            policy("allow_project_hooks = false\n[mcp]\nallow = [\"other\"]\n[extensions]\nproject_commands = false\n");
        assert_eq!(ask(p.path(), cfg.path(), Some(&all_off)), None, "nothing left to trust");
        let mcp_off = policy("[mcp]\ndeny = [\"files\"]\n");
        assert_eq!(ask(p.path(), cfg.path(), Some(&mcp_off)).unwrap().runs, ["hooks"]);
    }

    /// The person's home kept in git has no `.nexo` project (its `~/.nexo` is their own folder) and its
    /// `~/.claude` is their own too: the loading reads none of it as a project's, so nothing is asked.
    #[test]
    fn a_home_kept_in_git_is_not_asked_about_for_its_own_dot_folders() {
        let (cfg, home) = (tempfile::tempdir().unwrap(), repo());
        std::fs::create_dir_all(home.path().join(".nexo")).unwrap();
        w(home.path().join(".claude/commands/theirs.md"), "Theirs.");
        w(home.path().join(".claude/agents/a.md"), "Agent.");
        w(home.path().join(".claude/settings.json"), r#"{"hooks": {"Stop": [{"hooks": [{"command": "done.sh"}]}]}}"#);
        let trust = cfg.path().join("trusted-projects");
        let user = cfg.path().join("config.toml");
        let sub = home.path().join("work/deep");
        std::fs::create_dir_all(&sub).unwrap();
        for at in [home.path(), sub.as_path()] {
            assert_eq!(gate(at, &user, &trust, Some(home.path()), None), None, "{at:?}");
        }
        // the home is only known by the caller: any other folder kept in git is asked about
        let other = repo();
        w(other.path().join(".claude/commands/theirs.md"), "Theirs.");
        let g = gate(other.path(), &user, &trust, Some(home.path()), None).unwrap();
        assert_eq!(g.loads, ["commands"]);
        assert!(!g.home);
    }

    #[test]
    fn the_home_folder_is_said_to_be_the_home_folder() {
        let cfg = tempfile::tempdir().unwrap();
        let p = repo();
        w(p.path().join(".nexo/config.toml"), HOOK);
        let g = gate(p.path(), &cfg.path().join("config.toml"), &cfg.path().join("t"), Some(p.path()), None).unwrap();
        assert!(g.home && g.warning().unwrap().contains("home folder"));
    }

    #[test]
    fn only_an_interactive_start_on_a_terminal_asks() {
        assert!(asks(false, false, true, true));
        assert!(!asks(true, false, true, true), "-p");
        assert!(!asks(false, true, true, true), "a subcommand (acp, doctor, review...)");
        assert!(!asks(false, false, false, true), "stdin is not a terminal");
        assert!(!asks(false, false, true, false), "stdout is not a terminal");
    }
}
