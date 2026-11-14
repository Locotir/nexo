#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `args` lists the function's own parameters, not the six argument registers: with DWARF only the parameters (and
//! the registers past them for a variadic function), without it the registers as before. Skipped without `cc`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <stdarg.h>
#include <stdio.h>
__attribute__((noinline)) int add2(int a, char *s) { return a + (int)s[0]; }
__attribute__((noinline)) int vsum(int n, ...) {
    va_list ap; va_start(ap, n); int t = 0;
    for (int i = 0; i < n; i++) t += va_arg(ap, int);
    va_end(ap); return t;
}
int main(void) { return add2(3, "x") + vsum(2, 5, 6) > 0 ? 0 : 1; }
"#;

fn at(flags: &[&str], func: &str) -> Option<(Session, tempfile::TempDir, String)> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("p.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.path().join("p");
    let ok = std::process::Command::new("cc")
        .args(flags)
        .arg("-o")
        .arg(&exe)
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    s.exec(&split_words(&format!("break {func}"))).unwrap();
    s.exec(&split_words("cont")).unwrap();
    let out = s.exec(&split_words("args")).unwrap().text_all();
    Some((s, dir, out))
}

#[test]
fn two_parameters_show_only_those_two() {
    let Some((_s, _d, o)) = at(&["-g", "-O0"], "add2") else { return };
    assert!(o.contains("a (int) = 3"), "{o}");
    assert!(o.contains("s (char *)"), "{o}");
    assert!(!o.contains("arg"), "{o}");
}

#[test]
fn variadic_keeps_registers_after_the_named_ones() {
    let Some((_s, _d, o)) = at(&["-g", "-O0"], "vsum") else { return };
    assert!(o.contains("n (int) = 2"), "{o}");
    assert!(!o.contains("arg1 "), "{o}");
    assert!(o.contains("arg2 rsi = 0x5") && o.contains("(variadic)"), "{o}");
    assert!(o.contains("arg6 r9"), "{o}");
}

#[test]
fn without_debug_info_all_six_registers_stay() {
    let Some((_s, _d, o)) = at(&["-O0", "-no-pie"], "add2") else { return };
    for n in ["arg1 rdi", "arg2 rsi", "arg3 rdx", "arg4 rcx", "arg5 r8", "arg6 r9"] {
        assert!(o.contains(n), "{o}");
    }
}
