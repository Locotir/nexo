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
    /// Write the conversation as markdown (`path` None: next to the session file); answered with a `Notice`.
    Export {
        path: Option<PathBuf>,
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
    /// Find out what one model can do and try it with a real request (one short reply that
    /// should call a test tool); answered with `ModelChecked`.
    CheckModel {
        name: String,
        probe: ServerProbe,
        cfg: ProviderConfig,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
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
    /// Stop background job `id`, or every running one with None; answered with `Jobs`.
    KillJob {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
    },
    /// Ask for the background jobs; answered with `Jobs` and a `Notice`.
    ShowJobs,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
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
        /// What the policy is doing, for the title bar: "curator −38%",
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
    },
    Notice {
        level: Level,
        text: String,
    },
    /// A newer nexo is on `main`: how many commits ahead.
    UpdateAvailable {
        behind: u32,
    },
    ModelChanged {
        provider: String,
        model: String,
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
    /// The background jobs of the session, sent whenever one starts or ends.
    Jobs {
        jobs: Vec<crate::jobs::JobInfo>,
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
    },
    /// What a model can do and how the test request went.
    ModelChecked {
        name: String,
        model: String,
        check: ModelCheck,
    },
    /// The reasoning level of the model in use and the levels it takes (empty: it has none
    /// that can be set, or none is known).
    Effort {
        model: String,
        current: Option<String>,
        levels: Vec<String>,
    },
    /// The goal in force (None: cleared) and the continuation round out of the maximum.
    Goal {
        text: Option<String>,
        round: u32,
        max: u32,
    },
    Shutdown,
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
