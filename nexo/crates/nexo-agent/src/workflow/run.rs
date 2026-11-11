//! The runner: phases in order, the steps of a phase together, every sub-agent started through the code the
//! `task` tool starts its own with ([`SubAgent::launch`]), within the ceilings of the workflow.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures::stream::FuturesUnordered;
use futures::StreamExt;
use nexo_core::subs::{FlowRun, FlowState, StepState, SubMeter};
use nexo_core::tool::{ToolContext, ToolKind};
use serde_json::Value;

use super::file::{answer_key, expand, pieces, Piece, Step, Workflow, MAX_ITEMS};
use crate::subagent::{Ended, Launch, SubAgent, DEFAULT};

/// Seconds the sub-agents of a stopped run get to finish their cleanup.
const CLEANUP: Duration = Duration::from_secs(5);

/// The most often the tokens spent so far are sent while sub-agents work.
const TICK: Duration = Duration::from_secs(1);

/// Why a step the person stopped failed.
pub const STOPPED: &str = "stopped by the person";

/// Characters of one report, of a group of reports, and of an `{{item}}`, wherever they go: into the answer or
/// into a later prompt.
pub const REPORT_CAP: usize = 6000;
pub const GROUP_CAP: usize = 24000;
pub const ITEM_CAP: usize = 2000;

const LIST_LINE: &str = "End your report with one JSON array of strings, one string per item, and nothing after it.";

/// Why no new sub-agent started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Agents(usize),
    Tokens(u64),
}

impl Stop {
    pub fn text(self) -> String {
        match self {
            Stop::Agents(n) => format!("max_agents ({n}) was reached"),
            Stop::Tokens(n) => format!("max_tokens ({n}) was reached"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Ok,
    /// Some of its sub-agents ended without a report, or never started.
    Partial {
        missing: usize,
    },
    Failed(String),
    Skipped(String),
    NotRun(String),
}

impl State {
    /// Whether later steps may use its report.
    pub fn usable(&self) -> bool {
        matches!(self, State::Ok | State::Partial { .. })
    }
}

#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub id: String,
    pub phase: usize,
    pub state: State,
    /// Sub-agents started for it.
    pub agents: usize,
    pub tokens: u64,
    pub secs: u64,
    /// Its reports: one as it is, or several each under a header.
    pub parts: Vec<Part>,
    /// The list it returned (`returns = "list"`), at most `MAX_ITEMS`.
    pub items: Vec<String>,
    /// Things worth a line in the result: items left out, an empty list.
    pub notes: Vec<String>,
}

/// One report of a step, with what is printed before it. The header and the count line are never cut; the text is.
#[derive(Debug, Clone)]
pub struct Part {
    /// `item 2: REAL 2 of 3`, on the first report of an item whose copies all gave one of the `answers`.
    pub count: Option<String>,
    /// `--- check #3 (item 1: a) (copy 3) ---`; empty for the one report of a plain step.
    pub head: String,
    pub text: String,
}

/// Characters one report gets when `parts` reports share the group cap.
pub fn share(parts: usize) -> usize {
    (GROUP_CAP / parts.max(1)).min(REPORT_CAP)
}

/// `text` as it is, or its first `max` characters and a line saying how many were cut.
pub fn cut(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    if head.is_empty() {
        format!("[cut: {n} more characters]")
    } else {
        format!("{head}\n[cut: {} more characters]", n - max)
    }
}

impl StepOutcome {
    /// Whether it has anything to show as a report.
    pub fn has_report(&self) -> bool {
        self.parts.iter().any(|p| !p.text.is_empty() || !p.head.is_empty())
    }

    /// Its reports as one text, each report cut to `share` characters.
    pub fn report(&self, share: usize) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            if let Some(c) = &p.count {
                out.push_str(&format!("=> {c}\n"));
            }
            if !p.head.is_empty() {
                out.push_str(&p.head);
                out.push('\n');
            }
            out.push_str(&cut(&p.text, share));
        }
        out
    }

    /// What `{{steps.<id>.report}}` brings into a later prompt: the same cap as in the answer.
    pub fn text(&self) -> String {
        self.report(share(self.parts.len()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub steps: Vec<StepOutcome>,
    /// The ceiling that stopped new sub-agents.
    pub stop: Option<Stop>,
    /// The person interrupted the run.
    pub cancelled: bool,
    /// The person stopped the run (or one step of it) from the view: not the interrupt of the turn.
    pub stopped: bool,
    /// The run ended early for a reason other than a ceiling (a phase where every step failed).
    pub ended: Option<String>,
    pub agents: usize,
    pub tokens: u64,
    pub secs: u64,
}

/// One sub-agent to start.
struct Unit {
    /// Index of its step in the phase.
    step: usize,
    /// 1-based, within the step.
    number: usize,
    /// Which item and which copy it is for.
    item: Option<(usize, String)>,
    copy: Option<usize>,
    prompt: String,
    label: String,
}

/// How a unit ended.
struct Done {
    /// Whether a sub-agent started (a refused start has none).
    ran: bool,
    result: Result<String, String>,
    items: Vec<String>,
    tokens: u64,
    started: Option<Instant>,
    ended: Option<Instant>,
}

struct Live {
    /// A sub-agent that can change files: it runs on its own, as the `task` tool runs one.
    writer: bool,
    job: u32,
    meter: SubMeter,
    at: Instant,
}

struct Counters {
    started: usize,
    tokens: u64,
}

/// Runs `wf` with `args`. Nothing here starts unless the checks of the file passed.
pub async fn run(sub: &SubAgent, wf: &Workflow, args: &str, ctx: &ToolContext, flow: &FlowRun) -> Outcome {
    let t0 = Instant::now();
    let mut out = Outcome::default();
    let mut counters = Counters { started: 0, tokens: 0 };
    let mut halt: Option<String> = None;
    for (pi, phase) in wf.phases.iter().enumerate() {
        if let Some(why) = &halt {
            for s in &phase.steps {
                let o = unrun(s, pi, State::NotRun(why.clone()));
                tell(flow, &o);
                out.steps.push(o);
            }
            continue;
        }
        let by_id: HashMap<String, usize> = out.steps.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect();
        // the units of the phase, and the steps that cannot run because what they need is missing
        let mut units: Vec<Unit> = Vec::new();
        let mut skipped: HashMap<usize, String> = HashMap::new();
        for (si, step) in phase.steps.iter().enumerate() {
            match plan_step(wf, step, si, args, &out.steps, &by_id) {
                Ok(mut u) => units.append(&mut u),
                Err(why) => {
                    skipped.insert(si, why);
                }
            }
        }
        let steps: Vec<&Step> = phase.steps.iter().collect();
        let (done, cancelled) = schedule(sub, wf, ctx, flow, pi, &units, &steps, &mut counters, &mut out.stop).await;
        for (si, step) in phase.steps.iter().enumerate() {
            let o = match skipped.get(&si) {
                Some(why) => unrun(step, pi, State::Skipped(why.clone())),
                None => step_outcome(step, pi, si, &units, &done, out.stop, flow),
            };
            tell(flow, &o);
            out.steps.push(o);
        }
        if cancelled {
            out.cancelled = true;
            halt = Some("the person interrupted the run".into());
            continue;
        }
        if flow.run_stopped() {
            out.stopped = true;
            halt = Some("the person stopped the run".into());
            continue;
        }
        if let Some(stop) = out.stop {
            halt = Some(stop.text());
            continue;
        }
        let this: Vec<&StepOutcome> = out.steps.iter().filter(|s| s.phase == pi).collect();
        if !this.iter().any(|s| s.state.usable()) {
            let why = format!("every step of phase \"{}\" failed", phase.title);
            out.ended = Some(why.clone());
            halt = Some(why);
        }
    }
    out.agents = counters.started;
    out.tokens = counters.tokens;
    out.secs = t0.elapsed().as_secs();
    flow.end(if out.cancelled || out.stopped {
        FlowState::Stopped
    } else if out.stop.is_some() || out.ended.is_some() || out.steps.iter().any(|s| s.state != State::Ok) {
        FlowState::Problems
    } else {
        FlowState::Ok
    });
    out
}

fn unrun(step: &Step, phase: usize, state: State) -> StepOutcome {
    StepOutcome {
        id: step.id.clone(),
        phase,
        state,
        agents: 0,
        tokens: 0,
        secs: 0,
        parts: Vec::new(),
        items: Vec::new(),
        notes: Vec::new(),
    }
}

/// The units of one step, or why it cannot run: a step it needs ended without a report.
fn plan_step(
    wf: &Workflow,
    step: &Step,
    si: usize,
    args: &str,
    before: &[StepOutcome],
    by_id: &HashMap<String, usize>,
) -> Result<Vec<Unit>, String> {
    let mut needs: Vec<&str> = Vec::new();
    if let Some(e) = &step.each {
        needs.push(e);
    }
    for piece in pieces(&step.prompt).unwrap_or_default() {
        if let Piece::Report(id) = piece {
            if !needs.contains(&id) {
                needs.push(id);
            }
        }
    }
    for id in needs {
        let Some(dep) = by_id.get(id).map(|i| &before[*i]) else { return Err(format!("step `{id}` did not run")) };
        match &dep.state {
            s if s.usable() => {}
            State::Failed(e) => return Err(format!("step `{id}` failed: {e}")),
            State::Skipped(_) => return Err(format!("step `{id}` was skipped")),
            State::NotRun(_) | State::Ok | State::Partial { .. } => return Err(format!("step `{id}` did not run")),
        }
    }
    let report = |id: &str| by_id.get(id).map(|i| before[*i].text()).unwrap_or_default();
    let label = |n: usize, many: bool| {
        let phase = wf.phases.iter().find(|p| p.steps.iter().any(|s| s.id == step.id)).map_or("", |p| p.title.as_str());
        let mut l = format!("{} · {} · {}", wf.name, phase, step.id);
        if many {
            l.push_str(&format!(" #{n}"));
        }
        l
    };
    let closing = |prompt: String| if step.list { format!("{prompt}\n\n{LIST_LINE}") } else { prompt };
    let items: Vec<Option<(usize, String)>> = match &step.each {
        None => vec![None],
        Some(e) => {
            let list = by_id.get(e.as_str()).map(|i| before[*i].items.clone()).unwrap_or_default();
            list.into_iter().enumerate().map(|(i, t)| Some((i + 1, t))).collect()
        }
    };
    let many = step.each.is_some() || step.votes > 1;
    let mut units = Vec::new();
    for item in items {
        let shown = item.as_ref().map(|(_, t)| cut(t, ITEM_CAP));
        let prompt = closing(expand(&step.prompt, args, shown.as_deref(), &report));
        for copy in 1..=step.votes {
            let number = units.len() + 1;
            units.push(Unit {
                step: si,
                number,
                item: item.clone(),
                copy: (step.votes > 1).then_some(copy),
                prompt: prompt.clone(),
                label: label(number, many),
            });
        }
    }
    Ok(units)
}

/// Starts the units of a phase, never more than `max_parallel` at work over the whole run (a phase starts
/// when the one before it has ended), and waits for them. `None` for a unit that never started. Everything the
/// person watching should see goes to `flow` as it happens.
#[allow(clippy::too_many_arguments)]
async fn schedule(
    sub: &SubAgent,
    wf: &Workflow,
    ctx: &ToolContext,
    flow: &FlowRun,
    pi: usize,
    units: &[Unit],
    steps: &[&Step],
    counters: &mut Counters,
    stop: &mut Option<Stop>,
) -> (Vec<Option<Done>>, bool) {
    let mut res: Vec<Option<Done>> = (0..units.len()).map(|_| None).collect();
    let mut live: HashMap<usize, Live> = HashMap::new();
    let mut running = FuturesUnordered::new();
    let (mut next, mut cancelled) = (0, false);
    // the tokens spent so far go out at most once a second
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        while next < units.len() && live.len() < wf.max_parallel && stop.is_none() && !cancelled && !flow.run_stopped()
        {
            if ctx.cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            // a step the person stopped starts nothing more
            if flow.step_stopped(&steps[units[next].step].id) {
                res[next] = Some(refused_unit(STOPPED.into()));
                next += 1;
                step_done(flow, pi, units, steps, &res, *stop, units[next - 1].step);
                continue;
            }
            // the rule of the `task` tool: a kind that can change files runs alone, nothing else at work and
            // nothing started until it ends (unless `parallel_tasks` lets them run side by side)
            let writer = sub.kind_of(steps[units[next].step].agent.as_deref().unwrap_or(DEFAULT)) == ToolKind::Write;
            if !live.is_empty() && (writer || live.values().any(|l| l.writer)) {
                break;
            }
            if counters.started >= wf.max_agents {
                *stop = Some(Stop::Agents(wf.max_agents));
                break;
            }
            let working: u64 = live.values().map(|l| sum(&l.meter)).sum();
            if counters.tokens + working >= wf.max_tokens {
                *stop = Some(Stop::Tokens(wf.max_tokens));
                break;
            }
            let i = next;
            next += 1;
            let (u, step) = (&units[i], steps[units[i].step]);
            let launch = Launch {
                agent: step.agent.as_deref().unwrap_or(DEFAULT),
                prompt: &u.prompt,
                provider: None,
                model: step.model.as_deref(),
                description: Some(&u.label),
                title: Some(&u.label),
                background: false,
                detachable: false,
            };
            match sub.launch(launch, ctx).await {
                Err(refused) => {
                    res[i] = Some(refused_unit(refused));
                    step_done(flow, pi, units, steps, &res, *stop, u.step);
                }
                Ok(s) => {
                    counters.started += 1;
                    flow.agent_started(&step.id, &s.run_id, s.id);
                    // the outcome is in the result of the run: the main agent is never told of this job on its own, not
                    // even in the moment between its end and the run settling it (a run the person started by name
                    // finds the main agent idle, and a note would start a turn)
                    ctx.jobs.tell(s.id);
                    // a stop that came while it was starting could not reach it yet
                    if flow.step_stopped(&step.id) || flow.run_stopped() {
                        ctx.jobs.kill(s.id);
                    }
                    live.insert(i, Live { writer, job: s.id, meter: s.meter.clone(), at: Instant::now() });
                    let rx = s.rx;
                    running.push(async move { (i, rx.await) });
                }
            }
        }
        if running.is_empty() {
            break;
        }
        if cancelled {
            // the interrupt reaches every sub-agent through its job, then they get a few seconds to end
            for l in live.values() {
                ctx.jobs.kill(l.job);
            }
            let drain = async {
                while let Some((i, ended)) = running.next().await {
                    settle(&mut res, &mut live, counters, ctx, flow, steps[units[i].step], i, ended);
                    step_done(flow, pi, units, steps, &res, *stop, units[i].step);
                }
            };
            let _ = tokio::time::timeout(CLEANUP, drain).await;
            break;
        }
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => cancelled = true,
            Some((i, ended)) = running.next() => {
                settle(&mut res, &mut live, counters, ctx, flow, steps[units[i].step], i, ended);
                step_done(flow, pi, units, steps, &res, *stop, units[i].step);
            }
            _ = tick.tick() => spent_so_far(flow, units, steps, &res, &live),
        }
    }
    (res, cancelled)
}

fn sum(m: &SubMeter) -> u64 {
    let (i, o) = m.tokens();
    i + o
}

/// The tokens each step of the phase has spent so far, ended sub-agents and those still at work.
fn spent_so_far(flow: &FlowRun, units: &[Unit], steps: &[&Step], res: &[Option<Done>], live: &HashMap<usize, Live>) {
    let mut per: Vec<u64> = vec![0; steps.len()];
    for (i, u) in units.iter().enumerate() {
        per[u.step] += match (&res[i], live.get(&i)) {
            (Some(d), _) => d.tokens,
            (None, Some(l)) => sum(&l.meter),
            (None, None) => 0,
        };
    }
    let spent: Vec<(&str, u64)> = steps.iter().zip(per).map(|(s, n)| (s.id.as_str(), n)).collect();
    flow.tokens(&spent);
}

/// The outcome of step `si` of phase `pi`, from what its units did; a step the person stopped is failed, with
/// that reason, whatever its units had done.
fn step_outcome(
    step: &Step,
    pi: usize,
    si: usize,
    units: &[Unit],
    done: &[Option<Done>],
    stop: Option<Stop>,
    flow: &FlowRun,
) -> StepOutcome {
    let mine: Vec<(&Unit, &Option<Done>)> = units.iter().zip(done).filter(|(u, _)| u.step == si).collect();
    let mut o = finish_step(step, pi, &mine, stop);
    if flow.step_stopped(&step.id) {
        o.state = State::Failed(STOPPED.into());
    }
    o
}

/// When the last unit of step `si` has ended, the step has: say so now, not at the end of the phase.
fn step_done(
    flow: &FlowRun,
    pi: usize,
    units: &[Unit],
    steps: &[&Step],
    res: &[Option<Done>],
    stop: Option<Stop>,
    si: usize,
) {
    if units.iter().zip(res).any(|(u, r)| u.step == si && r.is_none()) {
        return;
    }
    tell(flow, &step_outcome(steps[si], pi, si, units, res, stop, flow));
}

/// A step's end, as the view has it.
fn tell(flow: &FlowRun, o: &StepOutcome) {
    let (state, why) = match &o.state {
        State::Ok => (StepState::Ok, None),
        State::Partial { missing } => (StepState::Partial, Some(format!("{missing} without a report"))),
        State::Failed(e) => (StepState::Failed, Some(e.clone())),
        State::Skipped(e) => (StepState::Skipped, Some(e.clone())),
        State::NotRun(e) => (StepState::NotRun, Some(e.clone())),
    };
    flow.step_ended(&o.id, state, why.as_deref(), o.tokens);
}

fn refused_unit(why: String) -> Done {
    Done { ran: false, result: Err(why), items: Vec::new(), tokens: 0, started: None, ended: None }
}

/// A sub-agent ended: its report (and its list, for a step that returns one), or why there is none.
#[allow(clippy::too_many_arguments)]
fn settle(
    res: &mut [Option<Done>],
    live: &mut HashMap<usize, Live>,
    counters: &mut Counters,
    ctx: &ToolContext,
    flow: &FlowRun,
    step: &Step,
    i: usize,
    ended: Result<Ended, tokio::sync::oneshot::error::RecvError>,
) {
    let Some(l) = live.remove(&i) else { return };
    flow.agent_ended(&step.id, l.job);
    let list = step.list;
    let tokens = sum(&l.meter);
    counters.tokens += tokens;
    // the outcome is in the result the main agent gets: no ended note for the job
    ctx.jobs.tell(l.job);
    let (result, items) = match ended {
        Ok(Ended::Report(r)) if list => match parse_list(&r) {
            Ok(items) => (Ok(r), items),
            Err(why) => (Err(why), Vec::new()),
        },
        Ok(Ended::Report(r)) => (Ok(r), Vec::new()),
        Ok(Ended::Failed(e, _)) => (Err(e), Vec::new()),
        Ok(Ended::Stopped(e, _)) if ctx.cancel.is_cancelled() => {
            (Err(format!("stopped by the person ({e})")), Vec::new())
        }
        Ok(Ended::Stopped(..)) if flow.step_stopped(&step.id) || flow.run_stopped() => {
            (Err(STOPPED.into()), Vec::new())
        }
        Ok(Ended::Stopped(e, _)) => (Err(format!("the sub-agent was stopped ({e})")), Vec::new()),
        Err(_) => (Err("the sub-agent ended without a report".into()), Vec::new()),
    };
    res[i] = Some(Done { ran: true, result, items, tokens, started: Some(l.at), ended: Some(Instant::now()) });
}

/// The last JSON array of strings in a report. No array, or only arrays of something else: why not.
pub fn parse_list(report: &str) -> Result<Vec<String>, String> {
    let mut found: Option<Vec<String>> = None;
    let mut any = false;
    let mut from = 0;
    for (i, c) in report.char_indices() {
        // an array inside one already read is part of it
        if c != '[' || i < from {
            continue;
        }
        let mut it = serde_json::Deserializer::from_str(&report[i..]).into_iter::<Value>();
        if let Some(Ok(Value::Array(a))) = it.next() {
            from = i + it.byte_offset();
            any = true;
            let strings: Option<Vec<String>> = a.iter().map(|v| v.as_str().map(str::to_string)).collect();
            if strings.is_some() {
                found = strings;
            }
        }
    }
    match (found, any) {
        (Some(items), _) => Ok(items),
        (None, true) => Err("the report has a JSON array, but not one of strings".into()),
        (None, false) => Err("the report has no JSON array of strings at its end".into()),
    }
}

/// Which of `answers` a report begins with: ignoring case and the characters before its first letter or digit; the
/// longest entry wins when one begins with another.
fn answer_of(report: &str, answers: &[String]) -> Option<usize> {
    let rest = answer_key(report);
    answers
        .iter()
        .enumerate()
        .map(|(i, a)| (i, answer_key(a)))
        .filter(|(_, a)| !a.is_empty() && rest.starts_with(a.as_str()))
        .max_by_key(|(_, a)| a.chars().count())
        .map(|(i, _)| i)
}

/// `REAL 2 of 3, NOT REAL 1 of 3` for the copies of one item, only when every copy began with one of `answers`.
fn count_line(copies: &[&str], answers: &[String]) -> Option<String> {
    if copies.len() < 2 || answers.is_empty() {
        return None;
    }
    let mut counts = vec![0usize; answers.len()];
    for r in copies {
        counts[answer_of(r, answers)?] += 1;
    }
    let parts: Vec<String> = answers
        .iter()
        .zip(&counts)
        .filter(|(_, n)| **n > 0)
        .map(|(a, n)| format!("{a} {n} of {}", copies.len()))
        .collect();
    Some(parts.join(", "))
}

/// One line of an item, for a header.
fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(max).collect();
    if line.chars().count() > max || text.trim().lines().count() > 1 {
        format!("{cut} ...")
    } else {
        cut
    }
}

fn finish_step(step: &Step, phase: usize, mine: &[(&Unit, &Option<Done>)], stop: Option<Stop>) -> StepOutcome {
    let mut o = unrun(step, phase, State::Ok);
    let (mut ok, mut failed, mut unrun_n) = (0usize, 0usize, 0usize);
    let mut first_error: Option<String> = None;
    let (mut first, mut last): (Option<Instant>, Option<Instant>) = (None, None);
    let mut seen = std::collections::HashSet::new();
    let mut all_items: Vec<String> = Vec::new();
    let (mut merged, mut unmetered) = (0usize, 0usize);
    for (_, d) in mine {
        let Some(d) = d else {
            unrun_n += 1;
            continue;
        };
        if d.ran {
            o.agents += 1;
        }
        o.tokens += d.tokens;
        // a sub-agent that reported and gave no usage: it counts as 0 tokens, and the result says so. One that
        // failed, was stopped or errored before any usage had nothing to report it with.
        if d.ran && d.tokens == 0 && d.result.is_ok() {
            unmetered += 1;
        }
        first = first.into_iter().chain(d.started).min();
        last = last.into_iter().chain(d.ended).max();
        match &d.result {
            Ok(_) => {
                ok += 1;
                for i in &d.items {
                    if seen.insert(i.clone()) {
                        all_items.push(i.clone());
                    } else {
                        merged += 1;
                    }
                }
            }
            Err(e) => {
                failed += 1;
                first_error.get_or_insert_with(|| e.clone());
            }
        }
    }
    o.secs = match (first, last) {
        (Some(a), Some(b)) => b.duration_since(a).as_secs(),
        _ => 0,
    };
    if merged > 0 {
        o.notes.push(format!("{merged} equal item{} merged", if merged == 1 { "" } else { "s" }));
    }
    if unmetered > 0 {
        o.notes.push(format!(
            "token use was not reported for {unmetered} sub-agent{}; {} counted as 0 tokens",
            if unmetered == 1 { "" } else { "s" },
            if unmetered == 1 { "it is" } else { "they are" }
        ));
    }
    if all_items.len() > MAX_ITEMS {
        o.notes.push(format!(
            "it returned {} items; the first {MAX_ITEMS} are used and the rest are left out",
            all_items.len()
        ));
        all_items.truncate(MAX_ITEMS);
    }
    if step.list && mine.is_empty() {
        o.notes.push("there was nothing to run it on".into());
    }
    o.items = all_items;
    o.parts = join(step, mine);
    o.state = if ok == mine.len() {
        State::Ok
    } else if ok > 0 {
        State::Partial { missing: failed + unrun_n }
    } else if failed == 0 && unrun_n > 0 {
        State::NotRun(stop.map_or_else(|| "the run was stopped".into(), Stop::text))
    } else {
        State::Failed(first_error.unwrap_or_else(|| "it ended without a report".into()))
    };
    o
}

/// The reports of a step: a step with one sub-agent gives its report as it is; with several, each has a header, and
/// the copies of an item that all gave one of `answers` get a count line first.
fn join(step: &Step, mine: &[(&Unit, &Option<Done>)]) -> Vec<Part> {
    let reports: Vec<(&Unit, &str)> = mine
        .iter()
        .filter_map(|(u, d)| match d.as_ref().map(|d| &d.result) {
            Some(Ok(r)) => Some((*u, r.as_str())),
            _ => None,
        })
        .collect();
    if step.each.is_none() && step.votes == 1 {
        return reports
            .first()
            .map(|(_, r)| Part { count: None, head: String::new(), text: r.to_string() })
            .into_iter()
            .collect();
    }
    let mut out = Vec::new();
    let mut last: Option<Option<usize>> = None;
    for (u, r) in &reports {
        let item = u.item.as_ref().map(|(n, _)| *n);
        let mut count = None;
        if step.votes > 1 && last != Some(item) {
            // a new item: the count of its copies, when every copy ended with a report
            let copies: Vec<&str> =
                reports.iter().filter(|(o, _)| o.item.as_ref().map(|(n, _)| *n) == item).map(|(_, r)| *r).collect();
            let all = mine.iter().filter(|(o, _)| o.item.as_ref().map(|(n, _)| *n) == item).count();
            if copies.len() == all {
                count = count_line(&copies, &step.answers)
                    .map(|line| format!("{}{line}", item.map_or(String::new(), |n| format!("item {n}: "))));
            }
        }
        last = Some(item);
        let mut head = format!("--- {} #{}", step.id, u.number);
        if let Some((n, t)) = &u.item {
            head.push_str(&format!(" (item {n}: {})", one_line(t, 80)));
        }
        if let Some(c) = u.copy {
            head.push_str(&format!(" (copy {c})"));
        }
        head.push_str(" ---");
        out.push(Part { count, head, text: r.trim_end().to_string() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_array_of_strings_is_the_list() {
        assert_eq!(parse_list("x\n[\"a\", \"b\"]").unwrap(), vec!["a", "b"]);
        // the last one wins, and text after it does not matter
        assert_eq!(parse_list("[\"a\"] then [\"b\", \"c\"] fin").unwrap(), vec!["b", "c"]);
        // an array of something else after it does not replace it
        assert_eq!(parse_list("[\"a\"]\nsizes [1, 2]").unwrap(), vec!["a"]);
        // an array inside a longer one is part of it
        assert!(parse_list("[[\"a\"], [\"b\"]]").unwrap_err().contains("not one of strings"));
        assert_eq!(parse_list("[]").unwrap(), Vec::<String>::new());
        assert!(parse_list("no list here").unwrap_err().contains("no JSON array"));
        assert!(parse_list("see [1, 2, 3]").unwrap_err().contains("not one of strings"));
        assert!(parse_list("broken [\"a\", ").unwrap_err().contains("no JSON array"));
    }

    fn words(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_count_line_needs_every_copy_to_begin_with_one_of_the_answers() {
        let yn = words(&["REAL", "NOT REAL"]);
        assert_eq!(
            count_line(&["REAL: a", "real. b", "NOT REAL"], &yn).as_deref(),
            Some("REAL 2 of 3, NOT REAL 1 of 3")
        );
        assert_eq!(count_line(&["Real", "**real**"], &yn).as_deref(), Some("REAL 2 of 2"));
        // an answer that is not in the list: no line
        assert_eq!(count_line(&["REAL", "Verdict: REAL"], &yn), None);
        assert_eq!(count_line(&["REAL", ""], &yn), None);
        // the first-word guess is gone: no `answers`, no line
        assert_eq!(count_line(&["Yes", "Yes"], &[]), None);
        assert_eq!(count_line(&["REAL"], &yn), None);
        // the longest entry wins when one begins with another
        assert_eq!(answer_of("not real at all", &words(&["NOT", "NOT REAL"])), Some(1));
        assert_eq!(answer_of("Not sure", &words(&["NOT", "NOT REAL"])), Some(0));
        assert_eq!(
            count_line(&["NOT REAL", "NOT REAL"], &words(&["NOT", "NOT REAL"])).as_deref(),
            Some("NOT REAL 2 of 2")
        );
    }

    #[test]
    fn a_cut_says_how_much_and_keeps_whole_characters() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "abc\n[cut: 1 more characters]");
        assert_eq!(cut("abcd", 0), "[cut: 4 more characters]");
        assert_eq!(cut("ééééé", 2), "éé\n[cut: 3 more characters]");
        assert_eq!(share(1), REPORT_CAP);
        assert_eq!(share(4), 6000);
        assert_eq!(share(5), 4800);
        assert_eq!(share(9), 2666);
    }
}
