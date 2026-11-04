//! What the agent did in a run, read from its events: steps, tool errors and calls repeated to no
//! effect.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use nexo_core::{AgentEvent, TurnEnd};

use crate::client::Stop;
use serde_json::Value;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct LoopStats {
    /// Replies of the model (each one a step of the loop).
    pub steps: u32,
    pub tool_calls: u32,
    /// Tool results flagged as errors.
    pub tool_errors: u32,
    /// Calls with the same tool and arguments as an earlier one that got the same result back:
    /// the model asked again for something it already had.
    pub repeated_calls: u32,
    /// User turns that ended.
    pub turns: u32,
    /// A turn ended because it reached the step limit.
    pub hit_max_steps: bool,
    /// A turn ended on an error (the provider failed, or the bench refused the request).
    pub errors: Vec<String>,
    pending: HashMap<String, String>,
    seen: HashMap<String, u64>,
}

impl LoopStats {
    pub fn observe(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::AssistantDone { .. } => self.steps += 1,
            AgentEvent::ToolCall { id, name, args, .. } => {
                self.tool_calls += 1;
                self.pending.insert(id.clone(), format!("{name}|{}", canonical(args)));
            }
            AgentEvent::ToolDone { id, output, .. } => {
                if output.is_error {
                    self.tool_errors += 1;
                }
                if let Some(key) = self.pending.remove(id) {
                    let h = hash(&output.content);
                    if self.seen.insert(key, h) == Some(h) {
                        self.repeated_calls += 1;
                    }
                }
            }
            AgentEvent::TurnEnd { reason } => {
                self.turns += 1;
                if matches!(reason, TurnEnd::MaxTurns) {
                    self.hit_max_steps = true;
                }
            }
            AgentEvent::Notice { level: nexo_core::Level::Error, text } => self.errors.push(text.clone()),
            _ => {}
        }
    }
}

/// `v` as JSON with object keys sorted, so the same arguments in another order are the same call.
pub fn canonical(v: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&m[k]))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            _ => v.clone(),
        }
    }
    sorted(v).to_string()
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Resamples the paired comparison draws (fixed seed: the same results give the same interval).
pub const RESAMPLES: usize = 10_000;

/// One arm against the baseline arm on the tasks both ran: the mean per-task difference (arm minus
/// baseline, each task's repeats averaged first) and its 95% bootstrap interval over tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub arm: String,
    pub baseline: String,
    pub tasks: usize,
    pub score: Delta,
    pub cost: Delta,
    pub steps: Delta,
    pub recalls: Delta,
    pub re_reads: Delta,
    /// Duplicate lines, in points of percentage.
    pub dup: Delta,
    /// Wall seconds.
    pub wall: Delta,
    /// Fewest runs any compared task has in either arm (the repeats behind each mean).
    pub reps_min: usize,
    /// Runs stopped by the benchmark budget: left out of every mean above, listed apart.
    pub excluded: Vec<String>,
    /// Runs of the two arms that hit the step cap: kept (a real result), counted apart.
    pub capped: usize,
    /// Runs of the two arms that reached outside their folders: left out of every mean above.
    pub contaminated: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Delta {
    pub mean: f64,
    /// The 2.5th and 97.5th percentiles of the resampled means; none under two tasks.
    pub ci: Option<(f64, f64)>,
}

impl Delta {
    pub fn of(diffs: &[f64]) -> Delta {
        let mean = if diffs.is_empty() { 0.0 } else { diffs.iter().sum::<f64>() / diffs.len() as f64 };
        Delta { mean, ci: bootstrap(diffs, RESAMPLES, 0x6e65_786f) }
    }
}

/// The mean of `xs` and their sample standard deviation (none under two values).
pub fn spread(xs: &[f64]) -> (f64, Option<f64>) {
    let n = xs.len() as f64;
    let mean = if xs.is_empty() { 0.0 } else { xs.iter().sum::<f64>() / n };
    let sd = (xs.len() >= 2).then(|| (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt());
    (mean, sd)
}

/// The 95% percentile interval of the mean of `xs` over `n` resamples with replacement; none when
/// there are fewer than two values.
pub fn bootstrap(xs: &[f64], n: usize, seed: u64) -> Option<(f64, f64)> {
    if xs.len() < 2 || n == 0 {
        return None;
    }
    let mut rng = crate::rng::Rng::new(seed);
    let mut means: Vec<f64> =
        (0..n).map(|_| (0..xs.len()).map(|_| xs[rng.below(xs.len())]).sum::<f64>() / xs.len() as f64).collect();
    means.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f64| means[((q * (n - 1) as f64).round() as usize).min(n - 1)];
    Some((at(0.025), at(0.975)))
}

/// `arm` against `baseline`, paired by task; none when they share no task. Runs the budget stopped
/// do not enter (they are cut short, so their steps and cost are too low); they are listed in
/// `excluded`.
pub fn paired(records: &[crate::run::Record], baseline: &str, arm: &str) -> Option<Paired> {
    let kept: Vec<&crate::run::Record> =
        records.iter().filter(|r| r.stop != Some(Stop::Budget) && !r.contaminated).collect();
    let mine = |r: &&crate::run::Record| r.arm == arm || r.arm == baseline;
    let excluded: Vec<String> = records
        .iter()
        .filter(|r| r.stop == Some(Stop::Budget) && mine(r))
        .map(|r| format!("{} {} #{}", r.task, r.arm, r.rep))
        .collect();
    let contaminated: Vec<String> = records
        .iter()
        .filter(|r| r.contaminated && mine(r))
        .map(|r| format!("{} {} #{}", r.task, r.arm, r.rep))
        .collect();
    let capped = kept.iter().filter(|r| r.stop == Some(Stop::MaxSteps) && mine(r)).count();
    let count = |a: &str, task: &str| kept.iter().filter(|r| r.arm == a && r.task == task).count();
    let mean = |a: &str, task: &str, f: fn(&crate::run::Record) -> f64| {
        let v: Vec<f64> = kept.iter().filter(|r| r.arm == a && r.task == task).map(|r| f(r)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    let mut tasks: Vec<&str> = Vec::new();
    for r in &kept {
        if !tasks.contains(&r.task.as_str()) {
            tasks.push(&r.task);
        }
    }
    let diffs = |f: fn(&crate::run::Record) -> f64| -> Vec<f64> {
        tasks.iter().filter_map(|t| Some(mean(arm, t, f)? - mean(baseline, t, f)?)).collect()
    };
    let score = diffs(|r| r.score);
    if score.is_empty() {
        return None;
    }
    let reps_min = tasks
        .iter()
        .filter(|t| count(arm, t) > 0 && count(baseline, t) > 0)
        .map(|t| count(arm, t).min(count(baseline, t)))
        .min()
        .unwrap_or(0);
    Some(Paired {
        arm: arm.into(),
        baseline: baseline.into(),
        tasks: score.len(),
        score: Delta::of(&score),
        cost: Delta::of(&diffs(|r| r.cost)),
        steps: Delta::of(&diffs(|r| r.steps as f64)),
        recalls: Delta::of(&diffs(|r| r.counters.recall_calls as f64)),
        re_reads: Delta::of(&diffs(|r| r.counters.re_reads as f64)),
        dup: Delta::of(&diffs(|r| r.counters.dup_percent())),
        wall: Delta::of(&diffs(|r| r.wall_s)),
        reps_min,
        excluded,
        capped,
        contaminated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::ToolOutput;
    use serde_json::json;

    fn call(s: &mut LoopStats, id: &str, args: Value, out: ToolOutput) {
        s.observe(&AgentEvent::ToolCall { id: id.into(), name: "read".into(), args, summary: String::new() });
        s.observe(&AgentEvent::ToolDone { id: id.into(), output: out, elapsed_ms: 1 });
    }

    #[test]
    fn the_same_call_with_the_same_result_counts_as_repeated() {
        let mut s = LoopStats::default();
        call(&mut s, "1", json!({"path": "a", "limit": 5}), ToolOutput::ok("x"));
        call(&mut s, "2", json!({"limit": 5, "path": "a"}), ToolOutput::ok("x"));
        call(&mut s, "3", json!({"path": "a", "limit": 5}), ToolOutput::ok("x"));
        assert_eq!((s.tool_calls, s.repeated_calls), (3, 2));
    }

    #[test]
    fn a_call_whose_result_changed_is_not_repeated() {
        let mut s = LoopStats::default();
        call(&mut s, "1", json!({"path": "a"}), ToolOutput::ok("before"));
        call(&mut s, "2", json!({"path": "a"}), ToolOutput::ok("after"));
        call(&mut s, "3", json!({"path": "b"}), ToolOutput::ok("after"));
        assert_eq!(s.repeated_calls, 0);
    }

    #[test]
    fn errors_steps_and_the_step_limit_are_counted() {
        let mut s = LoopStats::default();
        call(&mut s, "1", json!({"path": "nope"}), ToolOutput::error("no such file"));
        call(&mut s, "2", json!({"path": "ok"}), ToolOutput::ok("fine"));
        for _ in 0..3 {
            s.observe(&AgentEvent::AssistantDone { message: nexo_core::Message::assistant("hi") });
        }
        s.observe(&AgentEvent::TurnEnd { reason: TurnEnd::MaxTurns });
        assert_eq!((s.tool_errors, s.steps, s.turns, s.hit_max_steps), (1, 3, 1, true));
    }

    #[test]
    fn the_bootstrap_brackets_the_mean_and_repeats_itself() {
        assert_eq!(bootstrap(&[1.0], 100, 1), None, "one task: no interval");
        let xs = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let (lo, hi) = bootstrap(&xs, RESAMPLES, 7).unwrap();
        assert!(lo < 0.35 && 0.35 < hi && lo >= 0.1 && hi <= 0.6, "{lo} {hi}");
        assert_eq!(bootstrap(&xs, RESAMPLES, 7), Some((lo, hi)));
        assert_eq!(bootstrap(&[2.0; 5], 100, 3), Some((2.0, 2.0)));
    }

    #[test]
    fn arms_are_paired_by_task_with_repeats_averaged() {
        use crate::run::sample as record;
        let mut recs = vec![
            record("a", "plain", 1.0, 0.5),
            record("a", "curator", 0.5, 0.25),
            record("a", "curator", 1.0, 0.25),
            record("b", "plain", 0.0, 1.0),
            record("b", "curator", 0.5, 0.5),
            record("c", "curator", 1.0, 0.1),
        ];
        recs[1].steps = 4;
        let p = paired(&recs, "plain", "curator").unwrap();
        assert_eq!(p.tasks, 2, "c has no plain run");
        assert!((p.score.mean - 0.125).abs() < 1e-12, "{p:?}");
        assert!((p.cost.mean + 0.375).abs() < 1e-12, "{p:?}");
        assert!((p.steps.mean + 1.5).abs() < 1e-12, "{p:?}");
        let (lo, hi) = p.score.ci.unwrap();
        assert!(lo <= p.score.mean && p.score.mean <= hi);
        assert!(paired(&recs[..1], "plain", "curator").is_none());
        assert_eq!(paired(&recs[..3], "plain", "curator").unwrap().score.ci, None, "one task");
    }

    #[test]
    fn budget_stopped_runs_are_left_out_and_listed_and_step_capped_runs_stay_counted() {
        use crate::run::sample as record;
        let mut recs = vec![
            record("a", "plain", 1.0, 0.5),
            record("a", "cur", 1.0, 0.25),
            record("a", "cur", 0.0, 0.01),
            record("a", "cur", 1.0, 0.75),
        ];
        recs[2].rep = 1;
        recs[2].stop = Some(Stop::Budget);
        recs[3].rep = 2;
        recs[3].stop = Some(Stop::MaxSteps);
        recs[3].wall_s = 22.5;
        let p = paired(&recs, "plain", "cur").unwrap();
        assert!((p.cost.mean - 0.0).abs() < 1e-12, "mean of 0.25 and 0.75 minus 0.5: {p:?}");
        assert!((p.score.mean).abs() < 1e-12, "the cut run's 0 score is not averaged in: {p:?}");
        assert_eq!(p.excluded, vec!["a cur #1".to_string()]);
        assert_eq!((p.capped, p.reps_min), (1, 1));
        assert!((p.wall.mean - 5.0).abs() < 1e-12, "wall 17.5 mean against 12.5: {p:?}");
        let only_cut = [record("a", "plain", 1.0, 0.5), {
            let mut r = record("a", "cur", 0.0, 0.0);
            r.stop = Some(Stop::Budget);
            r
        }];
        assert!(paired(&only_cut, "plain", "cur").is_none(), "no whole run left to compare");
    }
}
