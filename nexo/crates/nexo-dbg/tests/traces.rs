#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `strace`, `ltrace`, hooks and coverage on a small C program of our own (skipped when there is no C
//! compiler). Every line checked is one the program really produced.

use nexo_dbg::{open, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

const SRC: &str = r#"#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
__attribute__((noinline)) int check(int x) { if (x > 10) return 1; return 0; }
__attribute__((noinline)) int big(void) { return 7; }
__attribute__((noinline)) int small(void) { return 3; }
int main(int argc, char **argv) {
    int fd = open("/nonexistent/nexo-hooks", O_RDONLY);
    write(1, "hello\n", 6);
    int n = argc > 1 ? atoi(argv[1]) : 0;
    size_t l = strlen(argv[0]);
    int r = check(n);
    int v = r ? big() : small();
    printf("r=%d v=%d fd=%d long=%d\n", r, v, fd, l > 3);
    return r;
}
"#;

fn build(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let cc = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))?;
    let src = dir.join("hooks.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.join("hooks");
    let ok = std::process::Command::new(cc)
        .args(["-g", "-O0", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .ok()?
        .success();
    (ok && exe.is_file()).then_some(exe)
}

fn start(args: &[&str]) -> Option<(tempfile::TempDir, Session)> {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path()) else {
        eprintln!("no C compiler: skipped");
        return None;
    };
    let (s, _) = open(&Kind::Run { exe, args: args.iter().map(|a| a.to_string()).collect() }).expect("start");
    Some((dir, s))
}

#[test]
fn strace_writes_down_each_system_call_with_its_arguments_and_result() {
    let Some((_d, mut s)) = start(&[]) else { return };
    assert!(run(&mut s, "strace").contains("tracing system calls: every call"));
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    assert!(end.contains("strace: ") && end.contains("new lines"), "{end}");
    let t = run(&mut s, "strace show");
    assert!(t.contains("openat(AT_FDCWD, \"/nonexistent/nexo-hooks\", O_RDONLY) = -1 ENOENT"), "{t}");
    assert!(t.contains("write(1, \"hello\\n\", 6) = 6"), "{t}");
    assert!(t.contains("exit_group(0) = ? (the program ended in it)"), "{t}");
    // only what was asked for
    let f = run(&mut s, "strace show openat");
    assert!(f.contains("openat(") && !f.contains("write(1"), "{f}");
    assert!(s.exec(&split_words("strace show nonsense")).is_err());
}

#[test]
fn strace_with_a_group_keeps_only_that_group() {
    let Some((_d, mut s)) = start(&[]) else { return };
    run(&mut s, "strace file");
    run(&mut s, "cont");
    let t = run(&mut s, "strace show");
    assert!(t.contains("openat(AT_FDCWD, \"/nonexistent/nexo-hooks\""), "{t}");
    assert!(!t.contains("exit_group") && !t.contains("brk("), "{t}");
}

#[test]
fn catch_syscall_stops_at_the_start_and_the_end_of_the_call() {
    let Some((_d, mut s)) = start(&[]) else { return };
    assert!(run(&mut s, "catch syscall write").contains("stops at the start and the end of: write"));
    let a = run(&mut s, "cont");
    assert!(a.contains("system call write(1, \"hello\\n\", 6) (about to run)"), "{a}");
    assert_eq!(s.last_stop.as_ref().map(|x| x.event.as_str()), Some("syscall"));
    let b = run(&mut s, "cont");
    assert!(b.contains("write(1, \"hello\\n\", 6) = 6 (returned)"), "{b}");
    run(&mut s, "catch syscall off");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
}

#[test]
fn ltrace_writes_each_library_call_with_typed_arguments_and_its_result() {
    let Some((_d, mut s)) = start(&["12"]) else { return };
    assert!(run(&mut s, "ltrace").contains("tracing library calls: every imported function"));
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 1"), "{end}");
    assert!(end.contains("ltrace: ") && end.contains("new lines"), "{end}");
    let t = run(&mut s, "ltrace show");
    assert!(t.contains("open(\"/nonexistent/nexo-hooks\", 0, ...) = -1"), "{t}");
    assert!(t.contains("write(1, 0x") && t.contains(", 6) = 6"), "{t}");
    assert!(t.contains("atoi(\"12\") = 12"), "{t}");
    assert!(t.contains("printf(\"r=%d v=%d fd=%d long=%d\\n\", ...) = 21"), "{t}");
    // never returns to its caller: said so, not made up
    assert!(t.contains("__libc_start_main(") && t.contains("= ? (its return was not seen)"), "{t}");
    let f = run(&mut s, "ltrace show atoi");
    assert!(f.contains("atoi(") && !f.contains("printf("), "{f}");
}

#[test]
fn ltrace_with_names_keeps_only_those_and_refuses_an_unknown_one() {
    let Some((_d, mut s)) = start(&["3"]) else { return };
    let e = s.exec(&split_words("ltrace no_such_import")).unwrap_err().to_string();
    assert!(e.contains("imports no") && e.contains("atoi"), "{e}");
    run(&mut s, "ltrace strlen");
    run(&mut s, "cont");
    let t = run(&mut s, "ltrace show");
    assert!(t.contains("strlen(\"") && t.contains("hooks\") = "), "{t}");
    assert!(!t.contains("atoi(") && !t.contains("printf("), "{t}");
}
