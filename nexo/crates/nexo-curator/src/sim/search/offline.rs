//! Offline search. Manifest TSV (no header): `dev|test<TAB>project<TAB>path.jsonl`.
//! Optional fourth column: annotation TSV path, rows category (negation|number|correction|constraint),
//! zero-based request index, exact clause. Unannotated cases are reported NA, not evidence of retention.
//! Exact matching misses paraphrases and may match irrelevant or contradicted text.
//! Paths are relative to the manifest. Projects and identical files may not cross splits.
//! This cannot detect renamed/edited versions of a session or dishonest project labels.
//!
//! Example, from the repository root (choose genuinely unrelated projects for real evaluation):
//! create knobs.toml containing `[int]` and `protect = [4, 40]`, and an empty
//! weights.toml; write the TSV with absolute fixture paths and distinct project labels.
//! Then run `nxcargo run --offline -p nexo-curator --example sim --features sim -- search
//! --stage offline --corpus corpus.tsv --knobs knobs.toml --weights weights.toml
//! --configs 10 --seed 1 --out report`. Empty weights mean J has no family weights;
//! this stage selects on per-case dimensions, not J. The files contain checksums and
//! generated case IDs, but specs and checksums may themselves still be sensitive.
//! Freeze test before the run, and do not repeatedly tune against its results.
use super::*;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

struct Fact {
    category: String,
    step: usize,
    clause: String,
}

fn fact_results(setup: &Setup, annotations: &[Vec<Fact>], spec: &str) -> Result<Vec<Vec<bool>>, String> {
    setup
        .fixtures
        .iter()
        .zip(annotations)
        .map(|(fixture, facts)| {
            if facts.is_empty() {
                return Ok(Vec::new());
            }
            let mut curator = crate::Curator::new(spec).map_err(|e| format!("bad spec: {e}"))?;
            curator.window(0);
            let mut results = vec![false; facts.len()];
            for (step, q) in fixture.requests.iter().enumerate() {
                if let Some(tools) = &q.tools {
                    curator.tools(tools);
                }
                let view = curator.view(q.system_tokens, &q.messages, q.now).map_err(|e| format!("view: {e:?}"))?.view;
                for (i, fact) in facts.iter().enumerate().filter(|(_, f)| f.step == step) {
                    results[i] = view.iter().any(|m| crate::sim::input::text_of(m).contains(&fact.clause));
                }
            }
            Ok(results)
        })
        .collect()
}

fn facts_no_regression(candidate: &[Vec<bool>], baseline: &[Vec<bool>]) -> bool {
    candidate.len() == baseline.len()
        && candidate.iter().zip(baseline).all(|(a, b)| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| !y || *x))
}

fn fingerprint(bytes: &[u8]) -> String {
    // Stable non-cryptographic checksum, not a privacy or integrity guarantee.
    let mut h = 0xcbf29ce484222325u64;
    for b in bytes {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{h:016x}")
}

fn dominates(a: &[Fix], b: &[Fix]) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    let mut better = false;
    for (a, b) in a.iter().zip(b) {
        for (x, y) in [
            (a.cost, b.cost),
            (a.extra_cost, b.extra_cost),
            (a.extra_steps, b.extra_steps),
            (a.p_fail, b.p_fail),
            (a.blind, b.blind),
        ] {
            if !x.is_finite() || !y.is_finite() || x > y {
                return false;
            }
            better |= x < y;
        }
    }
    better
}

fn eval_finalist(id: &str, eval: impl FnOnce() -> Result<Vec<Fix>, String>) -> Result<Vec<Fix>, String> {
    eval().map_err(|e| format!("offline finalist {id} failed on test: {e}"))
}

pub(super) fn run(o: &Opts, space: &Space, dir: &str) -> Result<(), String> {
    // Never mix artifacts from different runs, including a previously interrupted run.
    if Path::new(dir).exists() {
        let mut entries = std::fs::read_dir(dir).map_err(|e| format!("{dir}: {e}"))?;
        if entries.next().transpose().map_err(|e| format!("{dir}: {e}"))?.is_some() {
            return Err(format!("offline output directory is not empty: {dir}"));
        }
    }
    let manifest = o.corpus.as_deref().ok_or("offline requires --corpus manifest.tsv")?;
    if o.allow_missing {
        return Err("offline refuses --allow-missing".into());
    }
    if o.configs == 0 || o.pareto_max == 0 {
        return Err("offline needs positive --configs and --pareto-max".into());
    }
    let text = std::fs::read_to_string(manifest).map_err(|e| format!("{manifest}: {e}"))?;
    let base = Path::new(manifest).parent().unwrap_or(Path::new("."));
    let prices = Prices::named(&o.price).ok_or("--price takes live or legacy")?;
    let mut splits = [Vec::new(), Vec::new()];
    let mut annotations: [Vec<Vec<Fact>>; 2] = [Vec::new(), Vec::new()];
    let mut projects = [BTreeSet::new(), BTreeSet::new()];
    let mut seen_paths = HashSet::new();
    let mut contents: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut inputs = String::from("split\tcase_id\tchecksum_noncryptographic\n");
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if !(fields.len() == 3 || fields.len() == 4)
            || fields[1].is_empty()
            || fields[2].is_empty()
            || (fields.len() == 4 && fields[3].is_empty())
        {
            return Err(format!("manifest line {}: expected split, project, path", n + 1));
        }
        let split = match fields[0] {
            "dev" => 0,
            "test" => 1,
            _ => return Err(format!("manifest line {}: expected dev or test", n + 1)),
        };
        let path = base.join(fields[2]).canonicalize().map_err(|e| format!("{}: {e}", fields[2]))?;
        if !seen_paths.insert(path.clone()) {
            return Err(format!("duplicate path: {}", path.display()));
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if contents.iter().any(|(s, b)| *s != split && *b == bytes) {
            return Err("identical corpus contents across dev/test".into());
        }
        let pathstr = path.to_str().ok_or("non UTF-8 path")?;
        let requests = load(pathstr)?.requests;
        if requests.is_empty() {
            return Err(format!("empty session: {pathstr}"));
        }
        let mut facts = Vec::new();
        if fields.len() == 4 {
            let annotation_path = base.join(fields[3]);
            let annotation_text =
                std::fs::read_to_string(&annotation_path).map_err(|e| format!("{}: {e}", annotation_path.display()))?;
            for (line_no, line) in annotation_text.lines().enumerate() {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let parts: Vec<_> = line.splitn(3, '\t').collect();
                if parts.len() != 3
                    || !["negation", "number", "correction", "constraint"].contains(&parts[0])
                    || parts[2].is_empty()
                {
                    return Err(format!(
                        "annotation line {}: expected category, zero-based request index, literal clause",
                        line_no + 1
                    ));
                }
                let step: usize =
                    parts[1].parse().map_err(|_| format!("annotation line {}: invalid index", line_no + 1))?;
                let q =
                    requests.get(step).ok_or_else(|| format!("annotation line {}: index out of range", line_no + 1))?;
                if !q.messages.iter().any(|m| crate::sim::input::text_of(m).contains(parts[2])) {
                    return Err(format!("annotation line {}: literal absent from original request", line_no + 1));
                }
                facts.push(Fact { category: parts[0].into(), step, clause: parts[2].into() });
            }
            inputs += &format!(
                "{}\t{}_annotations_{:04}\t{}\n",
                fields[0],
                fields[0],
                splits[split].len(),
                fingerprint(annotation_text.as_bytes())
            );
        }
        annotations[split].push(facts);
        let family = auto_family(&requests).to_string();
        let plain = plain_cost(&requests, prices, o.life, o.slots);
        splits[split].push(Fixture {
            name: format!("{}_{:04}", fields[0], splits[split].len()),
            family,
            requests,
            max_steps: o.max_steps,
            plain,
        });
        projects[split].insert(fields[1].to_string());
        inputs += &format!("{}\t{}\t{}\n", fields[0], splits[split].last().unwrap().name, fingerprint(&bytes));
        contents.push((split, bytes));
    }
    if splits.iter().any(Vec::is_empty) {
        return Err("manifest requires nonempty dev and test".into());
    }
    if !projects[0].is_disjoint(&projects[1]) {
        return Err("project appears in both dev and test".into());
    }
    let weights_text = std::fs::read_to_string(&o.weights).map_err(|e| format!("{}: {e}", o.weights))?;
    let weights = weights_from(&weights_text);
    let params = match &o.params {
        Some(p) => ParamSet::load(p)?,
        None => ParamSet::default(),
    };
    let setup = |fixtures: Vec<Fixture>| Setup {
        fold: assign_folds(&fixtures, o.folds),
        fixtures,
        params: params.clone(),
        prices,
        life: o.life,
        slots: o.slots,
        weights: weights.clone(),
        c_blind: o.c_blind,
        folds: o.folds,
    };
    let [dev_annotations, test_annotations] = annotations;
    let [dev, test] = splits;
    let dev = setup(dev);
    let test = setup(test);
    // The agent's default context config passes the empty spec to Curator::new.
    // Do not render through the search space: it clamps knobs and adds FIXED.
    let baseline = crate::D_SPEC;
    let d = dev.eval(baseline)?;
    let dev_base_facts = fact_results(&dev, &dev_annotations, baseline)?;
    let (mut candidates, bad) = evaluate_all(&dev, space, lhs(space, o.configs, o.seed), o.workers);
    candidates.sort_by(|a, b| a.spec.cmp(&b.spec));
    candidates.dedup_by(|a, b| a.spec == b.spec);
    let mut cases = String::from("split\tcandidate\tcase\tcost_est\textra_cost_est\textra_steps_est\tp_fail_est\tblind_tokens\tdelta_cost\tdelta_extra_cost\tdelta_extra_steps\tdelta_p_fail\tdelta_blind\tspec\n");
    fn rows(out: &mut String, split: &str, id: &str, spec: &str, setup: &Setup, fixes: &[Fix], base: &[Fix]) {
        for ((f, x), b) in setup.fixtures.iter().zip(fixes).zip(base) {
            let _ = writeln!(
                out,
                "{split}\t{id}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{spec}",
                f.name,
                x.cost,
                x.extra_cost,
                x.extra_steps,
                x.p_fail,
                x.blind,
                x.cost - b.cost,
                x.extra_cost - b.extra_cost,
                x.extra_steps - b.extra_steps,
                x.p_fail - b.p_fail,
                x.blind - b.blind
            );
        }
    }
    rows(&mut cases, "dev", "baseline_D_SPEC", baseline, &dev, &d, &d);
    for (i, c) in candidates.iter().enumerate() {
        rows(&mut cases, "dev", &format!("C{i:04}"), &c.spec, &dev, &c.fix, &d);
    }
    let mut fact_rows = String::from("split\tcandidate\tcase\tannotation\tcategory\trequest_index\tliteral_present\n");
    fn report_facts(
        out: &mut String,
        split: &str,
        id: &str,
        setup: &Setup,
        annotations: &[Vec<Fact>],
        results: &[Vec<bool>],
    ) {
        for ((fixture, facts), values) in setup.fixtures.iter().zip(annotations).zip(results) {
            if facts.is_empty() {
                let _ = writeln!(out, "{split}\t{id}\t{}\tNA\tNA\tNA\tNA", fixture.name);
            }
            for (i, (fact, present)) in facts.iter().zip(values).enumerate() {
                let _ =
                    writeln!(out, "{split}\t{id}\t{}\t{i}\t{}\t{}\t{present}", fixture.name, fact.category, fact.step);
            }
        }
    }
    report_facts(&mut fact_rows, "dev", "baseline_D_SPEC", &dev, &dev_annotations, &dev_base_facts);
    let mut dev_facts = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let values = fact_results(&dev, &dev_annotations, &c.spec)?;
        report_facts(&mut fact_rows, "dev", &format!("C{i:04}"), &dev, &dev_annotations, &values);
        dev_facts.push(values);
    }
    // Strict per-case no-regression on all five estimated dimensions and each annotated literal.
    let eligible: Vec<usize> = (0..candidates.len())
        .filter(|&i| dominates(&candidates[i].fix, &d) && facts_no_regression(&dev_facts[i], &dev_base_facts))
        .collect();
    let finalists: Vec<usize> = eligible
        .iter()
        .copied()
        .filter(|&i| {
            !eligible.iter().any(|&j| {
                i != j
                    && dominates(&candidates[j].fix, &candidates[i].fix)
                    && facts_no_regression(&dev_facts[j], &dev_facts[i])
            })
        })
        .take(o.pareto_max)
        .collect();
    let tbase = test.eval(baseline)?;
    let test_base_facts = fact_results(&test, &test_annotations, baseline)?;
    report_facts(&mut fact_rows, "test", "baseline_D_SPEC", &test, &test_annotations, &test_base_facts);
    rows(&mut cases, "test", "baseline_D_SPEC", baseline, &test, &tbase, &tbase);
    let mut selection = String::from("id\tdev_front\ttest_strictly_dominates_baseline\tspec\n");
    for i in finalists {
        let c = &candidates[i];
        let id = format!("C{i:04}");
        let fix = eval_finalist(&id, || test.eval(&c.spec))?;
        rows(&mut cases, "test", &id, &c.spec, &test, &fix, &tbase);
        let tfacts = fact_results(&test, &test_annotations, &c.spec)?;
        report_facts(&mut fact_rows, "test", &id, &test, &test_annotations, &tfacts);
        let _ = writeln!(
            selection,
            "{id}\ttrue\t{}\t{}",
            dominates(&fix, &tbase) && facts_no_regression(&tfacts, &test_base_facts),
            c.spec
        );
    }
    let knobs = std::fs::read(&o.knobs).map_err(|e| format!("{}: {e}", o.knobs))?;
    let params_hash = match &o.params {
        Some(p) => fingerprint(&std::fs::read(p).map_err(|e| format!("{p}: {e}"))?),
        None => "built-in default".into(),
    };
    let rev = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|x| x.status.success())
        .map(|x| String::from_utf8_lossy(&x.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let meta = format!("revision={rev}\nworking_tree_may_be_dirty=true\nmanifest_checksum={}\nknobs_checksum={}\nweights_checksum={}\nparams_checksum={}\nseed={}\nconfigs={}\nrefused={}\nworkers={}\nprice={}\nlife={}\nslots={}\nmax_steps={}\nblind_c={}\nfolds={}\npareto_max={}\nbaseline=production_default_D_SPEC_without_user_overrides\nselection=per_case_five_axes_strict_dominance_and_annotated_literals_no_regression_then_dev_pareto; finalists_capped_in_spec_order\nfacts=exact_literal_presence_only_not_semantic_accuracy; NA_means_unannotated_not_pass; annotations_can_match_irrelevant_or_contradicted_text
limits=default_configuration_only_not_user_overrides; simulation_estimates_not_real_model_or_memory_accuracy; no_network_calls; labels_must_identify_projects; edited_duplicate_sessions_not_detected; repeated_test_use_leaks_information\n",fingerprint(text.as_bytes()),fingerprint(&knobs),fingerprint(weights_text.as_bytes()),params_hash,o.seed,o.configs,bad,o.workers,o.price,o.life,o.slots,o.max_steps,o.c_blind,o.folds,o.pareto_max);
    write(dir, "inputs.tsv", &inputs)?;
    write(dir, "metadata.txt", &meta)?;
    write(dir, "cases.tsv", &cases)?;
    write(dir, "selection.tsv", &selection)?;
    write(dir, "facts.tsv", &fact_rows)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn f(x: f64) -> Fix {
        Fix { cost: x, extra_cost: 0., extra_steps: 0., p_fail: 0., blind: 0. }
    }
    #[test]
    fn literal_gate_rejects_regression_but_not_missing_annotations() {
        assert!(!facts_no_regression(&[vec![false]], &[vec![true]]));
        assert!(facts_no_regression(&[vec![true]], &[vec![false]]));
        assert!(facts_no_regression(&[vec![]], &[vec![]]));
    }
    #[test]
    fn finalist_failure_is_an_error() {
        let err = eval_finalist("C0001", || Err("bad fixture".into())).err().unwrap();
        assert_eq!(err, "offline finalist C0001 failed on test: bad fixture");
    }
    #[test]
    fn strict_per_case_no_regression() {
        assert!(dominates(&[f(1.), f(2.)], &[f(2.), f(2.)]));
        assert!(!dominates(&[f(1.), f(3.)], &[f(2.), f(2.)]));
        assert!(!dominates(&[f(2.)], &[f(2.)]));
        assert!(!dominates(&[f(f64::NAN)], &[f(2.)]));
    }
}
