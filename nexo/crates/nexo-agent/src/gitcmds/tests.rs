use super::run::git;
use super::*;
use std::path::Path;
use std::process::Command;

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
