//! Putting the pieces together: presets under the configuration, the
//! provider and model to use, tools (built-in and MCP), the agent's setup,
//! and the informational subcommands.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use nexo_core::config::{Config, ProviderConfig};
use nexo_core::permission::{PermissionDecision, PermissionGate, PermissionRequest};
use nexo_core::ToolSet;

pub struct Picked {
    pub provider: String,
    pub model: String,
}

/// Presets are defaults: a configured provider with the same name keeps the
/// preset's fields it does not set itself.
pub fn apply_presets(cfg: &mut Config) {
    for (name, preset) in nexo_providers::presets() {
        match cfg.providers.get_mut(&name) {
            None => {
                cfg.providers.insert(name, preset);
            }
            Some(own) => {
                if own.protocol.is_empty() {
                    own.protocol = preset.protocol;
                }
                if own.base_url.is_empty() {
                    own.base_url = preset.base_url;
                }
                if own.api_key_env.is_none() {
                    own.api_key_env = preset.api_key_env;
                }
                if own.login.is_none() {
                    own.login = preset.login;
                }
                for (k, v) in preset.extra {
                    own.extra.entry(k).or_insert(v);
                }
                if own.models.is_empty() {
                    own.models = preset.models;
                }
                if own.default_model.is_none() {
                    own.default_model = preset.default_model;
                }
                for (k, v) in preset.pricing {
                    own.pricing.entry(k).or_insert(v);
                }
                for (k, v) in preset.context_window {
                    own.context_window.entry(k).or_insert(v);
                }
                for (k, v) in preset.max_output {
                    own.max_output.entry(k).or_insert(v);
                }
                for (k, v) in preset.headers {
                    own.headers.entry(k).or_insert(v);
                }
            }
        }
    }
    crate::account::add_directory(cfg);
}

pub fn pick(cfg: &Config, provider: Option<&str>, model: Option<&str>) -> anyhow::Result<Picked> {
    let mut c = cfg.clone();
    if let Some(p) = provider {
        c.provider = Some(p.to_string());
        if !c.providers.contains_key(p) {
            anyhow::bail!("unknown provider {p:?}; `nexo providers` lists them");
        }
    }
    if let Some(m) = model {
        c.model = Some(m.to_string());
    }
    let (provider, model) =
        c.pick().ok_or_else(|| {
            anyhow::anyhow!(
                "no provider can be used: set a key (e.g. {}) or run `nexo auth login <provider>`; `nexo guide` explains every way",
                crate::account::KEY_IN_ENV
            )
        })?;
    let pc = &c.providers[&provider];
    if !pc.ready(&provider) {
        anyhow::bail!(
            "provider {provider:?} needs a key: {} (`nexo doctor` checks the setup)",
            crate::account::how_to_unlock(&provider, pc)
        );
    }
    Ok(Picked { provider, model })
}

pub fn model_choices(cfg: &Config) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, p) in &cfg.providers {
        if !p.ready(name) {
            continue;
        }
        for m in &p.models {
            out.push((name.clone(), m.clone()));
        }
        if let Some(d) = &p.default_model {
            if !p.models.contains(d) {
                out.push((name.clone(), d.clone()));
            }
        }
    }
    out
}

pub async fn agent_setup(
    cfg: &Config,
    cwd: &Path,
    picked: &Picked,
    resume: Option<String>,
    gate: Option<Arc<dyn PermissionGate>>,
) -> anyhow::Result<nexo_agent::AgentSetup> {
    let client: Arc<dyn nexo_agent::client::ModelClient> =
        Arc::new(nexo_providers::Client::new(nexo_providers::registry()));
    let mut tools: ToolSet = nexo_tools::builtin_with(&cfg.web);
    for (name, server) in &cfg.mcp {
        if server.enabled == Some(false) {
            continue;
        }
        match nexo_tools::mcp::McpClient::connect(name, server).await {
            Ok(c) => {
                for t in c.tools() {
                    tools.add(t);
                }
                // the client lives as long as the process; its tools hold what they need
                std::mem::forget(c);
            }
            Err(e) => eprintln!("nexo: MCP server {name:?} not started: {e:#}"),
        }
    }
    Ok(nexo_agent::AgentSetup {
        config: cfg.clone(),
        cwd: cwd.to_path_buf(),
        provider: picked.provider.clone(),
        model: picked.model.clone(),
        tools,
        client,
        resume,
        gate,
    })
}

/// Headless runs cannot ask: what the policy does not allow is denied, with a hint.
pub struct HeadlessGate;

#[async_trait]
impl PermissionGate for HeadlessGate {
    async fn ask(&self, req: PermissionRequest) -> PermissionDecision {
        PermissionDecision::Deny {
            reason: Some(format!(
                "{} not allowed in a non-interactive run; use --permission-mode auto or an allow rule such as {}",
                req.summary,
                req.suggested_rules.first().cloned().unwrap_or_else(|| req.tool.clone())
            )),
        }
    }
}

/// `nexo providers [--all] [--json]`: the providers in use (presets,
/// configured, and directory entries with a key), or with `--all` every one
/// nexo knows.
pub fn print_providers(cfg: &Config, all: bool, json: bool) -> i32 {
    let presets = nexo_providers::presets();
    let mut rows: Vec<(String, ProviderConfig, &str)> = cfg
        .providers
        .iter()
        .map(|(n, p)| {
            let origin = if presets.contains_key(n) {
                "preset"
            } else if nexo_providers::directory::get(n).is_some() {
                "directory"
            } else {
                "configured"
            };
            (n.clone(), p.clone(), origin)
        })
        .collect();
    if all {
        for e in nexo_providers::directory::all() {
            if !cfg.providers.contains_key(e.id) && nexo_providers::directory::alias(e.id).is_none() {
                let mut p = e.to_config();
                p.auth = Some(e.id.to_string());
                rows.push((e.id.to_string(), p, "directory"));
            }
        }
    }
    if json {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|(n, p, origin)| {
                serde_json::json!({
                    "name": n,
                    "origin": origin,
                    "protocol": p.protocol,
                    "base_url": p.base_url,
                    "key_env": p.api_key_env,
                    "login": p.login,
                    "key": crate::account::key_source(n, p),
                    "ready": p.ready(n),
                    "default_model": p.default_model.clone().or_else(|| p.models.first().cloned()),
                    "models": p.models,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return 0;
    }
    println!(
        "{:<16} {:<17} {:<20} {:<22} {:<24} base url",
        "provider", "protocol", "key", "key variable / login", "default model"
    );
    for (name, p, origin) in &rows {
        let var = match (&p.api_key_env, &p.login, &p.api_key_cmd) {
            (Some(v), Some(l), _) => format!("{v} | login {l}"),
            (Some(v), None, _) => v.clone(),
            (None, Some(l), _) => format!("login {l}"),
            (None, None, Some(c)) => format!("$({c})"),
            _ => "-".into(),
        };
        let model = p.default_model.clone().or_else(|| p.models.first().cloned()).unwrap_or_default();
        println!(
            "{:<16} {:<17} {:<20} {:<22} {:<24} {}{}",
            shorten(name, 16),
            p.protocol,
            shorten(&crate::account::key_source(name, p), 20),
            shorten(&var, 22),
            shorten(&model, 24),
            p.base_url,
            if *origin == "configured" { " (configured)" } else { "" }
        );
    }
    if !all {
        println!(
            "\n`nexo providers --all` lists every provider nexo knows ({} more); `nexo guide` explains the setup",
            { nexo_providers::directory::all().iter().filter(|e| !cfg.providers.contains_key(e.id)).count() }
        );
    }
    0
}

fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

/// The limits of every model known under `name`: the configured ones, the
/// catalogue's and the vendor's, with prices and limits resolved the way the
/// agent resolves them.
pub async fn print_models(cfg: &Config, provider: Option<&str>, refresh: bool, json: bool) -> anyhow::Result<i32> {
    // with --json the notes go to stderr, so stdout holds only the list
    macro_rules! say {
        ($($t:tt)*) => {
            if json { eprintln!($($t)*) } else { println!($($t)*) }
        };
    }
    use nexo_providers::catalogue;
    let name = match provider {
        Some(p) => p.to_string(),
        None => cfg
            .pick()
            .map(|(p, _)| p)
            .ok_or_else(|| anyhow::anyhow!("no provider can be used; `nexo doctor` says why"))?,
    };
    let p: &ProviderConfig = cfg.providers.get(&name).ok_or_else(|| anyhow::anyhow!("unknown provider {name:?}"))?;
    if refresh {
        match catalogue::refresh().await {
            Ok(c) => say!(
                "catalogue refreshed: {} providers, cached at {}",
                c.providers.len(),
                catalogue::cache_file().display()
            ),
            Err(e) => say!("catalogue not refreshed: {}", e.message),
        }
    } else if let Some(age) = catalogue::cache_age() {
        say!("catalogue: cached {} day(s) ago ({})", age.as_secs() / 86_400, catalogue::cache_file().display());
    } else {
        say!("catalogue: not fetched yet (`nexo models --refresh`)");
    }
    let mut names: Vec<String> = p.models.clone();
    if let Some(c) = catalogue::cached() {
        for n in c.model_names(&name) {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    let vendor = if p.ready(&name) {
        let client = nexo_providers::Client::new(nexo_providers::registry());
        match tokio::time::timeout(std::time::Duration::from_secs(20), client.list_models(p)).await {
            Ok(Ok(list)) => {
                for m in &list {
                    if !names.contains(&m.id) {
                        names.push(m.id.clone());
                    }
                }
                Some(list)
            }
            Ok(Err(e)) => {
                say!("(the vendor's list is not available: {})", e.message);
                None
            }
            Err(_) => {
                say!("(the vendor's list did not answer in time)");
                None
            }
        }
    } else {
        say!("(no key for {name}: the vendor's list is not asked for; {})", crate::account::how_to_unlock(&name, p));
        None
    };
    if json {
        let list: Vec<serde_json::Value> = names
            .iter()
            .map(|m| {
                let l = nexo_agent::limits::resolve(&name, p, m, vendor.as_deref());
                serde_json::json!({
                    "provider": name,
                    "model": m,
                    "context_window": l.context_window,
                    "max_output": l.max_output,
                    "pricing": l.pricing.map(|pr| serde_json::json!({
                        "input": pr.input, "output": pr.output,
                        "cache_read": pr.cache_read, "cache_write": pr.cache_write,
                    })),
                    "source": l.source,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return Ok(0);
    }
    println!("{:<40} {:>9} {:>8} {:>8} {:>8} {:>8}  source", "model", "context", "output", "in", "out", "cache");
    for m in &names {
        let l = nexo_agent::limits::resolve(&name, p, m, vendor.as_deref());
        let k = |v: Option<u32>| v.map(|n| format!("{}k", n / 1000)).unwrap_or_else(|| "-".into());
        let (i, o, c) = match l.pricing {
            Some(pr) => (format!("{:.3}", pr.input), format!("{:.3}", pr.output), format!("{:.3}", pr.cache_read)),
            None => ("-".into(), "-".into(), "-".into()),
        };
        println!(
            "{:<40} {:>9} {:>8} {:>8} {:>8} {:>8}  {}",
            shorten(m, 40),
            k(l.context_window),
            k(l.max_output),
            i,
            o,
            c,
            l.source
        );
    }
    Ok(0)
}

/// `nexo pricing [provider/model] [--set in=..,out=..,cache=..,write=..]`.
pub async fn pricing_cmd(
    cfg: &Config,
    which: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    set: Option<&str>,
) -> anyhow::Result<i32> {
    let (provider, model) = match which {
        Some(w) => match w.split_once('/') {
            Some((p, m)) => (p.to_string(), m.to_string()),
            None => {
                let (p, _) = pick(cfg, provider, None).map(|p| (p.provider, p.model))?;
                (p, w.to_string())
            }
        },
        None => {
            let picked = pick(cfg, provider, model)?;
            (picked.provider, picked.model)
        }
    };
    let pc = cfg.providers.get(&provider).ok_or_else(|| anyhow::anyhow!("unknown provider {provider:?}"))?;
    let l = nexo_agent::limits::resolve(&provider, pc, &model, None);
    let Some(set) = set else {
        println!("{provider}/{model}");
        match l.pricing {
            Some(p) => println!(
                "  prices per million: in {} · out {} · cache read {} · cache write {}  ({})",
                p.input, p.output, p.cache_read, p.cache_write, l.source
            ),
            None => println!("  prices: unknown (`nexo pricing {provider}/{model} --set in=..,out=..,cache=..`)"),
        }
        println!("  context window: {}", l.context_window.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()));
        println!("  largest reply:  {}", l.max_output.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()));
        return Ok(0);
    };
    let mut p = l.pricing.unwrap_or_default();
    for kv in set.split([',', ';', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').ok_or_else(|| anyhow::anyhow!("expected key=value, got {kv:?}"))?;
        let v: f64 = v.trim().parse().map_err(|_| anyhow::anyhow!("{k}: not a number: {v:?}"))?;
        match k.trim().to_ascii_lowercase().as_str() {
            "in" | "input" => p.input = v,
            "out" | "output" => p.output = v,
            "cache" | "cache_read" | "cached" => p.cache_read = v,
            "write" | "cache_write" => p.cache_write = v,
            other => anyhow::bail!("unknown key {other:?} (in, out, cache, write)"),
        }
    }
    let file = nexo_core::paths::user_config_file();
    nexo_agent::config_edit::set_pricing(&file, &provider, &model, p)?;
    println!(
        "saved to {}: {provider}/{model} in {} · out {} · cache read {} · cache write {} per million",
        file.display(),
        p.input,
        p.output,
        p.cache_read,
        p.cache_write
    );
    Ok(0)
}

/// `nexo prompt --edit`: the prompt file in the person's editor.
pub fn edit_prompt(cfg: &Config) -> anyhow::Result<i32> {
    let file = nexo_agent::prompt::source_file(&cfg.agent).unwrap_or_else(nexo_core::paths::prompt_file);
    if !file.exists() {
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&file, format!("{}\n", nexo_agent::prompt::built_in()))?;
        println!("created {} from the built-in text", file.display());
    }
    // `$VISUAL`, `$EDITOR`, else Notepad on Windows and nano or vi elsewhere: the first that starts
    let status = nexo_core::shell::editors()
        .iter()
        .filter_map(|w| w.split_first())
        .find_map(|(cmd, args)| {
            std::process::Command::new(nexo_core::shell::program(cmd)).args(args).arg(&file).status().ok()
        })
        .ok_or_else(|| anyhow::anyhow!("no editor: set $EDITOR, or edit {}", file.display()))?;
    println!(
        "{} ({}); the next request uses it",
        file.display(),
        if status.success() { "saved" } else { "editor exited with an error" }
    );
    Ok(if status.success() { 0 } else { 1 })
}

pub fn print_sessions() -> i32 {
    let list = nexo_agent::session::list();
    if list.is_empty() {
        println!("no sessions yet");
        return 0;
    }
    println!("{:<22} {:<9} {:<28} title", "id", "messages", "directory");
    for m in list {
        let dir = m.cwd.to_string_lossy();
        let dir = if dir.len() > 28 { format!("…{}", &dir[dir.len() - 27..]) } else { dir.to_string() };
        println!("{:<22} {:<9} {:<28} {}", m.id, m.messages, dir, m.title);
    }
    0
}

/// `nexo sessions rename <id> <title>`: the title line is appended to the session file.
pub fn rename_session(id: &str, title: &str) -> anyhow::Result<i32> {
    let title = title.trim();
    if title.is_empty() {
        anyhow::bail!("give the session a name: nexo sessions rename <id> <name>");
    }
    let mut s = nexo_agent::session::Session::open(id)
        .map_err(|_| anyhow::anyhow!("no session {id:?}; `nexo sessions` lists them"))?;
    s.set_title(title)?;
    println!("{id} is now \"{title}\"");
    Ok(0)
}

/// `nexo brief <conversation> <task>`: the briefing a sub-agent would get.
pub async fn brief_cmd(
    cwd: &Path,
    conversation: &str,
    task: &str,
    cfg: &nexo_agent::brief::BriefConfig,
    json: bool,
) -> anyhow::Result<i32> {
    let history = load_messages(conversation)?;
    let started = std::time::Instant::now();
    let brief = nexo_agent::brief::build(task, &history, cwd, cfg).await;
    let ms = started.elapsed().as_millis() as u64;
    if json {
        let v = match &brief {
            Some(b) => serde_json::json!({"text": b.text, "tokens": b.tokens, "spans": b.spans, "facts": b.facts,
                                          "engine": b.engine, "ms": ms}),
            None => serde_json::json!({"text": "", "tokens": 0, "spans": 0, "facts": 0, "engine": null, "ms": ms}),
        };
        println!("{v}");
        return Ok(0);
    }
    match brief {
        Some(b) => {
            println!("{}\n", b.text);
            eprintln!("{} ({ms} ms)", b.line());
        }
        None => eprintln!("no briefing: the conversation holds nothing for this task (or the budget is 0)"),
    }
    Ok(0)
}

/// A session's messages, or those of a file (a JSON array, or one message per line).
fn load_messages(what: &str) -> anyhow::Result<Vec<nexo_core::message::Message>> {
    let path = Path::new(what);
    if !path.is_file() {
        let s = nexo_agent::session::Session::open(what)
            .map_err(|_| anyhow::anyhow!("{what:?} is neither a file nor a session id (`nexo sessions` lists them)"))?;
        return Ok(s.messages().to_vec());
    }
    let text = std::fs::read_to_string(path)?;
    if let Ok(all) = serde_json::from_str::<Vec<nexo_core::message::Message>>(&text) {
        return Ok(all);
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| anyhow::anyhow!("{}: not a message: {e}", path.display())))
        .collect()
}

pub fn config_cmd(cwd: &Path, what: Option<&crate::cli::ConfigCmd>) -> anyhow::Result<i32> {
    use crate::cli::ConfigCmd;
    match what {
        None | Some(ConfigCmd::Path) => {
            println!("user:    {}", nexo_core::paths::user_config_file().display());
            match nexo_core::paths::project_config_file(cwd) {
                Some(p) => {
                    let trusted = nexo_core::config::is_trusted(
                        &nexo_core::config::project_root(&p),
                        &nexo_core::paths::trusted_projects_file(),
                    );
                    let note = if trusted { "trusted" } else { "not trusted: `nexo config trust` to apply all of it" };
                    println!("project: {} ({note})", p.display())
                }
                None => println!("project: (no .nexo or .git directory above {})", cwd.display()),
            }
            println!("sessions: {}", nexo_core::paths::sessions_dir().display());
            Ok(0)
        }
        Some(ConfigCmd::Trust { dir }) => {
            let at = dir.as_deref().map(|d| cwd.join(d)).unwrap_or_else(|| cwd.to_path_buf());
            let Some(file) = nexo_core::paths::project_config_file(&at) else {
                println!("no .nexo or .git directory above {}: nothing to trust", at.display());
                return Ok(1);
            };
            let root = nexo_core::config::project_root(&file);
            let list = nexo_core::paths::trusted_projects_file();
            if nexo_core::config::trust_project(&root, &list)? {
                println!("trusted {} (listed in {})", root.display(), list.display());
            } else {
                println!("{} is trusted already", root.display());
            }
            Ok(0)
        }
        Some(ConfigCmd::Init { project }) => {
            let path = if *project {
                nexo_core::paths::project_dir(cwd).unwrap_or_else(|| cwd.join(".nexo")).join("config.toml")
            } else {
                nexo_core::paths::user_config_file()
            };
            if path.exists() {
                println!("{} exists; not touched", path.display());
                return Ok(1);
            }
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&path, STARTER)?;
            println!("wrote {}", path.display());
            Ok(0)
        }
    }
}

const STARTER: &str = r#"# nexo configuration. Keys never go in this file: name the variable that holds them.

# The provider and model used when none is asked for.
provider = "deepseek"
model = "deepseek-chat"

# Without editing this file:
#   nexo auth login deepseek --key      keep a key (in the credential store, not here)
#   nexo auth login copilot             a subscription: copilot, codex (ChatGPT), xai
#   nexo use deepseek deepseek-chat     make it the default
#   nexo doctor                         check everything, with a live call
#   nexo providers --all                the 200+ providers nexo knows by name
#   nexo guide                          the whole setup guide
#
# Presets exist for: openai, anthropic, gemini, deepseek, xai, groq, mistral, openrouter,
# together, fireworks, cerebras, moonshot, qwen, zhipu, minimax, copilot, codex,
# ollama, lmstudio, vllm, llamacpp; any directory entry works with its key set.
# A block here adds a provider or overrides a preset's fields.
#
# [providers.myserver]
# protocol = "openai-chat"          # openai-chat | openai-responses | anthropic | gemini
# base_url = "http://localhost:8000/v1"
# api_key_env = "MYSERVER_API_KEY"  # or: api_key_cmd = "pass show myserver"
# models = ["my-model"]
# [providers.myserver.pricing."my-model"]   # per million tokens, optional
# input = 0.5
# output = 1.5
# cache_read = 0.05

[agent]
max_turns = 200
max_tokens = 16384
# loop_guard = 3                    # stop when the same tool rounds come back 3 times with the same outputs (0: never)
# stale_guard = 6                   # stop after 6 tool rounds in a row that wrote nothing and brought no new output line (0: never)
# guard_answer = true               # when a guard stops, one last call without tools asks for the answer
# parallel_tasks = true             # several sub-agents of one message run at once, also ones that change files
# reasoning = "medium"              # off | low | medium | high | a token budget
# max_cost = 1.0                    # stop a session at this many dollars
# max_cost_day = 5.0                # and every session together at this many a day

[permissions]
mode = "ask"                        # ask | accept-edits | plan | auto | read-only
allow = ["read", "glob", "grep", "ls", "bash(git status*)", "bash(git diff*)"]
deny = []

[context]
policy = "auto"                     # auto (curator if found) | plain | curator | compact
# curator_spec = "eoq=24000"        # settings for the curator, key=value;... (see the README)

[ui]
theme = "dark"                      # dark | nexo | light | mono | catppuccin-mocha | gruvbox | nord | dracula | solarized-dark | tokyo-night
notify_after = 15                   # ring when a turn this long (s) ends; 0: never
mouse = true
show_reasoning = true
fold_after = 12
animations = true                   # smooth scrolling, fades, pulses; false: no motion
# timestamps = true                 # a dim HH:MM at the right of each message and reply, and how long a reply took; /timestamps toggles
screen = "full"                     # full (alternate screen) | inline (finished turns go to the terminal scrollback)
# status_line = ["model", "effort", "dir", "branch", "mode", "context", "curator", "mem", "cache", "tokens", "cost"]   # pieces of the bottom bar, in order; default: all
# status_command = "~/.config/nexo/statusline.sh"   # draws the bottom bar instead: JSON of the session on stdin, its first line (ANSI colours) shown

[web]
search = "auto"                     # auto | duckduckgo | tavily | brave | exa | searxng
# url = "https://searx.example.org" # SearXNG's address (or another one for the chosen backend)
# keys: TAVILY_API_KEY, BRAVE_API_KEY, EXA_API_KEY in the environment

# [mcp.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/project"]
"#;

/// `nexo sessions delete|restore <id>...`: each one moved to or from the trash; 1 when any failed.
pub fn trash_sessions(ids: &[String], restore: bool) -> i32 {
    if ids.is_empty() {
        eprintln!("which session? `nexo sessions` lists them");
        return 1;
    }
    let mut code = 0;
    for id in ids {
        let r = if restore { nexo_agent::session::restore(id) } else { nexo_agent::session::trash(id) };
        match r {
            Ok(p) => println!("{id} {} ({})", if restore { "restored" } else { "moved to the trash" }, p.display()),
            Err(e) => {
                eprintln!("{id}: {e}");
                code = 1;
            }
        }
    }
    code
}
