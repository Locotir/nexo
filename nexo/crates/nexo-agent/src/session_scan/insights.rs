//! Ranking a report for a period into the rows the usage view shows.

use super::model::*;
use super::scan::Report;
use std::collections::BTreeMap;
use std::sync::Arc;

/// What a row says.
#[derive(Debug, Clone, PartialEq)]
pub enum InsightKind {
    /// Calls over [`LONG_CTX`], and the sessions they were in.
    LongContext { sessions: usize, calls: u64 },
    /// Prompts re-sent whole, and the expected rebuilds that are not counted.
    CacheMisses { misses: u64, tokens: u64, rebuilds: u64 },
    /// One tool's results, and the command that weighed most.
    ToolOutput { tool: String, top: Option<String>, runs: u64 },
    /// One model's part of the spend.
    /// `dearer`: its calls cost more, one with another, than the rest's (else a cheaper model is no advice).
    Model { model: String, cost: f64, dearer: bool },
    /// One folder's part of the spend.
    Project { cwd: String, cost: f64 },
    /// Reasoning in what the model wrote.
    Reasoning { tokens: u64 },
    /// Sub-agents and other calls on the side (needs sessions written with the owner of each call).
    SideCalls { cost: f64 },
    /// History the curator kept out of the calls (needs the same).
    CuratorLeftOut { tokens: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    /// 0..=1 of the usage the row applies to; rows overlap.
    pub share: f64,
    pub kind: InsightKind,
}

impl Insight {
    pub fn percent(&self) -> u64 {
        (self.share * 100.0).round() as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    /// The folder the session ran in (its last name).
    pub project: String,
    /// When it was last used in the period: `today`, `yesterday` or `Nov 2`.
    pub when: String,
    pub cost: f64,
    pub cache_hit: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Insights {
    pub rows: Vec<Insight>,
    pub top: Vec<SessionRow>,
    pub totals: Tally,
    /// Sessions with a call in the period.
    pub sessions: usize,
}

/// The last name of a folder (the session's own name is not shown: it comes from what was typed).
pub fn folder_name(cwd: &str) -> String {
    let name = cwd.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() {
        "session".to_string()
    } else {
        name.to_string()
    }
}

/// `today`, `yesterday`, or the date (`Nov 2`) of a day number.
pub fn day_label(day: i64, today: i64) -> String {
    match today - day {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        _ => match time::Date::from_julian_day((day + 2_440_588) as i32) {
            Ok(d) => format!("{} {}", &d.month().to_string()[..3], d.day()),
            Err(_) => String::new(),
        },
    }
}

/// The figures of `period` ending today.
pub fn rank(report: &Report, period: Period) -> Insights {
    let from = report.today - period.days() + 1;
    let mut totals = Tally::default();
    let mut tools: BTreeMap<String, ToolTally> = BTreeMap::new();
    let mut models: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    let mut projects: BTreeMap<String, f64> = BTreeMap::new();
    let mut per_session: Vec<(Arc<Digest>, Tally, i64)> = Vec::new();
    let mut long_sessions = 0;
    for d in &report.sessions {
        let mut t = Tally::default();
        let mut last = from;
        for (n, day) in d.days.range(from..=report.today) {
            last = last.max(*n);
            t.add(&day.tally);
            for (name, tt) in &day.tools {
                tools.entry(name.clone()).or_default().add(tt);
            }
            for (name, m) in &day.models {
                let e = models.entry(name.clone()).or_default();
                e.0 += m.calls;
                e.1 += m.cost;
            }
        }
        if t.calls == 0 && t.total_input() == 0 && t.output == 0 {
            continue;
        }
        if t.long_calls > 0 {
            long_sessions += 1;
        }
        *projects.entry(d.cwd.clone()).or_default() += t.cost;
        totals.add(&t);
        per_session.push((d.clone(), t, last));
    }
    let sessions = per_session.len();
    // the shares are over the calls that were read: a rewritten file's one summed line is in no row
    let input = totals.total_input().saturating_sub(totals.summed_input) as f64;
    let mut rows = Vec::new();
    let mut push = |share: f64, kind: InsightKind| {
        if share.is_finite() && share >= MIN_SHARE {
            rows.push(Insight { share: share.min(1.0), kind });
        }
    };
    if input > 0.0 {
        push(
            totals.long_input as f64 / input,
            InsightKind::LongContext { sessions: long_sessions, calls: totals.long_calls },
        );
        push(
            totals.resent as f64 / input,
            InsightKind::CacheMisses { misses: totals.misses, tokens: totals.resent, rebuilds: totals.rebuilds },
        );
        let mut by_weight: Vec<(&String, &ToolTally)> = tools.iter().filter(|(_, t)| t.weighted > 0).collect();
        by_weight.sort_by(|a, b| b.1.weighted.cmp(&a.1.weighted).then_with(|| a.0.cmp(b.0)));
        for (name, t) in by_weight.into_iter().take(2) {
            let top = t.top.iter().max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0))).map(|(k, _)| k.clone());
            push(t.weighted as f64 / input, InsightKind::ToolOutput { tool: name.clone(), top, runs: t.runs });
        }
    }
    if totals.cost > 0.0 {
        let priced = (totals.cost - totals.summed_cost).max(0.0);
        if priced > 0.0 && models.values().filter(|m| m.1 > 0.0).count() >= 2 {
            if let Some((m, (calls, c))) =
                models.iter().max_by(|a, b| a.1 .1.total_cmp(&b.1 .1).then_with(|| b.0.cmp(a.0)))
            {
                if c / priced >= MODEL_SHARE {
                    let (rest_calls, rest_cost) =
                        models.iter().filter(|(k, _)| *k != m).fold((0u64, 0.0f64), |a, (_, v)| (a.0 + v.0, a.1 + v.1));
                    let dearer = *calls > 0 && rest_calls > 0 && c / *calls as f64 > rest_cost / rest_calls as f64;
                    push(c / priced, InsightKind::Model { model: m.clone(), cost: *c, dearer });
                }
            }
        }
        if projects.values().filter(|c| **c > 0.0).count() >= 2 {
            if let Some((p, c)) = projects.iter().max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0))) {
                push(c / totals.cost, InsightKind::Project { cwd: p.clone(), cost: *c });
            }
        }
    }
    if totals.output > 0 {
        push(totals.reasoning as f64 / totals.output as f64, InsightKind::Reasoning { tokens: totals.reasoning });
    }
    if totals.kinded_cost > 0.0 {
        push(totals.side_cost / totals.kinded_cost, InsightKind::SideCalls { cost: totals.side_cost });
    }
    if totals.held > 0 {
        let base = (totals.held + totals.held_base) as f64;
        push(totals.held as f64 / base, InsightKind::CuratorLeftOut { tokens: totals.held });
    }
    // biggest first; equal shares keep the order above
    rows.sort_by(|a, b| b.share.total_cmp(&a.share));
    rows.truncate(MAX_ROWS);
    per_session.sort_by(|a, b| {
        b.1.cost
            .total_cmp(&a.1.cost)
            .then_with(|| b.1.total_input().cmp(&a.1.total_input()))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    let top = per_session
        .iter()
        .take(3)
        .map(|(d, t, last)| SessionRow {
            id: d.id.clone(),
            project: folder_name(&d.cwd),
            when: day_label(*last, report.today),
            cost: t.cost,
            cache_hit: t.cache_hit(),
        })
        .collect();
    Insights { rows, top, totals, sessions }
}
