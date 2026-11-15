//! What a diff is taken against, and the diff cut to fit a prompt.

use super::run::*;
use std::path::Path;

/// Most characters of diff put in a prompt.
pub const DIFF_CAP: usize = 48_000;

/// What a diff is taken against.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Uncommitted changes plus the branch since its merge base with main/master.
    Default,
    /// The staged changes only.
    Staged,
    /// The branch (and the working tree) against its merge base with this branch.
    Base(String),
    /// One commit.
    Commit(String),
}

impl Target {
    /// `[base|commit|--staged]` as typed after `/review`; `--base X` is the explicit form.
    pub fn parse(arg: Option<&str>, cwd: &Path) -> Result<Target, String> {
        let words: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
        match words.as_slice() {
            [] => Ok(Target::Default),
            ["--staged"] | ["--cached"] | ["staged"] => Ok(Target::Staged),
            ["--base", b] => Ok(Target::Base((*b).to_string())),
            ["--commit", c] => Ok(Target::Commit((*c).to_string())),
            [w] if !w.starts_with('-') => {
                let is_branch = git_ok(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{w}")])
                    || git_ok(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{w}")])
                    || git_ok(cwd, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/origin/{w}")]);
                Ok(if is_branch { Target::Base((*w).to_string()) } else { Target::Commit((*w).to_string()) })
            }
            _ => Err("usage: /review [base|commit|--staged]".into()),
        }
    }
}

/// The branch the work will merge into: main or master, local or on origin.
pub(super) fn main_branch(cwd: &Path) -> Option<String> {
    if let Ok(h) = git(cwd, &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"]) {
        let h = h.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    ["main", "master", "origin/main", "origin/master"]
        .into_iter()
        .find(|b| git_ok(cwd, &["rev-parse", "--verify", "-q", &format!("{b}^{{commit}}")]))
        .map(String::from)
}

/// The diff text and a one-line description of what it covers.
pub(super) fn diff_for(cwd: &Path, target: &Target) -> Result<(String, String, String), String> {
    let (what, spec): (String, Vec<String>) = match target {
        Target::Staged => ("the staged changes".into(), vec!["--cached".into()]),
        Target::Commit(c) => {
            let sha = git(cwd, &["rev-parse", "--verify", "-q", &format!("{c}^{{commit}}")])
                .map_err(|_| format!("'{c}' is neither a branch nor a commit here"))?;
            let sha = sha.trim().to_string();
            // a root commit has no parent: diff against the empty tree
            let parent = if git_ok(cwd, &["rev-parse", "--verify", "-q", &format!("{sha}^")]) {
                format!("{sha}^")
            } else {
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()
            };
            (format!("commit {}", &sha[..sha.len().min(12)]), vec![parent, sha])
        }
        Target::Base(b) => {
            let mb = git(cwd, &["merge-base", b, "HEAD"])
                .map_err(|_| format!("no common ancestor between '{b}' and HEAD (is '{b}' a branch here?)"))?;
            (format!("uncommitted changes and the branch since its merge base with {b}"), vec![mb.trim().to_string()])
        }
        Target::Default => {
            if !has_commits(cwd) {
                ("the staged and unstaged changes (no commit yet)".into(), vec!["--cached".into()])
            } else {
                let base = main_branch(cwd)
                    .and_then(|b| git(cwd, &["merge-base", &b, "HEAD"]).ok().map(|m| (b, m.trim().to_string())));
                match base {
                    Some((b, mb)) if !mb.is_empty() => {
                        (format!("uncommitted changes and the branch since its merge base with {b}"), vec![mb])
                    }
                    _ => ("the uncommitted changes".into(), vec!["HEAD".into()]),
                }
            }
        }
    };
    let mut args = vec!["diff", "-M", "--no-ext-diff", "--no-color"];
    let spec_refs: Vec<&str> = spec.iter().map(String::as_str).collect();
    args.extend(&spec_refs);
    let diff = git(cwd, &args)?;
    let mut sargs = vec!["diff", "-M", "--no-ext-diff", "--no-color", "--stat=120"];
    sargs.extend(&spec_refs);
    let stat = git(cwd, &sargs)?;
    Ok((what, stat, diff))
}

pub(super) struct FileDiff {
    pub(super) header: String,
    pub(super) hunks: Vec<String>,
}

pub(super) fn split_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            files.push(FileDiff { header: line.to_string(), hunks: Vec::new() });
        } else if let Some(f) = files.last_mut() {
            if line.starts_with("@@") {
                f.hunks.push(line.to_string());
            } else if let Some(h) = f.hunks.last_mut() {
                h.push_str(line);
            } else {
                f.header.push_str(line);
            }
        }
    }
    files
}

/// The diff cut to `cap` characters: whole when it fits; else the biggest hunks of every
/// file that fit, in file order, and a note of what was left out.
pub fn cap_diff(diff: &str, cap: usize) -> String {
    if diff.len() <= cap {
        return diff.to_string();
    }
    let files = split_diff(diff);
    let mut budget = cap.saturating_sub(files.iter().map(|f| f.header.len()).sum::<usize>());
    // every hunk, the biggest first
    let mut order: Vec<(usize, usize, usize)> = files
        .iter()
        .enumerate()
        .flat_map(|(i, f)| f.hunks.iter().enumerate().map(move |(j, h)| (i, j, h.len())))
        .collect();
    order.sort_by_key(|&(i, j, n)| (std::cmp::Reverse(n), i, j));
    let mut keep = std::collections::HashSet::new();
    for (i, j, n) in order {
        if n <= budget {
            budget -= n;
            keep.insert((i, j));
        }
    }
    let mut out = String::new();
    for (i, f) in files.iter().enumerate() {
        out.push_str(&f.header);
        let mut left = 0;
        for (j, h) in f.hunks.iter().enumerate() {
            if keep.contains(&(i, j)) {
                out.push_str(h);
            } else {
                left += 1;
            }
        }
        if left > 0 {
            let name = f.header.lines().next().unwrap_or("").rsplit(" b/").next().unwrap_or("").to_string();
            out.push_str(&format!("[{left} hunk(s) left out for size: git diff the file `{name}` to read them]\n"));
        }
    }
    out
}

pub(super) fn untracked(cwd: &Path) -> String {
    git(cwd, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default()
}

pub(super) fn untracked_note(list: &str) -> String {
    let names: Vec<&str> = list.lines().take(40).collect();
    if names.is_empty() {
        return String::new();
    }
    format!("\nNew files git does not track yet (not in the diff; read them if they matter):\n{}\n", names.join("\n"))
}
