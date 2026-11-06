//! Running an exercise's tests as the benchmark does (`run_unit_tests` in aider's `benchmark/benchmark.py`):
//! one command per language, three minutes at most, standard output and error together, and the exercise
//! passes when the command exits 0. The commands run in the same box as the agent's (no network, nothing
//! outside the working directory).

use std::path::Path;
use std::process::{Command, Stdio};

use regex::Regex;

use super::exercise::Lang;
use super::prompts;
use crate::seal::Seal;

/// The seconds the tests may take (`timeout = 60 * 3`).
pub const TIMEOUT_S: u64 = 180;

/// The test command of `lang`, as a shell line run in the exercise's folder. These are the benchmark's:
/// `pytest`; `cargo test -- --include-ignored`; `go test ./...`; `npm run test` (the benchmark's
/// `npm-test.sh`, whose `xtest(` rewrite is done when the tests are installed); for C++ the steps of
/// `cpp-test.sh` (all tests enabled). Environment variables only keep the toolchains off the network.
pub fn command(lang: Lang) -> &'static str {
    match lang {
        Lang::Python => "pytest",
        Lang::Rust => "CARGO_NET_OFFLINE=true cargo test -- --include-ignored",
        Lang::Go => "GOTOOLCHAIN=local GOPROXY=off GOFLAGS=-mod=mod go test ./...",
        Lang::Cpp => "set -e; [ ! -d build ] && mkdir build; cd build; cmake -DEXERCISM_RUN_ALL_TESTS=1 -G \"Unix Makefiles\" ..; make",
        Lang::Javascript => "npm run test",
    }
}

/// What running the tests gave.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Exit status 0 within the time.
    pub pass: bool,
    pub timed_out: bool,
    /// The output as the model is shown it: cleaned and capped. `Tests timed out!` on a time-out.
    pub output: String,
    /// One line on what ran, read from the output (`12 passed, 3 failed`, `did not compile`).
    pub summary: String,
    pub seconds: f64,
}

/// Runs the tests of `lang` in `wd` inside `seal`. The test files must be in place already.
pub fn run_tests(lang: Lang, wd: &Path, seal: &Seal) -> Result<Outcome, String> {
    let started = std::time::Instant::now();
    let line = format!("{} 2>&1", seal.wrap(command(lang)));
    let out = Command::new("timeout")
        .args(["-k", "5", &TIMEOUT_S.to_string(), "sh", "-c", &line])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("timeout: {e}"))?;
    let seconds = started.elapsed().as_secs_f64();
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    let code = out.status.code();
    // timeout(1) exits 124 when the command ran out of time, 137 when it had to be killed
    if matches!(code, Some(124) | Some(137)) {
        return Ok(Outcome {
            pass: false,
            timed_out: true,
            output: prompts::TIMED_OUT.into(),
            summary: "timed out".into(),
            seconds,
        });
    }
    let summary = with_crash(summarize(lang, &raw), &raw);
    let output = prompts::cap_output(&prompts::cleanup_output(&raw, wd));
    Ok(Outcome { pass: code == Some(0), timed_out: false, output, summary, seconds })
}

/// Text a test run prints when the test process itself died (a signal, a stack overflow, a Go panic), not
/// when a test failed.
const CRASHES: [&str; 6] = [
    "(signal: ",
    "fatal runtime error",
    "fatal error: stack overflow",
    "\npanic: ",
    "Segmentation fault",
    "fatal error condition",
];

/// `summary` with a note when the test process crashed: the counts before it are not the whole run.
pub fn with_crash(summary: String, output: &str) -> String {
    let crashed = CRASHES.iter().any(|m| output.contains(m));
    match summary.as_str() {
        _ if !crashed => summary,
        "did not compile" | "did not load" => summary,
        "no test summary" => "crashed".into(),
        _ => format!("{summary}, then crashed"),
    }
}

fn count<'a>(re: &Regex, text: &'a str) -> Vec<regex::Captures<'a>> {
    re.captures_iter(text).collect()
}

/// One line on the test run in `output` (the raw output of `lang`'s command). The exit status alone decides
/// a pass; this is for the report.
pub fn summarize(lang: Lang, output: &str) -> String {
    let n = |c: &regex::Captures, i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok()).unwrap_or(0);
    let tally = |passed: u32, failed: u32| format!("{passed} passed, {failed} failed");
    match lang {
        Lang::Python => {
            // "=== 3 failed, 2 passed in 0.12s ===" or "=== 5 passed in 0.01s ==="; errors collect as "N error(s)"
            let re = Regex::new(r"(?m)^=+ (.*?)(?: in [\d.]+s.*)? =+$").unwrap();
            if output.contains("ERROR collecting") || output.contains("SyntaxError") {
                return "did not load".into();
            }
            match count(&re, output).last() {
                Some(c) => c[1].to_string(),
                None => "no test summary".into(),
            }
        }
        Lang::Rust => {
            let re = Regex::new(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored").unwrap();
            let all = count(&re, output);
            if all.is_empty() {
                return if output.contains("could not compile")
                    || output.contains("error[E")
                    || output.contains("error:")
                {
                    "did not compile".into()
                } else {
                    "no test summary".into()
                };
            }
            tally(all.iter().map(|c| n(c, 1)).sum(), all.iter().map(|c| n(c, 2)).sum())
        }
        Lang::Go => {
            if output.contains("[build failed]") || output.contains("[setup failed]") {
                return "did not compile".into();
            }
            let failed = output.lines().filter(|l| l.trim_start().starts_with("--- FAIL")).count() as u32;
            let packages_ok = output.lines().filter(|l| l.starts_with("ok")).count();
            if failed == 0 && packages_ok > 0 {
                "ok".into()
            } else if failed == 0 {
                "no test summary".into()
            } else {
                format!("{failed} failed")
            }
        }
        Lang::Cpp => {
            let all_pass = Regex::new(r"All tests passed \((\d+) assertions? in (\d+) test cases?\)").unwrap();
            let some = Regex::new(r"test cases: +(\d+) \| +(\d+) passed \| +(\d+) failed").unwrap();
            if let Some(c) = all_pass.captures(output) {
                return format!("{} passed, 0 failed", &c[2]);
            }
            if let Some(c) = some.captures(output) {
                return tally(n(&c, 2), n(&c, 3));
            }
            if output.contains("error:") || output.contains("Error 1") || output.contains("CMake Error") {
                "did not compile".into()
            } else {
                "no test summary".into()
            }
        }
        Lang::Javascript => {
            let re =
                Regex::new(r"Tests:\s+(?:(\d+) failed, )?(?:(\d+) skipped, )?(?:(\d+) passed, )?(\d+) total").unwrap();
            match re.captures(output) {
                Some(c) => tally(n(&c, 3), n(&c, 1)),
                None if output.contains("Test suite failed to run") => "did not load".into(),
                None => "no test summary".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pytest_summaries_are_read() {
        let fail = "FAILED bob_test.py::T::test_a - assert 1 == 2\n======================== 3 failed, 2 passed in 0.12s =========================\n";
        assert_eq!(summarize(Lang::Python, fail), "3 failed, 2 passed");
        assert_eq!(summarize(Lang::Python, "======= 5 passed in 0.01s =======\n"), "5 passed");
        assert_eq!(summarize(Lang::Python, "==== ERRORS ====\nERROR collecting x\n"), "did not load");
        assert_eq!(summarize(Lang::Python, "nothing"), "no test summary");
    }

    #[test]
    fn cargo_summaries_add_up_every_test_binary() {
        let ok = "running 3 tests\ntest result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n\
                  running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert_eq!(summarize(Lang::Rust, ok), "3 passed, 0 failed");
        let bad = "test result: FAILED. 2 passed; 4 failed; 0 ignored; 0 measured; 0 filtered out\n";
        assert_eq!(summarize(Lang::Rust, bad), "2 passed, 4 failed");
        assert_eq!(
            summarize(Lang::Rust, "error[E0425]: cannot find value\nerror: could not compile `x`\n"),
            "did not compile"
        );
    }

    #[test]
    fn a_crashed_test_process_is_said_so() {
        // cargo: one binary reported, the next one died of a stack overflow
        let rust = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\
                    fatal runtime error: stack overflow, aborting\nprocess didn't exit successfully: `x` (signal: 6, SIGABRT)\n";
        assert_eq!(with_crash(summarize(Lang::Rust, rust), rust), "1 passed, 0 failed, then crashed");
        let go =
            "runtime: goroutine stack exceeds 1000000000-byte limit\nfatal error: stack overflow\nFAIL\tforth\t0.7s\n";
        assert_eq!(with_crash(summarize(Lang::Go, go), go), "crashed");
        let built = "x.cpp:1: fatal error: y.h: No such file\nmake: *** [all] Error 1\n";
        assert_eq!(with_crash(summarize(Lang::Cpp, built), built), "did not compile");
        let fail = "--- FAIL: TestX (0.00s)\nFAIL\n";
        assert_eq!(with_crash(summarize(Lang::Go, fail), fail), "1 failed", "a failed test is no crash");
    }

    #[test]
    fn go_summaries_count_failed_tests_and_build_failures() {
        assert_eq!(summarize(Lang::Go, "ok  \talphametics\t0.004s\n"), "ok");
        let bad =
            "--- FAIL: TestSolve (0.00s)\n    --- FAIL: TestSolve/case1 (0.00s)\nFAIL\nFAIL\talphametics\t0.005s\n";
        assert_eq!(summarize(Lang::Go, bad), "2 failed");
        assert_eq!(summarize(Lang::Go, "FAIL\tx [build failed]\n"), "did not compile");
    }

    #[test]
    fn catch_summaries_cover_a_pass_a_fail_and_a_compile_error() {
        assert_eq!(summarize(Lang::Cpp, "All tests passed (41 assertions in 7 test cases)\n"), "7 passed, 0 failed");
        assert_eq!(
            summarize(Lang::Cpp, "test cases:  7 |  5 passed | 2 failed\nassertions: 41 | 30 passed | 11 failed\n"),
            "5 passed, 2 failed"
        );
        assert_eq!(
            summarize(Lang::Cpp, "x.cpp:3:1: error: expected ';'\nmake: *** [all] Error 2\n"),
            "did not compile"
        );
    }

    #[test]
    fn jest_summaries_read_the_tests_line() {
        assert_eq!(summarize(Lang::Javascript, "Tests:       2 failed, 9 passed, 11 total\n"), "9 passed, 2 failed");
        assert_eq!(summarize(Lang::Javascript, "Tests:       11 passed, 11 total\n"), "11 passed, 0 failed");
        assert_eq!(summarize(Lang::Javascript, "Test suite failed to run\n"), "did not load");
    }

    #[test]
    fn commands_are_the_benchmarks() {
        assert_eq!(command(Lang::Python), "pytest");
        assert!(command(Lang::Rust).ends_with("cargo test -- --include-ignored"));
        assert!(command(Lang::Go).ends_with("go test ./..."));
        assert!(
            command(Lang::Cpp).contains("-DEXERCISM_RUN_ALL_TESTS=1 -G \"Unix Makefiles\" ..")
                && command(Lang::Cpp).ends_with("make")
        );
        assert_eq!(command(Lang::Javascript), "npm run test");
    }

    #[test]
    fn a_failing_command_in_the_box_is_a_fail_and_a_slow_one_is_cut() {
        if crate::seal::available().is_err() {
            return;
        }
        let wd = tempfile::tempdir().unwrap();
        let wd = wd.path().canonicalize().unwrap();
        let seal = Seal::new(&wd, vec![], vec![]).unwrap();
        let line = |cmd: &str| format!("{} 2>&1", seal.wrap(cmd));
        let out = Command::new("sh").args(["-c", &line("echo hi; exit 3")]).output().unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
    }
}
