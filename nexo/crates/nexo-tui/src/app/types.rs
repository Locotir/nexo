//! The plain data types of the app: what a session keeps about files, context, the goal, searches and hits.

use nexo_core::agent::GoalState;
use nexo_core::pricing::Pricing;

use crate::scrollbar;

/// A file this session changed, with the lines added and removed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    pub view_tokens: u64,
    pub history_tokens: u64,
    /// Bytes of the whole history (the memory kept) and of what the next call sends.
    pub history_bytes: u64,
    pub view_bytes: u64,
    pub messages_in_view: usize,
    pub messages_total: usize,
    pub policy: String,
    pub note: Option<String>,
    /// What the policy is doing ("curator", "curator -12k tok", "curator idle") and why.
    pub label: String,
    pub detail: Option<String>,
}

/// What is known of the model in use (from the agent's `ModelLimits`).
/// The goal in force as the agent last told it.
#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub text: String,
    /// The continuation round in progress (0: none) and the most there are.
    pub round: u32,
    pub max: u32,
    pub state: GoalState,
}

impl Goal {
    /// The goal in one line: what the agent restated the person wants, or their own words.
    pub fn headline(&self) -> &str {
        let first = self.text.lines().next().unwrap_or_default();
        first.strip_prefix(nexo_core::agent::GOAL_BRIEF_HEAD).unwrap_or(first)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub pricing: Option<Pricing>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Search {
    pub query: String,
    /// The bar is open and takes the keys.
    pub focused: bool,
    /// Absolute line indices of the matches.
    pub matches: Vec<usize>,
    pub current: usize,
    /// Group blocks whose hit stands for calls the fold leaves out (one hit per group).
    pub groups: Vec<usize>,
}

/// A piece of the status bar that answers a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusHit {
    /// The model: `/model`.
    Model,
    /// The reasoning level: `/effort`.
    Effort,
    /// The fast-mode marker: `/fast`.
    Fast,
    /// The permission mode: its picker (`/permissions`).
    Mode,
    /// The context gauge: `/context`.
    Gauge,
    /// The tokens billed and the cost: `/usage`.
    Cost,
    /// The goal: its window.
    Goal,
    /// The to-do count: `/todo`.
    Todo,
}

/// A piece of the keys row that answers a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintHit {
    /// `todo d/t`: `/todo`.
    Todo,
    /// `⚙ N jobs`: `/jobs`.
    Jobs,
    /// `⊕ mem`: `/shared-memory list`.
    Mem,
    /// `◷ N queued`: `/queue`.
    Queued,
    /// The session id or title: `/resume`.
    Session,
}

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hover {
    #[default]
    Nothing,
    /// A scrollbar (its thumb grows and lights up).
    Bar(scrollbar::Target),
    /// The "↓ new" pill.
    Pill,
    Status(StatusHit),
    /// A piece of the keys row (todo, jobs, memory, queue, session).
    Hint(HintHit),
    /// A row of the palette, of a picker or of a question.
    Palette(usize),
    Picker(usize),
    /// The first line of a block that folds (a click there folds or opens it).
    Head(usize),
}

/// What a subscription's usage would have cost on the API, worded as included in the plan.
pub fn plan_value(usd: f64) -> String {
    format!("≈${usd:.4} API value · included in plan")
}
