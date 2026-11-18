//! The arm64 lifter on hand-assembled words (no toolchain): what the IR holds, and what the printer says.

use super::{lift, Abi, Env, Lifted};
use crate::decomp::flow::{Sig, Sigs};
use crate::decomp::ir::*;
use crate::decomp::render::Names;

const BASE: u64 = 0x1000;

const ADD_W0_W0_W1: u32 = 0x0b01_0000;
const RET: u32 = 0xd65f_03c0;
const STP_FP_LR_PRE: u32 = 0xa9bf_7bfd; // stp x29, x30, [sp, #-16]!
const MOV_FP_SP: u32 = 0x9100_03fd; // mov x29, sp
const LDP_FP_LR_POST: u32 = 0xa8c1_7bfd; // ldp x29, x30, [sp], #16
const MOV_W0_1: u32 = 0x5280_0020;
const MOV_W0_2: u32 = 0x5280_0040;
const MOV_W1_2: u32 = 0x5280_0041;
const CMP_W0_W1: u32 = 0x6b01_001f;
const CMN_W0_W1: u32 = 0x2b01_001f;
const ADDS_W0_W0_W1: u32 = 0x2b01_0000;
const TST_W0_1: u32 = 0x7200_001f;
const ANDS_W0_W0_1: u32 = 0x7200_0000;
const CSET_W0_HS: u32 = 0x1a9f_37e0;
const CSET_W0_LO: u32 = 0x1a9f_27e0;
const CSET_W0_HI: u32 = 0x1a9f_97e0;
const BGE_PLUS_12: u32 = 0x5400_006a;
const CBNZ_W0_MINUS_4: u32 = 0x35ff_ffe0;
const SUB_W0_W0_1: u32 = 0x5100_0400;
const ADRP_X8_PLUS_1_PAGE: u32 = 0xb000_0008;
const LDR_X8_X8_16: u32 = 0xf940_0908;
const LDR_W0_X8_16: u32 = 0xb940_1100;
const BL_PLUS_48: u32 = 0x9400_000c;
const BLR_X8: u32 = 0xd63f_0100;
const ADD_V0_4S: u32 = 0x4ea1_8400; // add v0.4s, v0.4s, v1.4s

/// Words at 0x1000. 0x1040 is the function `puts`, the pointer slot at 0x2010 is import 0, the word at 0x2010 a
/// global.
struct E {
    code: Vec<u8>,
}

fn env(words: &[u32]) -> E {
    E { code: words.iter().flat_map(|w| w.to_le_bytes()).collect() }
}

impl Env for E {
    fn read(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let o = addr.checked_sub(BASE)? as usize;
        self.code.get(o..).map(|s| &s[..len.min(s.len())])
    }
    fn name_of(&self, addr: u64) -> Option<String> {
        (addr == 0x1040).then(|| "puts".to_string())
    }
    fn import_slot(&self, slot: u64) -> Option<usize> {
        (slot == 0x2010).then_some(0)
    }
    fn import_name(&self, _: usize) -> String {
        "imp0".to_string()
    }
    fn image_base(&self) -> u64 {
        BASE
    }
    fn is_pe(&self) -> bool {
        false
    }
}

/// Every call takes two arguments.
struct Two;
impl Sigs for Two {
    fn sig_of(&mut self, _: &Target) -> Option<Sig> {
        Some(Sig { args: 2, ..Sig::default() })
    }
    fn cstr(&self, _: u64) -> Option<String> {
        None
    }
}

struct N;
impl Names for N {
    fn func(&self, a: u64) -> Option<String> {
        (a == 0x1040).then(|| "puts".to_string())
    }
    fn import(&self, i: usize) -> String {
        format!("imp{i}")
    }
    fn string(&self, _: u64) -> Option<String> {
        None
    }
    fn global(&self, a: u64) -> Option<String> {
        (a == 0x2010).then(|| "g_val".to_string())
    }
}

fn lifted(words: &[u32]) -> Lifted {
    let e = env(words);
    lift(&e, BASE, BASE + 4 * words.len() as u64, Abi::Aapcs64)
}

fn dump(f: &Lifted) -> String {
    let mut s = String::new();
    for (i, b) in f.blocks.iter().enumerate() {
        s += &format!("B{i}:\n");
        for st in &b.stmts {
            s += &format!("  {:?}\n", st.st);
        }
        s += &format!("  {:?}\n", b.term);
    }
    s
}

fn text(words: &[u32]) -> String {
    let e = env(words);
    let l = crate::decomp::decompile(&e, BASE, BASE + 4 * words.len() as u64, Abi::Aapcs64, &mut Two, &N, "f");
    l.iter().map(|x| format!("{}{}\n", "    ".repeat(x.indent), x.text)).collect()
}

#[test]
fn a_32_bit_add_returns_the_sum() {
    // add w0,w0,w1 ; ret
    let t = text(&[ADD_W0_W0_W1, RET]);
    println!("{t}");
    // The x86 `add edi,esi ; mov eax,edi` prints the same shape: a zero-extended 32-bit sum of the two arguments.
    assert!(t.contains("return") && t.contains("(u32)a1 + (u32)a2"), "{t}");
    assert!(!t.contains("not lifted"), "{t}");
}

#[test]
fn a_frame_around_a_call_vanishes_and_the_call_keeps_its_arguments() {
    // stp x29,x30,[sp,#-16]! ; mov x29,sp ; mov w0,#1 ; mov w1,#2 ; bl puts ; ldp x29,x30,[sp],#16 ; ret
    let w = [STP_FP_LR_PRE, MOV_FP_SP, MOV_W0_1, MOV_W1_2, BL_PLUS_48, LDP_FP_LR_POST, RET];
    let f = lifted(&w);
    let d = dump(&f);
    println!("{d}");
    assert!(d.contains("Direct(4160)"), "{d}");
    assert_eq!(f.raw, 0, "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("puts(1, 2)"), "{t}");
    assert!(!t.contains("x29") && !t.contains("x30") && !t.contains("sp"), "{t}");
    assert!(!t.contains("not lifted"), "{t}");
}

#[test]
fn a_call_through_a_loaded_import_slot_is_an_import_call() {
    // adrp x8,0x2000 ; ldr x8,[x8,#16] ; mov w0,#1 ; mov w1,#2 ; blr x8 ; ret
    let w = [ADRP_X8_PLUS_1_PAGE, LDR_X8_X8_16, MOV_W0_1, MOV_W1_2, BLR_X8, RET];
    let d = dump(&lifted(&w));
    println!("{d}");
    assert!(d.contains("Import(0)"), "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("imp0(1, 2)"), "{t}");
}

#[test]
fn compare_and_branch_makes_an_if() {
    // cmp w0,w1 ; b.ge L ; mov w0,#1 ; ret ; L: mov w0,#2 ; ret
    let w = [CMP_W0_W1, BGE_PLUS_12, MOV_W0_1, RET, MOV_W0_2, RET];
    let f = lifted(&w);
    let d = dump(&f);
    println!("{d}");
    assert_eq!(f.blocks.len(), 3, "{d}");
    assert!(d.contains("Flags(Sub"), "{d}");
    // ge is the x86 `ge`, number 13
    assert!(d.contains("Branch(Cond(Cc(13)"), "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("if (") && (t.contains(" < ") || t.contains(" >= ")), "{t}");
    assert!(t.contains("return 1") && t.contains("return 2"), "{t}");
}

#[test]
fn a_loop_on_cbnz() {
    // L: sub w0,w0,#1 ; cbnz w0,L ; ret
    let w = [SUB_W0_W0_1, CBNZ_W0_MINUS_4, RET];
    let f = lifted(&w);
    let d = dump(&f);
    println!("{d}");
    assert_eq!(f.blocks.len(), 2, "{d}");
    assert!(d.contains("Branch(Cmp(Ne"), "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("while"), "{t}");
}

#[test]
fn adrp_and_ldr_read_a_global() {
    // adrp x8,0x2000 ; ldr w0,[x8,#16] ; ret
    let w = [ADRP_X8_PLUS_1_PAGE, LDR_W0_X8_16, RET];
    let d = dump(&lifted(&w));
    println!("{d}");
    assert!(d.contains("Const(8208, 64)"), "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("g_val"), "{t}");
}

#[test]
fn a_simd_word_is_not_lifted_and_the_rest_is() {
    // mov w0,#1 ; add v0.4s,v0.4s,v1.4s ; ret
    let w = [MOV_W0_1, ADD_V0_4S, RET];
    let f = lifted(&w);
    let d = dump(&f);
    println!("{d}");
    assert_eq!(f.raw, 1, "{d}");
    assert!(d.contains("Raw {"), "{d}");
    let t = text(&w);
    println!("{t}");
    assert!(t.contains("/* not lifted: add"), "{t}");
}

/// The condition of `cset w0, <cc>` after `first`, as the dump shows it.
fn cset_after(first: u32, cset: u32) -> String {
    dump(&lifted(&[first, cset, RET]))
}

#[test]
fn carry_after_a_subtraction_is_not_borrow() {
    // cmp: hs is x86 ae (3), lo is b (2), hi is a (7)
    let d = cset_after(CMP_W0_W1, CSET_W0_HS);
    assert!(d.contains("Flags(Sub") && d.contains("Cc(3)"), "{d}");
    let d = cset_after(CMP_W0_W1, CSET_W0_LO);
    assert!(d.contains("Cc(2)") && !d.contains("Cc(3)"), "{d}");
    let d = cset_after(CMP_W0_W1, CSET_W0_HI);
    assert!(d.contains("Cc(7)"), "{d}");
}

#[test]
fn carry_after_an_add_or_a_logic_op_is_the_carry() {
    // cmn and adds: hs is x86 b (2), lo is ae (3), hi is b and ne (2, 5)
    for first in [CMN_W0_W1, ADDS_W0_W0_W1] {
        let d = cset_after(first, CSET_W0_HS);
        assert!(d.contains("Flags(Add") && d.contains("Cc(2)") && !d.contains("Cc(3)"), "{d}");
        let d = cset_after(first, CSET_W0_LO);
        assert!(d.contains("Cc(3)") && !d.contains("Cc(2)"), "{d}");
        let d = cset_after(first, CSET_W0_HI);
        assert!(d.contains("Cc(2)") && d.contains("Cc(5)"), "{d}");
    }
    // tst and ands: carry clear, so hs is b over a logic result
    for first in [TST_W0_1, ANDS_W0_W0_1] {
        let d = cset_after(first, CSET_W0_HS);
        assert!(d.contains("Flags(Logic") && d.contains("Cc(2)"), "{d}");
    }
}
