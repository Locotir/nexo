use super::*;

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
    /// What the agent does for a request before its reply starts, in plain words ("preparing the request",
    /// "checking the project (cargo test)", "waiting for the model"): sent the moment it takes the request and
    /// at each slow step after, so the interface never shows a silent wait. Ends with the reply's first event.
    Preparing {
        what: String,
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
    /// Every model `/model` offers now, `(provider, model)`: the vendors' lists and the catalogue came in
    /// (or were read again), so a model added since the start shows up without a restart.
    ModelsListed {
        models: Vec<(String, String)>,
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
    /// A workflow run began, or something in it changed (a step, a sub-agent, the tokens spent, the end); the
    /// whole run as it stands. Like the `Sub` events it is for the interface only: never written to the session
    /// and never printed by the headless mode. No report text is in it.
    FlowRun {
        info: crate::subs::FlowInfo,
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
                | AgentEvent::FlowRun { .. }
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
            SubChange::Flow(info) => AgentEvent::FlowRun { info },
        }
    }
}
