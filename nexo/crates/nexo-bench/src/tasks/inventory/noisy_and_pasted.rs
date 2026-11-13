use super::*;

// ------------------------------------------------------------------ the noisy marathon: the heavy one with junk

/// The CI run that really failed, among twelve, and its job id.
pub(super) const CI_RUN: usize = 7;
pub(super) const CI_STEPS: [&str; 10] = [
    "checkout",
    "cache-restore",
    "fetch-deps",
    "build",
    "lint",
    "unit-tests",
    "migrate-db",
    "integration",
    "package",
    "upload",
];

/// The job id of CI run `n` (1-based); twelve distinct ids.
pub(super) fn ci_job(n: usize) -> String {
    let mut rng = Rng::new(9000 + n as u64);
    format!("j-{:06x}", rng.next_u64() % 0xff_ffff)
}

/// One night's CI log: ~500 lines of steps, timings, downloads and retries. Most runs hit errors that a
/// retry fixed; run `CI_RUN` fails one step three times and is aborted.
pub(super) fn ci_log(n: usize) -> String {
    let mut rng = Rng::new(7000 + n as u64);
    let crates = ["serde", "tokio", "regex", "chrono", "rand", "log", "anyhow", "itertools", "uuid", "csv", "bytes"];
    let tests = ["stock_totals", "price_rounding", "unit_parse", "order_split", "report_rows", "plan_threshold"];
    let job = ci_job(n);
    let mut out = format!("== CI run {n:02} | job {job} | runner ci-{} | branch main ==\n", rng.int(10, 99));
    let (mut h, mut m, mut s) = (2, rng.int(0, 50), 0);
    let mut clock = |rng: &mut Rng| {
        s += rng.int(0, 9);
        m += s / 60;
        s %= 60;
        h += m / 60;
        m %= 60;
        format!("[{h:02}:{m:02}:{s:02}.{:03}]", rng.int(0, 999))
    };
    let flaky = if n == CI_RUN { "migrate-db" } else { CI_STEPS[4 + rng.below(4)] };
    for step in CI_STEPS {
        let _ = writeln!(out, "{} {step:<13}| step started", clock(&mut rng));
        for _ in 0..rng.int(35, 55) {
            let t = clock(&mut rng);
            let line = match rng.below(7) {
                0 => format!(
                    "Downloaded {} v0.{}.{} ({} KB)",
                    rng.pick(&crates),
                    rng.int(1, 40),
                    rng.int(0, 9),
                    rng.int(8, 900)
                ),
                1 => format!("Compiling {} v1.{}.{}", rng.pick(&crates), rng.int(0, 30), rng.int(0, 9)),
                2 => format!("test {} ... ok ({} ms)", rng.pick(&tests), rng.int(1, 4000)),
                3 => format!("WARN slow request to cache.internal: {} ms, retrying (attempt 1/3)", rng.int(900, 9000)),
                4 => format!(
                    "cache key inventory-{:08x} restored ({} MB)",
                    rng.next_u64() % 0xffff_ffff,
                    rng.int(1, 300)
                ),
                5 => format!("WARN deprecated runner image ubuntu-{}.04, will be removed", rng.int(18, 22)),
                _ => format!("debug: env CI_NODE={} CI_SHARD={}/8 RUST_LOG=info", rng.int(1, 40), rng.int(1, 8)),
            };
            let _ = writeln!(out, "{t} {step:<13}| {line}");
        }
        if step == flaky {
            let attempts = if n == CI_RUN { 3 } else { 1 };
            for a in 1..=attempts {
                let _ = writeln!(
                    out,
                    "{} {step:<13}| ERROR step failed: exit code {} (attempt {a}/3)",
                    clock(&mut rng),
                    rng.int(1, 3)
                );
            }
            if n == CI_RUN {
                let _ = writeln!(
                    out,
                    "{} {step:<13}| job {job} aborted: step {step} failed after 3 attempts",
                    clock(&mut rng)
                );
                for rest in CI_STEPS.iter().skip_while(|s| **s != step).skip(1) {
                    let _ = writeln!(out, "{} {rest:<13}| skipped", clock(&mut rng));
                }
                return out;
            }
            let _ = writeln!(out, "{} {step:<13}| step passed (attempt 2/3)", clock(&mut rng));
        } else {
            let _ = writeln!(out, "{} {step:<13}| step passed", clock(&mut rng));
        }
    }
    let _ = writeln!(out, "{} job {job} finished: success", clock(&mut rng));
    out
}

/// A code generator owned by another team: on every change in src/ it prints ~150 notes that have
/// nothing to do with the work, so each build after an edit adds them to the history.
pub(super) fn codegen_build() -> String {
    let mut rng = Rng::new(4242);
    let cols = ["sku", "qty", "location", "unit", "price", "batch", "expiry", "supplier", "lot"];
    let mut s = String::from(
        "// Generated-table checks of the warehouse schema. Owned by the platform team; do not edit.\nfn main() {\n    \
         println!(\"cargo:rerun-if-changed=src\");\n    for note in NOTES {\n        \
         println!(\"cargo:warning=codegen: {note}\");\n    }\n}\n\nconst NOTES: &[&str] = &[\n",
    );
    for i in 0..150 {
        let note = match rng.below(4) {
            0 => format!(
                "table t{i:03} column `{}` is deprecated since schema v{}.{}; regenerate with --schema {:08x}",
                rng.pick(&cols),
                rng.int(2, 9),
                rng.int(0, 20),
                rng.next_u64() % 0xffff_ffff
            ),
            1 => format!(
                "table t{i:03} has {} rows without an index on `{}` (informational)",
                rng.int(100, 99_999),
                rng.pick(&cols)
            ),
            2 => format!("mapping t{i:03}.{} -> legacy_{} kept for compatibility", rng.pick(&cols), rng.pick(&cols)),
            _ => format!(
                "table t{i:03} checksum {:016x} differs from the cached one, using the cached one",
                rng.next_u64()
            ),
        };
        let _ = writeln!(s, "    {note:?},");
    }
    s + "];\n"
}

pub(super) fn noisy_turns() -> Vec<(u32, String)> {
    let mut t = marathon_turns();
    t.insert(
        1,
        "Next: last night's CI logs are in ci/ (run-01.log to run-12.log). Most runs show errors that a retry fixed; \
         exactly one run really failed. Find it: I will need its run number and its job id at the end. Leave \
         build.rs as it is, it belongs to another team."
            .into(),
    );
    let last = t.len() - 1;
    t[last] += " Also add the keys ci_run (the number of the CI run that really failed, as a number) and ci_job (that \
                run's job id, like \"j-1a2b3c\").";
    t.into_iter().map(|t| (1, t)).collect()
}

pub(super) fn marathon_noisy() -> Task {
    let mut files = crate_files();
    files.extend(specs_scaled(20));
    files.push(("build.rs".into(), codegen_build()));
    files.extend((1..=12).map(|n| (format!("ci/run-{n:02}.log"), ci_log(n))));
    Task {
        id: "marathon-noisy",
        trap: "the heavy marathon plus junk: a build script that prints 150 useless notes after every edit and \
               twelve long CI logs with one fact in them, asked for at the very end; the history passes 200k \
               tokens early and most of it is noise",
        group: "heavy",
        files,
        turns: noisy_turns(),
        check: noisy_check,
        solution: noisy_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

// ------------------------------------------------------------------ the pasted marathon: the junk in the requests

/// The noisy marathon with its junk pasted into the requests instead of left in files: the twelve CI
/// logs in the second request and each specification in its own. What a person pastes stays in every
/// request that follows, whatever the agent reads, so the history passes 200k tokens in every run.
pub(super) fn pasted_turns() -> Vec<(u32, String)> {
    let specs = specs_scaled(20);
    let logs: String = (1..=12).map(|n| format!("--- run-{n:02}.log ---\n{}", ci_log(n))).collect();
    noisy_turns()
        .into_iter()
        .map(|(s, t)| {
            if t.contains("CI logs are in ci/") {
                return (
                    s,
                    format!(
                        "Next: here are last night's CI logs, all twelve runs, pasted. Most runs show errors that a \
                         retry fixed; exactly one run really failed. Find it: I will need its run number and its \
                         job id at the end. Leave build.rs as it is, it belongs to another team.\n\n```text\n{logs}```"
                    ),
                );
            }
            match specs.iter().find(|(p, _)| t.contains(p.as_str())) {
                Some((p, body)) => (
                    s,
                    format!(
                        "{}\n\nThe specification, pasted from the wiki:\n\n```markdown\n{body}```",
                        t.replace(p.as_str(), "the specification below")
                    ),
                ),
                None => (s, t),
            }
        })
        .collect()
}

pub(super) fn marathon_pasted() -> Task {
    let mut files = crate_files();
    files.push(("build.rs".into(), codegen_build()));
    Task {
        id: "marathon-pasted",
        trap: "the noisy marathon with the junk pasted into the requests: twelve CI logs and five long \
               specifications stay in every later request whatever the agent reads, so the history passes 200k \
               tokens in every run and a summary at 200k must happen",
        group: "heavy",
        files,
        turns: pasted_turns(),
        check: noisy_check,
        solution: noisy_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) fn noisy_check(wd: &Path) -> Outcome {
    let out = marathon_check(wd);
    if !out.ok {
        return out;
    }
    let rel: serde_json::Value = serde_json::from_str(&read(wd, "RELEASE.json")).unwrap_or_default();
    let run = rel["ci_run"].as_f64().or_else(|| {
        rel["ci_run"].as_str().and_then(|s| s.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok())
    });
    let job = rel["ci_job"].as_str().unwrap_or("").trim().to_string();
    Outcome::all(vec![
        unchanged(wd, "build.rs", &codegen_build()),
        (run == Some(CI_RUN as f64), format!("RELEASE.json: ci_run {}", rel["ci_run"])),
        (job == ci_job(CI_RUN), format!("RELEASE.json: ci_job {}", rel["ci_job"])),
    ])
}

pub(super) fn noisy_solution(wd: &Path) {
    marathon_solution(wd);
    let mut rel: serde_json::Value =
        serde_json::from_str(&read(wd, "RELEASE.json")).expect("the solution's RELEASE.json");
    rel["ci_run"] = CI_RUN.into();
    rel["ci_job"] = ci_job(CI_RUN).into();
    std::fs::write(wd.join("RELEASE.json"), rel.to_string()).expect("a writable working directory");
}
