//! Fitting the behaviour parameters from our own bench runs, and checking the simulator against held-out
//! pairs (`SIMULATOR-SPEC.md` 3 and 5.2).
//!
//! A run directory holds `results.jsonl` (one row per run: task, arm, rep, commit, spec, steps, cost, score,
//! stop, usage, counters) and one transcript per run. The transcript of a curator arm already holds the
//! curator's marks, so the curator is not run on it again: the induced re-fetches are read off its marks.
//! The events come from the PLAIN twin of the pair (same task, rep, commit), fed through the curator with
//! the curator run's own spec, which is exactly what `predict` does later.
//!
//! * `r`: unique cut results fetched back (a `recall`, or a call identical to an earlier one whose result
//!   was out of view when it was made) minus the in-view repeat rate of the family times the exposure,
//!   over the opportunities: the plain twin's `cited` and `denied` events.
//! * `beta`: `(steps_cur - steps_plain - fetches) / unfetched events`, pooled over the pairs, clipped at 0.
//! * `phi`: share of pairs with an unfetched event whose score fell, above the same share among pairs with
//!   no event, turned from a per-run into a per-event chance.
//! * `pi`: recall calls over fetches, in runs that had the tool.
//!
//! Intervals are percentile bootstraps over runs (2000 resamples); a count of zero gets the rule of three.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::behave::{analytic, auto_family, Context, ParamSet};
use super::events::{run, Kind, Tool};
use super::input::{load, reply_to, Request};
use super::price::{tokens_of, Cache, Prices};

/// What the older runs were costed with: `deepseek-flash` at the chat prices.
pub const PRICES_OLD: Prices = Prices { miss: 0.28, hit: 0.028, out: 0.42 };

/// The spec a pair is run with when its curator row carries none.
pub const DEFAULT_SPEC: &str = "directives=0;nudge=0";

/// Resamples of every bootstrap.
pub const BOOT: usize = 2000;

// ---- rows ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArmKind {
    Plain,
    Curator,
    Other,
}

/// One row of a `results.jsonl`.
#[derive(Debug, Clone)]
pub struct Row {
    /// Directory of the `results.jsonl` the row came from.
    pub dir: PathBuf,
    pub task: String,
    pub arm: String,
    pub kind: ArmKind,
    pub rep: i64,
    pub commit: String,
    pub model: String,
    pub spec: String,
    pub steps: usize,
    pub cost: f64,
    pub score: f64,
    pub stop: Option<String>,
    pub max_steps: usize,
    pub input: u64,
    pub cache_read: u64,
    pub output: u64,
    pub recall_calls: u64,
    pub rewrites: u64,
    pub transcript: Option<PathBuf>,
}

impl Row {
    fn parse(dir: &Path, v: &Value) -> Option<Row> {
        let arm = v["arm"].as_str()?.to_string();
        let policy = v["policy"].as_str().unwrap_or("");
        let spec = v["spec"].as_str().unwrap_or("").to_string();
        let kind = if arm == "plain" {
            ArmKind::Plain
        } else if (arm == "curator" || arm == "cur" || policy == "curator") && !spec.contains("at=") {
            ArmKind::Curator
        } else {
            ArmKind::Other
        };
        let transcript = v["transcript"].as_str().map(|t| dir.join(t)).filter(|p| p.is_file());
        Some(Row {
            dir: dir.to_path_buf(),
            task: v["task"].as_str()?.to_string(),
            arm,
            kind,
            rep: v["rep"].as_i64().unwrap_or(0),
            commit: v["commit"].as_str().unwrap_or("").chars().take(12).collect(),
            model: v["model"].as_str().unwrap_or("").to_string(),
            spec,
            steps: v["steps"].as_u64()? as usize,
            cost: v["cost"].as_f64().unwrap_or(0.0),
            score: v["score"].as_f64().unwrap_or(0.0),
            stop: v["stop"].as_str().map(str::to_string),
            max_steps: v["max_steps"].as_u64().unwrap_or(40) as usize,
            input: v["input"].as_u64().unwrap_or(0),
            cache_read: v["cache_read"].as_u64().unwrap_or(0),
            output: v["output"].as_u64().unwrap_or(0),
            recall_calls: v["counters"]["recall_calls"].as_u64().unwrap_or(0),
            rewrites: v["rewrites"].as_u64().unwrap_or(0),
            transcript,
        })
    }

    /// Both arms ended on their own: no step cap, no budget stop.
    pub fn clean(&self) -> bool {
        self.stop.is_none()
    }
}

fn find_results(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let f = dir.join("results.jsonl");
    if f.is_file() {
        out.push(f);
    }
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut subs: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    subs.sort();
    for s in subs {
        find_results(&s, depth - 1, out);
    }
}

/// The rows of every `results.jsonl` under the directories (or the files named), in path order.
pub fn load_rows(dirs: &[String]) -> Result<Vec<Row>, String> {
    let mut files = Vec::new();
    for d in dirs {
        let p = Path::new(d);
        if !p.exists() {
            return Err(format!("{d}: no such run directory"));
        }
        find_results(p, 3, &mut files);
    }
    files.sort();
    files.dedup();
    let mut rows = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let dir = f.parent().unwrap_or(Path::new("."));
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(r) = serde_json::from_str::<Value>(line).ok().and_then(|v| Row::parse(dir, &v)) {
                rows.push(r);
            }
        }
    }
    Ok(rows)
}

/// The family of a task by its name (the truth set of the tagger, `SIMULATOR-SPEC.md` 7.2), if the name says.
pub fn family_of_task(task: &str) -> Option<&'static str> {
    let t = task.to_ascii_lowercase();
    if t.starts_with("dbg") || t.contains("realm") {
        Some("binary")
    } else if t.contains("asm") {
        Some("asm")
    } else if t.contains("http") {
        Some("http")
    } else if t.ends_with("-chat") || t == "chat" {
        Some("chat")
    } else if t.contains("marathon") || t.contains("long-haul") || t.contains("noisy") {
        Some("logs")
    } else if t.contains("vuln-hunt")
        || t.contains("revert")
        || t.contains("three-sessions")
        || t.contains("stale-doc")
        || t.contains("polyglot")
    {
        Some("code")
    } else {
        None
    }
}

// ---- pairs ----

/// A curator run and its plain twin (same task, model, rep and commit), either of which may be missing.
#[derive(Debug, Clone)]
pub struct Unit {
    pub plain: Option<usize>,
    pub cur: Option<usize>,
    /// The rep as recorded, and the one parity is taken from (see `pair_up`).
    pub rep: i64,
    pub eff_rep: i64,
}

/// Pairs the plain and curator rows by (task, model, rep, commit); inside a group the pair from the same
/// directory goes first. Every run directory of the older benches counts its reps from 0, so when a task's
/// reps collide the parity is taken from the position of the pair among its task's pairs (directory order)
/// instead of the recorded rep; with unique reps it is the recorded rep.
pub fn pair_up(rows: &[Row]) -> Vec<Unit> {
    type Group = (Vec<usize>, Vec<usize>);
    let mut groups: BTreeMap<(String, String, i64, String), Group> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        let key = (r.task.clone(), r.model.clone(), r.rep, r.commit.clone());
        match r.kind {
            ArmKind::Plain => groups.entry(key).or_default().0.push(i),
            ArmKind::Curator => groups.entry(key).or_default().1.push(i),
            ArmKind::Other => {}
        }
    }
    let mut units = Vec::new();
    for ((_, _, rep, _), (mut plains, mut curs)) in groups {
        let mut out = Vec::new();
        for &c in &curs {
            if let Some(k) = plains.iter().position(|&p| rows[p].dir == rows[c].dir) {
                out.push((Some(plains.remove(k)), Some(c)));
            }
        }
        curs.retain(|c| !out.iter().any(|(_, x)| *x == Some(*c)));
        for c in curs {
            let p = (!plains.is_empty()).then(|| plains.remove(0));
            out.push((p, Some(c)));
        }
        for p in plains {
            out.push((Some(p), None));
        }
        for (plain, cur) in out {
            units.push(Unit { plain, cur, rep, eff_rep: rep });
        }
    }
    // effective rep: the recorded reps when each task's are unique; else, since the older run directories
    // each count their reps from 0, the ordinal of the unit in (task, model, directory, rep) order, which
    // splits the whole set into two balanced halves
    let mut by_task: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (k, u) in units.iter().enumerate() {
        let r = &rows[u.cur.or(u.plain).expect("a unit holds a row")];
        by_task.entry((r.task.clone(), r.model.clone())).or_default().push(k);
    }
    let collide = by_task.values().any(|ks| ks.iter().map(|&k| units[k].rep).collect::<BTreeSet<_>>().len() < ks.len());
    if collide {
        let mut all: Vec<usize> = (0..units.len()).collect();
        all.sort_by_key(|&k| {
            let r = &rows[units[k].cur.or(units[k].plain).unwrap()];
            (r.task.clone(), r.model.clone(), r.dir.clone(), units[k].rep)
        });
        for (n, k) in all.into_iter().enumerate() {
            units[k].eff_rep = n as i64;
        }
    }
    units
}

/// Which reps stay out of the fit (they are the ones to check on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holdout {
    None,
    Odd,
    Even,
}

impl Holdout {
    pub fn parse(s: &str) -> Option<Holdout> {
        match s {
            "odd" => Some(Holdout::Odd),
            "even" => Some(Holdout::Even),
            "none" | "all" => Some(Holdout::None),
            _ => None,
        }
    }

    /// Whether a unit with this rep is held out.
    pub fn holds(self, eff_rep: i64) -> bool {
        match self {
            Holdout::None => false,
            Holdout::Odd => eff_rep.rem_euclid(2) == 1,
            Holdout::Even => eff_rep.rem_euclid(2) == 0,
        }
    }
}

// ---- transcripts: induced re-fetches ----

/// What a transcript says about fetching cut results back.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub requests: usize,
    /// Cut results fetched back (a key once, however often it was asked for).
    pub fetched: BTreeSet<String>,
    pub recall_calls: usize,
    /// Prior calls (deduplicated by key) with their result out of view, summed over the requests.
    pub expo_out: u64,
    /// The same for results still in view.
    pub expo_in: u64,
    /// Calls identical to an earlier one whose result was still in view.
    pub repeats_in: u64,
    /// Whether the host offered `recall`.
    pub has_recall: bool,
}

fn is_edit(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "str_replace" | "patch" | "apply_patch" | "multi_edit" | "multiedit" | "create"
    )
}

/// The identity of a call: tool and arguments, a read by its path alone.
fn call_key(name: &str, input: &Value) -> String {
    if Tool::of(name) == Tool::Read {
        for k in ["path", "file_path", "file", "filename"] {
            if let Some(p) = input[k].as_str() {
                return format!("read:{p}");
            }
        }
    }
    format!("{}:{}", name.to_ascii_lowercase(), input)
}

/// Whether a result's text is a curator mark that lost content (not the lossless "a later output below").
pub fn lost_mark(text: &str) -> bool {
    text.contains("[curator]")
        && !text.contains("a later output below")
        && (text.contains("left out here") || text.contains("lines omitted") || text.contains("is shortened above"))
}

fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

fn calls_in(m: &Value) -> Vec<(String, String, Value)> {
    let Some(bs) = m["content"].as_array() else { return Vec::new() };
    bs.iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            (
                b["id"].as_str().unwrap_or("").to_string(),
                b["name"].as_str().unwrap_or("").to_string(),
                b["input"].clone(),
            )
        })
        .collect()
}

/// Reads the induced re-fetches off a transcript (`c2-q-por-modelo.md`: a `recall` call, or a call
/// identical to an earlier one whose result was out of view at that request).
pub fn analyse(requests: &[Request]) -> Analysis {
    let mut a = Analysis { requests: requests.len(), ..Default::default() };
    a.has_recall = requests.iter().any(|q| q.tools.as_ref().is_some_and(|t| t.iter().any(|n| n == "recall")));
    for (i, q) in requests.iter().enumerate() {
        let Some(reply) = reply_to(requests, i) else { continue };
        // prior calls in this request: key -> result lost?
        let mut open: BTreeMap<String, String> = BTreeMap::new(); // id -> key
        let mut prior: BTreeMap<String, bool> = BTreeMap::new();
        for m in &q.messages {
            for (id, name, input) in calls_in(m) {
                if !is_edit(&name) && name != "recall" {
                    open.insert(id, call_key(&name, &input));
                }
            }
            if let Some(bs) = m["content"].as_array() {
                for b in bs.iter().filter(|b| b["type"] == "tool_result") {
                    if let Some(key) = b["tool_use_id"].as_str().and_then(|id| open.get(id)) {
                        prior.insert(key.clone(), lost_mark(&result_text(b)));
                    }
                }
            }
        }
        a.expo_out += prior.values().filter(|&&lost| lost).count() as u64;
        a.expo_in += prior.values().filter(|&&lost| !lost).count() as u64;
        for (_, name, input) in calls_in(reply) {
            if name == "recall" {
                a.recall_calls += 1;
                a.fetched.insert(format!("recall:{input}"));
            } else if is_edit(&name) {
                continue;
            } else {
                let key = call_key(&name, &input);
                match prior.get(&key) {
                    Some(true) => {
                        a.fetched.insert(key);
                    }
                    Some(false) => a.repeats_in += 1,
                    None => {}
                }
            }
        }
    }
    a
}

// ---- statistics ----

/// A small deterministic generator for the bootstrap.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

/// A point estimate with its 95 % interval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Est {
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    /// The sample size the estimate stands on (events, pairs, fetches; see each parameter).
    pub n: f64,
    /// The interval is the rule of three (a count of zero), not a bootstrap.
    pub rule3: bool,
}

impl Est {
    pub const NONE: Est = Est { value: 0.0, lo: 0.0, hi: 1.0, n: 0.0, rule3: false };

    pub fn json(&self) -> Value {
        json!([round(self.lo), round(self.hi)])
    }

    pub fn show(&self) -> String {
        if self.n == 0.0 {
            return "n/a".to_string();
        }
        format!("{:.3} [{:.3}, {:.3}]{}", self.value, self.lo, self.hi, if self.rule3 { "*" } else { "" })
    }
}

fn round(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Percentile interval of `stat` over `BOOT` resamples of `units` (resamples whose statistic is undefined
/// are skipped).
fn percentile_ci<T: Copy>(units: &[T], seed: u64, stat: impl Fn(&[T]) -> Option<f64>) -> (f64, f64) {
    let mut rng = Rng::new(seed);
    let mut vals = Vec::with_capacity(BOOT);
    let mut pick: Vec<T> = Vec::with_capacity(units.len());
    for _ in 0..BOOT {
        pick.clear();
        for _ in 0..units.len() {
            pick.push(units[rng.below(units.len())]);
        }
        if let Some(v) = stat(&pick) {
            vals.push(v);
        }
    }
    if vals.is_empty() {
        return (0.0, 0.0);
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| vals[((vals.len() - 1) as f64 * p).round() as usize];
    (at(0.025), at(0.975))
}

/// One curator run seen against its plain twin.
#[derive(Debug, Clone)]
pub struct Obs {
    pub family: String,
    pub model: String,
    pub task: String,
    pub dir: String,
    pub rep: i64,
    pub eff_rep: i64,
    /// Events on the plain twin with this run's spec: `cited` + `denied`, and `blind` for the record.
    pub events: Option<usize>,
    pub blind: usize,
    pub fetched: usize,
    pub recall_calls: usize,
    pub expo_out: u64,
    pub has_recall: bool,
    pub steps_cur: usize,
    pub steps_plain: Option<usize>,
    pub score_cur: f64,
    pub score_plain: Option<f64>,
    /// Both arms ended on their own.
    pub clean_pair: bool,
    /// The curator run hit a step cap.
    pub capped: bool,
    pub rewrites: u64,
}

impl Obs {
    fn unfetched(&self) -> f64 {
        (self.events.unwrap_or(0) as f64 - self.fetched as f64).max(0.0)
    }
}

/// The in-view repeat rate of a set of analyses: repeats per prior call per request.
fn baseline(an: &[&Analysis]) -> f64 {
    let (rep, expo): (u64, u64) = an.iter().fold((0, 0), |(r, e), a| (r + a.repeats_in, e + a.expo_in));
    if expo == 0 {
        0.0
    } else {
        rep as f64 / expo as f64
    }
}

/// The fitted behaviour of one group of runs.
#[derive(Debug, Clone)]
pub struct GroupFit {
    pub runs: usize,
    pub pairs: usize,
    pub events: usize,
    pub fetched: usize,
    pub unpaired: usize,
    pub baseline: f64,
    pub r: Est,
    pub beta: Est,
    pub phi: Est,
    pub pi: Est,
}

/// Fits one group: the curator runs `obs` (those with a twin carry events) and the baseline `h`.
pub fn fit_group(obs: &[&Obs], h: f64, seed: u64) -> GroupFit {
    let with_events: Vec<&Obs> = obs.iter().copied().filter(|o| o.events.is_some()).collect();
    let pairs: Vec<&Obs> = with_events.iter().copied().filter(|o| o.steps_plain.is_some()).collect();
    let events: usize = with_events.iter().map(|o| o.events.unwrap()).sum();
    let fetched: usize = with_events.iter().map(|o| o.fetched).sum();

    // r
    let r_stat = |u: &[&Obs]| -> Option<f64> {
        let e: f64 = u.iter().map(|o| o.events.unwrap() as f64).sum();
        let f: f64 = u.iter().map(|o| o.fetched as f64).sum();
        let x: f64 = u.iter().map(|o| o.expo_out as f64).sum();
        (e > 0.0).then(|| ((f - h * x) / e).clamp(0.0, 1.0))
    };
    let all: &[&Obs] = &with_events;
    let r = if events == 0 {
        Est::NONE
    } else {
        let value = r_stat(all).unwrap_or(0.0);
        if fetched == 0 {
            Est { value: 0.0, lo: 0.0, hi: (3.0 / events as f64).min(1.0), n: events as f64, rule3: true }
        } else {
            let (lo, hi) = percentile_ci(&with_events, seed, r_stat);
            Est { value, lo: lo.min(value), hi: hi.max(value), n: events as f64, rule3: false }
        }
    };

    // beta over the pairs that have an unfetched event
    let bp: Vec<&Obs> = pairs.iter().copied().filter(|o| o.unfetched() > 0.0).collect();
    let b_stat = |u: &[&Obs]| -> Option<f64> {
        let d: f64 = u.iter().map(|o| o.steps_cur as f64 - o.steps_plain.unwrap() as f64 - o.fetched as f64).sum();
        let n: f64 = u.iter().map(|o| o.unfetched()).sum();
        (n > 0.0).then(|| (d / n).max(0.0))
    };
    let beta = if bp.is_empty() {
        Est::NONE
    } else {
        let ball: &[&Obs] = &bp;
        let value = b_stat(ball).unwrap_or(0.0);
        let (lo, hi) = percentile_ci(&bp, seed ^ 0xB, b_stat);
        Est { value, lo: lo.min(value), hi: hi.max(value), n: bp.iter().map(|o| o.unfetched()).sum(), rule3: false }
    };

    // phi: score drops among clean pairs
    let cp: Vec<&Obs> = pairs.iter().copied().filter(|o| o.clean_pair).collect();
    let dropped = |o: &Obs| f64::from(o.score_cur + 1e-9 < o.score_plain.unwrap_or(0.0));
    let p_stat = |u: &[&Obs]| -> Option<f64> {
        let a: Vec<&&Obs> = u.iter().filter(|o| o.unfetched() > 0.0).collect();
        if a.is_empty() {
            return None;
        }
        let b: Vec<&&Obs> = u.iter().filter(|o| o.events == Some(0)).collect();
        let sa = a.iter().map(|o| dropped(o)).sum::<f64>() / a.len() as f64;
        let sb = if b.is_empty() { 0.0 } else { b.iter().map(|o| dropped(o)).sum::<f64>() / b.len() as f64 };
        let k = a.iter().map(|o| o.unfetched()).sum::<f64>() / a.len() as f64;
        let x = (sa - sb).clamp(0.0, 1.0);
        Some(1.0 - (1.0 - x).powf(1.0 / k.max(1.0)))
    };
    let at_risk: f64 = cp.iter().map(|o| o.unfetched()).sum();
    let phi = if at_risk == 0.0 {
        Est::NONE
    } else {
        let call: &[&Obs] = &cp;
        let value = p_stat(call).unwrap_or(0.0);
        let drops: f64 = cp.iter().filter(|o| o.unfetched() > 0.0).map(|o| dropped(o)).sum();
        if drops == 0.0 {
            Est { value: 0.0, lo: 0.0, hi: (3.0 / at_risk).min(1.0), n: at_risk, rule3: true }
        } else {
            let (lo, hi) = percentile_ci(&cp, seed ^ 0xF, p_stat);
            Est { value, lo: lo.min(value), hi: hi.max(value), n: at_risk, rule3: false }
        }
    };

    // pi: recall calls over fetches, in runs that had the tool
    let rr: Vec<&Obs> = with_events.iter().copied().filter(|o| o.has_recall).collect();
    let rf: f64 = rr.iter().map(|o| o.fetched as f64).sum();
    let pi = if rf == 0.0 {
        Est::NONE
    } else {
        let s = |u: &[&Obs]| -> Option<f64> {
            let f: f64 = u.iter().map(|o| o.fetched as f64).sum();
            let c: f64 = u.iter().map(|o| o.recall_calls as f64).sum();
            (f > 0.0).then(|| (c / f).clamp(0.0, 1.0))
        };
        let value = s(&rr).unwrap_or(0.0);
        let (lo, hi) = percentile_ci(&rr, seed ^ 0xA, s);
        Est { value, lo: lo.min(value), hi: hi.max(value), n: rf, rule3: false }
    };

    // fewer than 10 events say nothing about the cost of a miss or the chance of failure
    let (beta, phi) = if events < 10 { (Est::NONE, Est::NONE) } else { (beta, phi) };
    GroupFit {
        runs: obs.len(),
        pairs: pairs.len(),
        events,
        fetched,
        unpaired: obs.len() - with_events.len(),
        baseline: h,
        r,
        beta,
        phi,
        pi,
    }
}

// ---- observing a unit ----

/// Everything the fit needs from one unit: loads its transcripts, runs the curator over the plain twin.
fn observe(rows: &[Row], u: &Unit) -> Result<Option<(Obs, Analysis, Option<Analysis>)>, String> {
    let Some(ci) = u.cur else { return Ok(None) };
    let (c, p) = (&rows[ci], u.plain.map(|k| &rows[k]));
    let Some(ct) = &c.transcript else { return Ok(None) };
    let cs = load(&ct.to_string_lossy())?;
    let an = analyse(&cs.requests);
    let family = family_of_task(&c.task).map_or_else(|| auto_family(&cs.requests).to_string(), str::to_string);
    let mut obs = Obs {
        family,
        model: c.model.clone(),
        task: c.task.clone(),
        dir: c.dir.to_string_lossy().to_string(),
        rep: u.rep,
        eff_rep: u.eff_rep,
        events: None,
        blind: 0,
        fetched: an.fetched.len(),
        recall_calls: an.recall_calls,
        expo_out: an.expo_out,
        has_recall: an.has_recall,
        steps_cur: c.steps,
        steps_plain: p.map(|p| p.steps),
        score_cur: c.score,
        score_plain: p.map(|p| p.score),
        clean_pair: p.is_some_and(|p| p.clean()) && c.clean(),
        capped: c.stop.as_deref() == Some("max_steps"),
        rewrites: c.rewrites,
    };
    let mut plain_an = None;
    if let Some(p) = p {
        if let Some(pt) = &p.transcript {
            let ps = load(&pt.to_string_lossy())?;
            let spec = if c.spec.is_empty() { DEFAULT_SPEC } else { &c.spec };
            let r = run(&ps.requests, spec, Prices::LIVE, 0.0, 0)?;
            obs.events = Some(r.events.iter().filter(|e| matches!(e.kind, Kind::Cited | Kind::Denied)).count());
            obs.blind = r.events.iter().filter(|e| e.kind == Kind::Blind).count();
            plain_an = Some(analyse(&ps.requests));
        }
    }
    Ok(Some((obs, an, plain_an)))
}

/// What `fit` does.
#[derive(Debug, Clone)]
pub struct FitConfig {
    pub holdout: Holdout,
    /// Fit the parameters of this model; `None` pools every model.
    pub model: Option<String>,
    /// Families with fewer events fall back to the pooled fit.
    pub min_events: usize,
    pub seed: u64,
    pub workers: usize,
}

impl Default for FitConfig {
    fn default() -> FitConfig {
        FitConfig {
            holdout: Holdout::None,
            model: Some("deepseek-flash".to_string()),
            min_events: 20,
            seed: 1,
            workers: 4,
        }
    }
}

/// The result of a fit: every (family, model) group, the pooled fit per model filter, and the baselines.
pub struct Fit {
    pub obs: Vec<Obs>,
    /// By (family, model).
    pub groups: BTreeMap<(String, String), GroupFit>,
    /// By family over the models the config selects.
    pub families: BTreeMap<String, GroupFit>,
    pub global: GroupFit,
    pub sources: Vec<String>,
    pub config: FitConfig,
    pub held_out: usize,
}

/// Runs `f` over the indices `0..n` on up to `workers` threads, keeping the order.
fn par_map<T: Send>(n: usize, workers: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= n {
                    break;
                }
                *slots[i].lock().unwrap() = Some(f(i));
            });
        }
    });
    slots.into_iter().map(|m| m.into_inner().unwrap().expect("every slot is filled")).collect()
}

/// Fits the parameters from the run directories.
pub fn fit_dirs(dirs: &[String], cfg: &FitConfig) -> Result<Fit, String> {
    let rows = load_rows(dirs)?;
    let units = pair_up(&rows);
    let held_out = units.iter().filter(|u| u.cur.is_some() && cfg.holdout.holds(u.eff_rep)).count();
    let kept: Vec<&Unit> = units.iter().filter(|u| !cfg.holdout.holds(u.eff_rep)).collect();
    let seen = par_map(kept.len(), cfg.workers, |i| observe(&rows, kept[i]));
    let mut obs = Vec::new();
    let mut ans: Vec<(String, String, Analysis)> = Vec::new();
    for (k, s) in seen.into_iter().enumerate() {
        let Some((o, a, pa)) = s? else { continue };
        let _ = k;
        ans.push((o.family.clone(), o.model.clone(), a));
        if let Some(pa) = pa {
            ans.push((o.family.clone(), o.model.clone(), pa));
        }
        obs.push(o);
    }
    // plain runs without a curator twin still tell the in-view rate
    for u in kept.iter().filter(|u| u.cur.is_none()) {
        let r = &rows[u.plain.unwrap()];
        if let Some(t) = &r.transcript {
            let s = load(&t.to_string_lossy())?;
            let family = family_of_task(&r.task).map_or_else(|| auto_family(&s.requests).to_string(), str::to_string);
            ans.push((family, r.model.clone(), analyse(&s.requests)));
        }
    }
    let chosen = |model: &str| cfg.model.as_deref().is_none_or(|m| m == model);
    let h_of = |f: Option<&str>, m: Option<&str>| {
        let sel: Vec<&Analysis> = ans
            .iter()
            .filter(|(fa, mo, _)| f.is_none_or(|x| x == fa) && m.map_or_else(|| chosen(mo), |x| x == mo))
            .map(|(_, _, a)| a)
            .collect();
        baseline(&sel)
    };
    let mut groups = BTreeMap::new();
    let keys: BTreeSet<(String, String)> = obs.iter().map(|o| (o.family.clone(), o.model.clone())).collect();
    for (f, m) in &keys {
        let sel: Vec<&Obs> = obs.iter().filter(|o| &o.family == f && &o.model == m).collect();
        groups.insert((f.clone(), m.clone()), fit_group(&sel, h_of(Some(f), Some(m)), cfg.seed));
    }
    let mut families = BTreeMap::new();
    let fams: BTreeSet<&String> = obs.iter().filter(|o| chosen(&o.model)).map(|o| &o.family).collect();
    for f in fams {
        let sel: Vec<&Obs> = obs.iter().filter(|o| &o.family == f && chosen(&o.model)).collect();
        families.insert(f.clone(), fit_group(&sel, h_of(Some(f), None), cfg.seed));
    }
    let all: Vec<&Obs> = obs.iter().filter(|o| chosen(&o.model)).collect();
    let global = fit_group(&all, h_of(None, None), cfg.seed);
    let mut sources: Vec<String> = rows.iter().map(|r| r.dir.to_string_lossy().to_string()).collect();
    sources.sort();
    sources.dedup();
    Ok(Fit { obs, groups, families, global, sources, config: cfg.clone(), held_out })
}

impl Fit {
    /// The `params.json` value: `{family: {r, beta, phi, pi, mu, n_runs, n_events, ci, sources, date}}` for
    /// the families with at least `min_events` events; a family with fewer takes the pooled fit and says so.
    pub fn params_json(&self, date: &str) -> Value {
        let entry = |g: &GroupFit, own: &GroupFit, fallback: bool| {
            json!({
                "r": round(g.r.value), "beta": round(g.beta.value), "phi": round(g.phi.value), "pi": round(g.pi.value),
                "mu": 2.0,
                "n_runs": own.runs, "n_events": own.events,
                "ci": { "r": g.r.json(), "beta": g.beta.json(), "phi": g.phi.json(), "pi": g.pi.json() },
                "validated": !fallback,
                "fallback": if fallback { json!("global") } else { Value::Null },
                "own": { "r": round(own.r.value), "beta": round(own.beta.value), "phi": round(own.phi.value),
                         "r_ci": own.r.json(), "beta_ci": own.beta.json(), "phi_ci": own.phi.json() },
                "sources": self.sources, "date": date,
            })
        };
        let mut o = serde_json::Map::new();
        let mut g = entry(&self.global, &self.global, false);
        g["r_den"] = json!(0.9);
        o.insert("global".to_string(), g);
        for (f, g) in &self.families {
            let fb = g.events < self.config.min_events;
            o.insert(f.clone(), if fb { entry(&self.global, g, true) } else { entry(g, g, false) });
        }
        let mut by_model = serde_json::Map::new();
        for ((f, m), g) in &self.groups {
            by_model.insert(format!("{f}@{m}"), entry(g, g, g.events < self.config.min_events));
        }
        o.insert("by_model".to_string(), Value::Object(by_model));
        o.insert("r_den".to_string(), json!(0.9));
        Value::Object(o)
    }

    /// The per-family table.
    pub fn table(&self) -> String {
        let mut s = String::new();
        let sel = self.config.model.clone().unwrap_or_else(|| "all models".to_string());
        s += &format!(
            "fit on {} curator runs ({} held out by rep parity: {:?}); parameters for {sel}; baseline = in-view repeat rate per call per request; * = rule of three\n",
            self.obs.len(),
            self.held_out,
            self.config.holdout
        );
        s += &format!(
            "{:<8} {:<16} {:>4} {:>5} {:>6} {:>5} {:>9}  {:<24} {:<24} {:<24} {:<22} {}\n",
            "family", "model", "runs", "pairs", "events", "fetch", "baseline", "r", "beta", "phi", "pi", "status"
        );
        let row = |s: &mut String, f: &str, m: &str, g: &GroupFit, status: &str| {
            *s += &format!(
                "{:<8} {:<16} {:>4} {:>5} {:>6} {:>5} {:>9.5}  {:<24} {:<24} {:<24} {:<22} {}\n",
                f,
                m,
                g.runs,
                g.pairs,
                g.events,
                g.fetched,
                g.baseline,
                g.r.show(),
                g.beta.show(),
                g.phi.show(),
                g.pi.show(),
                status
            );
        };
        let status = |g: &GroupFit, min: usize| {
            if g.events >= min {
                "validated".to_string()
            } else {
                format!("unvalidated (n events = {})", g.events)
            }
        };
        for ((f, m), g) in &self.groups {
            row(&mut s, f, m, g, &status(g, self.config.min_events));
        }
        s += "pooled over the selected model:\n";
        for (f, g) in &self.families {
            row(&mut s, f, "(pooled)", g, &status(g, self.config.min_events));
        }
        row(&mut s, "global", "(pooled)", &self.global, "fallback");
        s
    }
}

// ---- the price model ----

/// The prices that reproduce a recorded row best.
pub fn prices_for(r: &Row) -> Prices {
    let calc = |p: Prices| p.dollars(r.input as i64, r.cache_read as i64) + p.output(r.output as i64);
    let (a, b) = (calc(Prices::LIVE), calc(PRICES_OLD));
    if (a - r.cost).abs() <= (b - r.cost).abs() {
        Prices::LIVE
    } else {
        PRICES_OLD
    }
}

/// The plain arm priced by the prefix cache alone: input tokens as the engine estimates them, `life`
/// seconds and `slots` requests of cache. Returns (miss, hit, output tokens).
pub fn plain_tokens(requests: &[Request], life: f64, slots: usize) -> (i64, i64, i64) {
    let mut cache = Cache::new(life, slots);
    let (mut miss, mut hit, mut out) = (0, 0, 0);
    let mut prev_out = 0;
    for (i, q) in requests.iter().enumerate() {
        let all = q.messages.iter().map(tokens_of).sum::<i64>() + q.system_tokens as i64;
        // the provider caches what the model wrote at the step before, so it is not billed as new input
        let h = (cache.send(&q.messages, q.system_tokens as i64, q.now) + prev_out).min(all);
        prev_out = reply_to(requests, i).map_or(0, tokens_of);
        hit += h;
        miss += all - h;
        out += q.out_tokens.unwrap_or(0) as i64;
    }
    (miss, hit, out)
}

/// One plain run: predicted against recorded.
#[derive(Debug, Clone)]
pub struct PriceRow {
    pub path: String,
    pub recorded_cost: f64,
    pub predicted_cost: f64,
    pub recorded_cache: u64,
    pub predicted_cache: f64,
    pub scale: f64,
}

impl PriceRow {
    pub fn cost_err(&self) -> f64 {
        (self.predicted_cost - self.recorded_cost).abs() / self.recorded_cost.max(1e-12)
    }

    pub fn cache_err(&self) -> f64 {
        (self.predicted_cache - self.recorded_cache as f64).abs() / (self.recorded_cache as f64).max(1.0)
    }
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The token scale of a plain run: recorded input tokens over the engine's estimate.
pub fn token_scale(r: &Row, miss: i64, hit: i64) -> f64 {
    let est = (miss + hit).max(1) as f64;
    let real = (r.input + r.cache_read) as f64;
    if real > 0.0 {
        real / est
    } else {
        1.0
    }
}

/// Checks the price model on the plain arms of the rows: the cost and the cache read it predicts against the
/// recorded ones. `scale` is the engine-estimate-to-provider-tokens factor (`None`: each run's own).
pub fn price_check(rows: &[Row], life: f64, slots: usize, scale: Option<f64>) -> Vec<PriceRow> {
    let plains: Vec<&Row> = rows.iter().filter(|r| r.kind == ArmKind::Plain && r.transcript.is_some()).collect();
    let mut out = Vec::new();
    for r in plains {
        let Ok(s) = load(&r.transcript.as_ref().unwrap().to_string_lossy()) else { continue };
        let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
        let k = scale.unwrap_or_else(|| token_scale(r, miss, hit));
        let p = prices_for(r);
        let predicted = p.dollars((miss as f64 * k) as i64, (hit as f64 * k) as i64) + p.output(o);
        out.push(PriceRow {
            path: r.transcript.as_ref().unwrap().to_string_lossy().to_string(),
            recorded_cost: r.cost,
            predicted_cost: predicted,
            recorded_cache: r.cache_read,
            predicted_cache: hit as f64 * k,
            scale: k,
        });
    }
    out
}

/// Median cost error and median cache error of a price check.
pub fn price_medians(rows: &[PriceRow]) -> (f64, f64) {
    (
        median(&mut rows.iter().map(PriceRow::cost_err).collect::<Vec<_>>()),
        median(&mut rows.iter().map(PriceRow::cache_err).collect::<Vec<_>>()),
    )
}

// ---- predict and check ----

/// The simulator's prediction for one plain run.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub task: String,
    pub rep: i64,
    pub eff_rep: i64,
    pub family: String,
    pub steps_plain: usize,
    pub extra_steps: f64,
    pub cost_plain: f64,
    pub cost_cur: f64,
    pub p_fail: f64,
    pub events: usize,
}

impl Prediction {
    pub fn steps_cur(&self, max_steps: usize) -> f64 {
        (self.steps_plain as f64 + self.extra_steps).min(max_steps as f64)
    }
}

/// Predicts the curator arm of one plain run: its transcript through the curator with `spec`, the behaviour
/// layer on top. Costs are the simulator's own on both sides (priced at the run's recorded prices, tokens
/// scaled by `scale`, or by the run's own recorded usage when `None`).
pub fn predict_row(
    r: &Row,
    spec: &str,
    set: &ParamSet,
    scale: Option<f64>,
    life: f64,
    slots: usize,
) -> Result<Prediction, String> {
    let t = r.transcript.as_ref().ok_or("no transcript")?;
    let s = load(&t.to_string_lossy())?;
    let family = family_of_task(&r.task).map_or_else(|| auto_family(&s.requests).to_string(), str::to_string);
    let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
    let k = scale.unwrap_or_else(|| token_scale(r, miss, hit));
    let base = prices_for(r);
    let prices = Prices { miss: base.miss * k, hit: base.hit * k, out: base.out };
    let cost_plain = prices.dollars(miss, hit) + prices.output(o);
    let cur = run(&s.requests, spec, prices, life, slots)?;
    let cx = Context::measure(&s.requests, spec, prices, r.max_steps);
    let a = analytic(&cur, set.get(&family), set.r_den, &cx);
    Ok(Prediction {
        task: r.task.clone(),
        rep: r.rep,
        eff_rep: r.rep,
        family,
        steps_plain: r.steps,
        extra_steps: a.extra_steps,
        cost_plain,
        cost_cur: cur.cost + a.extra_cost,
        p_fail: a.p_fail,
        events: a.events,
    })
}

/// `sim predict`: the plain rows of `results`, each fed through the simulator.
pub fn predict(
    results: &str,
    arm: &str,
    spec: &str,
    set: &ParamSet,
    life: f64,
    slots: usize,
) -> Result<Vec<Prediction>, String> {
    let rows = load_rows(&[results.to_string()])?;
    let sel: Vec<(usize, &Row)> =
        rows.iter().enumerate().filter(|(_, r)| r.arm == arm && r.transcript.is_some()).collect();
    let all = pair_up(&rows);
    let scale: Option<f64> = None;
    par_map(sel.len(), 4, |i| {
        let mut p = predict_row(sel[i].1, spec, set, scale, life, slots)?;
        if let Some(u) = all.iter().find(|u| u.plain == Some(sel[i].0)) {
            p.eff_rep = u.eff_rep;
        }
        Ok(p)
    })
    .into_iter()
    .collect()
}

/// One task's line of the check table.
#[derive(Debug, Clone)]
pub struct TaskCheck {
    pub task: String,
    pub pairs: usize,
    pub family: String,
    pub obs_ratio: f64,
    pub pred_ratio: f64,
    pub ratio_tol: f64,
    pub obs_dcost: f64,
    pub pred_dcost: f64,
    pub ratio_ok: bool,
    pub cost_ok: bool,
    pub events: usize,
}

/// The §5.2 table.
pub struct Check {
    pub tasks: Vec<TaskCheck>,
    pub pooled_obs_ratio: f64,
    pub pooled_pred_ratio: f64,
    pub pooled_obs_dcost: f64,
    pub pooled_pred_dcost: f64,
    pub pooled_ratio_ok: bool,
    pub pooled_cost_ok: bool,
    /// Median error of the reverse price check (curator arm priced as sent vs recorded).
    pub reverse_err: f64,
    pub reverse_ok: bool,
    /// Capped curator runs: (task, rep, P[fail] predicted from the plain twin).
    pub capped: Vec<(String, i64, f64)>,
    pub capped_ok: bool,
    pub pairs: usize,
    pub half: String,
    pub price_cost_err: f64,
    pub price_cache_err: f64,
}

/// The tolerance of the steps ratio: 1.64 standard errors of a ratio of per-run means, with the per-run SD of
/// the spec (17 %) as the floor when the sample is too small to tell.
fn ratio_se(pairs: &[(f64, f64)]) -> f64 {
    let n = pairs.len().max(1) as f64;
    let floor = 0.17 * (2.0 / n).sqrt();
    if pairs.len() < 4 {
        return floor;
    }
    let ratios: Vec<f64> = pairs.iter().map(|(c, p)| c / p.max(1.0)).collect();
    let m = ratios.iter().sum::<f64>() / n;
    let sd = (ratios.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    floor.max(sd / n.sqrt())
}

/// Checks the simulator on the pairs of `results` whose rep parity is `holdout` (all pairs for `None`).
pub fn check(
    results: &str,
    spec: &str,
    set: &ParamSet,
    holdout: Holdout,
    life: f64,
    slots: usize,
) -> Result<Check, String> {
    let rows = load_rows(&[results.to_string()])?;
    let units = pair_up(&rows);
    let valid: Vec<&Unit> = units
        .iter()
        .filter(|u| {
            u.plain.zip(u.cur).is_some_and(|(p, c)| rows[p].clean() && rows[c].clean())
                && rows[u.plain.unwrap()].transcript.is_some()
                && (holdout == Holdout::None || holdout.holds(u.eff_rep))
        })
        .collect();
    let preds = par_map(valid.len(), 4, |i| {
        let u = valid[i];
        predict_row(&rows[u.plain.unwrap()], spec, set, None, life, slots)
    });
    let mut by_task: BTreeMap<String, Vec<(&Unit, Prediction)>> = BTreeMap::new();
    for (u, p) in valid.iter().zip(preds) {
        by_task.entry(rows[u.plain.unwrap()].task.clone()).or_default().push((u, p?));
    }
    let mut tasks = Vec::new();
    let (mut os_c, mut os_p, mut ps_c, mut oc_c, mut oc_p, mut pc_c, mut pc_p) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (task, items) in &by_task {
        let pairs: Vec<(f64, f64)> = items
            .iter()
            .map(|(u, _)| (rows[u.cur.unwrap()].steps as f64, rows[u.plain.unwrap()].steps as f64))
            .collect();
        let (sc, sp): (f64, f64) = pairs.iter().fold((0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1));
        let pred_c: f64 = items.iter().map(|(u, p)| p.steps_cur(rows[u.plain.unwrap()].max_steps)).sum();
        let (cc, cp): (f64, f64) = items
            .iter()
            .fold((0.0, 0.0), |a, (u, _)| (a.0 + rows[u.cur.unwrap()].cost, a.1 + rows[u.plain.unwrap()].cost));
        let (pcc, pcp): (f64, f64) = items.iter().fold((0.0, 0.0), |a, (_, p)| (a.0 + p.cost_cur, a.1 + p.cost_plain));
        let (obs_ratio, pred_ratio) = (sc / sp.max(1.0), pred_c / sp.max(1.0));
        let tol = 1.64 * ratio_se(&pairs);
        let (obs_d, pred_d) = (100.0 * (cc / cp.max(1e-12) - 1.0), 100.0 * (pcc / pcp.max(1e-12) - 1.0));
        tasks.push(TaskCheck {
            task: task.clone(),
            pairs: items.len(),
            family: items[0].1.family.clone(),
            obs_ratio,
            pred_ratio,
            ratio_tol: tol,
            obs_dcost: obs_d,
            pred_dcost: pred_d,
            ratio_ok: (pred_ratio - obs_ratio).abs() <= tol,
            cost_ok: (pred_d - obs_d).abs() <= 8.0,
            events: items.iter().map(|(_, p)| p.events).sum(),
        });
        os_c += sc;
        os_p += sp;
        ps_c += pred_c;
        oc_c += cc;
        oc_p += cp;
        pc_c += pcc;
        pc_p += pcp;
    }
    let (pooled_obs_ratio, pooled_pred_ratio) = (os_c / os_p.max(1.0), ps_c / os_p.max(1.0));
    let (pooled_obs_dcost, pooled_pred_dcost) =
        (100.0 * (oc_c / oc_p.max(1e-12) - 1.0), 100.0 * (pc_c / pc_p.max(1e-12) - 1.0));
    // the reverse check: the price model on the curator transcripts as sent
    let mut errs = Vec::new();
    for u in &valid {
        let (pr, r) = (&rows[u.plain.unwrap()], &rows[u.cur.unwrap()]);
        let Some(ct) = &r.transcript else { continue };
        let ps = load(&pr.transcript.as_ref().unwrap().to_string_lossy())?;
        let (pm, ph, _) = plain_tokens(&ps.requests, life, slots);
        let scale = token_scale(pr, pm, ph);
        let s = load(&ct.to_string_lossy())?;
        let (miss, hit, o) = plain_tokens(&s.requests, life, slots);
        let p = prices_for(r);
        let pred = p.dollars((miss as f64 * scale) as i64, (hit as f64 * scale) as i64) + p.output(o);
        errs.push((pred - r.cost).abs() / r.cost.max(1e-12));
    }
    let curs = errs.clone();
    let reverse_err = median(&mut errs);
    // the capped curator runs: P[fail] from the plain twin
    let mut capped = Vec::new();
    for u in units.iter().filter(|u| holdout == Holdout::None || holdout.holds(u.eff_rep)) {
        let (Some(p), Some(c)) = (u.plain, u.cur) else { continue };
        if rows[c].stop.as_deref() == Some("max_steps") && rows[p].transcript.is_some() {
            let pr = predict_row(&rows[p], spec, set, None, life, slots)?;
            capped.push((rows[c].task.clone(), u.rep, pr.p_fail));
        }
    }
    let need = |n: usize| (n * 5).div_ceil(6).max(1);
    let t = tasks.len();
    let pr = price_check(&rows, life, slots, None);
    let (price_cost_err, price_cache_err) = price_medians(&pr);
    let _ = need;
    Ok(Check {
        pooled_ratio_ok: (pooled_pred_ratio - pooled_obs_ratio).abs() <= 0.03,
        pooled_cost_ok: (pooled_pred_dcost - pooled_obs_dcost).abs() <= 3.0,
        reverse_ok: errs_ok(reverse_err, curs.len()),
        capped_ok: capped.iter().all(|c| c.2 >= 0.5),
        pooled_obs_ratio,
        pooled_pred_ratio,
        pooled_obs_dcost,
        pooled_pred_dcost,
        reverse_err,
        capped,
        pairs: valid.len(),
        half: match holdout {
            Holdout::None => "all valid pairs".to_string(),
            Holdout::Even => "even reps".to_string(),
            Holdout::Odd => "odd reps".to_string(),
        },
        price_cost_err,
        price_cache_err,
        tasks: {
            let _ = t;
            tasks
        },
    })
}

fn errs_ok(err: f64, n: usize) -> bool {
    n > 0 && err <= 0.05
}

impl Check {
    /// Tasks that pass the steps ratio and the cost rule, and how many are needed (5 of 6, scaled).
    pub fn passes(&self) -> (usize, usize, usize) {
        let t = self.tasks.len();
        (
            self.tasks.iter().filter(|x| x.ratio_ok).count(),
            self.tasks.iter().filter(|x| x.cost_ok).count(),
            (t * 5).div_ceil(6),
        )
    }

    pub fn overall(&self) -> bool {
        let (a, b, need) = self.passes();
        !self.tasks.is_empty()
            && a >= need
            && b >= need
            && self.pooled_ratio_ok
            && self.pooled_cost_ok
            && self.reverse_ok
            && self.capped_ok
    }

    pub fn table(&self) -> String {
        let mut s = String::new();
        s += &format!("check on {} valid pairs ({})\n", self.pairs, self.half);
        s += &format!(
            "{:<22} {:<7} {:>5} {:>7} | {:>9} {:>9} {:>8} {:>5} | {:>8} {:>8} {:>8} {:>5}\n",
            "task", "family", "pairs", "events", "obs ratio", "pred", "tol", "ok", "obs dC%", "pred dC%", "diff", "ok"
        );
        for t in &self.tasks {
            s += &format!(
                "{:<22} {:<7} {:>5} {:>7} | {:>9.3} {:>9.3} {:>8.3} {:>5} | {:>+8.1} {:>+8.1} {:>8.1} {:>5}\n",
                t.task,
                t.family,
                t.pairs,
                t.events,
                t.obs_ratio,
                t.pred_ratio,
                t.ratio_tol,
                if t.ratio_ok { "pass" } else { "FAIL" },
                t.obs_dcost,
                t.pred_dcost,
                t.pred_dcost - t.obs_dcost,
                if t.cost_ok { "pass" } else { "FAIL" }
            );
        }
        let (a, b, need) = self.passes();
        s += &format!(
            "{:<22} {:<7} {:>5} {:>7} | {:>9.3} {:>9.3} {:>8.3} {:>5} | {:>+8.1} {:>+8.1} {:>8.1} {:>5}\n",
            "pooled",
            "",
            self.pairs,
            "",
            self.pooled_obs_ratio,
            self.pooled_pred_ratio,
            0.03,
            if self.pooled_ratio_ok { "pass" } else { "FAIL" },
            self.pooled_obs_dcost,
            self.pooled_pred_dcost,
            self.pooled_pred_dcost - self.pooled_obs_dcost,
            if self.pooled_cost_ok { "pass" } else { "FAIL" }
        );
        s += &format!(
            "tasks passing: steps ratio {a}, cost {b}, of {} (need {need}); pooled ratio within 0.03, cost within 3 pts\n",
            self.tasks.len()
        );
        s += &format!(
            "reverse price check (curator arm priced as sent vs recorded): median error {:.1} % (limit 5 %): {}\n",
            100.0 * self.reverse_err,
            if self.reverse_ok { "pass" } else { "FAIL" }
        );
        if self.capped.is_empty() {
            s += "capped curator runs: none in this half (P[fail] rule not exercised)\n";
        } else {
            for (t, rep, p) in &self.capped {
                s += &format!(
                    "capped curator run {t} rep {rep}: P[fail] {p:.3} (need >= 0.5): {}\n",
                    if *p >= 0.5 { "pass" } else { "FAIL" }
                );
            }
        }
        s += &format!(
            "price model on the plain arms: median cost error {:.1} %, median cache_read error {:.1} %\n",
            100.0 * self.price_cost_err,
            100.0 * self.price_cache_err
        );
        s += &format!("overall: {}\n", if self.overall() { "PASS" } else { "FAIL" });
        s
    }

    /// The markdown report (`sim/reports/h1-check.md`).
    pub fn markdown(&self, results: &str, params: &str, spec: &str, date: &str) -> String {
        let mut s = format!("# Simulator check against held-out pairs\n\nWritten {date} by `sim check`.\n\n");
        s += &format!(
            "- results: `{results}`\n- params: `{params}`\n- spec: `{spec}`\n- half checked: {}, {} valid pairs\n\n",
            self.half, self.pairs
        );
        s += "| task | family | pairs | obs steps ratio | predicted | tol | steps | obs dCost % | predicted | cost |\n|---|---|---|---|---|---|---|---|---|---|\n";
        for t in &self.tasks {
            s += &format!(
                "| {} | {} | {} | {:.3} | {:.3} | {:.3} | {} | {:+.1} | {:+.1} | {} |\n",
                t.task,
                t.family,
                t.pairs,
                t.obs_ratio,
                t.pred_ratio,
                t.ratio_tol,
                if t.ratio_ok { "pass" } else { "fail" },
                t.obs_dcost,
                t.pred_dcost,
                if t.cost_ok { "pass" } else { "fail" }
            );
        }
        s += &format!(
            "| **pooled** | | {} | {:.3} | {:.3} | 0.030 | {} | {:+.1} | {:+.1} | {} |\n\n",
            self.pairs,
            self.pooled_obs_ratio,
            self.pooled_pred_ratio,
            if self.pooled_ratio_ok { "pass" } else { "fail" },
            self.pooled_obs_dcost,
            self.pooled_pred_dcost,
            if self.pooled_cost_ok { "pass" } else { "fail" }
        );
        let (a, b, need) = self.passes();
        s += &format!("Tasks passing: steps ratio {a}, cost {b}, of {} (need {need}).\n\n", self.tasks.len());
        s += &format!(
            "Reverse price check: median error {:.1} % (limit 5 %), {}.\n\n",
            100.0 * self.reverse_err,
            if self.reverse_ok { "pass" } else { "fail" }
        );
        for (t, rep, p) in &self.capped {
            s += &format!(
                "Capped curator run {t} rep {rep}: predicted P[fail] {p:.3}, {}.\n",
                if *p >= 0.5 { "pass" } else { "fail" }
            );
        }
        s += &format!(
            "\nPrice model on the plain arms: median cost error {:.1} %, median cache_read error {:.1} %.\n\n**Overall: {}**\n\nTo swap halves run `sim fit --holdout <parity>` and `sim check --holdout <parity>` with the other parity and compare.\n",
            100.0 * self.price_cost_err,
            100.0 * self.price_cache_err,
            if self.overall() { "PASS" } else { "FAIL" }
        );
        s
    }
}
