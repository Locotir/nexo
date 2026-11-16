//! The clap schema: arguments, subcommands and the value validators. Pure data.

use super::*;

#[derive(Parser, Debug)]
#[command(name = "nexo", version = env!("NEXO_VERSION"), about = "A terminal coding harness for any model provider.", long_about = None)]
pub struct Args {
    /// Run one request without the interface and print the reply.
    #[arg(short = 'p', long = "print", value_name = "PROMPT", num_args = 0..=1, default_missing_value = "")]
    pub prompt: Option<String>,
    /// With --print: print every agent event as a JSON line instead of the text.
    #[arg(long)]
    pub json: bool,
    #[command(flatten)]
    pub headless: crate::headless::Flags,
    /// Provider name (a preset or one from the configuration).
    #[arg(long, env = "NEXO_PROVIDER")]
    pub provider: Option<String>,
    /// Model name.
    #[arg(long, env = "NEXO_MODEL")]
    pub model: Option<String>,
    /// Working directory (default: the current one).
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    /// Resume a session by id.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,
    /// Resume the latest session of this directory.
    #[arg(short = 'c', long = "continue")]
    pub continue_: bool,
    /// ask | accept-edits | plan | yolo | read-only
    #[arg(long, value_name = "MODE", env = "NEXO_PERMISSION_MODE", value_parser = permission_mode_arg)]
    pub permission_mode: Option<String>,
    /// Never ask for permission (the same as --permission-mode yolo).
    #[arg(long, conflicts_with = "permission_mode")]
    pub yolo: bool,
    /// OS sandbox for the bash tool: off | workspace-write | read-only
    #[arg(long, value_name = "MODE", env = "NEXO_SANDBOX", value_parser = sandbox_mode_arg)]
    pub sandbox: Option<String>,
    /// Context policy: auto | plain | curator | compact (auto is the curator, built in)
    #[arg(long, value_name = "POLICY", env = "NEXO_CONTEXT")]
    pub context: Option<String>,
    /// Curator settings for this run, over [context] curator_spec (`eoq=32000;dedup=0`)
    #[arg(long, value_name = "SPEC", env = "NEXO_CURATOR_SPEC")]
    pub curator_spec: Option<String>,
    /// Theme: dark, nexo, light, mono, catppuccin-mocha, gruvbox, nord, dracula, solarized-dark, tokyo-night
    #[arg(long, value_parser = theme_arg)]
    pub theme: Option<String>,
    /// Disable mouse capture (lets the terminal select text natively).
    #[arg(long)]
    pub no_mouse: bool,
    /// Draw on the normal screen, so the session stays in the scrollback after quitting.
    #[arg(long)]
    pub no_alt_screen: bool,
    /// Inline mode: finished turns go to the terminal's own scrollback, only the draft and the turn in progress stay live.
    #[arg(long)]
    pub inline: bool,
    /// Run the session in a new git worktree on branch nexo/<name> (default: a short random name); kept if it changed, removed if not.
    #[arg(short = 'w', long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    pub worktree: Option<String>,
    /// Log to this file (RUST_LOG filters).
    #[arg(long, value_name = "FILE")]
    pub log: Option<PathBuf>,
    /// The first request: the session opens and sends it (`nexo "fix the failing test"`).
    #[arg(value_name = "PROMPT")]
    pub first: Option<String>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

/// `--sandbox`: refused at once, with the values that work.
fn sandbox_mode_arg(s: &str) -> Result<String, String> {
    match nexo_core::sandbox::SandboxMode::parse(s) {
        Some(_) => Ok(s.to_string()),
        None => Err(format!("unknown sandbox mode {s:?}; use one of: off, workspace-write, read-only")),
    }
}

/// `--permission-mode`: refused at once, with the values that work.
fn permission_mode_arg(s: &str) -> Result<String, String> {
    match PermissionMode::parse(s) {
        Some(_) => Ok(s.to_string()),
        None => Err(format!("unknown permission mode {s:?}; use one of: ask, accept-edits, plan, yolo, read-only")),
    }
}

/// `--theme`: refused at once, with the names that work.
fn theme_arg(s: &str) -> Result<String, String> {
    // "auto" is the config's own value for the terminal's background
    if s == "auto" || nexo_tui::theme::Theme::by_name(s).is_some() {
        Ok(s.to_string())
    } else {
        Err(format!("unknown theme {s:?}; use one of: auto, {}", nexo_tui::theme::NAMES.join(", ")))
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List the providers in use and where each one's key comes from.
    Providers {
        /// Every provider nexo knows (200+), not only the usable ones.
        #[arg(long)]
        all: bool,
        /// Print JSON (for scripts and assistants).
        #[arg(long)]
        json: bool,
    },
    /// List the models of a provider with their limits and prices (configured, catalogue, and the vendor's list when reachable).
    Models {
        /// Provider name (default: the one in use).
        provider: Option<String>,
        /// Fetch the models.dev catalogue again before listing.
        #[arg(long)]
        refresh: bool,
        /// Print JSON (for scripts and assistants).
        #[arg(long)]
        json: bool,
    },
    /// Keep keys and subscription logins (GitHub Copilot, ChatGPT, SuperGrok) in the credential store.
    Auth {
        #[command(subcommand)]
        what: AuthCmd,
    },
    /// Show how much of each subscription plan (ChatGPT, GitHub Copilot) is left, and when it starts over.
    Usage,
    /// Make a provider (and model) the default, in the user configuration.
    Use {
        /// Provider name (`nexo providers --all` lists them).
        provider: String,
        /// Model name (default: the provider's own default). A name nexo does not know is refused.
        model: Option<String>,
        /// Keep a model name that is not in the known list.
        #[arg(long)]
        force: bool,
    },
    /// Check the setup: files, the provider and model a run would use, the key, and a live call.
    Doctor {
        /// Skip the live call.
        #[arg(long)]
        offline: bool,
    },
    /// Print the setup guide (written so an assistant can follow it for you).
    Guide,
    /// List the recorded sessions, or rename one.
    Sessions {
        #[command(subcommand)]
        what: Option<SessionsCmd>,
    },
    /// Print the system prompt as it would be sent from this directory.
    Prompt {
        /// Open your preferences (~/.nexo/prompt.md, added after the built-in prompt) in $VISUAL or $EDITOR (else Notepad on Windows, nano or vi elsewhere), creating it on first use.
        #[arg(long)]
        edit: bool,
        /// Print the tool definitions as the model gets them (name, description, JSON parameters) instead of the prompt.
        #[arg(long, conflicts_with_all = ["edit", "builtin"])]
        tools: bool,
        /// Print only nexo's own part of the prompt: the built-in text and tool guidance, no instruction files or preferences.
        #[arg(long, conflicts_with_all = ["edit", "tools"])]
        builtin: bool,
        /// Print what a model with this context window (tokens) would get: the compact text at 32768 or less (and some tools left out).
        #[arg(long, value_name = "TOKENS", conflicts_with = "edit")]
        window: Option<u64>,
    },
    /// Show or set the prices of a model (per million tokens).
    Pricing {
        /// `provider/model` (default: the provider and model in use).
        model: Option<String>,
        /// Set prices: `--set in=0.28 out=0.42 cache=0.028 write=0` (or comma separated; keys left out keep their value).
        #[arg(long, value_name = "KEY=VALUE", num_args = 1.., value_delimiter = ',')]
        set: Vec<String>,
    },
    /// Show the configuration paths, or write a starter file.
    Config {
        #[command(subcommand)]
        what: Option<ConfigCmd>,
    },
    #[command(hide = true)]
    /// Print the briefing a sub-agent would get for a task from a conversation.
    Brief {
        /// A session id, or a file of messages (a JSON array, or one message per line).
        conversation: String,
        /// The task, as the `task` tool's prompt would give it.
        #[arg(required = true, num_args = 1..)]
        task: Vec<String>,
        /// The budget in tokens (default: `[agent] subagent_brief`).
        #[arg(long)]
        tokens: Option<usize>,
        /// auto | curator | lexical (default: `[agent] subagent_brief_engine`).
        #[arg(long)]
        engine: Option<String>,
        /// Do not compare the lines read with the files on disk.
        #[arg(long)]
        no_disk: bool,
        /// Print JSON: the text and its measures.
        #[arg(long)]
        json: bool,
    },
    /// Review the current diff for bugs, security and missing tests and print the findings; exits 1 on a high-severity one (for CI). Edits nothing.
    Review {
        /// Compare with the merge base of this branch (default: main or master).
        #[arg(long, value_name = "BRANCH", conflicts_with_all = ["staged", "commit"])]
        base: Option<String>,
        /// Review one commit.
        #[arg(long, value_name = "COMMIT", conflicts_with = "staged")]
        commit: Option<String>,
        /// Review only the staged changes.
        #[arg(long)]
        staged: bool,
        /// Print the findings as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add, list, remove and sign in to MCP servers (local commands and remote HTTP servers).
    Mcp {
        #[command(subcommand)]
        what: crate::mcp_cmd::McpCmd,
    },
    /// Write a session as a shareable file: HTML (one self-contained file), markdown or JSON, with keys and tokens taken out.
    Export(crate::export_cmd::ExportArgs),
    /// The local audit log of tool calls and model requests (`[audit]` in the configuration, see docs/AUDIT.md).
    Audit {
        #[command(subcommand)]
        what: crate::audit_cmd::AuditCmd,
    },
    /// Install, list, update and switch plugins (bundles of commands, agents, skills, hooks and MCP servers).
    Plugin {
        #[command(subcommand)]
        what: crate::plugin_cmd::PluginCmd,
    },
    /// Download the latest release binary from GitHub, building with cargo only if none fits this system (`--check`: only say whether there is one).
    Update {
        /// Only say whether a newer version exists; install nothing.
        #[arg(long)]
        check: bool,
    },
    /// Watch long-running sessions (run with no argument), answer their hooks, or print the hook settings to add.
    Supervise {
        #[command(subcommand)]
        what: Option<SuperviseCmd>,
    },
    /// List, remove or prune the git worktrees nexo made (see docs/WORKTREES.md).
    Worktree {
        #[command(subcommand)]
        what: crate::worktree::WorktreeCmd,
    },
    /// Read an ELF, PE or Mach-O file (or bare code with --raw) and answer in bounded pieces: info, fn, xref, calls, funcs, strings, bytes, find (the file is never run).
    Bin(Box<crate::bin_cmd::BinArgs>),
    /// Run a program under a debugger, or attach to one, and drive it one bounded call at a time: run, break, cont, step, regs, mem, bt, watch, kill (`nexo dbg help`).
    Dbg(crate::dbg_cmd::DbgArgs),
    /// Connect nexo to the Chrome you are logged in to, through an extension you accept once: writes the native-messaging manifest and opens Chrome on chrome://extensions.
    ChromeSetup(crate::chrome_setup_cmd::ChromeSetupArgs),
    /// nexo RE, the binary kit: no word prints the map of `nexo bin` (read a file) and `nexo dbg` (run it); a word runs that op or command.
    Re(crate::re_cmd::ReArgs),
    /// Run as an Agent Client Protocol server on stdin and stdout, for Zed, JetBrains and Neovim (see docs/ACP.md).
    Acp,
    /// Lanes that run on their own in the background (`<config dir>/team.toml`): start, status, stop, pause, hold, open.
    Team {
        #[command(subcommand)]
        what: crate::team::TeamCmd,
    },
    #[cfg(feature = "bench")]
    #[command(hide = true)]
    /// Measure the agent on the benchmark tasks with each context policy (see https://github.com/Locotir/nexo/blob/main/nexo/crates/nexo-bench/README.md).
    Bench(Box<crate::bench_cmd::BenchArgs>),
}

#[derive(Subcommand, Debug)]
pub enum SuperviseCmd {
    #[command(hide = true)]
    /// What a session's hook runs: reads the hook input on stdin, prints the answer (`{}` when no supervisor runs).
    Hook {
        #[arg(value_parser = ["stop", "permission", "notify"])]
        kind: String,
    },
    /// Print the hook settings to add to the sessions' settings file. Nothing is written.
    Install {
        /// Print the snippet (the only mode).
        #[arg(long)]
        print: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Log in with a subscription, or keep a key: `nexo auth login copilot`, `nexo auth login deepseek --key`.
    Login {
        /// Provider name (without one: the logins nexo can run).
        provider: Option<String>,
        /// Keep this key (`-` or nothing after it: read it from stdin or ask).
        #[arg(long, num_args = 0..=1, default_missing_value = "-")]
        key: Option<String>,
        /// The login flow when the name does not say it: copilot | codex | codex-browser | codex-cli | codex-device | xai.
        #[arg(long)]
        flow: Option<String>,
    },
    /// List what the store holds (never the secrets).
    List,
    /// Forget a provider's key or login.
    Logout { provider: String },
}

#[derive(Subcommand, Debug)]
pub enum SessionsCmd {
    /// List the recorded sessions (the default).
    List,
    /// Give a session a name (shown by `nexo sessions` and the /resume picker).
    Rename { id: String, title: Vec<String> },
    /// Move sessions to the trash (`nexo sessions restore` brings them back).
    Delete { ids: Vec<String> },
    /// Bring sessions back from the trash.
    Restore { ids: Vec<String> },
    /// List the sessions that are live now, with their running jobs (a session is live while its heartbeat is fresh).
    ///
    /// `--json` prints an array with one object per live session and the keys `id`, `title`, `provider`,
    /// `model` (both null when the session has no file yet), `cwd`, `state` ("busy", "idle" or "unknown"),
    /// `pid`, `started_ms`, `last_beat_ms`, `beat_age_ms` and `jobs`: an array of `id`, `kind` ("shell" or
    /// "agent"), `state` ("running") and `title`. Lanes of `nexo team` are live sessions like any other.
    /// Messages, prompts and tool output are never printed.
    Live {
        /// Print JSON (for scripts and dashboards).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the paths of the user and project configuration files.
    Path,
    /// Write a commented starter configuration for the user (or `--project` for this directory).
    Init {
        #[arg(long)]
        project: bool,
    },
    /// Trust a project (default: this directory's): its own config may then set provider addresses and
    /// keys, MCP servers, hooks, allow rules and commands, which are ignored until then.
    Trust { dir: Option<PathBuf> },
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_bare_word_is_the_first_request_and_a_subcommand_stays_one() {
        use clap::Parser;
        let a = super::Args::try_parse_from(["nexo", "fix the failing test"]).unwrap();
        assert_eq!(a.first.as_deref(), Some("fix the failing test"));
        assert!(a.cmd.is_none());
        let a = super::Args::try_parse_from(["nexo", "guide"]).unwrap();
        assert!(matches!(a.cmd, Some(super::Cmd::Guide)));
        assert!(a.first.is_none());
        let a = super::Args::try_parse_from(["nexo", "-c", "--model", "m", "go on"]).unwrap();
        assert_eq!(a.first.as_deref(), Some("go on"));
        assert!(a.continue_);
        let a = super::Args::try_parse_from(["nexo", "-i", "a.png", "--image", "b.png", "what changed?"]).unwrap();
        assert_eq!(a.headless.image, vec![std::path::PathBuf::from("a.png"), std::path::PathBuf::from("b.png")]);
        assert_eq!(a.first.as_deref(), Some("what changed?"));
    }

    #[test]
    fn bench_subcommand_follows_the_feature() {
        use clap::Parser;
        let a = super::Args::try_parse_from(["nexo", "bench"]).unwrap();
        let is_bench = format!("{:?}", a.cmd).starts_with("Some(Bench(");
        assert_eq!(is_bench, cfg!(feature = "bench"));
    }
}
