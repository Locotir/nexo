//! The live state of workflow runs: what a view draws, and what stops a run from outside.
//!
//! The runs live in the sub-agent registry ([`Subs`]) and share what it has: its change channel (so one receiver
//! hears both), its clear and its generation counter (a run that winds down after `/clear` tells nobody), and the
//! rule that only the newest [`KEEP_RUNS`] ended runs are kept. A snapshot holds no report text: the reports
//! stay in the result the main agent gets.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::{now_ms, SubChange, Subs, KEEP_RUNS};

/// Characters of the reason a step ended as it did.
const WHY_CHARS: usize = 160;

/// Where a workflow run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowState {
    Running,
    /// Every step ended ok.
    Ok,
    /// It ended, and a step did not end ok (or a ceiling or a failed phase ended it early).
    Problems,
    /// The person stopped it, or the session ended.
    Stopped,
}

/// Where one step stands: the runner's own states, and the two before the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Waiting,
    Running,
    Ok,
    Partial,
    Failed,
    Skipped,
    NotRun,
}

impl StepState {
    pub fn ended(self) -> bool {
        !matches!(self, StepState::Waiting | StepState::Running)
    }
}

/// One step of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowStep {
    pub id: String,
    pub state: StepState,
    /// One line, for a step that did not end ok: why.
    pub why: Option<String>,
    /// The ids of the sub-agent runs it started (the ones [`Subs::list`] has), in the order they started.
    pub subs: Vec<String>,
    /// Input and output tokens its sub-agents spent so far.
    pub tokens: u64,
    /// Unix milliseconds.
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowPhase {
    pub title: String,
    pub steps: Vec<FlowStep>,
}

/// What a view draws of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowInfo {
    /// `wf#1`, counted per session.
    pub id: String,
    /// The workflow's name.
    pub name: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub state: FlowState,
    pub phases: Vec<FlowPhase>,
}

struct Data {
    info: FlowInfo,
    /// The person stopped the run.
    stop_run: bool,
    /// Steps the person stopped.
    stopped: HashSet<String>,
    /// The jobs of the sub-agents at work, by step.
    jobs: Vec<(String, u32)>,
}

pub(super) struct Entry {
    data: Mutex<Data>,
}

impl Subs {
    /// A workflow run begins; `phases` are the title and step ids of each phase, in order. Refused while another
    /// run of this session is still going: one run at a time.
    pub fn begin_flow(&self, name: &str, phases: &[(String, Vec<String>)]) -> Result<FlowRun, String> {
        let entry;
        let id;
        {
            let mut runs = self.0.flows.lock().unwrap();
            if let Some(busy) = runs.iter().find_map(|e| {
                let d = e.data.lock().unwrap();
                (d.info.state == FlowState::Running).then(|| d.info.id.clone())
            }) {
                return Err(format!("A workflow is already running ({busy}); wait for it to end or stop it first."));
            }
            id = format!("wf#{}", self.0.flow_seq.fetch_add(1, Ordering::SeqCst) + 1);
            let step = |s: &String| FlowStep {
                id: s.clone(),
                state: StepState::Waiting,
                why: None,
                subs: Vec::new(),
                tokens: 0,
                started_ms: None,
                ended_ms: None,
            };
            entry = Arc::new(Entry {
                data: Mutex::new(Data {
                    info: FlowInfo {
                        id: id.clone(),
                        name: name.to_string(),
                        started_ms: now_ms(),
                        ended_ms: None,
                        state: FlowState::Running,
                        phases: phases
                            .iter()
                            .map(|(title, steps)| FlowPhase {
                                title: title.clone(),
                                steps: steps.iter().map(step).collect(),
                            })
                            .collect(),
                    },
                    stop_run: false,
                    stopped: HashSet::new(),
                    jobs: Vec::new(),
                }),
            });
            // the ended runs beyond the limit go, the oldest first; the new one counts toward it
            let mut ended: Vec<(u64, String)> = runs
                .iter()
                .filter_map(|e| {
                    let d = e.data.lock().unwrap();
                    d.info.state.ended().then(|| (d.info.ended_ms.unwrap_or(0), d.info.id.clone()))
                })
                .collect();
            let over = (ended.len() + 1).saturating_sub(KEEP_RUNS);
            ended.sort_by_key(|(at, _)| *at);
            let gone: HashSet<String> = ended.into_iter().take(over).map(|(_, id)| id).collect();
            runs.retain(|e| !gone.contains(&e.data.lock().unwrap().info.id));
            runs.push(entry.clone());
        }
        let run = FlowRun { entry, subs: self.clone(), generation: self.0.generation.load(Ordering::SeqCst) };
        run.send(&run.entry.data.lock().unwrap());
        Ok(run)
    }

    /// The workflow runs of the session, oldest first.
    pub fn flows(&self) -> Vec<FlowInfo> {
        let runs = self.0.flows.lock().unwrap().clone();
        runs.iter().map(|e| e.data.lock().unwrap().info.clone()).collect()
    }

    /// The id of the run that is going now, if one is.
    pub fn flow_running(&self) -> Option<String> {
        let runs = self.0.flows.lock().unwrap().clone();
        runs.iter().find_map(|e| {
            let d = e.data.lock().unwrap();
            (d.info.state == FlowState::Running).then(|| d.info.id.clone())
        })
    }

    fn flow(&self, id: &str) -> Option<Arc<Entry>> {
        self.0.flows.lock().unwrap().iter().find(|e| e.data.lock().unwrap().info.id == id).cloned()
    }

    /// The person stops step `step` of run `run`: its sub-agents at work are to be cancelled (their job ids come
    /// back, and the caller cancels them the way it cancels any sub-agent), it starts no more, and the runner ends
    /// it as failed. Refused for a run or step that is unknown or has ended.
    pub fn stop_flow_step(&self, run: &str, step: &str) -> Result<Vec<u32>, String> {
        let e = self.flow(run).ok_or_else(|| format!("there is no workflow run {run}"))?;
        let mut d = e.data.lock().unwrap();
        if d.info.state != FlowState::Running {
            return Err(format!("workflow run {run} has already ended"));
        }
        let state = d.info.phases.iter().flat_map(|p| &p.steps).find(|s| s.id == step).map(|s| s.state);
        match state {
            None => return Err(format!("workflow run {run} has no step `{step}`")),
            Some(s) if s.ended() => return Err(format!("step `{step}` has already ended")),
            Some(_) => {}
        }
        d.stopped.insert(step.to_string());
        Ok(d.jobs.iter().filter(|(s, _)| s == step).map(|(_, j)| *j).collect())
    }

    /// The person stops run `run`: nothing new starts, and the job ids of every sub-agent at work come back to be
    /// cancelled.
    pub fn stop_flow(&self, run: &str) -> Result<Vec<u32>, String> {
        let e = self.flow(run).ok_or_else(|| format!("there is no workflow run {run}"))?;
        let mut d = e.data.lock().unwrap();
        if d.info.state != FlowState::Running {
            return Err(format!("workflow run {run} has already ended"));
        }
        d.stop_run = true;
        Ok(d.jobs.iter().map(|(_, j)| *j).collect())
    }
}

impl FlowState {
    fn ended(self) -> bool {
        self != FlowState::Running
    }
}

/// The side the runner writes to. Dropping it before `end` ends the run as stopped.
pub struct FlowRun {
    entry: Arc<Entry>,
    subs: Subs,
    /// The registry's count of clears when this run began.
    generation: u64,
}

impl FlowRun {
    pub fn id(&self) -> String {
        self.entry.data.lock().unwrap().info.id.clone()
    }

    /// The registry was not cleared since this run began.
    fn live(&self) -> bool {
        self.subs.0.generation.load(Ordering::SeqCst) == self.generation
    }

    fn send(&self, d: &Data) {
        if self.live() {
            let _ = self.subs.0.changes.send(SubChange::Flow(d.info.clone()));
        }
    }

    /// Nothing more may start: the person stopped the run, or the session it belongs to is gone.
    pub fn run_stopped(&self) -> bool {
        !self.live() || self.entry.data.lock().unwrap().stop_run
    }

    /// The person stopped step `step`.
    pub fn step_stopped(&self, step: &str) -> bool {
        self.entry.data.lock().unwrap().stopped.contains(step)
    }

    fn step_mut<'a>(d: &'a mut Data, step: &str) -> Option<&'a mut FlowStep> {
        d.info.phases.iter_mut().flat_map(|p| p.steps.iter_mut()).find(|s| s.id == step)
    }

    /// A sub-agent of `step` started, as run `sub` and job `job`: the step is running.
    pub fn agent_started(&self, step: &str, sub: &str, job: u32) {
        let mut d = self.entry.data.lock().unwrap();
        if let Some(s) = Self::step_mut(&mut d, step) {
            if s.state.ended() {
                return;
            }
            s.state = StepState::Running;
            s.started_ms.get_or_insert_with(now_ms);
            s.subs.push(sub.to_string());
        }
        d.jobs.push((step.to_string(), job));
        self.send(&d);
    }

    /// A sub-agent of `step` ended.
    pub fn agent_ended(&self, step: &str, job: u32) {
        let mut d = self.entry.data.lock().unwrap();
        d.jobs.retain(|(s, j)| !(s == step && *j == job));
        self.send(&d);
    }

    /// The tokens each step has spent so far; one event, and none when nothing changed.
    pub fn tokens(&self, spent: &[(&str, u64)]) {
        let mut d = self.entry.data.lock().unwrap();
        let mut changed = false;
        for (step, n) in spent {
            if let Some(s) = Self::step_mut(&mut d, step) {
                if !s.state.ended() && s.tokens != *n {
                    s.tokens = *n;
                    changed = true;
                }
            }
        }
        if changed {
            self.send(&d);
        }
    }

    /// Step `step` ended as `state`, having spent `tokens`. A step that ended already stays as it was.
    pub fn step_ended(&self, step: &str, state: StepState, why: Option<&str>, tokens: u64) {
        let mut d = self.entry.data.lock().unwrap();
        let Some(s) = Self::step_mut(&mut d, step) else { return };
        if s.state.ended() {
            return;
        }
        s.state = state;
        s.why = why.map(|w| w.lines().next().unwrap_or("").chars().take(WHY_CHARS).collect());
        s.tokens = tokens;
        s.ended_ms = Some(now_ms());
        d.jobs.retain(|(x, _)| x != step);
        self.send(&d);
    }

    /// The run ended as `state`. Ending twice changes nothing.
    pub fn end(&self, state: FlowState) {
        let mut d = self.entry.data.lock().unwrap();
        if d.info.state.ended() {
            return;
        }
        d.info.state = state;
        d.info.ended_ms = Some(now_ms());
        self.send(&d);
    }
}

impl Drop for FlowRun {
    /// A run that stops without saying how (a panic, a dropped task) ends as stopped.
    fn drop(&mut self) {
        self.end(FlowState::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::Receiver;

    fn phases() -> Vec<(String, Vec<String>)> {
        vec![("One".into(), vec!["a".into(), "b".into()]), ("Two".into(), vec!["c".into()])]
    }

    fn infos(rx: &mut Receiver<SubChange>) -> Vec<FlowInfo> {
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            if let SubChange::Flow(i) = c {
                out.push(i);
            }
        }
        out
    }

    fn step<'a>(i: &'a FlowInfo, id: &str) -> &'a FlowStep {
        i.phases.iter().flat_map(|p| &p.steps).find(|s| s.id == id).unwrap()
    }

    #[test]
    fn a_run_goes_waiting_running_ok_and_each_change_is_one_snapshot() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let run = subs.begin_flow("w", &phases()).unwrap();
        assert_eq!(run.id(), "wf#1");
        run.agent_started("a", "t#1", 1);
        run.agent_ended("a", 1);
        run.step_ended("a", StepState::Ok, None, 15);
        run.step_ended("a", StepState::Failed, Some("late"), 99);
        run.end(FlowState::Problems);
        run.end(FlowState::Ok);
        let seen = infos(&mut rx);
        let a: Vec<StepState> = seen.iter().map(|i| step(i, "a").state).collect();
        assert_eq!(a, [StepState::Waiting, StepState::Running, StepState::Running, StepState::Ok, StepState::Ok]);
        let last = seen.last().unwrap();
        assert_eq!((last.state, step(last, "a").tokens), (FlowState::Problems, 15), "an ended step stays as it was");
        assert_eq!(step(last, "a").subs, ["t#1"]);
        assert_eq!(step(last, "b").state, StepState::Waiting);
        assert!(last.ended_ms.is_some() && step(last, "a").started_ms.is_some());
        assert_eq!(subs.flows().len(), 1);
    }

    #[test]
    fn tokens_make_one_snapshot_and_none_when_nothing_changed() {
        let subs = Subs::new();
        let run = subs.begin_flow("w", &phases()).unwrap();
        run.agent_started("a", "t#1", 1);
        let mut rx = subs.changes();
        run.tokens(&[("a", 40), ("b", 0)]);
        run.tokens(&[("a", 40)]);
        let seen = infos(&mut rx);
        assert_eq!(seen.len(), 1);
        assert_eq!(step(&seen[0], "a").tokens, 40);
        run.step_ended("a", StepState::Ok, None, 50);
        run.tokens(&[("a", 60)]);
        assert_eq!(step(subs.flows().last().unwrap(), "a").tokens, 50, "a step that ended keeps its count");
    }

    #[test]
    fn only_one_run_goes_at_a_time_and_a_new_one_is_allowed_once_it_ends() {
        let subs = Subs::new();
        let first = subs.begin_flow("w", &phases()).unwrap();
        let why = subs.begin_flow("w", &phases()).err().unwrap();
        assert_eq!(why, "A workflow is already running (wf#1); wait for it to end or stop it first.");
        assert_eq!(subs.flow_running().as_deref(), Some("wf#1"));
        first.end(FlowState::Ok);
        assert_eq!(subs.flow_running(), None);
        assert_eq!(subs.begin_flow("w", &phases()).unwrap().id(), "wf#2");
    }

    #[test]
    fn stopping_a_step_names_its_jobs_and_a_stopped_run_names_all_of_them() {
        let subs = Subs::new();
        let run = subs.begin_flow("w", &phases()).unwrap();
        run.agent_started("a", "t#1", 1);
        run.agent_started("a", "t#2", 2);
        run.agent_started("b", "t#3", 3);
        assert_eq!(subs.stop_flow_step("wf#1", "a"), Ok(vec![1, 2]));
        assert!(run.step_stopped("a") && !run.step_stopped("b") && !run.run_stopped());
        run.agent_ended("a", 1);
        assert_eq!(subs.stop_flow("wf#1"), Ok(vec![2, 3]));
        assert!(run.run_stopped());
        assert!(subs.stop_flow_step("wf#1", "zzz").unwrap_err().contains("no step"));
        assert!(subs.stop_flow_step("wf#9", "a").unwrap_err().contains("no workflow run"));
        run.step_ended("c", StepState::NotRun, Some("x"), 0);
        assert!(subs.stop_flow_step("wf#1", "c").unwrap_err().contains("already ended"));
        run.end(FlowState::Stopped);
        assert!(subs.stop_flow("wf#1").unwrap_err().contains("already ended"));
    }

    #[test]
    fn a_run_dropped_without_an_end_is_stopped() {
        let subs = Subs::new();
        drop(subs.begin_flow("w", &phases()).unwrap());
        assert_eq!(subs.flows()[0].state, FlowState::Stopped);
    }

    #[test]
    fn after_a_clear_the_old_run_tells_nobody_and_counting_starts_again() {
        let subs = Subs::new();
        let old = subs.begin_flow("w", &phases()).unwrap();
        subs.clear();
        let mut rx = subs.changes();
        assert!(old.run_stopped(), "a run of a cleared session may start nothing");
        old.agent_started("a", "t#1", 1);
        old.end(FlowState::Ok);
        assert!(infos(&mut rx).is_empty());
        assert!(subs.flows().is_empty());
        assert_eq!(subs.begin_flow("w", &phases()).unwrap().id(), "wf#1");
    }

    #[test]
    fn only_the_newest_ended_runs_are_kept() {
        let subs = Subs::new();
        for _ in 0..(KEEP_RUNS + 5) {
            subs.begin_flow("w", &phases()).unwrap().end(FlowState::Ok);
        }
        let all = subs.flows();
        assert_eq!(all.len(), KEEP_RUNS);
        assert_eq!(all.last().unwrap().id, format!("wf#{}", KEEP_RUNS + 5));
    }
}
