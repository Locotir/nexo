//! Paired checkpoints: what a curated view costs in answers, without the noise of whole runs.
//!
//! `cargo run --release -p nexo-bench --example checkpoint -- --llmb DIR --instance ID [--spec SPEC]...
//!  [--samples K] [--temperature T] [--upstream URL] [--model M] [--key-env VAR] [--budget USD] [--dry]
//!  [--at N]... [--recorded] FILE...`
//!
//! Each `FILE` is a `nexo bench` transcript of the plain arm of a canonstep instance. A checkpoint is a
//! request whose reply answered an objective (text, no tool call): the agent had what it needed there.
//! At each checkpoint the same request is sent again, once with the whole history (plain) and once with
//! the view of each `--spec` (the curator fed every request of the file in order, as in a run), `K` times
//! each, and every reply is scored with the instance's oracle. A reply that calls a tool instead counts as
//! a read: the view lacked something the agent went back for.
//!
//! Both branches see the same history up to the checkpoint, so a difference between them is the view's,
//! not the luck of the trajectory that sinks whole runs (a loop at the first objective, a misread file).
//! The pairs are compared with an exact sign test on the checkpoints where they disagree.
//!
//! `--dry` sends nothing: it lists the checkpoints, the tokens of each view and what the calls would cost.
//! `--at N` keeps only the checkpoints at request `N`. `--recorded` sends plain no more: its recorded answer
//! stands for it (at temperature 0 a fresh plain reply matches it almost always), so only the views are paid.
//! The key is read from `--key-env` (default `DEEPSEEK_API_KEY`) by the provider, never printed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use futures::StreamExt;
use nexo_bench::llmb::{canonstep, read_instance};
use nexo_core::{Collector, Message, ModelRequest, Part, ProviderConfig, ToolSpec};
use nexo_curator::Curator;
use serde_json::{Map, Value};

struct Args {
    llmb: PathBuf,
    instance: String,
    specs: Vec<String>,
    samples: u32,
    temperature: Option<f32>,
    upstream: String,
    model: String,
    key_env: String,
    budget: f64,
    dry: bool,
    /// Only the checkpoints at these request numbers (all when empty).
    at: Vec<u64>,
    /// Plain is not sent again: its recorded answer stands for it.
    recorded: bool,
    files: Vec<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        llmb: PathBuf::new(),
        instance: String::new(),
        specs: Vec::new(),
        samples: 1,
        temperature: Some(0.0),
        upstream: "https://api.deepseek.com/anthropic".into(),
        model: "deepseek-chat".into(),
        key_env: "DEEPSEEK_API_KEY".into(),
        budget: 0.05,
        dry: false,
        at: Vec::new(),
        recorded: false,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--llmb" => a.llmb = v()?.into(),
            "--instance" => a.instance = v()?,
            "--spec" => a.specs.push(v()?),
            "--samples" => a.samples = v()?.parse().map_err(|e| format!("--samples: {e}"))?,
            "--temperature" => a.temperature = Some(v()?.parse().map_err(|e| format!("--temperature: {e}"))?),
            "--upstream" => a.upstream = v()?,
            "--model" => a.model = v()?,
            "--key-env" => a.key_env = v()?,
            "--budget" => a.budget = v()?.parse().map_err(|e| format!("--budget: {e}"))?,
            "--dry" => a.dry = true,
            "--at" => a.at.push(v()?.parse().map_err(|e| format!("--at: {e}"))?),
            "--recorded" => a.recorded = true,
            f if !f.starts_with("--") => a.files.push(f.into()),
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if a.instance.is_empty() || a.files.is_empty() {
        return Err("give --llmb DIR, --instance ID and at least one transcript".into());
    }
    Ok(a)
}

/// A request of a transcript where the agent answered.
struct Checkpoint {
    file: String,
    n: u64,
    objective: String,
    system: String,
    tools: Vec<String>,
    /// The messages of each arm: plain first, then one per spec.
    views: Vec<Vec<Value>>,
    /// Whether the recorded answer was right.
    recorded: bool,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The objective the latest request names.
fn objective(messages: &[Value]) -> Option<String> {
    const MARK: &str = "Current independent objective: ";
    messages.iter().rev().filter(|m| m.get("role").and_then(Value::as_str) == Some("user")).find_map(|m| {
        let t = text_of(m.get("content").unwrap_or(&Value::Null));
        let at = t.rfind(MARK)?;
        t[at + MARK.len()..].split_whitespace().next().map(str::to_string)
    })
}

/// Whether `text` answers `obj` right, by the oracle.
fn right(a: &canonstep::Adapted, expected: &Map<String, Value>, obj: &str, text: &str) -> bool {
    let Some(st) = canonstep::statements(text, obj, &a.steps) else { return false };
    let mut stated: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (key, comps) in st.into_iter().filter(|(k, _)| k == obj) {
        stated.entry(key).or_default().extend(comps);
    }
    let snap = canonstep::score_snapshot(a, &stated, expected);
    snap.cases.iter().any(|(id, s)| id == obj && s.total > 0 && s.correct == s.total)
}

fn tokens(view: &[Value]) -> u64 {
    view.iter().map(|m| m.to_string().len() as u64 / 4).sum()
}

/// Two-sided exact sign test: the chance of a split at least this uneven among `b + c` coin flips.
fn sign_test(b: u64, c: u64) -> f64 {
    let n = b + c;
    if n == 0 {
        return 1.0;
    }
    let k = b.min(c);
    let mut p = 0.0;
    let mut choose = 1.0f64;
    for i in 0..=k {
        if i > 0 {
            choose = choose * (n - i + 1) as f64 / i as f64;
        }
        p += choose;
    }
    (2.0 * p / 2f64.powi(n as i32)).min(1.0)
}

fn checkpoints(a: &Args, inst: &canonstep::Adapted, expected: &Map<String, Value>) -> Result<Vec<Checkpoint>, String> {
    let mut out = Vec::new();
    for path in &a.files {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut curators: Vec<Curator> = a.specs.iter().map(|s| Curator::new(s)).collect::<Result<_, _>>()?;
        let mut system = String::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let rec: Value = serde_json::from_str(line).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(s) = rec.get("system").and_then(Value::as_str) {
                system = s.to_string();
            }
            let Some(Value::Array(messages)) = rec.get("messages") else { continue };
            let tools: Vec<String> = rec
                .get("tools")
                .and_then(Value::as_array)
                .map(|t| t.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            let mut views = vec![messages.clone()];
            for c in &mut curators {
                c.tools(&tools);
                views.push(c.view(system.len() as u64 / 4, messages, 0.0)?.view);
            }
            let reply = rec.get("reply").and_then(|r| r.get("content")).cloned().unwrap_or(Value::Null);
            let calls = reply
                .as_array()
                .is_some_and(|p| p.iter().any(|x| x.get("type").and_then(Value::as_str) == Some("tool_use")));
            let said = text_of(&reply);
            let Some(obj) = objective(messages) else { continue };
            if calls || said.trim().is_empty() || canonstep::statements(&said, &obj, &inst.steps).is_none() {
                continue;
            }
            let n = rec.get("n").and_then(Value::as_u64).unwrap_or(0);
            if !a.at.is_empty() && !a.at.contains(&n) {
                continue;
            }
            out.push(Checkpoint {
                file: path.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned()),
                n,
                recorded: right(inst, expected, &obj, &said),
                objective: obj,
                system: system.clone(),
                tools,
                views,
            });
        }
    }
    Ok(out)
}

#[derive(Default, Clone, Copy)]
struct Tally {
    right: u64,
    wrong: u64,
    reads: u64,
    tokens: u64,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("checkpoint: {e}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let battery = canonstep::battery(&a.llmb)?;
    let inst = battery.iter().find(|x| x.id == a.instance).ok_or(format!("no instance {}", a.instance))?;
    let oracle = read_instance(&a.llmb.join(&inst.path))?.oracle()?;
    let cps = checkpoints(&a, inst, &oracle.expected)?;
    let arms: Vec<String> = std::iter::once("plain".to_string()).chain(a.specs.iter().cloned()).collect();
    let price = nexo_bench::cost::price_of(&a.model).unwrap_or_default();
    let mut specs: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for set in [nexo_tools::builtin(), nexo_tools::local()] {
        for s in set.specs() {
            specs.entry(s.name.clone()).or_insert(s);
        }
    }

    println!(
        "| file | n | objective | recorded | {} |",
        arms.iter().map(|x| format!("{x} tokens")).collect::<Vec<_>>().join(" | ")
    );
    println!("|---|---|---|---|{}", "---|".repeat(arms.len()));
    let mut estimate = 0.0;
    let mut last: Vec<(String, Vec<Value>)> = vec![(String::new(), Vec::new()); arms.len()];
    let mut share_kept = vec![0.0; arms.len()];
    for cp in &cps {
        let t: Vec<u64> = cp.views.iter().map(|v| tokens(v)).collect();
        // a prefix cache: what the arm's previous call (same file) sent first is a hit, the rest a miss
        // once; the other samples hit whole
        for (k, v) in cp.views.iter().enumerate() {
            if k == 0 && a.recorded {
                share_kept[k] += 1.0 / cps.len() as f64;
                continue;
            }
            let same =
                if last[k].0 == cp.file { last[k].1.iter().zip(v).take_while(|(x, y)| x == y).count() } else { 0 };
            let miss = tokens(&v[same..]) as f64;
            let hits = t[k] as f64 - miss + t[k] as f64 * (a.samples as f64 - 1.0);
            estimate += (miss * 0.28 + hits * 0.028) / 1e6;
            share_kept[k] += t[k] as f64 / t[0].max(1) as f64 / cps.len() as f64;
            last[k] = (cp.file.clone(), v.clone());
        }
        let shown: Vec<String> = t.iter().map(u64::to_string).collect();
        println!(
            "| {} | {} | {} | {} | {} |",
            cp.file,
            cp.n,
            cp.objective,
            if cp.recorded { "1" } else { "x" },
            shown.join(" | ")
        );
    }
    for (k, arm) in arms.iter().enumerate() {
        println!("view of {arm}: {:.1} % of plain on average", 100.0 * share_kept[k]);
    }
    println!("\n{} checkpoints, {} arms, {} samples: about {estimate:.4} $ of input", cps.len(), arms.len(), a.samples);
    if a.dry {
        return Ok(());
    }
    if !price.is_known() {
        return Err(format!(
            "nexo has no prices for {}, so the budget ({} $) could not stop the run",
            a.model, a.budget
        ));
    }

    let provider = ProviderConfig::new(nexo_bench::protocol_of(&a.upstream), &a.upstream, Some(a.key_env.as_str()));
    let client = nexo_providers::Client::new(nexo_providers::registry());
    let mut spent = 0.0;
    let mut tally = vec![Tally::default(); arms.len()];
    // per checkpoint and arm: the share of samples that were right
    let mut share: Vec<Vec<f64>> = Vec::new();
    'cps: for cp in &cps {
        let mut row = Vec::new();
        for (k, view) in cp.views.iter().enumerate() {
            if k == 0 && a.recorded {
                let t = &mut tally[0];
                (t.right, t.wrong) = (t.right + u64::from(cp.recorded), t.wrong + u64::from(!cp.recorded));
                row.push(if cp.recorded { 1.0 } else { 0.0 });
                continue;
            }
            let messages: Vec<Message> = view
                .iter()
                .map(|m| serde_json::from_value(m.clone()))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            let mut req = ModelRequest::new(&a.model, &cp.system, messages);
            req.tools = cp.tools.iter().filter_map(|t| specs.get(t).cloned()).collect();
            req.max_tokens = 4096;
            req.temperature = a.temperature;
            let mut ok = 0;
            for _ in 0..a.samples {
                if spent >= a.budget {
                    eprintln!("budget reached at {spent:.4} $");
                    break 'cps;
                }
                let mut stream = client.stream(&provider, &req, 2).await.map_err(|e| e.to_string())?;
                let mut col = Collector::new();
                while let Some(ev) = stream.next().await {
                    col.push(ev);
                }
                if let Some(e) = &col.error {
                    return Err(format!("the reply failed: {e}"));
                }
                spent += nexo_bench::cost::cost(&price, &col.usage);
                let msg = col.message();
                let t = &mut tally[k];
                t.tokens += col.usage.input + col.usage.cache_read;
                if msg.content.iter().any(|p| matches!(p, Part::ToolCall { .. })) {
                    t.reads += 1;
                    continue;
                }
                let said: String = msg
                    .content
                    .iter()
                    .filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None })
                    .collect();
                if right(inst, &oracle.expected, &cp.objective, &said) {
                    t.right += 1;
                    ok += 1;
                } else {
                    t.wrong += 1;
                }
            }
            row.push(ok as f64 / a.samples as f64);
        }
        if row.len() == arms.len() {
            println!(
                "{} n={} {}: {}",
                cp.file,
                cp.n,
                cp.objective,
                row.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(" ")
            );
            share.push(row);
        }
    }

    println!("\n| arm | right | wrong | reads (tool call) | input tokens | vs plain: better / worse checkpoints | p (sign test) |");
    println!("|---|---|---|---|---|---|---|");
    for (k, arm) in arms.iter().enumerate() {
        let t = tally[k];
        let (b, w) = share.iter().fold((0, 0), |(b, w), r| (b + u64::from(r[k] > r[0]), w + u64::from(r[k] < r[0])));
        let vs = if k == 0 { "-".into() } else { format!("{b} / {w}") };
        let p = if k == 0 { "-".into() } else { format!("{:.3}", sign_test(b, w)) };
        println!("| {arm} | {} | {} | {} | {} | {vs} | {p} |", t.right, t.wrong, t.reads, t.tokens);
    }
    println!("\nspent {spent:.4} $ on {} checkpoints", share.len());
    Ok(())
}
