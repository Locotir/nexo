//! The command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::permission::PermissionMode;

use crate::setup::{self, Picked};

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
    /// ask | accept-edits | plan | auto | read-only
    #[arg(long, value_name = "MODE", env = "NEXO_PERMISSION_MODE", value_parser = permission_mode_arg)]
    pub permission_mode: Option<String>,
    /// Never ask for permission (the same as --permission-mode auto).
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
        None => Err(format!("unknown permission mode {s:?}; use one of: ask, accept-edits, plan, auto, read-only")),
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
        /// Print what a model with this context window (tokens) would get: the compact text at 32000 or less.
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
    /// Run as an Agent Client Protocol server on stdin and stdout, for Zed, JetBrains and Neovim (see docs/ACP.md).
    Acp,
    #[cfg(feature = "bench")]
    #[command(hide = true)]
    /// Measure the agent on the benchmark tasks with each context policy (see crates/nexo-bench/README.md).
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

pub async fn main() -> i32 {
    let args = Args::parse();
    if let Some(f) = &args.log {
        match std::fs::File::create(f) {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
            }
            Err(e) => eprintln!("nexo: cannot open log file {}: {e}", f.display()),
        }
    }
    if let Some(Cmd::Supervise { what }) = &args.cmd {
        return match what {
            Some(SuperviseCmd::Hook { kind }) => crate::supervise::hook(kind),
            Some(SuperviseCmd::Install { print: true }) => crate::supervise::install_print(),
            Some(SuperviseCmd::Install { print: false }) => {
                eprintln!("nexo: nothing is written; use `nexo supervise install --print` and add the text yourself");
                2
            }
            None => match crate::supervise::daemon().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("nexo: {e:#}");
                    1
                }
            },
        };
    }
    match run(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("nexo: {e:#}");
            1
        }
    }
}

async fn run(args: Args) -> anyhow::Result<i32> {
    let cwd = match &args.cwd {
        Some(d) => std::fs::canonicalize(d)?,
        None => std::env::current_dir()?,
    };
    let made = crate::worktree::enter(&args, &cwd)?;
    let code = run_in(args, made.as_ref().map_or(cwd, |w| w.path.clone())).await;
    if let Some(w) = &made {
        crate::worktree::leave(w);
    }
    code
}

/// The configuration of `cwd` with the presets and the providers the person asked for.
fn load_config(args: &Args, cwd: &std::path::Path) -> anyhow::Result<nexo_core::Config> {
    let mut config = nexo_core::Config::load(cwd)?;
    setup::apply_presets(&mut config);
    // a provider asked for by a name nexo knows works without a block in the file
    for p in [args.provider.clone(), config.provider.clone()].into_iter().flatten() {
        crate::account::adopt(&mut config, &p);
    }
    config.name_auth_entries();
    // the presets and the providers just adopted come under the managed policy too
    config.enforce_managed();
    Ok(config)
}

/// Before the first prompt of the interface: a folder not trusted yet that holds project hooks,
/// servers or commands is asked about, and a yes is recorded as `nexo config trust` records it.
/// True when it was: the configuration is then read again, with the project's own files in it.
/// A no, a headless run (`-p`), a subcommand and a start without a terminal ask nothing; the
/// notice the loading prints stays. The managed policy leaves out what it forbids from the question.
async fn trust_asked(args: &Args, cwd: &std::path::Path, config: &nexo_core::Config) -> bool {
    use std::io::IsTerminal;
    let tty = (std::io::stdin().is_terminal(), std::io::stdout().is_terminal());
    if !nexo_core::trust::asks(args.prompt.is_some(), args.cmd.is_some(), tty.0, tty.1) {
        return false;
    }
    let Some(gate) = nexo_core::trust::gate_here(cwd, config.managed.as_deref()) else { return false };
    let ask =
        nexo_tui::TrustAsk { path: gate.root.display().to_string(), enables: gate.enables(), warning: gate.warning() };
    let opts = nexo_tui::TuiOptions {
        cwd: cwd.to_path_buf(),
        theme: args.theme.clone().unwrap_or_else(|| config.ui.theme.clone()),
        mouse: config.ui.mouse && !args.no_mouse,
        animations: config.ui.animations,
        ..Default::default()
    };
    match nexo_tui::ask_trust(ask, opts).await {
        Ok(true) => match gate.record(&nexo_core::paths::trusted_projects_file()) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("nexo: cannot record the trust of {}: {e}", gate.root.display());
                false
            }
        },
        Ok(false) => false,
        Err(e) => {
            eprintln!("nexo: cannot ask about trusting {}: {e:#}", gate.root.display());
            false
        }
    }
}

async fn run_in(args: Args, cwd: PathBuf) -> anyhow::Result<i32> {
    let mut config = load_config(&args, &cwd)?;
    if trust_asked(&args, &cwd, &config).await {
        config = load_config(&args, &cwd)?;
    }
    // what the policy forbids is refused when the person asks for it on the command line, not changed quietly
    if let Some(m) = &args.permission_mode {
        config.permissions.mode =
            PermissionMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown permission mode {m:?}"))?;
        config.check_mode(config.permissions.mode).map_err(|e| anyhow::anyhow!(e))?;
    }
    if args.yolo {
        config.check_mode(PermissionMode::Auto).map_err(|e| anyhow::anyhow!(e))?;
        config.permissions.mode = PermissionMode::Auto;
    }
    if let Some(m) = &args.sandbox {
        config.permissions.sandbox =
            nexo_core::sandbox::SandboxMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown sandbox mode {m:?}"))?;
        if let Some(p) = &config.managed {
            p.check_sandbox(config.permissions.sandbox).map_err(|e| anyhow::anyhow!(e))?;
        }
    }
    if let Some(c) = &args.context {
        config.context.policy = c.clone();
    }
    if let Some(s) = &args.curator_spec {
        config.context.curator_spec = s.clone();
    }
    if let Some(t) = &args.theme {
        config.ui.theme = t.clone();
    }
    if args.no_mouse {
        config.ui.mouse = false;
    }
    if args.no_alt_screen {
        std::env::set_var("NEXO_NO_ALT_SCREEN", "1");
    }
    if args.inline || config.ui.screen == "inline" {
        std::env::set_var("NEXO_INLINE", "1");
    }

    match &args.cmd {
        // the benchmark sets up its own provider and keeps away from the user configuration
        #[cfg(feature = "bench")]
        Some(Cmd::Bench(b)) => return crate::bench_cmd::run(b).await,
        Some(Cmd::Acp) => return crate::acp::run(&args).await,
        Some(Cmd::Providers { all, json }) => return Ok(setup::print_providers(&config, *all, *json)),
        Some(Cmd::Models { provider, refresh, json }) => {
            let p = provider.as_deref().or(args.provider.as_deref());
            if let Some(p) = p {
                crate::account::adopt(&mut config, p);
            }
            return setup::print_models(&config, p, *refresh, *json).await;
        }
        Some(Cmd::Auth { what: AuthCmd::Login { provider, key, flow } }) => {
            return crate::account::login_cmd(&mut config, provider.as_deref(), key.as_deref(), flow.as_deref()).await
        }
        Some(Cmd::Auth { what: AuthCmd::List }) => return Ok(crate::account::list_cmd()),
        Some(Cmd::Auth { what: AuthCmd::Logout { provider } }) => return crate::account::logout_cmd(&config, provider),
        Some(Cmd::Usage) => return crate::account::usage_cmd(&mut config).await,
        Some(Cmd::Use { provider, model, force }) => {
            return crate::account::use_cmd(&mut config, provider, model.as_deref(), *force)
        }
        Some(Cmd::Doctor { offline }) => {
            if let Some(p) = &args.provider {
                config.provider = Some(p.clone());
            }
            if let Some(m) = &args.model {
                config.model = Some(m.clone());
            }
            return crate::account::doctor(&cwd, &config, *offline).await;
        }
        Some(Cmd::Mcp { what }) => return crate::mcp_cmd::run(&cwd, &config, what).await,
        Some(Cmd::Worktree { what }) => return crate::worktree::run(&cwd, what),
        Some(Cmd::Export(a)) => return crate::export_cmd::run(a, &cwd),
        Some(Cmd::Audit { what }) => return crate::audit_cmd::run(&config, what),
        Some(Cmd::Plugin { what }) => return crate::plugin_cmd::run(&cwd, &config, what),
        Some(Cmd::Update { check }) => return crate::update::run(*check).await,
        Some(Cmd::Guide) => {
            print!("{}", crate::account::GUIDE);
            return Ok(0);
        }
        Some(Cmd::Sessions { what: None | Some(SessionsCmd::List) }) => return Ok(setup::print_sessions()),
        Some(Cmd::Sessions { what: Some(SessionsCmd::Rename { id, title }) }) => {
            return setup::rename_session(id, &title.join(" "));
        }
        Some(Cmd::Sessions { what: Some(SessionsCmd::Delete { ids }) }) => {
            return Ok(setup::trash_sessions(ids, false))
        }
        Some(Cmd::Sessions { what: Some(SessionsCmd::Restore { ids }) }) => {
            return Ok(setup::trash_sessions(ids, true))
        }
        Some(Cmd::Prompt { edit: false, tools: show_tools, builtin, window }) => {
            let tools = setup::prompt_tools(&config, &cwd);
            // what the session would get: `auto` is the built-in curator, told which tools there are
            let specs = if nexo_agent::prompt::is_compact(*window) { tools.compact_specs() } else { tools.specs() };
            let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
            let policy = nexo_agent::context::from_config(&config.context);
            let directives = policy.directives(&names);
            if *show_tools {
                // as `turn::offered` hands them out: `exit_plan` only in plan mode, `goal_brief` only with a goal,
                // `recall` only when the policy offers it
                let specs: Vec<_> = specs
                    .into_iter()
                    .filter(|s| {
                        !matches!(s.name.as_str(), "exit_plan" | "goal_brief")
                            && (policy.recall() || s.name != "recall")
                    })
                    .collect();
                println!("{}", nexo_agent::prompt::tool_definitions_for(&specs, *window));
            } else if *builtin {
                println!("{}", nexo_agent::prompt::builtin_part_for(&tools, directives, *window));
            } else {
                let base = nexo_agent::prompt::system_prompt_for(&cwd, &config.agent, &tools, *window);
                println!("{}", nexo_agent::prompt::with_directives(base, &config.agent, directives));
            }
            return Ok(0);
        }
        Some(Cmd::Prompt { edit: true, .. }) => return setup::edit_prompt(&config),
        Some(Cmd::Pricing { model, set }) => {
            let set = if set.is_empty() { None } else { Some(set.join(",")) };
            return setup::pricing_cmd(
                &config,
                model.as_deref(),
                args.provider.as_deref(),
                args.model.as_deref(),
                set.as_deref(),
            )
            .await;
        }
        Some(Cmd::Config { what }) => return setup::config_cmd(&cwd, what.as_ref()),
        Some(Cmd::Brief { conversation, task, tokens, engine, no_disk, json }) => {
            let mut cfg = nexo_agent::brief::BriefConfig::from_agent(&config.agent);
            if let Some(t) = tokens {
                cfg.tokens = *t;
            }
            if let Some(e) = engine {
                cfg.engine = nexo_agent::brief::Engine::parse(e);
            }
            cfg.check_disk = !no_disk;
            return setup::brief_cmd(&cwd, conversation, &task.join(" "), &cfg, *json).await;
        }
        Some(Cmd::Review { .. }) => {}
        None => {}
        // handled before the configuration is read
        Some(Cmd::Supervise { .. }) => return Ok(0),
    }

    if args.prompt.is_none() && args.headless.any() {
        eprintln!("nexo: --output-format, --input-format, --max-turns, --allowed-tools, --disallowed-tools, --append-system-prompt and --json-schema need --print (-p)");
        return Ok(crate::headless::EXIT_USAGE);
    }
    // plain `nexo` with nothing set up opens on the locked home; a run that cannot ask says what to do
    let unset = args.provider.is_none() && setup::nothing_chosen(&config);
    if unset && args.prompt.is_some() {
        anyhow::bail!(setup::nothing_chosen_message());
    }
    let picked: Picked = if unset {
        setup::placeholder(&config)
    } else {
        setup::pick(&config, args.provider.as_deref(), args.model.as_deref())?
    };
    let resume = if args.continue_ { nexo_agent::session::latest_for(&cwd).map(|m| m.id) } else { args.resume.clone() };
    if args.continue_ && resume.is_none() {
        eprintln!("nexo: no earlier session in {}; starting a new one", cwd.display());
    }

    if let Some(Cmd::Review { base, commit, staged, json }) = &args.cmd {
        use nexo_agent::gitcmds as g;
        let target = match (staged, commit, base) {
            (true, _, _) => g::Target::Staged,
            (_, Some(c), _) => g::Target::Commit(c.clone()),
            (_, _, Some(b)) => g::Target::Base(b.clone()),
            _ => g::Target::Default,
        };
        let prompt = match g::review_prompt_json(&cwd, &target) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("nexo review: {e}");
                return Ok(2);
            }
        };
        // a review reads and never writes
        config.permissions.mode = PermissionMode::ReadOnly;
        let (text, code) = headless_collect(config, cwd, picked, prompt).await?;
        if code != 0 {
            eprintln!("nexo review: the model did not finish the review");
            return Ok(code);
        }
        return Ok(match g::parse_findings(&text) {
            Ok(f) => {
                if *json {
                    println!("{}", g::findings_json(&f));
                } else {
                    print!("{}", g::render_findings(&f));
                }
                i32::from(g::has_high(&f))
            }
            Err(e) => {
                eprintln!("nexo review: {e}; the reply was:\n{text}");
                3
            }
        });
    }

    if let Some(prompt) = args.prompt.clone() {
        // `nexo "question" -p`: the question is the request
        let prompt = if prompt.is_empty() { args.first.clone().unwrap_or_default() } else { prompt };
        return crate::headless::run(config, cwd, picked, resume, prompt, args.json, args.headless).await;
    }
    if !args.headless.add_dir.is_empty() {
        let roots = match crate::headless::add_dirs(&mut config, &cwd, &args.headless.add_dir) {
            Ok(r) => r,
            Err(m) => {
                eprintln!("nexo: {m}");
                return Ok(crate::headless::EXIT_USAGE);
            }
        };
        config.enforce_managed();
        let note = format!("You may also read and write in these folders: {}.", roots.join(", "));
        config.agent.system_prompt_extra = Some(match config.agent.system_prompt_extra.take() {
            Some(e) if !e.trim().is_empty() => format!("{}\n\n{note}", e.trim_end()),
            _ => note,
        });
    }
    let first = match crate::headless::images(&cwd, &args.headless.image) {
        Ok(i) => crate::headless::with_images(args.first.clone(), &i),
        Err(m) => {
            eprintln!("nexo: {m}");
            return Ok(crate::headless::EXIT_USAGE);
        }
    };
    interactive(config, cwd, picked, resume, unset, first).await
}

async fn interactive(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    resume: Option<String>,
    unset: bool,
    first: Option<String>,
) -> anyhow::Result<i32> {
    let (handle, ends) = channels();
    let models = setup::model_choices(&config);
    // A session nothing was said in has nothing to resume.
    let sessions = nexo_agent::session::list()
        .into_iter()
        .filter(|m| m.messages > 0 && !m.headless)
        .map(|m| (m.id, m.title))
        .collect();
    // sessions write to each other through the mailbox; it takes its session when the agent has one
    let mailbox = config.mail.enabled.then(|| {
        nexo_core::mailbox::Mailbox::unbound(
            &nexo_core::mailbox::mail_root(),
            &cwd.display().to_string(),
            false,
            nexo_core::mail::Inbound::parse(&config.mail.inbound),
        )
    });
    let agent_setup = setup::agent_setup(&config, &cwd, &picked, resume.clone(), None, mailbox.clone()).await?;
    let opts = nexo_tui::TuiOptions {
        cwd: cwd.clone(),
        // nothing chosen: the interface names none and opens on the locked home
        provider: if unset { String::new() } else { picked.provider.clone() },
        model: if unset { String::new() } else { picked.model.clone() },
        models,
        providers: config.providers.iter().map(|(n, c)| (n.clone(), c.clone())).collect(),
        theme: config.ui.theme.clone(),
        mouse: config.ui.mouse,
        show_reasoning: config.ui.show_reasoning,
        timestamps: config.ui.timestamps,
        fold_after: config.ui.fold_after,
        group_tools: config.ui.group_tools,
        complete_words: config.ui.complete_words,
        suggest: config.ui.suggest,
        keys: config.ui.keys.clone(),
        animations: config.ui.animations,
        status_line: config.ui.status_line.clone(),
        status_command: config.ui.status_command.clone(),
        notify_after: config.ui.notify_after,
        vim: config.ui.vim,
        prompt: first,
        session_id: resume,
        sessions,
        skills: nexo_core::skills::discover(&cwd, !config.agent.hermetic),
    };
    if !crate::update::off(&config) {
        let events = ends.events.clone();
        // at start, then every hour while nexo runs (two GitHub requests a check, well under its limit
        // of 60 an hour without a key); the interface says each time more commits are out
        tokio::spawn(async move {
            loop {
                if let Ok(Some(behind)) = crate::update::behind().await {
                    if events.send(AgentEvent::UpdateAvailable { behind }).is_err() {
                        return;
                    }
                }
                tokio::time::sleep(crate::update::EVERY).await;
            }
        });
    }
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    nexo_tui::set_supervise_options(nexo_agent::supervise::Options::detect());
    if let Some(m) = mailbox {
        nexo_tui::set_mailbox(m);
    }
    let ui = nexo_tui::run(handle, opts).await;
    agent.abort();
    ui?;
    Ok(0)
}

/// Runs one request without the interface and returns the reply text and the exit code (0 done, 2 not done, 1 agent failure).
async fn headless_collect(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    prompt: String,
) -> anyhow::Result<(String, i32)> {
    nexo_agent::session::mark_headless();
    let (mut handle, ends) = channels();
    let gate: std::sync::Arc<dyn nexo_core::PermissionGate> = std::sync::Arc::new(setup::HeadlessGate);
    let agent_setup = setup::agent_setup(&config, &cwd, &picked, None, Some(gate), None).await?;
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    let sent = handle.commands.send(AgentCommand::Submit { text: prompt, attachments: vec![] }).is_ok();
    let (mut text, mut code) = (String::new(), 0);
    while let Some(ev) = handle.events.recv().await {
        match &ev {
            AgentEvent::TextDelta { text: t } => text.push_str(t),
            AgentEvent::AssistantStart if !text.is_empty() => text.clear(),
            AgentEvent::TurnEnd { reason } => {
                code = if matches!(reason, nexo_core::agent::TurnEnd::Done) { 0 } else { 2 };
            }
            AgentEvent::Idle { .. } | AgentEvent::Shutdown => break,
            _ => {}
        }
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    let failed = match tokio::time::timeout(std::time::Duration::from_secs(2), agent).await {
        Ok(Ok(Err(e))) => Some(format!("{e:#}")),
        Ok(Err(e)) => Some(format!("the agent stopped: {e}")),
        _ if !sent => Some("the agent stopped before the request".to_string()),
        _ => None,
    };
    if let Some(e) = failed {
        eprintln!("nexo: {e}");
        return Ok((text, 1));
    }
    Ok((text, code))
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
