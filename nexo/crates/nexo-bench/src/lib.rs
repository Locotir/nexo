//! The benchmark of nexo (`nexo bench`): fixed tasks done by nexo's own agent loop under several
//! context policies (the arms), each run graded, priced and recorded, so the policies can be
//! compared on score, steps, tokens and money.

pub mod arm;
pub mod check;
pub mod client;
pub mod contam;
pub mod cost;
pub mod counters;
pub mod env;
pub mod html;
pub mod llmb;
pub mod polyglot;
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
    /// The model's context window in tokens, as a small model has; None: what nexo knows of it.
    pub window: Option<u32>,
    /// Sampling temperature of every request; None: the provider's own.
    pub temperature: Option<f32>,
    /// Run with `agent.close_on_verified` on.
    pub close_on_verified: bool,
    /// Seconds one run may take before it is interrupted and ends as `timeout`; 0: no limit.
    pub run_timeout: u64,
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
    /// The Polyglot benchmark checkout (task `polyglot`); None: the pinned commit is cloned into the cache.
    pub polyglot: Option<PathBuf>,
    /// The languages of the task `polyglot`; empty: python, rust, go, cpp and javascript.
    pub langs: Vec<String>,
}

/// The wall time one run may take, from `--run-timeout` seconds; 0: no limit.
pub fn run_limit(secs: u64) -> Option<std::time::Duration> {
    (secs > 0).then(|| std::time::Duration::from_secs(secs))
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
/// The sampling temperature of a run: the one given, else 0 when polyglot is among the tasks (Aider's harness
/// sends 0), else the provider's own.
pub fn temperature_of(given: Option<f32>, poly: bool) -> Option<f32> {
    given.or(poly.then_some(0.0))
}

pub async fn run(opts: Options) -> anyhow::Result<i32> {
    let arms = arm::Arm::parse_all(&opts.arms).map_err(anyhow::Error::msg)?;
    let (poly, rest) = take_polyglot(&opts.tasks);
    let temperature = temperature_of(opts.temperature, poly);
    let (llmb_names, nexo_names) = split_tasks(&rest);
    let (chats, nexo_names) = take_chats(&nexo_names);
    let tasks = if nexo_names.is_empty() && (!llmb_names.is_empty() || poly || !chats.is_empty()) {
        Vec::new()
    } else {
        tasks::select(&nexo_names).map_err(anyhow::Error::msg)?
    };
    let llmb_jobs = match (&opts.llmb, llmb_names.is_empty()) {
        (_, true) => Vec::new(),
        (None, false) => bail!("the tasks {} need --llmb DIR (the benchmark's distribution)", llmb_names.join(", ")),
        (Some(root), false) => {
            llmb::jobs(root, &llmb_names, &opts.instances, opts.limit).map_err(anyhow::Error::msg)?
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
    let cache = opts.cache.clone().unwrap_or_else(|| out.join(".cache").join("repos"));
    let mut poly_commit = None;
    let mut poly_root = None;
    let (poly_ctx, poly_jobs) = if poly {
        let langs = polyglot::parse_langs(&opts.langs).map_err(anyhow::Error::msg)?;
        seal::available().map_err(|e| anyhow::anyhow!("the task polyglot runs in a sealed box: {e}"))?;
        std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
        let cache = cache.canonicalize()?;
        let root = polyglot::root_of(opts.polyglot.as_deref(), &cache).map_err(anyhow::Error::msg)?;
        let jobs = polyglot::jobs(&root, &langs, &opts.instances, opts.limit).map_err(anyhow::Error::msg)?;
        poly_commit = polyglot::commit_of(&root);
        poly_root = Some(root.clone());
        let ctx = polyglot::Ctx { root, cache };
        polyglot::prepare(&ctx, &jobs).map_err(anyhow::Error::msg)?;
        (Some(ctx), jobs)
    } else {
        (None, Vec::new())
    };
    for w in &opts.instances {
        let named = |n: &str| n.ends_with(&format!("/{w}"));
        if !llmb_jobs.iter().any(|j| named(&j.name())) && !poly_jobs.iter().any(|j| named(&j.name())) {
            bail!("no instance {w:?} in the tasks {}", opts.tasks.join(", "));
        }
    }

    let protocol = opts.protocol.clone().unwrap_or_else(|| protocol_of(&opts.upstream).into());
    let mut provider = ProviderConfig::new(&protocol, &opts.upstream, opts.key.as_ref().map(|_| run::KEY_ENV));
    if let Some(w) = opts.window {
        provider.context_window.insert(opts.model.clone(), w);
    }
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
        temperature,
        out: out.clone(),
        hide,
        client,
        spent: Arc::default(),
        close_on_verified: opts.close_on_verified,
        run_timeout: run_limit(opts.run_timeout),
    };

    let ctx = llmb::Ctx {
        root: opts.llmb.as_ref().map(|r| r.canonicalize().unwrap_or_else(|_| r.clone())).unwrap_or_default(),
        cache: cache.clone(),
    };
    let units: Vec<Unit> = tasks
        .iter()
        .map(Unit::Nexo)
        .chain(chats.iter().map(Unit::Chat))
        .chain(llmb_jobs.iter().map(Unit::Llmb))
        .chain(poly_jobs.iter().map(Unit::Polyglot))
        .collect();
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
                    Unit::Chat(task) => tasks::padded::run_chat(&bench, task, a, rep, left).await,
                    Unit::Llmb(job) => llmb::run(&bench, &ctx, job, a, rep, left).await,
                    Unit::Polyglot(job) => match &poly_ctx {
                        Some(pc) => polyglot::run(&bench, pc, job, a, rep, left).await,
                        None => Err("polyglot has no context".to_string()),
                    },
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
    let stopped = stopped.or_else(|| budget_spent(opts.budget, &records));
    let header = report::Header {
        model: opts.model.clone(),
        upstream: opts.upstream.clone(),
        commit: run::COMMIT.into(),
        max_steps: opts.max_steps,
        max_tokens: opts.max_tokens,
        temperature,
        repeats: opts.repeats,
        budget: opts.budget,
        price: opts.price.map(|p| report::price_line(&p)),
        arm_order: Some(ARM_ORDER.into()),
        skipped,
        stopped: stopped.clone(),
        spent: Some(spent),
        repro: poly.then(|| repro_command(&opts, poly_root.as_deref().unwrap_or(&out), poly_commit.as_deref())),
    };
    let meta = Meta {
        results: Some(RESULTS.into()),
        budget: opts.budget,
        spent,
        stopped: stopped.clone(),
        skipped: header.skipped.clone(),
        repro: header.repro.clone(),
    };
    std::fs::write(out.join(Meta::FILE), serde_json::to_string(&meta)?)?;
    let summary = report::summary(&header, &records);
    std::fs::write(out.join("summary.md"), &summary)?;
    println!("{summary}");
    match html::write(&out, &records, Some(spent)) {
        Ok(p) => eprintln!("bench: report at {}", p.display()),
        Err(e) => eprintln!("bench: no report.html: {e}"),
    }
    Ok(if stopped.is_some() { 2 } else { 0 })
}

/// Why the benchmark stopped early when the loop ended on its own: the budget cut a run. Spend that
/// reached the budget with the last run's last reply left nothing undone, so it is no stop.
fn budget_spent(budget: Option<f64>, records: &[run::Record]) -> Option<String> {
    let b = budget?;
    let cut = records.iter().any(|r| r.stop == Some(client::Stop::Budget));
    cut.then(|| format!("the budget of ${b} is spent"))
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
    Chat(&'a tasks::padded::ChatTask),
    Llmb(&'a llmb::Job),
    Polyglot(&'a polyglot::Job),
}

impl Unit<'_> {
    fn name(&self) -> String {
        match self {
            Unit::Nexo(t) => t.id.to_string(),
            Unit::Chat(t) => t.id.to_string(),
            Unit::Llmb(j) => j.name(),
            Unit::Polyglot(j) => j.name(),
        }
    }
}

/// The task `polyglot` taken out of the names: whether it was asked for, and the other names.
pub fn take_polyglot(names: &[String]) -> (bool, Vec<String>) {
    let mut asked = false;
    let mut rest = Vec::new();
    for n in names.iter().flat_map(|n| n.split(',')).map(str::trim).filter(|n| !n.is_empty()) {
        if n == polyglot::NAME {
            asked = true;
        } else {
            rest.push(n.to_string());
        }
    }
    (asked, rest)
}

/// The command that repeats a Polyglot run; the key is read from `$API_KEY`.
fn repro_command(opts: &Options, root: &std::path::Path, commit: Option<&str>) -> String {
    let mut s = format!(
        "nexo bench --upstream {} --model {} --key \"$API_KEY\" --tasks polyglot --polyglot {} --max-steps {}",
        opts.upstream,
        opts.model,
        root.display(),
        opts.max_steps
    );
    if !opts.langs.is_empty() {
        s.push_str(&format!(" --langs {}", opts.langs.join(",")));
    }
    if let Some(n) = opts.limit {
        s.push_str(&format!(" --limit {n}"));
    }
    if !opts.instances.is_empty() {
        s.push_str(&format!(" --instances {}", opts.instances.join(",")));
    }
    if !opts.arms.is_empty() {
        s.push_str(&format!(" --arms {}", opts.arms.join(",")));
    }
    if let Some(b) = opts.budget {
        s.push_str(&format!(" --budget {b}"));
    }
    s.push_str(" --out DIR");
    if let Some(c) = commit {
        s.push_str(&format!("   # benchmark commit {c}"));
    }
    s
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

/// The chat tasks (no tools, graded on the answers) taken out of nexo's task names, and the other names.
/// They run only when named, so the groups keep their tasks.
pub fn take_chats(names: &[String]) -> (Vec<tasks::padded::ChatTask>, Vec<String>) {
    let mut chats: Vec<tasks::padded::ChatTask> = Vec::new();
    let mut rest = Vec::new();
    for n in names.iter().flat_map(|n| n.split(',')).map(str::trim).filter(|n| !n.is_empty()) {
        match tasks::padded::chat_tasks().into_iter().find(|c| c.id == n) {
            Some(c) if !chats.iter().any(|k| k.id == c.id) => chats.push(c),
            Some(_) => {}
            None => rest.push(n.to_string()),
        }
    }
    (chats, rest)
}

/// `nexo bench report`: report.html in `out` from the results.jsonl of `inputs` (default: `out`).
pub fn report(out: &std::path::Path, inputs: &[PathBuf], summary: Option<&std::path::Path>) -> anyhow::Result<i32> {
    let inputs = if inputs.is_empty() { vec![out.to_path_buf()] } else { inputs.to_vec() };
    // No model call: the counters are read again from the transcripts next to each results.jsonl.
    let records = counters::load_with_counters(&inputs).map_err(anyhow::Error::msg)?;
    let mut header = report::header_of(&records);
    // what the benchmarks themselves knew (spent with skipped runs, why they stopped), when every
    // input has it
    let metas: Option<Vec<Meta>> = inputs.iter().map(|p| Meta::read(p)).collect();
    if let Some(metas) = metas.filter(|m| !m.is_empty()) {
        header.spent = Some(metas.iter().map(|m| m.spent).sum());
        header.budget = metas.first().and_then(|m| m.budget).filter(|_| metas.len() == 1);
        header.stopped = metas.iter().find_map(|m| m.stopped.clone());
        header.repro = metas.iter().find_map(|m| m.repro.clone());
        header.skipped = metas.into_iter().flat_map(|m| m.skipped).collect();
    }
    if let Some(file) = summary {
        std::fs::write(file, report::summary(&header, &records))?;
        println!("{}: {} runs", file.display(), records.len());
    }
    let path = html::write(out, &records, header.spent)?;
    println!("{}: {} runs", path.display(), records.len());
    Ok(0)
}

/// The results file of a benchmark, in its `out` folder.
const RESULTS: &str = "results.jsonl";

/// What a benchmark knew that its records do not, kept in `bench.json` next to `results.jsonl` so
/// `nexo bench report` rebuilds the same header.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Meta {
    /// The results file these figures describe; a report of any other file does not use them.
    results: Option<String>,
    budget: Option<f64>,
    /// Every priced reply, runs skipped after an error included.
    spent: f64,
    stopped: Option<String>,
    skipped: Vec<String>,
    /// The command that repeats the run (Polyglot).
    repro: Option<String>,
}

impl Meta {
    const FILE: &'static str = "bench.json";

    /// The meta of the results at `p` (a results.jsonl or its folder), if it has one.
    fn read(p: &std::path::Path) -> Option<Meta> {
        let (dir, name) = if p.is_dir() { (p, RESULTS) } else { (p.parent()?, p.file_name()?.to_str()?) };
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(dir.join(Meta::FILE)).ok()?).ok()?;
        (meta.results.as_deref() == Some(name)).then_some(meta)
    }
}

/// Refuses an `out` that holds an earlier benchmark's runs: a new run would overwrite their
/// transcripts while their records stay, and the reports would mix the two.
fn fresh_out(out: &std::path::Path) -> anyhow::Result<()> {
    let file = out.join(RESULTS);
    if std::fs::read_to_string(&file).is_ok_and(|t| !t.trim().is_empty()) {
        bail!("{} already holds an earlier benchmark's runs; give another --out", file.display());
    }
    // a leftover of a run that wrote no records describes nothing of the run starting now
    match std::fs::remove_file(out.join(Meta::FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
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

    /// The tests that run bash without a session leave `$NEXO_TMP` of the process in the temp folder: it goes
    /// when the test binary exits. (Registered by a test, so a run that filters this one out does not sweep.)
    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_of_the_tests_goes_when_the_process_exits() {
        extern "C" {
            fn atexit(f: extern "C" fn()) -> i32;
        }
        extern "C" fn sweep() {
            nexo_tools::bash::wipe_tmp("");
        }
        // SAFETY: `sweep` is a plain `extern "C"` function that outlives the process.
        assert_eq!(unsafe { atexit(sweep) }, 0);
    }

    #[test]
    fn polyglot_runs_at_temperature_0_unless_told() {
        assert_eq!(temperature_of(None, true), Some(0.0), "as Aider's harness");
        assert_eq!(temperature_of(Some(0.7), true), Some(0.7));
        assert_eq!(temperature_of(None, false), None, "the provider's own");
    }

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
        assert!(budget_spent(Some(5.0), &[done(), cut]).is_some());
        assert!(budget_spent(Some(5.0), &[done()]).is_none(), "every run finished: nothing was left undone");
        assert!(budget_spent(None, &[done()]).is_none());
    }

    #[test]
    fn the_report_rebuilds_spent_and_the_early_stop_of_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let rec = run::Record { task: "t".into(), arm: "a".into(), cost: 1.75, ..Default::default() };
        run::append(dir.path(), &rec).unwrap();
        let meta = Meta {
            results: Some(RESULTS.into()),
            budget: Some(2.0),
            spent: 2.0,
            stopped: Some("the budget of $2 is spent".into()),
            skipped: vec!["x: no compiler".into()],
            repro: None,
        };
        std::fs::write(dir.path().join(Meta::FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        let file = dir.path().join("again.md");
        report(dir.path(), &[], Some(&file)).unwrap();
        let s = std::fs::read_to_string(&file).unwrap();
        assert!(s.contains("| $2 | **$2.0000** |") && s.contains("$0.2500 went to runs that were skipped"), "{s}");
        assert!(
            s.contains("**Stopped early**: the budget of $2 is spent.") && s.contains("Skipped: x: no compiler"),
            "{s}"
        );
        let html = std::fs::read_to_string(dir.path().join("report.html")).unwrap();
        assert!(html.contains("$2.0000") && html.contains("$0.2500"), "the page shows the same spent");
    }

    #[test]
    fn a_filtered_or_stale_file_does_not_take_the_folders_meta() {
        let dir = tempfile::tempdir().unwrap();
        let rec = run::Record { task: "t".into(), arm: "a".into(), cost: 1.0, ..Default::default() };
        run::append(dir.path(), &rec).unwrap();
        let meta = Meta { results: Some(RESULTS.into()), spent: 9.0, skipped: vec!["x".into()], ..Default::default() };
        std::fs::write(dir.path().join(Meta::FILE), serde_json::to_string(&meta).unwrap()).unwrap();
        let filtered = dir.path().join("cur.jsonl");
        std::fs::copy(dir.path().join(RESULTS), &filtered).unwrap();
        assert!(Meta::read(dir.path()).is_some() && Meta::read(&dir.path().join(RESULTS)).is_some());
        assert!(Meta::read(&filtered).is_none(), "a copy or a filter of the results is not the run");
        let file = dir.path().join("s.md");
        report(dir.path(), &[filtered], Some(&file)).unwrap();
        let s = std::fs::read_to_string(&file).unwrap();
        assert!(!s.contains("skipped") && !s.contains("$9"), "{s}");
        std::fs::write(dir.path().join(RESULTS), "").unwrap();
        fresh_out(dir.path()).unwrap();
        assert!(Meta::read(dir.path()).is_none(), "a new run drops the last one's bench.json");
    }

    #[test]
    fn llmb_task_names_are_split_off() {
        let (l, r) = split_tasks(&["repoqa,core".into(), "canonstep".into(), "repoqa".into()]);
        assert_eq!(l, vec!["repoqa", "canonstep"]);
        assert_eq!(r, vec!["core".to_string()]);
    }

    #[test]
    fn chat_task_names_are_split_off() {
        let (c, r) = take_chats(&["padded-revert-chat,core".into(), "padded-revert-chat".into()]);
        assert_eq!(c.iter().map(|t| t.id).collect::<Vec<_>>(), vec!["padded-revert-chat"]);
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
            window: None,
            temperature: None,
            close_on_verified: false,
            run_timeout: 0,
            out,
            skip_preflight: false,
            llmb: None,
            polyglot: None,
            langs: vec![],
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
