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
    // Both arguments are only read at 32 bits, so their webs are 32 wide and print without a cast; the w write
    // zero-extends the sum.
    assert!(t.contains("return") && t.contains("a1 + a2"), "{t}");
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
    assert!(!t.contains("x29") && !t.contains("x30"), "{t}");
    for stray in ["sp =", "sp +", "sp -", "(sp", "[sp", " sp)"] {
        assert!(!t.contains(stray), "{stray}: {t}");
    }
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
    assert!(t.contains("/* not lifted: .inst 0x4ea18400"), "{t}");
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

// AArch64 switch jump tables: words from clang and gcc (encoded with llvm-mc), table bytes after the code.

use crate::a64::fixture::{add_imm, adrp, mov_w};

const MOV_W8_W0: u32 = 0x2a00_03e8;
const LDRB_W11_X9_X8: u32 = 0x3868_692b; // ldrb w11, [x9, x8]
const LDRH_W11_X9_X8_LSL1: u32 = 0x7868_792b; // ldrh w11, [x9, x8, lsl #1]
const LDRSW_X11_X9_X8_LSL2: u32 = 0xb8a8_792b; // ldrsw x11, [x9, x8, lsl #2]
const ADD_X10_X10_X11_LSL2: u32 = 0x8b0b_094a; // add x10, x10, x11, lsl #2
const ADD_X10_X10_X11: u32 = 0x8b0b_014a; // add x10, x10, x11
const BR_X10: u32 = 0xd61f_0140;
const BR_X4: u32 = 0xd61f_0080;
const LDRSB_W3_X1_W0_UXTW: u32 = 0x38e0_4823; // ldrsb w3, [x1, w0, uxtw]
const LDRH_W3_X1_W0_UXTW1: u32 = 0x7860_5823; // ldrh w3, [x1, w0, uxtw #1]
const ADD_X4_X2_W3_SXTB2: u32 = 0x8b23_8844; // add x4, x2, w3, sxtb #2
const ADD_X4_X2_W3_SXTH2: u32 = 0x8b23_a844; // add x4, x2, w3, sxth #2
const LDR_X10_X9_X8_LSL3: u32 = 0xf868_792a; // ldr x10, [x9, x8, lsl #3]

fn cmp_w0(n: u32) -> u32 {
    0x7100_001f | (n << 10)
}

fn b_cond(cc: u32, from: u64, to: u64) -> u32 {
    0x5400_0000 | ((((to as i64 - from as i64) >> 2) as u32 & 0x7_ffff) << 5) | cc
}

fn adr(rd: u32, from: u64, to: u64) -> u32 {
    let d = (to as i64 - from as i64) as u32;
    0x1000_0000 | ((d & 3) << 29) | (((d >> 2) & 0x7_ffff) << 5) | rd
}

/// Four cases that return 10..13, then the default that returns 0.
fn bodies() -> Vec<u32> {
    let mut w = Vec::new();
    for i in 0..4 {
        w.extend([mov_w(0, 10 + i), RET]);
    }
    w.extend([mov_w(0, 0), RET]);
    w
}

fn with_table(mut w: Vec<u32>, table: &[u8]) -> Vec<u32> {
    for c in table.chunks(4) {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        w.push(u32::from_le_bytes(b));
    }
    w
}

/// Which table shape clang used.
#[derive(Clone, Copy)]
enum Clang {
    Byte,
    Half,
    Word,
}

/// clang: `cmp w0,#3 ; b.hi def ; mov w8,w0 ; adrp ; add ; adr ; ldr* ; add ; br x10`, then the cases.
fn clang(at: u64, kind: Clang) -> Vec<u32> {
    let table = at + 0x4c;
    let (load, add, base) = match kind {
        Clang::Byte => (LDRB_W11_X9_X8, ADD_X10_X10_X11_LSL2, at + 0x24),
        Clang::Half => (LDRH_W11_X9_X8_LSL1, ADD_X10_X10_X11_LSL2, at + 0x24),
        // the base is the `adr` itself
        Clang::Word => (LDRSW_X11_X9_X8_LSL2, ADD_X10_X10_X11, at + 0x14),
    };
    let mut w = vec![
        cmp_w0(3),
        b_cond(8, at + 4, at + 0x44),
        MOV_W8_W0,
        adrp(9, at + 0xc, table),
        add_imm(9, 9, (table & 0xfff) as u32),
        adr(10, at + 0x14, base),
        load,
        add,
        BR_X10,
    ];
    w.extend(bodies());
    let case = |i: u64| at + 0x24 + 8 * i;
    let mut t: Vec<u8> = Vec::new();
    for i in 0..4 {
        let off = case(i) - base;
        match kind {
            Clang::Byte => t.push((off / 4) as u8),
            Clang::Half => t.extend(((off / 4) as u16).to_le_bytes()),
            Clang::Word => t.extend((off as u32).to_le_bytes()),
        }
    }
    with_table(w, &t)
}

/// gcc: `cmp w0,#3 ; b.hi def ; adrp ; add ; ldr w3,[x1,w0,uxtw] ; adr x2,.Lrtx ; add x4,x2,w3,ext #2 ; br x4`. The
/// base is case 2, so the entries of cases 0 and 1 are negative.
fn gcc(at: u64, load: u32, add: u32, wide: bool) -> Vec<u32> {
    let table = at + 0x48;
    let mut w = vec![
        cmp_w0(3),
        b_cond(8, at + 4, at + 0x40),
        adrp(1, at + 8, table),
        add_imm(1, 1, (table & 0xfff) as u32),
        load,
        adr(2, at + 0x14, at + 0x30),
        add,
        BR_X4,
    ];
    w.extend(bodies());
    let mut t: Vec<u8> = Vec::new();
    for i in 0..4i64 {
        let e = (at as i64 + 0x20 + 8 * i - (at as i64 + 0x30)) / 4;
        if wide {
            t.extend((e as i16).to_le_bytes());
        } else {
            t.push(e as i8 as u8);
        }
    }
    with_table(w, &t)
}

fn expected(at: u64, first: u64) -> Vec<u64> {
    (0..4).map(|i| at + first + 8 * i).collect()
}

/// The case targets of the first switch, as addresses.
fn targets(f: &Lifted) -> Option<Vec<u64>> {
    f.blocks.iter().find_map(|b| match &b.term {
        Term::Switch(_, cases, _) => Some(cases.iter().map(|&(_, id)| f.blocks[id].addr).collect()),
        _ => None,
    })
}

fn is_a_switch(w: &[u32], first: u64) {
    let f = lifted(w);
    assert_eq!(f.tables, 1);
    assert_eq!(f.unresolved_jumps, 0);
    assert_eq!(targets(&f), Some(expected(BASE, first)));
    let t = text(w);
    println!("{t}");
    assert!(t.contains("switch ("), "{t}");
}

#[test]
fn clang_one_byte_entries_scaled_by_four_from_an_adr_base() {
    is_a_switch(&clang(BASE, Clang::Byte), 0x24);
}

#[test]
fn clang_two_byte_entries() {
    is_a_switch(&clang(BASE, Clang::Half), 0x24);
}

#[test]
fn clang_four_byte_entries_relative_to_the_adr() {
    is_a_switch(&clang(BASE, Clang::Word), 0x24);
}

#[test]
fn gcc_signed_byte_entries_with_a_negative_one() {
    is_a_switch(&gcc(BASE, LDRSB_W3_X1_W0_UXTW, ADD_X4_X2_W3_SXTB2, false), 0x20);
}

#[test]
fn gcc_signed_half_entries() {
    is_a_switch(&gcc(BASE, LDRH_W3_X1_W0_UXTW1, ADD_X4_X2_W3_SXTH2, true), 0x20);
}

#[test]
fn a_table_of_eight_byte_pointers_stays_unresolved() {
    // cmp ; b.hi ; adrp ; add ; ldr x10,[x9,x8,lsl #3] ; br x10 : not a shape recovered here
    let at = BASE;
    let table = at + 0x1c;
    let mut w = vec![
        cmp_w0(3),
        b_cond(8, at + 4, at + 0x1c),
        MOV_W8_W0,
        adrp(9, at + 0xc, table),
        add_imm(9, 9, (table & 0xfff) as u32),
        LDR_X10_X9_X8_LSL3,
        BR_X10,
    ];
    w.extend([RET, RET]);
    let f = lifted(&w);
    assert_eq!(f.tables, 0);
    assert_eq!(f.unresolved_jumps, 1);
    assert!(targets(&f).is_none());
}

#[test]
fn a_br_without_a_bound_check_stays_unresolved() {
    let mut w = clang(BASE, Clang::Byte);
    // the compare becomes a nop: no bound, no table
    w[0] = 0xd503_201f;
    let f = lifted(&w);
    assert_eq!(f.tables, 0);
    assert_eq!(f.unresolved_jumps, 1);
}

#[test]
fn entries_that_leave_the_function_stay_unresolved() {
    let mut w = clang(BASE, Clang::Byte);
    // the last table word: entry 3 points far outside
    let n = w.len();
    w[n - 1] = 0x7f04_0200;
    let f = lifted(&w);
    assert_eq!(f.tables, 0);
    assert_eq!(f.unresolved_jumps, 1);
}
