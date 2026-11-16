#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The pages a page watch guards, where the program cannot simply run one mprotect: a thread stopped at the start
//! of a system call, a parent let go at its fork, and children made while the pages are guarded (a fork child let
//! go or kept, a vfork child writing the memory it shares). The programs are compiled here; skipped without `cc`.

use nexo_dbg::{execute, parse, Host};

/// `call`: a system call, then a 64-byte buffer filled; otherwise a fork (`vfork` for a vfork) whose child writes the
/// buffer and exits with 9, and a parent that fills the buffer and reports, also into the file in argv[2].
const PROGRAM: &str = r#"
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/wait.h>
unsigned char buf[64];
__attribute__((noinline)) void fill(unsigned char *b, int n) {
    for (int i = 0; i < n; i++) b[i] = (unsigned char)(i * 3 + 1);
}
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    if (strcmp(argv[1], "call") == 0) {
        long r = syscall(SYS_getppid);
        fill(buf, 64);
        printf("getppid %s %d\n", r > 0 ? "ok" : "failed", buf[63]);
        return r > 0 ? 0 : 3;
    }
    fflush(stdout);
    pid_t p = strcmp(argv[1], "vfork") == 0 ? vfork() : fork();
    if (p == 0) {
        buf[1] = 7;
        _exit(9);
    }
    fill(buf, 64);
    int st = 0;
    waitpid(p, &st, 0);
    int code = WIFEXITED(st) ? WEXITSTATUS(st) : -WTERMSIG(st);
    char line[64];
    snprintf(line, sizeof line, "parent %d %d child %d\n", p > 0, buf[63], code);
    fputs(line, stdout);
    if (argc > 2) {
        FILE *f = fopen(argv[2], "w");
        if (f) {
            fputs(line, f);
            fclose(f);
        }
    }
    return 0;
}
"#;

fn cc(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let c = dir.join("guards.c");
    std::fs::write(&c, PROGRAM).unwrap();
    let exe = dir.join("guards");
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

/// The program started with `args`, `follow` set, stopped at `main` with `buf` watched through its page.
fn watched_at_main(state: &std::path::Path, exe: &std::path::Path, args: &[&str], follow: &str) {
    let mut run = vec!["run", exe.to_str().unwrap()];
    run.extend_from_slice(args);
    call(state, &run);
    call(state, &["follow", follow]);
    call(state, &["break", "main"]);
    let at = call(state, &["cont", "--wait", "20"]);
    assert!(at.contains("main"), "{at}");
    let w = call(state, &["watch", "buf"]);
    assert!(w.contains("(page)"), "{w}");
}

/// The parent's watch stops in `fill`, and with the watch gone the program ends well.
fn hit_in_fill_then_exit(state: &std::path::Path) {
    let hit = call(state, &["cont", "--wait", "20"]);
    assert!(hit.contains("page") && hit.contains("fill"), "{hit}");
    call(state, &["delete", "all"]);
    let end = call(state, &["cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
}

#[test]
fn a_stop_at_the_start_of_a_system_call_keeps_the_guard_and_the_call_runs_untouched() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path()) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    watched_at_main(s, &exe, &["call"], "parent");
    call(s, &["catch", "syscall", "getppid"]);
    let entry = call(s, &["cont", "--wait", "20"]);
    assert!(entry.contains("getppid") && entry.contains("about to run"), "{entry}");
    let exit = call(s, &["cont", "--wait", "20"]);
    assert!(exit.contains("(returned)") && !exit.contains("ENOSYS"), "{exit}");
    call(s, &["catch", "syscall", "off"]);
    hit_in_fill_then_exit(s);
    let out = call(s, &["out"]);
    assert!(out.contains("getppid ok 190"), "{out}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_parent_let_go_at_its_fork_gets_its_pages_back_and_the_kept_child_writes_them() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path()) else { return };
    let report = work.path().join("report");
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    watched_at_main(s, &exe, &["fork", report.to_str().unwrap()], "child");
    let at = call(s, &["cont", "--wait", "20"]);
    assert!(at.contains("under `follow child` the child is session"), "{at}");
    let end = call(s, &["cont", "--wait", "20"]);
    assert!(end.contains("exited with code 9"), "{end}");
    // the parent goes on alone: its line shows it filled the buffer and saw the child exit with 9
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut line = String::new();
    while std::time::Instant::now() < until {
        line = std::fs::read_to_string(&report).unwrap_or_default();
        if line.ends_with('\n') {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(line, "parent 1 190 child 9\n");
}

#[test]
fn a_fork_child_let_go_writes_its_copy_of_a_guarded_page_and_exits() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path()) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    watched_at_main(s, &exe, &["fork"], "parent");
    hit_in_fill_then_exit(s);
    let out = call(s, &["out"]);
    assert!(out.contains("parent 1 190 child 9"), "{out}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_vfork_child_writes_the_guarded_page_it_shares_and_the_parents_watch_still_stops() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path()) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    watched_at_main(s, &exe, &["vfork"], "both");
    hit_in_fill_then_exit(s);
    let out = call(s, &["out"]);
    assert!(out.contains("parent 1 190 child 9"), "{out}");
    let _ = call(s, &["end"]);
}
