#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Calls into the stopped program: `call add(2, 3)`, `print add(40, 2) + 1`, a text argument written on the stack,
//! eight arguments (two on the stack), a call that faults, and a breakpoint inside the function called. After each
//! one every register of the thread is what it was before, and the program runs on to its normal end. Skipped
//! without the compiler.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <stdio.h>
__attribute__((noinline)) int add(int a, int b) {
    int sum = a + b;
    return sum;
}
__attribute__((noinline)) unsigned long hash(const char *s) {
    unsigned long h = 5381;
    while (*s) {
        h = h * 33 + (unsigned char)*s;
        s++;
    }
    return h;
}
__attribute__((noinline)) long many(long a, long b, long c, long d, long e, long f, long g, long h) {
    return a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h;
}
__attribute__((noinline)) int crash(int *p) {
    return *p + 1;
}
__attribute__((noinline)) int deep(int n) {
    int twice = add(n, n);
    return twice + 1;
}
__attribute__((noinline)) int minus(int a) {
    return -a;
}
int counter;
__attribute__((noinline)) void tick(void) {
    counter++;
}
int main(void) {
    tick();
    puts("ready");
    return add(1, 2) == 3 ? 0 : 1;
}
"#;

/// Registers compared before and after a call.
const REGS: [&str; 18] = [
    "rip", "rsp", "rbp", "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14",
    "r15", "eflags",
];

fn build() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("p.c"), SRC).unwrap();
    let ok = std::process::Command::new("gcc")
        .args(["-g", "-O0", "-o", "p", "p.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no gcc: skipped");
        return None;
    }
    let exe = dir.path().join("p");
    Some((dir, exe))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// Stopped in `tick`, before `main` printed anything.
fn stopped_in_tick() -> Option<(tempfile::TempDir, Session)> {
    let (dir, exe) = build()?;
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break tick");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("tick"), "{stop}");
    Some((dir, s))
}

/// The values only: the names of the places they point at grow as the program loads more of its libraries.
fn regs(s: &mut Session) -> Vec<String> {
    REGS.iter().map(|r| run(s, &format!("print ${r}")).split(" (").next().unwrap_or("").to_string()).collect()
}

/// The program goes on from where it was stopped and ends as it would have without the calls.
fn ends_normally(s: &mut Session) {
    run(s, "delete all");
    let end = run(s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(s, "out --all");
    assert!(out.contains("ready"), "{out}");
}

fn djb2(s: &[u8]) -> u64 {
    s.iter().fold(5381u64, |h, c| h.wrapping_mul(33).wrapping_add(*c as u64))
}

#[test]
fn a_call_gives_the_value_of_a_function_and_puts_every_register_back() {
    let Some((_d, mut s)) = stopped_in_tick() else { return };
    let before = regs(&mut s);
    let out = run(&mut s, "call add(2, 3)");
    assert!(out.contains("add(2, 3) = 0x5 (5)"), "{out}");
    assert!(out.contains("back as before the call") && out.contains("every register as it was"), "{out}");
    assert_eq!(regs(&mut s), before);

    let out = run(&mut s, "print add(40, 2) + 1");
    assert!(out.contains("= 0x2b (43)"), "{out}");
    // eight arguments: rdi, rsi, rdx, rcx, r8, r9 and two on the stack
    let out = run(&mut s, "call many(1, 2, 3, 4, 5, 6, 7, 8)");
    assert!(out.contains("(204)"), "{out}");
    // an int below zero comes back in eax: the 32-bit value is named
    let out = run(&mut s, "call minus(7)");
    assert!(out.contains("-7 as a 32-bit int"), "{out}");
    assert_eq!(regs(&mut s), before);
    ends_normally(&mut s);
}

#[test]
fn a_text_argument_is_written_on_the_stack_for_the_function_to_read() {
    let Some((_d, mut s)) = stopped_in_tick() else { return };
    let before = regs(&mut s);
    let out = run(&mut s, "call hash(\"abc\")");
    assert!(out.contains(&format!("({})", djb2(b"abc"))), "{out}");
    let out = run(&mut s, "call hash(\"a b\\n\")");
    assert!(out.contains(&format!("({})", djb2(b"a b\n"))), "{out}");
    assert_eq!(regs(&mut s), before);
    ends_normally(&mut s);
}

#[test]
fn a_call_that_faults_says_where_and_leaves_the_program_as_it_was() {
    let Some((_d, mut s)) = stopped_in_tick() else { return };
    let before = regs(&mut s);
    let out = run(&mut s, "call crash(0)");
    assert!(out.contains("`crash(0)` faulted") && out.contains("SIGSEGV"), "{out}");
    assert!(out.contains("crash"), "{out}");
    assert!(out.contains("not handed to the program") && out.contains("back as before the call"), "{out}");
    assert_eq!(regs(&mut s), before);
    // a call works again after one that faulted
    let out = run(&mut s, "call add(1, 1)");
    assert!(out.contains("= 0x2 (2)"), "{out}");
    ends_normally(&mut s);
}

#[test]
fn a_breakpoint_inside_the_call_stops_there_and_cont_or_kill_call_ends_it() {
    let Some((_d, mut s)) = stopped_in_tick() else { return };
    let before = regs(&mut s);
    run(&mut s, "break add");
    let stop = run(&mut s, "call deep(5)");
    assert!(stop.contains("breakpoint #") && stop.contains("add"), "{stop}");
    assert!(stop.contains("stopped inside `deep(5)`") && stop.contains("`kill call`"), "{stop}");
    let bt = run(&mut s, "bt");
    assert!(bt.contains("deep") && bt.contains("return address of `deep(5)`, called by the debugger"), "{bt}");
    assert!(!bt.contains("main"), "{bt}");
    // a second call waits for the first
    let busy = s.exec(&split_words("call add(1, 2)")).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(busy.contains("under way"), "{busy}");
    let done = run(&mut s, "cont");
    assert!(done.contains("`deep(5)` returned 0xb (11)"), "{done}");
    assert_eq!(regs(&mut s), before);

    // `finish` twice: out of add into deep, then out of deep, which is the end of the call
    run(&mut s, "call deep(2)");
    let up = run(&mut s, "finish");
    assert!(up.contains("deep") && up.contains("stopped inside `deep(2)`"), "{up}");
    let done = run(&mut s, "finish");
    assert!(done.contains("`deep(2)` returned 0x5 (5)"), "{done}");
    assert_eq!(regs(&mut s), before);

    run(&mut s, "call deep(1)");
    let killed = run(&mut s, "kill call");
    assert!(killed.contains("`deep(1)` ended without a value") && killed.contains("back as before"), "{killed}");
    assert_eq!(regs(&mut s), before);
    let none = s.exec(&split_words("kill call")).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(none.contains("no call is under way"), "{none}");
    ends_normally(&mut s);
}

#[test]
fn a_call_asks_for_a_stopped_program_and_a_function_in_code() {
    let Some((_d, mut s)) = stopped_in_tick() else { return };
    let not_code = s.exec(&split_words("call counter(1)")).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(not_code.contains("not in executable memory"), "{not_code}");
    let no_parens = s.exec(&split_words("call add")).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(no_parens.contains("calls nothing"), "{no_parens}");
    ends_normally(&mut s);
}
