use super::*;

#[test]
fn the_heavy_specs_hold_twenty_times_the_examples() {
    let bytes = |s: Vec<(String, String)>| s.iter().map(|(_, t)| t.len()).sum::<usize>();
    let (one, eight) = (bytes(specs()), bytes(specs_scaled(20)));
    eprintln!("spec bytes: {one} and {eight}");
    assert!(eight > 15 * one, "{one} {eight}");
    assert!(specs_scaled(20)[0].1.starts_with(&specs()[0].1[..200]));
}

/// Slow: builds the crate (run with `cargo test -p nexo-bench -- --ignored noisy`).
#[test]
#[ignore]
fn the_noisy_checker_accepts_its_solution_and_rejects_the_start() {
    let task = marathon_noisy();
    let wd = tempfile::tempdir().unwrap();
    task.materialize(wd.path()).unwrap();
    assert!(!(task.check)(wd.path()).ok);
    (task.solution)(wd.path());
    let got = (task.check)(wd.path());
    assert!(got.ok, "{}", got.detail);
    let t = Copy::of(wd.path()).unwrap().cargo(&["build"]);
    assert!(t.stdout.contains("codegen:") || t.stderr.contains("codegen:"));
}

#[test]
fn the_pasted_marathon_sends_more_than_200k_tokens_of_requests() {
    let t = pasted_turns();
    let bytes: usize = t.iter().map(|(_, s)| s.len()).sum();
    eprintln!("pasted request bytes: {bytes}");
    assert!(bytes > 800_000, "{bytes}");
    assert_eq!(t.len(), 10);
    assert!(t.iter().all(|(_, s)| !s.contains("docs/spec/")));
    assert!(t[1].1.contains(&ci_job(CI_RUN)));
}

#[test]
fn one_ci_run_really_failed_and_the_build_script_is_noisy() {
    let logs: Vec<String> = (1..=12).map(ci_log).collect();
    let aborted: Vec<usize> = (1..=12).filter(|n| logs[n - 1].contains("aborted")).collect();
    assert_eq!(aborted, [CI_RUN]);
    assert!(logs[CI_RUN - 1].contains(&format!("job {} aborted", ci_job(CI_RUN))));
    let mut ids: Vec<String> = (1..=12).map(ci_job).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 12);
    let bytes: usize = logs.iter().map(String::len).sum();
    eprintln!("ci bytes: {bytes}, build.rs bytes: {}", codegen_build().len());
    assert!(bytes > 250_000, "{bytes}");
    assert_eq!(codegen_build().lines().filter(|l| l.starts_with("    \"")).count(), 150);
}
