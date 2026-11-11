//! Contamination: a run whose agent reached outside its own working directory (and the temporary
//! home it was given) could have read the task's answer from somewhere else on the machine. The
//! transcript's tool calls are scanned for absolute paths outside the allowed folders; a run with
//! any is marked contaminated and left out of the score means and deltas.
//!
//! The scan is deliberately conservative: a path-looking word in a shell command counts, even when
//! it is a regex or a quoted string, because a missed read is worse than a false alarm.

use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use regex::Regex;
use serde_json::Value;

/// The most offending paths one record keeps.
const MAX_LISTED: usize = 20;

/// System folders a build or a shell needs.
const SYSTEM: [&str; 10] = ["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/proc", "/dev"];

/// Arguments of the file tools that hold a path (or a path glob).
const PATH_KEYS: [&str; 5] = ["path", "file_path", "cwd", "dir", "glob"];

/// The folders a run may touch.
pub struct Scope {
    wd: PathBuf,
    roots: Vec<PathBuf>,
    /// Files in the temporary folder made at or after this time belong to the run.
    since: Option<SystemTime>,
}

impl Scope {
    /// The scope of a run in `wd`: `wd`, the system folders, the temporary homes the bench makes,
    /// and the cargo and rustup homes of this process.
    pub fn new(wd: &Path) -> Scope {
        let mut roots: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
        roots.push("/sys".into());
        roots.push(std::env::temp_dir().join("nexo-bench-home-"));
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for (var, dir) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
            match (std::env::var_os(var), &home) {
                (Some(v), _) if !v.is_empty() => roots.push(v.into()),
                (_, Some(h)) => roots.push(h.join(dir)),
                _ => {}
            }
        }
        Scope::with_roots(wd, roots)
    }

    /// A scope of `wd` and exactly `roots`.
    pub fn with_roots(wd: &Path, roots: Vec<PathBuf>) -> Scope {
        let wd = normalize(wd);
        let mut roots: Vec<PathBuf> = roots.iter().map(|r| normalize(r)).collect();
        roots.push(wd.clone());
        Scope { wd, roots, since: None }
    }

    fn allows(&self, p: &Path) -> bool {
        let p = normalize(p);
        // a root ending in a name prefix (`nexo-bench-home-`) allows the folders that start so
        self.roots.iter().any(|r| p.starts_with(r)) || self.prefixed(&p) || self.own_temp(&p)
    }

    /// Temporary files count as the run's own when they were made after it started, so a
    /// scratch file the agent wrote to `/tmp` and read back is not a reach outside.
    pub fn made_since(mut self, t: SystemTime) -> Scope {
        self.since = Some(t);
        self
    }

    fn own_temp(&self, p: &Path) -> bool {
        let Some(since) = self.since else {
            return false;
        };
        // agents write `/tmp/x` literally, whatever TMPDIR says
        let tmps = [normalize(&std::env::temp_dir()), PathBuf::from("/tmp")];
        // a temporary folder itself is never the run's own, also when TMPDIR sits inside /tmp
        if tmps.iter().any(|t| p == t) || !tmps.iter().any(|t| p.starts_with(t)) {
            return false;
        }
        std::fs::metadata(p)
            .and_then(|m| m.created().or_else(|_| m.modified()))
            .map(|made| made >= since)
            .unwrap_or(false)
    }

    fn prefixed(&self, p: &Path) -> bool {
        self.roots.iter().any(|r| {
            let (Some(parent), Some(name)) = (r.parent(), r.file_name().and_then(|n| n.to_str())) else {
                return false;
            };
            name.ends_with('-')
                && p.strip_prefix(parent)
                    .ok()
                    .and_then(|rest| rest.components().next())
                    .and_then(|c| c.as_os_str().to_str().map(|c| c.starts_with(name)))
                    .unwrap_or(false)
        })
    }

    /// The paths of a shell command that lie outside the scope.
    pub fn command(&self, cmd: &str) -> Vec<String> {
        let words = Regex::new(r#"(?:^|[\s"'=:(,;<>|&{\[`])(/[^\s"'`;|&<>(){}\[\],]*)"#).expect("a valid pattern");
        words
            .captures_iter(cmd)
            .filter(|c| !division(cmd, c.get(1).expect("group 1")))
            .map(|c| c[1].to_string())
            // `//host/..` is a URL, not a file
            .filter(|p| !p.starts_with("//") && !self.allows(Path::new(p)))
            .collect()
    }

    /// The paths among a file tool's arguments that lie outside the scope: absolute, `~`, or
    /// relative ones that climb out of the working directory.
    pub fn args(&self, tool: &str, input: &Value) -> Vec<String> {
        let mut keys: Vec<&str> = PATH_KEYS.to_vec();
        if tool == "glob" {
            keys.push("pattern");
        }
        let mut out = Vec::new();
        for key in keys {
            let Some(v) = input.get(key).and_then(Value::as_str) else { continue };
            let v = v.trim();
            let abs = if v.starts_with('~') {
                out.push(v.to_string());
                continue;
            } else if v.starts_with('/') {
                PathBuf::from(v)
            } else {
                self.wd.join(v)
            };
            if !self.allows(&abs) {
                out.push(v.to_string());
            }
        }
        out
    }

    /// The paths outside the scope that the tool calls of a transcript (one JSON request per
    /// line) reach, each once, in the order they first appear.
    pub fn transcript(&self, text: &str) -> Vec<String> {
        let mut seen_ids = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            for b in v["reply"]["content"].as_array().into_iter().flatten() {
                if b["type"] != "tool_use" || !seen_ids.insert(b["id"].as_str().unwrap_or_default().to_string()) {
                    continue;
                }
                let name = b["name"].as_str().unwrap_or_default();
                let found = match b["input"]["command"].as_str().filter(|_| name == "bash") {
                    Some(cmd) => self.command(cmd),
                    None => self.args(name, &b["input"]),
                };
                for p in found {
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
        out.truncate(MAX_LISTED);
        out
    }

    /// [`Scope::transcript`] of the file at `path`; nothing when it cannot be read.
    pub fn file(&self, path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).map(|t| self.transcript(&t)).unwrap_or_default()
    }
}

/// Whether a bare `/` at `m` divides two operands (`x / 100.0`, `(a) / b`), as in code a command
/// writes, rather than naming the root folder (`ls /`, `find / -name`, `du / 2>&1`).
fn division(cmd: &str, m: regex::Match) -> bool {
    if m.as_str() != "/" {
        return false;
    }
    let before = cmd[..m.start()].trim_end();
    let after = cmd[m.end()..].trim_start();
    let left = before.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ')' || c == ']');
    let operand: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
    let rest = &after[operand.len()..];
    let right = after.starts_with('(')
        || (!operand.is_empty() && !operand.starts_with('-') && !rest.starts_with('>') && !rest.starts_with('&'));
    left && right
}

/// `p` with `.` and `..` resolved by name, without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope::with_roots(Path::new("/tmp/nexo-bench-AbC"), SYSTEM.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn a_command_reading_another_folder_is_flagged() {
        let s = scope();
        assert_eq!(s.command("sed -n '1,9p' /home/x/file"), ["/home/x/file"]);
        assert_eq!(s.command("grep -rln foo /tmp /home/user"), ["/tmp", "/home/user"]);
        assert_eq!(s.command("cat /tmp/nexo-bench-AbC/../other/a.rs"), ["/tmp/nexo-bench-AbC/../other/a.rs"]);
        assert_eq!(s.command("FOO=/srv/data ls"), ["/srv/data"]);
    }

    #[test]
    fn the_work_dir_system_paths_and_relative_paths_are_fine() {
        let s = scope();
        assert!(s.command("cd /tmp/nexo-bench-AbC && cargo test 2>/dev/null").is_empty());
        assert!(s.command("/usr/bin/env ls /etc/hosts /lib/x86_64-linux-gnu; cat src/main.rs ../x ./a").is_empty());
        assert!(s.command("curl https://example.com/a/b; sed 's/a/b/'").is_empty());
        assert!(s.args("read", &json!({"path": "src/lib.rs"})).is_empty());
        assert!(s.args("read", &json!({"path": "/tmp/nexo-bench-AbC/a.rs"})).is_empty());
        assert!(s.args("grep", &json!({"pattern": "/home/x", "path": "."})).is_empty());
    }

    #[test]
    fn file_tools_are_checked_by_path_and_climbing() {
        let s = scope();
        assert_eq!(s.args("read", &json!({"path": "/home/x/f.rs"})), ["/home/x/f.rs"]);
        assert_eq!(s.args("ls", &json!({"path": "../../.."})), ["../../.."]);
        assert_eq!(s.args("read", &json!({"path": "~/notes"})), ["~/notes"]);
        assert_eq!(s.args("glob", &json!({"pattern": "/home/**/*.rs"})), ["/home/**/*.rs"]);
        assert!(s.args("glob", &json!({"pattern": "src/**/*.rs"})).is_empty());
    }

    #[test]
    fn a_division_in_written_code_is_not_the_root() {
        let s = scope();
        assert!(s.command("cat > a.rs <<'EOF'\nSome(units / daily_use)\n(x * 100.0).round() / 100.0\nEOF").is_empty());
        assert!(s.command("echo $((a / (b + 1)))").is_empty());
        for cmd in ["ls /", "find / -name x.rs", "du -sh / 2>&1", "grep -r foo / | head", "cd / && ls"] {
            assert_eq!(s.command(cmd), ["/"], "{cmd}");
        }
    }

    #[test]
    // the command parser reads Unix paths; a Windows temp folder is not one
    #[cfg(unix)]
    fn temporary_files_made_during_the_run_are_its_own() {
        let f = tempfile::Builder::new().prefix("contam-own-").tempfile().unwrap();
        let p = f.path().display().to_string();
        let cmd = format!("cat {p}");
        let wd = Path::new("/nonexistent/nexo-bench-AbC");
        let later = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(Scope::with_roots(wd, vec![]).made_since(later).command(&cmd), std::slice::from_ref(&p));
        assert!(Scope::with_roots(wd, vec![]).made_since(SystemTime::UNIX_EPOCH).command(&cmd).is_empty());
        // the temporary folder itself, and files already gone, stay outside
        let tmp = std::env::temp_dir().display().to_string();
        let gone = format!("{tmp}/contam-gone-none");
        let s = Scope::with_roots(wd, vec![]).made_since(SystemTime::UNIX_EPOCH);
        assert_eq!(s.command(&format!("ls {tmp} {gone}")), [tmp, gone]);
    }

    #[test]
    #[cfg(unix)]
    fn tmp_itself_is_never_the_runs_own_and_a_file_made_in_it_is() {
        let d = tempfile::Builder::new().prefix("contam-tmpdir-").tempdir_in("/tmp").unwrap();
        let s = Scope::with_roots(Path::new("/nonexistent/nexo-bench-AbC"), vec![]).made_since(SystemTime::UNIX_EPOCH);
        assert!(!s.own_temp(Path::new("/tmp")));
        let made = d.path().join("made");
        std::fs::write(&made, "").unwrap();
        assert!(s.own_temp(&made), "a file made in the run is its own");
    }

    #[test]
    fn the_bench_homes_are_allowed_by_prefix() {
        let s = Scope::with_roots(Path::new("/tmp/nexo-bench-AbC"), vec!["/tmp/nexo-bench-home-".into()]);
        assert!(s.command("ls /tmp/nexo-bench-home-Zz9/home/.cache").is_empty());
        assert_eq!(s.command("ls /tmp/other"), ["/tmp/other"]);
    }

    #[test]
    fn a_transcript_is_read_for_its_tool_calls_only() {
        let s = scope();
        let reply = |id: &str, name: &str, input: Value| {
            json!({"messages": [{"role": "user", "content": "see /home/x/prompt"}],
                   "reply": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}})
        };
        let text = [
            reply("1", "bash", json!({"command": "ls src"})),
            reply("2", "bash", json!({"command": "sed -n 1,5p /home/x/inventory.rs"})),
            reply("3", "read", json!({"path": "/home/x/inventory.rs"})),
            reply("3", "read", json!({"path": "/home/y/dup"})),
            reply("4", "write", json!({"path": "a.txt", "content": "/home/z/in/content"})),
        ]
        .map(|v| v.to_string())
        .join("\n");
        assert_eq!(s.transcript(&text), ["/home/x/inventory.rs"]);
    }
}
