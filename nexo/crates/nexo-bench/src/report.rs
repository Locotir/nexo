//! `summary.md`: the settings, one row per arm (totals), the arms paired with the first one by
//! task, the graded parts of tasks that have them, then one row per run. `results.jsonl` has every
//! number; this is the short view.

use crate::client::Stop;
use crate::run::Record;
use crate::stats::{bootstrap, paired, spread, Delta, RESAMPLES};

/// Fewest tasks, and fewest runs per task and arm, for which an interval is shown as one.
pub const MIN_TASKS: usize = 5;
pub const MIN_REPEATS: usize = 3;

/// What the summary's header says about the benchmark.
pub struct Header {
    pub model: String,
    pub upstream: String,
    /// The nexo commit measured.
    pub commit: String,
    pub max_steps: u32,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub repeats: u32,
    pub budget: Option<f64>,
    /// Tasks this machine could not run, with why.
    pub skipped: Vec<String>,
    /// How the arms were ordered in each repeat (none: not known, rebuilt from results).
    pub arm_order: Option<String>,
    /// The prices given with `--price`, as the summary shows them (none: nexo's own for the model).
    pub price: Option<String>,
    /// Why the benchmark stopped early, if it did.
    pub stopped: Option<String>,
    /// What every priced reply cost, runs that ended in an error included (none: the records' sum).
    pub spent: Option<f64>,
    /// The command that repeats the benchmark, for the Polyglot section.
    pub repro: Option<String>,
}

/// The columns after the arm or the run, in both tables. What counts is what a solved task costs
/// (every check passed): a cheap run that does not solve it saved nothing, so `$ / solved` and
/// `steps / solved` come first and spread the cost of the failed runs over the solved ones.
const COLUMNS: [&str; 21] = [
    "solved",
    "$ / solved",
    "steps / solved",
    "score",
    "steps",
    "requests",
    "input",
    "cache read",
    "cache write",
    "output",
    "cost $",
    "wall s",
    "tool errors",
    "final view",
    "max view",
    "compactions",
    "rewrites",
    "recall calls",
    "re-reads",
    "needless",
    "dup lines %",
];

/// A header read from the records themselves, for reports rebuilt from old results.
pub fn header_of(records: &[Record]) -> Header {
    let first = records.first().cloned().unwrap_or_default();
    Header {
        model: first.model,
        upstream: String::from("n/a (rebuilt from results.jsonl)"),
        commit: first.commit,
        max_steps: first.max_steps,
        max_tokens: first.max_tokens,
        temperature: first.temperature,
        repeats: records.iter().map(|r| r.rep + 1).max().unwrap_or(1),
        budget: None,
        price: None,
        arm_order: None,
        skipped: vec![],
        stopped: None,
        spent: None,
        repro: None,
    }
}

/// The settings the records do not agree on, if any.
fn mixed(records: &[Record]) -> Option<String> {
    let key = |r: &Record| (r.model.clone(), r.commit.clone(), r.temperature, r.max_steps, r.max_tokens);
    let mut groups: Vec<_> = Vec::new();
    for r in records {
        if !groups.contains(&key(r)) {
            groups.push(key(r));
        }
    }
    if groups.len() < 2 {
        return None;
    }
    let list: Vec<String> = groups
        .iter()
        .map(|(m, c, t, s, k)| {
            format!(
                "{m} @ {c}, temperature {}, max steps {s}, max tokens {}",
                t.map_or("default".into(), |t| t.to_string()),
                k.map_or("default".into(), |n| n.to_string())
            )
        })
        .collect();
    Some(format!(
        "**Mixed settings**: these results come from {} different setups, so the header above is only the first \
         record's and arms are not comparable across them: {}.",
        groups.len(),
        list.join("; ")
    ))
}

/// `x` dollars, or `unknown` when the model has no known price.
fn usd(unknown: bool, x: f64, places: usize) -> String {
    if unknown {
        "unknown".into()
    } else {
        format!("{x:.places$}")
    }
}

/// Prices given on the command line, as the summary shows them.
pub fn price_line(p: &nexo_core::Pricing) -> String {
    let write = if p.cache_write > 0.0 { format!(", ${} a cache write", p.cache_write) } else { String::new() };
    format!(
        "${} input on a cache miss, ${} on a cache hit, ${} output{write}, per million tokens",
        p.input, p.cache_read, p.output
    )
}

/// The summary of `records` in Markdown.
pub fn summary(h: &Header, records: &[Record]) -> String {
    let recorded: f64 = records.iter().map(|r| r.cost).sum();
    // what the provider billed: runs that ended in an error (no record) spent money too
    let total = h.spent.unwrap_or(recorded).max(recorded);
    let estimated = records.iter().any(|r| r.estimated);
    let mut s = format!(
        "# nexo bench\n\n| model | commit | max steps | max tokens | temperature | repeats | budget | spent |\n\
         |---|---|---|---|---|---|---|---|\n| `{}` | `{}` | {} | {} | {} | {} | {} | **{}** |\n\nUpstream: {}\n",
        h.model,
        h.commit,
        h.max_steps,
        h.max_tokens.map_or("default".into(), |n| n.to_string()),
        h.temperature.map_or("default".into(), |t| t.to_string()),
        h.repeats,
        h.budget.map_or("none".into(), |b| format!("${b}")),
        if estimated { "unknown".into() } else { format!("${total:.4}") },
        h.upstream,
    );
    if !estimated && total - recorded >= 0.00005 {
        s.push_str(&format!("\nOf what was spent, ${:.4} went to runs that were skipped.\n", total - recorded));
    }
    if let Some(p) = &h.price {
        s.push_str(&format!("\nPrices given with --price: {p}.\n"));
    }
    if estimated {
        s.push_str("\nPrices unknown: nexo has no price list for this model, so costs are shown as unknown, not guessed from another vendor (--price gives them).\n");
    }
    if let Some(order) = &h.arm_order {
        s.push_str(&format!("\nArm order: {order}.\n"));
    }
    if let Some(m) = mixed(records) {
        s.push_str(&format!("\n{m}\n"));
    }
    if let Some(why) = &h.stopped {
        s.push_str(&format!("\n**Stopped early**: {why}.\n"));
    }
    for why in &h.skipped {
        s.push_str(&format!("\nSkipped: {why}\n"));
    }
    let dirty = records.iter().filter(|r| r.contaminated).count();
    let clean: Vec<Record> = records.iter().filter(|r| !r.contaminated).cloned().collect();
    let mut arms: Vec<&str> = Vec::new();
    for r in records {
        if !arms.contains(&r.arm.as_str()) {
            arms.push(&r.arm);
        }
    }
    if arms.is_empty() {
        return s;
    }
    if let Some(p) = crate::polyglot::report::section(h.repro.as_deref(), records) {
        s.push_str(&p);
    }
    s.push_str("\n## Arms\n\nScore and views are means over runs; the other counts are totals. Runs that reached outside their folders (contaminated) are left out of the numbers and counted apart.\n\n");
    s.push_str(&header(&["arm", "settings", "runs", "contaminated"]));
    for arm in &arms {
        let all: Vec<&Record> = records.iter().filter(|r| r.arm == *arm).collect();
        let rows: Vec<&Record> = all.iter().copied().filter(|r| !r.contaminated).collect();
        let bad = all.len() - rows.len();
        s.push_str(&row(
            &[arm.to_string(), settings(all[0]), rows.len().to_string(), bad.to_string()],
            &numbers(&rows),
        ));
    }
    s.push_str(&time_section(records, &arms));
    let base = arms[0];
    let pairs: Vec<_> = arms[1..].iter().filter_map(|a| paired(records, base, a)).collect();
    if !pairs.is_empty() {
        s.push_str(&format!(
            "\n## Paired with {base}\n\nPer task (repeats averaged), arm minus {base}; mean and 95% bootstrap \
             interval over tasks. Runs stopped by the budget are left out. With fewer than {MIN_TASKS} tasks or {MIN_REPEATS} runs \
             per task and arm the interval is not shown: the mark \"not enough runs\" stands in its place.\n\n| arm | tasks | min runs | Δ score (points) | Δ cost $ | Δ steps | Δ recall calls | Δ re-reads | Δ dup lines (points) | Δ wall s |\n|---|---|---|---|---|---|---|---|---|---|\n"
        ));
        let unknown = records.iter().any(|r| r.estimated);
        for p in &pairs {
            let enough = p.tasks >= MIN_TASKS && p.reps_min >= MIN_REPEATS;
            let d = |d: Delta, k: f64, places: usize| delta(d, k, places, enough);
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                p.arm,
                p.tasks,
                p.reps_min,
                d(p.score, 100.0, 1),
                if unknown { "unknown".into() } else { d(p.cost, 1.0, 4) },
                d(p.steps, 1.0, 1),
                d(p.recalls, 1.0, 1),
                d(p.re_reads, 1.0, 1),
                d(p.dup, 1.0, 1),
                d(p.wall, 1.0, 1)
            ));
        }
        let left_out: Vec<&str> = pairs.iter().flat_map(|p| p.excluded.iter().map(String::as_str)).collect();
        if !left_out.is_empty() {
            let mut left_out = left_out;
            left_out.sort();
            left_out.dedup();
            s.push_str(&format!("\nLeft out (stopped by the budget): {}.\n", left_out.join(", ")));
        }
        let mut dirty_runs: Vec<&str> = pairs.iter().flat_map(|p| p.contaminated.iter().map(String::as_str)).collect();
        dirty_runs.sort();
        dirty_runs.dedup();
        if !dirty_runs.is_empty() {
            s.push_str(&format!("\nLeft out (contaminated): {}.\n", dirty_runs.join(", ")));
        }
        for p in pairs.iter().filter(|p| p.capped > 0) {
            s.push_str(&format!(
                "\n{}: {} run(s) of {} and {base} hit the step cap; kept, counted apart.\n",
                p.arm, p.capped, p.arm
            ));
        }
    }
    s.push_str(&steps_table(&clean, &arms));
    s.push_str(&parts(&clean, &arms));
    s.push_str(&closed_book(&clean, &arms));
    if dirty > 0 {
        s.push_str(&format!(
            "\n## Contaminated runs\n\n{dirty} run(s) used absolute paths outside their working directory, so they may have read the answer elsewhere; they are left out of the means and deltas above.\n\n| task | arm | paths |\n|---|---|---|\n"
        ));
        for r in records.iter().filter(|r| r.contaminated) {
            let paths: Vec<String> = r.outside.iter().map(|p| format!("`{p}`")).collect();
            s.push_str(&format!("| {} #{} | {} | {} |\n", r.task, r.rep, r.arm, paths.join(", ")));
        }
    }
    s.push_str("\n## Runs\n\n");
    s.push_str(&header(&["task", "arm"]));
    for r in records {
        let mut task = r.task.clone();
        if h.repeats > 1 {
            task.push_str(&format!(" #{}", r.rep));
        }
        if !r.finished {
            task.push_str(" (stopped)");
        }
        if r.contaminated {
            task.push_str(" (contaminated)");
        }
        s.push_str(&row(&[task, r.arm.clone()], &numbers(&[r])));
    }
    s
}

/// The closed-book control: for each task `X-closed` whose open twin `X` also ran, per arm the
/// open and the closed score and solves side by side. A closed run that solves the task answered
/// from memory of the text, so the open result for that task proves nothing about reading. A
/// closed score above zero alone is expected: parts the edits do not touch can be recalled.
fn closed_book(records: &[Record], arms: &[&str]) -> String {
    let mut s = String::new();
    let mut tasks: Vec<&str> = Vec::new();
    for r in records {
        let twin = r.task.strip_suffix("-closed");
        if twin.is_some_and(|t| records.iter().any(|o| o.task == t)) && !tasks.contains(&r.task.as_str()) {
            tasks.push(&r.task);
        }
    }
    if tasks.is_empty() {
        return s;
    }
    s.push_str(
        "\n## Closed-book control\n\nThe same task with no book in the folder. Passing it means the answer came from \
         memory, not from reading (contaminated). A partial closed score is expected: the parts the edits do not \
         touch can be recalled.\n\n| task | arm | open score | open solved | closed score | closed solved | verdict |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for closed in tasks {
        let open = closed.trim_end_matches("-closed");
        for arm in arms {
            let pick = |task: &str| -> Vec<&Record> {
                records.iter().filter(|r| r.task == task && r.arm == *arm && r.stop != Some(Stop::Budget)).collect()
            };
            let (o, c) = (pick(open), pick(closed));
            if o.is_empty() || c.is_empty() {
                continue;
            }
            let mean = |v: &[&Record]| v.iter().map(|r| r.score).sum::<f64>() / v.len() as f64;
            let solved = |v: &[&Record]| format!("{}/{}", v.iter().filter(|r| r.ok).count(), v.len());
            let verdict = if c.iter().any(|r| r.ok) { "CONTAMINATED: solved without the book" } else { "clean" };
            s.push_str(&format!(
                "| {open} | {arm} | {:.2} | {} | {:.2} | {} | {verdict} |\n",
                mean(&o),
                solved(&o),
                mean(&c),
                solved(&c)
            ));
        }
    }
    s
}

/// For each task whose runs have graded parts: every run's parts and each part's rate per arm,
/// then per arm the right parts and the cost (mean ± sd), the cost per right part, and the right
/// parts minus the first arm's (± the standard error of that difference). Runs stopped by the
/// budget are listed but left out of the rest.
fn parts(records: &[Record], arms: &[&str]) -> String {
    let mut s = String::new();
    let mut tasks: Vec<&str> = Vec::new();
    for r in records.iter().filter(|r| !r.parts.is_empty()) {
        if !tasks.contains(&r.task.as_str()) {
            tasks.push(&r.task);
        }
    }
    let cut = |r: &Record| r.stop == Some(Stop::Budget);
    for task in tasks {
        let runs: Vec<&Record> = records.iter().filter(|r| r.task == task && !r.parts.is_empty()).collect();
        let unknown = runs.iter().any(|r| r.estimated);
        let ids: Vec<&str> = runs[0].parts.iter().map(|p| p.id.as_str()).collect();
        let ok = |r: &Record, id: &str| r.parts.iter().find(|p| p.id == id).and_then(|p| p.ok);
        let right = |r: &Record| ids.iter().filter(|id| ok(r, id) == Some(true)).count();
        let numbers: Vec<String> = (1..=ids.len()).map(|i| i.to_string()).collect();
        s.push_str(&format!(
            "\n## Parts of {task}\n\n1 right, x wrong, · not answered. Runs stopped by the budget are left out of \
             the rates and the means.\n\n| arm | run | {} | right | cost $ |\n|{}\n",
            numbers.join(" | "),
            "---|".repeat(ids.len() + 4)
        ));
        let mut means: Vec<(&str, Vec<f64>, Vec<f64>)> = Vec::new();
        for arm in arms {
            let mine: Vec<&Record> = runs.iter().copied().filter(|r| r.arm == *arm).collect();
            if mine.is_empty() {
                continue;
            }
            for r in &mine {
                let marks: Vec<&str> = ids
                    .iter()
                    .map(|id| match ok(r, id) {
                        Some(true) => "1",
                        Some(false) => "x",
                        None => "·",
                    })
                    .collect();
                let run = format!("#{}{}", r.rep, if cut(r) { " (budget)" } else { "" });
                s.push_str(&format!(
                    "| {arm} | {run} | {} | {} | {} |\n",
                    marks.join(" | "),
                    right(r),
                    usd(r.estimated, r.cost, 4)
                ));
            }
            let whole: Vec<&Record> = mine.into_iter().filter(|r| !cut(r)).collect();
            let rates: Vec<String> = ids
                .iter()
                .map(|id| format!("{}/{}", whole.iter().filter(|r| ok(r, id) == Some(true)).count(), whole.len()))
                .collect();
            s.push_str(&format!("| {arm} | rate | {} | | |\n", rates.join(" | ")));
            means.push((arm, whole.iter().map(|r| right(r) as f64).collect(), whole.iter().map(|r| r.cost).collect()));
        }
        let legend: Vec<String> = ids.iter().enumerate().map(|(i, id)| format!("{} `{id}`", i + 1)).collect();
        s.push_str(&format!("\n{}\n", legend.join(" · ")));
        let Some((base, base_right, _)) = means.first().cloned() else { continue };
        s.push_str(&format!(
            "\n| arm | runs | right | cost $ | $ per right | Δ right vs {base} |\n|---|---|---|---|---|---|\n"
        ));
        let pm = |(mean, sd): (f64, Option<f64>), places: usize| match sd {
            Some(d) => format!("{mean:.places$} ± {d:.places$}"),
            None => format!("{mean:.places$}"),
        };
        for (arm, rights, costs) in &means {
            let total: f64 = rights.iter().sum();
            let per = if unknown {
                "unknown".into()
            } else if total > 0.0 {
                format!("{:.4}", costs.iter().sum::<f64>() / total)
            } else {
                "–".into()
            };
            let delta = if *arm == base {
                String::new()
            } else {
                let ((a, sa), (b, sb)) = (spread(rights), spread(&base_right));
                let se = sa
                    .zip(sb)
                    .map(|(sa, sb)| (sa * sa / rights.len() as f64 + sb * sb / base_right.len() as f64).sqrt());
                match se {
                    Some(e) => format!("{:+.2} ± {e:.2}", a - b),
                    None => format!("{:+.2}", a - b),
                }
            };
            s.push_str(&format!(
                "| {arm} | {} | {} | {} | {per} | {delta} |\n",
                rights.len(),
                pm(spread(rights), 2),
                if unknown { "unknown".into() } else { pm(spread(costs), 4) }
            ));
        }
    }
    s
}

/// What sets an arm apart: its policy and parameters.
fn settings(r: &Record) -> String {
    match (r.policy.as_str(), r.compact_at) {
        ("compact", Some(at)) => format!("compact at {at}"),
        (p, _) if !r.spec.is_empty() => format!("{p} `{}`", r.spec),
        (p, _) => p.to_string(),
    }
}

fn header(first: &[&str]) -> String {
    let names: Vec<&str> = first.iter().chain(COLUMNS.iter()).copied().collect();
    format!("| {} |\n|{}\n", names.join(" | "), "---|".repeat(names.len()))
}

fn row(first: &[String], rest: &[String]) -> String {
    format!("| {} | {} |\n", first.join(" | "), rest.join(" | "))
}

/// [`COLUMNS`] over `rows`: score and views averaged, the rest summed.
fn numbers(rows: &[&Record]) -> Vec<String> {
    let sum = |f: fn(&Record) -> f64| rows.iter().map(|r| f(r)).sum::<f64>();
    let mean = |f: fn(&Record) -> f64| sum(f) / rows.len().max(1) as f64;
    let solved = rows.iter().filter(|r| r.ok).count();
    let unknown = rows.iter().any(|r| r.estimated);
    vec![
        format!("{solved}/{}", rows.len()),
        if solved == 0 { "-".into() } else { usd(unknown, sum(|r| r.cost) / solved as f64, 4) },
        if solved == 0 { "-".into() } else { format!("{:.1}", sum(|r| r.steps as f64) / solved as f64) },
        format!("{:.2}%", mean(|r| r.score) * 100.0),
        format!("{}", sum(|r| r.steps as f64)),
        format!("{}", sum(|r| r.requests as f64)),
        format!("{}", sum(|r| r.input as f64)),
        format!("{}", sum(|r| r.cache_read as f64)),
        format!("{}", sum(|r| r.cache_write as f64)),
        format!("{}", sum(|r| r.output as f64)),
        usd(rows.iter().any(|r| r.estimated), sum(|r| r.cost), 4),
        format!("{:.1}", sum(|r| r.wall_s)),
        format!("{}", sum(|r| r.tool_errors as f64)),
        format!("{:.0}", mean(|r| r.final_view_tokens as f64)),
        format!("{:.0}", mean(|r| r.max_view_tokens as f64)),
        format!("{}", sum(|r| r.compactions as f64)),
        format!("{}", sum(|r| r.rewrites as f64)),
        format!("{}", sum(|r| r.counters.recall_calls as f64)),
        format!("{}", sum(|r| r.counters.re_reads as f64)),
        format!("{}", sum(|r| r.counters.needless.len() as f64)),
        dup_percent(rows),
    ]
}

/// Wall time and tool time per arm, in seconds: where calls ran at once the time saved shows, not only the steps.
fn time_section(records: &[Record], arms: &[&str]) -> String {
    let mut s = String::from(
        "\n## Time\n\nTotals over runs, in seconds. Tool time: one call running alone, two or more at once, and the time \
         that overlap saved. Results from before this was recorded show `-`.\n\n\
         | arm | wall s | tool alone s | tool at once s | saved s |\n|---|---|---|---|---|\n",
    );
    for arm in arms {
        let rows: Vec<&Record> = records.iter().filter(|r| r.arm == *arm && !r.contaminated).collect();
        let ms = |f: fn(&Record) -> u64| rows.iter().map(|r| f(r)).sum::<u64>() as f64 / 1000.0;
        let wall = rows.iter().map(|r| if r.wall_ms > 0 { r.wall_ms as f64 / 1000.0 } else { r.wall_s }).sum::<f64>();
        let timed = rows.iter().any(|r| r.wall_ms > 0);
        let cell = |v: f64| if timed { format!("{v:.1}") } else { "-".into() };
        s.push_str(&format!(
            "| {arm} | {wall:.1} | {} | {} | {} |\n",
            cell(ms(|r| r.tool_serial_ms)),
            cell(ms(|r| r.tool_overlap_ms)),
            cell(ms(|r| r.tool_saved_ms)),
        ));
    }
    s
}

/// Duplicate lines over all lines tools returned, for `rows` together.
fn dup_percent(rows: &[&Record]) -> String {
    let c = rows.iter().fold(crate::counters::Counters::default(), |mut a, r| {
        a.dup_lines += r.counters.dup_lines;
        a.result_lines += r.counters.result_lines;
        a
    });
    format!("{:.1}", c.dup_percent())
}

/// Steps per run for each arm with their interval, and the runs that ended by the step cap or the
/// budget counted apart from the ones the agent finished. Runs the budget stopped are left out of
/// the steps.
fn steps_table(records: &[Record], arms: &[&str]) -> String {
    let mut s = format!(
        "\n## Steps per run\n\nMean over the runs not stopped by the budget, with the 95% bootstrap interval over runs \
         (the mark \"not enough runs\" under {MIN_TASKS} runs).\n\n\
         | arm | runs | finished | hit step cap | stopped by budget | steps per run |\n|---|---|---|---|---|---|\n"
    );
    for arm in arms {
        let mine: Vec<&Record> = records.iter().filter(|r| r.arm == *arm).collect();
        let whole: Vec<&Record> = mine.iter().copied().filter(|r| r.stop != Some(Stop::Budget)).collect();
        let steps: Vec<f64> = whole.iter().map(|r| r.steps as f64).collect();
        let d = Delta { mean: spread(&steps).0, ci: bootstrap(&steps, RESAMPLES, 0x6e65_786f) };
        let count = |stop: Stop| mine.iter().filter(|r| r.stop == Some(stop.clone())).count();
        let finished = mine.iter().filter(|r| r.finished).count();
        s.push_str(&format!(
            "| {arm} | {} | {finished} | {} | {} | {} |\n",
            mine.len(),
            count(Stop::MaxSteps),
            count(Stop::Budget),
            delta(d, 1.0, 1, whole.len() >= MIN_TASKS).replacen('+', "", 1)
        ));
    }
    s
}

/// `d` scaled by `k` with `places` decimals, then its interval, or the mark when `enough` is false.
fn delta(d: Delta, k: f64, places: usize, enough: bool) -> String {
    let n = |x: f64| format!("{:+.*}", places, x * k);
    match d.ci {
        _ if !enough => format!("{} (not enough runs)", n(d.mean)),
        Some((lo, hi)) => format!("{} [{}, {}]", n(d.mean), n(lo), n(hi)),
        None => n(d.mean),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::sample as record;

    fn head(repeats: u32) -> Header {
        Header {
            model: "deepseek-chat".into(),
            upstream: "https://api.deepseek.com/anthropic".into(),
            commit: "abc1234".into(),
            max_steps: 100,
            max_tokens: None,
            temperature: None,
            repeats,
            budget: Some(5.0),
            price: None,
            arm_order: None,
            skipped: vec![],
            stopped: None,
            spent: None,
            repro: None,
        }
    }

    #[test]
    fn the_closed_book_score_is_printed_beside_the_open_one() {
        let mut recs = vec![
            record("quixote-island", "plain", 1.0, 0.5),
            record("quixote-island-closed", "plain", 0.4, 0.1),
            record("quixote-windmills", "plain", 1.0, 0.5),
            record("quixote-windmills-closed", "plain", 1.0, 0.1),
            record("lonely-closed", "plain", 1.0, 0.1),
        ];
        for r in &mut recs {
            r.ok = r.score >= 1.0;
        }
        let s = summary(&head(1), &recs);
        assert!(s.contains("## Closed-book control"), "{s}");
        assert!(s.contains("| quixote-island | plain | 1.00 | 1/1 | 0.40 | 0/1 | clean |"), "{s}");
        assert!(s.contains("| quixote-windmills | plain | 1.00 | 1/1 | 1.00 | 1/1 | CONTAMINATED"), "{s}");
        assert!(!s.contains("| lonely |"), "a closed task without its open twin has no row: {s}");
        let none = summary(&head(1), &[record("a", "plain", 1.0, 0.5)]);
        assert!(!none.contains("Closed-book"), "{none}");
    }

    #[test]
    fn arms_get_a_totals_row_and_runs_a_row_each() {
        let mut recs =
            [record("a", "plain", 1.0, 0.5), record("b", "plain", 0.5, 0.25), record("a", "compact", 0.2075, 1.0)];
        recs[2].policy = "compact".into();
        recs[2].compact_at = Some(100_000);
        recs[2].compactions = 2;
        recs[2].rewrites = 2;
        let s = summary(&head(1), &recs);
        assert!(s.contains("| `deepseek-chat` | `abc1234` | 100 | default | default | 1 | $5 | **$1.7500** |"), "{s}");
        assert!(!s.contains("## Parts"), "no task has parts: {s}");
        assert!(s.contains("| plain | plain | 2 | 0 | 1/2 | 0.7500 | 20.0 | 75.00% | 20 | 22 | 2000 | 10000 | 0 | 600 | 0.7500 | 25.0 | 4 | 6000 | 6000 | 0 | 0 |"), "{s}");
        assert!(s.contains("| compact | compact at 100000 | 1 | 0 | 0/1 | - | - | 20.75% |"), "{s}");
        assert!(
            s.contains(
                "| a | compact | 0/1 | - | - | 20.75% | 10 | 11 | 1000 | 5000 | 0 | 300 | 1.0000 | 12.5 | 2 | 6000 | 6000 | 2 | 2 |"
            ),
            "{s}"
        );
        assert!(s.contains("## Paired with plain"), "{s}");
        assert!(!s.contains("runs that were skipped"), "{s}");
        let billed = summary(&Header { spent: Some(2.0), ..head(1) }, &recs);
        assert!(billed.contains("| $5 | **$2.0000** |") && billed.contains("$0.2500 went to runs that were skipped"));
        assert!(
            s.contains(
                "| compact | 1 | 1 | -79.2 (not enough runs) | +0.5000 (not enough runs) | +0.0 (not enough runs) |"
            ),
            "one task: no interval: {s}"
        );
    }

    #[test]
    fn counters_show_in_the_arms_runs_and_paired_tables() {
        let mut recs = vec![record("a", "plain", 1.0, 0.5), record("a", "cur", 1.0, 0.5)];
        recs[0].counters = crate::counters::Counters {
            recall_calls: 0,
            re_reads: 4,
            dup_lines: 20,
            result_lines: 100,
            ..Default::default()
        };
        recs[1].counters = crate::counters::Counters {
            recall_calls: 3,
            re_reads: 1,
            dup_lines: 5,
            result_lines: 100,
            ..Default::default()
        };
        let s = summary(&head(1), &recs);
        assert!(s.contains("| recall calls | re-reads | needless | dup lines % |"), "{s}");
        assert!(s.contains("| 0 | 4 | 0 | 20.0 |\n"), "{s}");
        assert!(s.contains("| 3 | 1 | 0 | 5.0 |\n"), "{s}");
        assert!(s.contains("| +3.0 (not enough runs) | -3.0 (not enough runs) | -15.0 (not enough runs) |"), "{s}");
    }

    #[test]
    fn two_tasks_get_the_not_enough_runs_mark_and_repeats_are_numbered() {
        let mut recs = vec![
            record("a", "plain", 1.0, 0.5),
            record("b", "plain", 0.0, 0.5),
            record("a", "curator", 1.0, 0.25),
            record("b", "curator", 0.5, 0.25),
        ];
        recs[3].rep = 1;
        recs[3].finished = false;
        let s = summary(&head(2), &recs);
        assert!(
            s.contains(
                "| curator | 2 | 1 | +25.0 (not enough runs) | -0.2500 (not enough runs) | +0.0 (not enough runs) |"
            ),
            "{s}"
        );
        assert!(s.contains("| b #1 (stopped) | curator |"), "{s}");
    }

    #[test]
    fn parts_get_a_matrix_rates_and_means_without_the_runs_the_budget_cut() {
        let part = |id: &str, ok: Option<bool>| crate::run::Part { id: id.into(), ok };
        let mut recs = vec![
            record("t", "plain", 0.5, 0.25),
            record("t", "plain", 1.0, 0.75),
            record("t", "cur", 0.5, 0.25),
            record("t", "cur", 0.0, 0.5),
        ];
        recs[0].parts = vec![part("p", Some(true)), part("q", Some(false))];
        recs[1].rep = 1;
        recs[1].parts = vec![part("p", Some(true)), part("q", Some(true))];
        recs[2].parts = vec![part("p", Some(true)), part("q", None)];
        recs[3].rep = 1;
        recs[3].stop = Some(Stop::Budget);
        recs[3].parts = vec![part("p", None), part("q", None)];
        let mut h = head(2);
        h.temperature = Some(0.0);
        let s = summary(&h, &recs);
        assert!(s.contains("| 100 | default | 0 | 2 |"), "{s}");
        assert!(s.contains("## Parts of t"), "{s}");
        assert!(s.contains("| arm | run | 1 | 2 | right | cost $ |\n|---|---|---|---|---|---|\n"), "{s}");
        assert!(s.contains("| plain | #0 | 1 | x | 1 | 0.2500 |"), "{s}");
        assert!(s.contains("| cur | #1 (budget) | · | · | 0 | 0.5000 |"), "{s}");
        assert!(s.contains("| plain | rate | 2/2 | 1/2 | | |"), "{s}");
        assert!(s.contains("| cur | rate | 1/1 | 0/1 | | |"), "{s}");
        assert!(s.contains("1 `p` · 2 `q`"), "{s}");
        assert!(s.contains("| arm | runs | right | cost $ | $ per right | Δ right vs plain |"), "{s}");
        assert!(s.contains("| plain | 2 | 1.50 ± 0.71 | 0.5000 ± 0.3536 | 0.3333 |  |"), "{s}");
        assert!(s.contains("| cur | 1 | 1.00 | 0.2500 | 0.2500 | -0.50 |"), "{s}");
    }

    fn grid(tasks: usize, reps: u32) -> Vec<Record> {
        let mut recs = Vec::new();
        for t in 0..tasks {
            for rep in 0..reps {
                for (arm, cost) in [("plain", 0.5), ("cur", 0.25 + t as f64 * 0.01)] {
                    let mut r = record(&format!("t{t}"), arm, 1.0, cost);
                    r.rep = rep;
                    recs.push(r);
                }
            }
        }
        recs
    }

    #[test]
    fn enough_tasks_and_repeats_show_the_interval() {
        let s = summary(&head(3), &grid(6, 3));
        assert!(!s.contains("(not enough runs)"), "{s}");
        assert!(s.contains("| cur | 6 | 3 | +0.0 [+0.0, +0.0] | -0.2250 ["), "{s}");
        let few_reps = summary(&head(2), &grid(6, 2));
        assert!(few_reps.contains("| cur | 6 | 2 | +0.0 (not enough runs) |"), "{few_reps}");
    }

    #[test]
    fn budget_stopped_runs_are_listed_apart_and_capped_runs_counted() {
        let mut recs = grid(6, 3);
        recs[1].stop = Some(Stop::Budget);
        recs[1].finished = false;
        recs[3].stop = Some(Stop::MaxSteps);
        recs[3].finished = false;
        let s = summary(&head(3), &recs);
        assert!(s.contains("Left out (stopped by the budget): t0 cur #0."), "{s}");
        assert!(s.contains("cur: 1 run(s) of cur and plain hit the step cap"), "{s}");
        assert!(s.contains("| cur | 18 | 16 | 1 | 1 | 10.0 ["), "{s}");
    }

    #[test]
    fn an_unpriced_model_shows_unknown_costs_and_cache_writes_get_a_column() {
        let mut recs = grid(1, 1);
        for r in &mut recs {
            r.estimated = true;
            r.cache_write = 777;
        }
        let s = summary(&head(1), &recs);
        assert!(s.contains("**unknown**"), "{s}");
        assert!(s.contains("Prices unknown"), "{s}");
        assert!(s.contains("| cache read | cache write | output |"), "{s}");
        assert!(s.contains("| 5000 | 777 | 300 | unknown |"), "{s}");
        assert!(!s.contains("0.5000"), "no guessed dollar figure: {s}");
    }

    #[test]
    fn prices_from_the_command_line_are_named_in_the_summary() {
        let mut h = head(1);
        h.price =
            Some(price_line(&nexo_core::Pricing { input: 0.14, output: 0.28, cache_read: 0.0028, cache_write: 0.0 }));
        let s = summary(&h, &grid(1, 1));
        assert!(s.contains("Prices given with --price: $0.14 input on a cache miss, $0.0028 on a cache hit, $0.28 output, per million tokens."), "{s}");
        assert!(!s.contains("Prices unknown"), "{s}");
    }

    #[test]
    fn mixed_settings_and_the_arm_order_are_said_in_the_header() {
        let mut recs = grid(2, 1);
        assert!(!summary(&head(1), &recs).contains("Mixed settings"));
        recs[1].commit = "def5678".into();
        recs[2].temperature = Some(0.7);
        let mut h = head(1);
        h.arm_order = Some("shuffled".into());
        let s = summary(&h, &recs);
        assert!(s.contains("**Mixed settings**: these results come from 3 different setups"), "{s}");
        assert!(s.contains("deepseek-chat @ def5678"), "{s}");
        assert!(s.contains("Arm order: shuffled."), "{s}");
    }

    #[test]
    fn wall_time_is_compared() {
        let mut recs = grid(1, 1);
        recs[1].wall_s = 30.0;
        let s = summary(&head(1), &recs);
        assert!(s.contains("Δ wall s |"), "{s}");
        assert!(s.contains("| +17.5 (not enough runs) |\n"), "{s}");
    }

    #[test]
    fn the_time_section_shows_wall_time_and_what_the_overlap_saved() {
        let mut recs = grid(1, 1);
        recs[0].wall_ms = 20_000;
        recs[0].tool_serial_ms = 4_000;
        recs[0].tool_overlap_ms = 2_000;
        recs[0].tool_saved_ms = 2_000;
        let s = summary(&head(1), &recs);
        assert!(s.contains("| arm | wall s | tool alone s | tool at once s | saved s |"), "{s}");
        assert!(s.contains("| 20.0 | 4.0 | 2.0 | 2.0 |"), "{s}");
    }

    #[test]
    fn contaminated_runs_are_left_out_of_means_and_deltas_and_listed() {
        let mut dirty = record("a", "cur", 1.0, 0.5);
        dirty.contaminated = true;
        dirty.outside = vec!["/home/x/inventory.rs".into()];
        let recs = [record("a", "plain", 0.0, 0.5), record("a", "cur", 0.5, 0.5), dirty];
        let s = summary(&head(1), &recs);
        assert!(s.contains("| cur | cur | 1 | 1 | 0/1 | - | - | 50.00% |"), "{s}");
        assert!(s.contains("Left out (contaminated): a cur #0."), "{s}");
        assert!(s.contains("| +50.0 "), "{s}");
        assert!(s.contains("## Contaminated runs") && s.contains("`/home/x/inventory.rs`"), "{s}");
        assert!(s.contains("a (contaminated)"), "{s}");
    }
}
