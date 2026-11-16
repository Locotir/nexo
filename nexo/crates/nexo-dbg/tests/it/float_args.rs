#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! Arguments past the integer registers: doubles and floats in xmm registers, the 7th integer and the 9th double on
//! the stack, an x87 `long double` in memory and in st0, and the `...` of printf read off its format string. Also
//! the xmm registers themselves (`regs xmm0 --as f32`, `setreg xmm0 2.5`). Skipped when gcc is missing; the 32-bit
//! test also when gcc cannot build 32-bit programs.

use std::path::PathBuf;

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <stdio.h>
__attribute__((noinline)) double mix(double a, float b, int c) { return a * b + c; }
__attribute__((noinline)) long sum9(long a1, int a2, int a3, int a4, int a5, int a6, int a7, int a8, int a9) {
    return a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9;
}
__attribute__((noinline)) double nine(double d0, double d1, double d2, double d3, double d4, double d5, double d6,
                                      double d7, double d8) {
    return d0 + d1 * 0 + d2 * 0 + d3 * 0 + d4 * 0 + d5 * 0 + d6 * 0 + d7 * 0 + d8;
}
__attribute__((noinline)) long double half(long double x) { return x / 2; }
int main(int argc, char **argv) {
    (void)argv;
    double m = mix(1.5, 2.25f, argc + 2);
    long s = sum9(1, 2, 3, 4, 5, 6, 70, 80, 90);
    double n = nine(0.5, 1, 2, 3, 4, 5, 6, 7, 8.25);
    long double h = half(5.0L);
    printf("%d %.3f %s %c %lx %p %5.2f %Lf\n", 42, m, "word", 'Z', 0xbeefL, (void *)0, n, h);
    return (int)(s & 0x7f);
}
"#;

const SRC32: &str = r#"
#include <stdio.h>
__attribute__((noinline)) double mix(double a, float b, int c) { return a * b + c; }
int main(void) {
    double m = mix(1.5, 2.25f, 3);
    printf("%d %.3f %s %lld\n", 7, m, "w", 1LL << 40);
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
fn doubles_floats_and_stack_arguments_are_read_at_a_hook_with_the_float_result() {
    let test = "doubles_floats_and_stack_arguments_are_read_at_a_hook_with_the_float_result";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    for f in ["mix", "sum9", "nine", "half"] {
        run(&mut s, &format!("hook {f}"));
    }
    let t = run(&mut s, "cont");
    // 1 + 2 + 3 + 4 + 5 + 6 + 70 + 80 + 90 = 261, and 261 & 0x7f = 5
    assert!(t.contains("exited with code 5"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("mix(a=1.5, b=2.25, c=3)") && t.contains("= 6.375"), "{t}");
    assert!(t.contains("sum9(a1=1, a2=2, a3=3, a4=4, a5=5, a6=6, a7=70, a8=80, a9=90)") && t.contains("= 261"), "{t}");
    assert!(t.contains("nine(d0=0.5, d1=1.0,") && t.contains("d8=8.25)") && t.contains("= 8.75"), "{t}");
    assert!(t.contains("half(x=5.0)") && t.contains("= 2.5"), "{t}");
    assert!(!t.contains("not read"), "{t}");
}

#[test]
fn a_hook_sets_float_and_stack_arguments_and_a_float_result() {
    let test = "a_hook_sets_float_and_stack_arguments_and_a_float_result";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    let h = run(&mut s, "hook mix set arg0 2.5 ret 100.5");
    assert!(h.contains("changes the program"), "{h}");
    run(&mut s, "hook sum9 set arg8 10");
    run(&mut s, "hook half ret 1.25");
    let l = run(&mut s, "hooks");
    assert!(l.contains("set arg0 2.5, ret 100.5") && l.contains("set arg8 0xa"), "{l}");
    let e = s.exec(&split_words("hook mix ret lots")).unwrap_err().to_string();
    assert!(e.contains("takes a float"), "{e}");
    let t = run(&mut s, "cont");
    // sum9 got 10 for 90: 181, and 181 & 0x7f = 53
    assert!(t.contains("exited with code 53"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("mix(a=1.5, b=2.25, c=3) [changed arg0=2.5]"), "{t}");
    // 2.5 * 2.25 + 3: the new argument reached the function, then the result was replaced
    assert!(t.contains("= 8.625, changed to 100.5"), "{t}");
    assert!(t.contains("[changed arg8=0xa]") && t.contains("= 181"), "{t}");
    assert!(t.contains("half(x=5.0)") && t.contains("= 2.5, changed to 1.25"), "{t}");
    let o = run(&mut s, "out");
    assert!(o.contains("42 100.500 word") && o.contains(" 1.250000"), "{o}");
}

#[test]
fn ltrace_reads_the_printf_arguments_its_format_string_names() {
    let test = "ltrace_reads_the_printf_arguments_its_format_string_names";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    run(&mut s, "ltrace printf");
    run(&mut s, "cont");
    let t = run(&mut s, "ltrace show");
    assert!(
        t.contains(r#"printf("%d %.3f %s %c %lx %p %5.2f %Lf\n", 42, 6.375, "word", 'Z', 0xbeef, NULL, 8.75, 2.5) = "#),
        "{t}"
    );
}

#[test]
fn regs_shows_and_setreg_changes_an_xmm_register_by_lanes() {
    let test = "regs_shows_and_setreg_changes_an_xmm_register_by_lanes";
    let Some((mut s, _d)) = start(test, SRC, &[]) else { return };
    run(&mut s, "break *mix");
    run(&mut s, "cont");
    let t = run(&mut s, "regs xmm0");
    assert!(t.contains("xmm0 = 0x") && t.contains("f64x2 [1.5, ") && t.contains("f32x4 ["), "{t}");
    let t = run(&mut s, "regs $xmm1 --as f32");
    assert!(t.contains("f32x4 [2.25, ") && !t.contains("f64x2"), "{t}");
    let t = run(&mut s, "regs xmm0 --as u32x4");
    assert!(t.contains("u32x4 [0x0, 0x3ff80000, "), "{t}");
    assert!(run(&mut s, "regs --all").contains("xmm15"));
    assert!(run(&mut s, "regs st0").contains("st0 = "));
    let e = s.exec(&split_words("regs xmm0 --as f128")).unwrap_err().to_string();
    assert!(e.contains("f64|f32|u8x16"), "{e}");
    let t = run(&mut s, "setreg xmm0 2.5");
    assert!(t.contains("f64x2 [2.5, "), "{t}");
    assert!(run(&mut s, "regs xmm0").contains("f64x2 [2.5, "));
    let e = s.exec(&split_words("setreg xmm0 1 2 3")).unwrap_err().to_string();
    assert!(e.contains("holds 2 f64 lanes"), "{e}");
    run(&mut s, "cont");
    // 2.5 * 2.25 + 3
    assert!(run(&mut s, "out").contains("42 8.625 word"));
}

#[test]
fn a_32_bit_program_passes_floats_on_the_stack_and_returns_them_in_st0() {
    let test = "a_32_bit_program_passes_floats_on_the_stack_and_returns_them_in_st0";
    let Some((mut s, _d)) = start(test, SRC32, &["-m32"]) else { return };
    run(&mut s, "hook mix ret 1.5");
    run(&mut s, "ltrace printf");
    let t = run(&mut s, "cont");
    assert!(t.contains("exited with code 0"), "{t}");
    let t = run(&mut s, "hook show");
    assert!(t.contains("mix(a=1.5, b=2.25, c=3)") && t.contains("= 6.375, changed to 1.5"), "{t}");
    let t = run(&mut s, "ltrace show");
    assert!(t.contains(r#"printf("%d %.3f %s %lld\n", 7, 1.5, "w", 1099511627776) = "#), "{t}");
    assert!(run(&mut s, "out").contains("7 1.500 w 1099511627776"));
}
