//! The git commands `/review`, `/commit` and `/pr` (and `nexo review`): the git facts are
//! gathered here, up front, and put into one well-made prompt, so the model does not spend
//! steps asking for the status, the diff and the log. A diff too big for the prompt is cut
//! by file and by hunk (the biggest hunks stay), never summarised by a model.

use std::path::Path;
use std::process::Command;

/// Most characters of diff put in a prompt.
pub const DIFF_CAP: usize = 48_000;

/// Runs `git` in `cwd`; the output on success, the error text on failure.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
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

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    git(cwd, args).is_ok()
}

fn ensure_repo(cwd: &Path) -> Result<(), String> {
    git(cwd, &["rev-parse", "--is-inside-work-tree"]).map(|_| ()).map_err(|e| {
        if e.starts_with("git is not") {
            e
        } else {
            "this directory is not in a git repository".into()
        }
    })
}

fn has_commits(cwd: &Path) -> bool {
    git_ok(cwd, &["rev-parse", "--verify", "-q", "HEAD"])
}

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
fn main_branch(cwd: &Path) -> Option<String> {
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
fn diff_for(cwd: &Path, target: &Target) -> Result<(String, String, String), String> {
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

struct FileDiff {
    header: String,
    hunks: Vec<String>,
}

fn split_diff(diff: &str) -> Vec<FileDiff> {
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

fn untracked(cwd: &Path) -> String {
    git(cwd, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default()
}

fn untracked_note(list: &str) -> String {
    let names: Vec<&str> = list.lines().take(40).collect();
    if names.is_empty() {
        return String::new();
    }
    format!("\nNew files git does not track yet (not in the diff; read them if they matter):\n{}\n", names.join("\n"))
}

const REVIEW_RULES: &str = "Review it for, in this order: correctness bugs (wrong logic, off-by-one, unhandled \
errors, races, broken edge cases), security problems (injection, unsafe input handling, secrets, permissions, \
path traversal), and missing or weak tests for the changed behaviour. Read the surrounding code with your tools \
when the diff alone does not settle a doubt; do not flag style or taste. Do not edit any file and do not run \
commands that change anything. Report only what you are fairly sure of, and say how sure when unsure.";

/// The `/review` prompt: the facts, then the task. Findings come as a ranked list.
pub fn review_prompt(cwd: &Path, target: &Target) -> Result<String, String> {
    let (facts, _) = review_facts(cwd, target)?;
    Ok(format!(
        "{facts}\n{REVIEW_RULES}\n\nAnswer with the findings ranked from the most to the least severe, each as \
`N. [HIGH|MEDIUM|LOW] path:line - what is wrong, why it matters, how to fix it` (the line is in the new file). \
Then one line: how many of each, and whether the change is fit to merge. If there is nothing to report, say so in \
one line. Make no edits."
    ))
}

/// The prompt of `nexo review`: the same review, answered as JSON only.
pub fn review_prompt_json(cwd: &Path, target: &Target) -> Result<String, String> {
    let (facts, _) = review_facts(cwd, target)?;
    Ok(format!(
        "{facts}\n{REVIEW_RULES}\n\nAnswer with one JSON object and nothing else (no prose, no code fence): \
{{\"findings\":[{{\"severity\":\"high|medium|low\",\"file\":\"path\",\"line\":12,\"title\":\"short\",\
\"detail\":\"what is wrong, why, how to fix\"}}]}}, ranked from the most to the least severe. \
Use \"high\" only for a bug, a vulnerability or a missing guard that would hurt in production. \
An empty list when there is nothing to report."
    ))
}

fn review_facts(cwd: &Path, target: &Target) -> Result<(String, bool), String> {
    ensure_repo(cwd)?;
    let (what, stat, diff) = diff_for(cwd, target)?;
    let new_files = if matches!(target, Target::Default | Target::Base(_)) { untracked(cwd) } else { String::new() };
    if diff.trim().is_empty() && new_files.trim().is_empty() {
        return Err(format!("nothing to review: no changes in {what}"));
    }
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let log = git(cwd, &["log", "-8", "--format=%h %s"]).unwrap_or_default();
    let capped = diff.len() > DIFF_CAP;
    let body = cap_diff(&diff, DIFF_CAP);
    let note = if capped {
        "\nThe diff was too big to include whole: the biggest hunks of each file are below, the rest is marked. \
Read the files for the parts left out.\n"
    } else {
        ""
    };
    Ok((
        format!(
            "Review {what} in this repository (branch {}).\n\nRecent commits:\n{}\nFiles changed:\n{}{}{note}\nDiff:\n```diff\n{}```\n",
            branch.trim(),
            log,
            stat,
            untracked_note(&new_files),
            body
        ),
        capped,
    ))
}

/// The `/commit` prompt: staged changes (or all, said first), the repo's own message style.
pub fn commit_prompt(cwd: &Path, hint: Option<&str>) -> Result<String, String> {
    ensure_repo(cwd)?;
    let status = git(cwd, &["status", "--short"])?;
    if status.trim().is_empty() {
        return Err("nothing to commit: the working tree is clean".into());
    }
    let staged = git(cwd, &["diff", "--cached", "--stat=120"])?;
    let has_staged = !staged.trim().is_empty();
    let (scope, stat, diff) = if has_staged {
        let d = git(cwd, &["diff", "--cached", "-M", "--no-ext-diff", "--no-color"])?;
        ("the staged changes".to_string(), staged, d)
    } else {
        let head = if has_commits(cwd) { "HEAD" } else { "--cached" };
        let d = git(cwd, &["diff", "-M", "--no-ext-diff", "--no-color", head]).unwrap_or_default();
        let s = git(cwd, &["diff", "-M", "--no-ext-diff", "--no-color", "--stat=120", head]).unwrap_or_default();
        ("all the changes (nothing is staged)".to_string(), s, d)
    };
    let log = git(cwd, &["log", "-10", "--format=%s"]).unwrap_or_default();
    let step = if has_staged {
        "Commit what is staged and nothing else: `git commit -m <message>` (use more than one -m, or a here-document, for a body)."
    } else {
        "Nothing is staged, so start your reply by saying you will commit all the changes (new files included), then run `git add -A` and commit with `git commit -m <message>`."
    };
    let hint = hint.map(|h| format!("The person asks: {h}\n")).unwrap_or_default();
    Ok(format!(
        "Make a git commit of {scope}.\n{hint}\nStatus:\n{status}\nStat:\n{stat}\nThe last subjects in this repository, \
whose style (language, case, prefixes like `fix:` or none, length, mood) your message must follow:\n{log}\nDiff:\n```diff\n{}```\n\n\
{step} Write the message from the diff: one subject line of at most 72 characters saying what changed and why; add a \
short body only when the subject cannot carry it. NEVER add a trailer, a Co-Authored-By line, a Signed-off-by \
line or any note about who or what wrote it. Do not use --no-verify or --amend, and do not push. If a hook fails, \
read the error and fix the cause. After the commit, show `git log -1 --stat`.",
        cap_diff(&diff, DIFF_CAP)
    ))
}

/// Whether the GitHub CLI `gh` is installed and signed in; else the plain reason.
pub fn gh_ready(gh: &str) -> Result<(), String> {
    match Command::new(gh).args(["auth", "status"]).output() {
        Err(_) => Err(format!(
            "the GitHub CLI ({gh}) is not installed, so no pull request can be opened. Install it from https://cli.github.com and run `gh auth login`."
        )),
        Ok(o) if !o.status.success() => {
            Err(format!("the GitHub CLI ({gh}) is not signed in. Run `gh auth login`, then try /pr again."))
        }
        Ok(_) => Ok(()),
    }
}

/// The `/pr` prompt: branch facts, the commits since the base, and the steps.
pub fn pr_prompt(cwd: &Path, base: Option<&str>, gh: &str) -> Result<String, String> {
    ensure_repo(cwd)?;
    gh_ready(gh)?;
    if !has_commits(cwd) {
        return Err("no commit yet: nothing to open a pull request for".into());
    }
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    if branch == "HEAD" {
        return Err("HEAD is detached: switch to a branch first".into());
    }
    let base = match base.map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => main_branch(cwd).ok_or("no main or master branch found: say the base, /pr <base>")?,
    };
    let base_short = base.strip_prefix("origin/").unwrap_or(&base).to_string();
    if branch == base_short {
        return Err(format!(
            "you are on {base_short} itself: switch to the branch with the work, or give another base"
        ));
    }
    let range = format!("{base}..HEAD");
    let commits = git(cwd, &["log", &range, "--format=%h %s%n%b%n--"])
        .map_err(|e| format!("cannot compare with '{base}': {e}"))?;
    if commits.trim().is_empty() {
        return Err(format!("{branch} has no commit that {base} lacks: nothing to open a pull request for"));
    }
    let stat = git(cwd, &["diff", "--stat=120", &format!("{base}...HEAD")]).unwrap_or_default();
    let upstream =
        git(cwd, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]).map(|s| s.trim().to_string());
    let push = match &upstream {
        Err(_) => "The branch has no upstream: push it with `git push -u origin HEAD`.".to_string(),
        Ok(u) => {
            let ahead = git(cwd, &["rev-list", "--count", &format!("{u}..HEAD")]).unwrap_or_default();
            if ahead.trim() == "0" {
                format!("The branch is already pushed to {u}: do not push again.")
            } else {
                format!("The branch is {} commit(s) ahead of {u}: push it with `git push`.", ahead.trim())
            }
        }
    };
    let dirty = !git(cwd, &["status", "--short"]).unwrap_or_default().trim().is_empty();
    let dirty_note = if dirty {
        "There are uncommitted changes; they are not part of the pull request, say so in your reply.\n"
    } else {
        ""
    };
    Ok(format!(
        "Open a pull request for branch {branch} into {base_short}.\n\nCommits since {base}:\n{}\nFiles changed:\n{stat}\n{dirty_note}\n\
{push} Then run `gh pr create --base {base_short} --title <title> --body <body>`. The title is one line of at most \
70 characters, in the voice of the commits above; the body says what changed and why, in a few lines or a short \
list, and how it was tested if the commits tell. Take both from the commits, not from guesses. Add no \"generated \
by\" line and no co-author line. Do not force-push. Show the pull request URL at the end. If a pull request for \
this branch already exists, say so and show its URL instead.",
        commits.trim_end()
    ))
}

/// One finding of `nexo review`.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: String,
    pub file: String,
    pub line: Option<u64>,
    pub title: String,
    pub detail: String,
}

/// Reads the model's JSON answer (it may be wrapped in a fence or prose).
pub fn parse_findings(text: &str) -> Result<Vec<Finding>, String> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else { return Err("the reply holds no JSON object".into()) };
    let v: serde_json::Value =
        serde_json::from_str(&text[a..=b]).map_err(|e| format!("the reply is not valid JSON: {e}"))?;
    let list = v.get("findings").and_then(|f| f.as_array()).ok_or("the reply has no \"findings\" list")?;
    let s = |x: &serde_json::Value, k: &str| x.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    Ok(list
        .iter()
        .map(|f| Finding {
            severity: s(f, "severity").to_ascii_lowercase(),
            file: s(f, "file"),
            line: f.get("line").and_then(|l| l.as_u64()),
            title: s(f, "title"),
            detail: s(f, "detail"),
        })
        .collect())
}

/// Whether CI should fail on these findings.
pub fn has_high(findings: &[Finding]) -> bool {
    findings.iter().any(|f| matches!(f.severity.as_str(), "high" | "critical"))
}

/// The findings as plain text, one block each.
pub fn render_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "No findings.\n".into();
    }
    let mut out = String::new();
    for (i, f) in findings.iter().enumerate() {
        let at = match f.line {
            Some(l) => format!("{}:{l}", f.file),
            None => f.file.clone(),
        };
        out.push_str(&format!("{}. [{}] {at} - {}\n", i + 1, f.severity.to_ascii_uppercase(), f.title));
        if !f.detail.is_empty() {
            out.push_str(&format!("   {}\n", f.detail));
        }
    }
    out
}

/// The findings as JSON, for scripts.
pub fn findings_json(findings: &[Finding]) -> String {
    let list: Vec<_> = findings
        .iter()
        .map(|f| serde_json::json!({"severity": f.severity, "file": f.file, "line": f.line, "title": f.title, "detail": f.detail}))
        .collect();
    serde_json::json!({"findings": list, "high": has_high(findings)}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let o = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        run(d.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        run(d.path(), &["add", "."]);
        run(d.path(), &["commit", "-q", "-m", "feat: first"]);
        d
    }

    #[test]
    fn review_default_sees_branch_and_uncommitted_and_new_files() {
        let d = repo();
        run(d.path(), &["checkout", "-q", "-b", "work"]);
        std::fs::write(d.path().join("a.txt"), "one\ntwo\n").unwrap();
        run(d.path(), &["commit", "-qam", "fix: two"]);
        std::fs::write(d.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.path().join("new.rs"), "fn x() {}\n").unwrap();
        let p = review_prompt(d.path(), &Target::Default).unwrap();
        assert!(p.contains("+two") && p.contains("+three"), "{p}");
        assert!(p.contains("new.rs") && p.contains("branch work"));
        assert!(p.contains("Make no edits"));
    }

    #[test]
    fn review_staged_and_commit_and_base_targets() {
        let d = repo();
        std::fs::write(d.path().join("a.txt"), "one\nstaged\n").unwrap();
        run(d.path(), &["add", "a.txt"]);
        std::fs::write(d.path().join("b.txt"), "loose\n").unwrap();
        let p = review_prompt(d.path(), &Target::Staged).unwrap();
        assert!(p.contains("+staged") && !p.contains("loose"));
        run(d.path(), &["commit", "-qm", "wip"]);
        let sha = git(d.path(), &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(Target::parse(Some(sha.trim()), d.path()).unwrap(), Target::Commit(sha.trim().into()));
        assert_eq!(Target::parse(Some("main"), d.path()).unwrap(), Target::Base("main".into()));
        let p = review_prompt(d.path(), &Target::Commit(sha.trim().into())).unwrap();
        assert!(p.contains("+staged"));
        assert!(Target::parse(Some("a b c"), d.path()).is_err());
    }

    #[test]
    fn review_with_nothing_or_no_repo_says_so() {
        let d = repo();
        assert!(review_prompt(d.path(), &Target::Default).unwrap_err().contains("nothing to review"));
        let plain = tempfile::tempdir().unwrap();
        assert!(review_prompt(plain.path(), &Target::Default).unwrap_err().contains("not in a git repository"));
    }

    #[test]
    fn a_big_diff_keeps_the_biggest_hunks_and_marks_the_rest() {
        let mut diff = String::new();
        for f in 0..3 {
            diff.push_str(&format!("diff --git a/f{f} b/f{f}\n--- a/f{f}\n+++ b/f{f}\n"));
            diff.push_str("@@ -1 +1 @@\n-a\n+b\n");
            diff.push_str("@@ -50,1 +50,40 @@\n");
            for i in 0..40 {
                diff.push_str(&format!("+line {i} of file {f}\n"));
            }
        }
        let cap = diff.len() / 2;
        let out = cap_diff(&diff, cap);
        assert!(out.len() <= cap + 400, "{} vs {cap}", out.len());
        assert!(out.contains("hunk(s) left out for size") && out.contains("diff --git a/f2 b/f2"));
        assert!(out.contains("line 39 of file"), "the biggest hunk stays");
        assert_eq!(cap_diff(&diff, diff.len()), diff, "a diff that fits is untouched");
    }

    #[test]
    fn commit_prompt_follows_scope_style_and_forbids_trailers() {
        let d = repo();
        assert!(commit_prompt(d.path(), None).unwrap_err().contains("clean"));
        std::fs::write(d.path().join("a.txt"), "one\nmore\n").unwrap();
        let p = commit_prompt(d.path(), Some("mention the parser")).unwrap();
        assert!(p.contains("nothing is staged") && p.contains("git add -A"));
        assert!(p.contains("feat: first") && p.contains("mention the parser") && p.contains("NEVER add a trailer"));
        run(d.path(), &["add", "a.txt"]);
        std::fs::write(d.path().join("c.txt"), "x\n").unwrap();
        let p = commit_prompt(d.path(), None).unwrap();
        assert!(p.contains("the staged changes") && !p.contains("git add -A") && !p.contains("c.txt\n+"));
    }

    #[test]
    fn pr_without_gh_says_so_plainly() {
        let d = repo();
        let e = pr_prompt(d.path(), None, "nexo-no-such-gh-program").unwrap_err();
        assert!(e.contains("not installed") && e.contains("gh auth login"), "{e}");
    }

    #[test]
    fn pr_prompt_lists_commits_and_push_step() {
        let d = repo();
        run(d.path(), &["checkout", "-q", "-b", "topic"]);
        std::fs::write(d.path().join("a.txt"), "one\nx\n").unwrap();
        run(d.path(), &["commit", "-qam", "fix: the x"]);
        // `true` stands for an installed, signed-in gh
        let p = pr_prompt(d.path(), None, "true").unwrap();
        assert!(p.contains("fix: the x") && p.contains("git push -u origin HEAD") && p.contains("--base main"), "{p}");
        run(d.path(), &["checkout", "-q", "main"]);
        assert!(pr_prompt(d.path(), None, "true").unwrap_err().contains("itself"));
    }

    #[test]
    fn findings_parse_and_decide_the_exit() {
        let t = "```json\n{\"findings\":[{\"severity\":\"HIGH\",\"file\":\"a.rs\",\"line\":3,\"title\":\"t\",\"detail\":\"d\"},{\"severity\":\"low\",\"file\":\"b.rs\",\"title\":\"u\"}]}\n```";
        let f = parse_findings(t).unwrap();
        assert_eq!(f.len(), 2);
        assert!(has_high(&f) && !has_high(&f[1..]));
        assert!(render_findings(&f).contains("1. [HIGH] a.rs:3 - t"));
        assert!(findings_json(&f).contains("\"high\":true"));
        assert!(parse_findings("no json here").is_err());
        assert!(!has_high(&parse_findings("{\"findings\":[]}").unwrap()));
    }
}
