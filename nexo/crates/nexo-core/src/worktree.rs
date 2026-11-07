//! Git worktrees for isolation: a session, or one sub-agent, works in its own checkout on its own
//! branch (`nexo/<name>`), so parallel agents cannot step on each other or on the person's files.
//!
//! The worktrees live in `<repo>/.nexo/worktrees/<name>`; that folder is listed in the repository's
//! `.git/info/exclude` (never in `.gitignore`), so nothing in it is committed by accident.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Where the worktrees of a repository are kept, relative to its root.
pub const DIR: &str = ".nexo/worktrees";
/// The line added to `.git/info/exclude`.
const EXCLUDE_LINE: &str = ".nexo/worktrees/";
/// The branch of a worktree.
pub const BRANCH_PREFIX: &str = "nexo/";
/// Untracked files to copy into a new worktree, in gitignore syntax, in the repository root.
pub const INCLUDE_FILE: &str = ".worktreeinclude";

/// One worktree made by nexo.
#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    /// The repository's main checkout.
    pub root: PathBuf,
    /// The commit the branch started from.
    pub base: String,
}

/// What a worktree holds beyond its starting point.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changes {
    /// `git status --porcelain` lines (modified, added, untracked).
    pub dirty: Vec<String>,
    /// Commits made on the branch since it started.
    pub commits: usize,
    /// `git diff --stat` against the start, plus the untracked files.
    pub stat: String,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty() && self.commits == 0
    }
}

/// What happened to a worktree when its work ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Finished {
    /// Nothing had changed: the worktree and its branch are gone.
    Removed,
    /// Something changed: it stays.
    Kept(Changes),
}

fn git_in(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("could not run git: {e}"))
}

/// Runs git and returns its trimmed stdout, or its stderr as the error.
fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_in(dir, args)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("git {}: {}", args.first().unwrap_or(&""), err.trim()))
    }
}

/// The main checkout of the repository `cwd` is in (also when `cwd` is one of its worktrees).
pub fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let not_repo = || format!("{} is not inside a git repository; worktrees need one", cwd.display());
    let common = git_ok(cwd, &["rev-parse", "--git-common-dir"]).map_err(|_| not_repo())?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() { common } else { cwd.join(common) };
    let common = crate::paths::canonical(&common).map_err(|_| not_repo())?;
    // a bare repository has no checkout to put worktrees in
    if git_ok(cwd, &["rev-parse", "--is-bare-repository"]).is_ok_and(|b| b == "true") {
        return Err(format!("{} is a bare repository; worktrees need a checkout", cwd.display()));
    }
    common.parent().map(Path::to_path_buf).ok_or_else(not_repo)
}

/// A name that is safe as a folder and as the end of a branch name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 60
        && !name.starts_with(['.', '-'])
        && !name.ends_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A short random name such as `k3x9qa`.
pub fn random_name() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..6].to_string()
}

/// Adds the worktrees folder to the repository's `.git/info/exclude`, once. Returns whether it wrote.
pub fn ensure_exclude(root: &Path) -> Result<bool, String> {
    let rel = git_ok(root, &["rev-parse", "--git-path", "info/exclude"])?;
    let path = PathBuf::from(rel);
    let path = if path.is_absolute() { path } else { root.join(path) };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == EXCLUDE_LINE || l.trim() == ".nexo/worktrees") {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(EXCLUDE_LINE);
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(true)
}

/// Makes `<root>/.nexo/worktrees/<name>` on a new branch `nexo/<name>` from the HEAD of `cwd`, copies
/// the files `.worktreeinclude` names, and runs the `setup` commands in it. `name` none: a random one.
pub fn create(cwd: &Path, name: Option<&str>, setup: &[String]) -> Result<Worktree, String> {
    let root = repo_root(cwd)?;
    let name = match name {
        Some(n) if !valid_name(n) => {
            return Err(format!("`{n}` is not a good worktree name; use letters, digits, `-`, `_` and `.`"))
        }
        Some(n) => n.to_string(),
        None => random_name(),
    };
    let base = git_ok(cwd, &["rev-parse", "--verify", "HEAD"])
        .map_err(|_| "this repository has no commits yet; make one first, a worktree starts from HEAD".to_string())?;
    let path = root.join(DIR).join(&name);
    let branch = format!("{BRANCH_PREFIX}{name}");
    if path.exists() {
        return Err(format!(
            "{} already exists; pick another name or run `nexo worktree remove {name}`",
            path.display()
        ));
    }
    ensure_exclude(&root)?;
    std::fs::create_dir_all(root.join(DIR)).map_err(|e| e.to_string())?;
    let p = path.to_string_lossy().to_string();
    git_ok(cwd, &["worktree", "add", "-q", "-b", &branch, &p, &base])?;
    // the start is kept for later: a branch has "new commits" only past it
    let _ = git_ok(&root, &["config", "--local", &format!("branch.{branch}.nexobase"), &base]);
    let wt = Worktree { name, path, branch, root: root.clone(), base };
    if let Err(e) = prepare(&wt, setup) {
        // a half-made worktree is no use: take it back
        let _ = remove(&root, &wt.name);
        return Err(e);
    }
    Ok(wt)
}

/// Copies the included untracked files and runs the setup commands.
fn prepare(wt: &Worktree, setup: &[String]) -> Result<(), String> {
    copy_included(&wt.root, &wt.path)?;
    for cmd in setup {
        let out = crate::shell::shell()
            .command(cmd)
            .current_dir(&wt.path)
            .output()
            .map_err(|e| format!("worktree_setup `{cmd}`: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: String = err.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
            return Err(format!("worktree_setup `{cmd}` failed ({}): {}", out.status, tail.trim()));
        }
    }
    // what the copies and the setup left in the tree is not the agent's work: remember it
    if let Some(file) = baseline_file(wt) {
        let now = git_ok(&wt.path, &["status", "--porcelain"]).unwrap_or_default();
        let _ = std::fs::write(file, now);
    }
    Ok(())
}

/// Where the status right after setup is kept, inside the worktree's own git folder.
fn baseline_file(wt: &Worktree) -> Option<PathBuf> {
    let p = git_ok(&wt.path, &["rev-parse", "--git-path", "nexo-baseline"]).ok()?;
    let p = PathBuf::from(p);
    Some(if p.is_absolute() { p } else { wt.path.join(p) })
}

/// Copies the untracked files of `root` that `.worktreeinclude` (gitignore syntax) matches.
fn copy_included(root: &Path, to: &Path) -> Result<usize, String> {
    let list = root.join(INCLUDE_FILE);
    if !list.is_file() {
        return Ok(0);
    }
    // git does the pattern matching: untracked files that the given patterns (and no others) match
    let out =
        git_in(root, &["ls-files", "-z", "--others", "--ignored", &format!("--exclude-from={}", list.display())])?;
    if !out.status.success() {
        return Err(format!("could not read {INCLUDE_FILE}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut n = 0;
    for rel in String::from_utf8_lossy(&out.stdout).split('\0').filter(|s| !s.is_empty()) {
        // never copy a worktree into a worktree
        if rel.starts_with(DIR) || rel.starts_with(".git/") {
            continue;
        }
        let (src, dst) = (root.join(rel), to.join(rel));
        if !src.is_file() {
            continue;
        }
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("could not copy {rel}: {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// What the worktree holds beyond its start.
pub fn changes(wt: &Worktree) -> Changes {
    let status = git_ok(&wt.path, &["status", "--porcelain"]).unwrap_or_default();
    let before = baseline_file(wt).and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    let before: Vec<&str> = before.lines().collect();
    let dirty: Vec<String> = status.lines().filter(|l| !before.contains(l)).map(str::to_string).collect();
    let commits = git_ok(&wt.path, &["rev-list", "--count", &format!("{}..HEAD", wt.base)])
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let mut stat = git_ok(&wt.path, &["diff", "--stat", &wt.base]).unwrap_or_default();
    for l in dirty.iter().filter(|l| l.starts_with("??")) {
        if !stat.is_empty() {
            stat.push('\n');
        }
        stat.push_str(&format!("{} (new)", l[2..].trim()));
    }
    Changes { dirty, commits, stat }
}

/// Removes the worktree and its branch.
pub fn remove(root: &Path, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("`{name}` is not a worktree name"));
    }
    let path = root.join(DIR).join(name);
    let branch = format!("{BRANCH_PREFIX}{name}");
    let p = path.to_string_lossy().to_string();
    let removed = git_ok(root, &["worktree", "remove", "--force", &p]);
    if path.exists() {
        // git did not know it (or failed): the folder goes anyway
        std::fs::remove_dir_all(&path).map_err(|e| format!("could not remove {}: {e}", path.display()))?;
        let _ = git_ok(root, &["worktree", "prune"]);
    } else {
        removed?;
    }
    // the branch may be gone already
    let _ = git_ok(root, &["branch", "-D", &branch]);
    Ok(())
}

/// Ends a worktree's work: gone when nothing changed, kept otherwise.
pub fn finish(wt: &Worktree) -> Finished {
    let c = changes(wt);
    if c.is_empty() && remove(&wt.root, &wt.name).is_ok() {
        return Finished::Removed;
    }
    Finished::Kept(c)
}

/// The lines that tell the person where a kept worktree is and what to do with it.
pub fn kept_message(wt: &Worktree, c: &Changes) -> String {
    let what = match (c.commits, c.dirty.len()) {
        (0, d) => format!("{d} changed file(s), not committed"),
        (n, 0) => format!("{n} new commit(s)"),
        (n, d) => format!("{n} new commit(s) and {d} changed file(s) not committed"),
    };
    format!(
        "worktree kept ({what})\n  path    {}\n  branch  {}\n  merge   git merge {}\n  remove  nexo worktree remove {}",
        wt.path.display(),
        wt.branch,
        wt.branch,
        wt.name
    )
}

/// A worktree made by nexo, as found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub worktree: Worktree,
    pub changes: Changes,
}

/// The worktrees of the repository, by name.
pub fn list(root: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(root.join(DIR)) else { return Vec::new() };
    let mut names: Vec<String> =
        rd.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let branch = format!("{BRANCH_PREFIX}{name}");
            let base = git_ok(root, &["config", "--local", &format!("branch.{branch}.nexobase")]).unwrap_or_default();
            let wt = Worktree { path: root.join(DIR).join(&name), name, branch, root: root.to_path_buf(), base };
            let changes = if wt.base.is_empty() { Changes::default() } else { changes(&wt) };
            Entry { worktree: wt, changes }
        })
        .collect()
}

/// Removes the worktrees with no changes and forgets the ones whose folder is gone. Returns the names removed.
pub fn prune(root: &Path) -> Vec<String> {
    let _ = git_ok(root, &["worktree", "prune"]);
    let mut gone = Vec::new();
    for e in list(root) {
        // without a recorded start nothing can be said about the commits: keep it
        if !e.worktree.base.is_empty() && e.changes.is_empty() && remove(root, &e.worktree.name).is_ok() {
            gone.push(e.worktree.name);
        }
    }
    // branches whose worktree was deleted by hand
    if let Ok(b) = git_ok(root, &["for-each-ref", "--format=%(refname:short)", "refs/heads/nexo/"]) {
        for branch in b.lines() {
            let name = branch.trim_start_matches(BRANCH_PREFIX);
            let checked_out = git_ok(root, &["worktree", "list", "--porcelain"])
                .is_ok_and(|l| l.lines().any(|x| x == format!("branch refs/heads/{branch}")));
            let ahead = git_ok(root, &["rev-list", "--count", &format!("HEAD..{branch}")])
                .ok()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1);
            if !checked_out && ahead == 0 && git_ok(root, &["branch", "-D", branch]).is_ok() {
                gone.push(name.to_string());
            }
        }
    }
    gone
}

/// If `cwd` is inside one of nexo's worktrees: its name and branch.
pub fn current(cwd: &Path) -> Option<(String, String)> {
    let cwd = crate::paths::canonical(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let comps: Vec<_> = cwd.components().collect();
    let at = comps.windows(3).position(|w| {
        w[0].as_os_str() == ".nexo"
            && w[1].as_os_str() == "worktrees"
            && valid_name(&w[2].as_os_str().to_string_lossy())
    })?;
    let name = comps[at + 2].as_os_str().to_string_lossy().to_string();
    let top = cwd.ancestors().find(|a| a.ends_with(format!("{DIR}/{name}")))?;
    let branch =
        git_ok(top, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| format!("{BRANCH_PREFIX}{name}"));
    Some((name, branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for a in [&["init", "-q"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"]] {
            git_ok(d.path(), a).unwrap();
        }
        std::fs::write(d.path().join("a.txt"), "a\n").unwrap();
        git_ok(d.path(), &["add", "."]).unwrap();
        git_ok(d.path(), &["commit", "-qm", "first"]).unwrap();
        d
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("fix-1_a.b"));
        for bad in ["", "..", "a/b", "-x", ".x", "a b", "a..b"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_name(&random_name()));
    }

    #[test]
    fn exclude_is_written_once() {
        let d = repo();
        assert!(ensure_exclude(d.path()).unwrap());
        assert!(!ensure_exclude(d.path()).unwrap());
        let text = std::fs::read_to_string(d.path().join(".git/info/exclude")).unwrap();
        assert_eq!(text.matches(".nexo/worktrees/").count(), 1);
    }

    #[test]
    fn a_clean_worktree_goes_away_and_a_changed_one_stays() {
        let d = repo();
        let a = create(d.path(), Some("one"), &[]).unwrap();
        assert!(a.path.join("a.txt").is_file());
        assert_eq!(finish(&a), Finished::Removed);
        assert!(!a.path.exists());
        assert!(git_ok(d.path(), &["branch", "--list", "nexo/one"]).unwrap().is_empty());
        let b = create(d.path(), Some("two"), &[]).unwrap();
        std::fs::write(b.path.join("new.txt"), "x").unwrap();
        let Finished::Kept(c) = finish(&b) else { panic!("kept") };
        assert!(c.stat.contains("new.txt") && b.path.exists());
        // a commit alone also keeps it
        let e = create(d.path(), Some("three"), &[]).unwrap();
        std::fs::write(e.path.join("a.txt"), "b\n").unwrap();
        git_ok(&e.path, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qam", "m"]).unwrap();
        assert!(matches!(finish(&e), Finished::Kept(c) if c.commits == 1));
        assert_eq!(list(d.path()).len(), 2);
        assert_eq!(current(&b.path).unwrap().0, "two");
        // from inside a worktree the root is still the main checkout
        assert_eq!(repo_root(&b.path).unwrap().canonicalize().unwrap(), d.path().canonicalize().unwrap());
        remove(d.path(), "two").unwrap();
        remove(d.path(), "three").unwrap();
        assert!(list(d.path()).is_empty());
    }

    #[test]
    fn include_file_and_setup() {
        let d = repo();
        std::fs::write(d.path().join(".env"), "KEY=1\n").unwrap();
        std::fs::write(d.path().join("other.log"), "no\n").unwrap();
        std::fs::write(d.path().join(INCLUDE_FILE), ".env\n").unwrap();
        let w = create(d.path(), Some("s"), &["echo ran > setup.txt".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(w.path.join(".env")).unwrap(), "KEY=1\n");
        assert!(!w.path.join("other.log").exists());
        assert!(w.path.join("setup.txt").is_file());
        let bad = create(d.path(), Some("f"), &["exit 3".to_string()]);
        assert!(bad.unwrap_err().contains("worktree_setup"));
        assert!(!d.path().join(DIR).join("f").exists());
    }

    #[test]
    fn outside_a_repository_it_refuses() {
        let d = tempfile::tempdir().unwrap();
        let err = create(d.path(), None, &[]).unwrap_err();
        assert!(err.contains("not inside a git repository"), "{err}");
    }
}
