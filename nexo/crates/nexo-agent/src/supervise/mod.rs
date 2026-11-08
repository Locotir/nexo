//! The supervisor's pure core: what is known about each session ([`AgentTrack`], built from
//! [`Observation`]s that any source produces), the state it is in ([`classify`]), and the rules the person
//! set ([`policy`]). Nothing in here reads a file of a session, opens a socket or calls a model; sources and
//! actions are layered on top and talk to this core only through these types.

pub mod act;
pub mod alert;
pub mod audit;
pub mod classify;
pub mod decide;
pub mod events;
pub mod install;
pub mod model;
pub mod policy;
pub mod run;
pub mod socket;
pub mod source;
pub mod source_jsonl;
pub mod source_list;
pub mod source_mail;
pub mod source_nexo;
pub mod watch;

use std::collections::VecDeque;

pub use events::{Ev, EvDetail, EvKind};
use nexo_core::mail::Stage;

pub use classify::{classify, ends_in_question, loop_signal, LoopWhy};
pub use policy::{Loaded, Policy, Verdict};
pub use run::{AgentRow, MailRow, Options, Panel, Setup, Supervisor};
pub use source::{Source, SourceKind};
pub use source_nexo::{NexoSource, Nudge};

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
    /// An event the older kinds cannot say (test results, commits, compactions, retries, the todo list).
    Ev(Ev),
    /// The session wrote to another one.
    MsgOut {
        to: String,
        mail: Option<String>,
        summary: String,
        bytes: usize,
    },
    /// Another session wrote to this one; `held` while it waits for approval.
    MsgIn {
        from: String,
        mail: Option<String>,
        summary: String,
        held: bool,
    },
    /// A mail moved on the receipt ladder.
    MsgStage {
        mail: String,
        stage: Stage,
    },
}

impl ObsKind {
    /// Whether this says the session is alive and doing something (names, settings, spend and reports of a
    /// process manager do not).
    pub fn is_sign_of_life(&self) -> bool {
        !matches!(
            self,
            ObsKind::Meta { .. }
                | ObsKind::Reported(_)
                | ObsKind::Dead
                | ObsKind::Spend { .. }
                | ObsKind::MsgStage { .. }
        )
    }
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
    /// The latest events, newest last.
    pub ribbon: VecDeque<Ev>,
    /// Tokens produced, in per-minute buckets (minute number, tokens), oldest first.
    pub tokens: VecDeque<(Secs, u32)>,
    /// Todo items done and in all.
    pub todo: Option<(u16, u16)>,
    /// The last test run: passed, failed, when.
    pub tests: Option<(u32, u32, Secs)>,
    /// The provider is being retried until this moment.
    pub retry_until: Option<Secs>,
    /// Mails received and not yet handed to the model: id and whether it is held for approval.
    pub inbox: Vec<(String, bool)>,
    /// Mail events already noted (two sources may tell the same one).
    seen_mail: VecDeque<String>,
    /// When the process was last reported gone.
    dead_at: Option<Secs>,
}

impl AgentTrack {
    pub fn new(key: impl Into<String>) -> Self {
        AgentTrack { key: key.into(), ..Default::default() }
    }

    /// Fold one observation in. Order of arrival does not move the activity clock backwards.
    pub fn apply(&mut self, o: &Observation) {
        let at = o.at;
        let alive = o.kind.is_sign_of_life();
        if alive {
            if self.dead_at.is_some_and(|d| at > d) {
                // it writes again after its process was reported gone: it came back
                self.dead = false;
                self.dead_at = None;
            }
            self.last_activity = Some(self.last_activity.map_or(at, |t| t.max(at)));
        }
        if !self.seen_mail_event(&o.kind) {
            if let Some(ev) = events::derive(&o.kind, at) {
                self.note(ev);
            }
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
            ObsKind::Dead => {
                self.dead = true;
                self.dead_at = Some(self.dead_at.map_or(at, |d| d.max(at)));
            }
            ObsKind::Reported(r) => self.reported = Some(*r),
            ObsKind::Step { text } => self.last_step = Some(source::clean_step(text)),
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
            ObsKind::Ev(_) => {}
            ObsKind::MsgOut { .. } => {}
            ObsKind::MsgIn { mail, held, .. } => {
                if let Some(id) = mail {
                    if !self.inbox.iter().any(|(m, _)| m == id) {
                        self.inbox.push((id.clone(), *held));
                    }
                    while self.inbox.len() > 50 {
                        self.inbox.remove(0);
                    }
                }
            }
            ObsKind::MsgStage { mail, stage } => match stage {
                Stage::Held => {
                    if let Some(m) = self.inbox.iter_mut().find(|(m, _)| m == mail) {
                        m.1 = true;
                    }
                }
                Stage::Sent => {}
                _ => self.inbox.retain(|(m, _)| m != mail),
            },
        }
    }

    /// Whether this mail observation was already noted; notes it when not.
    fn seen_mail_event(&mut self, kind: &ObsKind) -> bool {
        let key = match kind {
            ObsKind::MsgOut { mail: Some(id), .. } => format!("o:{id}"),
            ObsKind::MsgIn { mail: Some(id), .. } => format!("i:{id}"),
            ObsKind::MsgStage { mail, stage } => format!("s:{mail}:{}", stage.label()),
            _ => return false,
        };
        if self.seen_mail.contains(&key) {
            return true;
        }
        self.seen_mail.push_back(key);
        while self.seen_mail.len() > 128 {
            self.seen_mail.pop_front();
        }
        false
    }

    /// Keep an event and what it says besides: the tests, the todo bar, the retry wait, the token buckets.
    fn note(&mut self, ev: Ev) {
        match (&ev.kind, &ev.detail) {
            (EvKind::TestsPass | EvKind::TestsFail, EvDetail::Tests { pass, fail }) => {
                self.tests = Some((*pass, *fail, ev.at));
            }
            (EvKind::Todo, EvDetail::Progress { done, total }) => self.todo = Some((*done, *total)),
            (EvKind::Retry | EvKind::RateLimit, EvDetail::Wait { secs }) => {
                self.retry_until = Some(ev.at + *secs as Secs)
            }
            (EvKind::Cost, EvDetail::Tokens(n)) => {
                let minute = ev.at.div_euclid(60);
                match self.tokens.back_mut() {
                    Some(b) if b.0 == minute => b.1 = b.1.saturating_add(*n),
                    _ => self.tokens.push_back((minute, *n)),
                }
                while self.tokens.len() > 30 {
                    self.tokens.pop_front();
                }
                return;
            }
            _ => {}
        }
        if ev.kind == EvKind::TurnStart || ev.kind == EvKind::TurnEnd {
            self.retry_until = None;
        }
        events::push(&mut self.ribbon, ev);
    }

    /// New work starts: the last turn is over for good and the session is not finished.
    fn begin_work(&mut self) {
        self.turn_end = None;
        self.done = false;
    }

    /// When the process was last reported gone, while it is.
    pub fn dead_since(&self) -> Option<Secs> {
        self.dead_at.filter(|_| self.dead)
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
