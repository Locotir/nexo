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

/// vfork, then exec of the program named in argv[1]: what posix_spawn and system() do. The child calls
/// `before_exec` in the memory it shares with the parent; the parent calls `after_wait` once the child is done.
const SPAWNS: &str = r#"
#include <unistd.h>
#include <sys/wait.h>
__attribute__((noinline)) int before_exec(int x) { return x + 1; }
__attribute__((noinline)) int after_wait(int x) { return x + 1; }
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t p = vfork();
    if (p == 0) {
        before_exec(1);
        execl(argv[1], argv[1], (char *)0);
        _exit(127);
    }
    int st = 0;
    waitpid(p, &st, 0);
    after_wait(st);
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

/// One thread vforks and its child sleeps before the exec, while the main thread reaches `tick`.
const THREADS: &str = r#"
#include <pthread.h>
#include <unistd.h>
#include <sys/wait.h>
static const char *prog;
__attribute__((noinline)) int tick(int x) { return x + 1; }
static void *spawn(void *arg) {
    (void)arg;
    pid_t p = vfork();
    if (p == 0) {
        usleep(600000);
        execl(prog, prog, (char *)0);
        _exit(127);
    }
    int st = 0;
    waitpid(p, &st, 0);
    return 0;
}
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    prog = argv[1];
    pthread_t t;
    pthread_create(&t, 0, spawn, 0);
    usleep(300000);
    tick(1);
    pthread_join(t, 0);
    return 0;
}
"#;

/// The parent writes a global right after the fork; the child outlives that write.
const WRITES: &str = r#"
#include <stdio.h>
#include <unistd.h>
#include <sys/wait.h>
volatile int g;
int main(void) {
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        usleep(500000);
        _exit(0);
    }
    g = 5;
    printf("parent wrote %d\n", g);
    fflush(stdout);
    waitpid(p, 0, 0);
    return 0;
}
"#;

/// A function that forks, for a debugger call.
const CALLS: &str = r#"
#include <stdio.h>
#include <unistd.h>
__attribute__((noinline)) int forker(void) {
    pid_t p = fork();
    if (p == 0) {
        usleep(300000);
        _exit(0);
    }
    return 7;
}
__attribute__((noinline)) void stop_here(void) {}
int main(void) {
    stop_here();
    printf("main done\n");
    fflush(stdout);
    return 0;
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

/// `call`, with an answer within `secs` seconds or a failed test (a session that hangs never answers).
fn call_within(state: &std::path::Path, words: &[&str], secs: u64) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let s = state.to_path_buf();
    let w: Vec<String> = words.iter().map(|x| x.to_string()).collect();
    std::thread::spawn(move || {
        let w: Vec<&str> = w.iter().map(String::as_str).collect();
        let _ = tx.send(call(&s, &w));
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .unwrap_or_else(|_| panic!("no answer to {words:?} in {secs}s: the session hangs"))
}

/// The session of the one child kept, from the `follow` listing.
fn kept_child(state: &std::path::Path) -> String {
    let f = call(state, &["follow"]);
    f.lines()
        .find_map(|l| l.split("): session ").nth(1))
        .map(|t| t.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>())
        .unwrap_or_else(|| panic!("no kept child: {f}"))
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
    // the child runs to its exec under the debugger and is kept there; the parent then waits for it
    let running = call(s, &["cont", "--wait", "2"]);
    assert!(!running.contains("exited"), "{running}");
    let id = kept_child(s);
    let exec = call(s, &["-s", &id, "status"]);
    assert!(exec.contains("stopped at its exec of "), "{exec}");
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

#[test]
fn a_thread_waiting_in_vfork_does_not_hang_a_stop_in_another_thread_under_follow_both() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "threads", THREADS) else { return };
    let Some(second) = cc(work.path(), "second", SECOND) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), second.to_str().unwrap()]);
    call(s, &["follow", "both"]);
    call(s, &["break", "tick"]);
    // the main thread reaches `tick` while the other one waits for its vfork child, which has not exec'd yet
    let hit = call_within(s, &["cont", "--wait", "20"], 40);
    assert!(hit.contains("breakpoint #1 (thread") && hit.contains("tick"), "{hit}");
    let on = call_within(s, &["cont", "--wait", "3"], 40);
    assert!(!on.contains("exited"), "the parent waits for the child kept at its exec: {on}");
    let id = kept_child(s);
    let kid = call_within(s, &["-s", &id, "cont", "--wait", "20"], 40);
    assert!(kid.contains("exited with code 0"), "{kid}");
    let done = call_within(s, &["wait", "--wait", "20"], 40);
    assert!(done.contains("exited with code 0"), "{done}");
    let _ = call(s, &["-s", &id, "end"]);
    let _ = call(s, &["end"]);
}

#[test]
fn ending_a_vfork_child_kept_under_follow_both_leaves_the_parents_breakpoints_in_place() {
    let work = tempfile::tempdir().unwrap();
    let Some(spawner) = cc(work.path(), "spawns", SPAWNS) else { return };
    let Some(second) = cc(work.path(), "second", SECOND) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", spawner.to_str().unwrap(), second.to_str().unwrap()]);
    call(s, &["follow", "both"]);
    call(s, &["break", "after_wait"]);
    let running = call(s, &["cont", "--wait", "2"]);
    assert!(!running.contains("exited"), "{running}");
    let id = kept_child(s);
    let end = call(s, &["-s", &id, "end"]);
    assert!(!end.starts_with("error"), "{end}");
    let hit = call(s, &["wait", "--wait", "20"]);
    assert!(
        hit.contains("breakpoint #1 (thread") && hit.contains("after_wait"),
        "the parent's breakpoint still fires: {hit}"
    );
    let _ = call(s, &["end"]);
}

#[test]
fn under_follow_parent_a_vfork_child_runs_past_a_breakpoint_before_its_exec() {
    let work = tempfile::tempdir().unwrap();
    let Some(spawner) = cc(work.path(), "spawns", SPAWNS) else { return };
    let Some(second) = cc(work.path(), "second", SECOND) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", spawner.to_str().unwrap(), second.to_str().unwrap()]);
    let b = call(s, &["break", "before_exec"]);
    assert!(b.contains("breakpoint #1"), "{b}");
    // only the child calls `before_exec`: it runs over the breakpoint, execs and exits 0, and so does the parent
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    let all = call(s, &["out", "--all"]);
    assert!(all.contains("second 42"), "{all}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_parent_let_go_under_follow_child_keeps_no_hardware_watch() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "writes", WRITES) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    let w = call(s, &["watch", "g", "4", "w"]);
    assert!(w.contains("watch #"), "{w}");
    call(s, &["follow", "child"]);
    let at = call(s, &["cont", "--wait", "20"]);
    assert!(at.contains("under `follow child` the child is session"), "{at}");
    let id = session_in(&at);
    // the parent writes `g` without the debugger: no armed debug register is left to kill it with SIGTRAP
    let end = call(s, &["cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
    let out = call(s, &["out", "--all"]);
    assert!(out.contains("parent wrote 5"), "{out}");
    let _ = call(s, &["-s", &id, "end"]);
}

#[test]
fn a_debugger_call_that_forks_under_follow_child_puts_the_thread_back_before_the_parent_is_let_go() {
    let work = tempfile::tempdir().unwrap();
    let Some(exe) = cc(work.path(), "calls", CALLS) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap()]);
    call(s, &["break", "stop_here"]);
    let at = call(s, &["cont", "--wait", "20"]);
    assert!(at.contains("stop_here"), "{at}");
    call(s, &["follow", "child"]);
    let c = call(s, &["call", "forker()"]);
    assert!(!c.contains("the program ended during it"), "{c}");
    assert!(c.contains("`forker()` did not return") && c.contains("is back as before the call"), "{c}");
    let id = session_in(&c);
    // the parent goes on from `stop_here`, as before the call
    let end = call(s, &["cont", "--wait", "20"]);
    assert!(end.contains("exited with code 0"), "{end}");
    let out = call(s, &["out", "--all"]);
    assert!(out.contains("main done"), "{out}");
    let _ = call(s, &["-s", &id, "end"]);
}
