//! What checkers share: outcomes, running a command with a timeout, and building a copy of the
//! working directory with hidden tests added, so the agent's tree is never touched by a check.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One independent requirement of a task: whether it holds and, when it does not, why.
pub type Part = (bool, String);

/// The verdict on a finished run.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Every requirement holds.
    pub ok: bool,
    /// Partial credit in `[0, 1]`; 1 when `ok`.
    pub score: f64,
    /// "ok", or the first requirement that does not hold.
    pub detail: String,
}

impl Outcome {
    /// All or nothing: scores 1 only when every part holds.
    pub fn all(parts: Vec<Part>) -> Outcome {
        match parts.into_iter().find(|(ok, _)| !ok) {
            None => Outcome { ok: true, score: 1.0, detail: "ok".into() },
            Some((_, why)) => Outcome { ok: false, score: 0.0, detail: why },
        }
    }

    /// Partial credit: the share of the parts that hold.
    pub fn share(parts: Vec<Part>) -> Outcome {
        let n = parts.len().max(1) as f64;
        let held = parts.iter().filter(|(ok, _)| *ok).count() as f64;
        let mut out = Outcome::all(parts);
        out.score = (held / n * 10_000.0).round() / 10_000.0;
        out
    }

    /// A run that could not be checked at all.
    pub fn fail(why: impl Into<String>) -> Outcome {
        Outcome { ok: false, score: 0.0, detail: why.into() }
    }
}

/// What a finished command left.
#[derive(Debug, Default)]
pub struct Output {
    /// The exit code; `None` when it was killed or could not start.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    /// Exited with 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The end of what it printed, for a failure's detail.
    pub fn tail(&self, n: usize) -> String {
        tail(&format!("{}{}", self.stdout, self.stderr), n)
    }
}

/// The last `n` characters of `s`.
pub fn tail(s: &str, n: usize) -> String {
    let skip = s.chars().count().saturating_sub(n);
    s.chars().skip(skip).collect()
}

/// Runs `cmd`, feeding it `stdin`, and kills it after `timeout`.
pub fn run(cmd: &mut Command, stdin: Option<&str>, timeout: Duration) -> Output {
    // a checker runs the agent's code: a cleared environment with a home of its own, no key
    let home = match crate::env::Home::new() {
        Ok(h) => h,
        Err(e) => return Output { code: None, stdout: String::new(), stderr: format!("cannot start: {e}") },
    };
    crate::env::apply(cmd, &home.env());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Output { code: None, stdout: String::new(), stderr: format!("cannot start: {e}") },
    };
    let feed = stdin.map(str::to_owned).zip(child.stdin.take()).map(|(text, mut pipe)| {
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        })
    });
    let reader = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = reader(Box::new(child.stdout.take().expect("piped stdout")));
    let err = reader(Box::new(child.stderr.take().expect("piped stderr")));
    let start = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if let Some(f) = feed {
        let _ = f.join();
    }
    let mut stderr = err.join().unwrap_or_default();
    if code.is_none() && start.elapsed() >= timeout {
        stderr.push_str(&format!("\nstopped after {} s", timeout.as_secs()));
    }
    Output { code, stdout: out.join().unwrap_or_default(), stderr }
}

/// How long one `cargo` command of a checker may take.
pub const CARGO_TIMEOUT: Duration = Duration::from_secs(300);

/// A throwaway copy of a working directory (without build output), where hidden tests are added
/// and `cargo` runs with its own target folder.
pub struct Copy {
    _dir: tempfile::TempDir,
    /// The copy's root.
    pub root: PathBuf,
}

impl Copy {
    /// A copy of `wd`, leaving out `target/` and `.git/`.
    pub fn of(wd: &Path) -> std::io::Result<Copy> {
        let dir = tempfile::Builder::new().prefix("nexo-bench-check-").tempdir()?;
        let root = dir.path().join("wd");
        copy_tree(wd, &root)?;
        Ok(Copy { _dir: dir, root })
    }

    /// Writes `text` at `rel` in the copy.
    pub fn write(&self, rel: &str, text: &str) -> std::io::Result<()> {
        let p = self.root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, text)
    }

    /// Runs `cargo ARGS` offline in the copy (the flag goes right after the subcommand, before any `--`).
    pub fn cargo(&self, args: &[&str]) -> Output {
        self.cargo_env(args, &[])
    }

    /// `cargo ARGS` as [`Copy::cargo`] runs it, with the variables `env` set.
    pub fn cargo_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("cargo");
        cmd.envs(env.iter().copied());
        cmd.args(&args[..1]).arg("--offline").args(&args[1..]).current_dir(&self.root).env("CARGO_TERM_COLOR", "never");
        cmd.env("CARGO_TARGET_DIR", self.root.join("target"));
        run(&mut cmd, None, CARGO_TIMEOUT)
    }

    /// Whether the project's own tests pass.
    pub fn tests_pass(&self) -> Part {
        let out = self.cargo(&["test", "--quiet"]);
        (out.success(), format!("the tests fail: {}", out.tail(600)))
    }

    /// Adds `tests/NAME.rs` with `source`, runs it alone, and reports each test it names:
    /// `(test function, what its failure means)`. A test file that does not compile fails them all.
    pub fn hidden(&self, name: &str, source: &str, tests: &[(&str, &str)]) -> Vec<Part> {
        if let Err(e) = self.write(&format!("tests/{name}.rs"), source) {
            return tests.iter().map(|_| (false, format!("cannot write the hidden test: {e}"))).collect();
        }
        let out = self.cargo(&["test", "--test", name, "--", "--test-threads=1"]);
        tests
            .iter()
            .map(|(test, why)| {
                let passed = out.stdout.lines().any(|l| l.trim() == format!("test {test} ... ok"));
                let detail = if out.stdout.contains(&format!("test {test} ...")) {
                    format!("{why}: {}", failure_of(&out.stdout, test))
                } else {
                    format!("{why} (the code does not build with the hidden test: {})", out.tail(400))
                };
                (passed, detail)
            })
            .collect()
    }
}

/// The panic message cargo printed for a failed test, shortened.
fn failure_of(stdout: &str, test: &str) -> String {
    let head = format!("---- {test} stdout ----");
    let text = stdout.split(&head).nth(1).unwrap_or("").split("\n\n").next().unwrap_or("");
    tail(text.trim(), 300)
}

/// Copies `from` into `to` recursively, skipping build output and version control.
pub fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// The text of the file at `p`, or "" when it cannot be read.
pub fn text(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// The text of `rel` under `wd`, or "" when it cannot be read.
pub fn read(wd: &Path, rel: &str) -> String {
    std::fs::read_to_string(wd.join(rel)).unwrap_or_default()
}

/// Whether `rel` under `wd` still holds exactly `original`.
pub fn unchanged(wd: &Path, rel: &str, original: &str) -> Part {
    (std::fs::read_to_string(wd.join(rel)).is_ok_and(|t| t == original), format!("{rel} was modified"))
}

/// The functions in Rust `source` that have no `///` doc comment: `(line, name)`. Attributes
/// between the comment and the function are allowed.
pub fn undocumented(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let Some(name) = fn_name(line) else { continue };
        let documented = lines[..k]
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|l| !l.starts_with("#["))
            .is_some_and(|l| l.starts_with("///"));
        if !documented {
            out.push((k + 1, name));
        }
    }
    out
}

/// The name of the function a line declares, if it starts one.
pub fn fn_name(line: &str) -> Option<String> {
    let mut t = line.trim_start();
    for prefix in ["pub(crate) ", "pub(super) ", "pub ", "const ", "async ", "unsafe ", "extern \"C\" "] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// The files under `dir` (recursively) whose name ends with `suffix`, sorted.
pub fn files(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name() != "target" {
                    stack.push(p);
                }
            } else if p.to_string_lossy().ends_with(suffix) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// `x` rounded to `digits` decimals, as the tasks' references round.
pub fn round_to(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_counts_the_parts_that_hold() {
        let o = Outcome::share(vec![(true, "a".into()), (false, "b".into()), (true, "c".into()), (true, "d".into())]);
        assert!(!o.ok);
        assert_eq!(o.score, 0.75);
        assert_eq!(o.detail, "b");
        assert_eq!(Outcome::all(vec![(true, "a".into())]).score, 1.0);
    }

    #[test]
    fn functions_without_doc_comments_are_found() {
        let src = "/// Doc.\n#[inline]\npub fn a() {}\nfn b() {}\n// plain comment\npub(crate) fn c() {}\n";
        assert_eq!(undocumented(src), vec![(4, "b".to_string()), (6, "c".to_string())]);
        assert_eq!(fn_name("    pub async fn go(x: u8)"), Some("go".into()));
        assert_eq!(fn_name("let f = 1;"), None);
    }

    #[test]
    fn a_command_runs_and_its_output_is_kept() {
        let mut cmd = Command::new("cargo");
        cmd.arg("--version");
        let out = run(&mut cmd, None, Duration::from_secs(60));
        assert!(out.success(), "{}", out.tail(200));
        assert!(out.stdout.starts_with("cargo"));
    }

    #[test]
    #[cfg(unix)]
    fn a_checked_program_never_sees_the_bench_key() {
        std::env::set_var(crate::run::KEY_ENV, "bench-key-marker");
        std::env::set_var("NEXO_BENCH_CHECK_TOKEN", "check-token-marker");
        let out = run(Command::new("sh").args(["-c", "env"]).env("OWN", "kept"), None, Duration::from_secs(60));
        assert!(out.success() && !out.stdout.contains("bench-key-marker"), "{}", out.tail(200));
        assert!(!out.stdout.contains("check-token-marker") && out.stdout.contains("OWN=kept"), "{}", out.stdout);
        let home = std::env::var("HOME").unwrap_or_default();
        assert!(!out.stdout.lines().any(|l| l == format!("HOME={home}")), "{}", out.stdout);
    }
}
