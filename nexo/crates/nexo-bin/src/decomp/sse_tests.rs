//! Scalar SSE (float and double) through the whole decompiler: one instruction at a time, then whole functions.
//!
//! Every fixture is a fixed byte sequence. The C source and the compiler are in the comment above it; the compiler is
//! `cc (GCC) 16.2.1 20260810`, `nice -n 19 cc -O2 -fno-math-errno -c`, x86-64 (Win64 variants: the same C with
//! `__attribute__((ms_abi))` and `long long` for `long`). A constant pool follows the function: the code, `cc`
//! padding, then the `.rodata.cst8/16` words, each `rip+disp` re-pointed at its word. Hand-assembled fixtures carry
//! their assembly instead of C.

use super::flow::{Sig, Sigs};
use super::ir::Target;
use super::lift::Abi;
use super::testenv::Raw;

struct NoSigs;
impl Sigs for NoSigs {
    fn sig_of(&mut self, _: &Target) -> Option<Sig> {
        None
    }
    fn cstr(&self, _: u64) -> Option<String> {
        None
    }
}

struct NoNames;
impl super::render::Names for NoNames {
    fn func(&self, _: u64) -> Option<String> {
        None
    }
    fn import(&self, i: usize) -> String {
        format!("imp{i}")
    }
    fn string(&self, _: u64) -> Option<String> {
        None
    }
    fn global(&self, _: u64) -> Option<String> {
        None
    }
}

/// Code at 0x1000; the function is `[0x1000, 0x1000 + end)`; bytes after `end` are its read-only constant pool.
fn fx(code: &[u8], end: u64, abi: Abi) -> String {
    let env = Raw { base: 0x1000, code: code.to_vec() };
    let l = super::decompile(&env, 0x1000, 0x1000 + end, abi, &mut NoSigs, &NoNames, "f");
    l.iter().map(|x| format!("{}{}\n", "    ".repeat(x.indent), x.text)).collect()
}

/// The prototype line.
fn sig(t: &str) -> &str {
    t.lines().next().unwrap_or("")
}

/// `a` appears before `b` in `t`.
fn before(t: &str, a: &str, b: &str) -> bool {
    matches!((t.find(a), t.find(b)), (Some(i), Some(j)) if i < j)
}

/// The function bytes, `cc` padding up to a multiple of `align`, then the constant words (8 bytes each, low first).
fn pool(code: &[u8], align: usize, words: &[u64]) -> Vec<u8> {
    let mut v = code.to_vec();
    while v.len() % align != 0 {
        v.push(0xcc);
    }
    for w in words {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v
}

fn sysv(code: &[u8], end: u64) -> String {
    fx(code, end, Abi::SysV)
}

fn clean(t: &str) {
    assert!(!t.contains("not lifted"), "{t}");
}

// ---------------------------------------------------------------------------------------------
// One instruction at a time (SysV)

#[test]
fn movsd_register_copy_is_a_double_copy() {
    // asm: movsd xmm0, xmm1 ; ret
    let t = sysv(&[0xf2, 0x0f, 0x10, 0xc1, 0xc3], 5);
    clean(&t);
    assert_eq!(sig(&t), "double f(double f2)", "{t}");
    assert!(t.contains("return f2;"), "{t}");
}

#[test]
fn movsd_load_reads_a_double() {
    // double ld(const double *p) { return p[1]; }
    let t = sysv(&[0xf2, 0x0f, 0x10, 0x47, 0x08, 0xc3], 6);
    clean(&t);
    assert!(t.contains("return *(double*)(a1 + 8);"), "{t}");
}

#[test]
fn movsd_store_writes_a_double() {
    // void st(double *p, double v) { p[2] = v; }
    let t = sysv(&[0xf2, 0x0f, 0x11, 0x47, 0x10, 0xc3], 6);
    clean(&t);
    assert!(sig(&t).starts_with("void f("), "{t}");
    assert!(sig(&t).contains("double f1"), "{t}");
    assert!(t.contains("*(double*)(a1 + 0x10) = f1;"), "{t}");
}

#[test]
fn addsd_and_mulsd_are_float_arithmetic() {
    // double madd(double a, double b, double c) { return a * b + c; }
    let t = sysv(&[0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x58, 0xc2, 0xc3], 9);
    clean(&t);
    assert!(t.contains("return f1 * f2 + f3;"), "{t}");
}

#[test]
fn subsd_is_float_subtraction() {
    // double sub_(double a, double b) { return a - b; }
    let t = sysv(&[0xf2, 0x0f, 0x5c, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return f1 - f2;"), "{t}");
}

#[test]
fn divsd_is_float_division() {
    // double div_(double a, double b) { return a / b; }
    let t = sysv(&[0xf2, 0x0f, 0x5e, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return f1 / f2;"), "{t}");
}

#[test]
fn sqrtsd_is_sqrt() {
    // double sq(double a) { return __builtin_sqrt(a); }
    let t = sysv(&[0xf2, 0x0f, 0x51, 0xc0, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return sqrt(f1);"), "{t}");
}

#[test]
fn minsd_is_the_less_than_ternary() {
    // double mn(double a, double b) { return a < b ? a : b; }
    let t = sysv(&[0xf2, 0x0f, 0x5d, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return f1 < f2 ? f1 : f2;"), "{t}");
    assert!(!t.contains("fmin"), "{t}");
}

#[test]
fn maxsd_is_the_greater_than_ternary() {
    // double mx(double a, double b) { return a > b ? a : b; }
    let t = sysv(&[0xf2, 0x0f, 0x5f, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return f1 > f2 ? f1 : f2;"), "{t}");
    assert!(!t.contains("fmax"), "{t}");
}

#[test]
fn pxor_of_a_register_with_itself_is_zero() {
    // double zero(void) { return 0.0; }
    let t = sysv(&[0x66, 0x0f, 0xef, 0xc0, 0xc3], 5);
    clean(&t);
    assert_eq!(sig(&t), "double f(void)", "{t}");
    assert!(t.contains("return 0.0;"), "{t}");
}

#[test]
fn xorpd_of_a_register_with_itself_is_zero() {
    // asm: xorpd xmm0, xmm0 ; ret
    let t = sysv(&[0x66, 0x0f, 0x57, 0xc0, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return 0.0;"), "{t}");
}

#[test]
fn xorpd_with_the_sign_mask_is_negation() {
    // double neg(double a) { return -a; }
    // xorpd xmm0, [rip+8] ; ret ; the mask is at +0x10
    let code = pool(&[0x66, 0x0f, 0x57, 0x05, 0x08, 0, 0, 0, 0xc3], 16, &[0x8000_0000_0000_0000, 0]);
    let t = sysv(&code, 9);
    clean(&t);
    assert!(t.contains("return -f1;"), "{t}");
}

#[test]
fn andpd_with_the_abs_mask_is_fabs() {
    // double absd(double a) { return fabs(a); }
    // andpd xmm0, [rip+8] ; ret ; the mask is at +0x10
    let code = pool(&[0x66, 0x0f, 0x54, 0x05, 0x08, 0, 0, 0, 0xc3], 16, &[0x7fff_ffff_ffff_ffff, 0]);
    let t = sysv(&code, 9);
    clean(&t);
    assert!(t.contains("return fabs(f1);"), "{t}");
}

#[test]
fn andpd_with_a_register_is_a_bit_operation() {
    // asm: andpd xmm0, xmm1 ; ret
    let t = sysv(&[0x66, 0x0f, 0x54, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return as_float(as_int(f1) & as_int(f2));"), "{t}");
}

#[test]
fn andnpd_with_a_register_is_a_bit_operation() {
    // asm: andnpd xmm0, xmm1 ; ret
    let t = sysv(&[0x66, 0x0f, 0x55, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return as_float(~as_int(f1) & as_int(f2));"), "{t}");
}

#[test]
fn xorpd_with_a_register_is_a_bit_operation() {
    // asm: xorpd xmm0, xmm1 ; ret
    let t = sysv(&[0x66, 0x0f, 0x57, 0xc1, 0xc3], 5);
    clean(&t);
    assert!(t.contains("return as_float(as_int(f1) ^ as_int(f2));"), "{t}");
}

#[test]
fn cvtsi2sd_and_cvttsd2si_of_64_bits() {
    // long l2d(long n) { return (long)((double)n / 3.0); }
    // pxor xmm0,xmm0 ; cvtsi2sd xmm0,rdi ; divsd xmm0,[rip+7] ; cvttsd2si rax,xmm0 ; ret ; 3.0 at +0x18
    let code = pool(
        &[
            0x66, 0x0f, 0xef, 0xc0, 0xf2, 0x48, 0x0f, 0x2a, 0xc7, 0xf2, 0x0f, 0x5e, 0x05, 0x07, 0, 0, 0, 0xf2, 0x48,
            0x0f, 0x2c, 0xc0, 0xc3,
        ],
        8,
        &[0x4008_0000_0000_0000],
    );
    let t = sysv(&code, 0x17);
    clean(&t);
    assert!(t.contains("return (i64)((double)a1 / 3.0);"), "{t}");
}

#[test]
fn cvtsd2si_rounds_with_lrint() {
    // long rnd(double a) { return __builtin_lrint(a); }
    let t = sysv(&[0xf2, 0x48, 0x0f, 0x2d, 0xc0, 0xc3], 6);
    clean(&t);
    assert!(t.contains("return lrint(f1);"), "{t}");
    assert!(!t.contains("(i64)f1"), "{t}");
}

#[test]
fn cvtss2sd_widens_a_float() {
    // double f2d(float f) { return f; }
    let t = sysv(&[0xf3, 0x0f, 0x5a, 0xc0, 0xc3], 5);
    clean(&t);
    assert_eq!(sig(&t), "double f(float f1)", "{t}");
    assert!(t.contains("return (double)f1;"), "{t}");
}

#[test]
fn cvtsd2ss_narrows_a_double() {
    // float d2f(double d) { return (float)d; }
    let t = sysv(&[0xf2, 0x0f, 0x5a, 0xc0, 0xc3], 5);
    clean(&t);
    assert_eq!(sig(&t), "float f(double f1)", "{t}");
    assert!(t.contains("return (float)f1;"), "{t}");
}

#[test]
fn comisd_seta_is_ordered_greater() {
    // int lt(double a, double b) { return a < b; }
    let t = sysv(&[0x31, 0xc0, 0x66, 0x0f, 0x2f, 0xc8, 0x0f, 0x97, 0xc0, 0xc3], 10);
    clean(&t);
    assert!(t.contains("return f2 > f1;"), "{t}");
}

#[test]
fn comisd_setae_is_ordered_greater_or_equal() {
    // int le(double a, double b) { return a <= b; }
    let t = sysv(&[0x31, 0xc0, 0x66, 0x0f, 0x2f, 0xc8, 0x0f, 0x93, 0xc0, 0xc3], 10);
    clean(&t);
    assert!(t.contains("return f2 >= f1;"), "{t}");
}

#[test]
fn ucomisd_setnp_cmovne_is_equal() {
    // int eq(double a, double b) { return a == b; }
    let code = [0x31, 0xc0, 0x66, 0x0f, 0x2e, 0xc1, 0xba, 0, 0, 0, 0, 0x0f, 0x9b, 0xc0, 0x0f, 0x45, 0xc2, 0xc3];
    let t = sysv(&code, 18);
    clean(&t);
    assert!(t.contains("return f1 == f2;"), "{t}");
    assert!(!t.contains("isunordered"), "{t}");
}

#[test]
fn ucomisd_setp_cmovne_is_not_equal() {
    // int ne(double a, double b) { return a != b; }
    let code = [0x31, 0xc0, 0x66, 0x0f, 0x2e, 0xc1, 0xba, 1, 0, 0, 0, 0x0f, 0x9a, 0xc0, 0x0f, 0x45, 0xc2, 0xc3];
    let t = sysv(&code, 18);
    clean(&t);
    assert!(t.contains("return f1 != f2;"), "{t}");
    assert!(!t.contains("isunordered"), "{t}");
}

#[test]
fn ucomisd_setp_is_isunordered() {
    // int unord(double a, double b) { return __builtin_isunordered(a, b); }
    let t = sysv(&[0x31, 0xc0, 0x66, 0x0f, 0x2e, 0xc1, 0x0f, 0x9a, 0xc0, 0xc3], 10);
    clean(&t);
    assert!(t.contains("return isunordered(f1, f2);"), "{t}");
}

#[test]
fn ucomisd_jb_is_true_for_nan() {
    // asm: ucomisd xmm0, xmm1 ; jb L ; mov eax, 1 ; ret ; L: xor eax, eax ; ret
    let code = [0x66, 0x0f, 0x2e, 0xc1, 0x72, 0x06, 0xb8, 1, 0, 0, 0, 0xc3, 0x31, 0xc0, 0xc3];
    let t = sysv(&code, 15);
    clean(&t);
    // B holds when the pair is unordered: the text is `!(f1 >= f2)` or an `if (f1 >= f2)` with the arms swapped
    assert!(t.contains("f1 >= f2"), "{t}");
    assert!(!t.contains("f1 < f2"), "{t}");
}

#[test]
fn comisd_cmova_keeps_the_nan_arm() {
    // int notgt(double a, double b, int x) { if (!(a > b)) return x + 1; return x - 1; }
    let code = [0x8d, 0x47, 0x01, 0x83, 0xef, 0x01, 0x66, 0x0f, 0x2f, 0xc1, 0x0f, 0x47, 0xc7, 0xc3];
    let t = sysv(&code, 14);
    clean(&t);
    assert!(t.contains("f1 > f2 ? a1 - 1 : a1 + 1") || t.contains("!(f1 > f2) ? a1 + 1 : a1 - 1"), "{t}");
    assert!(!t.contains("f1 <= f2"), "{t}");
}

#[test]
fn double_constants_print_exactly() {
    // double tenth(double a) { return a + 0.1; }
    let code = pool(&[0xf2, 0x0f, 0x58, 0x05, 0x08, 0, 0, 0, 0xc3], 8, &[0x3fb9_9999_9999_999a]);
    let t = sysv(&code, 9);
    clean(&t);
    assert!(t.contains("return f1 + 0.1;"), "{t}");
}

#[test]
fn infinity_and_nan_are_spelled_out() {
    // double inf_(void) { return INFINITY; }  /  double nan_(void) { return NAN; }
    // movsd xmm0, [rip+8] ; ret ; the word at +0x10
    let head = [0xf2, 0x0f, 0x10, 0x05, 0x08, 0, 0, 0, 0xc3];
    let t = sysv(&pool(&head, 8, &[0x7ff0_0000_0000_0000]), 9);
    clean(&t);
    assert!(t.contains("return INFINITY;"), "{t}");
    let t = sysv(&pool(&head, 8, &[0x7ff8_0000_0000_0000]), 9);
    clean(&t);
    assert!(t.contains("return NAN;"), "{t}");
}

#[test]
fn odd_double_bit_patterns_round_trip() {
    // asm: movsd xmm0, [rip+8] ; ret
    let head = [0xf2, 0x0f, 0x10, 0x05, 0x08, 0, 0, 0, 0xc3];
    let t = sysv(&pool(&head, 8, &[0x8000_0000_0000_0000]), 9);
    clean(&t);
    assert!(t.contains("return -0.0;"), "{t}");
    let t = sysv(&pool(&head, 8, &[0x7ff0_0000_0000_0001]), 9);
    clean(&t);
    assert!(t.contains("return as_float(0x7ff0000000000001);"), "{t}");
}

#[test]
fn addss_is_float_arithmetic_for_free() {
    // float addf(float a, float b) { return a + b; }
    let t = sysv(&[0xf3, 0x0f, 0x58, 0xc1, 0xc3], 5);
    clean(&t);
    assert_eq!(sig(&t), "float f(float f1, float f2)", "{t}");
    assert!(t.contains("return f1 + f2;"), "{t}");
}

#[test]
fn malformed_sse_bytes_do_not_panic() {
    // asm: mulsd with its bytes cut off
    let _ = sysv(&[0xf2, 0x0f, 0x59], 3);
    // asm: andpd xmm0, [rip+0x40000000] ; ret   (the constant is outside the image)
    let t = sysv(&[0x66, 0x0f, 0x54, 0x05, 0x00, 0x00, 0x00, 0x40, 0xc3], 9);
    assert!(!t.contains("fabs"), "{t}");
}

// ---------------------------------------------------------------------------------------------
// Whole functions, SysV and Win64

const MADD: [u8; 9] = [0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x58, 0xc2, 0xc3];

fn check_madd(t: &str) {
    clean(t);
    assert_eq!(sig(t), "double f(double f1, double f2, double f3)", "{t}");
    assert!(t.contains("return f1 * f2 + f3;"), "{t}");
}

#[test]
fn whole_a_times_b_plus_c_sysv() {
    // double madd(double a, double b, double c) { return a * b + c; }
    check_madd(&fx(&MADD, 9, Abi::SysV));
}

#[test]
fn whole_a_times_b_plus_c_win64() {
    // the same C, ms_abi
    check_madd(&fx(&MADD, 9, Abi::Win64));
}

fn polynomial() -> Vec<u8> {
    // double poly(double x) { return ((3.0 * x - 2.0) * x + 0.5) * x + 1.0; }
    let code = [
        0x66, 0x0f, 0x28, 0xc8, 0xf2, 0x0f, 0x10, 0x05, 0x2c, 0, 0, 0, 0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x5c, 0x05,
        0x28, 0, 0, 0, 0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x58, 0x05, 0x24, 0, 0, 0, 0xf2, 0x0f, 0x59, 0xc1, 0xf2,
        0x0f, 0x58, 0x05, 0x20, 0, 0, 0, 0xc3,
    ];
    // 3.0, 2.0, 0.5, 1.0
    pool(&code, 8, &[0x4008_0000_0000_0000, 0x4000_0000_0000_0000, 0x3fe0_0000_0000_0000, 0x3ff0_0000_0000_0000])
}

fn check_polynomial(t: &str) {
    clean(t);
    assert_eq!(sig(t), "double f(double f1)", "{t}");
    assert!(t.contains("return ((3.0 * f1 - 2.0) * f1 + 0.5) * f1 + 1.0;"), "{t}");
    assert!(!t.contains("u64") && !t.contains("u128"), "{t}");
}

#[test]
fn whole_polynomial_sysv() {
    check_polynomial(&fx(&polynomial(), 0x31, Abi::SysV));
}

#[test]
fn whole_polynomial_win64() {
    check_polynomial(&fx(&polynomial(), 0x31, Abi::Win64));
}

fn absneg() -> Vec<u8> {
    // double absneg(double a, double b) { return fabs(a) * -b; }
    let code =
        [0x66, 0x0f, 0x54, 0x05, 0x18, 0, 0, 0, 0x66, 0x0f, 0x57, 0x0d, 0x20, 0, 0, 0, 0xf2, 0x0f, 0x59, 0xc1, 0xc3];
    // abs mask at +0x20, sign mask at +0x30
    pool(&code, 16, &[0x7fff_ffff_ffff_ffff, 0, 0x8000_0000_0000_0000, 0])
}

#[test]
fn whole_fabs_and_negation_sysv() {
    let t = fx(&absneg(), 0x15, Abi::SysV);
    clean(&t);
    assert!(t.contains("return fabs(f1) * -f2;"), "{t}");
}

#[test]
fn whole_fabs_and_negation_win64() {
    let t = fx(&absneg(), 0x15, Abi::Win64);
    clean(&t);
    assert!(t.contains("return fabs(f1) * -f2;"), "{t}");
}

/// `if (f1 == f2)` guards the `+ 1` arm, or `if (f1 != f2)` guards the `- 1` arm; never `isunordered`.
fn check_equal_is_false_for_nan(t: &str, k: usize) {
    clean(t);
    let ok = (t.contains("if (f1 == f2)") && t.contains(&format!("a{k} + 1")))
        || (t.contains("if (f1 != f2)") && t.contains(&format!("a{k} - 1")));
    assert!(ok, "{t}");
    assert!(!t.contains("isunordered"), "{t}");
}

#[test]
fn whole_equal_is_false_for_nan_sysv() {
    // int bre(double a, double b, int x) { if (a == b) return x + 1; return x - 1; }
    let code = [
        0x66, 0x0f, 0x2e, 0xc1, 0x7a, 0x02, 0x74, 0x08, 0x8d, 0x47, 0xff, 0xc3, 0x0f, 0x1f, 0x40, 0x00, 0x8d, 0x47,
        0x01, 0xc3,
    ];
    let t = fx(&code, 20, Abi::SysV);
    check_equal_is_false_for_nan(&t, 1);
    assert!(before(sig(&t), "a1", "f1") && before(sig(&t), "f1", "f2"), "{t}");
}

#[test]
fn whole_equal_is_false_for_nan_win64() {
    // the same C, ms_abi
    let code = [
        0x66, 0x0f, 0x2e, 0xc1, 0x7a, 0x02, 0x74, 0x08, 0x41, 0x8d, 0x40, 0xff, 0xc3, 0x0f, 0x1f, 0x00, 0x41, 0x8d,
        0x40, 0x01, 0xc3,
    ];
    let t = fx(&code, 21, Abi::Win64);
    check_equal_is_false_for_nan(&t, 3);
    let s = sig(&t);
    assert!(before(s, "f1", "f2") && before(s, "f2", "a3"), "{t}");
    assert!(!s.contains("a1") && !s.contains("a2"), "{t}");
}

#[test]
fn whole_int_to_double_and_back_sysv() {
    // int i2d2i(int n, double s) { return (int)((double)n * s); }
    let code = [
        0x66, 0x0f, 0x28, 0xc8, 0x66, 0x0f, 0xef, 0xc0, 0xf2, 0x0f, 0x2a, 0xc7, 0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f,
        0x2c, 0xc0, 0xc3,
    ];
    let t = fx(&code, 21, Abi::SysV);
    clean(&t);
    assert!(t.contains("return (i32)((double)a1 * f1);"), "{t}");
}

#[test]
fn whole_int_to_double_and_back_win64() {
    // the same C, ms_abi
    let code = [0x66, 0x0f, 0xef, 0xc0, 0xf2, 0x0f, 0x2a, 0xc1, 0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x2c, 0xc0, 0xc3];
    let t = fx(&code, 17, Abi::Win64);
    clean(&t);
    assert!(t.contains("return (i32)((double)a1 * f2);"), "{t}");
    assert!(before(sig(&t), "a1", "f2"), "{t}");
}

fn check_sum(t: &str) {
    clean(t);
    assert!(sig(t).starts_with("double f("), "{t}");
    assert!(t.contains("0.0"), "{t}");
    assert!(t.contains("*(double*)") || t.contains("((double*)"), "{t}");
    assert!(t.contains("while") || t.contains("do {"), "{t}");
    assert!(!t.contains("u64*"), "{t}");
    for l in t.lines().filter(|l| l.trim_start().starts_with("return")) {
        assert!(l.trim() != "return;", "{t}");
    }
    assert!(!sig(t).starts_with("void"), "{t}");
}

#[test]
fn whole_sum_of_an_array_sysv() {
    // double sum(const double *p, long n) { double s = 0.0; for (long i = 0; i < n; i++) s += p[i]; return s; }
    let code = [
        0x48, 0x85, 0xf6, 0x7e, 0x33, 0x48, 0x8d, 0x04, 0xf7, 0x83, 0xe6, 0x01, 0x66, 0x0f, 0xef, 0xc0, 0x74, 0x0e,
        0xf2, 0x0f, 0x58, 0x07, 0x48, 0x83, 0xc7, 0x08, 0x48, 0x39, 0xc7, 0x74, 0x1e, 0x90, 0xf2, 0x0f, 0x58, 0x07,
        0x48, 0x83, 0xc7, 0x10, 0xf2, 0x0f, 0x58, 0x47, 0xf8, 0x48, 0x39, 0xc7, 0x75, 0xee, 0xc3, 0x0f, 0x1f, 0x44,
        0x00, 0x00, 0x66, 0x0f, 0xef, 0xc0, 0xc3, 0xc3,
    ];
    check_sum(&fx(&code, 0x3e, Abi::SysV));
}

#[test]
fn whole_sum_of_an_array_win64() {
    // the same C, ms_abi, `long long`
    let code = [
        0x48, 0x85, 0xd2, 0x7e, 0x43, 0x48, 0x8d, 0x04, 0xd1, 0x83, 0xe2, 0x01, 0x66, 0x0f, 0xef, 0xc0, 0x74, 0x1e,
        0xf2, 0x0f, 0x58, 0x01, 0x48, 0x83, 0xc1, 0x08, 0x48, 0x39, 0xc1, 0x74, 0x2e, 0x66, 0x0f, 0x1f, 0x44, 0x00,
        0x00, 0x66, 0x66, 0x2e, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf2, 0x0f, 0x58, 0x01, 0x48, 0x83,
        0xc1, 0x10, 0xf2, 0x0f, 0x58, 0x41, 0xf8, 0x48, 0x39, 0xc1, 0x75, 0xee, 0xc3, 0x0f, 0x1f, 0x44, 0x00, 0x00,
        0x66, 0x0f, 0xef, 0xc0, 0xc3, 0xc3,
    ];
    check_sum(&fx(&code, 0x4e, Abi::Win64));
}

#[test]
fn whole_mixed_arguments_keep_their_positions_sysv() {
    // double mixed(int n, double a, long m, double b) { return n * a + m * b; }
    let code = [
        0x66, 0x0f, 0x28, 0xd0, 0x66, 0x0f, 0xef, 0xc0, 0xf2, 0x0f, 0x2a, 0xc7, 0xf2, 0x0f, 0x59, 0xc2, 0x66, 0x0f,
        0xef, 0xd2, 0xf2, 0x48, 0x0f, 0x2a, 0xd6, 0xf2, 0x0f, 0x59, 0xd1, 0xf2, 0x0f, 0x58, 0xc2, 0xc3,
    ];
    let t = fx(&code, 34, Abi::SysV);
    clean(&t);
    assert!(t.contains("return (double)a1 * f1 + (double)a2 * f2;"), "{t}");
    assert!(before(sig(&t), "a2", "f1"), "{t}");
}

#[test]
fn whole_mixed_arguments_keep_their_positions_win64() {
    // the same C, ms_abi, `long long m`
    let code = [
        0x66, 0x0f, 0xef, 0xc0, 0xf2, 0x0f, 0x2a, 0xc1, 0xf2, 0x0f, 0x59, 0xc1, 0x66, 0x0f, 0xef, 0xc9, 0xf2, 0x49,
        0x0f, 0x2a, 0xc8, 0xf2, 0x0f, 0x59, 0xcb, 0xf2, 0x0f, 0x58, 0xc1, 0xc3,
    ];
    let t = fx(&code, 30, Abi::Win64);
    clean(&t);
    assert!(t.contains("return (double)a1 * f2 + (double)a3 * f4;"), "{t}");
    let s = sig(&t);
    assert!(before(s, "a1", "f2") && before(s, "f2", "a3") && before(s, "a3", "f4"), "{t}");
    assert!(!s.contains("a2") && !s.contains("a4"), "{t}");
}
