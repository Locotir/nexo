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
        // the exercises' `./gradlew test`, with the gradle it names, fetched beforehand (the box has no network)
        Lang::Java => "gradle test --offline",
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
    let (code, raw, timed_out) = run_line(&line, TIMEOUT_S)?;
    let seconds = started.elapsed().as_secs_f64();
    if timed_out {
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

/// The GNU `timeout` program: `timeout` on Linux, `gtimeout` where Homebrew coreutils is installed (a stock Mac
/// has neither), None when there is none.
fn gnu_timeout() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        return Some("timeout");
    }
    Command::new("gtimeout").arg("--version").output().is_ok_and(|o| o.status.success()).then_some("gtimeout")
}

/// Runs the shell line for at most `secs` seconds. Gives the exit code, the output (standard error included by
/// the line itself) and whether the time ran out.
fn run_line(line: &str, secs: u64) -> Result<(Option<i32>, String, bool), String> {
    let Some(program) = gnu_timeout() else { return run_with_deadline(line, secs) };
    let out = Command::new(program)
        .args(["-k", "5", &secs.to_string(), "sh", "-c", line])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    let code = out.status.code();
    // timeout(1) exits 124 when the command ran out of time, 137 when it had to be killed
    let timed_out = matches!(code, Some(124) | Some(137));
    Ok((code, String::from_utf8_lossy(&out.stdout).into_owned(), timed_out))
}

/// The same without a `timeout` program: the line leads its own process group, and when the time is up the
/// whole group is killed, so what the tests started goes too.
#[cfg(unix)]
fn run_with_deadline(line: &str, secs: u64) -> Result<(Option<i32>, String, bool), String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::time::{Duration, Instant};
    let mut child = Command::new("sh")
        .args(["-c", line])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sh: {e}"))?;
    let mut pipe = child.stdout.take().ok_or("no output pipe")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(secs);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("sh: {e}"))? {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", child.id())]).status();
            let _ = child.kill();
            break (child.wait().map_err(|e| format!("sh: {e}"))?, true);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // after a group kill every holder of the pipe is gone, so the reader ends
    let bytes = reader.join().unwrap_or_default();
    Ok((status.code(), String::from_utf8_lossy(&bytes).into_owned(), timed_out))
}

#[cfg(not(unix))]
fn run_with_deadline(_line: &str, _secs: u64) -> Result<(Option<i32>, String, bool), String> {
    Err("running the tests needs a `timeout` program on this system".into())
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
        Lang::Java => {
            // "31 tests completed, 2 failed, 1 skipped" on a failure; each test's "PASSED" line on a pass
            if output.contains("Compilation failed") || output.contains("compileJava FAILED") {
                return "did not compile".into();
            }
            let re = Regex::new(r"(\d+) tests? completed, (\d+) failed(?:, (\d+) skipped)?").unwrap();
            if let Some(c) = re.captures(output) {
                let (all, failed) = (n(&c, 1), n(&c, 2));
                return tally(all.saturating_sub(failed + n(&c, 3)), failed);
            }
            let passed = output.lines().filter(|l| l.trim_end().ends_with(" PASSED")).count() as u32;
            if output.contains("BUILD SUCCESSFUL") {
                tally(passed, 0)
            } else {
                "no test summary".into()
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

    #[cfg(unix)]
    #[test]
    fn a_line_that_outlives_its_deadline_is_killed_and_said_to_have_timed_out() {
        let t = std::time::Instant::now();
        let (_, _, timed_out) = run_with_deadline("sleep 5", 1).unwrap();
        assert!(timed_out);
        assert!(t.elapsed() < std::time::Duration::from_secs(4), "{:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn a_line_that_ends_in_time_gives_its_output_and_code() {
        let (code, out, timed_out) = run_with_deadline("echo hi; exit 3", 20).unwrap();
        assert_eq!((code, out.trim(), timed_out), (Some(3), "hi", false));
    }

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
    fn gradle_summaries_cover_a_pass_a_fail_and_a_compile_error() {
        let ok = "BobTest > a() PASSED\nBobTest > b() PASSED\n\nBUILD SUCCESSFUL in 4s\n";
        assert_eq!(summarize(Lang::Java, ok), "2 passed, 0 failed");
        let bad = "BobTest > a() FAILED\n    org.opentest4j.AssertionFailedError\n\n7 tests completed, 2 failed, 1 skipped\n\nFAILURE: Build failed with an exception.\n";
        assert_eq!(summarize(Lang::Java, bad), "4 passed, 2 failed");
        let broken = "> Task :compileJava FAILED\nBob.java:3: error: ';' expected\nCompilation failed; see the compiler error output for details.\n";
        assert_eq!(summarize(Lang::Java, broken), "did not compile");
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
        assert_eq!(command(Lang::Java), "gradle test --offline");
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
