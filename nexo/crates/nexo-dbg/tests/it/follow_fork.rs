#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `follow parent|child|both`: a child the program forks is let go (the default) or kept as a session of its own,
//! reached with `-s <id>`, with the breakpoints copied; an exec in a kept child names the new program's modules.
//! The programs are compiled here; skipped without `cc`.

use nexo_dbg::{execute, parse, Host};

/// The parent forks, does its own work in `after_fork` and waits; only the child calls `secret`.
const FORKS: &str = r#"
#include <stdio.h>
#include <unistd.h>
#include <sys/wait.h>
__attribute__((noinline)) int secret(int x) { return x * 3 + 1; }
__attribute__((noinline)) int after_fork(int x) { return x + 1; }
int main(void) {
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        printf("child %d\n", secret(7));
        fflush(stdout);
        _exit(0);
    }
    after_fork(1);
    int st = 0;
    waitpid(p, &st, 0);
    printf("parent done\n");
    return 0;
}
"#;

/// vfork, then exec of the program named in argv[1]: what posix_spawn and system() do.
const SPAWNS: &str = r#"
#include <unistd.h>
#include <sys/wait.h>
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t p = vfork();
    if (p == 0) {
        execl(argv[1], argv[1], (char *)0);
        _exit(127);
    }
    int st = 0;
    waitpid(p, &st, 0);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
"#;

/// The program a spawned child execs.
const SECOND: &str = r#"
#include <stdio.h>
__attribute__((noinline)) int in_second(int x) { return x + 2; }
int main(void) {
    printf("second %d\n", in_second(40));
    return 0;
}
"#;

/// system() runs a shell; the shell is the child.
const SYSTEM: &str = r#"
#include <stdlib.h>
__attribute__((noinline)) int secret(int x) { return x * 3 + 1; }
int main(void) {
    int r = system("true");
    return r == 0 ? 0 : 1;
}
"#;

fn cc(dir: &std::path::Path, name: &str, src: &str) -> Option<std::path::PathBuf> {
    let c = dir.join(format!("{name}.c"));
    std::fs::write(&c, src).unwrap();
    let exe = dir.join(name);
    let ok = std::process::Command::new("cc")
        .args(["-O0", "-g", "-o"])
        .arg(&exe)
        .arg(&c)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    Some(exe)
}

fn call(state: &std::path::Path, words: &[&str]) -> String {
    let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    match parse(&w).and_then(|c| execute(&c, state, &Host::Thread, "nexo dbg")) {
        Ok(s) => s,
        Err(e) => format!("error: {e}"),
    }
}

/// The session id after "is session " in an answer.
fn session_in(text: &str) -> String {
    let at = text.find("is session ").unwrap_or_else(|| panic!("no kept child in: {text}"));
    text[at + "is session ".len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect()
}

#[test]
fn by_default_a_breakpoint_only_the_child_reaches_is_never_hit_and_the_answer_names_the_fork() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "forks", FORKS) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    let b = call(s, &["break", "secret"]);
    assert!(b.contains("breakpoint #1"), "{b}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    assert!(out.contains("was never hit"), "{out}");
    assert!(out.contains("the program forked 1 child that ran without the debugger: pid "), "{out}");
    assert!(out.contains("(fork at 0x"), "{out}");
    let f = call(s, &["follow"]);
    assert!(f.contains("follow parent") && f.contains("let go"), "{f}");
    let _ = call(s, &["end"]);
}

#[test]
fn under_follow_both_the_child_is_a_session_of_its_own_and_hits_the_copied_breakpoint() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "forks", FORKS) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    let f = call(s, &["follow", "both"]);
    assert!(f.contains("follow both"), "{f}");
    call(s, &["break", "secret"]);
    call(s, &["break", "after_fork"]);
    // the parent goes on to its own breakpoint; the answer says the child is kept and how to reach it
    let parent = call(s, &["cont", "--wait", "20"]);
    assert!(parent.contains("after_fork"), "{parent}");
    let id = session_in(&parent);
    assert!(parent.contains(&format!("`-s {id} cont` runs it")), "{parent}");
    // a call without `-s` still goes to the session that started the program
    let st = call(s, &["status"]);
    assert!(!st.starts_with("error"), "{st}");
    let first = call(s, &["-s", &id, "status"]);
    assert!(
        first.contains("stopped where fork returned 0 in it") && first.contains("breakpoints #1 #2 copied"),
        "{first}"
    );
    let hit = call(s, &["-s", &id, "cont", "--wait", "20"]);
    assert!(hit.contains("secret"), "the child stops in secret: {hit}");
    let bt = call(s, &["-s", &id, "breaks"]);
    assert!(bt.contains("hits 1") || bt.contains("hits: 1") || bt.contains("1 hit"), "{bt}");
    let end = call(s, &["-s", &id, "cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
    let done = call(s, &["cont", "--wait", "20"]);
    assert!(done.contains("exited with code 0"), "{done}");
    let out = call(s, &["out", "--all"]);
    assert!(out.contains("parent done"), "{out}");
    let _ = call(s, &["-s", &id, "end"]);
    let _ = call(s, &["end"]);
}

#[test]
fn under_follow_child_the_parent_is_let_go_and_the_child_becomes_the_default_session() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "forks", FORKS) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    call(s, &["follow", "child"]);
    call(s, &["break", "secret"]);
    let at = call(s, &["cont", "--wait", "20"]);
    assert!(at.contains("under `follow child` the child is session"), "{at}");
    let id = session_in(&at);
    // no `-s`: the parent's session is gone, the child's is the only one
    let hit = call(s, &["cont", "--wait", "20"]);
    assert!(hit.contains("secret"), "{hit}");
    let end = call(s, &["cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
    let _ = call(s, &["-s", &id, "end"]);
}

#[test]
fn a_vfork_child_kept_under_follow_both_execs_and_its_answer_names_the_new_program() {
    let work = tempfile::tempdir().unwrap();
    let Some(spawner) = cc(work.path(), "spawns", SPAWNS) else { return };
    let Some(second) = cc(work.path(), "second", SECOND) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", spawner.to_str().unwrap(), second.to_str().unwrap()]);
    call(s, &["follow", "both"]);
    // the parent waits inside vfork until the child execs; the child is held stopped, so the parent runs on
    let running = call(s, &["cont", "--wait", "2"]);
    assert!(!running.contains("exited"), "{running}");
    let f = call(s, &["follow"]);
    assert!(f.contains("vfork at"), "{f}");
    let id = f
        .lines()
        .find_map(|l| l.split("): session ").nth(1))
        .map(|t| t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>())
        .unwrap_or_else(|| panic!("no kept child: {f}"));
    let exec = call(s, &["-s", &id, "cont", "--wait", "20"]);
    assert!(exec.contains("modules now: ") && exec.contains("second at 0x"), "{exec}");
    let b = call(s, &["-s", &id, "break", "in_second"]);
    assert!(b.contains("breakpoint #"), "the new program's names resolve: {b}");
    let hit = call(s, &["-s", &id, "cont", "--wait", "20"]);
    assert!(hit.contains("in_second"), "{hit}");
    let end = call(s, &["-s", &id, "cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
    let done = call(s, &["wait", "--wait", "20"]);
    assert!(done.contains("exited with code 0"), "{done}");
    let _ = call(s, &["-s", &id, "end"]);
    let _ = call(s, &["end"]);
}

#[test]
fn the_shell_system_starts_is_a_child_the_never_hit_answer_names() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "system", SYSTEM) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    call(s, &["break", "secret"]);
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "the shell ran untraced and returned: {out}");
    assert!(out.contains("the program forked 1 child that ran without the debugger"), "{out}");
    assert!(out.contains("vfork at") || out.contains("fork at"), "{out}");
    let _ = call(s, &["end"]);
}
