//! Contamination: a run whose agent reached outside its own working directory (and the temporary
//! home it was given) could have read the task's answer from somewhere else on the machine. The
//! transcript's tool calls are scanned for absolute paths outside the allowed folders; a run with
//! any is marked contaminated and left out of the score means and deltas.
//!
//! The scan is deliberately conservative: a path-looking word in a shell command counts, even when
//! it is a regex or a quoted string, because a missed read is worse than a false alarm.

use std::path::{Component, Path, PathBuf};

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
        Scope { wd, roots }
    }

    fn allows(&self, p: &Path) -> bool {
        let p = normalize(p);
        // a root ending in a name prefix (`nexo-bench-home-`) allows the folders that start so
        self.roots.iter().any(|r| p.starts_with(r)) || self.prefixed(&p)
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
        assert_eq!(s.command("grep -rln foo /tmp /home/lctr"), ["/tmp", "/home/lctr"]);
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
