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
    "php", "swift", "scala", "sh", "toml", "json", "yaml", "yml", "sql", "lua", "zig", "ex", "exs", "hs", "ml",
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

/// `\ncheck: cargo test -q | exit 1 | 3.2 s\n<tail>`: how a check is reported.
pub fn report(command: &str, exit: Option<i32>, timed_out: bool, secs: f64, cap: Duration, output: &str) -> String {
    let code = match (exit, timed_out) {
        (Some(c), _) => c.to_string(),
        (None, true) => format!("killed at the {} s cap", cap.as_secs()),
        (None, false) => "none".into(),
    };
    let body = tail(output);
    let body = if body.is_empty() { "(no output)".to_string() } else { body };
    format!("\ncheck: {command} | exit {code} | {secs:.1} s\n{body}")
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
        let c = command.split_whitespace().collect::<Vec<_>>().join(" ");
        let c = c.strip_suffix(" 2>&1").unwrap_or(&c);
        (r.fingerprint == fingerprint && r.command.split_whitespace().collect::<Vec<_>>().join(" ") == c)
            .then(|| r.text.clone())
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
        let p = crate::bash::prove(command, dir, sandbox, cap, cancel).await;
        self.record_run(command, dir, p.timed_out);
        report(command, p.exit, p.timed_out, p.secs, cap, &p.output)
    }
}

/// The proof command of an `edit` or `write` call (its `check` argument), run in the call's folder.
pub async fn run_own(command: &str, ctx: &nexo_core::ToolContext) -> String {
    let cap = Duration::from_secs(ctx.check_secs.max(1));
    let p = crate::bash::prove(command, &ctx.cwd, &ctx.sandbox, cap, &ctx.cancel).await;
    report(command, p.exit, p.timed_out, p.secs, cap, &p.output)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
