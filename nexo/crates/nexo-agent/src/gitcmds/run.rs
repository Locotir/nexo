//! Running git and asking it simple questions.

use std::path::Path;
use std::process::Command;

/// Runs `git` in `cwd`; the output on success, the error text on failure.
pub(super) fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(["--no-pager", "-c", "core.quotepath=off", "-c", "color.ui=never"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|e| format!("git is not available: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub(super) fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    git(cwd, args).is_ok()
}

pub(super) fn ensure_repo(cwd: &Path) -> Result<(), String> {
    git(cwd, &["rev-parse", "--is-inside-work-tree"]).map(|_| ()).map_err(|e| {
        if e.starts_with("git is not") {
            e
        } else {
            "this directory is not in a git repository".into()
        }
    })
}

pub(super) fn has_commits(cwd: &Path) -> bool {
    git_ok(cwd, &["rev-parse", "--verify", "-q", "HEAD"])
}
