//! The emulator's floating point and AVX2 forms against small C programs compiled here: where the program
//! can print the answer itself, the test runs it and checks that emu says the same; the vector loops are
//! checked against the same arithmetic done in Rust.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{emulate, EmuSpec};
use crate::Image;

const SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

float K = 1.5f;

__attribute__((noipa)) double poly(int n) { double x = n * 0.5; return x * x * 3.0 - x / 4.0 + 1.5; }
__attribute__((noipa)) double hyp(int a, int b) { return __builtin_sqrt((double)a * a + (double)b * b); }
__attribute__((noipa)) int less(int a, int b) { return (float)a / 3.0f < (float)b / 7.0f; }
__attribute__((noipa)) int notnan(int z) { float n = (float)z / (float)z; return n == n; }
__attribute__((noipa)) int trunc2(int x) { double d = x * 1.7; return (int)d * 1000 + (int)(d + 0.5); }
__attribute__((noipa)) long big(int x) { return (long)(x * 1e12); }
__attribute__((noipa)) double pick(int a, int b) { double x = a, y = b; return (x < y ? x : y) + (x > y ? x : y) * 2; }
__attribute__((noipa)) void show(int n) {
    float s = 0;
    for (int i = 0; i < n; i++) s += i * 0.1f;
    printf("%f %.3f\n", s, (double)s / 3);
}
__attribute__((noipa)) void scale(float *a, int n) { for (int i = 0; i < n; i++) a[i] = a[i] * K + 1.0f; }
__attribute__((noipa)) void addk(int *a, int n, int k) { for (int i = 0; i < n; i++) a[i] += k * 3; }
__attribute__((noipa)) void xorb(unsigned char *b, int n, unsigned char k) { for (int i = 0; i < n; i++) b[i] ^= k; }
__attribute__((noipa)) int sum(const int *a, int n) { int s = 0; for (int i = 0; i < n; i++) s += a[i]; return s; }
__attribute__((noipa)) void widen(double *o, const int *a, int n) { for (int i = 0; i < n; i++) o[i] = a[i] * 0.5; }

int main(int argc, char **argv) {
    if (argc < 2) return 1;
    int a = argc > 2 ? atoi(argv[2]) : 0, b = argc > 3 ? atoi(argv[3]) : 0;
    if (!strcmp(argv[1], "poly")) printf("%.17g\n", poly(a));
    if (!strcmp(argv[1], "hyp")) printf("%.17g\n", hyp(a, b));
    if (!strcmp(argv[1], "less")) printf("%d\n", less(a, b));
    if (!strcmp(argv[1], "notnan")) printf("%d\n", notnan(a));
    if (!strcmp(argv[1], "trunc2")) printf("%d\n", trunc2(a));
    if (!strcmp(argv[1], "big")) printf("%ld\n", big(a));
    if (!strcmp(argv[1], "pick")) printf("%.17g\n", pick(a, b));
    if (!strcmp(argv[1], "show")) show(a);
    return 0;
}
"#;

/// Compile the fixture with `flags`; None (with a message) when the compiler is missing or refuses a flag.
fn build(dir: &Path, tag: &str, flags: &[&str]) -> Option<PathBuf> {
    let src = dir.join("fp.c");
    std::fs::write(&src, SOURCE).ok()?;
    let exe = dir.join(format!("fp{tag}"));
    let ok = ["cc", "gcc"].iter().any(|cc| {
        Command::new(cc)
            .args(flags)
            .args(["-fcf-protection=full", "-fno-math-errno", "-o"])
            .arg(&exe)
            .arg(&src)
            .arg("-lm")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    if !ok || !exe.is_file() {
        eprintln!("skipped: no C compiler that builds the float fixture ({flags:?}) as an ELF file here");
        return None;
    }
    Some(exe)
}

fn real(exe: &Path, args: &[&str]) -> String {
    let o = Command::new(exe).args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn run(img: &Image, target: &str, args: &[&str], edit: impl FnOnce(&mut EmuSpec)) -> (String, serde_json::Value) {
    let mut spec = EmuSpec::new(target);
    spec.args = args.iter().map(|s| s.to_string()).collect();
    edit(&mut spec);
    let out = emulate(img, &spec).unwrap();
    let page = out.page(None, 400);
    (out.text_all(), page.value)
}

fn returned(v: &serde_json::Value) -> u64 {
    assert_eq!(v["stop"]["kind"], "returned", "{v:#}");
    v["return"].as_u64().unwrap()
}

/// The double in xmm0 after a return (the answer line prints it only when it is not zero).
fn xmm0(text: &str) -> f64 {
    match text.split("xmm0=0x").nth(1) {
        Some(rest) => {
            let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            f64::from_bits(u128::from_str_radix(&hex, 16).unwrap() as u64)
        }
        None => 0.0,
    }
}

fn int_hex(vals: &[i32]) -> String {
    let b: Vec<String> = vals.iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect();
    format!("hex:{}", b.join(""))
}

/// The first `n` bytes of `want` appear in the written-memory dump, in the 16-byte groups it prints.
fn shows(text: &str, want: &[u8], n: usize) -> bool {
    want.iter().take(n).collect::<Vec<_>>().chunks(16).all(|c| {
        let s: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
        text.contains(&s.join(" "))
    })
}

/// Does `name` hold an instruction for which `pred` is true (to know the compiler really used AVX2 there)?
fn uses(img: &Image, name: &str, pred: impl Fn(&iced_x86::Instruction) -> bool) -> bool {
    let f = crate::ops::resolve(img, name).unwrap();
    let mut found = false;
    crate::disasm::walk(img.read_at(f.start, f.size() as usize).unwrap(), f.start, |ins, _| found |= pred(ins));
    found
}

const SCALAR_OPTS: [&str; 3] = ["-O0", "-O1", "-O2"];

#[test]
fn emu_computes_double_and_single_arithmetic_like_the_real_program() {
    let dir = tempfile::tempdir().unwrap();
    for opt in SCALAR_OPTS {
        let Some(exe) = build(dir.path(), opt, &[opt]) else { return };
        let img = Image::open(&exe).unwrap();
        for n in ["0", "3", "-7", "11"] {
            let (text, _) = run(&img, "poly", &[n], |_| {});
            let want: f64 = real(&exe, &["poly", n]).parse().unwrap();
            assert_eq!(xmm0(&text), want, "{opt} poly({n})\n{text}");
        }
        for (a, b) in [("3", "4"), ("5", "12"), ("1", "1")] {
            let (text, v) = run(&img, "hyp", &[a, b], |_| {});
            if v["stop"]["kind"] == "unknown_import" {
                eprintln!("skipped hyp: the compiler called sqrt instead of using sqrtsd");
                continue;
            }
            let want: f64 = real(&exe, &["hyp", a, b]).parse().unwrap();
            assert_eq!(xmm0(&text), want, "{opt} hyp({a},{b})\n{text}");
        }
        for (a, b) in [("3", "4"), ("9", "4"), ("0", "0"), ("-2", "1")] {
            let (text, v) = run(&img, "pick", &[a, b], |_| {});
            returned(&v);
            let want: f64 = real(&exe, &["pick", a, b]).parse().unwrap();
            assert_eq!(xmm0(&text), want, "{opt} pick({a},{b})\n{text}");
            let (text, v) = run(&img, "less", &[a, b], |_| {});
            let want: u64 = real(&exe, &["less", a, b]).parse().unwrap();
            assert_eq!(returned(&v) & 0xff, want, "{opt} less({a},{b})\n{text}");
        }
    }
}

#[test]
fn emu_follows_nan_compares_and_float_to_integer_conversions() {
    let dir = tempfile::tempdir().unwrap();
    for opt in SCALAR_OPTS {
        let Some(exe) = build(dir.path(), opt, &[opt]) else { return };
        let img = Image::open(&exe).unwrap();
        for z in ["0", "5"] {
            let (text, v) = run(&img, "notnan", &[z], |_| {});
            let want: u64 = real(&exe, &["notnan", z]).parse().unwrap();
            assert_eq!(returned(&v) & 0xff, want, "{opt} notnan({z})\n{text}");
        }
        for x in ["0", "1", "7", "-3", "100"] {
            let (text, v) = run(&img, "trunc2", &[x], |_| {});
            let want: i32 = real(&exe, &["trunc2", x]).parse().unwrap();
            assert_eq!(returned(&v) as u32 as i32, want, "{opt} trunc2({x})\n{text}");
        }
        for x in ["0", "3", "-9"] {
            let (text, v) = run(&img, "big", &[x], |_| {});
            let want: i64 = real(&exe, &["big", x]).parse().unwrap();
            assert_eq!(returned(&v) as i64, want, "{opt} big({x})\n{text}");
        }
    }
}

#[test]
fn emu_prints_the_single_precision_sum_through_printf_as_the_real_program_does() {
    let dir = tempfile::tempdir().unwrap();
    for opt in SCALAR_OPTS {
        let Some(exe) = build(dir.path(), opt, &[opt]) else { return };
        let img = Image::open(&exe).unwrap();
        for n in ["0", "5", "40"] {
            let (text, v) = run(&img, "show", &[n], |_| {});
            returned(&v);
            assert_eq!(v["stdout"].as_str().unwrap().trim(), real(&exe, &["show", n]), "{opt} show({n})\n{text}");
        }
    }
}

#[test]
fn emu_runs_a_vectorized_float_loop_with_packed_single_instructions() {
    let dir = tempfile::tempdir().unwrap();
    // -O3 vectorizes the loop with mulps/addps (SSE2 only)
    let Some(exe) = build(dir.path(), "-O3", &["-O3"]) else { return };
    let img = Image::open(&exe).unwrap();
    assert!(uses(&img, "scale", |i| i.mnemonic() == iced_x86::Mnemonic::Mulps), "the loop is vectorized at -O3");
    let vals: Vec<f32> = (0..37).map(|i| i as f32 * 0.37 - 4.0).collect();
    let hex: Vec<String> = vals.iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect();
    let (text, v) = run(&img, "scale", &[&format!("hex:{}", hex.join("")), "37"], |s| s.dump = 64);
    returned(&v);
    let want: Vec<u8> = vals.iter().flat_map(|v| (v * 1.5f32 + 1.0f32).to_le_bytes()).collect();
    assert!(shows(&text, &want, 64), "{text}");
}

#[test]
fn emu_runs_avx2_integer_loops_on_ymm_registers() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-avx2", &["-O3", "-mavx2"]) else { return };
    let img = Image::open(&exe).unwrap();
    let ymm = |i: &iced_x86::Instruction| {
        (0..i.op_count()).any(|o| i.op_kind(o) == iced_x86::OpKind::Register && i.op_register(o).is_ymm())
    };
    for f in ["addk", "xorb", "sum", "widen"] {
        if !uses(&img, f, ymm) {
            eprintln!("note: this compiler did not use ymm registers in {f}");
        }
    }
    assert!(uses(&img, "addk", ymm), "the loop uses ymm registers when built with -mavx2");
    // 77 ints: eight full ymm steps and a tail
    let ints: Vec<i32> = (0..77).map(|i| i * 31 - 500).collect();
    let (text, v) = run(&img, "addk", &[&int_hex(&ints), "77", "5"], |s| s.dump = 64);
    returned(&v);
    let want: Vec<u8> = ints.iter().flat_map(|x| (x + 15).to_le_bytes()).collect();
    assert!(shows(&text, &want, 64), "{text}");
    // bytes
    let plain: Vec<u8> = (0..101u32).map(|i| (i * 37 + 11) as u8).collect();
    let hex: Vec<String> = plain.iter().map(|b| format!("{b:02x}")).collect();
    let (text, v) = run(&img, "xorb", &[&format!("hex:{}", hex.join("")), "101", "0x5c"], |s| s.dump = 64);
    returned(&v);
    let want: Vec<u8> = plain.iter().map(|b| b ^ 0x5c).collect();
    assert!(shows(&text, &want, 64), "{text}");
    // a reduction (horizontal adds through vextracti128, vpsrldq and vmovd)
    let (text, v) = run(&img, "sum", &[&int_hex(&ints), "77"], |_| {});
    let total = ints.iter().fold(0i32, |a, b| a.wrapping_add(*b));
    assert_eq!(returned(&v) as u32 as i32, total, "{text}");
    // ints to doubles
    let (text, v) = run(&img, "widen", &["buf:640", &int_hex(&ints), "77"], |s| s.dump = 64);
    returned(&v);
    let want: Vec<u8> = ints.iter().flat_map(|x| (*x as f64 * 0.5).to_le_bytes()).collect();
    assert!(shows(&text, &want, 64), "{text}");
}

#[test]
fn emu_runs_avx2_float_loops_too() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-avx2f", &["-O3", "-mavx2"]) else { return };
    let img = Image::open(&exe).unwrap();
    let vals: Vec<f32> = (0..45).map(|i| i as f32 * 0.37 - 4.0).collect();
    let hex: Vec<String> = vals.iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect();
    let (text, v) = run(&img, "scale", &[&format!("hex:{}", hex.join("")), "45"], |s| s.dump = 64);
    returned(&v);
    let want: Vec<u8> = vals.iter().flat_map(|v| (v * 1.5f32 + 1.0f32).to_le_bytes()).collect();
    assert!(shows(&text, &want, 64), "{text}");
}
