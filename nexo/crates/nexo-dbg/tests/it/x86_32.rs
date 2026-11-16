#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A 32-bit program (`gcc -m32`) under the 64-bit debugger: its code read as 32-bit code, its registers as eax ..
//! eip, its frames and the arguments its callers pushed on the stack. Skipped when gcc cannot build 32-bit programs
//! (no multilib C library).

use std::io::{Read, Write};
use std::path::PathBuf;

use nexo_dbg::{open, serve_gdb, split_words, Kind, Session};

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

/// `src` built 32-bit with `flags`.
fn build(test: &str, src: &str, flags: &[&str]) -> Option<(PathBuf, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path().join("p.c");
    std::fs::write(&c, src).unwrap();
    let exe = dir.path().join("p");
    let built = std::process::Command::new("gcc")
        .arg("-m32")
        .args(flags)
        .arg("-o")
        .arg(&exe)
        .arg(&c)
        .output()
        .is_ok_and(|o| o.status.success());
    if !built {
        eprintln!("SKIPPED {test}: gcc -m32 cannot build a 32-bit program here, so nothing was checked");
        return None;
    }
    Some((exe, dir))
}

/// The program built 32-bit with `flags`, started and stopped at its entry, with what the debugger said there.
fn start(test: &str, flags: &[&str]) -> Option<(Session, String, tempfile::TempDir)> {
    let (exe, dir) = build(test, SRC, flags)?;
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
    // the pushed words read through the program's own 4-byte pointers: `t` alone, then the text it points to
    let t = run(&mut s, "print *(char*)*(char**)($esp+4)");
    assert!(t.contains("(104)"), "{t}");
    let t = run(&mut s, "print *(int*)($esp+8) * 10 + sizeof(long) + sizeof(char*)");
    assert!(t.contains("(58)"), "{t}");

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

const TABLE: &str = r#"
#include <stdio.h>
__attribute__((noinline)) int add_one(int x) { return x + 1; }
__attribute__((noinline)) int double_it(int x) { return x * 2; }
int (*volatile table[2])(int) = { add_one, double_it };
int main(void) {
    int v = 1;
    for (int i = 0; i < 4; i++) v = table[i & 1](v);
    printf("%d\n", v);
    return 0;
}
"#;

#[test]
fn the_calls_through_a_pointer_of_a_32_bit_program_are_logged_by_name() {
    let test = "the_calls_through_a_pointer_of_a_32_bit_program_are_logged_by_name";
    let Some((exe, _d)) = build(test, TABLE, &["-g", "-O0"]) else { return };
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let on = run(&mut s, "calls");
    assert!(on.starts_with("logging ") && on.contains(" indirect calls"), "{on}");
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0") && t.contains("10"), "{t}");
    let log = run(&mut s, "log");
    let names: Vec<&str> = log
        .lines()
        .filter(|l| l.contains(" main+"))
        .map(|l| {
            if l.contains("add_one") {
                "add_one"
            } else if l.contains("double_it") {
                "double_it"
            } else {
                "?"
            }
        })
        .collect();
    assert_eq!(names, ["add_one", "double_it", "add_one", "double_it"], "{log}");
}

/// A raw GDB remote client: one packet out, the stub's reply back.
struct Wire(std::net::TcpStream);

impl Wire {
    fn ask(&mut self, body: &str) -> String {
        let sum = body.bytes().fold(0u8, u8::wrapping_add);
        write!(self.0, "${body}#{sum:02x}").unwrap();
        let mut b = [0u8; 1];
        loop {
            self.0.read_exact(&mut b).expect("the stub answers");
            if b[0] == b'$' {
                break;
            }
        }
        let mut out = Vec::new();
        loop {
            self.0.read_exact(&mut b).unwrap();
            match b[0] {
                b'#' => break,
                b'}' => {
                    self.0.read_exact(&mut b).unwrap();
                    out.push(b[0] ^ 0x20);
                }
                c => out.push(c),
            }
        }
        let mut sum = [0u8; 2];
        self.0.read_exact(&mut sum).unwrap();
        self.0.write_all(b"+").unwrap();
        String::from_utf8(out).unwrap()
    }
}

/// nexo's stub on a thread, serving `exe`; the address it listens on.
fn serve(exe: PathBuf) -> (String, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        let r = serve_gdb(&Kind::Run { exe, args: vec![] }, 0, &mut |a| {
            let _ = tx.send(a.to_string());
        });
        assert!(r.is_ok(), "{r:?}");
    });
    (rx.recv_timeout(std::time::Duration::from_secs(20)).expect("the stub did not come up"), h)
}

#[test]
fn the_stub_serves_a_32_bit_program_as_i386() {
    let test = "the_stub_serves_a_32_bit_program_as_i386";
    let src = "void _start(void) { __asm__ volatile(\"movl $1, %eax\\n movl $7, %ebx\\n int $0x80\"); }\n";
    let Some((exe, _d)) = build(test, src, &["-nostdlib", "-static", "-fno-pie", "-no-pie"]) else { return };
    let elf = std::fs::read(&exe).unwrap();
    let entry = u32::from_le_bytes(elf[0x18..0x1c].try_into().unwrap());

    // nexo's own client reads x86-64 only, and is told the program's real architecture
    let (addr, h) = serve(exe.clone());
    let e = open(&Kind::Remote { addr, exe: None }).map(|_| ()).expect_err("an i386 remote").to_string();
    assert!(e.contains("i386") && e.contains("x86-64 remotes only"), "{e}");
    h.join().unwrap();

    let (addr, h) = serve(exe);
    let mut w = Wire(std::net::TcpStream::connect(&addr).unwrap());
    w.0.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    let xml = w.ask("qXfer:features:read:target.xml:0,fff");
    assert!(xml.contains("<architecture>i386</architecture>"), "{xml}");
    assert!(xml.contains("name=\"eax\" bitsize=\"32\"") && !xml.contains("rax"), "{xml}");
    // eax ecx edx ebx esp ebp esi edi eip eflags and six segments, eight x87 registers, eight x87 words, orig_eax
    let g = w.ask("g");
    assert_eq!(g.len(), 2 * (16 * 4 + 8 * 10 + 8 * 4 + 4), "{g}");
    let word = |i: usize| u32::from_str_radix(&g[i * 8..i * 8 + 8], 16).map(u32::swap_bytes).unwrap();
    assert_eq!(word(8), entry, "eip is the ninth word: {g}");
    assert_eq!(word(10), 0x23, "cs is the 32-bit code segment: {g}");
    assert_eq!(w.ask("p8"), format!("{:08x}", entry.swap_bytes()));
    // a register written alone leaves the others as they were
    assert_eq!(w.ask("P3=2a000000"), "OK");
    assert_eq!(w.ask("p3"), "2a000000");
    assert_eq!(w.ask("p8"), format!("{:08x}", entry.swap_bytes()));
    // the program runs on from there: exit(7)
    assert_eq!(w.ask("c"), "W07");
    drop(w);
    h.join().unwrap();
}
