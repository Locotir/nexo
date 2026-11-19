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
/// Where the loop's own goal mark ([`GOAL_SET_MARK`] or [`GOAL_CONTINUE_MARK`]) begins in the text of a message
/// nexo sent: at the start, or after a blank line (the notes for jobs, hooks, memory and a stop line go before the
/// mark). `None` when the message is no goal round. The first such mark counts.
pub fn goal_mark_at(text: &str) -> Option<usize> {
    text.match_indices('[').map(|(i, _)| i).find(|&at| {
        (at == 0 || text[..at].ends_with("\n\n"))
            && [GOAL_SET_MARK, GOAL_CONTINUE_MARK].iter().any(|m| text[at..].starts_with(m))
    })
}

/// The first line of the note nexo puts at the head of a request (what changed in the project, what its check said).
/// It is nexo's own text, never the person's. The curator has a copy (`nexo_curator::START_NOTE_HEAD`).
pub const START_NOTE_HEAD: &str = "[project at the start of this request]";

/// The goal request in the text of a message nexo sent: from the loop's goal mark ([`goal_mark_at`]) to the first blank
/// line that is followed by the start note ([`START_NOTE_HEAD`]), a file nexo or the person put in the request
/// ([`headed_file`]) or a `@path` line that says the file was an image or was not attached, else to the end; trimmed.
/// What follows it (the start note and the files) is nexo's, not the goal's words. `None` when the message is no goal
/// round. The curator has the same rule (`nexo_curator` msg.rs).
pub fn goal_request(text: &str) -> Option<&str> {
    let rest = &text[goal_mark_at(text)?..];
    let end = rest
        .match_indices("\n\n")
        .map(|(i, _)| i)
        .find(|&i| {
            let after = &rest[i + 2..];
            let line = after.lines().next().unwrap_or("");
            let not_in = line.contains(": attached as an image") || line.contains(": not attached");
            after.starts_with(START_NOTE_HEAD) || headed_file(line).is_some() || (line.starts_with('@') && not_in)
        })
        .unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// How the line starts that says a goal that was met is not running; the writer (`goal_state_line`) and the readers
/// ([`goal_state_line_of`]) both use it. The curator has a copy (`nexo_curator` msg.rs).
pub const GOAL_MET_HEAD: &str = "[goal: met, not running";
/// How the line starts that says a goal is paused, with the reason after it.
pub const GOAL_PAUSED_HEAD: &str = "[goal: paused (";

/// The goal-state line `text` starts with, if it starts with one: the first line, when it begins with
/// [`GOAL_MET_HEAD`] or [`GOAL_PAUSED_HEAD`] and ends with `]`. nexo puts it before a request it starts by itself
/// (after the stop line), so that what it sits in front of is not read as the person's words.
pub fn goal_state_line_of(text: &str) -> Option<&str> {
    let first = text.split('\n').next()?;
    ((first.starts_with(GOAL_MET_HEAD) || first.starts_with(GOAL_PAUSED_HEAD)) && first.ends_with(']')).then_some(first)
}

/// The most characters of a goal a line of the screen carries: a few lines, the rest is in the goal window.
pub const GOAL_SHOWN: usize = 200;

/// A goal as one notice line shows it, on one line. `whole_in_window` says the goal window holds these very words
/// (they are the goal it has now): then the line is cut to [`GOAL_SHOWN`] characters with where the rest is. Words
/// the window does not hold (an earlier goal, replayed) are shown whole: there is nowhere else to read the rest.
pub fn goal_shown(goal: &str, whole_in_window: bool) -> String {
    let one = goal.split_whitespace().collect::<Vec<_>>().join(" ");
    if !whole_in_window || one.chars().count() <= GOAL_SHOWN {
        return one;
    }
    let cut: String = one.chars().take(GOAL_SHOWN).collect();
    format!("{}… · the rest is in the goal window", cut.trim_end())
}

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

/// What a goal has run so far, the part of [`GoalLog`] that is saved with the session: when it was set, the tool calls
/// made for it and the times it stopped short. The spend and the latest reply are not kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalProgress {
    /// When the goal was set (unix ms); 0 when unknown.
    pub started_ms: i64,
    pub steps: u32,
    pub stops: Vec<GoalStop>,
}

impl GoalLog {
    /// The part of this log that is saved with the session.
    pub fn progress(&self) -> GoalProgress {
        GoalProgress { started_ms: self.started_ms, steps: self.steps, stops: self.stops.clone() }
    }

    /// This log with the progress saved earlier put back (the words and the restatement stay as they are).
    pub fn with_progress(self, p: GoalProgress) -> GoalLog {
        GoalLog { started_ms: p.started_ms, steps: p.steps, stops: p.stops, ..self }
    }
}

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
