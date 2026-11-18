//! Backward slices on AArch64: small images from instruction words written by hand.

use std::path::PathBuf;

use super::fixture::{self, adrp, bl, ldr_w, mov_w, LDP_FRAME, RET, STP_FRAME};
use crate::*;

fn movz(rd: u32, imm: u32, hw: u32) -> u32 {
    0xd280_0000 | (hw << 21) | (imm << 5) | rd
}

fn movk(rd: u32, imm: u32, hw: u32) -> u32 {
    0xf280_0000 | (hw << 21) | (imm << 5) | rd
}

/// `add xd, xn, xm`
fn add_reg(rd: u32, rn: u32, rm: u32) -> u32 {
    0x8b00_0000 | (rm << 16) | (rn << 5) | rd
}

/// `mov xd, xm`
fn mov_reg(rd: u32, rm: u32) -> u32 {
    0xaa00_03e0 | (rm << 16) | rd
}

fn ldr_x(rt: u32, rn: u32, off: u32) -> u32 {
    0xf940_0000 | ((off / 8) << 10) | (rn << 5) | rt
}

fn str_x(rt: u32, rn: u32, off: u32) -> u32 {
    0xf900_0000 | ((off / 8) << 10) | (rn << 5) | rt
}

/// `sub sp, sp, #n`
fn sub_sp(n: u32) -> u32 {
    0xd100_03ff | (n << 10)
}

/// The extra function G of the fixture (nothing about it depends on its words): its start, helper, C, the global.
fn places() -> (u64, u64, u64, u64) {
    let f = fixture::elf(true);
    (f.g, f.b, f.c, f.global)
}

fn image(extra: &[u32]) -> Image {
    let f = fixture::elf_with(true, extra);
    Image::from_bytes(f.bytes, PathBuf::from("/fx/prog")).expect("the fixture opens")
}

fn hex(a: u64) -> String {
    format!("{a:#x}")
}

fn pad(a: u64) -> String {
    format!("{a:#010x}")
}

fn run(img: &Image, at: u64, reg: Option<&str>, arg: Option<usize>, mem: Option<&str>) -> Result<String> {
    slice_window(img, &hex(at), reg, arg, mem, None, 200).map(|o| o.page(None, 1000).text())
}

#[test]
fn a_call_argument_is_followed_back_through_movz_movk_and_an_add() {
    let (g, b, ..) = places();
    let words = [movz(0, 0x1234, 0), movk(0, 0x5678, 1), mov_w(1, 7), add_reg(0, 0, 1), bl(g + 16, b), RET];
    let img = image(&words);
    let t = run(&img, g + 16, None, Some(1), None).unwrap();
    assert!(t.contains("arguments of the call to helper (x0)"), "{t}");
    assert!(t.contains(&format!("{}  add x0, x0, x1", pad(g + 12))), "{t}");
    assert!(t.contains("defines x0; needs x0, x1"), "{t}");
    assert!(t.contains(&pad(g + 8)) && t.contains("sets x1 to a constant"), "{t}");
    assert!(t.contains(&format!("{}  movk x0", pad(g + 4))), "{t}");
    assert!(t.contains(&pad(g)) && t.contains("sets x0 to a constant"), "{t}");
    assert!(t.contains("stops: none"), "{t}");
    // without --arg every argument register set since the previous call is followed
    let all = run(&img, g + 16, None, None, None).unwrap();
    assert!(all.contains("(x0, x1)"), "{all}");
}

#[test]
fn a_global_loaded_through_adrp_and_ldr_is_one_place() {
    let (g, b, _, global) = places();
    let img = image(&[adrp(8, g, global), ldr_w(0, 8, (global & 0xfff) as u32), bl(g + 8, b), RET]);
    let t = run(&img, g + 8, Some("x0"), None, None).unwrap();
    assert!(t.contains(&format!("{}  ldr w0, [x8,", pad(g + 4))), "{t}");
    assert!(t.contains(&format!("needs global {global:#x}")), "{t}");
    assert!(t.contains("is not written in"), "{t}");
    assert!(!t.contains("adrp"), "the address register is not followed: {t}");
}

#[test]
fn a_w_write_feeds_an_x_read_and_the_register_name_may_be_w() {
    let (g, b, ..) = places();
    let img = image(&[mov_w(0, 5), mov_reg(1, 0), bl(g + 8, b), RET]);
    let t = run(&img, g + 8, Some("w1"), None, None).unwrap();
    assert!(t.contains("slice of x1 (w1 is its low half)"), "{t}");
    assert!(t.contains(&format!("{}  mov x1, x0", pad(g + 4))) && t.contains("needs x0"), "{t}");
    assert!(t.contains(&format!("{}  mov w0, #0x5", pad(g))) && t.contains("sets x0 to a constant"), "{t}");
    assert!(t.contains("stops: none"), "{t}");
}

#[test]
fn a_stack_slot_follows_sp_through_a_push_and_a_pop() {
    let (g, b, ..) = places();
    let words = [sub_sp(32), mov_w(9, 9), str_x(9, 31, 8), STP_FRAME, ldr_x(0, 31, 24), LDP_FRAME, bl(g + 24, b), RET];
    let img = image(&words);
    let t = run(&img, g + 24, Some("x0"), None, None).unwrap();
    assert!(t.contains(&format!("{}  ldr x0, [sp", pad(g + 16))), "{t}");
    assert!(t.contains("defines x0; needs [sp+0x18]"), "{t}");
    assert!(t.contains("defines [sp+0x8]; needs x9"), "{t}");
    assert!(t.contains(&format!("{}  mov w9, #0x9", pad(g + 4))), "{t}");
    assert!(t.contains("[inferred]"), "an sp slot is inferred: {t}");
    // the same slot, asked for by hand just before the pop
    let m = run(&img, g + 20, None, None, Some("sp+0x18")).unwrap();
    assert!(m.contains("defines [sp+0x8]; needs x9"), "{m}");
}

#[test]
fn call_results_and_clobbers_stop_the_walk() {
    let (g, b, c, _) = places();
    let img = image(&[bl(g, b), mov_reg(1, 0), movz(2, 3, 0), bl(g + 12, c), RET, 0]);
    let t = run(&img, g + 12, Some("x1"), None, None).unwrap();
    assert!(t.contains("x0 is the result of the call to helper"), "{t}");
    let t = run(&img, g + 12, Some("x20"), None, None).unwrap();
    assert!(t.contains("x20 is not written in"), "callee-saved registers pass through calls: {t}");
    let t = run(&img, g + 12, Some("x3"), None, None).unwrap();
    assert!(t.contains("x3 is changed by the call to helper"), "{t}");
}

#[test]
fn what_is_not_followed_is_refused_or_said() {
    let (g, b, ..) = places();
    let img = image(&[mov_reg(1, 0), bl(g + 4, b), RET, 0]);
    let err = |reg: Option<&str>, arg: Option<usize>, mem: Option<&str>| {
        run(&img, g + 4, reg, arg, mem).unwrap_err().to_string()
    };
    assert!(err(Some("sp"), None, None).contains("stack pointer"));
    assert!(err(Some("xzr"), None, None).contains("zero register"));
    assert!(err(Some("rax"), None, None).contains("not a register name"));
    assert!(err(None, Some(9), None).contains("x0..x7"));
    assert!(err(None, None, Some("rbp-8")).contains("not a stack slot"));
    // an argument register that is not set before the call comes from the caller
    let t = run(&img, g + 4, None, Some(1), None).unwrap();
    assert!(t.contains("x0 is argument 1 of"), "{t}");
    // --forward says it is not done on AArch64
    let e = slice_forward(&img, &hex(g), None, None, None, 50).unwrap_err().to_string();
    assert!(e.contains("slice --forward does not read AArch64"), "{e}");
}
