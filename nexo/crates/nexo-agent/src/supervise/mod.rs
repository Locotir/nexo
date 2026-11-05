//! The supervisor's pure core: what is known about each session ([`AgentTrack`], built from
//! [`Observation`]s that any source produces), the state it is in ([`classify`]), and the rules the person
//! set ([`policy`]). Nothing in here reads a file of a session, opens a socket or calls a model; sources and
//! actions are layered on top and talk to this core only through these types.

pub mod act;
pub mod alert;
pub mod audit;
pub mod classify;
pub mod decide;
pub mod install;
pub mod model;
pub mod policy;
pub mod run;
pub mod socket;
pub mod source;
pub mod source_jsonl;
pub mod source_list;
pub mod source_nexo;

use std::collections::VecDeque;

pub use classify::{classify, ends_in_question, loop_signal, LoopWhy};
pub use policy::{Loaded, Policy, Verdict};
pub use run::{AgentRow, Options, Panel, Setup, Supervisor};
pub use source::{Source, SourceKind};
pub use source_nexo::NexoSource;

/// Seconds since the Unix epoch.
pub type Secs = i64;

/// The state a session is in, as the supervisor shows and acts on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Done,
    WaitingPermission,
    WaitingAnswer,
    Looping,
    Stalled,
    /// Quiet for this many whole minutes.
    Idle {
        min: u64,
    },
    Working,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Done => "done",
            State::WaitingPermission => "permission",
            State::WaitingAnswer => "question",
            State::Looping => "looping",
            State::Stalled => "stalled",
            State::Idle { .. } => "idle",
            State::Working => "working",
        }
    }
}

/// What a session's own process manager says about it, when a source can ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reported {
    Working,
    Blocked,
    Done,
}

/// One thing seen in a session, with the moment it happened. Sources turn their own formats into these.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    /// Stable id of the session; a sub-agent's activity is reported under its parent's key.
    pub agent_key: String,
    pub at: Secs,
    pub kind: ObsKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObsKind {
    /// Any record of life that changes nothing else (a log line, a sub-agent step, a timing record).
    Activity,
    /// The person wrote in the session: new work begins, and they are present.
    UserInput,
    ToolPending {
        name: String,
    },
    ToolResult {
        name: String,
        input_hash: u64,
        is_error: bool,
    },
    /// A turn ended with this tail of the final text.
    TurnEnd {
        last_text_tail: String,
    },
    /// A question to the person is open (explicit, or an idle prompt).
    AskPending,
    AskResolved,
    PermissionPending {
        tool: String,
    },
    PermissionResolved,
    /// The session runs on a schedule with this period.
    Loop {
        cron_secs: u64,
    },
    /// An error level record.
    SystemError,
    Done,
    /// The process is gone.
    Dead,
    Reported(Reported),
    /// What the session is doing, in one short redacted line (a tool and its gist).
    Step {
        text: String,
    },
    /// What the session has cost so far and the model it runs; not a sign of life.
    Spend {
        usd: f64,
        model: Option<String>,
    },
    /// Names and settings; not a sign of life.
    Meta {
        name: Option<String>,
        cwd: Option<String>,
        permission_mode: Option<String>,
    },
}

/// A tool result kept for the loop signal.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultMark {
    pub name: String,
    pub input_hash: u64,
    pub is_error: bool,
}

/// How many recent results the loop signal looks at.
pub const RESULT_WINDOW: usize = 8;
const ERROR_KEEP: usize = 32;

/// Everything known about one session, folded from its observations.
#[derive(Clone, Debug, Default)]
pub struct AgentTrack {
    pub key: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub permission_mode: Option<String>,
    pub last_activity: Option<Secs>,
    /// The person's last message.
    pub last_user: Option<Secs>,
    /// Tools asked and not yet answered: name and moment, oldest first.
    pub pending: Vec<(String, Secs)>,
    pub results: VecDeque<ResultMark>,
    /// Moments of error level records, oldest first.
    pub errors: VecDeque<Secs>,
    /// A turn ended at this moment with this tail of text, and nothing has happened since.
    pub turn_end: Option<(Secs, String)>,
    pub permission_pending: Option<(String, Secs)>,
    pub ask_pending: Option<Secs>,
    pub loop_secs: Option<u64>,
    pub reported: Option<Reported>,
    /// The latest step, redacted and short.
    pub last_step: Option<String>,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub done: bool,
    pub dead: bool,
}

impl AgentTrack {
    pub fn new(key: impl Into<String>) -> Self {
        AgentTrack { key: key.into(), ..Default::default() }
    }

    /// Fold one observation in. Order of arrival does not move the activity clock backwards.
    pub fn apply(&mut self, o: &Observation) {
        let at = o.at;
        let alive =
            !matches!(o.kind, ObsKind::Meta { .. } | ObsKind::Reported(_) | ObsKind::Dead | ObsKind::Spend { .. });
        if alive {
            self.last_activity = Some(self.last_activity.map_or(at, |t| t.max(at)));
        }
        match &o.kind {
            ObsKind::Activity => {}
            ObsKind::UserInput => {
                self.begin_work();
                self.last_user = Some(at);
                self.ask_pending = None;
                self.permission_pending = None;
            }
            ObsKind::ToolPending { name } => {
                self.begin_work();
                self.pending.push((name.clone(), at));
            }
            ObsKind::ToolResult { name, input_hash, is_error } => {
                self.begin_work();
                if let Some(i) = self.pending.iter().position(|(n, _)| n == name) {
                    self.pending.remove(i);
                }
                self.permission_pending = None;
                if name == "AskUserQuestion" {
                    self.ask_pending = None;
                }
                self.results.push_back(ResultMark { name: name.clone(), input_hash: *input_hash, is_error: *is_error });
                while self.results.len() > RESULT_WINDOW {
                    self.results.pop_front();
                }
            }
            ObsKind::TurnEnd { last_text_tail } => {
                self.pending.clear();
                self.permission_pending = None;
                self.turn_end = Some((at, last_text_tail.clone()));
            }
            ObsKind::AskPending => self.ask_pending = Some(at),
            ObsKind::AskResolved => self.ask_pending = None,
            ObsKind::PermissionPending { tool } => self.permission_pending = Some((tool.clone(), at)),
            ObsKind::PermissionResolved => self.permission_pending = None,
            ObsKind::Loop { cron_secs } => self.loop_secs = Some(*cron_secs),
            ObsKind::SystemError => {
                self.errors.push_back(at);
                while self.errors.len() > ERROR_KEEP {
                    self.errors.pop_front();
                }
            }
            ObsKind::Done => self.done = true,
            ObsKind::Dead => self.dead = true,
            ObsKind::Reported(r) => self.reported = Some(*r),
            ObsKind::Step { text } => self.last_step = Some(text.clone()),
            ObsKind::Spend { usd, model } => {
                self.cost_usd = Some(*usd);
                if model.is_some() {
                    self.model = model.clone();
                }
            }
            ObsKind::Meta { name, cwd, permission_mode } => {
                if name.is_some() {
                    self.name = name.clone();
                }
                if cwd.is_some() {
                    self.cwd = cwd.clone();
                }
                if permission_mode.is_some() {
                    self.permission_mode = permission_mode.clone();
                }
            }
        }
    }

    /// New work starts: the last turn is over for good and the session is not finished.
    fn begin_work(&mut self) {
        self.turn_end = None;
        self.done = false;
    }

    /// Seconds without activity; None when nothing was ever seen.
    pub fn idle_secs(&self, now: Secs) -> Option<u64> {
        self.last_activity.map(|t| (now - t).max(0) as u64)
    }

    /// Age in seconds of the oldest tool still waiting for its result.
    pub fn oldest_pending_age(&self, now: Secs) -> Option<u64> {
        self.pending.iter().map(|(_, t)| (now - t).max(0) as u64).max()
    }

    /// The session cannot ask for permission: it runs with every tool allowed.
    pub fn bypasses_permission(&self) -> bool {
        self.permission_mode.as_deref().is_some_and(|m| m.to_ascii_lowercase().contains("bypass"))
    }
}

/// A stable 64-bit hash (FNV-1a) of a tool's input, for sources that must tell "the same call" apart.
pub fn input_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}
