//! The per-family report (`SIMULATOR-SPEC.md` 7.4): for every family the runs and events behind it, the
//! fitted `r`, `beta`, `phi`, `pi` with their intervals, the price-model error, the section 5.2 outcome, the
//! Pareto front of the family's fixtures and whether the family is validated; and the family weights.

use std::collections::BTreeMap;

use super::behave::ParamSet;
use super::family::FAMILIES;
use super::fit::{check, family_of_task, fit_dirs, load_rows, price_check, ArmKind, FitConfig, GroupFit, Holdout};
use super::synth::refuse;

/// The measured cost shares of our own runs (682 runs, 16.20 $), the default family weights of J.
pub const WEIGHTS: [(&str, f64); 6] =
    [("asm", 0.53), ("code", 0.20), ("logs", 0.15), ("binary", 0.05), ("chat", 0.00), ("http", 0.00)];

/// Where the weights come from.
pub const WEIGHTS_SOURCE: &str = "682 runs, 16.20 $, 2026-11-08";

/// The text of `weights.toml`.
pub fn weights_toml() -> String {
    let mut s = String::new();
    s += "# Family weights of the objective J (SIMULATOR-SPEC.md 4.2): the measured cost share of our own runs.\n";
    s += &format!("# Source: {WEIGHTS_SOURCE}. A fixture's weight is its family's weight.\n");
    s += "# A CLI flag (--weights FILE) swaps this file; the report always prints the unweighted table too.\n";
    s += &format!("source = \"{WEIGHTS_SOURCE}\"\n[weights]\n");
    for (f, w) in WEIGHTS {
        s += &format!("{f} = {w:.2}\n");
    }
    s
}

/// What the report reads.
pub struct ReportInput {
    pub runs: Vec<String>,
    /// A `results.jsonl` for the section 5.2 check, with the params and spec it runs with.
    pub results: Option<String>,
    pub params: Option<String>,
    pub spec: String,
    /// A `pareto.tsv` of `sim search`.
    pub pareto: Option<String>,
    /// The model the rows are fitted for.
    pub model: String,
    pub min_events: usize,
}

impl Default for ReportInput {
    fn default() -> ReportInput {
        ReportInput {
            runs: Vec::new(),
            results: None,
            params: None,
            spec: "directives=0;nudge=0".to_string(),
            pareto: None,
            model: "deepseek-flash".to_string(),
            min_events: 20,
        }
    }
}

/// One line of the table: a family on one model.
#[derive(Debug, Clone)]
pub struct Line {
    pub family: String,
    pub model: String,
    pub fit: Option<GroupFit>,
    pub price_err: Option<f64>,
    pub outcome: String,
    pub pareto: String,
    pub status: String,
}

pub struct Report {
    pub lines: Vec<Line>,
    pub fitted_runs: usize,
    pub weights: BTreeMap<String, f64>,
}

/// The non-dominated rows (minimising all three) of `(cost, steps, p_fail)`.
fn front(rows: &[(f64, f64, f64)]) -> usize {
    (0..rows.len())
        .filter(|&i| {
            !(0..rows.len()).any(|j| {
                let (a, b) = (rows[i], rows[j]);
                j != i && b.0 <= a.0 && b.1 <= a.1 && b.2 <= a.2 && (b.0 < a.0 || b.1 < a.1 || b.2 < a.2)
            })
        })
        .count()
}

/// The Pareto front of a family from a `pareto.tsv`: the columns `cost_<f>`, `extra_steps_<f>`, `p_fail_<f>`
/// when the file has them; else only the size of the whole front can be said.
fn pareto_text(text: Option<&str>, family: &str) -> String {
    let Some(text) = text else { return "-".to_string() };
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().unwrap_or("").split('\t').collect();
    let col = |n: &str| head.iter().position(|h| *h == n);
    let body: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
    match (col(&format!("cost_{family}")), col(&format!("extra_steps_{family}")), col(&format!("p_fail_{family}"))) {
        (Some(c), Some(s), Some(p)) => {
            let rows: Vec<(f64, f64, f64)> = body
                .iter()
                .filter_map(|r| Some((r.get(c)?.parse().ok()?, r.get(s)?.parse().ok()?, r.get(p)?.parse().ok()?)))
                .collect();
            format!("{} of {} configs", front(&rows), rows.len())
        }
        _ => format!("all fixtures only ({} rows)", body.len()),
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

/// Builds the report. Synthetic files are refused anywhere in the inputs.
pub fn build(inp: &ReportInput) -> Result<Report, String> {
    for p in inp.runs.iter().chain(inp.results.iter()).chain(inp.pareto.iter()) {
        refuse(p)?;
    }
    let cfg = FitConfig { model: Some(inp.model.clone()), min_events: inp.min_events, ..FitConfig::default() };
    let fit = fit_dirs(&inp.runs, &cfg)?;
    let rows = load_rows(&inp.runs)?;
    for r in &rows {
        if let Some(t) = &r.transcript {
            refuse(&t.to_string_lossy())?;
        }
    }
    let of_path: BTreeMap<String, String> = rows
        .iter()
        .filter_map(|r| {
            Some((r.transcript.as_ref()?.to_string_lossy().to_string(), family_of_task(&r.task)?.to_string()))
        })
        .collect();
    let mut price: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for p in price_check(&rows, 0.0, 0, None) {
        if let Some(f) = of_path.get(&p.path) {
            price.entry(f.clone()).or_default().push(p.cost_err());
        }
    }
    let checked = match (&inp.results, &inp.params) {
        (Some(r), Some(p)) => {
            let set = ParamSet::load(p)?;
            Some(check(r, &inp.spec, &set, Holdout::Even, 0.0, 0)?)
        }
        _ => None,
    };
    let pareto_src = inp.pareto.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let mut keys: Vec<(String, String)> = Vec::new();
    for f in FAMILIES {
        keys.push((f.to_string(), inp.model.clone()));
        for (g, m) in fit.groups.keys() {
            if g == f && *m != inp.model {
                keys.push((g.clone(), m.clone()));
            }
        }
    }
    let mut lines = Vec::new();
    for (family, model) in keys {
        let g = fit.groups.get(&(family.clone(), model.clone())).cloned();
        let events = g.as_ref().map_or(0, |g| g.events);
        let outcome = match &checked {
            None => "no check".to_string(),
            Some(c) => {
                let ts: Vec<_> = c.tasks.iter().filter(|t| t.family == family).collect();
                if ts.is_empty() {
                    "no pairs".to_string()
                } else {
                    format!(
                        "steps {}/{}, cost {}/{}",
                        ts.iter().filter(|t| t.ratio_ok).count(),
                        ts.len(),
                        ts.iter().filter(|t| t.cost_ok).count(),
                        ts.len()
                    )
                }
            }
        };
        let failed = checked.as_ref().is_some_and(|c| {
            let ts: Vec<_> = c.tasks.iter().filter(|t| t.family == family).collect();
            !ts.is_empty() && ts.iter().any(|t| !t.ratio_ok || !t.cost_ok)
        });
        let status = if events < inp.min_events {
            format!("unvalidated (n events = {events})")
        } else if failed {
            "unvalidated (check failed)".to_string()
        } else {
            "validated".to_string()
        };
        lines.push(Line {
            price_err: price.get(&family).cloned().and_then(median),
            pareto: pareto_text(pareto_src.as_deref(), &family),
            fit: g,
            family,
            model,
            outcome,
            status,
        });
    }
    let fitted_runs = rows.iter().filter(|r| r.kind == ArmKind::Curator).count();
    Ok(Report { lines, fitted_runs, weights: WEIGHTS.iter().map(|(f, w)| (f.to_string(), *w)).collect() })
}

impl Report {
    pub fn table(&self) -> String {
        let mut s = format!(
            "per-family report ({} curator runs); * = rule of three; weights {}: {}\n",
            self.fitted_runs,
            WEIGHTS_SOURCE,
            WEIGHTS.iter().map(|(f, w)| format!("{f} {w:.2}")).collect::<Vec<_>>().join(", ")
        );
        s += &format!(
            "{:<7} {:<14} {:>4} {:>6}  {:<24} {:<24} {:<24} {:<22} {:>7} {:<22} {:<24} {}\n",
            "family", "model", "runs", "events", "r", "beta", "phi", "pi", "price", "check 5.2", "pareto", "status"
        );
        for l in &self.lines {
            let (runs, events, r, b, p, pi) = match &l.fit {
                Some(g) => (g.runs, g.events, g.r.show(), g.beta.show(), g.phi.show(), g.pi.show()),
                None => (0, 0, "n/a".into(), "n/a".into(), "n/a".into(), "n/a".into()),
            };
            let price = l.price_err.map_or("-".to_string(), |e| format!("{:.1} %", 100.0 * e));
            s += &format!(
                "{:<7} {:<14} {:>4} {:>6}  {:<24} {:<24} {:<24} {:<22} {:>7} {:<22} {:<24} {}\n",
                l.family, l.model, runs, events, r, b, p, pi, price, l.outcome, l.pareto, l.status
            );
        }
        s
    }

    pub fn markdown(&self, date: &str) -> String {
        let mut s = format!("# Per-family report, {date}\n\n");
        s += &format!("Weights ({WEIGHTS_SOURCE}): ");
        s += &WEIGHTS.iter().map(|(f, w)| format!("{f} {w:.2}")).collect::<Vec<_>>().join(", ");
        s += "\n\n| family | model | runs | events | r | beta | phi | pi | price err | check 5.2 | pareto | status |\n";
        s += "|---|---|---|---|---|---|---|---|---|---|---|---|\n";
        for l in &self.lines {
            let (runs, events, r, b, p, pi) = match &l.fit {
                Some(g) => (g.runs, g.events, g.r.show(), g.beta.show(), g.phi.show(), g.pi.show()),
                None => (0, 0, "n/a".into(), "n/a".into(), "n/a".into(), "n/a".into()),
            };
            let price = l.price_err.map_or("-".to_string(), |e| format!("{:.1} %", 100.0 * e));
            s += &format!(
                "| {} | {} | {runs} | {events} | {r} | {b} | {p} | {pi} | {price} | {} | {} | {} |\n",
                l.family, l.model, l.outcome, l.pareto, l.status
            );
        }
        s
    }
}
