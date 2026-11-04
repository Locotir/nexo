//! The benchmark of nexo (`nexo bench`): fixed tasks done by nexo's own agent loop under several
//! context policies (the arms), each run graded, priced and recorded, so the policies can be
//! compared on score, steps, tokens and money.

pub mod arm;
pub mod check;
pub mod client;
pub mod cost;
pub mod counters;
pub mod env;
pub mod html;
pub mod llmb;
pub mod preflight;
pub mod report;
pub mod rng;
pub mod run;
pub mod seal;
pub mod stats;
pub mod tasks;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context as _};
use nexo_core::{Pricing, ProviderConfig};

/// What `nexo bench` was asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// The provider's address, e.g. `https://api.deepseek.com/anthropic`.
    pub upstream: String,
    pub model: String,
    pub key: Option<String>,
    /// The provider's protocol; None: guessed from the address.
    pub protocol: Option<String>,
    /// Task ids or groups (comma lists allowed); empty: every task.
    pub tasks: Vec<String>,
    /// Arms (`[NAME@]POLICY[:k=v;...]`, comma lists allowed); empty: the default arms.
    pub arms: Vec<String>,
    pub repeats: u32,
    pub max_steps: u32,
    /// Dollars after which the whole benchmark stops; it needs prices (`price`, or nexo's own for the model).
    pub budget: Option<f64>,
    /// The model's prices; None: nexo's own for the model, if it has any.
    pub price: Option<Pricing>,
    pub max_tokens: Option<u32>,
    /// Sampling temperature of every request; None: the provider's own.
    pub temperature: Option<f32>,
    pub out: PathBuf,
    pub skip_preflight: bool,
    /// The external benchmark's distribution, for the tasks `repoqa` and `canonstep`.
    pub llmb: Option<PathBuf>,
    /// Instance ids of those tasks; empty: every instance.
    pub instances: Vec<String>,
    /// At most this many instances per task (after sorting by id).
    pub limit: Option<usize>,
    /// Where repositories are cloned once; None: `<out>/.cache/repos`.
    pub cache: Option<PathBuf>,
}

/// The protocol an address most likely speaks: `anthropic` when the address says so (DeepSeek's
/// and others' Anthropic-compatible endpoints), `gemini` for Google's, else OpenAI chat.
pub fn protocol_of(upstream: &str) -> &'static str {
    let u = upstream.to_ascii_lowercase();
    if u.contains("anthropic") {
        "anthropic"
    } else if u.contains("generativelanguage.googleapis.com") {
        "gemini"
    } else {
        "openai-chat"
    }
}

/// The model's prices: `--price`, else nexo's own; an error when a budget is set and there are none, because
/// the budget could not stop the run.
fn prices(opts: &Options) -> anyhow::Result<Option<Pricing>> {
    let priced = opts.price.or_else(|| cost::price_of(&opts.model));
    if opts.budget.is_some() && priced.is_none() {
        bail!(
            "--budget cannot stop a run without prices, and nexo has none for {:?}: give them with --price MISS,HIT,OUT \
             (dollars per million tokens: input on a cache miss, input on a cache hit, output)",
            opts.model
        );
    }
    Ok(priced)
}

/// Runs the benchmark; the exit code is 0, or 2 when it stopped at the budget.
pub async fn run(opts: Options) -> anyhow::Result<i32> {
    let arms = arm::Arm::parse_all(&opts.arms).map_err(anyhow::Error::msg)?;
    let (llmb_names, nexo_names) = split_tasks(&opts.tasks);
    let tasks = if nexo_names.is_empty() && !llmb_names.is_empty() {
        Vec::new()
    } else {
        tasks::select(&nexo_names).map_err(anyhow::Error::msg)?
    };
    let llmb_jobs = match (&opts.llmb, llmb_names.is_empty()) {
        (_, true) => Vec::new(),
        (None, false) => bail!("the tasks {} need --llmb DIR (the benchmark's distribution)", llmb_names.join(", ")),
        (Some(root), false) => {
            let jobs = llmb::jobs(root, &llmb_names, &opts.instances, opts.limit).map_err(anyhow::Error::msg)?;
            for w in &opts.instances {
                if !jobs.iter().any(|j| j.name().ends_with(&format!("/{w}"))) {
                    bail!("no instance {w:?} in the tasks {}", llmb_names.join(", "));
                }
            }
            jobs
        }
    };
    if opts.repeats == 0 || opts.max_steps == 0 {
        bail!("--repeats and --max-steps must be at least 1");
    }
    let priced = prices(&opts)?;
    std::fs::create_dir_all(&opts.out).with_context(|| format!("creating {}", opts.out.display()))?;
    let out = opts.out.canonicalize()?;
    fresh_out(&out)?;
    isolate(&out, opts.key.as_deref())?;

    let protocol = opts.protocol.clone().unwrap_or_else(|| protocol_of(&opts.upstream).into());
    let provider = ProviderConfig::new(&protocol, &opts.upstream, opts.key.as_ref().map(|_| run::KEY_ENV));
    let client: Arc<dyn nexo_agent::ModelClient> = Arc::new(nexo_providers::Client::new(nexo_providers::registry()));
    if !opts.skip_preflight {
        let n = opts.max_tokens.unwrap_or(4096);
        match preflight::check(client.as_ref(), &provider, &opts.model, n).await {
            Ok(what) => eprintln!("preflight: {what}"),
            Err(why) => bail!("preflight failed ({}): {why}\n(--skip-preflight starts anyway)", opts.model),
        }
    }
    let (price, estimated) = (priced.unwrap_or_default(), priced.is_none());
    if estimated {
        eprintln!("bench: no prices known for {:?}; costs are unknown (--price MISS,HIT,OUT gives them)", opts.model);
    }
    let mut hide = vec![out.clone()];
    hide.extend(std::env::current_dir().ok().and_then(|d| d.canonicalize().ok()));
    let bench = run::Bench {
        provider,
        model: opts.model.clone(),
        price,
        estimated,
        max_steps: opts.max_steps,
        max_tokens: opts.max_tokens,
        temperature: opts.temperature,
        out: out.clone(),
        hide,
        client,
        spent: Arc::default(),
    };

    let ctx = llmb::Ctx {
        root: opts.llmb.as_ref().map(|r| r.canonicalize().unwrap_or_else(|_| r.clone())).unwrap_or_default(),
        cache: opts.cache.clone().unwrap_or_else(|| out.join(".cache").join("repos")),
    };
    let units: Vec<Unit> = tasks.iter().map(Unit::Nexo).chain(llmb_jobs.iter().map(Unit::Llmb)).collect();
    let mut records = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut stopped = None;
    'all: for rep in 0..opts.repeats {
        for (ui, unit) in units.iter().enumerate() {
            let order = arm_order(&arms, rep, ui);
            for a in &order {
                // every reply priced so far, those of runs that ended in an error included
                let spent = *bench.spent.lock().unwrap();
                let left = opts.budget.map_or(f64::INFINITY, |b| b - spent);
                if left <= 0.0 {
                    stopped = Some(format!("the budget of ${} is spent", opts.budget.unwrap_or_default()));
                    break 'all;
                }
                eprintln!("bench: {} with {} (repeat {rep})", unit.name(), a.name);
                let done = match unit {
                    Unit::Nexo(task) => run::run(&bench, task, a, rep, left).await,
                    Unit::Llmb(job) => llmb::run(&bench, &ctx, job, a, rep, left).await,
                };
                match done {
                    Ok(rec) => {
                        eprintln!(
                            "bench: {} {}: score {:.2}% in {} steps, ${:.4}",
                            rec.task,
                            rec.arm,
                            rec.score * 100.0,
                            rec.steps,
                            rec.cost
                        );
                        run::append(&out, &rec)?;
                        records.push(rec);
                    }
                    Err(why) => {
                        let note = format!("{}: {why}", unit.name());
                        eprintln!("bench: skipped {note}");
                        if !skipped.contains(&note) {
                            skipped.push(note);
                        }
                    }
                }
            }
        }
    }
    let spent = *bench.spent.lock().unwrap();
    let stopped = stopped.or_else(|| budget_spent(opts.budget, spent, &records));
    let header = report::Header {
        model: opts.model.clone(),
        upstream: opts.upstream.clone(),
        commit: run::COMMIT.into(),
        max_steps: opts.max_steps,
        max_tokens: opts.max_tokens,
        temperature: opts.temperature,
        repeats: opts.repeats,
        budget: opts.budget,
        price: opts.price.map(|p| report::price_line(&p)),
        arm_order: Some(ARM_ORDER.into()),
        skipped,
        stopped: stopped.clone(),
        spent: Some(spent),
    };
    let summary = report::summary(&header, &records);
    std::fs::write(out.join("summary.md"), &summary)?;
    println!("{summary}");
    match html::write(&out, &records, Some(spent)) {
        Ok(p) => eprintln!("bench: report at {}", p.display()),
        Err(e) => eprintln!("bench: no report.html: {e}"),
    }
    Ok(if stopped.is_some() { 2 } else { 0 })
}

/// Why the benchmark stopped early when the loop ended on its own: the budget ran out during the
/// last run (that run was cut, or everything priced reached the budget).
fn budget_spent(budget: Option<f64>, spent: f64, records: &[run::Record]) -> Option<String> {
    let b = budget?;
    let cut = records.iter().any(|r| r.stop == Some(client::Stop::Budget));
    (cut || spent >= b).then(|| format!("the budget of ${b} is spent"))
}

/// How the report says the arms were ordered.
pub const ARM_ORDER: &str = "shuffled for every task and repeat (fixed seed), so no arm always goes first and \
the provider cache is shared evenly";

/// The arms in the order they run for repeat `rep` of unit `unit`: a seeded shuffle, because the
/// provider caches the prefix the arms share and the first arm to send it pays the miss.
pub fn arm_order<T>(arms: &[T], rep: u32, unit: usize) -> Vec<&T> {
    let mut order: Vec<&T> = arms.iter().collect();
    let mut rng = rng::Rng::new(0x6f72_6465 ^ (u64::from(rep) << 32) ^ unit as u64);
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }
    order
}

/// What one iteration runs: one of nexo's tasks, or an instance of the external benchmark.
enum Unit<'a> {
    Nexo(&'a tasks::Task),
    Llmb(&'a llmb::Job),
}

impl Unit<'_> {
    fn name(&self) -> String {
        match self {
            Unit::Nexo(t) => t.id.to_string(),
            Unit::Llmb(j) => j.name(),
        }
    }
}

/// The task names split into the external benchmark's (`repoqa`, `canonstep`) and nexo's own.
pub fn split_tasks(names: &[String]) -> (Vec<&'static str>, Vec<String>) {
    let mut llmb_names = Vec::new();
    let mut rest = Vec::new();
    for n in names.iter().flat_map(|n| n.split(',')).map(str::trim).filter(|n| !n.is_empty()) {
        match llmb::NAMES.iter().find(|k| **k == n) {
            Some(k) if !llmb_names.contains(k) => llmb_names.push(*k),
            Some(_) => {}
            None => rest.push(n.to_string()),
        }
    }
    (llmb_names, rest)
}

/// `nexo bench report`: report.html in `out` from the results.jsonl of `inputs` (default: `out`).
pub fn report(out: &std::path::Path, inputs: &[PathBuf], summary: Option<&std::path::Path>) -> anyhow::Result<i32> {
    let inputs = if inputs.is_empty() { vec![out.to_path_buf()] } else { inputs.to_vec() };
    // No model call: the counters are read again from the transcripts next to each results.jsonl.
    let records = counters::load_with_counters(&inputs).map_err(anyhow::Error::msg)?;
    if let Some(file) = summary {
        std::fs::write(file, report::summary(&report::header_of(&records), &records))?;
        println!("{}: {} runs", file.display(), records.len());
    }
    let path = html::write(out, &records, None)?;
    println!("{}: {} runs", path.display(), records.len());
    Ok(0)
}

/// Refuses an `out` that holds an earlier benchmark's runs: a new run would overwrite their
/// transcripts while their records stay, and the reports would mix the two.
fn fresh_out(out: &std::path::Path) -> anyhow::Result<()> {
    let file = out.join("results.jsonl");
    if std::fs::read_to_string(&file).is_ok_and(|t| !t.trim().is_empty()) {
        bail!("{} already holds an earlier benchmark's runs; give another --out", file.display());
    }
    Ok(())
}

/// Keeps the benchmark's agents away from the person's own nexo: settings, sessions and caches live
/// under `<out>/.nexo`, the curator is the built-in one, and the key reaches the provider through an
/// environment variable of this process only.
fn isolate(out: &std::path::Path, key: Option<&str>) -> anyhow::Result<()> {
    let home = out.join(".nexo");
    for (var, dir) in [("NEXO_CONFIG_DIR", "config"), ("NEXO_DATA_DIR", "data"), ("NEXO_CACHE_DIR", "cache")] {
        let d = home.join(dir);
        std::fs::create_dir_all(&d)?;
        std::env::set_var(var, d);
    }
    std::env::remove_var("NEXO_CURATOR");
    std::env::remove_var("NEXO_CURATOR_PATH");
    if let Some(k) = key {
        std::env::set_var(run::KEY_ENV, k);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_out_folder_with_earlier_results_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(fresh_out(dir.path()).is_ok());
        std::fs::write(dir.path().join("results.jsonl"), "").unwrap();
        assert!(fresh_out(dir.path()).is_ok(), "an empty file holds no runs");
        std::fs::write(dir.path().join("results.jsonl"), "{}\n").unwrap();
        assert!(fresh_out(dir.path()).unwrap_err().to_string().contains("results.jsonl"));
    }

    #[test]
    fn a_budget_cut_in_the_last_run_still_stops_early() {
        let cut = run::Record { stop: Some(client::Stop::Budget), ..Default::default() };
        let done = run::Record::default;
        assert!(budget_spent(Some(5.0), 4.2, &[done(), cut]).is_some());
        assert!(budget_spent(Some(5.0), 5.0, &[done()]).is_some());
        assert!(budget_spent(Some(5.0), 4.2, &[done()]).is_none());
        assert!(budget_spent(None, 9.0, &[done()]).is_none());
    }

    #[test]
    fn llmb_task_names_are_split_off() {
        let (l, r) = split_tasks(&["repoqa,core".into(), "canonstep".into(), "repoqa".into()]);
        assert_eq!(l, vec!["repoqa", "canonstep"]);
        assert_eq!(r, vec!["core".to_string()]);
    }

    #[test]
    fn the_arm_order_is_shuffled_per_task_and_repeat_and_repeatable() {
        let arms = [0, 1, 2];
        let mut first = [0; 3];
        for rep in 0..60 {
            let o = arm_order(&arms, rep, 0);
            assert_eq!(o, arm_order(&arms, rep, 0));
            let mut sorted: Vec<i32> = o.iter().map(|x| **x).collect();
            sorted.sort();
            assert_eq!(sorted, vec![0, 1, 2]);
            first[*o[0] as usize] += 1;
        }
        assert!(first.iter().all(|&n| n >= 10), "every arm goes first often enough: {first:?}");
    }

    #[test]
    fn the_protocol_is_guessed_from_the_address() {
        assert_eq!(protocol_of("https://api.deepseek.com/anthropic"), "anthropic");
        assert_eq!(protocol_of("https://generativelanguage.googleapis.com/v1beta"), "gemini");
        assert_eq!(protocol_of("http://localhost:8000/v1"), "openai-chat");
    }

    fn opts(model: &str, budget: Option<f64>, price: Option<Pricing>, out: PathBuf) -> Options {
        Options {
            upstream: "http://127.0.0.1:9".into(),
            model: model.into(),
            key: None,
            protocol: None,
            tasks: vec![],
            arms: vec![],
            repeats: 1,
            max_steps: 1,
            budget,
            price,
            max_tokens: None,
            temperature: None,
            out,
            skip_preflight: false,
            llmb: None,
            instances: vec![],
            limit: None,
            cache: None,
        }
    }

    #[test]
    fn a_budget_needs_prices() {
        let out = PathBuf::from("unused");
        let err = prices(&opts("no-such-model", Some(1.0), None, out.clone())).unwrap_err().to_string();
        assert!(err.contains("--budget") && err.contains("--price"), "{err}");
        assert_eq!(
            prices(&opts("no-such-model", None, None, out.clone())).unwrap(),
            None,
            "no budget: unknown costs are fine"
        );
        let p = Pricing { input: 0.1, output: 0.2, cache_read: 0.01, cache_write: 0.0 };
        assert_eq!(prices(&opts("no-such-model", Some(1.0), Some(p), out.clone())).unwrap(), Some(p));
        assert_eq!(prices(&opts("deepseek-chat", Some(1.0), Some(p), out.clone())).unwrap(), Some(p), "--price wins");
        assert!(prices(&opts("deepseek-chat", Some(1.0), None, out.clone())).unwrap().is_some());
    }

    #[tokio::test]
    async fn a_budget_without_prices_stops_before_anything_is_written() {
        let dir = std::env::temp_dir().join(format!("nexo-bench-budget-{}", std::process::id()));
        let err = run(opts("no-such-model", Some(1.0), None, dir.clone())).await.unwrap_err().to_string();
        assert!(err.contains("--budget"), "{err}");
        assert!(!dir.exists(), "nothing is written before the refusal");
    }
}
