//! Says so when a sandboxed command changed a path that later runs outside the sandbox or changes what
//! the agent may do (git hooks, project settings, MCP servers). The sandbox cannot make these read-only
//! inside the project it lets the command write, so the person is told instead. Only `stat` and
//! `read_dir`: no file is ever read. No refusal and no rollback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nexo_core::sandbox::{SandboxMode, SandboxSettings};

/// Paths of the project (relative to its directory) that a command may use to run code outside the sandbox
/// later, or to widen what the agent may do.
pub const GUARDED: &[&str] =
    &[".git/hooks", ".git/config", ".nexo", ".mcp.json", ".claude", ".codex", ".envrc", ".vscode", ".husky"];

/// Start of the line that goes into the tool result (the agent turns it into a warning for the person).
pub const NOTE_PREFIX: &str = "[sandbox: this command changed ";
/// Entries counted per guarded directory before it is judged by its own mtime.
const MAX_ENTRIES: usize = 200;
/// Levels below a guarded directory that are looked at.
const MAX_DEPTH: usize = 2;
/// Paths named in the line.
const NAMED: usize = 3;

#[derive(Debug, Clone, PartialEq)]
enum Sig {
    File(u64, Option<SystemTime>),
    /// A directory; the mtime only when it was too big to list.
    Dir(Option<SystemTime>),
}

/// What the guarded paths looked like at one moment.
#[derive(Debug, Default)]
pub struct Snapshot {
    seen: BTreeMap<PathBuf, Sig>,
    /// The paths that were watched (also the ones that did not exist).
    top: Vec<PathBuf>,
}

fn sig(meta: &std::fs::Metadata, listed: bool) -> Sig {
    if meta.is_dir() {
        Sig::Dir(if listed { None } else { meta.modified().ok() })
    } else {
        Sig::File(meta.len(), meta.modified().ok())
    }
}

/// Walks `dir` (already known to be a directory) at most `MAX_DEPTH` levels and `MAX_ENTRIES` entries.
/// Returns false when it stopped for size.
fn walk(dir: &Path, depth: usize, count: &mut usize, out: &mut BTreeMap<PathBuf, Sig>) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return true };
    for e in rd.flatten() {
        *count += 1;
        if *count > MAX_ENTRIES {
            return false;
        }
        let p = e.path();
        let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
        out.insert(p.clone(), sig(&m, true));
        if m.is_dir() && depth < MAX_DEPTH && !walk(&p, depth + 1, count, out) {
            return false;
        }
    }
    true
}

/// The guarded paths of the project at `cwd`, including those of a git worktree where `roots` let the command write.
fn targets(cwd: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = GUARDED.iter().map(|g| cwd.join(g)).collect();
    let dot_git = cwd.join(".git");
    if dot_git.is_file() {
        let read = |p: &Path| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
        let base = |p: &str| {
            let p = Path::new(p);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cwd.join(p)
            }
        };
        let gitdir = read(&dot_git).and_then(|s| s.strip_prefix("gitdir:").map(|r| base(r.trim())));
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(g) = gitdir {
            if let Some(c) = read(&g.join("commondir")) {
                let c = Path::new(&c);
                dirs.push(if c.is_absolute() { c.to_path_buf() } else { g.join(c) });
            }
            dirs.push(g);
        }
        for d in dirs {
            let Ok(real) = d.canonicalize() else { continue };
            // only where the sandbox lets the command write
            if roots.iter().any(|r| real.starts_with(r)) {
                v.push(real.join("hooks"));
                v.push(real.join("config"));
            }
        }
    }
    v
}

/// The snapshot to take just before a sandboxed command. None when the sandbox does not let it write the project.
pub fn before(s: &SandboxSettings, cwd: &Path) -> Option<Snapshot> {
    if s.mode != SandboxMode::WorkspaceWrite || !crate::sandbox::auto_allows(s) {
        return None;
    }
    Some(take(cwd, &s.write_roots(cwd)))
}

fn take(cwd: &Path, roots: &[PathBuf]) -> Snapshot {
    let mut snap = Snapshot::default();
    for t in targets(cwd, roots) {
        if let Ok(m) = std::fs::symlink_metadata(&t) {
            if m.is_dir() {
                let (mut n, mut sub) = (0, BTreeMap::new());
                let whole = walk(&t, 1, &mut n, &mut sub);
                if whole {
                    snap.seen.extend(sub);
                }
                snap.seen.insert(t.clone(), sig(&m, whole));
            } else {
                snap.seen.insert(t.clone(), sig(&m, true));
            }
        }
        snap.top.push(t);
    }
    snap
}

/// The line for the tool result when something guarded changed since `before` (None: nothing did).
pub fn after(before: Snapshot, s: &SandboxSettings, cwd: &Path) -> Option<String> {
    let now = take(cwd, &s.write_roots(cwd));
    let mut changed: Vec<&PathBuf> = Vec::new();
    for (p, sig) in &now.seen {
        if before.seen.get(p) != Some(sig) {
            changed.push(p);
        }
    }
    changed.extend(before.seen.keys().filter(|p| !now.seen.contains_key(*p)));
    // a directory that gained or lost an entry is not named: the entry is
    let named: Vec<&PathBuf> =
        changed.iter().copied().filter(|p| !changed.iter().any(|q| q != p && q.starts_with(p))).collect();
    if named.is_empty() {
        return None;
    }
    let show = |p: &Path| {
        let rel = p.strip_prefix(cwd).unwrap_or(p).display().to_string();
        rel.replace(['\n', '\r', ']'], "?")
    };
    let mut names: Vec<String> = named.iter().take(NAMED).map(|p| show(p)).collect();
    if named.len() > NAMED {
        names.push(format!("+{} more", named.len() - NAMED));
    }
    let mut list = names.join(", ");
    if named.len() > NAMED {
        // "a, b, c, +N more" reads better as "a, b, c +N more"
        list = list.replacen(", +", " +", 1);
    }
    let verb = if named.len() == 1 { "which runs" } else { "which run" };
    Some(format!("{NOTE_PREFIX}{list}, {verb} outside the sandbox later]"))
}

/// The line `after` made, found at the end of a tool result.
pub fn note_in(content: &str) -> Option<&str> {
    content.lines().rev().take(3).find(|l| l.starts_with(NOTE_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ws() -> SandboxSettings {
        SandboxSettings::new(SandboxMode::WorkspaceWrite, false, vec![])
    }

    fn roots(d: &Path) -> Vec<PathBuf> {
        vec![d.canonicalize().unwrap()]
    }

    fn diff(d: &Path, act: impl FnOnce()) -> Option<String> {
        let b = take(d, &roots(d));
        act();
        after(b, &ws(), d)
    }

    #[test]
    fn nothing_guarded_changed_says_nothing() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git/hooks")).unwrap();
        assert!(diff(d.path(), || fs::write(d.path().join("a.rs"), "x").unwrap()).is_none());
        // ordinary git files
        assert!(diff(d.path(), || fs::write(d.path().join(".git/index"), "x").unwrap()).is_none());
    }

    #[test]
    fn a_new_hook_is_named() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git/hooks")).unwrap();
        let n = diff(d.path(), || fs::write(d.path().join(".git/hooks/pre-commit"), "x").unwrap()).unwrap();
        assert_eq!(n, "[sandbox: this command changed .git/hooks/pre-commit, which runs outside the sandbox later]");
    }

    #[test]
    fn created_removed_and_resized_files_count() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/config"), "a").unwrap();
        fs::write(d.path().join(".envrc"), "a").unwrap();
        assert!(diff(d.path(), || fs::write(d.path().join(".git/config"), "ab").unwrap())
            .unwrap()
            .contains(".git/config"));
        assert!(diff(d.path(), || fs::remove_file(d.path().join(".envrc")).unwrap()).unwrap().contains(".envrc"));
        assert!(diff(d.path(), || fs::write(d.path().join(".mcp.json"), "{}").unwrap()).unwrap().contains(".mcp.json"));
    }

    #[test]
    fn more_than_three_are_counted() {
        let d = tempfile::tempdir().unwrap();
        let n = diff(d.path(), || {
            for f in [".mcp.json", ".envrc", ".claude", ".codex", ".nexo"] {
                fs::write(d.path().join(f), "x").unwrap();
            }
        })
        .unwrap();
        assert!(n.contains(" +2 more"), "{n}");
    }

    #[test]
    fn a_big_directory_is_judged_by_its_own_mtime() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join(".vscode");
        fs::create_dir_all(&v).unwrap();
        for i in 0..250 {
            fs::write(v.join(format!("f{i}")), "x").unwrap();
        }
        let b = take(d.path(), &roots(d.path()));
        assert!(!b.seen.keys().any(|k| k.starts_with(v.join("f"))), "entries kept past the cap");
        assert!(matches!(b.seen.get(&v), Some(Sig::Dir(Some(_)))));
    }

    #[test]
    fn a_worktree_guards_its_git_dir_only_where_writable() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("main/.git");
        let gd = main.join("worktrees/w");
        fs::create_dir_all(&gd).unwrap();
        fs::create_dir_all(main.join("hooks")).unwrap();
        fs::write(gd.join("commondir"), "../..").unwrap();
        let wt = d.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", gd.display())).unwrap();
        let writable = vec![d.path().canonicalize().unwrap()];
        let t = targets(&wt, &writable);
        assert!(t.iter().any(|p| p.ends_with("main/.git/hooks")) && t.iter().any(|p| p.ends_with("main/.git/config")));
        let only_wt = vec![wt.canonicalize().unwrap()];
        let t = targets(&wt, &only_wt);
        assert!(!t.iter().any(|p| p.ends_with("main/.git/hooks")));
    }

    #[test]
    fn the_line_is_found_in_a_result() {
        let n = "[sandbox: this command changed .envrc, which runs outside the sandbox later]";
        assert_eq!(note_in(&format!("out\n\n{n}")), Some(n));
        assert_eq!(note_in("out"), None);
    }
}
