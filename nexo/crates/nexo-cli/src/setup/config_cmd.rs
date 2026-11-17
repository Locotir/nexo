//! `nexo prompt --edit`, `nexo brief` and `nexo config`.

use super::*;

/// `nexo prompt --edit`: the prompt file in the person's editor.
pub fn edit_prompt(cfg: &Config) -> anyhow::Result<i32> {
    let file = nexo_agent::prompt::source_file(&cfg.agent).unwrap_or_else(nexo_core::paths::prompt_file);
    if !file.exists() {
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        // `[agent] system_prompt_file` is a whole prompt; `~/.nexo/prompt.md` adds preferences after the built-in one
        if cfg.agent.system_prompt_file.is_some() {
            std::fs::write(&file, format!("{}\n", nexo_agent::prompt::built_in()))?;
            println!("created {} from the built-in text", file.display());
        } else {
            std::fs::write(&file, nexo_agent::prompt::PREFS_TEMPLATE)?;
            println!("created {} for your preferences (added after the built-in prompt)", file.display());
        }
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
pub(super) fn load_messages(what: &str) -> anyhow::Result<Vec<nexo_core::message::Message>> {
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
            // The project lookup walks up through parents without checking that the start exists, so a
            // typo inside a repo would silently trust the whole enclosing repo, a wider grant than named.
            if !at.is_dir() {
                anyhow::bail!("no such directory: {}", at.display());
            }
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

# The provider and model used when none is asked for. Left unset, `nexo` opens on "Set up a model".
# provider = "your-provider"
# model = "your-model"

# Without editing this file:
#   nexo auth login <provider> --key    keep a key (in the credential store, not here)
#   nexo auth login copilot             a subscription: copilot, codex (ChatGPT; reuses the Codex CLI login or opens the browser), xai
#   nexo use <provider> <model>         make it the default
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
# on_limit = "checkpoint"           # at max_turns the model gets a progress report and decides to stop or go on; "ask": stop and wait for you
# max_checkpoints = 5               # per request; default 5 with no spend limit, unlimited under max_cost / max_cost_day (0: never)
max_tokens = 16384                 # reply size asked for; on a model whose window is 16x its largest reply nexo asks for the largest reply (thinking counts in it); under 16384 it is your cap
# reasoning_budget = 8000          # a reply that only reasons past this many tokens is cut and asked again at its lowest effort, without more room; with no lower effort the third reply runs free (0: never; not applied when reasoning is set)
# loop_guard = 3                    # stop when the same tool rounds come back 3 times with the same outputs (0: never)
# stale_guard = 6                   # stop after 6 tool rounds in a row that wrote nothing and brought no new output line (0: never)
# guard_answer = true               # when a guard stops, one last call without tools asks for the answer
# close_on_verified = false         # an answer in a reply with a passing check after a change ends the turn there
# watch = true                      # the supervisor counts repeats, undone edits and long read runs, and adds a hidden note to the next request (no model call)
# check_in = true                   # a stopped session with unfinished work gets one background check-in with its own model (CONTINUE or DONE)
# parallel_tasks = true             # several sub-agents of one message run at once, also ones that change files
# detach_on_message = true          # a message sent while a sub-agent runs sends it to the background and is answered at once
# reasoning = "medium"              # off | low | medium | high | a token budget
# max_cost = 1.0                    # stop a session at this many dollars
# max_cost_day = 5.0                # and every session together at this many a day

[permissions]
mode = "ask"                        # ask | accept-edits | plan | yolo | read-only
allow = ["read", "glob", "grep", "ls", "bash(git status*)", "bash(git diff*)"]
deny = []

[context]
policy = "auto"                     # auto (curator if found) | plain | curator | compact
# curator_spec = "eoq=24000"        # settings for the curator, key=value;... (see the README)

[ui]
theme = "nexo"                      # nexo | dark | light | mono | catppuccin-mocha | gruvbox | nord | dracula | solarized-dark | tokyo-night
notify_after = 15                   # ring when a turn this long (s) ends; 0: never
mouse = true
show_reasoning = true
fold_after = 12
group_tools = true                  # a run of tool calls is one summary line (Made 1 edit, read 2 files, ran 3 shell commands); Enter or a click opens it; false: every call on its own
complete_words = true                # grey ghost text finishes the word you type from any word of this session (names, paths, hashes, plain words); → takes it; false: off
animations = true                   # smooth scrolling, fades, pulses, the opening mark; false: no motion
# figure = "galaxy"                  # the dots on the welcome screen: galaxy | constellation | none
# home_stars = true                # faint stars drift across an empty session until its first message
# bursts = true                    # a burst of dots when a goal is met (green) or a turn fails (red)
# detail = "quiet"                  # quiet: no key hints row, no doubled figures, short dialogs; full: everything on screen
# todo_strip = false                # true: the open to-do list as rows above the draft; false: a count like 3/7 in the bottom bar (click: /todo)
suggest = true                      # when a turn ends, a dim suggestion of your next message shows in the empty input; Tab or → takes it; costs one short cached call; false: off
hints = true                        # the empty input shows one short hint (queued messages, a key worth knowing); false: only "ask anything"
# timestamps = true                 # a dim HH:MM at the right of each message and reply, and how long a reply took; /settings timestamps toggles
screen = "full"                     # full (alternate screen) | inline (finished turns go to the terminal scrollback)
# status_line = ["model", "effort", "dir", "branch", "mode", "context", "curator", "mem", "cache", "plan", "tokens", "cost"]   # pieces of the bottom bar, in order; default: all
# status_command = "~/.config/nexo/statusline.sh"   # draws the bottom bar instead: JSON of the session on stdin, its first line (ANSI colours) shown

[tools]
edit_diagnostics = true             # after edit and write, add the compiler errors of the changed file (Rust, TypeScript, Python, Go) to the result; false: off
post_write_check = true            # after a step that changed source files, run the project test command (cargo test -q, npm test -s, pytest -q -x, go test ./...) once and add its result to the last changing call; false: off
post_write_check_secs = 45         # seconds before that check is killed; a command that ran over is not run again by it
# bin = true                        # the bin tool: read a compiled program (ELF, PE, Mach-O, wasm) as bytes, never run; false: not offered

[compat]
# claude_code = true                # false: ignore every Claude Code file (~/.claude, CLAUDE.md, CLAUDE.local.md, .claude/ commands, agents, skills, hooks)

[web]
search = "auto"                     # auto | duckduckgo | tavily | brave | exa | searxng
# url = "https://searx.example.org" # SearXNG's address (or another one for the chosen backend)
# keys: TAVILY_API_KEY, BRAVE_API_KEY, EXA_API_KEY in the environment

[browser]
# source = "installed"               # installed (your Chrome, Chromium or Edge); own-chrome is not available yet (set path instead); auto = installed
# profile = "ephemeral"              # ephemeral: clean, deleted at close | persistent: kept in ~/.nexo/browser/profile, so you sign in to a site once
# headless = true                   # hide the window; unset: hidden for ephemeral, shown for persistent (so you can sign in)
# path = "/usr/bin/google-chrome"   # the browser program, instead of searching for one
# window = "1280x800"               # window size

# [mcp.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/project"]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusting_a_missing_directory_inside_a_repo_is_an_error_and_trusts_nothing() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let typo = std::path::PathBuf::from("./typo");
        let err = config_cmd(repo.path(), Some(&crate::cli::ConfigCmd::Trust { dir: Some(typo) })).unwrap_err();
        assert!(err.to_string().starts_with("no such directory: "), "{err}");
    }

    #[test]
    fn the_starter_file_names_no_provider() {
        assert!(!STARTER.lines().any(|l| l.starts_with("provider") || l.starts_with("model")));
        let cfg = Config::parse(STARTER).unwrap();
        assert!(cfg.provider.is_none() && cfg.model.is_none());
        assert!(!STARTER.contains("deepseek-flash"));
    }
}
