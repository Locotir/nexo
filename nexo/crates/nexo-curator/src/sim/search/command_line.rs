use super::*;

/// One evaluated config.
struct Scored {
    spec: String,
    fix: Vec<Fix>,
}

fn weights_from(text: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for line in text.lines().map(|l| l.split('#').next().unwrap_or("").trim()) {
        if let Some((k, v)) = line.split_once('=') {
            if let Ok(x) = v.trim().parse::<f64>() {
                out.insert(k.trim().to_string(), x);
            }
        }
    }
    out
}

struct Opts {
    knobs: String,
    tier: String,
    configs: usize,
    workers: usize,
    seed: u64,
    stage: String,
    from: Option<String>,
    folds: usize,
    weights: String,
    params: Option<String>,
    out: Option<String>,
    fixtures: String,
    price: String,
    life: f64,
    slots: usize,
    max_steps: usize,
    c_blind: f64,
    holdout: usize,
    sweeps: usize,
    starts: usize,
    pareto_max: usize,
    pareto_min: usize,
    allow_missing: bool,
}

const USAGE: &str = "usage: sim search [--knobs sim/knobs.toml] [--tier fast|mid|all] [--configs N] [--workers W] [--seed X]
                  [--stage lhs|coord|confirm] [--from FILE] [--folds 3] [--weights sim/weights.toml] [--params sim/params.json]
                  [--out DIR] [--fixtures DIR[:DIR...]] [--price live|legacy] [--life S] [--slots K] [--max-steps 40] [--blind-c C]
                  [--holdout K] [--sweeps 3] [--starts 20] [--pareto-max 50] [--pareto-min 5] [--allow-missing]
  lhs     sample N configs by Latin hypercube, score them on the tier, write configs.tsv, pareto.tsv, folds.tsv
  coord    --from configs.tsv: coordinate descent from its best --starts configs and D_SPEC (N evaluations in all)
  confirm  --from pareto.tsv: rerun its configs on all 21 fixtures, write confirm.tsv
  A fixture of the tier that no --fixtures directory holds is an error, unless --allow-missing (then it is noted).";

fn parse_opts(mut args: impl Iterator<Item = String>) -> Result<Opts, String> {
    let mut o = Opts {
        knobs: "sim/knobs.toml".into(),
        tier: "fast".into(),
        configs: 1000,
        workers: std::thread::available_parallelism().map_or(4, |n| n.get()),
        seed: 1,
        stage: "lhs".into(),
        from: None,
        folds: 3,
        weights: "sim/weights.toml".into(),
        params: None,
        out: None,
        fixtures: format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR")),
        price: "live".into(),
        life: 0.0,
        slots: 0,
        max_steps: super::super::input::MAX_STEPS,
        c_blind: 0.0,
        holdout: 0,
        sweeps: 3,
        starts: 20,
        pareto_max: 50,
        pareto_min: 5,
        allow_missing: false,
    };
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or_else(|| format!("{a} needs a value\n{USAGE}"));
        let num = |s: String| s.parse::<f64>().map_err(|_| format!("{a}: {s:?} is not a number"));
        match a.as_str() {
            "--knobs" => o.knobs = val()?,
            "--tier" => o.tier = val()?,
            "--configs" => o.configs = num(val()?)? as usize,
            "--workers" => o.workers = num(val()?)? as usize,
            "--seed" => o.seed = num(val()?)? as u64,
            "--stage" => o.stage = val()?,
            "--from" => o.from = Some(val()?),
            "--folds" => o.folds = num(val()?)? as usize,
            "--weights" => o.weights = val()?,
            "--params" => o.params = Some(val()?),
            "--out" => o.out = Some(val()?),
            "--fixtures" => o.fixtures = val()?,
            "--price" => o.price = val()?,
            "--life" => o.life = num(val()?)?,
            "--slots" => o.slots = num(val()?)? as usize,
            "--max-steps" => o.max_steps = num(val()?)? as usize,
            "--blind-c" => o.c_blind = num(val()?)?,
            "--holdout" => o.holdout = num(val()?)? as usize,
            "--sweeps" => o.sweeps = num(val()?)? as usize,
            "--starts" => o.starts = num(val()?)? as usize,
            "--pareto-max" => o.pareto_max = num(val()?)? as usize,
            "--pareto-min" => o.pareto_min = num(val()?)? as usize,
            "--allow-missing" => o.allow_missing = true,
            _ => return Err(format!("unknown option {a:?}\n{USAGE}")),
        }
    }
    if o.folds < 2 {
        return Err("--folds takes 2 or more".into());
    }
    Ok(o)
}

fn build_setup(o: &Opts, tier_name: &str) -> Result<Setup, String> {
    let names = tier(tier_name)?;
    // Ok(Err(name)): the fixture is in no directory
    let loaded = parallel_map(o.workers, names.len(), &|i| -> Result<Result<(String, Vec<Request>), String>, String> {
        let Some(path) =
            o.fixtures.split(':').map(|d| format!("{d}/{}.jsonl", names[i])).find(|p| std::path::Path::new(p).exists())
        else {
            return Ok(Err(names[i].to_string()));
        };
        Ok(Ok((names[i].to_string(), load(&path)?.requests)))
    });
    let prices = Prices::named(&o.price).ok_or("--price takes live or legacy")?;
    let (mut fixtures, mut missing) = (Vec::new(), Vec::new());
    for l in loaded {
        match l? {
            Ok((name, requests)) => {
                let family = auto_family(&requests).to_string();
                let plain = plain_cost(&requests, prices, o.life, o.slots);
                fixtures.push(Fixture { name, family, requests, max_steps: o.max_steps, plain });
            }
            Err(name) => missing.push(name),
        }
    }
    if !missing.is_empty() && !o.allow_missing {
        return Err(format!(
            "fixtures of tier {tier_name} not found in {}: {} (a missing one changes J and the folds; --allow-missing goes on without)",
            o.fixtures,
            missing.join(", ")
        ));
    }
    if fixtures.is_empty() {
        return Err(format!("no fixture of tier {tier_name} found in {}", o.fixtures));
    }
    if !missing.is_empty() {
        eprintln!(
            "note: {} of {} fixtures of tier {tier_name} are missing in {}: {}",
            missing.len(),
            names.len(),
            o.fixtures,
            missing.join(", ")
        );
    }
    let params = match &o.params {
        Some(p) => ParamSet::load(p)?,
        None => ParamSet::default(),
    };
    let weights =
        std::fs::read_to_string(&o.weights).map(|t| weights_from(&t)).map_err(|e| format!("{}: {e}", o.weights))?;
    let fold = assign_folds(&fixtures, o.folds);
    Ok(Setup {
        fixtures,
        params,
        prices,
        life: o.life,
        slots: o.slots,
        weights,
        c_blind: o.c_blind,
        fold,
        folds: o.folds,
    })
}

fn evaluate_all(setup: &Setup, space: &Space, configs: Vec<Config>, workers: usize) -> (Vec<Scored>, usize) {
    let results = parallel_map(workers, configs.len(), &|i| {
        let spec = space.render(&configs[i]);
        let r = setup.eval(&spec);
        (spec, r)
    });
    let mut bad = 0;
    let mut out = Vec::new();
    for (spec, r) in results {
        match r {
            Ok(fix) => out.push(Scored { spec, fix }),
            Err(e) => {
                bad += 1;
                if bad <= 3 {
                    eprintln!("config refused ({e}): {spec}");
                }
            }
        }
    }
    (out, bad)
}

fn row_numbers(setup: &Setup, fix: &[Fix]) -> String {
    let a = setup.axes(fix);
    let mut s = format!(
        "{:.6}\t{:.6}\t{:.4}\t{:.5}\t{:.6}",
        setup.j(fix, &|_| true),
        a[0],
        a[1],
        a[2],
        setup.j_flat(fix, &|_| true)
    );
    for k in 0..setup.folds {
        let _ = write!(s, "\t{:.6}", setup.j_fold(fix, k));
    }
    s
}

fn fold_header(setup: &Setup) -> String {
    (0..setup.folds).map(|k| format!("J_fold{k}")).collect::<Vec<_>>().join("\t")
}

/// The folds D_SPEC loses in held-out J to `fix`: how many of the folds `fix` beats D_SPEC in.
fn folds_beaten(setup: &Setup, fix: &[Fix], dspec: &[Fix]) -> usize {
    (0..setup.folds).filter(|&k| setup.j_fold(fix, k) < setup.j_fold(dspec, k)).count()
}

pub(super) fn write(dir: &str, name: &str, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{dir}: {e}"))?;
    std::fs::write(format!("{dir}/{name}"), text).map_err(|e| format!("{dir}/{name}: {e}"))
}

/// configs.tsv, pareto.tsv and folds.tsv for a set of scored configs and D_SPEC.
fn report(
    setup: &Setup,
    o: &Opts,
    dir: &str,
    dspec: &Scored,
    scored: &[Scored],
) -> Result<Vec<(usize, usize)>, String> {
    let head = format!("J\tcost\textra_steps\tp_fail\tJ_unweighted\t{}", fold_header(setup));
    let mut all: Vec<&Scored> = scored.iter().collect();
    all.sort_by(|a, b| a.spec.cmp(&b.spec));
    all.dedup_by(|a, b| a.spec == b.spec);
    let mut t = format!("{head}\tspec\n");
    t += &format!("{}\t{}\n", row_numbers(setup, &dspec.fix), dspec.spec);
    for s in &all {
        t += &format!("{}\t{}\n", row_numbers(setup, &s.fix), s.spec);
    }
    write(dir, "configs.tsv", &t)?;

    // Pareto: D_SPEC takes part, so a config it dominates is not listed; its own row is the reference
    let mut pool: Vec<&Scored> = vec![dspec];
    pool.extend(all.iter().copied());
    let pts: Vec<[f64; 3]> = pool.iter().map(|s| setup.axes(&s.fix)).collect();
    let layers = pareto_layers(&pts, o.pareto_min);
    let total = layers.iter().filter(|&&(i, r)| i != 0 && r == 1).count();
    let mut front: Vec<(usize, usize)> = layers.into_iter().filter(|&(i, _)| i != 0).collect();
    front.sort_by(|&(a, ra), &(b, rb)| {
        let (ja, jb) = (setup.j(&pool[a].fix, &|_| true), setup.j(&pool[b].fix, &|_| true));
        ra.cmp(&rb).then(ja.total_cmp(&jb)).then(pool[a].spec.cmp(&pool[b].spec))
    });
    front.truncate(o.pareto_max);
    let mut t = format!("id\t{head}\tfolds_beaten_in_sample\trank\tspec\n");
    t += &format!("D_SPEC\t{}\t-\t-\t{}\n", row_numbers(setup, &dspec.fix), dspec.spec);
    for (n, &(i, rank)) in front.iter().enumerate() {
        let s = pool[i];
        t += &format!(
            "P{:03}\t{}\t{}/{}\t{}\t{}\n",
            n + 1,
            row_numbers(setup, &s.fix),
            folds_beaten(setup, &s.fix, &dspec.fix),
            setup.folds,
            rank,
            s.spec
        );
    }
    let mut notes = vec![format!(
        "# {total} of {} configs are non-dominated (rank 1). Rows of rank 2 or more are dominated by a rank-1 row and are listed only to reach --pareto-min {}.",
        all.len(),
        o.pareto_min
    )];
    let collapsed = collapsed_axes(&pts);
    if !collapsed.is_empty() {
        notes.push(format!(
            "# The axes {} do not vary across the configs (with r = beta = phi = 0 in the params no config adds steps or fails), so the front is over fewer axes than three.",
            collapsed.join(" and ")
        ));
    }
    for (fam, w) in setup.uncovered() {
        notes.push(format!(
            "# Family {fam} weighs {w} in J and has no fixture in this tier: the search never sees its load."
        ));
    }
    notes.push(
        "# folds_beaten_in_sample counts folds of the fixtures the configs were chosen on: it is not a held-out result (folds.tsv is)."
            .to_string(),
    );
    t += &(notes.join("\n") + "\n");
    write(dir, "pareto.tsv", &t)?;
    println!(
        "pareto: {total} non-dominated configs of {} (listed {}, ranks 1-{}), D_SPEC row first",
        all.len(),
        front.len(),
        front.iter().map(|f| f.1).max().unwrap_or(1)
    );
    for n in &notes[1..] {
        println!("{}", n.trim_start_matches("# "));
    }

    // folds: per fold, the config chosen on the other folds and how it does on the held-out one
    let mut t = String::from("fold\tfixtures\tJ_train\tJ_heldout\tdspec_J_train\tdspec_J_heldout\tbeats_dspec\tspec\n");
    for k in 0..setup.folds {
        let names: Vec<&str> = (0..setup.fixtures.len())
            .filter(|&i| setup.fold[i] == k)
            .map(|i| setup.fixtures[i].name.as_str())
            .collect();
        let Some(best) = all
            .iter()
            .min_by(|a, b| setup.j_train(&a.fix, k).total_cmp(&setup.j_train(&b.fix, k)).then(a.spec.cmp(&b.spec)))
        else {
            continue;
        };
        let (jt, jh) = (setup.j_train(&best.fix, k), setup.j_fold(&best.fix, k));
        let (dt, dh) = (setup.j_train(&dspec.fix, k), setup.j_fold(&dspec.fix, k));
        t += &format!("{k}\t{}\t{jt:.6}\t{jh:.6}\t{dt:.6}\t{dh:.6}\t{}\t{}\n", names.join(","), jh < dh, best.spec);
        println!("fold {k}: held-out J {jh:.6} vs D_SPEC {dh:.6} ({}), selected on the other folds (train J {jt:.6} vs {dt:.6})", if jh < dh { "better" } else { "worse" });
    }
    write(dir, "folds.tsv", &t)?;
    Ok(front)
}

fn summary_line(setup: &Setup, scored: &[Scored], bad: usize, secs: f64) {
    let n = scored.len() + bad;
    println!(
        "evaluated {n} configs ({bad} refused) on {} fixtures in {secs:.1} s wall: configs/hour {:.0}",
        setup.fixtures.len(),
        n as f64 / secs.max(1e-9) * 3600.0
    );
}

/// The `spec` column of a tsv written by this module.
fn read_specs(path: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().ok_or(format!("{path}: empty"))?.split('\t').collect();
    let at = head.iter().position(|h| *h == "spec").ok_or(format!("{path}: no spec column"))?;
    Ok(lines
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let c: Vec<String> = l.split('\t').map(str::to_string).collect();
            (c[at].clone(), c)
        })
        .collect())
}

/// `sim search`.
pub fn cli(args: impl Iterator<Item = String>) -> Result<(), String> {
    let o = parse_opts(args)?;
    let space = Space::parse(&std::fs::read_to_string(&o.knobs).map_err(|e| format!("{}: {e}", o.knobs))?)?;
    let dir = o.out.clone().unwrap_or_else(|| format!("sim/reports/search-{}-{}-s{}", o.stage, o.tier, o.seed));
    let t0 = Instant::now();
    match o.stage.as_str() {
        "lhs" => {
            let setup = build_setup(&o, &o.tier)?;
            println!(
                "tier {}: {} fixtures, families {:?}, folds {:?}",
                o.tier,
                setup.fixtures.len(),
                families(&setup),
                setup.fold
            );
            let d = space.default_config();
            let dspec = evaluate_all(&setup, &space, vec![d], 1).0.pop().ok_or("D_SPEC does not run")?;
            let t1 = Instant::now();
            let (scored, bad) = evaluate_all(&setup, &space, lhs(&space, o.configs, o.seed), o.workers);
            summary_line(&setup, &scored, bad, t1.elapsed().as_secs_f64());
            report(&setup, &o, &dir, &dspec, &scored)?;
        }
        "coord" => {
            let from = o.from.as_deref().ok_or("--stage coord needs --from configs.tsv")?;
            let setup = build_setup(&o, &o.tier)?;
            let held = o.holdout % setup.folds;
            let rows = read_specs(from)?;
            // the best starts on the training folds of the held-out one, from the J_fold columns of the file
            let mut tops: Vec<(f64, &str)> = rows
                .iter()
                .skip(1)
                .filter_map(|(spec, c)| {
                    let cols: Vec<f64> = c.iter().filter_map(|x| x.parse().ok()).collect();
                    // J, cost, extra_steps, p_fail, J_unweighted, then the folds
                    let f = cols.get(5..)?;
                    (f.len() > held).then(|| (f.iter().sum::<f64>() - f[held], spec.as_str()))
                })
                .collect();
            tops.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(b.1)));
            let mut starts: Vec<Config> = vec![space.default_config()];
            for (_, spec) in tops.iter().take(o.starts) {
                starts.push(space.parse_spec(spec)?);
            }
            let per = (o.configs / starts.len()).max(1);
            let memo: Mutex<HashMap<String, Option<Vec<Fix>>>> = Mutex::new(HashMap::new());
            let eval = |c: &Config| -> Option<Vec<Fix>> {
                let spec = space.render(c);
                if let Some(v) = memo.lock().unwrap().get(&spec) {
                    return v.clone();
                }
                let r = setup.eval(&spec).ok();
                memo.lock().unwrap().insert(spec, r.clone());
                r
            };
            println!("coord: {} starts, {} evaluations each, hold out fold {held}", starts.len(), per);
            let obj = |c: &Config| eval(c).map_or(f64::INFINITY, |f| setup.j_train(&f, held));
            let runs = parallel_map(o.workers, starts.len(), &|i| {
                coordinate_descent_budget(&space, starts[i].clone(), &obj, o.sweeps, o.seed.wrapping_add(i as u64), per)
            });
            for (i, d) in runs.iter().enumerate() {
                println!(
                    "  start {i}: train J {:.6} -> {:.6} in {} sweeps, {} evals",
                    obj(&starts[i]),
                    d.j,
                    d.sweeps,
                    d.evals
                );
            }
            let memo = memo.into_inner().unwrap();
            let mut scored: Vec<Scored> =
                memo.into_iter().filter_map(|(spec, f)| Some(Scored { spec, fix: f? })).collect();
            scored.sort_by(|a, b| a.spec.cmp(&b.spec));
            let dspec = Scored {
                spec: space.render(&space.default_config()),
                fix: setup.eval(&space.render(&space.default_config()))?,
            };
            summary_line(&setup, &scored, 0, t0.elapsed().as_secs_f64());
            report(&setup, &o, &dir, &dspec, &scored)?;
        }
        "confirm" => {
            let from = o.from.clone().ok_or("--stage confirm needs --from pareto.tsv")?;
            let mut o = o;
            o.tier = "all".into();
            let setup = build_setup(&o, "all")?;
            let rows = read_specs(&from)?;
            let mut configs = vec![space.default_config()];
            for (spec, c) in &rows {
                if c.first().map(String::as_str) != Some("D_SPEC") {
                    configs.push(space.parse_spec(spec)?);
                }
            }
            let ids: Vec<String> = std::iter::once("D_SPEC".to_string())
                .chain(
                    rows.iter()
                        .filter(|(_, c)| c.first().map(String::as_str) != Some("D_SPEC"))
                        .map(|(_, c)| c[0].clone()),
                )
                .collect();
            let t1 = Instant::now();
            let (scored, bad) = evaluate_all(&setup, &space, configs, o.workers);
            summary_line(&setup, &scored, bad, t1.elapsed().as_secs_f64());
            let dspec = scored.first().ok_or("D_SPEC does not run")?;
            // The configs were chosen on the fast tier, which sits inside these 21 fixtures and their folds:
            // the fold columns are in sample. Only the fixtures outside the fast tier were never seen.
            let unseen = |i: usize| setup.unseen(i);
            let n_unseen = (0..setup.fixtures.len()).filter(|&i| unseen(i)).count();
            println!(
                "{n_unseen} of {} fixtures are outside the fast tier (never seen by the search)",
                setup.fixtures.len()
            );
            let mut t = format!(
                "id\tJ_weighted\tJ_unweighted\tcost\textra_steps\tp_fail\t{}\tfolds_beaten_in_sample\tbeats_dspec_all_folds_in_sample\tJ_unseen_weighted\tJ_unseen_unweighted\tbeats_dspec_unseen\tspec\n",
                fold_header(&setup)
            );
            for (id, s) in ids.iter().zip(&scored) {
                let a = setup.axes(&s.fix);
                let folds: Vec<String> = (0..setup.folds).map(|k| format!("{:.6}", setup.j_fold(&s.fix, k))).collect();
                let beaten = folds_beaten(&setup, &s.fix, &dspec.fix);
                let all = beaten == setup.folds && id != "D_SPEC";
                let (uw, uf) = (setup.j(&s.fix, &unseen), setup.j_flat(&s.fix, &unseen));
                let wins = uf < setup.j_flat(&dspec.fix, &unseen);
                t += &format!(
                    "{id}\t{:.6}\t{:.6}\t{:.6}\t{:.4}\t{:.5}\t{}\t{beaten}/{}\t{}\t{uw:.6}\t{uf:.6}\t{}\t{}\n",
                    setup.j(&s.fix, &|_| true),
                    setup.j_flat(&s.fix, &|_| true),
                    a[0],
                    a[1],
                    a[2],
                    folds.join("\t"),
                    setup.folds,
                    if id == "D_SPEC" { "-".to_string() } else { all.to_string() },
                    if id == "D_SPEC" { "-".to_string() } else { wins.to_string() },
                    s.spec
                );
                println!(
                    "{id:>6}: J weighted {:.6}, unweighted {:.6}; in sample, folds beaten {beaten}/{}; unseen fixtures J {uw:.6} weighted, {uf:.6} unweighted{}",
                    setup.j(&s.fix, &|_| true),
                    setup.j_flat(&s.fix, &|_| true),
                    setup.folds,
                    if id == "D_SPEC" {
                        " (reference)".to_string()
                    } else if wins {
                        " : beats D_SPEC on the unseen fixtures".into()
                    } else {
                        " : does not beat D_SPEC on the unseen fixtures".into()
                    }
                );
            }
            let mut notes = Vec::new();
            for (fam, w) in setup.uncovered() {
                notes.push(format!("# Family {fam} weighs {w} in J and has no fixture: its load is not scored."));
            }
            notes.push(
                "# folds_beaten_in_sample and beats_dspec_all_folds_in_sample use fixtures the configs were chosen on; beats_dspec_unseen (unweighted J on the fixtures outside the fast tier) is the held-out claim."
                    .to_string(),
            );
            t += &(notes.join("\n") + "\n");
            write(&dir, "confirm.tsv", &t)?;
        }
        other => return Err(format!("unknown stage {other:?} (lhs, coord, confirm)\n{USAGE}")),
    }
    println!("wrote {dir}/ in {:.1} s", t0.elapsed().as_secs_f64());
    Ok(())
}

pub(super) fn families(setup: &Setup) -> BTreeMap<&str, usize> {
    let mut m = BTreeMap::new();
    for f in &setup.fixtures {
        *m.entry(f.family.as_str()).or_insert(0) += 1;
    }
    m
}
