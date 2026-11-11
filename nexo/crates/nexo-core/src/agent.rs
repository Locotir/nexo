//! The line between the agent and whoever drives it (the TUI, the headless
//! runner, a test): commands in, events out, over channels. The interface
//! never touches providers or tools; it draws what these events say.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio::sync::mpsc;

use crate::config::ProviderConfig;
use crate::message::Message;
use crate::permission::{PermissionDecision, PermissionMode, PermissionRequest, Question};
use crate::pricing::Pricing;
use crate::problem::Problem;
use crate::provider::{ModelCheck, ServerProbe};
use crate::stream::Usage;
use crate::tool::ToolOutput;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
}

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

/// Where an MCP server stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    /// A remote server that answered 401 and has no login that works.
    NeedsLogin,
    /// Could not start or reach; the reason.
    Failed(String),
    /// Turned off (in the configuration or for this session).
    Disabled,
}

/// One argument of an MCP prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpArgInfo {
    pub name: String,
    #[serde(default)]
    pub required: bool,
}

/// One prompt of an MCP server (it becomes the command `/mcp__<server>__<name>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub args: Vec<McpArgInfo>,
}

/// One MCP server as the `/mcp` window and the completion of `@server:` and `/mcp__` see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    /// `stdio`, `http` or `sse`.
    pub transport: String,
    pub status: McpStatus,
    pub tools: usize,
    /// The URIs of the listed resources.
    #[serde(default)]
    pub resources: Vec<String>,
    /// The URI templates of the listed resource templates.
    #[serde(default)]
    pub templates: Vec<String>,
    #[serde(default)]
    pub prompts: Vec<McpPromptInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Around what hooks added for the model in front of a request: the screen shows it as a line, not as
/// the person's words.
pub const HOOK_CONTEXT_START: &str = "<hook-context>";
pub const HOOK_CONTEXT_END: &str = "</hook-context>";

/// What the loop sends in the person's place to start a goal; the person's words follow it.
pub const GOAL_SET_MARK: &str = "[goal set]";
/// What the loop sends in the person's place to keep a goal going.
pub const GOAL_CONTINUE_MARK: &str = "[goal: continue]";
/// What the loop sends in the person's place to resume a session after the supervisor's check-in said CONTINUE;
/// what to do next travels as hidden context of that request.
pub const SUPERVISOR_RESUME_MARK: &str = "[supervisor: continue]";
/// How a goal the agent restated (`goal_brief`) begins.
pub const GOAL_BRIEF_HEAD: &str = "Want: ";

/// One time the goal stopped short of done: when (unix ms) and why (Esc, a guard, the round limit, an error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalStop {
    pub at_ms: i64,
    pub why: String,
}

/// What the goal panel shows besides the goal's state: both wordings of the goal, how long it has run,
/// what it cost and how it was interrupted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoalLog {
    /// The person's own words, as they set the goal.
    pub original: Option<String>,
    /// How the agent restated it (`goal_brief`), once it has.
    pub brief: Option<String>,
    /// When the goal was set (unix ms); 0 when it comes from a session resumed from disk.
    pub started_ms: i64,
    /// Tool calls made since the goal was set.
    pub steps: u32,
    /// What the session spent since the goal was set.
    pub cost_usd: f64,
    /// The times it stopped short of done, oldest first (the last [`GOAL_STOPS`] are kept).
    pub stops: Vec<GoalStop>,
    /// The first line of the agent's latest reply.
    pub last: Option<String>,
}

/// How many stops a goal remembers.
pub const GOAL_STOPS: usize = 20;

/// Where a goal stands, which says what happens when a turn ends.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "why")]
pub enum GoalState {
    /// Worked on: a turn that ends without the goal met is followed by a continuation.
    #[default]
    Active,
    /// The model asked the person something it cannot go on without: their next message answers
    /// it and makes the goal active again.
    Waiting,
    /// Held until `/goal resume`, for the reason given (asked for, Esc, an error, a limit, the rounds
    /// used up, a round that did nothing); messages in between are ordinary turns.
    Paused(String),
    /// Declared met: nothing more runs for it until `/goal resume` or a new goal.
    Done,
}

impl GoalState {
    pub fn is_active(&self) -> bool {
        *self == GoalState::Active
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    Done,
    Interrupted,
    Error,
    MaxTurns,
}

/// What a message of the history is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Something the person wrote.
    Request,
    /// The model's text.
    Reply,
    /// The model asking for tools.
    Call,
    /// What the tools answered.
    Output,
}

/// How much of a message the next model call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellState {
    Whole,
    /// Shortened or stubbed.
    Shrunk,
    Dropped,
    /// Left out, with parts of it brought back.
    Recalled,
}

/// Where the output of one tool call stands for the model call just made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMark {
    /// Sent whole, in the part of the request equal to the one before (the provider serves it from its
    /// cache while that lasts).
    Cached,
    /// Sent whole, after the point where the request changed: paid in full.
    Sent,
    /// Sent shortened.
    Cut,
    /// Left out of the request; kept in the history, so it can come back.
    Out,
    /// Left out, with parts of it brought back.
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCell {
    pub kind: CellKind,
    pub tokens: u64,
    pub state: CellState,
    /// Index into the map's `zones` (0 when there are none).
    #[serde(default)]
    pub zone: u32,
}

/// A part of the curator's memory: the anchor or one topic of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextZone {
    /// The request that opened the topic (`anchor` for the first message).
    pub label: String,
    /// The topic in progress, whose messages the call carries.
    pub active: bool,
    /// Described by a card in the call.
    #[serde(default)]
    pub card: bool,
}

/// What kind of thing the curator put in a call (or held back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratedKind {
    /// A tool output kept in the history: one a step brought back, or one the view left out or cut.
    Recalled,
    /// The start of the request: the map of what moved out, a file it pins or one the agent wrote.
    Start,
    /// A standing instruction the user gave.
    Rule,
    /// A topic of the conversation, as a card or left out.
    Thread,
    /// The plan the agent wrote down.
    Goal,
    /// A fact the agent or the curator's ledger kept.
    Memory,
}

/// Where an item of the curator stands in the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CuratedState {
    In,
    /// In the call, shortened.
    Trimmed,
    /// Held back (its tokens are what was held back).
    Out,
}

/// One item the curator added to a call, or held back: its name, kind and size, never its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratedItem {
    pub kind: CuratedKind,
    pub label: String,
    pub tokens: u64,
    pub state: CuratedState,
}

/// What nexo put in a message of the person beyond the words they typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddedKind {
    /// An MCP resource named with `@server:uri`.
    Mention,
    /// A file attached, named by the person or found by nexo.
    File,
    /// The note of background jobs that ended.
    Jobs,
    /// What a hook added for the model.
    Hook,
    /// Memory shared from another session.
    Memory,
    /// The `[project at the start of this request]` lines.
    StartNote,
    /// A note of the supervisor that rides on one request and is never saved.
    Supervisor,
}

/// One piece of text nexo added to a message of the person, kept apart from their own words where the
/// two are put together (a live session tells them as they are built; a resumed one finds them by markers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedPart {
    pub kind: AddedKind,
    /// What it is, short: a path, a title, `hook context`.
    pub label: String,
    /// The exact text that went to the model.
    pub text: String,
    /// It rides on one request only and is not in the saved conversation.
    #[serde(default)]
    pub request_only: bool,
}

impl AddedPart {
    pub fn new(kind: AddedKind, label: impl Into<String>, text: impl Into<String>) -> AddedPart {
        AddedPart { kind, label: label.into(), text: text.into(), request_only: false }
    }

    /// About how many tokens it takes (4 characters a token).
    pub fn tokens(&self) -> u64 {
        (self.text.len() as u64).div_ceil(4)
    }
}

/// One section of the last request as it was sent (`/sent`): a heading, its text and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentSection {
    pub heading: String,
    /// 0: a main section (system prompt, a message); 1: a part of the one above.
    pub depth: u8,
    /// `curator` or `supervisor` when the context policy or the supervisor put this text in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub tokens: u64,
    pub text: String,
    /// Shown as its first lines until opened (a tool result).
    #[serde(default)]
    pub collapse: bool,
}

/// What the agent tells the interface. Every event is serialisable, so the
/// headless mode can print them as JSON lines and a test can record them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    Ready {
        session_id: String,
        provider: String,
        model: String,
        cwd: PathBuf,
        permission_mode: PermissionMode,
    },
    /// A message of the history (on resume, and the echo of a submission).
    History {
        index: usize,
        message: Message,
    },
    /// What nexo added to the user message at `index` of the history, beyond the person's words (sent right
    /// after its `History`; nothing is sent when nothing was added). With `request_only` parts: notes that ride
    /// on the request in progress and are not in the history.
    Added {
        index: usize,
        parts: Vec<AddedPart>,
    },
    /// The last request exactly as it was sent, in sections (answers `ShowSent`).
    Sent {
        model: String,
        sections: Vec<SentSection>,
    },
    TurnStart {
        turn: u32,
    },
    AssistantStart,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    /// A call the model asked for, with its arguments complete.
    ToolCall {
        id: String,
        name: String,
        args: Value,
        summary: String,
    },
    ToolStart {
        id: String,
    },
    ToolProgress {
        id: String,
        line: String,
    },
    ToolDone {
        id: String,
        output: ToolOutput,
        elapsed_ms: u64,
    },
    /// The assistant message as recorded (text, reasoning, calls).
    AssistantDone {
        message: Message,
    },
    PermissionAsk {
        request: PermissionRequest,
    },
    PermissionResolved {
        id: String,
        decision: PermissionDecision,
    },
    /// The agent asks the person to choose; answered with `AgentCommand::Answer`.
    Question {
        question: Question,
    },
    /// The question was answered or dropped (an interrupt).
    QuestionResolved {
        id: String,
        answer: Option<String>,
    },
    /// After each model call: `turn` is what that call spent, attempts the client retried
    /// included (a turn of several calls sends several of these), `call` the usage of the attempt
    /// that answered alone (the size of the request it sent; empty for a side call, a sub-agent,
    /// the reader or a compaction, whose request is not the conversation's), `session` the running
    /// total.
    Usage {
        turn: Usage,
        #[serde(default)]
        call: Usage,
        session: Usage,
        cost_usd: f64,
    },
    /// What the next model call will see.
    Context {
        view_tokens: u64,
        history_tokens: u64,
        /// Bytes of the whole history (every message and tool output), the "memory" kept.
        history_bytes: u64,
        /// Bytes of what the next call sends.
        view_bytes: u64,
        messages_in_view: usize,
        messages_total: usize,
        policy: String,
        note: Option<String>,
        /// What the policy is doing, for the title bar: "curator +2 in -5 out",
        /// "curator idle", "curator off", "plain", "compact".
        #[serde(default)]
        label: String,
        /// Why, in a sentence (for /context and /status).
        #[serde(default)]
        detail: Option<String>,
    },
    /// Every message of the history, oldest first, with how much of it the
    /// next model call carries (sent with `Context`).
    ContextMap {
        cells: Vec<ContextCell>,
        /// The curator's zones the cells belong to (empty without the curator).
        #[serde(default)]
        zones: Vec<ContextZone>,
        /// Where each tool call's output stands (call id, mark).
        #[serde(default)]
        calls: Vec<(String, CallMark)>,
        /// The tokens of each tool call's output (call id, tokens), where one message carries just that
        /// one output; the interface prices it with the mark.
        #[serde(default)]
        call_tokens: Vec<(String, u64)>,
    },
    /// What the curator put in the call just prepared beyond the conversation, and what it held back
    /// (sent with `ContextMap`; only with the curator; empty when it added nothing).
    Curated {
        items: Vec<CuratedItem>,
    },
    Notice {
        level: Level,
        text: String,
        /// The same failure as a card (title, cause, steps, raw text); `text` is its plain form.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<Box<Problem>>,
    },
    /// A newer nexo is on `main`: how many commits ahead.
    UpdateAvailable {
        behind: u32,
    },
    ModelChanged {
        provider: String,
        model: String,
    },
    /// The setup could not save the model (nothing was changed): why, and what to do.
    SaveFailed {
        name: String,
        model: String,
        reason: String,
    },
    PermissionModeChanged {
        mode: PermissionMode,
    },
    ShellOutput {
        command: String,
        output: String,
        exit_code: Option<i32>,
    },
    TurnEnd {
        reason: TurnEnd,
    },
    /// The transcript starts over (resume, rewind, clear); `History` events follow when there is history.
    HistoryCleared,
    /// The session the agent now writes to.
    SessionChanged {
        session_id: String,
        title: String,
    },
    /// A session went to the trash (`restore` brings it back).
    SessionDeleted {
        id: String,
        title: String,
    },
    /// A session came back from the trash.
    SessionRestored {
        id: String,
        title: String,
    },
    /// The sessions as a tree of forks, parents first: `(depth, id, title)` each.
    SessionTree {
        entries: Vec<(usize, String, String)>,
    },
    /// The sessions in the trash, newest first: `(id, title)` each.
    Trash {
        sessions: Vec<(String, String)>,
    },
    /// What waits to go with the next request, from other sessions: `(id, title, tokens)` each.
    MemoryShared {
        pending: Vec<(String, String, usize)>,
    },
    /// Requests waiting for the running turn to end.
    Queued {
        pending: usize,
    },
    /// The turn ended and nothing follows it (no queued request, goal round or job
    /// notice): the agent waits for the person. Jobs may still run.
    Idle {
        running_jobs: usize,
    },
    /// The MCP servers, sent at the start and whenever one changes (state, tools, resources, prompts).
    Mcp {
        servers: Vec<McpServerInfo>,
    },
    /// The background jobs of the session, sent whenever one starts or ends.
    Jobs {
        jobs: Vec<crate::jobs::JobInfo>,
    },
    /// An item of a sub-agent run's conversation (see [`crate::subs`]). The four `Sub` events are for the
    /// interface only: they are not written to the session and not printed by the headless mode.
    Sub {
        id: String,
        item: crate::subs::SubItem,
    },
    /// Part of the reply a sub-agent run is writing; the whole reply follows as a `Sub` text item.
    SubDelta {
        id: String,
        text: String,
    },
    /// A sub-agent run began, or its state changed.
    SubState {
        info: crate::subs::SubInfo,
    },
    /// A message sent with `SubSubmit` was not delivered.
    SubNotDelivered {
        id: String,
        why: String,
    },
    /// The answer to `SubTranscript`: what the registry keeps of the run, or nothing if it has no such run.
    SubTranscript {
        id: String,
        transcript: Option<crate::subs::SubTranscript>,
    },
    /// The end of the output of each background job, answering `JobTails`.
    JobTails {
        tails: Vec<crate::jobs::JobTail>,
    },
    /// The next message the person is likely to send, answering `Suggest`.
    Suggestion {
        text: String,
    },
    /// The system prompt of this session, as the model receives it.
    SystemPrompt {
        text: String,
    },
    /// What is known of the current model: sent at start, after a model change, after a
    /// correction learned from a provider's error, and on `ShowModel`. `source` says where the
    /// numbers come from (`config`, `preset`, `catalogue`, `vendor`, `error`).
    ModelLimits {
        provider: String,
        model: String,
        pricing: Option<Pricing>,
        context_window: Option<u32>,
        max_output: Option<u32>,
        source: String,
    },
    /// What was spent and the limits: this session, today across every session.
    Budget {
        session_cost: f64,
        day_cost: f64,
        session_limit: Option<f64>,
        day_limit: Option<f64>,
    },
    /// The usual local servers found running: `(preset, what answered)`.
    LocalServers {
        found: Vec<(String, String)>,
    },
    /// The page a sign-in waits on, and the code to type there when it has one (empty: none).
    LoginPrompt {
        url: String,
        code: String,
    },
    /// A sign-in ended: stored, or why not.
    LoggedIn {
        name: String,
        result: Result<(), String>,
    },
    /// What answers at a provider's address, or why nothing does.
    Probed {
        name: String,
        result: Result<ServerProbe, String>,
        #[serde(default)]
        seq: u64,
    },
    /// What a model can do and how the test request went.
    ModelChecked {
        name: String,
        model: String,
        check: ModelCheck,
        #[serde(default)]
        seq: u64,
    },
    /// The reasoning level of the model in use and the levels it takes (empty: it has none
    /// that can be set, or none is known).
    Effort {
        model: String,
        current: Option<String>,
        levels: Vec<String>,
    },
    /// Whether the model in use runs fast: a fast twin, or the provider's fast request option.
    Fast {
        on: bool,
    },
    /// The goal in force (None: cleared), the continuation round out of the maximum, and where it stands.
    Goal {
        text: Option<String>,
        round: u32,
        max: u32,
        #[serde(default)]
        state: GoalState,
    },
    /// What the goal has done so far, sent just before each `Goal` that sets, restates or moves a goal.
    GoalLog(GoalLog),
    /// A mail of this session was written into another's inbox.
    MailSent {
        id: String,
        to: String,
        to_name: String,
        summary: String,
        bytes: usize,
    },
    /// A mail could not be sent; `reason` is written for the model to read ("rate limit, retry in 30 s").
    MailFailed {
        to: String,
        reason: String,
    },
    /// A mail reached this session's inbox; `held` when it waits for the person's approval.
    MailArrived {
        id: String,
        from: String,
        from_name: String,
        summary: String,
        held: bool,
    },
    /// Mails handed to the model (receiver side).
    MailDelivered {
        ids: Vec<String>,
    },
    /// The model started answering the turn that carried these mails (receiver side).
    MailRead {
        ids: Vec<String>,
    },
    /// A mail this session sent moved on the receipt ladder.
    MailStage {
        id: String,
        stage: crate::mail::Stage,
    },
    /// The sessions that can be written to (the answer to `ListPeers`).
    Peers {
        peers: Vec<crate::mail::PeerInfo>,
    },
    /// The history was replaced by a summary of it.
    Compacted {
        before_tokens: u32,
        after_tokens: u32,
    },
    /// A request to the provider failed and is tried again.
    Retrying {
        attempt: u8,
        wait_secs: u32,
        rate_limit: bool,
    },
    /// The todo list changed: items done out of the total.
    TodoChanged {
        done: u16,
        total: u16,
    },
    Shutdown,
}

impl AgentEvent {
    /// One of the events about a sub-agent run's own conversation: for the interface only, never written to
    /// the session and never printed by the headless mode.
    pub fn is_sub(&self) -> bool {
        matches!(
            self,
            AgentEvent::Sub { .. }
                | AgentEvent::SubDelta { .. }
                | AgentEvent::SubState { .. }
                | AgentEvent::SubNotDelivered { .. }
                | AgentEvent::SubTranscript { .. }
        )
    }
}

impl From<crate::subs::SubChange> for AgentEvent {
    fn from(c: crate::subs::SubChange) -> AgentEvent {
        use crate::subs::SubChange;
        match c {
            SubChange::State(info) => AgentEvent::SubState { info },
            SubChange::Item { id, item } => AgentEvent::Sub { id, item },
            SubChange::Delta { id, text } => AgentEvent::SubDelta { id, text },
            SubChange::NotDelivered { id, why } => AgentEvent::SubNotDelivered { id, why },
        }
    }
}

pub type CommandTx = mpsc::UnboundedSender<AgentCommand>;
pub type CommandRx = mpsc::UnboundedReceiver<AgentCommand>;
pub type EventTx = mpsc::UnboundedSender<AgentEvent>;
pub type EventRx = mpsc::UnboundedReceiver<AgentEvent>;

/// The two channels of an agent, from the driver's side.
pub struct AgentHandle {
    pub commands: CommandTx,
    pub events: EventRx,
}

/// The two channels of an agent, from the agent's side.
pub struct AgentEnds {
    pub commands: CommandRx,
    pub events: EventTx,
}

/// Makes the pair.
pub fn channels() -> (AgentHandle, AgentEnds) {
    let (ctx, crx) = mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded_channel();
    (AgentHandle { commands: ctx, events: erx }, AgentEnds { commands: crx, events: etx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subs::SubItem;

    #[test]
    fn sub_events_are_marked_and_the_others_are_not() {
        let item = SubItem::Task { text: "t".into() };
        let subs = [
            AgentEvent::Sub { id: "a#1".into(), item },
            AgentEvent::SubDelta { id: "a#1".into(), text: "x".into() },
            AgentEvent::SubNotDelivered { id: "a#1".into(), why: "it ended".into() },
        ];
        assert!(subs.iter().all(AgentEvent::is_sub));
        assert!(!AgentEvent::Shutdown.is_sub());
        assert!(!AgentEvent::Jobs { jobs: vec![] }.is_sub());
    }
}
