//! Forward slices on AArch64: small images from instruction words written by hand.

use super::fixture::{adrp, bl, mov_w, str_w, RET};
use super::tests_slice::{add_reg, hex, image, ldr_x, mov_reg, pad, places, str_x, sub_sp};
use crate::*;

/// `cmp x0, #0`
const CMP_X0: u32 = 0xf100_001f;
/// `b.eq` two words on.
const BEQ_PLUS_8: u32 = 0x5400_0040;

fn run(img: &Image, at: u64, reg: Option<&str>, mem: Option<&str>) -> Result<String> {
    slice_forward(img, &hex(at), reg, mem, None, 200).map(|o| o.page(None, 1000).text())
}

#[test]
fn a_value_flows_through_add_and_mov_into_a_call_argument() {
    let (g, b, ..) = places();
    let words = [mov_w(0, 5), mov_w(1, 7), add_reg(2, 0, 1), mov_reg(0, 2), bl(g + 16, b), RET];
    let img = image(&words);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains(&format!("forward slice of x0 written at {}", hex(g))), "{t}");
    assert!(t.contains(&format!("{}  add x2, x0, x1", pad(g + 8))), "{t}");
    assert!(t.contains("; x0 feeds x2 [proven]"), "{t}");
    assert!(t.contains("; x2 is copied to x0 [proven]"), "{t}");
    assert!(t.contains("x0 is argument 1 of the call to helper"), "{t}");
    assert!(t.contains("x2 is argument 3 of the call to helper"), "{t}");
    // the call loses both
    assert!(t.contains("x0 is changed by the call to helper"), "{t}");
    assert!(t.contains("; 4 steps, 2 stops"), "{t}");
    // x1 holds another value and is never reported
    assert!(!t.contains("x1 is"), "{t}");
}

#[test]
fn a_stack_slot_keeps_the_value_when_sp_moves() {
    let (g, ..) = places();
    let words = [mov_w(9, 9), str_x(9, 31, 8), sub_sp(32), ldr_x(0, 31, 40), RET];
    let img = image(&words);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains(&format!("{}  str x9, [sp, #8]", pad(g + 4))), "{t}");
    assert!(t.contains("x9 is stored to [sp+0x8] [inferred]"), "{t}");
    // sub sp, sp, #32 moves the slot from sp+8 to sp+40
    assert!(t.contains(&format!("{}  ldr x0, [sp, #40]", pad(g + 12))), "{t}");
    assert!(t.contains("[sp+0x28] is copied to x0 [inferred]"), "{t}");
    // asked for by hand, as it stands after the store
    let m = run(&img, g + 4, None, Some("sp+8")).unwrap();
    assert!(m.contains("forward slice of [sp+0x8] after"), "{m}");
    assert!(m.contains("[sp+0x28] is copied to x0"), "{m}");
}

#[test]
fn x0_at_ret_is_returned() {
    let (g, ..) = places();
    let img = image(&[mov_w(1, 3), mov_reg(0, 1), RET]);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains("x1 is copied to x0"), "{t}");
    assert!(t.contains("; x0 is returned from"), "{t}");
    assert!(t.contains("  returned from"), "{t}");
    // a value that is not in x0 at the ret is not reported as returned
    let img = image(&[mov_w(1, 3), RET]);
    let t = run(&img, g, None, None).unwrap();
    assert!(!t.contains("is returned from"), "{t}");
    assert!(t.contains("steps: none"), "{t}");
}

#[test]
fn a_call_loses_the_registers_it_changes_and_keeps_the_others() {
    let (g, b, ..) = places();
    let img = image(&[mov_w(3, 1), mov_w(20, 2), bl(g + 8, b), mov_reg(1, 3), mov_reg(2, 20), RET]);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains("x3 is argument 4 of the call to helper"), "{t}");
    assert!(t.contains("x3 is changed by the call to helper"), "{t}");
    assert!(!t.contains(&pad(g + 12)), "nothing follows the call: {t}");
    let t = run(&img, g + 4, Some("x20"), None).unwrap();
    assert!(t.contains("forward slice of x20 after"), "{t}");
    assert!(t.contains(&format!("{}  mov x2, x20", pad(g + 16))), "{t}");
    assert!(t.contains("x20 is copied to x2"), "{t}");
    assert!(!t.contains("changed by the call"), "{t}");
    // the result of a call is x0
    let t = run(&img, g + 8, None, None).unwrap();
    assert!(t.contains("the result (x0) of the call to helper"), "{t}");
}

#[test]
fn a_store_to_a_global_through_adrp_is_followed_to_the_next_call() {
    let (g, b, _, global) = places();
    let lo = (global & 0xfff) as u32;
    let img = image(&[mov_w(9, 7), adrp(8, g + 4, global), str_w(9, 8, lo), bl(g + 12, b), RET]);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains("str w9, [x8, #32]"), "{t}");
    assert!(t.contains("x9 is stored to global"), "{t}");
    assert!(t.contains(&format!("{global:#x}")), "{t}");
    assert!(t.contains("may be changed by the call to helper"), "{t}");
    assert!(t.contains("a global is visible to every function"), "{t}");
}

#[test]
fn both_sides_of_a_branch_are_followed() {
    let (g, ..) = places();
    let img = image(&[mov_w(0, 5), CMP_X0, BEQ_PLUS_8, mov_w(0, 1), RET]);
    let t = run(&img, g, None, None).unwrap();
    assert!(t.contains(&format!("x0 is compared; b.eq {} decides on it [proven]", hex(g + 16))), "{t}");
    // the branch-not-taken side overwrites x0, the taken side returns it
    assert!(t.contains(&format!("{}  x0 is overwritten", pad(g + 12))), "{t}");
    assert!(t.contains("; x0 is returned from"), "{t}");
    assert!(t.contains("[inferred]"), "{t}");
}

#[test]
fn what_is_not_followed_is_refused() {
    let (g, ..) = places();
    let img = image(&[mov_reg(1, 0), RET, 0]);
    let err = |at: u64, reg: Option<&str>, mem: Option<&str>| run(&img, at, reg, mem).unwrap_err().to_string();
    assert!(err(g, Some("sp"), None).contains("stack pointer"));
    assert!(err(g, Some("xzr"), None).contains("zero register"));
    assert!(err(g, Some("rax"), None).contains("not a register name"));
    assert!(err(g, None, Some("rbp-8")).contains("not a stack slot"));
    assert!(err(g + 4, None, None).contains("writes no register or stack slot"));
    assert!(err(g + 2, None, None).contains("inside an instruction"));
    // the command line the page prints
    let o = slice_forward(&img, &hex(g), Some("x1"), None, None, 50).unwrap();
    assert!(o.command.contains("--reg x1 --forward"), "{}", o.command);
}
