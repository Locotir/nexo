//! The session simulator: `sim events <input>... --spec S [--price live|legacy] [--life S] [--slots K] [--json]`.
//!
//! Each input is a golden fixture or a `nexo bench` transcript (told apart by the first line). Every request
//! goes through a curator with the spec; the events are the places where the view lacks content the recorded
//! trajectory depends on (`cited`, `denied`, `blind`, see `nexo_curator::sim::events`), and the cost is the
//! replay's: a prefix cache with one slot (`--life S` keeps every request of the last S seconds, `--slots K`
//! the last K) priced with `--price` (`live`: deepseek-flash 0.3/0.006/1.2 $ per M, `legacy`: the replay's
//! 0.28/0.028, input only).

use nexo_curator::sim::behave::{analytic, auto_family, monte_carlo, probe, Context, ParamSet};
use nexo_curator::sim::events::{run, Run};
use nexo_curator::sim::input::load;
use nexo_curator::sim::price::Prices;
use serde_json::{json, Value};

fn usage() -> ! {
    eprintln!("usage: sim events INPUT... --spec SPEC [--price live|legacy] [--life S] [--slots K] [--json]");
    eprintln!("       sim search [--tier fast|mid|all] [--configs N] [--workers W] [--seed X] [--params P] [--stage lhs|coord|confirm --from FILE] (see sim search --help)");
    eprintln!("       sim run INPUT... --spec SPEC [--params P] [--family auto|binary|http|logs|code|chat] [--mc N --seed X] [--max-steps 40] [--price live|legacy] [--life S] [--slots K] [--r R --beta B --phi F --pi P --mu M --r-den D] [--json]");
    eprintln!("       sim fit --runs DIR... [--holdout odd|even|none] [--out params.json] [--model M|all] [--min-events N] [--seed X]");
    eprintln!("       sim predict --results R --arm plain --spec S --params P [--life S] [--slots K]");
    eprintln!("       sim check --results R --params P --spec S [--holdout even|odd|none] [--report FILE] [--life S] [--slots K]");
    eprintln!("       sim synth [--seed S] [--steps N] [--family binary|asm|http|logs|code|chat] [--files F] [--hex-share H] [--log-share L] [--lag-mean M] [--recite P] --out X.synth.jsonl");
    eprintln!("       sim family INPUT...");
    eprintln!("       sim report --runs DIR... [--results R --params P --spec S] [--pareto pareto.tsv] [--model M] [--min-events N] [--out DIR] [--write-weights FILE]");
    eprintln!("       sim price --runs DIR... [--life S] [--slots K]");
    std::process::exit(2);
}

fn count(r: &Run, kind: &str) -> usize {
    r.events.iter().filter(|e| e.kind.name() == kind).count()
}

fn row(path: &str, r: &Run) -> Value {
    json!({
        "input": path, "requests": r.requests,
        "events": { "cited": count(r, "cited"), "denied": count(r, "denied"), "blind": count(r, "blind") },
        "forgotten": r.forgotten, "replies": r.replies, "copied": r.copied,
        "miss": r.miss, "hit": r.hit, "out": r.out, "views": r.views,
        "blind_tokens": [r.blind.0, r.blind.1], "denied_tokens": [r.denied.0, r.denied.1],
        "input_cost": r.input_cost, "cost": r.cost,
        "list": r.events.iter().map(|e| json!({
            "t": e.t, "mi": e.mi, "kind": e.kind.name(), "s_e": e.s_e, "tool": e.tool.name(),
            "s_rest": e.s_rest, "words": e.words,
        })).collect::<Vec<_>>(),
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("events") => events_cmd(args),
        Some("run") => run_cmd(args),
        Some("fit") => fit_cmd(args),
        Some("predict") => predict_cmd(args),
        Some("check") => check_cmd(args),
        Some("price") => price_cmd(args),
        Some("synth") => synth_cmd(args),
        Some("family") => family_cmd(args),
        Some("report") => report_cmd(args),
        Some("search") => {
            if let Err(e) = nexo_curator::sim::search::cli(args) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        _ => usage(),
    }
}

fn events_cmd(mut args: impl Iterator<Item = String>) {
    let (mut inputs, mut spec, mut price, mut life, mut slots, mut json_out) =
        (Vec::new(), String::new(), "live".to_string(), 0.0f64, 0usize, false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--spec" => spec = args.next().unwrap_or_else(|| usage()),
            "--price" => price = args.next().unwrap_or_else(|| usage()),
            "--life" => life = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--slots" => slots = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--json" => json_out = true,
            _ if a.starts_with("--") => usage(),
            _ => inputs.push(a),
        }
    }
    let Some(prices) = Prices::named(&price) else { usage() };
    if inputs.is_empty() {
        usage();
    }
    let mut pooled = Run::default();
    for path in &inputs {
        let session = load(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        let r = run(&session.requests, &spec, prices, life, slots).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        });
        if json_out {
            println!("{}", row(path, &r));
        } else {
            println!(
                "{path}: {} requests, events {} (cited {}, denied {}, blind {}), forgotten {} words in {} replies, miss {} hit {}, cost {:.6} $",
                r.requests,
                r.events.len(),
                count(&r, "cited"),
                count(&r, "denied"),
                count(&r, "blind"),
                r.forgotten,
                r.replies,
                r.miss,
                r.hit,
                r.cost
            );
        }
        pooled.requests += r.requests;
        pooled.replies += r.replies;
        pooled.forgotten += r.forgotten;
        pooled.copied += r.copied;
        pooled.miss += r.miss;
        pooled.hit += r.hit;
        pooled.out += r.out;
        pooled.blind = (pooled.blind.0 + r.blind.0, pooled.blind.1 + r.blind.1);
        pooled.denied = (pooled.denied.0 + r.denied.0, pooled.denied.1 + r.denied.1);
        pooled.input_cost += r.input_cost;
        pooled.cost += r.cost;
        pooled.events.extend(r.events);
    }
    if json_out {
        println!("{}", row("total", &pooled));
    } else {
        println!(
            "total: {} requests, events {} (cited {}, denied {}, blind {}), forgotten {} words, cost {:.6} $ ({:.6} $ input)",
            pooled.requests,
            pooled.events.len(),
            count(&pooled, "cited"),
            count(&pooled, "denied"),
            count(&pooled, "blind"),
            pooled.forgotten,
            pooled.cost,
            pooled.input_cost
        );
    }
}

/// `sim run`: the open-loop cost plus the behaviour layer's expectation (and, with `--mc`, its Monte Carlo).
fn run_cmd(mut args: impl Iterator<Item = String>) {
    let (mut inputs, mut spec, mut price, mut life, mut slots, mut json_out) =
        (Vec::new(), "directives=0;nudge=0".to_string(), "live".to_string(), 0.0f64, 0usize, false);
    let (mut params_path, mut family, mut mc, mut seed, mut max_steps) =
        (None::<String>, "auto".to_string(), 0usize, 1u64, 40usize);
    let mut over: [Option<f64>; 6] = [None; 6];
    let num = |a: Option<String>| a.and_then(|n| n.parse::<f64>().ok()).unwrap_or_else(|| usage());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--spec" => spec = args.next().unwrap_or_else(|| usage()),
            "--params" => params_path = Some(args.next().unwrap_or_else(|| usage())),
            "--family" => family = args.next().unwrap_or_else(|| usage()),
            "--price" => price = args.next().unwrap_or_else(|| usage()),
            "--life" => life = num(args.next()),
            "--slots" => slots = num(args.next()) as usize,
            "--mc" => mc = num(args.next()) as usize,
            "--seed" => seed = num(args.next()) as u64,
            "--max-steps" => max_steps = num(args.next()) as usize,
            "--r" => over[0] = Some(num(args.next())),
            "--beta" => over[1] = Some(num(args.next())),
            "--phi" => over[2] = Some(num(args.next())),
            "--pi" => over[3] = Some(num(args.next())),
            "--mu" => over[4] = Some(num(args.next())),
            "--r-den" => over[5] = Some(num(args.next())),
            "--json" => json_out = true,
            _ if a.starts_with("--") => usage(),
            _ => inputs.push(a),
        }
    }
    let Some(prices) = Prices::named(&price) else { usage() };
    if inputs.is_empty() {
        usage();
    }
    let mut set = match &params_path {
        Some(p) => ParamSet::load(p).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        }),
        None => ParamSet::default(),
    };
    for p in set.families.values_mut().chain(std::iter::once(&mut set.global)) {
        p.r = over[0].unwrap_or(p.r);
        p.beta = over[1].unwrap_or(p.beta);
        p.phi = over[2].unwrap_or(p.phi);
        p.pi = over[3].unwrap_or(p.pi);
        p.mu = over[4].unwrap_or(p.mu);
    }
    set.r_den = over[5].unwrap_or(set.r_den);
    let (mut tot_cost, mut tot_steps, mut tot_extra, mut tot_ok, mut tot_events, mut tot_req) =
        (0.0, 0.0, 0.0, 1.0, 0usize, 0usize);
    for path in &inputs {
        let session = load(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
        let fam = if family == "auto" { auto_family(&session.requests) } else { family.as_str() };
        let p = set.get(fam);
        let mut r = run(&session.requests, &spec, prices, life, slots).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        });
        let cx = Context::measure(&session.requests, &spec, prices, max_steps);
        if let Err(e) = probe(&mut r, &session.requests, &spec, life, slots) {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
        let a = analytic(&r, p, set.r_den, &cx);
        let m = (mc > 0).then(|| {
            monte_carlo(&session.requests, &spec, &cx, life, slots, p, set.r_den, mc, seed).unwrap_or_else(|e| {
                eprintln!("{path}: {e}");
                std::process::exit(1);
            })
        });
        let ev = |k: &str| r.events.iter().filter(|e| e.kind.name() == k).count();
        if json_out {
            let mut row = json!({
                "input": path, "family": fam, "requests": r.requests, "cost": r.cost,
                "extra_steps": a.extra_steps, "extra_cost": a.extra_cost, "p_fail": a.p_fail, "p_cap": a.p_cap,
                "events": { "cited": ev("cited"), "denied": ev("denied"), "blind": ev("blind") },
                "params": { "r": p.r, "beta": p.beta, "phi": p.phi, "pi": p.pi, "mu": p.mu, "r_den": set.r_den },
                "max_steps": max_steps, "c_out": cx.c_out,
            });
            if let Some(m) = &m {
                row["mc"] = json!({
                    "samples": m.samples, "extra_steps": m.extra_steps, "extra_cost": m.extra_cost,
                    "p_fail": m.p_fail, "p_cap": m.p_cap, "steps_sd": m.steps_sd, "events": m.events,
                    "wall_secs": m.wall_secs,
                });
            }
            println!("{row}");
        } else {
            println!(
                "{path}: [{fam}] {} requests, cost {:.6} $, E[extra_steps] {:.3}, E[extra_cost] {:.6} $, P[fail] {:.3}, events cited {} denied {} blind {}",
                r.requests, r.cost, a.extra_steps, a.extra_cost, a.p_fail, ev("cited"), ev("denied"), ev("blind")
            );
            if let Some(m) = &m {
                println!(
                    "  mc {}: extra_steps {:.3}, extra_cost {:.6} $, P[fail] {:.3}, {:.2} s",
                    m.samples, m.extra_steps, m.extra_cost, m.p_fail, m.wall_secs
                );
            }
        }
        tot_cost += r.cost;
        tot_steps += a.extra_steps;
        tot_extra += a.extra_cost;
        tot_ok *= 1.0 - a.p_fail;
        tot_events += a.events;
        tot_req += r.requests;
    }
    let n = inputs.len() as f64;
    if json_out {
        println!(
            "{}",
            json!({"input": "pooled", "inputs": inputs.len(), "requests": tot_req, "cost": tot_cost,
                "extra_steps": tot_steps, "extra_cost": tot_extra, "p_fail_mean": 1.0 - tot_ok.powf(1.0 / n), "events": tot_events})
        );
    } else {
        println!(
            "pooled: {} inputs, {tot_req} requests, cost {tot_cost:.6} $, E[extra_steps] {tot_steps:.3}, E[extra_cost] {tot_extra:.6} $, mean P[fail] {:.3}, {tot_events} events",
            inputs.len(),
            1.0 - tot_ok.powf(1.0 / n)
        );
    }
}

fn die(e: String) -> ! {
    eprintln!("{e}");
    std::process::exit(1);
}

fn today() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

/// The cache the price model is checked with: how many requests it keeps (see `sim price`).
const SLOTS: usize = 0;

/// `sim fit`: `params.json` from run directories.
fn fit_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::fit::{fit_dirs, FitConfig, Holdout};
    let (mut runs, mut out, mut cfg) = (Vec::new(), None::<String>, FitConfig::default());
    let mut in_runs = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => in_runs = true,
            "--holdout" => {
                in_runs = false;
                cfg.holdout = args.next().and_then(|h| Holdout::parse(&h)).unwrap_or_else(|| usage());
            }
            "--out" => {
                in_runs = false;
                out = Some(args.next().unwrap_or_else(|| usage()));
            }
            "--model" => {
                in_runs = false;
                let m = args.next().unwrap_or_else(|| usage());
                cfg.model = (m != "all").then_some(m);
            }
            "--min-events" => cfg.min_events = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--seed" => cfg.seed = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            _ if a.starts_with("--") => usage(),
            _ if in_runs => runs.push(a),
            _ => usage(),
        }
    }
    if runs.is_empty() {
        usage();
    }
    let fit = fit_dirs(&runs, &cfg).unwrap_or_else(|e| die(e));
    print!("{}", fit.table());
    if let Some(out) = out {
        let v = fit.params_json(&today());
        if let Some(d) = std::path::Path::new(&out).parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(&out, serde_json::to_string_pretty(&v).unwrap() + "\n")
            .unwrap_or_else(|e| die(format!("{out}: {e}")));
        println!("wrote {out}");
    }
}

fn load_set(path: &str) -> ParamSet {
    ParamSet::load(path).unwrap_or_else(|e| die(e))
}

/// `sim predict`: the plain runs of a results file through the curator and the behaviour layer.
fn predict_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::fit::predict;
    let (mut results, mut arm, mut spec, mut params, mut life, mut slots) =
        (None::<String>, "plain".to_string(), "directives=0;nudge=0".to_string(), None::<String>, 0.0f64, SLOTS);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--results" => results = args.next(),
            "--arm" => arm = args.next().unwrap_or_else(|| usage()),
            "--spec" => spec = args.next().unwrap_or_else(|| usage()),
            "--params" => params = args.next(),
            "--life" => life = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--slots" => slots = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let (Some(results), Some(params)) = (results, params) else { usage() };
    let set = load_set(&params);
    let preds = predict(&results, &arm, &spec, &set, life, slots).unwrap_or_else(|e| die(e));
    println!(
        "{:<22} {:<7} {:>4} {:>7} {:>7} {:>11} {:>11} {:>7}",
        "task", "family", "rep", "events", "steps", "extra steps", "cost plain", "P[fail]"
    );
    for p in &preds {
        println!(
            "{:<22} {:<7} {:>4} {:>7} {:>7} {:>11.3} {:>11.6} {:>7.3}   cur cost {:.6}",
            p.task, p.family, p.rep, p.events, p.steps_plain, p.extra_steps, p.cost_plain, p.p_fail, p.cost_cur
        );
    }
}

/// `sim check`: the section 5.2 table on the held-out half; writes the markdown report.
fn check_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::fit::{check, Holdout};
    let (mut results, mut params, mut spec, mut holdout, mut report, mut life, mut slots) = (
        None::<String>,
        None::<String>,
        "directives=0;nudge=0".to_string(),
        Holdout::Even,
        "sim-check.md".to_string(),
        0.0f64,
        SLOTS,
    );
    while let Some(a) = args.next() {
        match a.as_str() {
            "--results" => results = args.next(),
            "--params" => params = args.next(),
            "--spec" => spec = args.next().unwrap_or_else(|| usage()),
            "--holdout" => holdout = args.next().and_then(|h| Holdout::parse(&h)).unwrap_or_else(|| usage()),
            "--report" => report = args.next().unwrap_or_else(|| usage()),
            "--life" => life = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--slots" => slots = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let (Some(results), Some(params)) = (results, params) else { usage() };
    let set = load_set(&params);
    let c = check(&results, &spec, &set, holdout, life, slots).unwrap_or_else(|e| die(e));
    print!("{}", c.table());
    if let Some(d) = std::path::Path::new(&report).parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(&report, c.markdown(&results, &params, &spec, &today()))
        .unwrap_or_else(|e| die(format!("{report}: {e}")));
    println!("wrote {report}");
}

/// `sim price`: the price model on the plain arms of run directories.
fn price_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::fit::{load_rows, price_check, price_medians};
    let (mut runs, mut life, mut slots) = (Vec::new(), 0.0f64, SLOTS);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => {}
            "--life" => life = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            "--slots" => slots = args.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| usage()),
            _ if a.starts_with("--") => usage(),
            _ => runs.push(a),
        }
    }
    let rows = load_rows(&runs).unwrap_or_else(|e| die(e));
    let pr = price_check(&rows, life, slots, None);
    for p in &pr {
        println!(
            "{}: recorded {:.6} $ predicted {:.6} $ ({:+.1} %), cache_read recorded {} predicted {:.0} ({:+.1} %), own scale {:.3}",
            p.path.rsplit('/').next().unwrap_or(&p.path),
            p.recorded_cost,
            p.predicted_cost,
            100.0 * (p.predicted_cost - p.recorded_cost) / p.recorded_cost,
            p.recorded_cache,
            p.predicted_cache,
            100.0 * (p.predicted_cache - p.recorded_cache as f64) / (p.recorded_cache as f64).max(1.0),
            p.scale
        );
    }
    let (c, k) = price_medians(&pr);
    println!("{} plain runs (token scale per run, from its own recorded usage): median cost error {:.1} %, median cache_read error {:.1} %", pr.len(), 100.0 * c, 100.0 * k);
}

/// `sim synth`: a synthetic session for stress tests, with its planted dependencies beside it.
fn synth_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::synth::{write, Synth, FAMILIES};
    let mut p = Synth::new(1, 60, "binary");
    let mut out = None::<String>;
    let num = |a: Option<String>| a.and_then(|n| n.parse::<f64>().ok()).unwrap_or_else(|| usage());
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seed" => p.seed = num(args.next()) as u64,
            "--steps" => p.steps = num(args.next()) as usize,
            "--family" => p.family = args.next().unwrap_or_else(|| usage()),
            "--files" => p.files = num(args.next()) as usize,
            "--hex-share" => p.hex_share = num(args.next()),
            "--log-share" => p.log_share = num(args.next()),
            "--lag-mean" => p.lag_mean = num(args.next()),
            "--recite" => p.recite = num(args.next()),
            "--out" => out = args.next(),
            _ => usage(),
        }
    }
    let Some(out) = out else { usage() };
    if !FAMILIES.contains(&p.family.as_str()) {
        die(format!("unknown family {}: one of {}", p.family, FAMILIES.join(", ")));
    }
    if !out.ends_with(".synth.jsonl") {
        die(format!("{out}: a synthetic session is written to a file named *.synth.jsonl"));
    }
    let g = write(&p, &out).unwrap_or_else(|e| die(e));
    println!("wrote {out}: {} requests, {} bytes, {} planted dependencies", p.steps, g.jsonl.len(), g.deps.len());
}

/// `sim report`: the per-family table (`SIMULATOR-SPEC.md` 7.4).
fn report_cmd(mut args: impl Iterator<Item = String>) {
    use nexo_curator::sim::report::{build, weights_toml, ReportInput};
    let mut inp = ReportInput::default();
    let (mut out, mut weights) = (None::<String>, None::<String>);
    let mut in_runs = false;
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_else(|| usage());
        if a != "--runs" && a.starts_with("--") {
            in_runs = false;
        }
        match a.as_str() {
            "--runs" => in_runs = true,
            "--results" => inp.results = Some(val()),
            "--params" => inp.params = Some(val()),
            "--spec" => inp.spec = val(),
            "--pareto" => inp.pareto = Some(val()),
            "--model" => inp.model = val(),
            "--min-events" => inp.min_events = val().parse().unwrap_or_else(|_| usage()),
            "--out" => out = Some(val()),
            "--write-weights" => weights = Some(val()),
            _ if a.starts_with("--") => usage(),
            _ if in_runs => inp.runs.push(a),
            _ => usage(),
        }
    }
    if inp.runs.is_empty() {
        usage();
    }
    let rep = build(&inp).unwrap_or_else(|e| die(e));
    print!("{}", rep.table());
    if let Some(w) = weights {
        std::fs::write(&w, weights_toml()).unwrap_or_else(|e| die(format!("{w}: {e}")));
        println!("wrote {w}");
    }
    if let Some(d) = out {
        let _ = std::fs::create_dir_all(&d);
        let f = format!("{d}/family-{}.md", today());
        std::fs::write(&f, rep.markdown(&today())).unwrap_or_else(|e| die(format!("{f}: {e}")));
        println!("wrote {f}");
    }
}

/// `sim family`: the family of each input and its result-token shares.
fn family_cmd(args: impl Iterator<Item = String>) {
    use nexo_curator::sim::family::{shares, tag, FAMILIES};
    for path in args {
        let s = load(&path).unwrap_or_else(|e| die(e));
        let sh = shares(&s.requests);
        let by: Vec<String> = FAMILIES[..5].iter().map(|f| format!("{f} {:.2}", sh.share(f))).collect();
        println!("{path}: {} ({} result tokens; {})", tag(&s.requests), sh.total, by.join(", "));
    }
}
