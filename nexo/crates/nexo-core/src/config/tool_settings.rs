use super::*;

/// `[tools]`: switches of the built-in file tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// After `edit` and `write` change a source file, the compiler errors in that file are added to the result
    /// (Rust, TypeScript, Python, Go; one bounded check, 20 s at most, nothing when the file is clean).
    pub edit_diagnostics: bool,
    /// After a step that changed source files (`edit`, `write` or a shell command), the compiler errors and the
    /// project's own tests run once and their result is added to the last changing call's result.
    pub post_write_check: bool,
    /// Seconds that check may take (also the cap of an `edit`/`write` `check` command).
    pub post_write_check_secs: u64,
    /// Offer the `debug` tool (run a program under a debugger, break, step, read memory) to the model. On by
    /// default where the debugger has a backend (Linux x86-64); off elsewhere, and there the tool is never
    /// registered, whatever this says.
    pub debug: bool,
    /// Offer the `bin` tool (read a compiled program as bytes: map, functions, disassembly, pseudo-C, references,
    /// strings; the file is never run or changed) to the model. On everywhere; the tool is only a short line in
    /// each request.
    pub bin: bool,
}

impl Default for ToolsConfig {
    fn default() -> ToolsConfig {
        ToolsConfig {
            edit_diagnostics: true,
            post_write_check: true,
            post_write_check_secs: 45,
            debug: cfg!(all(target_os = "linux", target_arch = "x86_64")),
            bin: true,
        }
    }
}

/// `[compat]`: the other coding agents' files nexo reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompatConfig {
    /// Reads Claude Code's files: `~/.claude/CLAUDE.md` (when nexo has no user file of its own), `~/.claude`
    /// commands, agents and skills, and in a project `CLAUDE.md`, `CLAUDE.local.md`, `.claude/CLAUDE.md`,
    /// `.claude/commands`, `.claude/agents`, `.claude/skills` and `.claude/settings.json` hooks. `false`: none of it.
    pub claude_code: bool,
}

impl Default for CompatConfig {
    fn default() -> CompatConfig {
        CompatConfig { claude_code: true }
    }
}

/// The web tools: `web_search` and `web_fetch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Registers the two tools.
    pub enabled: bool,
    /// The search backend: `auto` (the first whose key is present, else
    /// DuckDuckGo), `duckduckgo`, `brave`, `tavily`, `exa` or `searxng`.
    pub search: String,
    /// The backend's base URL instead of its public one (needed for `searxng`).
    pub url: Option<String>,
    /// The environment variable that holds the backend's key, instead of the usual one.
    pub api_key_env: Option<String>,
    /// The User-Agent sent by both tools.
    pub user_agent: Option<String>,
}

impl Default for WebConfig {
    fn default() -> WebConfig {
        WebConfig { enabled: true, search: "auto".into(), url: None, api_key_env: None, user_agent: None }
    }
}
