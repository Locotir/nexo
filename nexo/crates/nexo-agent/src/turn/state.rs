use super::*;

pub(super) struct State {
    pub(super) config: Config,
    pub(super) cwd: PathBuf,
    pub(super) tools: ToolSet,
    pub(super) client: Arc<dyn ModelClient>,
    pub(super) target: Target,
    pub(super) session: Session,
    /// What the session sends before its messages, fixed for its life (see `prefix.rs`).
    pub(super) prefix: crate::session::Prefix,
    pub(super) history: crate::history::History,
    pub(super) policy: Box<dyn ContextPolicy>,
    /// The tokens of the history and of the last view sent, for /status.
    pub(super) last_view: (u64, u64),
    /// The messages of the last request, to tell which part of the next one repeats it (the cache).
    pub(super) last_sent: Vec<Message>,
    /// The last request the conversation sent, which a prompt suggestion repeats (the provider's cache) with one message more.
    pub(super) last_request: Option<ModelRequest>,
    /// The mends of a broken history already said (each is said once).
    pub(super) mended: HashSet<String>,
    pub(super) perm: Policy,
    /// The OS sandbox of the `bash` tool (`/sandbox` changes its mode).
    pub(super) sandbox: nexo_core::sandbox::SandboxSettings,
    pub(super) gate: Arc<dyn PermissionGate>,
    /// No person is there to answer (`nexo -p`, a script): what a person would be left to decide is decided here.
    pub(super) alone: bool,
    pub(super) events: EventTx,
    pub(super) usage: Usage,
    pub(super) cost: f64,
    pub(super) system: String,
    /// What is known of the model in use (prices, window, largest reply).
    pub(super) limits: Limits,
    /// `/fast` asked for the provider's fast request option (it counts only where the provider has one).
    pub(super) fast: bool,
    /// The models the provider listed when `/fast` last asked, with its name.
    pub(super) live: Option<(String, Vec<String>)>,
    /// Where the request in progress starts in `history`.
    pub(super) turn_start: usize,
    pub(super) turns: u32,
    /// The goal the agent keeps working towards, and the continuation round in progress.
    pub(super) goal: Option<String>,
    pub(super) goal_round: u32,
    /// Where the goal stands: only an active goal is continued when a turn ends.
    pub(super) goal_state: GoalState,
    /// What the goal panel shows: both wordings of the goal, its stops, steps and spend (see `goal_log`).
    pub(super) goal_log: GoalLog,
    /// The session's cost when the goal was set, to tell what the goal spent.
    pub(super) goal_cost0: f64,
    /// What files looked like before the agent wrote them: `(history length at the time, file,
    /// its bytes then or None when it did not exist)`, so a rewind can put them back.
    pub(super) checkpoints: Vec<(usize, PathBuf, Option<Vec<u8>>)>,
    /// The spend limit of this session (from `[agent] max_cost`, changed by `/budget`).
    pub(super) budget: Option<f64>,
    /// The 80% warning was given.
    pub(super) budget_warned: bool,
    /// Checkpoints taken since the person last wrote (`agent.on_limit`); a goal's rounds, a mail or a job's end do not reset it.
    pub(super) ck_used: u32,
    /// Why the last request ended at a checkpoint cap or for lack of progress, for a goal's pause.
    pub(super) ck_stop: Option<String>,
    /// Messages sent while a turn runs: they join it before its next model call.
    pub(super) inbox: VecDeque<(String, Vec<Attachment>)>,
    /// What other sessions know, waiting to go with the next request: `(id, title, block)`.
    pub(super) shared: Vec<(String, String, String)>,
    /// Files the model has seen, so an unchanged one is not sent again.
    pub(super) reads: Reads,
    /// When the last requests ran (start, end): a file changed inside one was made by the model's own work.
    pub(super) worked: VecDeque<(SystemTime, SystemTime)>,
    /// The notes of the reader passes so far, by material and request.
    pub(super) digests: HashMap<u64, String>,
    /// Commands and sub-agents running in the background.
    pub(super) jobs: Jobs,
    /// The files running sub-agents are changing (the main agent's writes respect them).
    pub(super) claims: nexo_core::claims::Claims,
    /// Sends the sub-agents a round waits for to the background.
    pub(super) detach: nexo_core::jobs::Detach,
    /// The files the model has read or written, as they were then.
    pub(super) seen: nexo_core::seen::Seen,
    /// What the step-level check remembers: commands too slow to repeat, and the last check's report.
    pub(super) post: Arc<nexo_tools::postcheck::Guard>,
    /// Failures of the session kept by what they were on, to say when one comes back.
    pub(super) repeats: crate::repeat::Repeats,
    /// Whether the line about `nexo bin` is in this session (see `bin_hint.rs`): kept through a compaction, worked
    /// out again from the history when it is replaced, rewound or cleared.
    pub(super) bin_told: bool,
    /// The same for the lines that name a `debug` call, one per kind (see `dbg_hint.rs`).
    pub(super) dbg_told: crate::dbg_hint::Told,
    /// The same for the line in a start note that names a compiled program's tools (see `ask_hint.rs`).
    pub(super) ask_told: bool,
    /// Bumped when the conversation is cleared, rewound or switched, so a reply asked for before is let go.
    pub(super) epoch: u64,
    /// The one-run slot of a workflow started by name: 0 when free, else the number of the start that holds it.
    /// It starts over with the conversation (see `flow`).
    pub(super) flow_slot: Arc<std::sync::atomic::AtomicU64>,
    /// Plan mode: the mode to go back to and the switch an approved plan asks for.
    pub(super) plan: crate::ask::PlanCell,
    /// The size of the last request as the provider counted it (its prompt and reply tokens) and the
    /// length of the history then: what the `compact` policy measures the next request from.
    pub(super) last_prompt: Option<(u64, usize)>,
    /// The estimate (4 characters a token) of the prompt of the last request, and the provider's
    /// tokens per estimated token learned so far: code, logs and other scripts hold more tokens than
    /// 4 characters each, so the view is fitted to the window in the provider's own count.
    pub(super) est_sent: u64,
    pub(super) tok_ratio: Option<f64>,
    /// The last call the provider billed in this session: a call cut before its usage event is estimated from it.
    pub(super) last_call: Option<Usage>,
    /// The mailbox other sessions write to, and the mails that reached the model: those waiting for the turn
    /// in progress to end, and those the turn in progress carries (told "read" when the model starts answering).
    pub(super) mail: Option<Arc<Mailbox>>,
    /// The MCP servers, and the hub count the tool set was last brought up to date with.
    pub(super) mcp: Option<Arc<nexo_tools::mcp_hub::McpHub>>,
    pub(super) mcp_gen: u64,
    pub(super) mail_wait: Vec<Mail>,
    pub(super) mail_turn: Vec<Mail>,
    /// Text hooks added for the model (SessionStart, UserPromptSubmit): it goes with the next request.
    pub(super) hook_context: Vec<String>,
    /// Notes the supervisor's watch wants on the NEXT model request only (never saved in the conversation), and
    /// the findings of this request already said, so each is said once.
    pub(super) hidden: Vec<String>,
    /// How many of `hidden` the interface was told of (they ride on the request, which `/sent` shows).
    pub(super) hidden_told: usize,
    pub(super) noticed: HashSet<String>,
    /// A check-in was made for this request of the person (at most one; the person speaking opens it again).
    pub(super) checked_in: bool,
    /// How many times `agent.max_tokens` a reply may take in this request: doubled (up to 4) when a reply
    /// was cut at max_tokens while still thinking, as the reasoning had no room left for an answer or a call.
    pub(super) reply_boost: u32,
    /// The audit log writer, when `[audit] enabled`.
    pub(super) audit: Option<Audit>,
}

pub(super) type Shared = Arc<Mutex<State>>;

/// Which of the tools the hints name this session offers.
pub(super) fn offer_of(st: &State) -> crate::hint_cmd::Offer {
    crate::hint_cmd::Offer { bin: st.tools.get("bin").is_some(), debug: st.tools.get("debug").is_some() }
}
