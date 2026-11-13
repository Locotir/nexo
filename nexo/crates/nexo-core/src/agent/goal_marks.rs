use super::*;

/// Around what hooks added for the model in front of a request: the screen shows it as a line, not as
/// the person's words.
pub const HOOK_CONTEXT_START: &str = "<hook-context>";
/// The line before the person's own words when nexo put anything in their message (notes, hook context, files
/// attached): what nexo adds goes first, and the request is the last thing the model reads. The curator has the same
/// line (`nexo_curator::PERSON_MARK`).
pub const PERSON_MARK: &str = "[the person's message]";

/// The person's own words in the text of a message they sent: what follows the last [`PERSON_MARK`] line, else the
/// whole text (a message nexo added nothing to, or one saved before the line existed).
pub fn person_words(text: &str) -> &str {
    let line = format!("\n{PERSON_MARK}\n");
    match text.rfind(&line) {
        Some(i) => &text[i + line.len()..],
        None => text.strip_prefix(PERSON_MARK).and_then(|r| r.strip_prefix('\n')).unwrap_or(text),
    }
}
pub const HOOK_CONTEXT_END: &str = "</hook-context>";

/// The end of the line that heads a file the person attached with `@path`: `@path (attached in full, as it is now):`.
pub const ATTACHED: &str = "(attached in full, as it is now):";
/// The end of the line that heads a project file nexo put in a request by itself (one the message names, or a small
/// one within `agent.attach_budget`): `path (project file, included in full by nexo, as it is now):`. It has no `@`
/// and says who put it there, so it never reads as something the person handed over.
pub const INCLUDED: &str = "(project file, included in full by nexo, as it is now):";

/// The path `line` heads when it is the first line of a file put whole in a request: the person's `@path`
/// attachment ([`ATTACHED`]) or a project file nexo included by itself ([`INCLUDED`]).
pub fn headed_file(line: &str) -> Option<&str> {
    let path = match line.strip_suffix(ATTACHED) {
        Some(l) => l.strip_prefix('@')?,
        None => line.strip_suffix(INCLUDED)?,
    };
    Some(path.trim_end()).filter(|p| !p.is_empty())
}

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
