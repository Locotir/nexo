#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A 32-bit program (`gcc -m32`) under the 64-bit debugger: its code read as 32-bit code, its registers as eax ..
//! eip, its frames and the arguments its callers pushed on the stack. Skipped when gcc cannot build 32-bit programs
//! (no multilib C library).

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <stdio.h>
#include <string.h>
__attribute__((noinline)) int add(int a, int b) { int s = a + b; return s * 2; }
__attribute__((noinline)) int helper(const char *t, int n) {
    int l = (int)strlen(t);
    return add(n, 3) + l;
}
int main(int argc, char **argv) {
    (void)argv;
    int r = helper("hello", argc + 4);
    printf("%d\n", r);
    return 0;
}
"#;

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn refused(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).map(|o| o.text_all()).expect_err(cmd).to_string()
}

/// The program built 32-bit with `flags`, started and stopped at its entry, with what the debugger said there.
fn start(test: &str, flags: &[&str]) -> Option<(Session, String, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("p.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.path().join("p");
    let built = std::process::Command::new("gcc")
        .arg("-m32")
        .args(flags)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .output()
        .is_ok_and(|o| o.status.success());
    if !built {
        eprintln!("SKIPPED {test}: gcc -m32 cannot build a 32-bit program here, so nothing was checked");
        return None;
    }
    let (s, first) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    Some((s, first.text_all(), dir))
}

#[test]
fn a_32_bit_program_is_debugged_as_32_bit() {
    let Some((mut s, first, _d)) = start("a_32_bit_program_is_debugged_as_32_bit", &["-g", "-O0"]) else { return };
    // the entry read as 32-bit code (as 64-bit code these bytes do not read `xor ebp, ebp` then `pop esi`)
    assert!(first.contains("=> xor ebp, ebp") && first.contains("pop esi"), "{first}");

    let t = run(&mut s, "break add");
    assert!(t.contains("add+0x"), "{t}");
    let t = run(&mut s, "cont");
    assert!(t.contains("breakpoint #1") && t.contains("[ebp+8]"), "{t}");

    let t = run(&mut s, "regs");
    for r in ["eax ", "ebx ", "ecx ", "edx ", "esi ", "edi ", "ebp ", "esp ", "eip ", "eflags "] {
        assert!(t.contains(r), "{r} missing:\n{t}");
    }
    for r in ["rax", "rip", "r8 ", "r15"] {
        assert!(!t.contains(r), "{r} shown:\n{t}");
    }
    assert!(t.contains("[esp] "), "{t}");

    let t = run(&mut s, "args");
    assert!(t.contains("a (int) = 5") && t.contains("b (int) = 3"), "{t}");
    assert!(!t.contains("rdi") && !t.contains("arg1"), "{t}");

    // the frames up to the program's entry, with nothing past it
    let t = run(&mut s, "bt");
    let names: Vec<&str> = t.lines().filter_map(|l| l.split_whitespace().nth(2)).collect();
    let at = |n: &str| names.iter().position(|x| x.starts_with(n)).unwrap_or_else(|| panic!("{n} missing:\n{t}"));
    assert!(at("add+") < at("helper+") && at("helper+") < at("main+"), "{t}");
    assert!(names.last().is_some_and(|n| n.starts_with("_start")), "{t}");
    assert_eq!(names.len(), t.lines().filter(|l| l.starts_with('#')).count(), "a frame without a place:\n{t}");

    let t = run(&mut s, "dis");
    assert!(t.contains("mov eax, [ebp+0xc]") && t.contains("leave") && !t.contains("rbp"), "{t}");

    // the caller's frame: its parameters from DWARF, and the registers the call may have changed shown as such
    let t = run(&mut s, "up");
    assert!(t.contains("helper+") && t.contains("n = 5"), "{t}");
    let t = run(&mut s, "regs");
    assert!(t.contains("eax    <not saved>") && t.contains("ebp    0x"), "{t}");
    run(&mut s, "down");

    let t = run(&mut s, "info frame");
    assert!(t.contains("eip 0x") && t.contains("esp 0x") && t.contains("the caller's esp"), "{t}");

    // add(5, 3) is (5 + 3) * 2
    let t = run(&mut s, "finish");
    assert!(t.contains("helper+") && t.contains("eax 0x") && t.contains("-> 0x10"), "{t}");
    assert!(t.contains("n = 5") && t.contains("l = 5"), "{t}");
    let t = run(&mut s, "next");
    assert!(t.contains("helper+"), "{t}");
    let t = run(&mut s, "step");
    assert!(t.contains("helper+"), "{t}");
    let t = run(&mut s, "history");
    assert!(t.contains("eax 0x10") && !t.contains("rax"), "{t}");

    let e = refused(&mut s, "strace");
    assert!(e.contains("32-bit") && e.contains("i386"), "{e}");
    let e = refused(&mut s, "call add(1,2)");
    assert!(e.contains("was not run") && e.contains("32-bit"), "{e}");

    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0") && t.contains("21"), "{t}");
}

#[test]
fn the_pushed_arguments_are_read_at_a_function_entry_hooks_and_the_library_trace() {
    let Some((mut s, _, _d)) =
        start("the_pushed_arguments_are_read_at_a_function_entry_hooks_and_the_library_trace", &["-g", "-O0"])
    else {
        return;
    };
    run(&mut s, "break *helper");
    // before the prologue: the parameters where the caller pushed them
    let t = run(&mut s, "cont");
    assert!(t.contains("parameters on the stack: t = 0x") && t.contains("\"hello\", n = 5"), "{t}");
    let t = run(&mut s, "args");
    assert!(t.contains("\"hello\" (on the stack, where the caller pushed it)"), "{t}");
    assert!(t.contains("n (int) = 5 (on the stack"), "{t}");

    run(&mut s, "hook add");
    run(&mut s, "ltrace strlen");
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("add(a=5, b=3)") && t.contains("= 16"), "{t}");
    let t = run(&mut s, "ltrace show");
    assert!(t.contains("strlen(\"hello\") = 5"), "{t}");
}

#[test]
fn without_variable_info_the_pushed_words_are_shown() {
    let Some((mut s, _, _d)) = start("without_variable_info_the_pushed_words_are_shown", &["-O0"]) else { return };
    run(&mut s, "break add");
    let t = run(&mut s, "cont");
    assert!(t.contains("add"), "{t}");
    let t = run(&mut s, "args");
    assert!(t.contains("arg1 [0x") && t.contains("= 0x5 (5)"), "{t}");
    assert!(t.contains("arg2 [0x") && t.contains("= 0x3 (3)"), "{t}");
    assert!(t.contains("how many are arguments is not known"), "{t}");
    let t = run(&mut s, "bt");
    assert!(t.contains("helper") && t.contains("main"), "{t}");
}

#[test]
fn a_hook_changes_a_pushed_argument_and_the_returned_value() {
    let Some((mut s, _, _d)) = start("a_hook_changes_a_pushed_argument_and_the_returned_value", &["-g", "-O0"]) else {
        return;
    };
    run(&mut s, "hook add set arg1 10 ret 100");
    // add(5, 10) is 30; the hook makes it give 100, and helper adds strlen("hello")
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0") && t.contains("105"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("add(a=5, b=3)") && t.contains("arg1=0xa"), "{t}");
    assert!(t.contains("= 30") && t.contains("changed to 100"), "{t}");
}
