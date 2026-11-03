//! The repo_qa family: questions about a pinned open-source repository, answered by reading it.
//! Each attempt gets a fresh copy of the repository at its commit (without `.git`) with the
//! instance's own files laid over it.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::sha::Sha256;

/// The family's folder, relative to the distribution's root.
pub const DIR: &str = "instances/repo_qa";

/// The system prompt the family's own runner gives the model under test.
pub const SYSTEM: &str = "You are the model under test for benchmark evaluation.

Solve only the benchmark instance you are given and follow its requested output format.
Reason solely from the supplied material and return your best answer directly.
";

/// The read-only tools an attempt gets.
pub const TOOLS: [&str; 5] = ["read", "grep", "glob", "ls", "note"];

/// Every file under `dir` (following links), as `/`-joined relative paths in the order of their
/// path components.
fn files(dir: &Path) -> std::io::Result<Vec<(Vec<String>, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![(Vec::<String>::new(), dir.to_path_buf())];
    while let Some((parts, d)) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let path = e.path();
            let mut p = parts.clone();
            p.push(e.file_name().to_string_lossy().into_owned());
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                stack.push((p, path));
            } else if meta.is_file() {
                out.push((p, path));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The distribution's digest of a folder: over its files in path order, each as its relative
/// path, a NUL, its bytes and a NUL.
pub fn tree_digest(dir: &Path) -> std::io::Result<String> {
    let mut h = Sha256::default();
    for (parts, path) in files(dir)? {
        h.update(parts.join("/").as_bytes());
        h.update(b"\0");
        h.update(&std::fs::read(&path)?);
        h.update(b"\0");
    }
    Ok(h.hex())
}

/// Copies the tree `src` into `dst` (merging into what is there), leaving out every entry named
/// `.git` when `skip_git`.
pub fn copy_tree(src: &Path, dst: &Path, skip_git: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name();
        if skip_git && name == ".git" {
            continue;
        }
        let (from, to) = (e.path(), dst.join(&name));
        let Ok(meta) = std::fs::metadata(&from) else { continue };
        if meta.is_dir() {
            copy_tree(&from, &to, skip_git)?;
        } else if meta.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The cache folder name of a repository at a commit: `<name>-<commit>`.
pub fn cache_name(repository: &str, commit: &str) -> String {
    let last = repository.trim_end_matches('/').rsplit(['/', '\\', ':']).next().unwrap_or(repository);
    let stem = match last.rfind('.') {
        Some(i) if i > 0 => &last[..i],
        _ => last,
    };
    format!("{}-{commit}", stem.strip_suffix(".git").unwrap_or(stem))
}

/// Runs git with `args`, in `dir` when given; its standard output, or why it failed.
pub type Git<'a> = &'a mut dyn FnMut(&[&str], Option<&Path>) -> Result<String, String>;

/// The real git, through the command line.
pub fn system_git(args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    let out = c.args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The cached clean checkout of `repository` at `commit` under `cache`, cloned the first time.
pub fn materialise(cache: &Path, repository: &str, commit: &str, git: Git) -> Result<PathBuf, String> {
    let target = cache.join(cache_name(repository, commit));
    if !target.exists() {
        std::fs::create_dir_all(cache).map_err(|e| format!("{}: {e}", cache.display()))?;
        // cloned beside the cache entry and moved in once checked out, so a killed clone leaves no half entry
        let partial = cache.join(format!("{}.partial", cache_name(repository, commit)));
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
        }
        let p = partial.to_string_lossy().into_owned();
        git(&["clone", "-q", repository, &p], None)?;
        git(&["checkout", "-q", commit], Some(&partial))?;
        std::fs::rename(&partial, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let head = git(&["rev-parse", "HEAD"], Some(&target))?;
    let dirty = git(&["status", "--porcelain"], Some(&target))?;
    if head.trim() != commit || !dirty.trim().is_empty() {
        return Err(format!("cached repository {} is not a clean checkout of {commit}", target.display()));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_is_named_after_the_repository_and_commit() {
        assert_eq!(cache_name("https://github.com/aws-cloudformation/cfn-lint.git", "89ea"), "cfn-lint-89ea");
        assert_eq!(cache_name("https://github.com/o/faker", "1"), "faker-1");
    }

    #[test]
    fn a_checkout_that_is_not_clean_or_not_at_the_commit_is_refused() {
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cache.path().join("r-abc")).unwrap();
        let mut wrong_head = |args: &[&str], _: Option<&Path>| match args[0] {
            "rev-parse" => Ok("def\n".to_string()),
            _ => Ok(String::new()),
        };
        let e = materialise(cache.path(), "https://x/r.git", "abc", &mut wrong_head).unwrap_err();
        assert!(e.contains("not a clean checkout"), "{e}");
        let mut dirty = |args: &[&str], _: Option<&Path>| match args[0] {
            "rev-parse" => Ok("abc\n".to_string()),
            _ => Ok(" M setup.cfg\n".to_string()),
        };
        assert!(materialise(cache.path(), "https://x/r.git", "abc", &mut dirty).is_err());
        let mut clean = |args: &[&str], _: Option<&Path>| match args[0] {
            "rev-parse" => Ok("abc\n".to_string()),
            _ => Ok(String::new()),
        };
        assert_eq!(
            materialise(cache.path(), "https://x/r.git", "abc", &mut clean).unwrap(),
            cache.path().join("r-abc")
        );
    }

    #[test]
    fn a_failed_clone_leaves_no_cache_entry() {
        let cache = tempfile::tempdir().unwrap();
        let mut offline =
            |args: &[&str], _: Option<&Path>| -> Result<String, String> { Err(format!("git {}: offline", args[0])) };
        let e = materialise(cache.path(), "https://x/r.git", "abc", &mut offline).unwrap_err();
        assert!(e.contains("offline"));
        assert!(!cache.path().join("r-abc").exists());
    }

    #[test]
    fn trees_are_copied_without_git_and_digested_in_path_order() {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path().join(".git")).unwrap();
        std::fs::write(src.path().join(".git/HEAD"), "x").unwrap();
        std::fs::create_dir_all(src.path().join("a")).unwrap();
        std::fs::write(src.path().join("a/b"), "1").unwrap();
        std::fs::write(src.path().join("a-c"), "2").unwrap();
        let dst = tempfile::tempdir().unwrap();
        copy_tree(src.path(), dst.path(), true).unwrap();
        assert!(!dst.path().join(".git").exists() && dst.path().join("a/b").exists());
        // components order: a/b before a-c, though '/' sorts after '-'
        let mut h = Sha256::default();
        for (p, d) in [("a/b", "1"), ("a-c", "2")] {
            h.update(format!("{p}\0{d}\0").as_bytes());
        }
        assert_eq!(tree_digest(dst.path()).unwrap(), h.hex());
    }
}
