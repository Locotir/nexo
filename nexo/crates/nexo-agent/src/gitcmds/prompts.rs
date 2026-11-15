//! The prompts of `/review`, `/commit` and `/pr`, with their git facts gathered up front.

use super::diff::*;
use super::run::*;
use std::path::Path;
use std::process::Command;

pub(super) const REVIEW_RULES: &str =
    "Review it for, in this order: correctness bugs (wrong logic, off-by-one, unhandled \
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

pub(super) fn review_facts(cwd: &Path, target: &Target) -> Result<(String, bool), String> {
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
