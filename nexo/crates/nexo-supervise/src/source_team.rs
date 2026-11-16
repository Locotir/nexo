//! The team adapter: every lane of `team.toml` is a row with its clock, spend and last round, whether or not
//! its runner lives. A lane that has a session id is keyed `nexo:<id>`, so it is one row with what the live
//! source reads from the session's heartbeat; a lane without one is `lane:<NAME>`. It reads files only.

use std::collections::HashMap;

use nexo_core::config::{parse_every, Lane};
use nexo_core::team::state::{self, LaneState};

use super::run::LaneInfo;
use super::source::{Source, SourceKind};
use super::{ObsKind, Observation, Secs};

/// Seconds between two reads of the lane files.
const EVERY: Secs = 2;

/// A lane as the disk says it stands.
pub struct Input {
    pub lane: Lane,
    pub st: LaneState,
    /// The recorded pid is still this lane's runner.
    pub alive: bool,
    pub held: bool,
}

/// What the loader reads: the lanes, and whether every lane is paused.
pub type Loaded = (Vec<Input>, bool);

/// The lanes of `team.toml` and their state files (nothing when there is no team file).
pub fn from_disk() -> Loaded {
    let lanes = nexo_core::team::read_lanes().map(|f| f.team.lane).unwrap_or_default();
    let inputs = lanes
        .into_iter()
        .map(|lane| {
            let st = LaneState::load(&lane.name);
            let alive = st.live_pid(&lane.name).is_some();
            let held = state::held(&lane.name);
            Input { lane, st, alive, held }
        })
        .collect();
    (inputs, state::paused())
}

/// The word `nexo team status` leads with.
fn word(i: &Input, paused: bool) -> &'static str {
    if !i.alive {
        return match i.st.state.as_str() {
            "failed" => "failed",
            _ if i.held => "held",
            _ => "stopped",
        };
    }
    match i.st.state.as_str() {
        "running" => "running",
        _ if paused => "paused",
        _ => "idle",
    }
}

/// The lane's facts for the panel at `today`.
pub fn info(i: &Input, paused: bool, today: &str) -> LaneInfo {
    let secs = |ms: i64| (ms / 1000).max(0) as Secs;
    LaneInfo {
        name: i.lane.name.clone(),
        state: word(i, paused).to_string(),
        runner: i.alive,
        next_round: i.alive.then_some(i.st.next_round).flatten().map(secs),
        every_secs: i.lane.every.as_deref().and_then(|e| parse_every(e).ok()).map(|d| d.as_secs()),
        last_round: i.st.last_round.as_ref().map(|r| (secs(r.at), r.result.clone(), r.spend)),
        rounds: i.st.rounds,
        failures: i.st.failures,
        spend_today: i.st.spent_on(today),
        cap_day: i.lane.max_spend_day,
        why: i.st.why.clone(),
        dir: i.lane.dir.clone(),
        model: i.lane.model.clone(),
        mode: i.lane.mode.clone(),
    }
}

pub struct TeamSource {
    load: Box<dyn FnMut() -> Loaded + Send>,
    today: Box<dyn Fn() -> String + Send>,
    last_read: Option<Secs>,
    /// What each lane's row was last told: its key and facts.
    told: HashMap<String, (String, LaneInfo)>,
}

impl Default for TeamSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TeamSource {
    pub fn new() -> TeamSource {
        TeamSource::with(from_disk, nexo_agent_today)
    }

    /// A source over another loader and day (tests).
    pub fn with(
        load: impl FnMut() -> Loaded + Send + 'static,
        today: impl Fn() -> String + Send + 'static,
    ) -> TeamSource {
        TeamSource { load: Box::new(load), today: Box::new(today), last_read: None, told: HashMap::new() }
    }
}

fn nexo_agent_today() -> String {
    nexo_core::clock::today()
}

impl Source for TeamSource {
    fn id(&self) -> &str {
        "team-lanes"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Nexo
    }

    fn poll(&mut self, now: Secs) -> Vec<Observation> {
        if self.last_read.is_some_and(|t| now < t + EVERY) {
            return Vec::new();
        }
        self.last_read = Some(now);
        let (inputs, paused) = (self.load)();
        let today = (self.today)();
        let mut out = Vec::new();
        let obs = |key: &str, kind: ObsKind| Observation { agent_key: key.to_string(), at: now, kind };
        for i in &inputs {
            let key = match &i.st.session_id {
                Some(id) => format!("nexo:{id}"),
                None => format!("lane:{}", i.lane.name),
            };
            let facts = info(i, paused, &today);
            let prev = self.told.get(&i.lane.name).cloned();
            if prev.as_ref().map(|p| &p.0) != Some(&key) {
                out.push(obs(
                    &key,
                    ObsKind::Meta {
                        name: Some(i.lane.name.clone()),
                        cwd: Some(i.lane.dir.clone()),
                        permission_mode: i.lane.mode.clone(),
                    },
                ));
                if let Some(m) = &i.lane.model {
                    out.push(obs(&key, ObsKind::Spend { usd: facts.spend_today, model: Some(m.clone()) }));
                }
            }
            let before = prev.as_ref().filter(|p| p.0 == key).map(|p| p.1.state.clone());
            if before.as_deref() != Some(facts.state.as_str()) {
                // a lane with no session of its own to beat tells its turns here
                if i.st.session_id.is_none() {
                    match (facts.state.as_str(), before.as_deref()) {
                        ("running", _) => out.push(obs(&key, ObsKind::TurnBegin)),
                        // a lane that failed has no runner and its round did not end: it reads as stalled, not as
                        // the work its spend note and error would otherwise look like
                        ("failed", _) => {
                            out.push(obs(&key, ObsKind::TurnBegin));
                            out.push(obs(&key, ObsKind::SystemError));
                            out.push(obs(&key, ObsKind::Dead));
                        }
                        (_, Some("running")) | (_, None) => {
                            out.push(obs(&key, ObsKind::Activity));
                            out.push(obs(&key, ObsKind::TurnEnd { last_text_tail: String::new() }));
                        }
                        _ => {}
                    }
                }
            }
            if prev.as_ref().map(|p| &p.1) != Some(&facts) || prev.as_ref().map(|p| &p.0) != Some(&key) {
                out.push(obs(&key, ObsKind::Lane(Box::new(facts.clone()))));
            }
            self.told.insert(i.lane.name.clone(), (key, facts));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NexoSource, Policy, Setup};
    use super::*;
    use nexo_core::team::state::Round;

    const NOW_MS: i64 = 1_800_000_000_000;
    const NOW: Secs = (NOW_MS / 1000) as Secs;

    fn lane(name: &str) -> Lane {
        Lane {
            name: name.into(),
            dir: "/w".into(),
            every: Some("2h".into()),
            max_spend_day: Some(3.0),
            ..Lane::default()
        }
    }

    fn input(name: &str, st: LaneState, alive: bool) -> Input {
        Input { lane: lane(name), st, alive, held: false }
    }

    fn idle_state() -> LaneState {
        LaneState {
            started: Some(NOW_MS - 60_000),
            state: "idle".into(),
            next_round: Some(NOW_MS + 600_000),
            rounds: 4,
            spend_day: "2026-11-15".into(),
            spend_today: 2.5,
            last_round: Some(Round { at: NOW_MS - 3_000_000, result: "ok".into(), spend: 0.6 }),
            ..LaneState::default()
        }
    }

    fn src(inputs: Vec<Input>, paused: bool) -> TeamSource {
        let mut inputs = Some(inputs);
        TeamSource::with(move || (inputs.take().unwrap_or_default(), paused), || "2026-11-15".to_string())
    }

    fn lane_of(o: &[Observation]) -> LaneInfo {
        o.iter()
            .find_map(|o| match &o.kind {
                ObsKind::Lane(l) => Some((**l).clone()),
                _ => None,
            })
            .expect("a lane observation")
    }

    #[test]
    fn a_stopped_lane_still_shows() {
        let st = LaneState { state: "idle".into(), ..LaneState::default() };
        let o = src(vec![input("WINDOWS", st, false)], false).poll(NOW);
        let l = lane_of(&o);
        assert_eq!((l.state.as_str(), l.runner, l.next_round), ("stopped", false, None));
        assert!(o.iter().all(|o| o.agent_key == "lane:WINDOWS"));
        assert_eq!(l.every_secs, Some(7200));
        assert_eq!(l.cap_day, Some(3.0));
    }

    #[test]
    fn a_held_or_failed_lane_says_so() {
        let mut held = input("A", LaneState::default(), false);
        held.held = true;
        assert_eq!(info(&held, false, "2026-11-15").state, "held");
        let st = LaneState { state: "failed".into(), why: Some("three rounds failed".into()), ..LaneState::default() };
        let l = info(&input("B", st, false), false, "2026-11-15");
        assert_eq!((l.state.as_str(), l.why.as_deref()), ("failed", Some("three rounds failed")));
        let o =
            src(vec![input("B", LaneState { state: "failed".into(), ..LaneState::default() }, false)], false).poll(NOW);
        assert!(o.iter().any(|o| o.kind == ObsKind::SystemError));
    }

    #[test]
    fn an_idle_lane_has_its_next_round_spend_and_last_round() {
        let o = src(vec![input("IOS", idle_state(), true)], false).poll(NOW);
        let l = lane_of(&o);
        assert_eq!(l.state, "idle");
        assert!(l.runner);
        assert_eq!(l.next_round, Some(NOW + 600));
        assert_eq!(l.spend_today, 2.5);
        assert_eq!(l.rounds, 4);
        assert_eq!(l.last_round, Some((NOW - 3000, "ok".to_string(), 0.6)));
        // yesterday's spend does not count today
        let mut old = idle_state();
        old.spend_day = "2026-11-14".into();
        assert_eq!(info(&input("IOS", old, true), false, "2026-11-15").spend_today, 0.0);
    }

    #[test]
    fn a_paused_lane_is_paused_and_a_running_one_running() {
        assert_eq!(info(&input("A", idle_state(), true), true, "2026-11-15").state, "paused");
        let running = LaneState { state: "running".into(), ..idle_state() };
        assert_eq!(info(&input("A", running, true), true, "2026-11-15").state, "running");
    }

    #[test]
    fn a_lane_with_a_session_shares_its_row_with_the_live_source() {
        let st = LaneState { session_id: Some("20261115-120000-aaaa".into()), ..idle_state() };
        let o = src(vec![input("IOS", st, true)], false).poll(NOW);
        assert!(o.iter().all(|o| o.agent_key == "nexo:20261115-120000-aaaa"), "{o:?}");
        assert_eq!(lane_of(&o).name, "IOS");
        // the heartbeat tells busy or idle: this source adds no turn event for it
        assert!(!o.iter().any(|o| matches!(o.kind, ObsKind::TurnBegin | ObsKind::TurnEnd { .. })));
    }

    #[test]
    fn only_a_change_is_told_again_and_reads_are_two_seconds_apart() {
        let mut calls = 0;
        let mut s = TeamSource::with(
            move || {
                calls += 1;
                let st = if calls < 3 { idle_state() } else { LaneState { state: "running".into(), ..idle_state() } };
                (vec![input("IOS", st, true)], false)
            },
            || "2026-11-15".to_string(),
        );
        assert!(!s.poll(NOW).is_empty());
        assert!(s.poll(NOW + 1).is_empty(), "too soon");
        assert!(s.poll(NOW + 2).is_empty(), "nothing changed");
        let o = s.poll(NOW + 4);
        assert_eq!(lane_of(&o).state, "running");
        assert!(o.iter().any(|o| o.kind == ObsKind::TurnBegin));
    }

    #[test]
    fn a_lane_and_its_heartbeat_are_one_row_with_the_lane_ctx_and_goal_filled() {
        use nexo_core::agent::AgentEvent as E;
        use nexo_core::mail::Inbound;
        use nexo_core::mailbox::Mailbox;
        let d = tempfile::tempdir().unwrap();
        let id = "20261115-120000-aaaa";
        let m = Mailbox::open(d.path(), id, "IOS", "/w", false, Inbound::Auto).unwrap();
        m.set_idle(false);
        m.set_lane("IOS");
        m.observe(&E::Goal {
            text: Some("ship 1.3.1".into()),
            round: 0,
            max: 3,
            state: nexo_core::agent::GoalState::Active,
        });
        let st = LaneState { session_id: Some(id.into()), ..idle_state() };
        let now = nexo_core::mailbox::now_ms() / 1000;
        let mut s = Setup::none().supervisor(Policy::default(), NexoSource::default());
        s.add(Box::new(super::super::source_live::LiveSource::new(d.path()).with_own_pid(0)));
        s.add(Box::new(src(vec![input("IOS", st, true)], false)));
        s.tick(now);
        let rows: Vec<_> = s.rows(now).into_iter().filter(|r| r.key == format!("nexo:{id}")).collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let r = &rows[0];
        assert_eq!(r.lane.as_ref().map(|l| l.name.as_str()), Some("IOS"));
        assert_eq!(r.goal.as_deref(), Some("ship 1.3.1"));
        assert_eq!(r.state, crate::State::Working);
        assert_eq!(r.program, "nexo");
    }

    #[test]
    fn a_dead_pid_is_not_a_runner() {
        let d = tempfile::tempdir().unwrap();
        // a process id nobody has, and a command line that is not `nexo team run`
        let st = LaneState { pid: Some(std::process::id()), ..LaneState::default() };
        assert!(st.live_pid("X").is_none(), "this test process is not a lane runner");
        let _ = d;
        assert!(LaneState { pid: Some(u32::MAX - 1), ..LaneState::default() }.live_pid("X").is_none());
    }

    #[test]
    fn a_failed_lane_row_reads_as_stalled_on_its_own() {
        let now = NOW;
        let st = LaneState { state: "failed".into(), why: Some("three rounds failed".into()), ..LaneState::default() };
        let mut i = input("B", st, false);
        i.lane.model = Some("xai/grok".into());
        let mut s = Setup::none().supervisor(Policy::default(), NexoSource::default());
        s.add(Box::new(src(vec![i], false)));
        s.tick(now);
        let rows = s.rows(now);
        let r = rows.iter().find(|r| r.key == "lane:B").expect("the lane row");
        assert_eq!(r.state, crate::State::Stalled, "{r:?}");
        assert_eq!(r.lane.as_ref().map(|l| l.state.as_str()), Some("failed"));
    }
}
