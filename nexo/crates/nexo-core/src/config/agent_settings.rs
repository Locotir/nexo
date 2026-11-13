use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Tool rounds per request before the agent reaches a limit: what happens then is `on_limit`. Sub-agents
    /// have their own round limit and always stop there.
    pub max_turns: u32,
    /// At `max_turns`: `checkpoint` (default) sends the model one short message with the progress so far, and
    /// it decides to stop or to go on, so a long job needs nobody to type "continue"; `ask` stops and waits
    /// for the person. A checkpoint appends to the end of the conversation, so the prompt cache stays. Code
    /// stops the request by itself after two checkpoints in a row with no file written, no new output line and
    /// no change in the todo list; the spend limits (`max_cost`, `max_cost_day`) keep stopping as always.
    pub on_limit: OnLimit,
    /// The most checkpoints one request of the person may take. Unset: 5 when no spend limit is in force
    /// (`max_cost`, `max_cost_day` or `/budget`), so a default install cannot run forever, and no cap when one
    /// is. 0: never checkpoint (as `on_limit = "ask"`).
    pub max_checkpoints: Option<u32>,
    pub max_tokens: u32,
    /// Reasoning tokens a reply may stream before any text or call; past it the reply is cut and asked again (0: off, and skipped when a reasoning level is set).
    pub reasoning_budget: u32,
    pub temperature: Option<f32>,
    /// `off`, `low`, `medium`, `high`, or a token budget as a number.
    pub reasoning: Option<String>,
    /// A file whose text replaces the built-in system prompt.
    pub system_prompt_file: Option<PathBuf>,
    /// Text appended to the system prompt.
    pub system_prompt_extra: Option<String>,
    /// Leave out what lives on this machine: the project instructions (`NEXO.md`, `AGENTS.md`)
    /// and `~/.nexo/prompt.md`. Benchmarks set it, so a run sends the same prompt everywhere.
    pub hermetic: bool,
    /// Files a request names by path (a word with a `/` or an extension) that exist under the working
    /// directory, are text and at most 24 KB, up to three, are included in it in full, so the model needs no
    /// step to read them. They go under nexo's own header (`path (project file, included in full by nexo, as it
    /// is now):`), never under the `@path` header of a file the person attached.
    pub attach_named_files: bool,
    /// Tokens of the project's small text files included whole in each request, under that same header, the
    /// named ones counted, smallest first (nothing in `target/`, `node_modules/`, `.git/`, binaries, files over
    /// 2000 lines). 0: none.
    pub attach_budget: u32,
    /// Runs the project's check (its tests) once at the start of each request, when the policy lets commands
    /// run and the files changed since the last result, and puts the failing tests in the request.
    pub check_at_start: bool,
    /// Retries on retryable provider errors.
    pub retries: u32,
    /// Gives the model the `task` tool: a read-only sub-agent for wide searches.
    pub subagents: bool,
    /// Several `task` calls of one message run at the same time, also sub-agents that change files; a
    /// file one of them is changing is closed to the others until it ends. Off: those that can change
    /// files run one after another.
    pub parallel_tasks: bool,
    /// A message sent while a sub-agent runs sends it to the background and is answered at once. Off: the
    /// message waits for the round to end (the key that sends sub-agents to the background still works).
    pub detach_on_message: bool,
    /// The most a session may spend, in dollars (none: no limit).
    pub max_cost: Option<f64>,
    /// The most every session together may spend in a day, in dollars (none: no limit).
    pub max_cost_day: Option<f64>,
    /// Tokens of the briefing a sub-agent gets with its task: what the conversation
    /// already holds that bears on it (0: none).
    pub subagent_brief: u32,
    /// Who picks the briefing's excerpts: `auto` or `curator` (the curator's
    /// selection, built into nexo) or `lexical` (plain word overlap).
    pub subagent_brief_engine: String,
    /// Stops a request whose latest tool rounds are one block of rounds that ran this many times in a row
    /// with the same calls and the same outputs: nothing new came in, so the next round would not bring
    /// anything either. The request ends as at `max_turns` with `on_limit = "ask"` (0: never).
    pub loop_guard: u32,
    /// Stops a request after this many tool rounds in a row that wrote nothing and brought no output line
    /// the session had not seen already: the agent goes back over what it has, by other paths, with
    /// nothing new coming in. The request ends as at `max_turns` with `on_limit = "ask"` (0: never).
    pub stale_guard: u32,
    /// When a guard (`loop_guard`, `stale_guard`) stops the tool rounds, one last call asks the model in words
    /// to answer with what it has gathered, so the request does not end empty. That call keeps the tools and
    /// the tool choice, so it finds the prompt cache; only if the model calls a tool all the same does one
    /// more call tell the protocol that none may be called.
    pub guard_answer: bool,
    /// A reply that has text and tool calls ends the turn when the answer is not a lead-in, this turn has changed
    /// something (an edit, a write, a command that is not read-only) and the calls of that reply hold a check that
    /// succeeded after the last change (a build, test or lint run, a write carrying `check`). Reads and listings
    /// never count: one request less per turn. Off by default.
    pub close_on_verified: bool,
    /// Notes the supervisor adds to the NEXT model request when plain counting sees the session go wrong: the
    /// same call again and again, a failing command re-run unchanged, edits that undo each other, a long run
    /// of reads, a turn that ends with open todo items or a failed test. They ride on that request only (not
    /// saved in the conversation) and cost no model call.
    pub watch: bool,
    /// When a turn ended with work that looks unfinished by those signals and nothing will wake the session
    /// (no active goal, nothing queued), one background request asks the session's own model whether the
    /// task is finished: DONE changes nothing, CONTINUE resumes the session with the reply as hidden context.
    /// At most one per request of the person; it is never shown as a message and has no tools.
    pub check_in: bool,
    /// A background shell command still running after this many seconds is weighed by one request of its
    /// own, apart from the conversation and with no tools: the command, how long it has run, how much it
    /// printed since the last look, the end of its output and the request in progress. The answer is one
    /// word: STOP ends the job and the model is told why; CONTINUE, or no clear answer, leaves it running
    /// and the next look comes twice as late (0: never).
    pub job_review: u32,
    /// Shell commands run in a new git worktree (`nexo --worktree`, or a sub-agent with `isolation: worktree`),
    /// for example `npm ci`. Files named in `.worktreeinclude` are copied there too. A project that is not
    /// trusted cannot set it, as it runs commands.
    pub worktree_setup: Vec<String>,
}

/// The reasoning tokens `agent.reasoning_budget` allows a reply before it says anything.
pub const DEFAULT_REASONING_BUDGET: u32 = 8_000;

/// The reply size `agent.max_tokens` asks for when the configuration does not say.
pub const DEFAULT_MAX_TOKENS: u32 = 16_384;

impl Default for AgentConfig {
    fn default() -> AgentConfig {
        AgentConfig {
            max_turns: 200,
            on_limit: OnLimit::Checkpoint,
            max_checkpoints: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            reasoning_budget: DEFAULT_REASONING_BUDGET,
            temperature: None,
            reasoning: None,
            system_prompt_file: None,
            system_prompt_extra: None,
            hermetic: false,
            attach_named_files: true,
            attach_budget: 6000,
            check_at_start: true,
            retries: 4,
            subagents: true,
            parallel_tasks: true,
            detach_on_message: true,
            max_cost: None,
            max_cost_day: None,
            subagent_brief: 3000,
            subagent_brief_engine: "auto".into(),
            loop_guard: 3,
            stale_guard: 6,
            guard_answer: true,
            close_on_verified: false,
            watch: true,
            check_in: true,
            job_review: 600,
            worktree_setup: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionsConfig {
    pub mode: PermissionMode,
    /// OS sandbox for the bash tool: `off`, `workspace-write` or `read-only`.
    pub sandbox: crate::sandbox::SandboxMode,
    /// Whether sandboxed commands may use the network (default: no).
    pub sandbox_network: bool,
    /// Directories besides the project that sandboxed commands may write (those that exist).
    pub sandbox_write_dirs: Vec<PathBuf>,
    /// Rules: `bash(git *)`, `write(src/**)`, `read`, `mcp:server:tool`.
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// `auto` or `curator` (the context curator built into nexo), `plain` (send
    /// everything), `compact` (summarise when over the window). Keys this struct
    /// no longer has (`curator_command`, `curator_model`) are ignored.
    pub policy: String,
    /// `compact`: summarise once a request reaches this many input tokens (as the
    /// provider counts them: system, tools and messages), capped at the window
    /// minus the reply budget.
    pub compact_at: u64,
    /// Settings passed to the curator (`threshold=0.02;head=turn;...`); empty: its default D.
    pub curator_spec: String,
    /// The curator's working window as `target/cap/hard` in tokens of the view, e.g. `"10000/40000/80000"`
    /// (empty: its default). The view aims at `target`, is cut at `cap` whatever the cache costs, and
    /// never passes `hard`; what the agent is working from is never cut before `hard`. It overrides the
    /// same settings in `curator_spec`; `compact_at` is not changed by it.
    pub working_set: String,
    /// A tool output of more than this many tokens is read by a separate request, and the session gets
    /// its notes in place of the output (0: off).
    pub reader_at: u64,
}

impl Default for ContextConfig {
    fn default() -> ContextConfig {
        ContextConfig {
            policy: "auto".into(),
            compact_at: 100_000,
            curator_spec: String::new(),
            working_set: String::new(),
            reader_at: 0,
        }
    }
}
