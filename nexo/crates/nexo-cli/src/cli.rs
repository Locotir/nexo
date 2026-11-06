//! The command line.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use nexo_core::agent::{channels, AgentCommand, AgentEvent};
use nexo_core::permission::PermissionMode;

use crate::setup::{self, Picked};

#[derive(Parser, Debug)]
#[command(name = "nexo", version = nexo_bench::run::VERSION, about = "A terminal coding harness for any model provider.", long_about = None)]
pub struct Args {
    /// Run one request without the interface and print the reply.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub prompt: Option<String>,
    /// With --print: print every agent event as a JSON line instead of the text.
    #[arg(long)]
    pub json: bool,
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
    #[arg(long, value_name = "MODE", env = "NEXO_PERMISSION_MODE")]
    pub permission_mode: Option<String>,
    /// Never ask for permission (the same as --permission-mode auto).
    #[arg(long, conflicts_with = "permission_mode")]
    pub yolo: bool,
    /// Context policy: auto | plain | curator | compact (auto is the curator, built in)
    #[arg(long, value_name = "POLICY", env = "NEXO_CONTEXT")]
    pub context: Option<String>,
    /// Curator settings for this run, over [context] curator_spec (`eoq=32000;dedup=0`)
    #[arg(long, value_name = "SPEC", env = "NEXO_CURATOR_SPEC")]
    pub curator_spec: Option<String>,
    /// Theme: dark, nexo, light, mono, catppuccin-mocha, gruvbox, nord, dracula, solarized-dark, tokyo-night
    #[arg(long)]
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
    /// Log to this file (RUST_LOG filters).
    #[arg(long, value_name = "FILE")]
    pub log: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
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
    /// Make a provider (and model) the default, in the user configuration.
    Use { provider: String, model: Option<String> },
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
    /// Install the latest nexo from GitHub with cargo (`--check`: only say whether there is one).
    Update {
        #[arg(long)]
        check: bool,
    },
    /// Watch long-running sessions (run with no argument), answer their hooks, or print the hook settings to add.
    Supervise {
        #[command(subcommand)]
        what: Option<SuperviseCmd>,
    },
    /// Measure the agent on the benchmark tasks with each context policy (see crates/nexo-bench/README.md).
    Bench(Box<BenchArgs>),
}

#[derive(Subcommand, Debug)]
pub enum SuperviseCmd {
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

/// `nexo bench`: every task done by the agent with every arm, graded, priced and summed up.
#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    #[command(subcommand)]
    pub action: Option<BenchCmd>,
    /// The provider's address, e.g. https://api.deepseek.com/anthropic (needed to run).
    #[arg(long, value_name = "URL")]
    pub upstream: Option<String>,
    /// The model, e.g. deepseek-chat (needed to run).
    #[arg(long)]
    pub model: Option<String>,
    /// The API key (none for a local server; `-` reads it from stdin).
    #[arg(long, value_name = "KEY")]
    pub key: Option<String>,
    /// anthropic | openai-chat | openai-responses | gemini (default: guessed from the address).
    #[arg(long)]
    pub protocol: Option<String>,
    /// Task ids or groups (all, core, long, heavy, more, hard, padded), the external benchmark's repoqa and canonstep (with --llmb), or polyglot (Aider's exercises, see --polyglot),
    /// comma separated or repeated.
    #[arg(long, value_name = "TASKS", value_delimiter = ',', default_value = "all")]
    pub tasks: Vec<String>,
    /// Arms `[NAME@]POLICY[:k=v;...]` (POLICY: plain, compact, curator; the pairs are the curator spec),
    /// comma separated or repeated.
    #[arg(long, value_name = "ARMS", value_delimiter = ',', default_value = nexo_bench::arm::DEFAULT_ARMS)]
    pub arms: Vec<String>,
    /// Times each task is run with each arm.
    #[arg(long, default_value_t = 1)]
    pub repeats: u32,
    /// Most tool steps (model requests) a task may take.
    #[arg(long, default_value_t = 100)]
    pub max_steps: u32,
    /// Stop the whole benchmark once this many dollars are spent (needs prices: nexo's own or --price).
    #[arg(long, value_name = "USD")]
    pub budget: Option<f64>,
    /// The model's prices in dollars per million tokens: input on a cache miss, on a cache hit, output
    /// (default: nexo's own for the model, if it has any).
    #[arg(long, value_name = "MISS,HIT,OUT", value_parser = price_arg)]
    pub price: Option<nexo_core::Pricing>,
    /// Largest reply, in tokens.
    #[arg(long)]
    pub max_tokens: Option<u32>,
    /// Sampling temperature of every request (default: 0 with the task polyglot, as Aider; the provider's own otherwise).
    #[arg(long)]
    pub temperature: Option<f32>,
    /// The model's context window in tokens (default: what nexo knows of it), e.g. 8000 for a small model.
    #[arg(long)]
    pub window: Option<u32>,
    /// Where results.jsonl, the transcripts and summary.md go.
    #[arg(long, value_name = "DIR", default_value = "./bench-out")]
    pub out: PathBuf,
    /// Start without the one small request that checks the key and the model's tool calls.
    #[arg(long)]
    pub skip_preflight: bool,
    /// The external benchmark's distribution folder (for --tasks repoqa,canonstep).
    #[arg(long, value_name = "DIR")]
    pub llmb: Option<PathBuf>,
    /// The Polyglot benchmark checkout (for --tasks polyglot; default: the pinned commit is cloned into the cache).
    #[arg(long, value_name = "DIR")]
    pub polyglot: Option<PathBuf>,
    /// Languages of polyglot, comma separated (default: python,rust,go,cpp,javascript,java).
    #[arg(long, value_name = "LANGS", value_delimiter = ',')]
    pub langs: Vec<String>,
    /// Instance ids of repoqa, canonstep and polyglot (lang/name), comma separated (default: all of them).
    #[arg(long, value_name = "IDS", value_delimiter = ',')]
    pub instances: Vec<String>,
    /// At most this many instances per task (per language for polyglot, in a fixed shuffled order), in id order otherwise.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Where repoqa's repositories, polyglot's checkout and its package caches are kept once (default: <out>/.cache/repos).
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
}

/// `nexo bench` subcommands.
#[derive(Subcommand, Debug)]
pub enum BenchCmd {
    /// Write report.html in --out from one or more results.jsonl (files or folders; default: --out).
    Report {
        #[arg(long, value_name = "DIR", default_value = "./bench-out")]
        out: PathBuf,
        /// results.jsonl files, or folders holding one.
        results: Vec<PathBuf>,
        /// Also write the Markdown summary here, with the counters read again from the transcripts.
        #[arg(long, value_name = "FILE")]
        summary: Option<PathBuf>,
    },
}

impl BenchArgs {
    fn options(&self) -> anyhow::Result<nexo_bench::Options> {
        let (Some(upstream), Some(model)) = (self.upstream.clone(), self.model.clone()) else {
            anyhow::bail!("nexo bench needs --upstream and --model");
        };
        Ok(nexo_bench::Options {
            upstream,
            model,
            key: key_arg(self.key.as_deref(), std::io::stdin().lock())?,
            protocol: self.protocol.clone(),
            tasks: self.tasks.clone(),
            arms: self.arms.clone(),
            repeats: self.repeats,
            max_steps: self.max_steps,
            budget: self.budget,
            price: self.price,
            max_tokens: self.max_tokens,
            window: self.window,
            temperature: self.temperature,
            out: self.out.clone(),
            skip_preflight: self.skip_preflight,
            llmb: self.llmb.clone(),
            polyglot: self.polyglot.clone(),
            langs: self.langs.clone(),
            instances: self.instances.clone(),
            limit: self.limit,
            cache: self.cache.clone(),
        })
    }
}

/// `--price MISS,HIT,OUT`: dollars per million tokens of input on a cache miss, input on a cache hit, output.
fn price_arg(s: &str) -> Result<nexo_core::Pricing, String> {
    let n: Vec<f64> =
        s.split(',').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    match n[..] {
        [input, cache_read, output] if n.iter().all(|x| x.is_finite() && *x >= 0.0) && input + output > 0.0 => {
            Ok(nexo_core::Pricing { input, output, cache_read, cache_write: 0.0 })
        }
        _ => Err("three prices, not all zero: MISS,HIT,OUT in dollars per million tokens, e.g. 0.28,0.028,0.42".into()),
    }
}

/// The key as given, or read from `input` when it is `-`: then it stays out of the process list and the shell
/// history.
fn key_arg(key: Option<&str>, mut input: impl std::io::Read) -> anyhow::Result<Option<String>> {
    match key {
        Some("-") => {
            let mut all = String::new();
            input.read_to_string(&mut all)?;
            let k = all.trim();
            anyhow::ensure!(!k.is_empty(), "--key -: nothing on stdin");
            Ok(Some(k.to_string()))
        }
        k => Ok(k.map(str::to_string)),
    }
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
    let mut config = nexo_core::Config::load(&cwd)?;
    setup::apply_presets(&mut config);
    // a provider asked for by a name nexo knows works without a block in the file
    for p in [args.provider.clone(), config.provider.clone()].into_iter().flatten() {
        crate::account::adopt(&mut config, &p);
    }
    config.name_auth_entries();
    if let Some(m) = &args.permission_mode {
        config.permissions.mode =
            PermissionMode::parse(m).ok_or_else(|| anyhow::anyhow!("unknown permission mode {m:?}"))?;
    }
    if args.yolo {
        config.permissions.mode = PermissionMode::Auto;
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
        Some(Cmd::Bench(b)) => {
            return match &b.action {
                Some(BenchCmd::Report { out, results, summary }) => {
                    nexo_bench::report(out, results, summary.as_deref())
                }
                None => nexo_bench::run(b.options()?).await,
            }
        }
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
        Some(Cmd::Use { provider, model }) => return crate::account::use_cmd(&mut config, provider, model.as_deref()),
        Some(Cmd::Doctor { offline }) => {
            if let Some(p) = &args.provider {
                config.provider = Some(p.clone());
            }
            if let Some(m) = &args.model {
                config.model = Some(m.clone());
            }
            return crate::account::doctor(&cwd, &config, *offline).await;
        }
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
        Some(Cmd::Prompt { edit: false }) => {
            let tools = nexo_tools::builtin_with(&config.web);
            let base = nexo_agent::prompt::system_prompt(&cwd, &config.agent, &tools);
            // what the session would get: `auto` is the built-in curator, told which tools there are
            let specs = tools.specs();
            let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
            let directives = nexo_agent::context::from_config(&config.context).directives(&names);
            println!("{}", nexo_agent::prompt::with_directives(base, &config.agent, directives));
            return Ok(0);
        }
        Some(Cmd::Prompt { edit: true }) => return setup::edit_prompt(&config),
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
        None => {}
        // handled before the configuration is read
        Some(Cmd::Supervise { .. }) => return Ok(0),
    }

    let picked: Picked = setup::pick(&config, args.provider.as_deref(), args.model.as_deref())?;
    let resume = if args.continue_ { nexo_agent::session::latest_for(&cwd).map(|m| m.id) } else { args.resume.clone() };
    if args.continue_ && resume.is_none() {
        eprintln!("nexo: no earlier session in {}; starting a new one", cwd.display());
    }

    if let Some(prompt) = args.prompt.clone() {
        return headless(config, cwd, picked, resume, prompt, args.json).await;
    }
    interactive(config, cwd, picked, resume).await
}

async fn interactive(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    resume: Option<String>,
) -> anyhow::Result<i32> {
    let (handle, ends) = channels();
    let models = setup::model_choices(&config);
    // A session nothing was said in has nothing to resume.
    let sessions =
        nexo_agent::session::list().into_iter().filter(|m| m.messages > 0).map(|m| (m.id, m.title)).collect();
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
        provider: picked.provider.clone(),
        model: picked.model.clone(),
        models,
        providers: config.providers.iter().map(|(n, c)| (n.clone(), c.clone())).collect(),
        theme: config.ui.theme.clone(),
        mouse: config.ui.mouse,
        show_reasoning: config.ui.show_reasoning,
        timestamps: config.ui.timestamps,
        fold_after: config.ui.fold_after,
        group_tools: config.ui.group_tools,
        complete_words: config.ui.complete_words,
        keys: config.ui.keys.clone(),
        animations: config.ui.animations,
        status_line: config.ui.status_line.clone(),
        status_command: config.ui.status_command.clone(),
        notify_after: config.ui.notify_after,
        vim: config.ui.vim,
        session_id: resume,
        sessions,
    };
    if !crate::update::off(&config) {
        let events = ends.events.clone();
        tokio::spawn(async move {
            if let Ok(Some(behind)) = crate::update::behind().await {
                let _ = events.send(AgentEvent::UpdateAvailable { behind });
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

async fn headless(
    config: nexo_core::Config,
    cwd: PathBuf,
    picked: Picked,
    resume: Option<String>,
    prompt: String,
    json: bool,
) -> anyhow::Result<i32> {
    use std::io::Write;
    let (mut handle, ends) = channels();
    let gate: std::sync::Arc<dyn nexo_core::PermissionGate> = std::sync::Arc::new(setup::HeadlessGate);
    let agent_setup = setup::agent_setup(&config, &cwd, &picked, resume, Some(gate), None).await?;
    let agent = tokio::spawn(nexo_agent::run(ends, agent_setup));
    // an agent that already stopped (a session that cannot be resumed) closed its end: its own error says why
    let sent = handle.commands.send(AgentCommand::Submit { text: prompt, attachments: vec![] }).is_ok();
    let mut out = std::io::stdout().lock();
    let mut code = 0;
    // the text printed so far ends in the middle of a line
    let mut printed = false;
    while let Some(ev) = handle.events.recv().await {
        if json {
            writeln!(out, "{}", serde_json::to_string(&ev)?)?;
        }
        match &ev {
            AgentEvent::TextDelta { text } if !json => {
                out.write_all(text.as_bytes())?;
                out.flush()?;
                printed = !text.ends_with('\n');
            }
            // the next message starts on a line of its own
            AgentEvent::AssistantStart if !json && printed => {
                writeln!(out)?;
                printed = false;
            }
            AgentEvent::ToolCall { summary, .. } if !json => eprintln!("· {summary}"),
            AgentEvent::Notice { level, text } if !json => eprintln!("[{level:?}] {text}"),
            AgentEvent::TurnEnd { reason } => {
                if !json && printed {
                    writeln!(out)?;
                }
                code = if matches!(reason, nexo_core::agent::TurnEnd::Done) { 0 } else { 2 };
                printed = false;
            }
            // a job that ended may start another turn; the run is over when none does
            AgentEvent::Idle { .. } => break,
            AgentEvent::Shutdown => break,
            _ => {}
        }
    }
    let _ = handle.commands.send(AgentCommand::Shutdown);
    // the agent can stop before any turn (a session that cannot be resumed): that is a failure
    let failed = match tokio::time::timeout(std::time::Duration::from_secs(2), agent).await {
        Ok(Ok(Err(e))) => Some(format!("{e:#}")),
        Ok(Err(e)) => Some(format!("the agent stopped: {e}")),
        _ if !sent => Some("the agent stopped before the request".to_string()),
        _ => None,
    };
    if let Some(e) = failed {
        if json {
            writeln!(out, "{}", serde_json::json!({"event": "error", "message": e}))?;
        }
        out.flush()?;
        eprintln!("nexo: {e}");
        return Ok(1);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::{key_arg, price_arg};

    #[test]
    fn bench_key_dash_reads_stdin() {
        let key = |arg, input: &str| key_arg(arg, input.as_bytes()).map_err(|e| e.to_string());
        assert_eq!(key(Some("-"), "k-123\n"), Ok(Some("k-123".to_string())));
        assert_eq!(key(Some("k-456"), "ignored"), Ok(Some("k-456".to_string())));
        assert_eq!(key(None, "ignored"), Ok(None));
        assert_eq!(key(Some("-"), " \n"), Err("--key -: nothing on stdin".to_string()));
    }

    #[test]
    fn bench_price_reads_miss_hit_out() {
        let p = price_arg("0.28, 0.028,0.42").unwrap();
        assert_eq!((p.input, p.cache_read, p.output, p.cache_write), (0.28, 0.028, 0.42, 0.0));
        for bad in ["0.28,0.028", "0.28,0.028,0.42,1", "a,b,c", "0,0,0", "-1,0,1", "NaN,0,1", ""] {
            assert!(price_arg(bad).is_err(), "{bad:?}");
        }
    }
}
