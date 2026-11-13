use super::*;

/// What the interface asks the agent to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AgentCommand {
    /// A request from the person. Queued if a turn is running.
    Submit {
        text: String,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    /// Stop the running turn (tools are cancelled, the partial reply is kept).
    Interrupt,
    /// Send the sub-agents the running turn waits for to the background (they go on; the turn is free).
    Detach,
    /// A message for the sub-agent run `id`: it joins that run's conversation between its rounds (the main agent
    /// sees nothing of it). When it cannot be delivered (unknown id, the run ended) a `SubNotDelivered` says why.
    SubSubmit {
        id: String,
        text: String,
    },
    /// Asks for the kept conversation of the sub-agent run `id`; one `SubTranscript` event answers (with `None`
    /// when the run is unknown or was forgotten).
    SubTranscript {
        id: String,
    },
    /// Run the saved workflow `name` with `args` as its `{{args}}` (`/workflow <name> [args]`). It asks the same
    /// question the `workflow` tool asks, about the same pinned file, and runs in the background: the main agent
    /// gets the result as a note from nexo when it ends. A refusal (one run at a time, no such workflow, a denial)
    /// comes back as a `Notice`.
    FlowStart {
        name: String,
        #[serde(default)]
        args: String,
    },
    /// Stop step `step` of workflow run `run`: its sub-agents are cancelled, it ends as failed, what needs it is
    /// skipped, the other steps go on.
    FlowStopStep {
        run: String,
        step: String,
    },
    /// Stop workflow run `run`: every sub-agent of it is cancelled and nothing new starts.
    FlowStop {
        run: String,
    },
    /// Stop the running turn and run the queued request at once (Enter on an empty draft). With
    /// nothing queued it is an `Interrupt`.
    InterruptRun,
    /// Take a queued request back out of the queue (to edit it): the one at `index` if it still
    /// reads `text`, else the first waiting one that does; answered with `Queued`. A request that
    /// already started is not found, and nothing changes.
    Unqueue {
        index: usize,
        text: String,
    },
    Permission {
        id: String,
        decision: PermissionDecision,
    },
    /// The answer to a `Question`: the picked labels (joined by `, `) or the
    /// person's own words; None when they closed it without answering.
    Answer {
        id: String,
        answer: Option<String>,
    },
    SetModel {
        provider: Option<String>,
        model: String,
    },
    SetPermissionMode(PermissionMode),
    /// Change the sandbox mode of the `bash` tool (`/sandbox`; None switches between off and workspace-write).
    SetSandbox(Option<crate::sandbox::SandboxMode>),
    /// Let the file tools read and write in one more folder (`/add-dir`), a sandboxed command write
    /// there, and the model know it; answered with a `Notice`.
    AddDir(std::path::PathBuf),
    /// Replace the history with a summary of it.
    Compact,
    /// Start a new session in the same directory.
    Clear,
    /// Run a shell command outside the model's view (the `!` prefix).
    Shell {
        command: String,
    },
    /// Replace the conversation with a recorded session (`HistoryCleared`, then its `History`).
    Resume {
        id: String,
    },
    /// Continue in a new session that starts as a copy of this one up to before message `index`
    /// (None: all of it); the new session knows its parent. Answered like `Resume`.
    Fork {
        index: Option<usize>,
    },
    /// The sessions of this directory as a tree of forks; answered with `SessionTree`.
    ShowTree,
    /// Move a recorded session (not the one in use) to the trash; answered with `SessionDeleted`.
    DeleteSession {
        id: String,
    },
    /// What waits in the trash; answered with `Trash`.
    ListTrash,
    /// Bring a session back from the trash; answered with `SessionRestored`.
    RestoreSession {
        id: String,
    },
    /// Take what another session knows that bears on `focus` (None: that session's goal or title)
    /// along with the next request; answered with `MemoryShared`. `id` None forgets what waits.
    ShareMemory {
        id: Option<String>,
        focus: Option<String>,
    },
    /// Keep only the first `index` messages (the session file is rewritten); the
    /// interface gets `HistoryCleared` and the kept `History` again.
    Rewind {
        index: usize,
    },
    /// Write the conversation (`path` None: next to the session file); answered with a `Notice`.
    /// `format` is `html`, `md` or `json`; none: by the file name's extension, else markdown.
    Export {
        path: Option<PathBuf>,
        format: Option<String>,
    },
    /// Ask for the system prompt as it is sent; answered with `SystemPrompt`.
    ShowPrompt,
    /// Set the prices of the current model (per million tokens) and remember them in the
    /// user's configuration; answered with `ModelLimits`.
    SetPricing {
        pricing: Pricing,
    },
    /// Ask what is known of the current model (prices, context window, largest reply); answered with `ModelLimits`.
    ShowModel,
    /// Set (or clear, with None) the goal the agent keeps working towards across turns: it is
    /// pinned in the system prompt and, when a turn ends without the model declaring the goal
    /// done, the agent asks it to continue, up to a few rounds; answered with `Goal`.
    SetGoal {
        text: Option<String>,
    },
    /// Hold the goal: the turn in progress finishes, no continuation starts after it; answered with `Goal`.
    PauseGoal,
    /// Work on the goal again (after a pause, a wait or its end), the rounds counted from zero; when
    /// nothing runs, a continuation starts now; answered with `Goal`.
    ResumeGoal,
    /// Give the session a name (kept in its file; shown by the pickers); answered with `SessionChanged`.
    Rename {
        title: String,
    },
    /// Set the spend limit of this session in dollars (None: no limit), or ask for the
    /// spending (`show`); answered with `Budget` and a `Notice`.
    SetBudget {
        limit: Option<f64>,
    },
    ShowBudget,
    /// Ask for the state of things (version, session, model and where its key comes from,
    /// context policy, configuration files, tools); answered with a `Notice`.
    ShowStatus,
    /// Ask for the tools the model has, by kind; answered with a `Notice`.
    ShowTools,
    /// Ask for the hooks that are loaded and where each comes from; answered with a `Notice`.
    ShowHooks,
    /// Ask for the last request exactly as it was sent, in sections; answered with `Sent` (a `Notice` when
    /// nothing was sent yet).
    ShowSent,
    /// Ask for the MCP servers with their state and lists; answered with `Mcp`.
    ShowMcp,
    /// Turn an MCP server on or off for this session (`persist`: also in the configuration); answered with `Mcp`.
    McpToggle {
        server: String,
        on: bool,
        #[serde(default)]
        persist: bool,
    },
    /// Sign in to an MCP server that needs a login (the browser opens); answered with `Mcp`.
    McpLogin {
        server: String,
    },
    /// Connect an MCP server again; answered with `Mcp`.
    McpReconnect {
        server: String,
    },
    /// Run a prompt of an MCP server (`/mcp__<server>__<prompt> args`): its messages become the request.
    /// `args` are as typed, positional or `key=value`.
    McpPrompt {
        server: String,
        prompt: String,
        #[serde(default)]
        args: String,
    },
    /// Look for the usual local servers (Ollama, LM Studio, vLLM, llama.cpp) at their usual
    /// ports; answered with `LocalServers`.
    ScanLocal,
    /// Ask what answers at `cfg.base_url` (`/model` setup): the server kind, its protocol and
    /// its models; answered with `Probed`. `key` is one typed in the setup, not yet saved.
    Probe {
        name: String,
        cfg: ProviderConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// Which check this is: the answer carries it back, and a late answer is told apart.
        #[serde(default)]
        seq: u64,
    },
    /// Sign in to a provider's subscription (`/model` setup) with `flow` (see `login::choices`);
    /// the address to open is announced with `LoginPrompt` and the end with `LoggedIn`. The
    /// tokens go to the credential store, never to the screen.
    Login {
        name: String,
        flow: String,
    },
    /// Give up a sign-in that waits for the browser.
    CancelLogin,
    /// Give up the address probe or the test request the setup waits for (its task is stopped and a
    /// key tried leaves nothing behind).
    CancelCheck,
    /// Find out what one model can do and try it with a real request (one short reply that
    /// should call a test tool); answered with `ModelChecked`.
    CheckModel {
        name: String,
        probe: ServerProbe,
        cfg: ProviderConfig,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default)]
        seq: u64,
    },
    /// Save the provider and the model as the setup left them in the user's configuration
    /// (a typed key goes to a file of its own, readable only by the user) and switch to them.
    SaveModel {
        name: String,
        cfg: ProviderConfig,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// Set the reasoning level of the current model (None: back to `agent.reasoning`) and
    /// remember it; answered with `Effort`.
    SetEffort {
        effort: Option<String>,
    },
    /// `/fast`: switch the model to its fast version or back (None: the other one of the two), or turn
    /// on the provider's fast request option; answered with `Fast`, or a `Notice` when there is none.
    SetFast {
        on: Option<bool>,
    },
    /// Stop background job `id`, or every running one with None; answered with `Jobs`.
    KillJob {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
    },
    /// Ask for the background jobs; answered with `Jobs` and a `Notice`.
    ShowJobs,
    /// Mails taken from this session's inbox, handed over at an idle point: one user message that is
    /// not the person's own (like a nudge). The host sends it only after `Idle`, never mid-turn.
    Deliver {
        mails: Vec<crate::mail::Mail>,
    },
    /// The person writes to another session (`/msg`); answered with `MailSent` or `MailFailed`.
    SendMail {
        to: String,
        text: String,
    },
    /// Ask which sessions can be written to; answered with `Peers`.
    ListPeers,
    /// Ask for the end of the output of every background job; answered with `JobTails`.
    JobTails,
    /// Ask for a suggestion of the person's next message, made from the last request of the conversation; answered
    /// with `Suggestion`, or with nothing when there is none. The interface sends it when a turn ends.
    Suggest,
    /// The supervisor asks the session to look at its own work once it has stopped: `evidence` is what it saw.
    /// One background request to the session's model (no tools, never shown as a message) decides: DONE leaves
    /// the session as it is, CONTINUE resumes it with the answer as hidden context. Ignored while a turn runs,
    /// when a goal or a queued request will wake the session, when `agent.check_in` is off, or when this request
    /// of the person was checked already.
    CheckIn {
        evidence: String,
    },
    Shutdown,
}
