#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Arguments whose place depends on what their type is, not on how it is spelled: a typedef of `double` in an xmm
//! register, structs by value in the registers of their eightbytes or in memory, a struct result through the hidden
//! pointer in rdi (the first stack slot in a 32-bit program), an `__int128` in two registers. Every argument after
//! one of them is read, and set, where the call put it. Skipped when gcc is missing; the 32-bit test also when gcc
//! cannot build 32-bit programs.

use std::path::PathBuf;

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <stdio.h>
typedef double real;
typedef unsigned int count_t;
struct pair { double d; int i; };
struct big { long a, b, c; };
struct fl2 { float x, y; };
typedef struct { int x; double y; } pt_t;
__attribute__((noinline)) double scale(real a, int b) { return a * b; }
__attribute__((noinline)) long take(struct pair p, int n, struct big g, double z, count_t k) {
    return (long)(p.d * 10) + p.i + n + g.a + g.c + (long)z + k;
}
__attribute__((noinline)) struct big make(int a, real b) { struct big g = {a, (long)b, 3}; return g; }
__attribute__((noinline)) unsigned __int128 wide(int a, __int128 w, int b) { return w + a + b; }
__attribute__((noinline)) struct fl2 swap(struct fl2 v, int k) { struct fl2 r = {v.y + k, v.x}; return r; }
__attribute__((noinline)) int px(pt_t p, int k) { return p.x + k; }
int main(int argc, char **argv) {
    (void)argv;
    double s = scale(1.5, argc + 3);
    struct pair p = {2.5, 7};
    struct big g = {100, 200, 300};
    long t = take(p, 5, g, 4.75, 9);
    struct big m = make(11, 22.5);
    unsigned __int128 w = wide(1, ((__int128)1 << 64) + 5, 2);
    struct fl2 v = swap((struct fl2){1.5f, 2.5f}, 1);
    int q = px((pt_t){4, 0.5}, 3);
    printf("%.1f %ld %ld %ld %ld %.1f %d\n", s, t, m.a, m.b, (long)(w >> 64), v.x, q);
    return 0;
}
"#;

const SRC32: &str = r#"
#include <stdio.h>
typedef double real;
struct pair { double d; int i; };
struct big { long a, b, c; };
__attribute__((noinline)) double scale(real a, int b) { return a * b; }
__attribute__((noinline)) long take(struct pair p, int n) { return (long)(p.d * 10) + p.i + n; }
__attribute__((noinline)) struct big make(int a, real b) { struct big g = {a, (long)b, 3}; return g; }
int main(int argc, char **argv) {
    (void)argv;
    double s = scale(1.5, argc + 3);
    struct pair p = {2.5, 7};
    long t = take(p, 5);
    struct big m = make(11, 22.5);
    printf("%.1f %ld %ld %ld\n", s, t, m.a, m.b);
    return 0;
}
"#;

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// `src` built with gcc and `flags`, started and stopped at its entry.
fn start(test: &str, src: &str, flags: &[&str]) -> Option<(Session, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path().join("p.c");
    std::fs::write(&c, src).unwrap();
    let exe: PathBuf = dir.path().join("p");
    let built = std::process::Command::new("gcc")
        .args(["-g", "-O0", "-no-pie"])
        .args(flags)
        .arg("-o")
        .arg(&exe)
        .arg(&c)
        .output()
        .is_ok_and(|o| o.status.success());
    if !built {
        eprintln!("SKIPPED {test}: gcc {} cannot build the program here, so nothing was checked", flags.join(" "));
        return None;
    }
    let (s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    Some((s, dir))
}

#[test]
fn typedefs_structs_by_value_hidden_results_and_int128_put_every_later_argument_in_its_place() {
    let test = "typedefs_structs_by_value_hidden_results_and_int128_put_every_later_argument_in_its_place";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    for f in ["scale", "take", "make", "wide", "swap", "px"] {
        run(&mut s, &format!("hook {f}"));
    }
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0"), "{t}");
    let t = run(&mut s, "hook show");
    // `real` is a double: in xmm0, and b in edi
    assert!(t.contains("scale(a=1.5, b=4)") && t.contains("= 6.0"), "{t}");
    // the pair in xmm0 and rdi, n in rsi, the big struct on the stack, z in xmm1, the unsigned typedef in rdx
    assert!(
        t.contains("take(p={d=2.5, i=7}, n=5, g={a=100, b=200, c=300}, z=4.75, k=9)") && t.contains("= 450"),
        "{t}"
    );
    // rdi holds where the result goes, so a is in rsi; the result is read where rax points
    assert!(t.contains("make(a=11, b=22.5)") && t.contains("= {a=11, b=22, c=3}"), "{t}");
    // the __int128 takes rsi and rdx, b is in rcx; the result comes back in rax and rdx
    assert!(t.contains("wide(a=1, w=18446744073709551621, b=2)") && t.contains("= 18446744073709551624"), "{t}");
    // two floats share one xmm register, as argument and as result
    assert!(t.contains("swap(v={x=1.5, y=2.5}, k=1)") && t.contains("= {x=3.5, y=1.5}"), "{t}");
    // a typedef of an unnamed struct goes by its layout too: an int and a double, in rdi and xmm0, then k in esi
    assert!(t.contains("px(p={x=4, y=0.5}, k=3)") && t.contains("= 7"), "{t}");
    let o = run(&mut s, "out");
    assert!(o.contains("6.0 450 11 22 1 3.5 7"), "{o}");
}

#[test]
fn a_hook_sets_the_arguments_after_a_struct_or_a_hidden_result_pointer_and_refuses_a_struct() {
    let test = "a_hook_sets_the_arguments_after_a_struct_or_a_hidden_result_pointer_and_refuses_a_struct";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    run(&mut s, "hook scale set arg0 2.5");
    run(&mut s, "hook take set arg1 6 set arg3 5.5");
    run(&mut s, "hook make set arg1 33.5");
    let e = s.exec(&split_words("hook take set arg0 1")).unwrap_err().to_string();
    assert!(e.contains("struct pair passed by value (16 bytes)"), "{e}");
    let e = s.exec(&split_words("hook make ret 1")).unwrap_err().to_string();
    assert!(e.contains("returns a struct big"), "{e}");
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("[changed arg0=2.5]") && t.contains("= 10.0"), "{t}");
    // 25 + 7 + 6 + 100 + 300 + 5 + 9
    assert!(t.contains("[changed arg1=0x6 arg3=5.5]") && t.contains("= 452"), "{t}");
    assert!(t.contains("[changed arg1=33.5]") && t.contains("= {a=11, b=33, c=3}"), "{t}");
    let o = run(&mut s, "out");
    assert!(o.contains("10.0 452 11 33 1 3.5"), "{o}");
}

#[test]
fn a_32_bit_call_reads_a_typedef_double_and_a_struct_from_their_slots_past_the_hidden_result_pointer() {
    let test = "a_32_bit_call_reads_a_typedef_double_and_a_struct_from_their_slots_past_the_hidden_result_pointer";
    let Some((mut s, _d)) = start(test, SRC32, &["-m32"]) else { return };
    for f in ["scale", "take"] {
        run(&mut s, &format!("hook {f}"));
    }
    run(&mut s, "hook make set arg1 33.5");
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("scale(a=1.5, b=4)") && t.contains("= 6.0"), "{t}");
    assert!(t.contains("take(p={d=2.5, i=7}, n=5)") && t.contains("= 37"), "{t}");
    // the callee pops the hidden pointer, and its return is still seen
    assert!(t.contains("make(a=11, b=22.5) [changed arg1=33.5]") && t.contains("= {a=11, b=33, c=3}"), "{t}");
    assert!(!t.contains("was not seen"), "{t}");
    let o = run(&mut s, "out");
    assert!(o.contains("6.0 37 11 33"), "{o}");
}
