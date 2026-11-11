//! The runner: phases in order, the steps of a phase together, every sub-agent started through the code the
//! `task` tool starts its own with ([`SubAgent::launch`]), within the ceilings of the workflow.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures::stream::FuturesUnordered;
use futures::StreamExt;
use nexo_core::subs::SubMeter;
use nexo_core::tool::ToolContext;
use serde_json::Value;

use super::file::{expand, pieces, Piece, Step, Workflow, MAX_ITEMS};
use crate::subagent::{Ended, Launch, SubAgent, DEFAULT};

/// Seconds the sub-agents of a stopped run get to finish their cleanup.
const CLEANUP: Duration = Duration::from_secs(5);

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
    /// Its reports: one report as it is, several joined each under a header.
    pub report: String,
    /// How many reports `report` holds.
    pub parts: usize,
    /// The list it returned (`returns = "list"`), at most `MAX_ITEMS`.
    pub items: Vec<String>,
    /// Things worth a line in the result: items left out, an empty list.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub steps: Vec<StepOutcome>,
    /// The ceiling that stopped new sub-agents.
    pub stop: Option<Stop>,
    /// The person interrupted the run.
    pub cancelled: bool,
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
    job: u32,
    meter: SubMeter,
    at: Instant,
}

struct Counters {
    started: usize,
    tokens: u64,
}

/// Runs `wf` with `args`. Nothing here starts unless the checks of the file passed.
pub async fn run(sub: &SubAgent, wf: &Workflow, args: &str, ctx: &ToolContext) -> Outcome {
    let t0 = Instant::now();
    let mut out = Outcome::default();
    let mut counters = Counters { started: 0, tokens: 0 };
    let mut halt: Option<String> = None;
    for (pi, phase) in wf.phases.iter().enumerate() {
        if let Some(why) = &halt {
            for s in &phase.steps {
                out.steps.push(unrun(s, pi, State::NotRun(why.clone())));
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
        let (done, cancelled) = schedule(sub, wf, ctx, &units, &steps, &mut counters, &mut out.stop).await;
        for (si, step) in phase.steps.iter().enumerate() {
            if let Some(why) = skipped.get(&si) {
                out.steps.push(unrun(step, pi, State::Skipped(why.clone())));
                continue;
            }
            let mine: Vec<(&Unit, &Option<Done>)> = units.iter().zip(&done).filter(|(u, _)| u.step == si).collect();
            out.steps.push(finish_step(step, pi, &mine, out.stop));
        }
        if cancelled {
            out.cancelled = true;
            halt = Some("the person interrupted the run".into());
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
        report: String::new(),
        parts: 0,
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
    let report = |id: &str| by_id.get(id).map(|i| before[*i].report.clone()).unwrap_or_default();
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
        let prompt = closing(expand(&step.prompt, args, item.as_ref().map(|(_, t)| t.as_str()), &report));
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
/// when the one before it has ended), and waits for them. `None` for a unit that never started.
async fn schedule(
    sub: &SubAgent,
    wf: &Workflow,
    ctx: &ToolContext,
    units: &[Unit],
    steps: &[&Step],
    counters: &mut Counters,
    stop: &mut Option<Stop>,
) -> (Vec<Option<Done>>, bool) {
    let mut res: Vec<Option<Done>> = (0..units.len()).map(|_| None).collect();
    let mut live: HashMap<usize, Live> = HashMap::new();
    let mut running = FuturesUnordered::new();
    let (mut next, mut cancelled) = (0, false);
    loop {
        while next < units.len() && live.len() < wf.max_parallel && stop.is_none() && !cancelled {
            if ctx.cancel.is_cancelled() {
                cancelled = true;
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
                Err(refused) => res[i] = Some(refused_unit(refused)),
                Ok(s) => {
                    counters.started += 1;
                    live.insert(i, Live { job: s.id, meter: s.meter.clone(), at: Instant::now() });
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
                    settle(&mut res, &mut live, counters, ctx, steps[units[i].step].list, i, ended);
                }
            };
            let _ = tokio::time::timeout(CLEANUP, drain).await;
            break;
        }
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => cancelled = true,
            Some((i, ended)) = running.next() => {
                settle(&mut res, &mut live, counters, ctx, steps[units[i].step].list, i, ended);
            }
        }
    }
    (res, cancelled)
}

fn sum(m: &SubMeter) -> u64 {
    let (i, o) = m.tokens();
    i + o
}

fn refused_unit(why: String) -> Done {
    Done { ran: false, result: Err(why), items: Vec::new(), tokens: 0, started: None, ended: None }
}

/// A sub-agent ended: its report (and its list, for a step that returns one), or why there is none.
fn settle(
    res: &mut [Option<Done>],
    live: &mut HashMap<usize, Live>,
    counters: &mut Counters,
    ctx: &ToolContext,
    list: bool,
    i: usize,
    ended: Result<Ended, tokio::sync::oneshot::error::RecvError>,
) {
    let Some(l) = live.remove(&i) else { return };
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

/// The first word of a report, as a verdict: letters and digits, upper case.
fn verdict(report: &str) -> String {
    let w = report.split_whitespace().next().unwrap_or("");
    w.trim_matches(|c: char| !c.is_alphanumeric()).to_uppercase()
}

/// `REAL 2 of 3` for the copies of one item, when every copy ended with a report and they begin with no more
/// than two different words; none otherwise.
fn count_line(copies: &[&str]) -> Option<String> {
    if copies.len() < 2 {
        return None;
    }
    let words: Vec<String> = copies.iter().map(|r| verdict(r)).collect();
    let mut tally: Vec<(String, usize)> = Vec::new();
    for w in &words {
        match tally.iter_mut().find(|(t, _)| t == w) {
            Some((_, n)) => *n += 1,
            None => tally.push((w.clone(), 1)),
        }
    }
    if tally.len() > 2 || tally.iter().any(|(w, _)| w.is_empty()) {
        return None;
    }
    let (top, n) = tally.iter().fold(&tally[0], |best, t| if t.1 > best.1 { t } else { best });
    Some(format!("{top} {n} of {}", copies.len()))
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
    for (_, d) in mine {
        let Some(d) = d else {
            unrun_n += 1;
            continue;
        };
        if d.ran {
            o.agents += 1;
        }
        o.tokens += d.tokens;
        first = first.into_iter().chain(d.started).min();
        last = last.into_iter().chain(d.ended).max();
        match &d.result {
            Ok(_) => {
                ok += 1;
                for i in &d.items {
                    if seen.insert(i.clone()) {
                        all_items.push(i.clone());
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
    o.report = join(step, mine);
    o.parts = ok;
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

/// The reports of a step as one text: a step with one sub-agent gives its report as it is; with several, each
/// is under a header, and the copies of an item that agree get a count line first.
fn join(step: &Step, mine: &[(&Unit, &Option<Done>)]) -> String {
    let reports: Vec<(&Unit, &str)> = mine
        .iter()
        .filter_map(|(u, d)| match d.as_ref().map(|d| &d.result) {
            Some(Ok(r)) => Some((*u, r.as_str())),
            _ => None,
        })
        .collect();
    if step.each.is_none() && step.votes == 1 {
        return reports.first().map(|(_, r)| r.to_string()).unwrap_or_default();
    }
    let mut out = String::new();
    let mut last: Option<Option<usize>> = None;
    for (u, r) in &reports {
        let item = u.item.as_ref().map(|(n, _)| *n);
        if step.votes > 1 && last != Some(item) {
            // a new item: the count of its copies, when every copy ended with a report
            let copies: Vec<&str> =
                reports.iter().filter(|(o, _)| o.item.as_ref().map(|(n, _)| *n) == item).map(|(_, r)| *r).collect();
            let all = mine.iter().filter(|(o, _)| o.item.as_ref().map(|(n, _)| *n) == item).count();
            if copies.len() == all {
                if let Some(line) = count_line(&copies) {
                    out.push_str(&format!("=> {}{line}\n", item.map_or(String::new(), |n| format!("item {n}: "))));
                }
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
        head.push_str(" ---\n");
        out.push_str(&head);
        out.push_str(r.trim_end());
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
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

    #[test]
    fn a_count_line_needs_one_or_two_first_words_and_two_copies() {
        assert_eq!(count_line(&["REAL: a", "real. b", "NOT REAL"]).as_deref(), Some("REAL 2 of 3"));
        assert_eq!(count_line(&["Yes", "Yes"]).as_deref(), Some("YES 2 of 2"));
        assert_eq!(count_line(&["A", "B", "C"]), None);
        assert_eq!(count_line(&["only one"]), None);
        assert_eq!(count_line(&["", "x"]), None);
    }
}
