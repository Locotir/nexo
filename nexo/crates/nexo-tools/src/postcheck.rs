//! Proof that comes back with a change: after a step that changed source files, the compiler errors and the
//! project's own tests run once, bounded, and their result is appended to the result of the call that changed
//! the files, so the model does not spend a step asking for it. Also the proof command of `edit`/`write`
//! (`check`), the snapshot that tells which files a step changed, and the memory that keeps a slow command
//! from being run again at every step.

use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Lines of output kept in a check's report.
pub const TAIL_LINES: usize = 40;
/// Characters of output kept in a check's report.
pub const TAIL_CHARS: usize = 3000;
/// Source files looked at to tell what a step changed.
pub const MAX_FILES: usize = 5000;

/// Extensions of the files whose change makes a step "changed code".
const SOURCE: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "go", "java", "kt", "c", "h", "cc", "cpp", "hpp", "cs", "rb",
    "php", "swift", "scala", "sh", "toml", "json", "yaml", "yml", "csv", "tsv", "sql", "lua", "zig", "ex", "exs", "hs",
    "ml",
];
/// Folders that hold build output, dependencies or tool state, never walked.
const SKIP_DIRS: &[&str] =
    &["target", "node_modules", "dist", "build", "out", "__pycache__", "venv", "vendor", "coverage"];

/// A file as a snapshot knows it: modified time (ns) and size.
pub type Snapshot = BTreeMap<PathBuf, (u128, u64)>;

/// mtime and size of the source files under `root` (hidden folders, build output and dependencies left out),
/// at most [`MAX_FILES`]; past that, in a git repository, the files `git status` lists.
pub fn snapshot(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut stack = vec![root.to_path_buf()];
    let mut over = false;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ty) = e.file_type() else { continue };
            let name = e.file_name();
            let name = name.to_string_lossy();
            if ty.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    stack.push(e.path());
                }
            } else if ty.is_file() && is_source(&e.path()) {
                if out.len() >= MAX_FILES {
                    over = true;
                    break;
                }
                if let Ok(m) = e.metadata() {
                    out.insert(e.path(), stamp(&m));
                }
            }
        }
        if over {
            break;
        }
    }
    if over {
        if let Some(listed) = git_changed(root) {
            return listed;
        }
    }
    out
}

fn stamp(m: &std::fs::Metadata) -> (u128, u64) {
    let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    (t, m.len())
}

fn is_source(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| SOURCE.contains(&e))
}

/// The source files `git status --porcelain` lists, with their stamps.
fn git_changed(root: &Path) -> Option<Snapshot> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let mut s = Snapshot::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(rel) = line.get(3..) else { continue };
        let rel = rel.rsplit(" -> ").next().unwrap_or(rel).trim_matches('"');
        let p = root.join(rel);
        if is_source(&p) {
            if let Ok(m) = std::fs::metadata(&p) {
                s.insert(p, stamp(&m));
            }
        }
    }
    Some(s)
}

/// The files that are new, gone or different in `after` compared with `before`.
pub fn changed(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        after.iter().filter(|(p, v)| before.get(*p) != Some(v)).map(|(p, _)| p.clone()).collect();
    out.extend(before.keys().filter(|p| !after.contains_key(*p)).cloned());
    out.sort();
    out
}

/// A number that is the same for the same snapshot.
pub fn fingerprint(s: &Snapshot) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// The last [`TAIL_LINES`] lines of `text`, and no more than [`TAIL_CHARS`] characters, cut at a line.
pub fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    let mut kept: Vec<&str> = lines[from..].to_vec();
    let mut size: usize = kept.iter().map(|l| l.len() + 1).sum();
    while size > TAIL_CHARS && kept.len() > 1 {
        size -= kept.remove(0).len() + 1;
    }
    let mut out = kept.join("\n");
    if out.len() > TAIL_CHARS {
        let cut = out.len() - TAIL_CHARS;
        let at = (cut..out.len()).find(|i| out.is_char_boundary(*i)).unwrap_or(0);
        out = out[at..].to_string();
    }
    let omitted = lines.len() - kept.len();
    if omitted > 0 {
        out = format!("… {omitted} earlier lines\n{out}");
    }
    out
}

/// What a check that passed without running a test says.
pub const ZERO_TESTS: &str = "0 tests ran: it compiles, nothing exercised";

/// Whether the output of a passing check says no test ran and nothing else needs the model's eyes: the
/// summary lines of cargo (`0 passed; 0 failed`), pytest (`no tests ran`), go (`[no test files]`) or
/// jest (`No tests found`), and no warnings.
pub fn ran_no_tests(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    if lower.contains("warning") {
        return false;
    }
    let results: Vec<&str> = output.lines().filter(|l| l.trim_start().starts_with("test result:")).collect();
    if !results.is_empty() {
        return results.iter().all(|l| l.contains(" 0 passed; 0 failed"));
    }
    lower.contains("no tests ran") || lower.contains("no tests found") || lower.contains("collected 0 items")
}

/// Most failing tests named in a report's summary line.
const SUMMARY_NAMES: usize = 8;
/// Said instead of a check output that is the same as the one reported before in the turn.
const SAME_OUTPUT: &str = "same output as the check above";

/// `failed 3 of 12: a, b, c` from the output of a test run, when it names failures: cargo (`---- name stdout
/// ----`, `test name ... FAILED`, `test result:`), pytest (`FAILED path::name`, `2 failed, 5 passed`) and go
/// (`--- FAIL: Name`). The total is left out where the output has none (go); names past eight are counted.
pub fn summary(output: &str) -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    let (mut passed, mut failed, mut totals) = (0u64, 0u64, false);
    let mut add = |n: &str| {
        let n = n.trim();
        if !n.is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for line in output.lines() {
        let l = line.trim();
        if let Some(n) = l.strip_prefix("test ").and_then(|r| r.strip_suffix(" ... FAILED")) {
            add(n);
        } else if let Some(n) = l.strip_prefix("---- ").and_then(|r| r.strip_suffix(" stdout ----")) {
            add(n);
        } else if let Some(r) = l.strip_prefix("--- FAIL: ") {
            // a failing subtest is told by its parent
            let n = r.split_whitespace().next().unwrap_or_default();
            if !n.contains('/') {
                add(n);
            }
        } else if let Some(r) = l.strip_prefix("FAILED ") {
            add(r.split(" - ").next().unwrap_or(r));
        } else if let Some(r) = l.strip_prefix("test result:") {
            let count = |word: &str| -> u64 {
                r.split(';')
                    .find_map(|p| {
                        let p = p.trim().trim_start_matches(|c: char| !c.is_ascii_digit());
                        p.strip_suffix(word).and_then(|n| n.trim().parse().ok())
                    })
                    .unwrap_or(0)
            };
            passed += count("passed");
            failed += count("failed");
            totals = true;
        } else if let Some((p, f)) = pytest_totals(l) {
            passed += p;
            failed += f;
            totals = true;
        }
    }
    if names.is_empty() && failed == 0 {
        return None;
    }
    let n = if failed > 0 { failed.max(names.len() as u64) } else { names.len() as u64 };
    let mut out = format!("failed {n}");
    if totals && passed + failed >= n {
        out.push_str(&format!(" of {}", passed + failed));
    }
    if !names.is_empty() {
        let shown: Vec<&str> = names.iter().take(SUMMARY_NAMES).map(String::as_str).collect();
        out.push_str(&format!(": {}", shown.join(", ")));
        if names.len() > SUMMARY_NAMES {
            out.push_str(&format!(", +{} more", names.len() - SUMMARY_NAMES));
        }
    }
    Some(out)
}

/// `(passed, failed)` from a pytest closing line (`2 failed, 5 passed in 0.1s`), when `l` is one.
fn pytest_totals(l: &str) -> Option<(u64, u64)> {
    let l = l.trim_matches(|c: char| c == '=' || c.is_whitespace());
    let l = l.split(" in ").next().unwrap_or(l);
    let (mut p, mut f, mut seen) = (0u64, 0u64, false);
    for part in l.split(',') {
        let mut w = part.split_whitespace();
        let n: u64 = w.next()?.parse().ok()?;
        match w.next()? {
            "passed" => p = n,
            "failed" => f = n,
            "skipped" | "error" | "errors" | "xfailed" | "xpassed" | "warnings" | "warning" | "deselected" => {}
            _ => return None,
        }
        seen = true;
    }
    seen.then_some((p, f))
}

/// `\ncheck: cargo test -q | exit 1 | 3.2 s | failed 2 of 9: a, b\n<tail>`: how a check is reported.
pub fn report(command: &str, exit: Option<i32>, timed_out: bool, secs: f64, cap: Duration, output: &str) -> String {
    render(command, exit, timed_out, secs, cap, output, false)
}

fn render(
    command: &str,
    exit: Option<i32>,
    timed_out: bool,
    secs: f64,
    cap: Duration,
    output: &str,
    repeat: bool,
) -> String {
    let code = match (exit, timed_out) {
        (Some(c), _) => c.to_string(),
        (None, true) => format!("killed at the {} s cap", cap.as_secs()),
        (None, false) => "none".into(),
    };
    let body = if repeat {
        SAME_OUTPUT.to_string()
    } else if exit == Some(0) && ran_no_tests(output) {
        ZERO_TESTS.to_string()
    } else {
        tail(output)
    };
    let body = if body.is_empty() { "(no output)".to_string() } else { body };
    let failed = summary(output).map(|s| format!(" | {s}")).unwrap_or_default();
    format!("\ncheck: {command} | exit {code} | {secs:.1} s{failed}\n{body}")
}

/// A check report cut to `max` characters: its first line, and for a failing one the end of its output.
pub fn brief(report: &str, max: usize) -> String {
    let r = report.trim_start();
    let (head, body) = r.split_once('\n').unwrap_or((r, ""));
    if head.contains("| exit 0 |") || body.trim().is_empty() {
        return head.to_string();
    }
    let room = max.saturating_sub(head.len() + 1);
    let mut start = body.len().saturating_sub(room);
    while !body.is_char_boundary(start) {
        start += 1;
    }
    // begin at a line
    if start > 0 {
        if let Some(i) = body[start..].find('\n') {
            start += i + 1;
        }
    }
    format!("{head}\n{}", &body[start..])
}

/// `command` without the flags that only change how failures are collected, and without `2>&1`.
fn normal(command: &str) -> String {
    let c = command.split_whitespace().filter(|w| *w != "--no-fail-fast").collect::<Vec<_>>().join(" ");
    c.strip_suffix(" 2>&1").map(str::to_string).unwrap_or(c)
}

/// What the agent remembers between steps: commands that ran over the cap, and the last check that ran.
#[derive(Default)]
pub struct Guard {
    state: Mutex<GuardState>,
}

#[derive(Default)]
struct GuardState {
    slow: HashSet<String>,
    last: Option<Recorded>,
    /// The output (hashed) of the last check reported in this turn.
    said: Option<u64>,
}

struct Recorded {
    command: String,
    fingerprint: u64,
    text: String,
}

fn key(command: &str, dir: &Path) -> String {
    format!("{}\u{0}{}", dir.display(), command.trim())
}

impl Guard {
    pub fn new() -> Guard {
        Guard::default()
    }

    /// Why `command` is not run: it ran past the cap earlier in this session.
    pub fn skip_note(&self, command: &str, dir: &Path, cap: Duration) -> Option<String> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.slow.contains(&key(command, dir)).then(|| {
            format!(
                "\ncheck: {command} | skipped | tests not run here: the last run took over {} s; run them yourself in the background",
                cap.as_secs()
            )
        })
    }

    /// Notes a run: one that hit the cap is not run again by the checks.
    pub fn record_run(&self, command: &str, dir: &Path, timed_out: bool) {
        if timed_out {
            self.state.lock().unwrap_or_else(|e| e.into_inner()).slow.insert(key(command, dir));
        }
    }

    /// Keeps the last check's report with the state of the files it ran on.
    pub fn remember(&self, command: &str, fingerprint: u64, text: &str) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).last =
            Some(Recorded { command: command.trim().to_string(), fingerprint, text: text.to_string() });
    }

    /// The report of the last check when `command` is exactly that command and the files are as they were.
    pub fn recorded(&self, command: &str, fingerprint: u64) -> Option<String> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let r = st.last.as_ref()?;
        (r.fingerprint == fingerprint && normal(&r.command) == normal(command)).then(|| r.text.clone())
    }

    /// A new request starts: what was said about check output before is not "above" any more.
    pub fn new_turn(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).said = None;
    }

    /// [`report`], with the output replaced by a line when it is the one the last report of this turn showed.
    pub fn report(
        &self,
        command: &str,
        exit: Option<i32>,
        timed_out: bool,
        secs: f64,
        cap: Duration,
        output: &str,
    ) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        output.hash(&mut h);
        let h = h.finish();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // a short output costs less than the line that replaces it
        let repeat = st.said == Some(h) && tail(output).len() >= 60;
        st.said = Some(h);
        render(command, exit, timed_out, secs, cap, output, repeat)
    }

    /// Runs `command` in `dir` within `cap` and reports it; a command that ran over the cap before is skipped.
    pub async fn run(
        &self,
        command: &str,
        dir: &Path,
        cap: Duration,
        sandbox: &nexo_core::sandbox::SandboxSettings,
        cancel: &CancellationToken,
    ) -> String {
        if let Some(note) = self.skip_note(command, dir, cap) {
            return note;
        }
        let p = crate::bash::prove(command, dir, sandbox, cap, cancel, None).await;
        self.record_run(command, dir, p.timed_out);
        self.report(command, p.exit, p.timed_out, p.secs, cap, &p.output)
    }
}

/// The proof command of an `edit` or `write` call (its `check` argument), run in the call's folder.
pub async fn run_own(command: &str, ctx: &nexo_core::ToolContext) -> String {
    let cap = Duration::from_secs(ctx.check_secs.max(1));
    let p = crate::bash::prove(
        command,
        &ctx.cwd,
        &ctx.sandbox,
        cap,
        &ctx.cancel,
        Some(&crate::bash::tmp_dir(&ctx.session_id)),
    )
    .await;
    report(command, p.exit, p.timed_out, p.secs, cap, &p.output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_tests_are_said_in_one_sentence() {
        let out = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = report("cargo test -q", Some(0), false, 1.0, Duration::from_secs(45), out);
        assert_eq!(r, "\ncheck: cargo test -q | exit 0 | 1.0 s\n0 tests ran: it compiles, nothing exercised");
        // tests that ran are shown as they are, and so is a failure
        let ran = "running 2 tests\n\ntest result: ok. 2 passed; 0 failed; 0 ignored\n";
        assert!(report("cargo test", Some(0), false, 1.0, Duration::from_secs(45), ran).contains("2 passed"));
        let bad = "running 0 tests\nerror: could not compile\n";
        assert!(!report("cargo test", Some(101), false, 1.0, Duration::from_secs(45), bad).contains("0 tests ran"));
    }

    #[tokio::test]
    async fn post_write_check_skips_a_command_that_exceeded_the_cap_before() {
        if cfg!(windows) {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let g = Guard::new();
        let cap = Duration::from_secs(1);
        let sb = Default::default();
        let cancel = CancellationToken::new();
        let first = g.run("sleep 5; echo late", d.path(), cap, &sb, &cancel).await;
        assert!(first.starts_with("\ncheck: sleep 5; echo late | exit killed at the 1 s cap | 1."), "{first}");
        let again = g.run("sleep 5; echo late", d.path(), cap, &sb, &cancel).await;
        assert!(again.contains("| skipped | tests not run here: the last run took over 1 s"), "{again}");
        // another command is not affected, and a fast one reports its exit code and output
        let ok = g.run("echo fine", d.path(), cap, &sb, &cancel).await;
        assert!(ok.starts_with("\ncheck: echo fine | exit 0 | 0.") && ok.ends_with("\nfine"), "{ok}");
    }

    #[test]
    fn a_report_keeps_the_tail_and_a_repeat_of_the_check_gets_the_record() {
        let long: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let t = tail(&long);
        assert!(t.starts_with("… 60 earlier lines\nline 61") && t.ends_with("line 100"), "{t}");
        let wide = "x".repeat(10_000);
        assert!(tail(&wide).len() <= TAIL_CHARS + 30);
        let g = Guard::new();
        g.remember("cargo test -q", 7, "RECORDED");
        assert_eq!(g.recorded("cargo  test -q 2>&1", 7).as_deref(), Some("RECORDED"));
        assert_eq!(g.recorded("cargo test -q", 8), None, "files changed");
        assert_eq!(g.recorded("cargo test", 7), None, "another command");
    }

    #[test]
    fn a_snapshot_sees_changed_new_and_removed_source_files_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::create_dir_all(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), "a").unwrap();
        std::fs::write(d.path().join("src/b.rs"), "b").unwrap();
        std::fs::write(d.path().join("notes.txt"), "n").unwrap();
        let before = snapshot(d.path());
        assert_eq!(before.len(), 2);
        std::fs::write(d.path().join("src/a.rs"), "aa").unwrap();
        std::fs::remove_file(d.path().join("src/b.rs")).unwrap();
        std::fs::write(d.path().join("src/c.rs"), "c").unwrap();
        std::fs::write(d.path().join("target/x.rs"), "x").unwrap();
        std::fs::write(d.path().join("notes.txt"), "changed").unwrap();
        let after = snapshot(d.path());
        let names: Vec<String> =
            changed(&before, &after).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.rs", "b.rs", "c.rs"]);
        assert_ne!(fingerprint(&before), fingerprint(&after));
        assert_eq!(fingerprint(&after), fingerprint(&snapshot(d.path())));
    }

    const CARGO_FAIL: &str = "\nrunning 2 tests\n.F\nfailures:\n\n---- adds stdout ----\nthread 'adds' panicked at src/lib.rs:9:9:\nassertion `left == right` failed\n\nfailures:\n    adds\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n\nrunning 1 test\nF\nfailures:\n\n---- subs stdout ----\npanicked\n\nfailures:\n    subs\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";

    #[test]
    fn a_failing_report_says_how_many_failed_and_which() {
        let r = report("cargo test -q --no-fail-fast", Some(101), false, 2.3, Duration::from_secs(45), CARGO_FAIL);
        assert!(
            r.starts_with("\ncheck: cargo test -q --no-fail-fast | exit 101 | 2.3 s | failed 2 of 3: adds, subs\n"),
            "{r}"
        );
        let ok = report(
            "cargo test -q",
            Some(0),
            false,
            1.0,
            Duration::from_secs(45),
            "test result: ok. 3 passed; 0 failed",
        );
        assert!(ok.starts_with("\ncheck: cargo test -q | exit 0 | 1.0 s\n"), "{ok}");
        let py = "FAILED tests/test_a.py::test_one - assert 1 == 2\nFAILED tests/test_a.py::test_two - x\n2 failed, 5 passed in 0.1s";
        let r = report("pytest -q -x", Some(1), false, 0.2, Duration::from_secs(45), py);
        assert!(r.contains("| failed 2 of 7: tests/test_a.py::test_one, tests/test_a.py::test_two\n"), "{r}");
        let go = "--- FAIL: TestA (0.00s)\n--- FAIL: TestB (0.00s)\nFAIL";
        let r = report("go test ./...", Some(1), false, 0.2, Duration::from_secs(45), go);
        assert!(r.contains("| failed 2: TestA, TestB\n"), "{r}");
        let names: String = (1..=12).map(|i| format!("--- FAIL: T{i} (0s)\n")).collect();
        let r = report("go test ./...", Some(1), false, 0.2, Duration::from_secs(45), &names);
        assert!(r.contains("failed 12: T1, T2, T3, T4, T5, T6, T7, T8, +4 more\n"), "{r}");
    }

    #[test]
    fn identical_check_output_is_said_once() {
        let g = Guard::new();
        let cap = Duration::from_secs(45);
        let first = g.report("cargo test -q", Some(101), false, 1.0, cap, CARGO_FAIL);
        assert!(first.contains("---- adds stdout ----"), "{first}");
        let again = g.report("cargo test -q", Some(101), false, 1.2, cap, CARGO_FAIL);
        assert!(again.contains("| failed 2 of 3: adds, subs\nsame output as the check above"), "{again}");
        assert!(!again.contains("---- adds stdout ----"));
        // another output is shown, and a new turn starts again
        let other = g.report("cargo test -q", Some(101), false, 1.0, cap, &CARGO_FAIL.replace("adds", "adds2"));
        assert!(other.contains("adds2 stdout"), "{other}");
        let _ = g.report("cargo test -q", Some(101), false, 1.0, cap, CARGO_FAIL);
        g.new_turn();
        assert!(g.report("cargo test -q", Some(101), false, 1.0, cap, CARGO_FAIL).contains("adds stdout"));
        // the same command with or without the flag finds the record
        g.remember("cargo test -q --no-fail-fast", 3, "R");
        assert_eq!(g.recorded("cargo test -q", 3).as_deref(), Some("R"));
    }
}
