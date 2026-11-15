//! The live adapter: every other nexo session that beats under the mail folder (a terminal, a team lane's
//! runner) becomes a row, keyed `nexo:<session id>`, with what its heartbeat says: busy or idle, the running
//! tool, model, mode, spend, context use, goal and the jobs it runs. It reads files only and never a message.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use nexo_core::jobs::JobKind;
use nexo_core::mailbox::{live_sessions, LiveSession};

use super::events::{Ev, EvKind};
use super::source::{one_line, Source, SourceKind};
use super::{ObsKind, Observation, Secs};

/// Characters of a job's title kept as its name.
const JOB_NAME: usize = 40;

/// What the source remembers of a session between two polls, so only a change says anything.
#[derive(Default)]
struct Seen {
    busy: bool,
    step: Option<String>,
    /// The name, mode, model and cost last told.
    meta: (Option<String>, Option<String>, Option<String>, Option<String>),
    cost: Option<f64>,
    ctx: Option<u8>,
    goal: Option<String>,
    /// The team lane it was, so that its row outlives its runner (the team source keeps it).
    lane: Option<String>,
    jobs: HashSet<u32>,
}

pub struct LiveSource {
    root: PathBuf,
    /// This process: its own session is the supervisor's own row, not a peer.
    own_pid: u32,
    seen: HashMap<String, Seen>,
}

impl LiveSource {
    pub fn new(root: impl Into<PathBuf>) -> LiveSource {
        LiveSource { root: root.into(), own_pid: std::process::id(), seen: HashMap::new() }
    }

    /// As [`LiveSource::new`] for a process other than this one (tests, where every heartbeat is this process's).
    pub fn with_own_pid(mut self, pid: u32) -> LiveSource {
        self.own_pid = pid;
        self
    }
}

fn tool_of(step: &str) -> String {
    step.split_whitespace().next().unwrap_or("tool").to_string()
}

impl Source for LiveSource {
    fn id(&self) -> &str {
        "live-sessions"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Nexo
    }

    fn poll(&mut self, now: Secs) -> Vec<Observation> {
        let mut out = Vec::new();
        let live: Vec<LiveSession> =
            live_sessions(&self.root, now * 1000).into_iter().filter(|s| s.pid != self.own_pid).collect();
        let obs = |key: &str, kind: ObsKind| Observation { agent_key: key.to_string(), at: now, kind };
        for s in &live {
            let key = format!("nexo:{}", s.id);
            let first = !self.seen.contains_key(&s.id);
            let seen = self.seen.entry(s.id.clone()).or_default();
            let f = &s.facts;
            seen.lane = f.lane.clone();

            let spend_moved = seen.cost != f.cost_usd || seen.meta.3 != f.model;
            // names and settings, once and when they change; a lane's name is the team source's
            let meta = (f.lane.is_none().then(|| s.name.clone()), Some(s.cwd.clone()), f.mode.clone(), f.model.clone());
            if first || seen.meta != meta {
                out.push(obs(
                    &key,
                    ObsKind::Meta { name: meta.0.clone(), cwd: meta.1.clone(), permission_mode: meta.2.clone() },
                ));
                seen.meta = meta;
            }
            if first || spend_moved {
                out.push(obs(&key, ObsKind::Spend { usd: f.cost_usd.unwrap_or(0.0), model: f.model.clone() }));
                seen.cost = f.cost_usd;
            }
            if let (Some(pct), true) = (f.ctx_pct, seen.ctx != f.ctx_pct) {
                out.push(obs(&key, ObsKind::Ctx { pct }));
            }
            seen.ctx = f.ctx_pct;
            if first && f.goal.is_some() || seen.goal != f.goal && !first {
                out.push(obs(&key, ObsKind::Goal { text: f.goal.clone() }));
            }
            seen.goal = f.goal.clone();

            // busy or idle: a turn that starts or ends is one event, work in between keeps the row alive
            let busy = s.busy == Some(true);
            match (first, seen.busy, busy) {
                (true, _, true) | (false, false, true) => out.push(obs(&key, ObsKind::TurnBegin)),
                (true, _, false) => {
                    out.push(obs(&key, ObsKind::Activity));
                    out.push(obs(&key, ObsKind::TurnEnd { last_text_tail: String::new() }));
                }
                (false, true, false) => {
                    if let Some(old) = &seen.step {
                        out.push(obs(&key, ObsKind::ToolResult { name: tool_of(old), input_hash: 0, is_error: false }));
                        seen.step = None;
                    }
                    out.push(obs(&key, ObsKind::TurnEnd { last_text_tail: String::new() }));
                }
                _ => {}
            }
            seen.busy = busy;
            if busy {
                out.push(obs(&key, ObsKind::Activity));
                let step = f.step.clone();
                if step != seen.step {
                    if let Some(old) = &seen.step {
                        out.push(obs(&key, ObsKind::ToolResult { name: tool_of(old), input_hash: 0, is_error: false }));
                    }
                    if let Some(new) = &step {
                        out.push(obs(&key, ObsKind::Step { text: new.clone() }));
                        out.push(obs(&key, ObsKind::ToolPending { name: tool_of(new) }));
                    }
                    seen.step = step;
                }
            }

            // its jobs are its sub-agents: one row each while it runs
            let mut now_jobs = HashSet::new();
            for j in &s.jobs {
                let jk = format!("{key}.{}", j.id);
                if j.state != "running" {
                    continue;
                }
                now_jobs.insert(j.id);
                if seen.jobs.insert(j.id) {
                    out.push(obs(
                        &jk,
                        ObsKind::Meta { name: Some(one_line(&j.title, JOB_NAME)), cwd: None, permission_mode: None },
                    ));
                    out.push(obs(&jk, ObsKind::Step { text: j.title.clone() }));
                    if j.kind == JobKind::Agent {
                        out.push(obs(&key, ObsKind::Ev(Ev::new(now, EvKind::SubSpawn))));
                    }
                }
                out.push(obs(&jk, ObsKind::Activity));
            }
            let ended: Vec<u32> = seen.jobs.iter().copied().filter(|id| !now_jobs.contains(id)).collect();
            for id in ended {
                seen.jobs.remove(&id);
                out.push(obs(&format!("{key}.{id}"), ObsKind::Done));
                out.push(obs(&key, ObsKind::Ev(Ev::new(now, EvKind::SubDone))));
            }
        }

        // a session that stopped beating has ended; a lane's row stays, the team source tells how it stands
        let here: HashSet<&str> = live.iter().map(|s| s.id.as_str()).collect();
        let gone: Vec<String> = self.seen.keys().filter(|id| !here.contains(id.as_str())).cloned().collect();
        for id in gone {
            let Some(seen) = self.seen.remove(&id) else { continue };
            let key = format!("nexo:{id}");
            for j in seen.jobs {
                out.push(obs(&format!("{key}.{j}"), ObsKind::Done));
            }
            out.push(obs(&key, ObsKind::TurnEnd { last_text_tail: String::new() }));
            if seen.lane.is_none() {
                out.push(obs(&key, ObsKind::Dead));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::jobs::{JobInfo, JobStatus};
    use nexo_core::mail::Inbound;
    use nexo_core::mailbox::{now_ms, Mailbox};

    fn open(root: &std::path::Path, id: &str, name: &str) -> std::sync::Arc<Mailbox> {
        Mailbox::open(root, id, name, "/work/x", false, Inbound::Auto).unwrap()
    }

    fn keys(o: &[Observation]) -> Vec<String> {
        let mut k: Vec<String> = o.iter().map(|o| o.agent_key.clone()).collect();
        k.sort();
        k.dedup();
        k
    }

    fn call(id: &str, name: &str, cmd: &str) -> nexo_core::agent::AgentEvent {
        nexo_core::agent::AgentEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            args: serde_json::json!({ "command": cmd }),
            summary: String::new(),
        }
    }

    #[test]
    fn two_heartbeats_become_two_rows_and_this_process_is_not_one() {
        let d = tempfile::tempdir().unwrap();
        let a = open(d.path(), "20261115-120000-aaaa", "api");
        let b = open(d.path(), "20261115-120001-bbbb", "web");
        a.set_idle(true);
        b.set_idle(false);
        let now = now_ms() / 1000;
        // every heartbeat here is this process's: told apart by pid, none is a peer
        assert!(LiveSource::new(d.path()).poll(now).is_empty());
        let mut src = LiveSource::new(d.path()).with_own_pid(0);
        let first = src.poll(now);
        assert_eq!(keys(&first), vec!["nexo:20261115-120000-aaaa", "nexo:20261115-120001-bbbb"]);
        assert!(first.iter().any(|o| o.agent_key.ends_with("bbbb") && o.kind == ObsKind::TurnBegin));
        assert!(
            first.iter().any(|o| o.agent_key.ends_with("aaaa") && matches!(o.kind, ObsKind::TurnEnd { .. })),
            "an idle session is idle, not stalled"
        );
        assert!(first.iter().any(
            |o| matches!(&o.kind, ObsKind::Meta { name: Some(n), cwd: Some(c), .. } if n == "api" && c == "/work/x")
        ));
    }

    #[test]
    fn a_change_of_step_is_one_tool_event_and_cost_model_ctx_and_goal_come_through() {
        let d = tempfile::tempdir().unwrap();
        let a = open(d.path(), "20261115-120000-aaaa", "api");
        a.set_idle(false);
        let mut src = LiveSource::new(d.path()).with_own_pid(0);
        let now = now_ms() / 1000;
        src.poll(now);
        a.observe(&call("1", "bash", "cargo test"));
        let o = src.poll(now + 1);
        let pending: Vec<&Observation> = o.iter().filter(|o| matches!(o.kind, ObsKind::ToolPending { .. })).collect();
        assert_eq!(pending.len(), 1, "{o:?}");
        assert!(o.iter().any(|o| o.kind == ObsKind::Step { text: "bash cargo test".into() }));
        // the same step again says nothing new
        let again = src.poll(now + 2);
        assert!(!again.iter().any(|o| matches!(o.kind, ObsKind::ToolPending { .. } | ObsKind::Step { .. })));
        a.observe(&call("2", "read", "x"));
        let next = src.poll(now + 3);
        assert_eq!(next.iter().filter(|o| matches!(o.kind, ObsKind::ToolPending { .. })).count(), 1);
        assert_eq!(next.iter().filter(|o| matches!(o.kind, ObsKind::ToolResult { .. })).count(), 1);

        use nexo_core::agent::AgentEvent as E;
        a.observe(&E::ModelLimits {
            provider: "xai".into(),
            model: "grok".into(),
            pricing: None,
            context_window: Some(1000),
            max_output: None,
            source: "t".into(),
        });
        a.observe(&E::Context {
            view_tokens: 340,
            history_tokens: 400,
            history_bytes: 0,
            view_bytes: 0,
            messages_in_view: 1,
            messages_total: 1,
            policy: "plain".into(),
            note: None,
            label: String::new(),
            detail: None,
        });
        a.observe(&E::Goal {
            text: Some("ship the panel".into()),
            round: 0,
            max: 3,
            state: nexo_core::agent::GoalState::Active,
        });
        a.observe(&E::ModelChanged { provider: "xai".into(), model: "grok".into() });
        a.beat();
        let o = src.poll(now + 4);
        assert!(o.iter().any(|o| o.kind == ObsKind::Ctx { pct: 34 }), "{o:?}");
        assert!(o.iter().any(|o| o.kind == ObsKind::Goal { text: Some("ship the panel".into()) }));
        assert!(o.iter().any(|o| matches!(&o.kind, ObsKind::Spend { model: Some(m), .. } if m == "xai/grok")));
    }

    #[test]
    fn running_jobs_are_sub_rows_and_end_when_they_leave_the_heartbeat() {
        let d = tempfile::tempdir().unwrap();
        let a = open(d.path(), "20261115-120000-aaaa", "api");
        let job = |status| JobInfo {
            id: 3,
            kind: JobKind::Agent,
            label: "audit grep caps".into(),
            status,
            elapsed_ms: 0,
            lines: 0,
        };
        a.set_jobs(&[job(JobStatus::Running)]);
        let mut src = LiveSource::new(d.path()).with_own_pid(0);
        let now = now_ms() / 1000;
        let o = src.poll(now);
        assert!(keys(&o).contains(&"nexo:20261115-120000-aaaa.3".to_string()), "{o:?}");
        assert!(o
            .iter()
            .any(|o| o.agent_key.ends_with(".3") && o.kind == ObsKind::Step { text: "audit grep caps".into() }));
        a.set_jobs(&[]);
        let o = src.poll(now + 1);
        assert!(o.iter().any(|o| o.agent_key.ends_with(".3") && o.kind == ObsKind::Done), "{o:?}");
    }

    #[test]
    fn a_session_that_stops_beating_ends() {
        let d = tempfile::tempdir().unwrap();
        let a = open(d.path(), "20261115-120000-aaaa", "api");
        let mut src = LiveSource::new(d.path()).with_own_pid(0);
        let now = now_ms() / 1000;
        assert!(!src.poll(now).is_empty());
        a.close();
        let o = src.poll(now + 1);
        assert!(o.iter().any(|o| o.agent_key == "nexo:20261115-120000-aaaa" && o.kind == ObsKind::Dead), "{o:?}");
        assert!(src.poll(now + 2).is_empty());
    }
}
