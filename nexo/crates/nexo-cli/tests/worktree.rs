//! `nexo --worktree` and `nexo worktree`, run as the binary against the mock provider on a temp repo.

use std::path::Path;
use std::process::{Command, Output};

const MOCK: &str =
    "provider = \"mock\"\nmodel = \"mock\"\n[providers.mock]\nprotocol = \"mock\"\nmodels = [\"mock\"]\n\
                    [context]\npolicy = \"plain\"\n";

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct Setup {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
}

fn setup(extra_config: &str) -> Setup {
    let (repo, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "first"]);
    std::fs::write(home.path().join("config.toml"), format!("{MOCK}{extra_config}")).unwrap();
    Setup { repo, home }
}

impl Setup {
    fn nexo(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_nexo"))
            .args(args)
            .current_dir(self.repo.path())
            .env("NEXO_CONFIG_DIR", self.home.path())
            .env("NEXO_DATA_DIR", self.home.path().join("data"))
            .env("NEXO_CACHE_DIR", self.home.path().join("cache"))
            .output()
            .expect("nexo runs")
    }

    fn worktrees(&self) -> Vec<String> {
        match std::fs::read_dir(self.repo.path().join(".nexo/worktrees")) {
            Ok(rd) => rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn a_write_in_a_worktree_session_lands_in_the_worktree() {
    let s = setup("");
    let out = s.nexo(&["-w", "feat", "--yolo", "-p", "tool:write {\"path\":\"made.txt\",\"content\":\"hi\\n\"}"]);
    assert!(out.status.success(), "{}", err(&out));
    let wt = s.repo.path().join(".nexo/worktrees/feat");
    assert!(wt.join("made.txt").is_file(), "the write is in the worktree");
    assert!(!s.repo.path().join("made.txt").exists(), "the main checkout is untouched");
    // it changed something: kept, with the way to merge and to remove it
    let e = err(&out);
    assert!(e.contains("worktree kept") && e.contains("nexo/feat") && e.contains("nexo worktree remove feat"), "{e}");
    assert!(git(s.repo.path(), &["branch", "--list", "nexo/feat"]).contains("nexo/feat"));
    let list = s.nexo(&["worktree", "list"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("feat"), "{}", err(&list));
    // the exclude entry is there once, and a second worktree does not add it again
    let again = s.nexo(&["--worktree", "second", "-p", "hello"]);
    assert!(again.status.success(), "{}", err(&again));
    let ex = std::fs::read_to_string(s.repo.path().join(".git/info/exclude")).unwrap();
    assert_eq!(ex.matches(".nexo/worktrees/").count(), 1, "{ex}");
    assert!(!s.repo.path().join(".gitignore").exists());
    // remove deletes the folder and the branch
    let rm = s.nexo(&["worktree", "remove", "feat"]);
    assert!(rm.status.success(), "{}", err(&rm));
    assert!(s.worktrees().is_empty());
    assert!(git(s.repo.path(), &["branch", "--list", "nexo/feat"]).is_empty());
}

#[test]
fn a_clean_worktree_is_removed_on_exit() {
    let s = setup("");
    let out = s.nexo(&["--worktree", "-p", "hello"]);
    assert!(out.status.success(), "{}", err(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "echo: hello");
    assert!(err(&out).contains("no changes and was removed"), "{}", err(&out));
    assert!(s.worktrees().is_empty());
    assert!(git(s.repo.path(), &["branch", "--list", "nexo/*"]).is_empty());
}

#[test]
fn setup_commands_run_and_included_files_are_copied() {
    let s = setup("[agent]\nworktree_setup = [\"echo done > setup.log\"]\n");
    std::fs::write(s.repo.path().join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(s.repo.path().join(".worktreeinclude"), ".env\n").unwrap();
    git(s.repo.path(), &["add", ".worktreeinclude"]);
    git(s.repo.path(), &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "include"]);
    // the setup log is read back by the session itself, so it must already exist when the tool runs
    let out = s.nexo(&["-w", "ws", "--yolo", "-p", "tool:bash {\"command\":\"cat .env setup.log > seen.txt\"}"]);
    assert!(out.status.success(), "{}", err(&out));
    let seen = std::fs::read_to_string(s.repo.path().join(".nexo/worktrees/ws/seen.txt")).unwrap();
    assert!(seen.contains("TOKEN=1") && seen.contains("done"), "{seen}");
}

#[test]
fn outside_a_repository_it_refuses() {
    let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(home.path().join("config.toml"), MOCK).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_nexo"))
            .args(args)
            .current_dir(dir.path())
            .env("NEXO_CONFIG_DIR", home.path())
            .env("NEXO_DATA_DIR", home.path().join("data"))
            .env("NEXO_CACHE_DIR", home.path().join("cache"))
            .env("GIT_CEILING_DIRECTORIES", dir.path().parent().unwrap())
            .output()
            .unwrap()
    };
    let out = run(&["-w", "-p", "hello"]);
    assert!(!out.status.success());
    assert!(err(&out).contains("not inside a git repository"), "{}", err(&out));
    assert!(!dir.path().join(".nexo").exists());
    let out = run(&["worktree", "list"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(err(&out).contains("not inside a git repository"));
}
