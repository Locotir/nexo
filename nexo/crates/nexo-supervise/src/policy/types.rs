//! The shapes of the policy files: the policy, its sections, the verdict of a check and what loading returns.

use serde::{Deserialize, Serialize};

/// The whole policy. Every field has a default, so an empty file is a valid one; a misspelt key is an error,
/// because a rule that silently does nothing is worse than none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub enabled: bool,
    /// Seconds between rounds of the rules.
    pub interval_secs: u64,
    /// How far back, in hours, session logs are looked at.
    pub scan_hours: u64,
    /// A cheap model for the few cases rules cannot settle; none: rules only.
    pub model: Option<ModelPolicy>,
    pub thresholds: Thresholds,
    pub nudge: Nudge,
    pub approve: Rules,
    pub deny: Rules,
    pub alert: Alert,
    #[serde(rename = "agent")]
    pub agents: Vec<AgentRule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPolicy {
    pub provider: String,
    pub name: String,
    pub max_usd_per_hour: f64,
    pub max_calls_per_hour: u32,
    pub max_input_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub idle_min: u64,
    pub stall_min: u64,
    /// A tool running for less than this many minutes is a long job, not a stop.
    pub tool_grace_min: u64,
    pub alert_after_min: u64,
    /// A tool without a result for this many seconds, in a blocked session, is waiting for permission.
    pub permission_secs: u64,
    /// A final question counts as unanswered after this many seconds.
    pub answer_secs: u64,
    /// Phrases (any case) that make the end of a reply a question without a question mark.
    pub ask_patterns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Nudge {
    pub answer_questions: bool,
    pub nudges_per_hour: u32,
    pub nudge_cooldown_min: u64,
    /// Texts; `{idle}`, `{errors}` and `{tool}` are filled in.
    pub stalled: String,
    pub question: String,
    pub looping: String,
}

/// Globs: on the tool name, on a shell command, on a file path (`~` is the home folder).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    pub tools: Vec<String>,
    pub bash: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Alert {
    /// The program and its arguments, run without a shell; `{agent}`, `{state}`, `{why}` are filled in.
    pub command: Vec<String>,
}

/// Rules for the sessions whose name or folder matches `match` (a glob). Later rules override earlier ones.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentRule {
    #[serde(rename = "match")]
    pub pattern: String,
    pub answer_questions: Option<bool>,
    pub paused: Option<bool>,
}

/// What applies to one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForAgent {
    pub answer_questions: bool,
    pub paused: bool,
}

/// The answer to "may this pending call be approved for the person?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    Deny(String),
    /// Not covered: the person decides.
    Pass,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            enabled: true,
            interval_secs: 30,
            scan_hours: 12,
            model: None,
            thresholds: Thresholds::default(),
            nudge: Nudge::default(),
            approve: Rules::default(),
            deny: Rules::default(),
            alert: Alert::default(),
            agents: Vec::new(),
        }
    }
}

impl Default for ModelPolicy {
    fn default() -> Self {
        ModelPolicy {
            provider: String::new(),
            name: String::new(),
            max_usd_per_hour: 0.20,
            max_calls_per_hour: 30,
            max_input_tokens: 2000,
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            idle_min: 5,
            stall_min: 30,
            tool_grace_min: 20,
            alert_after_min: 20,
            permission_secs: 20,
            answer_secs: 60,
            ask_patterns: [
                "should i",
                "shall i",
                "want me to",
                "do you want",
                "let me know",
                "¿sigo",
                "¿continúo",
                "¿lo hago",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

impl Default for Nudge {
    fn default() -> Self {
        Nudge {
            answer_questions: true,
            nudges_per_hour: 3,
            nudge_cooldown_min: 15,
            stalled: "You have made no progress for {idle}. Continue with the next step of your task; if you are \
                      blocked, say why in one line and move on to something else."
                .into(),
            question: "Yes, go ahead with what you propose. Decide the small things yourself.".into(),
            looping: "Errors are repeating: {why}. Stop, read the error and try a different approach.".into(),
        }
    }
}

/// The outcome of reading the files: the policy in force, and what went wrong with the files, for the panel.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    pub policy: Policy,
    pub errors: Vec<String>,
}
