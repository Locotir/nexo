//! What the heartbeat says of a session beyond who it is: its model and mode, the tool it runs, since when the
//! turn runs, what it has spent, how full its context is, its goal and the team lane it is. A host feeds its agent
//! events to [`Mailbox::observe`] and the facts follow. Only metadata: a tool's name and target, never a message.

use super::peers::{clean, clean_name};
use super::*;

/// Characters of a step or a goal kept in the heartbeat.
const FACT_MAX: usize = 80;
/// Tool calls followed at once for the step line.
const CALLS_MAX: usize = 16;

/// The optional fields of a heartbeat. A file of an older nexo has none of them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveFacts {
    /// `provider/model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The permission mode (`ask`, `yolo`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The tool that runs now as `name target`; none between calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// When the running turn started (ms since the epoch); none between turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_ms: Option<i64>,
    /// What the session has spent so far, in dollars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// How full the context window is, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_pct: Option<u8>,
    /// The goal's text, short.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// The team lane's name, when the process is `nexo team run`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
}

/// The facts and what is needed to keep them up to date.
#[derive(Default)]
pub(super) struct Folded {
    pub facts: LiveFacts,
    /// Tool calls that have not ended: call id and its step line, oldest first.
    calls: Vec<(String, String)>,
    window: Option<u64>,
    view_tokens: Option<u64>,
}

/// `name target` for a tool call: the summary the tool gave, or else the first argument that says what it works on.
pub(super) fn step_of(name: &str, args: &serde_json::Value, summary: &str) -> String {
    const KEYS: [&str; 7] = ["command", "file_path", "path", "pattern", "query", "url", "description"];
    let first = |t: &str| t.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    let target = match first(summary) {
        s if !s.is_empty() => s,
        _ => KEYS.iter().find_map(|k| args.get(*k).and_then(|v| v.as_str()).map(first)).unwrap_or_default(),
    };
    let name = clean(name, 40);
    clean(&if target.is_empty() { name } else { format!("{name} {target}") }, FACT_MAX)
}

impl Folded {
    fn ctx(&mut self) {
        self.facts.ctx_pct = match (self.view_tokens, self.window) {
            (Some(v), Some(w)) if w > 0 => Some((v.saturating_mul(100) / w).min(100) as u8),
            _ => None,
        };
    }

    fn step(&mut self) {
        self.facts.step = self.calls.last().map(|c| c.1.clone());
    }

    /// Folds one agent event in.
    pub fn fold(&mut self, ev: &AgentEvent, now: i64) {
        match ev {
            AgentEvent::Ready { provider, model, permission_mode, .. } => {
                self.facts.model = Some(clean(&format!("{provider}/{model}"), FACT_MAX));
                self.facts.mode = Some(permission_mode.as_str().to_string());
            }
            AgentEvent::ModelChanged { provider, model } => {
                self.facts.model = Some(clean(&format!("{provider}/{model}"), FACT_MAX));
            }
            AgentEvent::PermissionModeChanged { mode } => self.facts.mode = Some(mode.as_str().to_string()),
            AgentEvent::ModelLimits { context_window, .. } => {
                self.window = context_window.map(u64::from);
                self.ctx();
            }
            AgentEvent::Context { view_tokens, .. } => {
                self.view_tokens = Some(*view_tokens);
                self.ctx();
            }
            AgentEvent::TurnStart { .. } => self.facts.turn_ms = Some(now),
            AgentEvent::ToolCall { id, name, args, summary } => {
                self.calls.retain(|c| c.0 != *id);
                if self.calls.len() >= CALLS_MAX {
                    self.calls.remove(0);
                }
                self.calls.push((id.clone(), step_of(name, args, summary)));
                self.step();
            }
            AgentEvent::ToolDone { id, .. } => {
                self.calls.retain(|c| c.0 != *id);
                self.step();
            }
            AgentEvent::TurnEnd { .. } | AgentEvent::Idle { .. } | AgentEvent::HistoryCleared => {
                self.calls.clear();
                self.facts.step = None;
                self.facts.turn_ms = None;
            }
            AgentEvent::Usage { cost_usd, .. } => self.facts.cost_usd = Some(*cost_usd),
            AgentEvent::Budget { session_cost, .. } => self.facts.cost_usd = Some(*session_cost),
            AgentEvent::Goal { text, .. } => {
                self.facts.goal = text.as_deref().map(|t| clean(t, FACT_MAX)).filter(|t| !t.is_empty());
            }
            _ => {}
        }
    }

    /// What a heartbeat must show at once when it changes (cost and context ride the next beat).
    fn urgent(&self) -> LiveFacts {
        LiveFacts { cost_usd: None, ctx_pct: None, ..self.facts.clone() }
    }
}

impl Mailbox {
    /// The host's agent said `ev`: the facts of the heartbeat follow. A change of step, turn, model, mode, goal or
    /// lane shows in the session file now; cost and context use come with the next write.
    pub fn observe(&self, ev: &AgentEvent) {
        let now = self.now();
        let moved = {
            let mut s = self.lock();
            let before = s.live.urgent();
            s.live.fold(ev, now);
            s.live.urgent() != before
        };
        if moved {
            self.write_session(now);
        }
    }

    /// This process is the runner of team lane `name`.
    pub fn set_lane(&self, name: &str) {
        self.lock().live.facts.lane = Some(clean_name(name));
        self.write_session(self.now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_old_heartbeat_without_the_new_fields_still_reads() {
        let old = r#"{"id":"a","name":"n","cwd":"/w","pid":1,"started_ms":1,"beat_ms":2}"#;
        let f: LiveFacts = serde_json::from_str(old).unwrap();
        assert_eq!(f, LiveFacts::default());
        let json = serde_json::to_string(&LiveFacts { lane: Some("IOS".into()), ..LiveFacts::default() }).unwrap();
        assert_eq!(json, r#"{"lane":"IOS"}"#);
    }

    #[test]
    fn a_step_names_the_tool_and_its_first_line_of_target() {
        assert_eq!(step_of("bash", &json!({"command": "cargo test\n--all"}), ""), "bash cargo test");
        assert_eq!(step_of("read", &json!({"file_path": "/a/b.rs"}), ""), "read /a/b.rs");
        assert_eq!(step_of("edit", &json!({"file_path": "/x"}), "src/lib.rs"), "edit src/lib.rs");
        assert_eq!(step_of("idle", &json!({}), ""), "idle");
        assert_eq!(step_of("bash", &json!({"command": "x".repeat(300)}), "").chars().count(), FACT_MAX);
    }
}
