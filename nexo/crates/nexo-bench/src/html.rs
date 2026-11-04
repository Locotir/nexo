//! `report.html`: one self-contained page (inline CSS and SVG, no scripts) comparing the arms of
//! one or more `results.jsonl`, with conclusions written out in English. Short sentences, bold
//! numbers and tables, so it reads easily.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::run::Record;

/// The records of every `results.jsonl` in `paths` (a file, or a folder holding one).
pub fn load(paths: &[PathBuf]) -> Result<Vec<Record>, String> {
    let mut out = Vec::new();
    for p in paths {
        let file = if p.is_dir() { p.join("results.jsonl") } else { p.clone() };
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", file.display(), i + 1))?);
        }
    }
    Ok(out)
}

/// Writes `report.html` for `records` into `out`; its path.
pub fn write(out: &Path, records: &[Record]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let path = out.join("report.html");
    std::fs::write(&path, render(records))?;
    Ok(path)
}

/// The family of a task: `repoqa/x` is `repoqa`, nexo's own tasks are `nexo`.
pub fn family(task: &str) -> &str {
    task.split_once('/').map_or("nexo", |(f, _)| f)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The prompt tokens of a run: fresh input plus cache reads and writes.
pub fn prompt_tokens(r: &Record) -> u64 {
    r.input + r.cache_read + r.cache_write
}

/// What an arm did over some runs.
#[derive(Debug, Default, Clone)]
pub struct Totals {
    pub runs: usize,
    pub ok: usize,
    pub score: f64,
    pub steps: u64,
    pub cost: f64,
    pub input: u64,
    pub cache: u64,
    pub output: u64,
    pub wall_s: f64,
    pub tool_errors: u64,
}

impl Totals {
    fn add(&mut self, r: &Record) {
        self.runs += 1;
        self.ok += r.ok as usize;
        self.score += r.score;
        self.steps += r.steps as u64;
        self.cost += r.cost;
        self.input += r.input;
        self.cache += r.cache_read + r.cache_write;
        self.output += r.output;
        self.wall_s += r.wall_s;
        self.tool_errors += r.tool_errors as u64;
    }
    /// Mean score, 0..100.
    pub fn mean(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            100.0 * self.score / self.runs as f64
        }
    }
    /// Dollars per point of score (a point: 1% of one run); None without points.
    pub fn cost_per_point(&self) -> Option<f64> {
        (self.score > 0.0).then(|| self.cost / (self.score * 100.0))
    }
}

/// The arms in the order they first appear, with their policy.
fn arms(records: &[Record]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in records {
        if !out.iter().any(|(a, _)| a == &r.arm) {
            out.push((r.arm.clone(), r.policy.clone()));
        }
    }
    out
}

fn totals<'a>(records: impl Iterator<Item = &'a Record>) -> Totals {
    let mut t = Totals::default();
    records.for_each(|r| t.add(r));
    t
}

fn n(x: u64) -> String {
    let s = x.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A run the budget stopped: its numbers are too low to compare.
fn cut(r: &Record) -> bool {
    r.stop == Some(crate::client::Stop::Budget)
}

/// Prompt-token savings of arm `a` against arm `b` over the tasks both ran, each task's mean over
/// its whole runs (runs the budget cut are left out): (percent saved, tokens of a, tokens of b, tasks).
pub fn savings(records: &[Record], a: &str, b: &str) -> Option<(f64, u64, u64, usize)> {
    let of = |arm: &str| -> BTreeMap<&str, u64> {
        let mut sums: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for r in records.iter().filter(|r| r.arm == arm && !cut(r)) {
            let e = sums.entry(r.task.as_str()).or_default();
            *e = (e.0 + prompt_tokens(r), e.1 + 1);
        }
        sums.into_iter().map(|(t, (sum, k))| (t, (sum as f64 / k as f64).round() as u64)).collect()
    };
    let (ta, tb) = (of(a), of(b));
    let common: Vec<&&str> = ta.keys().filter(|k| tb.contains_key(*k)).collect();
    let sa: u64 = common.iter().map(|k| ta[*k]).sum();
    let sb: u64 = common.iter().map(|k| tb[*k]).sum();
    (sb > 0).then(|| (100.0 * (1.0 - sa as f64 / sb as f64), sa, sb, common.len()))
}

/// The conclusions, in English, one short sentence per item.
pub fn conclusions(records: &[Record]) -> Vec<String> {
    let mut out = Vec::new();
    let arms = arms(records);
    if arms.is_empty() {
        out.push("No runs to compare.".into());
        return out;
    }
    let unknown = records.iter().any(|r| r.estimated);
    let all: Vec<(String, String, Totals)> = arms
        .iter()
        .map(|(a, p)| (a.clone(), p.clone(), totals(records.iter().filter(|r| &r.arm == a && !cut(r)))))
        .collect();
    let best = all
        .iter()
        .max_by(|x, y| x.2.mean().total_cmp(&y.2.mean()).then(y.2.cost.total_cmp(&x.2.cost)))
        .expect("an arm");
    out.push(format!(
        "The best arm is <b>{}</b>{}: mean score <b>{:.1}%</b> over <b>{}</b> runs.",
        esc(&best.0),
        if best.1.is_empty() { String::new() } else { format!(" ({})", esc(&best.1)) },
        best.2.mean(),
        best.2.runs
    ));
    for (a, _, t) in all.iter().filter(|_| !unknown) {
        match t.cost_per_point() {
            Some(c) => out.push(format!(
                "<b>{}</b> costs <b>${:.5}</b> per point (total cost <b>${:.4}</b>).",
                esc(a),
                c,
                t.cost
            )),
            None => out.push(format!("<b>{}</b> scored no points (total cost <b>${:.4}</b>).", esc(a), t.cost)),
        }
    }
    let by_policy = |p: &str| arms.iter().find(|(_, q)| q == p).map(|(a, _)| a.clone());
    if let Some(cur) = by_policy("curator") {
        for other in ["plain", "compact"] {
            let Some(o) = by_policy(other) else { continue };
            match savings(records, &cur, &o) {
                Some((pct, sa, sb, k)) if pct >= 0.0 => out.push(format!(
                    "The curator (<b>{}</b>) saves <b>{:.1}%</b> input tokens against <b>{}</b>: <b>{}</b> versus <b>{}</b>, the mean run of <b>{k}</b> common tasks.",
                    esc(&cur), pct, esc(&o), n(sa), n(sb)
                )),
                Some((pct, sa, sb, k)) => out.push(format!(
                    "The curator (<b>{}</b>) uses <b>{:.1}%</b> more input tokens than <b>{}</b>: <b>{}</b> versus <b>{}</b>, the mean run of <b>{k}</b> common tasks.",
                    esc(&cur), -pct, esc(&o), n(sa), n(sb)
                )),
                None => out.push(format!("No common runs between <b>{}</b> and <b>{}</b>.", esc(&cur), esc(&o))),
            }
        }
    }
    let k = records.iter().filter(|r| cut(r)).count();
    if k > 0 {
        out.push(format!(
            "<b>{k}</b> run{} cut by the budget {} left out of these conclusions.",
            if k == 1 { "" } else { "s" },
            if k == 1 { "is" } else { "are" }
        ));
    }
    if unknown {
        out.push("Note: costs are unknown (no prices for this model; <code>--price</code> gives them).".into());
    }
    out
}

const PALETTE: [&str; 6] = ["#2f6db5", "#d1762b", "#3a9a5b", "#8a5cc2", "#c2455a", "#5f6b7a"];

/// A horizontal bar chart: one labelled bar per (label, value), the value written at its end.
fn bars(title: &str, rows: &[(String, f64, String)]) -> String {
    let max = rows.iter().map(|r| r.1).fold(0.0_f64, f64::max).max(1e-12);
    let (lw, bw, h) = (110.0, 300.0, 26.0);
    let height = 30.0 + h * rows.len() as f64;
    let mut s = format!(
        "<figure><svg role=\"img\" aria-label=\"{t}\" viewBox=\"0 0 {w} {height}\" width=\"100%\" style=\"max-width:{w}px\"><text x=\"0\" y=\"16\" class=\"t\">{t}</text>",
        t = esc(title),
        w = lw + bw + 90.0
    );
    for (i, (label, v, shown)) in rows.iter().enumerate() {
        let y = 26.0 + h * i as f64;
        let len = (bw * v / max).max(1.0);
        let _ = write!(
            s,
            "<text x=\"{x}\" y=\"{ty}\" text-anchor=\"end\">{l}</text><rect x=\"{lw}\" y=\"{y}\" width=\"{len:.1}\" height=\"{bh}\" rx=\"3\" fill=\"{c}\"/><text x=\"{vx:.1}\" y=\"{ty}\" class=\"v\">{shown}</text>",
            x = lw - 8.0,
            ty = y + 14.0,
            l = esc(label),
            bh = h - 8.0,
            c = PALETTE[i % PALETTE.len()],
            vx = lw + len + 6.0,
            shown = esc(shown),
        );
    }
    s.push_str("</svg></figure>");
    s
}

const STYLE: &str = r#"
:root{--bg:#fbfaf7;--fg:#1d1f22;--muted:#5d6670;--line:#d9d6cf;--head:#efece5}
@media (prefers-color-scheme: dark){:root{--bg:#17191c;--fg:#e9e7e2;--muted:#a3abb4;--line:#3a3f45;--head:#23272b}}
body{background:var(--bg);color:var(--fg);font:17px/1.6 Verdana,"Atkinson Hyperlegible",Arial,sans-serif;margin:0 auto;max-width:1100px;padding:16px;letter-spacing:.01em}
h1,h2,h3{line-height:1.25}
p.m{color:var(--muted)}
ul.c li{margin:.4em 0}
.scroll{overflow-x:auto}
table{border-collapse:collapse;margin:.5em 0 1.5em;font-variant-numeric:tabular-nums}
th,td{border:1px solid var(--line);padding:.35em .6em;text-align:right;white-space:nowrap}
th{background:var(--head)}
td:first-child,th:first-child{text-align:left}
tr:nth-child(even) td{background:color-mix(in srgb,var(--head) 45%,transparent)}
figure{margin:.5em 0 1em}
svg text{fill:var(--fg);font-size:13px}
svg text.t{font-weight:bold;font-size:14px}
svg text.v{font-weight:bold}
.charts{display:flex;flex-wrap:wrap;gap:12px}
.charts figure{flex:1 1 320px}
"#;

/// Whether `r` is a run of `arm` on `task`, repeat `rep`.
fn same(r: &Record, arm: &str, task: &str, rep: u32) -> bool {
    r.arm == arm && r.task == task && r.rep == rep
}

/// The page for `records`.
pub fn render(records: &[Record]) -> String {
    let arms = arms(records);
    let mut families: Vec<&str> = Vec::new();
    for r in records {
        let f = family(&r.task);
        if !families.contains(&f) {
            families.push(f);
        }
    }
    let mut s = String::new();
    s.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>nexo bench</title><style>");
    s.push_str(STYLE);
    s.push_str("</style></head><body><h1>nexo bench: report</h1>");
    // no prices known: every cost is 0, so none is shown
    let unknown = records.iter().any(|r| r.estimated);
    let usd = |x: f64, places: usize| if unknown { "unknown".to_string() } else { format!("{x:.places$}") };
    let spent: f64 = records.iter().map(|r| r.cost).sum();
    let _ = write!(
        s,
        "<p class=\"m\"><b>{}</b> runs. <b>{}</b> arms. <b>{}</b> families. Total cost <b>{}</b>.</p>",
        records.len(),
        arms.len(),
        families.len(),
        if unknown { "unknown".to_string() } else { format!("${spent:.4}") }
    );

    s.push_str("<h2>Conclusions</h2><ul class=\"c\">");
    for c in conclusions(records) {
        let _ = write!(s, "<li>{c}</li>");
    }
    s.push_str("</ul>");

    s.push_str("<h2>Arms</h2>");
    for fam in std::iter::once(None).chain(families.iter().map(Some)) {
        let rows: Vec<&Record> = records
            .iter()
            .filter(|r| match fam {
                None => true,
                Some(f) => *f == family(&r.task),
            })
            .collect();
        let _ = write!(s, "<h3>{}</h3>", fam.map_or("All tasks".to_string(), |f| format!("Family {}", esc(f))));
        s.push_str("<div class=\"scroll\"><table><tr><th>arm</th><th>policy</th><th>runs</th><th>score</th><th>all passed</th><th>steps</th><th>cost $</th><th>$ / point</th><th>input</th><th>cache</th><th>output</th><th>time s</th><th>tool errors</th></tr>");
        let mut chart_score = Vec::new();
        let mut chart_tokens = Vec::new();
        let mut chart_cost = Vec::new();
        for (a, p) in &arms {
            let t = totals(rows.iter().copied().filter(|r| &r.arm == a));
            if t.runs == 0 {
                continue;
            }
            let _ = write!(
                s,
                "<tr><td><b>{}</b></td><td>{}</td><td>{}</td><td><b>{:.1}%</b></td><td>{}</td><td>{}</td><td><b>{}</b></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0}</td><td>{}</td></tr>",
                esc(a),
                esc(p),
                t.runs,
                t.mean(),
                t.ok,
                t.steps,
                usd(t.cost, 4),
                t.cost_per_point().map_or("-".into(), |c| usd(c, 5)),
                n(t.input),
                n(t.cache),
                n(t.output),
                t.wall_s,
                t.tool_errors
            );
            chart_score.push((a.clone(), t.mean(), format!("{:.1}%", t.mean())));
            let tok = (t.input + t.cache + t.output) as f64 / t.runs as f64;
            chart_tokens.push((a.clone(), tok, n(tok.round() as u64)));
            chart_cost.push((a.clone(), t.cost, format!("${:.4}", t.cost)));
        }
        s.push_str("</table></div>");
        if fam.is_some() {
            s.push_str("<div class=\"charts\">");
            s.push_str(&bars("Mean score", &chart_score));
            s.push_str(&bars("Tokens per run", &chart_tokens));
            if !unknown {
                s.push_str(&bars("Total cost", &chart_cost));
            }
            s.push_str("</div>");
        }
    }

    s.push_str("<h2>Per instance</h2><p class=\"m\">Score, cost and input tokens of each run.</p><div class=\"scroll\"><table><tr><th>task</th><th>rep.</th>");
    for (a, _) in &arms {
        let _ = write!(s, "<th>{0} score</th><th>{0} $</th><th>{0} tokens</th>", esc(a));
    }
    s.push_str("</tr>");
    // merged files may hold several runs of one (task, arm, repeat): the k-th of each arm share a row
    let nth = |r: &Record| {
        records.iter().take_while(|x| !std::ptr::eq(*x, r)).filter(|x| same(x, &r.arm, &r.task, r.rep)).count()
    };
    let mut keys: Vec<(String, u32, usize)> = Vec::new();
    for r in records {
        let key = (r.task.clone(), r.rep, nth(r));
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    for (task, rep, k) in &keys {
        let run = if *k == 0 { String::new() } else { format!(" (run {})", k + 1) };
        let _ = write!(s, "<tr><td>{}</td><td>{rep}{run}</td>", esc(task));
        for (a, _) in &arms {
            match records.iter().filter(|r| same(r, a, task, *rep)).nth(*k) {
                Some(r) => {
                    let _ = write!(
                        s,
                        "<td><b>{:.0}%</b>{}</td><td>{}</td><td>{}</td>",
                        r.score * 100.0,
                        if r.finished { "" } else { " (stopped)" },
                        usd(r.cost, 4),
                        n(prompt_tokens(r))
                    );
                }
                None => s.push_str("<td>-</td><td>-</td><td>-</td>"),
            }
        }
        s.push_str("</tr>");
    }
    s.push_str("</table></div></body></html>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(task: &str, arm: &str, policy: &str, score: f64, cost: f64, input: u64) -> Record {
        Record {
            task: task.into(),
            arm: arm.into(),
            policy: policy.into(),
            spec: String::new(),
            rep: 0,
            score,
            ok: score == 1.0,
            detail: String::new(),
            finished: true,
            stop: None,
            steps: 3,
            requests: 3,
            input,
            cache_read: 0,
            cache_write: 0,
            output: 10,
            cost,
            estimated: false,
            tool_calls: 2,
            tool_errors: 1,
            repeated_calls: 0,
            wall_s: 1.5,
            errors: vec![],
            transcript: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn the_report_states_the_key_numbers() {
        let records = vec![
            rec("repoqa/a", "plain", "plain", 0.5, 0.02, 10_000),
            rec("repoqa/a", "compact", "compact", 0.5, 0.015, 8_000),
            rec("repoqa/a", "curator", "curator", 1.0, 0.01, 4_000),
            rec("canonstep/asm", "curator", "curator", 0.25, 0.01, 2_000),
        ];
        let dir = tempfile::tempdir().unwrap();
        let mut lines = String::new();
        for r in &records {
            lines.push_str(&serde_json::to_string(r).unwrap());
            lines.push('\n');
        }
        std::fs::write(dir.path().join("results.jsonl"), lines).unwrap();
        let loaded = load(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(loaded.len(), 4);
        let path = write(dir.path(), &loaded).unwrap();
        let html = std::fs::read_to_string(path).unwrap();
        assert!(!html.contains("<script"));
        assert!(html.contains("The best arm is <b>curator</b>"), "{html}");
        assert!(html.contains("<b>62.5%</b>"), "curator's mean over its two runs");
        // curator vs plain on the one common run: 4,000 against 10,000 tokens, 60% saved
        assert!(html.contains("saves <b>60.0%</b>") && html.contains("<b>4,000</b> versus <b>10,000</b>"));
        assert!(html.contains("saves <b>50.0%</b>"), "against compact");
        // $0.02 for 125 points
        assert!(html.contains("<b>$0.00016</b> per point"), "{html}");
        assert!(html.contains("Family repoqa") && html.contains("Family canonstep") && html.contains("<svg"));
    }

    #[test]
    fn unknown_prices_show_no_dollar_figures() {
        let mut records = vec![
            rec("repoqa/a", "plain", "plain", 0.5, 0.0, 10_000),
            rec("repoqa/a", "curator", "curator", 1.0, 0.0, 4_000),
        ];
        records.iter_mut().for_each(|r| r.estimated = true);
        let html = render(&records);
        assert!(!html.contains("$0.0") && !html.contains("0.0000<") && !html.contains("per point"), "{html}");
        assert!(!html.contains("Total cost</text>") && html.contains("costs are unknown"), "{html}");
        assert!(html.contains("Total cost <b>unknown</b>"), "{html}");
    }

    #[test]
    fn runs_cut_by_the_budget_stay_out_of_the_conclusions() {
        let mut cut = rec("repoqa/b", "curator", "curator", 0.0, 0.001, 500);
        cut.stop = Some(crate::client::Stop::Budget);
        let records = vec![
            rec("repoqa/a", "plain", "plain", 0.5, 0.02, 10_000),
            rec("repoqa/a", "curator", "curator", 0.5, 0.01, 4_000),
            rec("repoqa/b", "plain", "plain", 0.5, 0.02, 10_000),
            cut,
        ];
        let c = conclusions(&records).join("\n");
        assert!(c.contains("saves <b>60.0%</b>"), "{c}");
        assert!(c.contains("mean score <b>50.0%</b> over <b>"), "{c}");
        assert!(c.contains("<b>1</b> run cut by the budget"), "{c}");
    }

    #[test]
    fn merged_runs_of_the_same_task_and_repeat_are_all_counted() {
        let records = vec![
            rec("repoqa/a", "plain", "plain", 0.5, 0.02, 10_000),
            rec("repoqa/a", "curator", "curator", 0.5, 0.01, 4_000),
            rec("repoqa/a", "plain", "plain", 0.5, 0.02, 20_000),
            rec("repoqa/a", "curator", "curator", 0.5, 0.01, 16_000),
        ];
        // means: 10,000 against 15,000
        let (pct, sa, sb, _) = savings(&records, "curator", "plain").unwrap();
        assert_eq!((sa, sb), (10_000, 15_000));
        assert!((pct - 33.333).abs() < 0.01);
        let html = render(&records);
        assert!(html.contains("16,000") && html.contains("20,000"), "{html}");
    }

    #[test]
    fn families_and_thousands() {
        assert_eq!(family("repoqa/x"), "repoqa");
        assert_eq!(family("fix-bug"), "nexo");
        assert_eq!(n(1_234_567), "1,234,567");
        assert_eq!(n(12), "12");
    }
}
