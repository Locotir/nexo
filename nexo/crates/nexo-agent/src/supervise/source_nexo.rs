//! The nexo events adapter: turns this program's own [`AgentEvent`]s and background jobs into observations.
//! The interface (or any host) feeds events in through a [`NexoSource`] handle; the supervisor drains the same
//! handle as a [`Source`]. A sub-agent is keyed `parent.child`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use nexo_core::agent::{AgentEvent, TurnEnd};
use nexo_core::jobs::{JobKind, JobStatus};
use nexo_core::mail::Stage;
use nexo_core::permission::PermissionMode;

use super::events::{after_command, Ev, EvDetail, EvKind};
use super::source::{one_line, tail, tool_name, tool_step, Source, SourceKind, STEP_CHARS};
use super::{input_hash, ObsKind, Observation, Secs};

const TAIL: usize = 300;
/// Observations kept while nobody drains them.
const QUEUE_MAX: usize = 4096;
const OUTBOX_MAX: usize = 8;
/// Characters of a mail's summary kept for the card.
const MAIL_SUMMARY: usize = 80;
/// Key of the session before it says its id.
pub const LOCAL_KEY: &str = "this-session";

#[derive(Default)]
struct Inner {
    key: Option<String>,
    queue: VecDeque<Observation>,
    /// Tool calls waiting for their result: call id to (name, input hash).
    calls: HashMap<String, (String, u64)>,
    text: String,
    cost: f64,
    model: Option<String>,
    /// Sub-agents still running, by key.
    running: Vec<String>,
    /// What the supervisor wants delivered to this session, oldest first.
    outbox: VecDeque<Nudge>,
    /// The next turn is the supervisor's resume after a check-in, not the person's.
    nudge_turn: bool,
    /// The next turn starts from mail of another session, not from the person.
    mail_turn: bool,
    /// Shell commands waiting for their result: call id to command.
    cmds: HashMap<String, String>,
}

impl Inner {
    fn key(&self) -> String {
        self.key.clone().unwrap_or_else(|| LOCAL_KEY.to_string())
    }
}

/// Something the supervisor wants said to a live session. A `manual` one is a message the person typed in the
/// panel: it goes in as their own message. Any other is evidence for the session's check-in, which asks its
/// own model whether the task is finished and shows nothing in the conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Nudge {
    pub key: String,
    pub text: String,
    pub manual: bool,
}

/// A handle shared by the host that feeds events and the supervisor that reads them.
#[derive(Clone, Default)]
pub struct NexoSource {
    inner: Arc<Mutex<Inner>>,
}

impl NexoSource {
    pub fn new() -> NexoSource {
        NexoSource::default()
    }

    /// Fold one event in.
    pub fn observe(&self, ev: &AgentEvent, now: Secs) {
        let Ok(mut g) = self.inner.lock() else { return };
        let obs = translate(&mut g, ev, now);
        for o in obs {
            // a stream of text deltas is one sign of life per second
            let same = o.kind == ObsKind::Activity
                && g.queue
                    .back()
                    .is_some_and(|b| b.kind == ObsKind::Activity && b.agent_key == o.agent_key && b.at == o.at);
            if same {
                continue;
            }
            if g.queue.len() >= QUEUE_MAX {
                g.queue.pop_front();
            }
            g.queue.push_back(o);
        }
    }

    /// The supervisor asks for a message to this session (the host delivers it).
    pub fn send_nudge(&self, key: &str, text: &str, manual: bool) {
        if let Ok(mut g) = self.inner.lock() {
            if g.outbox.len() >= OUTBOX_MAX {
                g.outbox.pop_front();
            }
            g.outbox.push_back(Nudge { key: key.to_string(), text: text.to_string(), manual });
        }
    }

    /// What the supervisor wants said to this session; the host delivers each.
    pub fn take_nudges(&self) -> Vec<Nudge> {
        let Ok(mut g) = self.inner.lock() else { return Vec::new() };
        g.outbox.drain(..).collect()
    }

    /// Whether a nudge waits for delivery.
    pub fn has_nudges(&self) -> bool {
        self.inner.lock().map(|g| !g.outbox.is_empty()).unwrap_or(false)
    }
}

fn translate(g: &mut Inner, ev: &AgentEvent, now: Secs) -> Vec<Observation> {
    let key = g.key();
    let mk = |kind: ObsKind| Observation { agent_key: key.clone(), at: now, kind };
    let mut out = Vec::new();
    // a sub-agent is alive while its parent works
    let busy = |g: &Inner, out: &mut Vec<Observation>| {
        for k in &g.running {
            out.push(Observation { agent_key: k.clone(), at: now, kind: ObsKind::Activity });
        }
    };
    match ev {
        AgentEvent::Ready { session_id, model, cwd, permission_mode, .. } => {
            g.key = Some(session_id.clone());
            g.model = Some(model.clone());
            let k = session_id.clone();
            let mk2 = |kind: ObsKind| Observation { agent_key: k.clone(), at: now, kind };
            let mode = if *permission_mode == PermissionMode::Auto { "bypassPermissions" } else { "ask" };
            out.push(mk2(ObsKind::Meta {
                name: None,
                cwd: Some(cwd.display().to_string()),
                permission_mode: Some(mode.to_string()),
            }));
            out.push(mk2(ObsKind::Spend { usd: g.cost, model: g.model.clone() }));
            out.push(mk2(ObsKind::Activity));
        }
        AgentEvent::SessionChanged { session_id, title } => {
            g.key = Some(session_id.clone());
            let name = (!title.is_empty()).then(|| title.clone());
            out.push(Observation {
                agent_key: session_id.clone(),
                at: now,
                kind: ObsKind::Meta { name, cwd: None, permission_mode: None },
            });
        }
        AgentEvent::PermissionModeChanged { mode } => {
            let m = if *mode == PermissionMode::Auto { "bypassPermissions" } else { "ask" };
            out.push(mk(ObsKind::Meta { name: None, cwd: None, permission_mode: Some(m.to_string()) }));
        }
        AgentEvent::ModelChanged { model, .. } => {
            g.model = Some(model.clone());
            out.push(mk(ObsKind::Spend { usd: g.cost, model: g.model.clone() }));
        }
        AgentEvent::History { message, .. }
            if message.role == nexo_core::message::Role::User
                && message.text().trim() == nexo_core::agent::SUPERVISOR_RESUME_MARK =>
        {
            g.nudge_turn = true;
        }
        AgentEvent::TurnStart { .. } => {
            g.text.clear();
            let by_mail = std::mem::take(&mut g.mail_turn);
            let by_supervisor = std::mem::take(&mut g.nudge_turn) || by_mail;
            out.push(mk(if by_supervisor { ObsKind::Activity } else { ObsKind::UserInput }));
        }
        AgentEvent::TextDelta { text } => {
            g.text.push_str(text);
            if g.text.chars().count() > TAIL * 2 {
                g.text = tail(&nexo_core::redact::redact(&g.text), TAIL);
            }
            out.push(mk(ObsKind::Activity));
        }
        AgentEvent::ReasoningDelta { .. } => {
            out.push(mk(ObsKind::Activity));
            out.push(mk(ObsKind::Ev(Ev::new(now, EvKind::Thinking))));
        }
        AgentEvent::AssistantStart | AgentEvent::ToolProgress { .. } => {
            out.push(mk(ObsKind::Activity));
        }
        AgentEvent::ToolCall { id, name, args, summary } => {
            g.calls.insert(id.clone(), (name.clone(), input_hash(&args.to_string())));
            if g.calls.len() > 512 {
                g.calls.clear();
            }
            if let Some(c) = args.get("command").and_then(|c| c.as_str()) {
                if g.cmds.len() > 64 {
                    g.cmds.clear();
                }
                g.cmds.insert(id.clone(), c.to_string());
            }
            let text = if summary.is_empty() {
                tool_step(name, args)
            } else {
                format!("{}  {}", tool_name(name), one_line(summary, STEP_CHARS))
            };
            out.push(mk(ObsKind::Step { text }));
            out.push(mk(ObsKind::ToolPending { name: name.clone() }));
            busy(g, &mut out);
        }
        AgentEvent::ToolDone { id, output, .. } => {
            let (name, hash) = g.calls.remove(id).unwrap_or_else(|| ("tool".to_string(), 0));
            out.push(mk(ObsKind::ToolResult { name, input_hash: hash, is_error: output.is_error }));
            if let Some(cmd) = g.cmds.remove(id) {
                if let Some(ev) = after_command(&cmd, &output.content, output.is_error, now) {
                    out.push(mk(ObsKind::Ev(ev)));
                }
            }
            busy(g, &mut out);
        }
        AgentEvent::PermissionAsk { request } => {
            out.push(mk(ObsKind::PermissionPending { tool: request.tool.clone() }))
        }
        AgentEvent::PermissionResolved { .. } => out.push(mk(ObsKind::PermissionResolved)),
        AgentEvent::Question { .. } => out.push(mk(ObsKind::AskPending)),
        AgentEvent::QuestionResolved { .. } => out.push(mk(ObsKind::AskResolved)),
        AgentEvent::Usage { cost_usd, call, .. } => {
            g.cost = *cost_usd;
            if call.output > 0 {
                let n = call.output.min(u32::MAX as u64) as u32;
                out.push(mk(ObsKind::Ev(Ev::with(now, EvKind::Cost, EvDetail::Tokens(n)))));
            }
            out.push(mk(ObsKind::Spend { usd: g.cost, model: g.model.clone() }));
            out.push(mk(ObsKind::Activity));
        }
        AgentEvent::TurnEnd { reason } => {
            let t = tail(&nexo_core::redact::redact(&g.text), TAIL);
            if *reason == TurnEnd::Error {
                out.push(mk(ObsKind::SystemError));
            }
            out.push(mk(ObsKind::TurnEnd { last_text_tail: t }));
        }
        AgentEvent::Jobs { jobs } => {
            let parent = g.key();
            for j in jobs.iter().filter(|j| j.kind == JobKind::Agent) {
                let k = format!("{parent}.{}", j.id);
                let mk3 = |kind: ObsKind| Observation { agent_key: k.clone(), at: now, kind };
                out.push(mk3(ObsKind::Meta { name: Some(one_line(&j.label, 40)), cwd: None, permission_mode: None }));
                if j.status == JobStatus::Running {
                    out.push(mk3(ObsKind::Activity));
                    if !g.running.contains(&k) {
                        out.push(Observation {
                            agent_key: parent.clone(),
                            at: now,
                            kind: ObsKind::Ev(Ev::new(now, EvKind::SubSpawn)),
                        });
                        g.running.push(k.clone());
                    }
                } else {
                    out.push(mk3(ObsKind::Done));
                    if g.running.contains(&k) {
                        out.push(Observation {
                            agent_key: parent.clone(),
                            at: now,
                            kind: ObsKind::Ev(Ev::new(now, EvKind::SubDone)),
                        });
                    }
                    g.running.retain(|r| r != &k);
                }
            }
        }
        AgentEvent::MailSent { id, to, summary, bytes, .. } => {
            out.push(mk(ObsKind::MsgOut {
                to: to.clone(),
                mail: Some(id.clone()),
                summary: one_line(summary, MAIL_SUMMARY),
                bytes: *bytes,
            }));
        }
        AgentEvent::MailArrived { id, from, summary, held, .. } => {
            out.push(mk(ObsKind::MsgIn {
                from: from.clone(),
                mail: Some(id.clone()),
                summary: one_line(summary, MAIL_SUMMARY),
                held: *held,
            }));
        }
        AgentEvent::MailDelivered { ids } => {
            g.mail_turn = true;
            for id in ids {
                out.push(mk(ObsKind::MsgStage { mail: id.clone(), stage: Stage::Delivered }));
            }
        }
        AgentEvent::MailRead { ids } => {
            for id in ids {
                out.push(mk(ObsKind::MsgStage { mail: id.clone(), stage: Stage::Read }));
            }
        }
        AgentEvent::MailStage { id, stage } => {
            out.push(mk(ObsKind::MsgStage { mail: id.clone(), stage: *stage }));
        }
        AgentEvent::Compacted { .. } => out.push(mk(ObsKind::Ev(Ev::new(now, EvKind::Compact)))),
        AgentEvent::Retrying { wait_secs, rate_limit, .. } => {
            let kind = if *rate_limit { EvKind::RateLimit } else { EvKind::Retry };
            out.push(mk(ObsKind::Ev(Ev::with(now, kind, EvDetail::Wait { secs: *wait_secs }))));
        }
        AgentEvent::TodoChanged { done, total } => {
            out.push(mk(ObsKind::Ev(Ev::with(now, EvKind::Todo, EvDetail::Progress { done: *done, total: *total }))));
        }
        _ => {}
    }
    out
}

impl Source for NexoSource {
    fn id(&self) -> &str {
        "nexo-events"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Nexo
    }

    fn poll(&mut self, _now: Secs) -> Vec<Observation> {
        self.inner.lock().map(|mut g| std::mem::take(&mut g.queue).into_iter().collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod nudge_tests {
    use super::*;

    #[test]
    fn a_nudge_is_taken_once_and_a_resume_turn_is_not_the_persons() {
        let s = NexoSource::new();
        assert!(!s.has_nudges());
        s.send_nudge("s1", "go on", false);
        assert!(s.has_nudges());
        assert_eq!(s.take_nudges(), vec![Nudge { key: "s1".into(), text: "go on".into(), manual: false }]);
        assert!(s.take_nudges().is_empty());
        let mut me = s.clone();
        let resume = nexo_core::message::Message::user(nexo_core::agent::SUPERVISOR_RESUME_MARK);
        s.observe(&AgentEvent::History { index: 4, message: resume }, 9);
        s.observe(&AgentEvent::TurnStart { turn: 1 }, 10);
        let obs = me.poll(11);
        assert!(obs.iter().all(|o| o.kind != ObsKind::UserInput), "{obs:?}");
        s.observe(&AgentEvent::TurnStart { turn: 2 }, 20);
        assert!(me.poll(21).iter().any(|o| o.kind == ObsKind::UserInput), "the person's turn counts again");
    }
}
