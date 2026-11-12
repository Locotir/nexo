//! Data processing (immediate and register), branches and system instructions.

use super::text::Mn;
use super::{bits, gpr, gsp, sext, Flow, Ins, Opd, Why, R};

fn w(i: &mut Ins, n: u32) {
    if n != 31 {
        i.writes |= 1 << n;
    }
}

fn mn(i: &mut Ins, s: &str) {
    i.mn = Mn::new(s);
}

/// The value a logical-immediate encoding stands for, or None when the encoding is undefined.
fn bitmask(n: u32, imms: u32, immr: u32, datasize: u32) -> Option<u64> {
    let x = (n << 6) | (!imms & 0x3f);
    if x == 0 {
        return None;
    }
    let len = 31 - x.leading_zeros();
    if len < 1 {
        return None;
    }
    let esize = 1u32 << len;
    if esize > datasize {
        return None;
    }
    let levels = esize - 1;
    let s = imms & levels;
    let r = immr & levels;
    if s == levels {
        return None;
    }
    let welem: u64 = if s + 1 >= 64 { u64::MAX } else { (1u64 << (s + 1)) - 1 };
    let emask: u64 = if esize >= 64 { u64::MAX } else { (1u64 << esize) - 1 };
    let rot = if r == 0 { welem } else { ((welem >> r) | (welem << (esize - r))) & emask };
    let mut out = 0u64;
    let mut at = 0;
    while at < datasize {
        out |= rot << at;
        at += esize;
    }
    Some(if datasize == 32 { out & 0xffff_ffff } else { out })
}

pub(super) fn imm(i: &mut Ins) -> R {
    let raw = i.raw;
    let sf = bits(raw, 31, 31) == 1;
    let rd = bits(raw, 4, 0);
    let rn = bits(raw, 9, 5);
    match bits(raw, 25, 23) {
        0b000 | 0b001 => {
            let imm = sext(u64::from((bits(raw, 23, 5) << 2) | bits(raw, 30, 29)), 21);
            if sf {
                let page = (i.pc & !0xfff).wrapping_add((imm << 12) as u64);
                mn(i, "adrp");
                i.ops = [gpr(rd, true), Opd::Target(page), Opd::None, Opd::None, Opd::None];
                i.adrp = Some((rd as u8, page));
            } else {
                let a = i.pc.wrapping_add(imm as u64);
                mn(i, "adr");
                i.ops = [gpr(rd, true), Opd::Target(a), Opd::None, Opd::None, Opd::None];
                i.adr = Some((rd as u8, a));
            }
            w(i, rd);
            Ok(())
        }
        0b010 => {
            let (sub, s) = (bits(raw, 30, 30) == 1, bits(raw, 29, 29) == 1);
            let sh = bits(raw, 22, 22) == 1;
            let imm = u64::from(bits(raw, 21, 10));
            let v = if sh { imm << 12 } else { imm };
            let shift = if sh { Opd::Shift { kind: 0, amt: 12 } } else { Opd::None };
            if s && rd == 31 {
                mn(i, if sub { "cmp" } else { "cmn" });
                i.ops = [gsp(rn, sf), Opd::Imm(imm as i64), shift, Opd::None, Opd::None];
            } else if !sub && !s && !sh && imm == 0 && (rd == 31 || rn == 31) {
                mn(i, "mov");
                i.ops = [gsp(rd, sf), gsp(rn, sf), Opd::None, Opd::None, Opd::None];
                w(i, rd);
            } else {
                mn(
                    i,
                    match (sub, s) {
                        (false, false) => "add",
                        (false, true) => "adds",
                        (true, false) => "sub",
                        (true, true) => "subs",
                    },
                );
                let d = if s { gpr(rd, sf) } else { gsp(rd, sf) };
                i.ops = [d, gsp(rn, sf), Opd::Imm(imm as i64), shift, Opd::None];
                if !sub && !s {
                    i.addimm = Some((rd as u8, rn as u8, v));
                }
                w(i, rd);
            }
            Ok(())
        }
        0b100 => {
            let opc = bits(raw, 30, 29);
            let n = bits(raw, 22, 22);
            if !sf && n == 1 {
                return Err(Why::Reserved);
            }
            let v = bitmask(n, bits(raw, 15, 10), bits(raw, 21, 16), if sf { 64 } else { 32 }).ok_or(Why::Reserved)?;
            if opc == 3 && rd == 31 {
                mn(i, "tst");
                i.ops = [gpr(rn, sf), Opd::UImm(v), Opd::None, Opd::None, Opd::None];
            } else if opc == 1 && rn == 31 {
                mn(i, "mov");
                i.ops = [
                    gsp(rd, sf),
                    Opd::Imm(if sf { v as i64 } else { i64::from(v as u32 as i32) }),
                    Opd::None,
                    Opd::None,
                    Opd::None,
                ];
                w(i, rd);
            } else {
                mn(i, ["and", "orr", "eor", "ands"][opc as usize]);
                let d = if opc == 3 { gpr(rd, sf) } else { gsp(rd, sf) };
                i.ops = [d, gpr(rn, sf), Opd::UImm(v), Opd::None, Opd::None];
                w(i, rd);
            }
            Ok(())
        }
        0b101 => {
            let opc = bits(raw, 30, 29);
            let hw = bits(raw, 22, 21);
            if opc == 1 || (!sf && hw >= 2) {
                return Err(Why::Reserved);
            }
            let imm16 = u64::from(bits(raw, 20, 5));
            let shift = hw * 16;
            let mask = if sf { u64::MAX } else { 0xffff_ffff };
            let shown = if shift != 0 { Opd::Shift { kind: 0, amt: shift as u8 } } else { Opd::None };
            w(i, rd);
            match opc {
                0 => {
                    let v = !(imm16 << shift) & mask;
                    if (imm16 == 0 && hw != 0) || (!sf && imm16 == 0xffff && hw == 0) {
                        mn(i, "movn");
                        i.ops = [gpr(rd, sf), Opd::Imm(imm16 as i64), shown, Opd::None, Opd::None];
                    } else {
                        mn(i, "mov");
                        i.ops = [
                            gpr(rd, sf),
                            Opd::Imm(if sf { v as i64 } else { i64::from(v as u32 as i32) }),
                            Opd::None,
                            Opd::None,
                            Opd::None,
                        ];
                    }
                }
                2 => {
                    if imm16 == 0 && hw != 0 {
                        mn(i, "movz");
                        i.ops = [gpr(rd, sf), Opd::Imm(0), shown, Opd::None, Opd::None];
                    } else {
                        mn(i, "mov");
                        i.ops = [
                            gpr(rd, sf),
                            Opd::Imm(if sf {
                                (imm16 << shift) as i64
                            } else {
                                i64::from((imm16 << shift) as u32 as i32)
                            }),
                            Opd::None,
                            Opd::None,
                            Opd::None,
                        ];
                    }
                }
                _ => {
                    mn(i, "movk");
                    i.ops = [gpr(rd, sf), Opd::Imm(imm16 as i64), shown, Opd::None, Opd::None];
                }
            }
            Ok(())
        }
        0b110 => bitfield(i, sf, rd, rn),
        _ => {
            // extract
            if bits(raw, 30, 29) != 0 || bits(raw, 21, 21) != 0 || bits(raw, 22, 22) != u32::from(sf) {
                return Err(Why::Reserved);
            }
            let (rm, lsb) = (bits(raw, 20, 16), bits(raw, 15, 10));
            if !sf && lsb >= 32 {
                return Err(Why::Reserved);
            }
            w(i, rd);
            if rn == rm {
                mn(i, "ror");
                i.ops = [gpr(rd, sf), gpr(rn, sf), Opd::Dec(i64::from(lsb)), Opd::None, Opd::None];
            } else {
                mn(i, "extr");
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), Opd::Dec(i64::from(lsb)), Opd::None];
            }
            Ok(())
        }
    }
}

fn bitfield(i: &mut Ins, sf: bool, rd: u32, rn: u32) -> R {
    let raw = i.raw;
    let opc = bits(raw, 30, 29);
    if opc == 3 || bits(raw, 22, 22) != u32::from(sf) {
        return Err(Why::Reserved);
    }
    let (immr, imms) = (bits(raw, 21, 16), bits(raw, 15, 10));
    let size = if sf { 64 } else { 32 };
    if !sf && (immr >= 32 || imms >= 32) {
        return Err(Why::Reserved);
    }
    w(i, rd);
    let d = |n: i64| Opd::Dec(n);
    let (rdo, rno) = (gpr(rd, sf), gpr(rn, sf));
    match opc {
        0 => {
            if imms == size - 1 {
                mn(i, "asr");
                i.ops = [rdo, rno, d(i64::from(immr)), Opd::None, Opd::None];
            } else if immr == 0 && matches!(imms, 7 | 15) || (immr == 0 && imms == 31 && sf) {
                mn(
                    i,
                    match imms {
                        7 => "sxtb",
                        15 => "sxth",
                        _ => "sxtw",
                    },
                );
                i.ops = [rdo, gpr(rn, false), Opd::None, Opd::None, Opd::None];
            } else if imms < immr {
                mn(i, "sbfiz");
                i.ops = [rdo, rno, d(i64::from((size - immr) % size)), d(i64::from(imms + 1)), Opd::None];
            } else {
                mn(i, "sbfx");
                i.ops = [rdo, rno, d(i64::from(immr)), d(i64::from(imms - immr + 1)), Opd::None];
            }
        }
        2 => {
            if imms != size - 1 && imms + 1 == immr {
                mn(i, "lsl");
                i.ops = [rdo, rno, d(i64::from(size - 1 - imms)), Opd::None, Opd::None];
            } else if imms == size - 1 {
                mn(i, "lsr");
                i.ops = [rdo, rno, d(i64::from(immr)), Opd::None, Opd::None];
            } else if !sf && immr == 0 && matches!(imms, 7 | 15) {
                mn(i, if imms == 7 { "uxtb" } else { "uxth" });
                i.ops = [rdo, rno, Opd::None, Opd::None, Opd::None];
            } else if imms < immr {
                mn(i, "ubfiz");
                i.ops = [rdo, rno, d(i64::from((size - immr) % size)), d(i64::from(imms + 1)), Opd::None];
            } else {
                mn(i, "ubfx");
                i.ops = [rdo, rno, d(i64::from(immr)), d(i64::from(imms - immr + 1)), Opd::None];
            }
        }
        _ => {
            if rn == 31 && imms < immr {
                mn(i, "bfc");
                i.ops = [rdo, d(i64::from((size - immr) % size)), d(i64::from(imms + 1)), Opd::None, Opd::None];
            } else if imms < immr {
                mn(i, "bfi");
                i.ops = [rdo, rno, d(i64::from((size - immr) % size)), d(i64::from(imms + 1)), Opd::None];
            } else {
                mn(i, "bfxil");
                i.ops = [rdo, rno, d(i64::from(immr)), d(i64::from(imms - immr + 1)), Opd::None];
            }
        }
    }
    Ok(())
}

const HINTS: [(u32, &str); 24] = [
    (0, "nop"),
    (1, "yield"),
    (2, "wfe"),
    (3, "wfi"),
    (4, "sev"),
    (5, "sevl"),
    (6, "dgh"),
    (7, "xpaclri"),
    (8, "pacia1716"),
    (10, "pacib1716"),
    (12, "autia1716"),
    (14, "autib1716"),
    (16, "esb"),
    (20, "csdb"),
    (24, "paciaz"),
    (25, "paciasp"),
    (26, "pacibz"),
    (27, "pacibsp"),
    (28, "autiaz"),
    (29, "autiasp"),
    (30, "autibz"),
    (31, "autibsp"),
    (32, "bti"),
    (34, "bti c"),
];

fn barrier(v: u32) -> Opd {
    match v {
        1 => Opd::Name("oshld"),
        2 => Opd::Name("oshst"),
        3 => Opd::Name("osh"),
        5 => Opd::Name("nshld"),
        6 => Opd::Name("nshst"),
        7 => Opd::Name("nsh"),
        9 => Opd::Name("ishld"),
        10 => Opd::Name("ishst"),
        11 => Opd::Name("ish"),
        13 => Opd::Name("ld"),
        14 => Opd::Name("st"),
        15 => Opd::Name("sy"),
        n => Opd::Imm(i64::from(n)),
    }
}

fn sysreg(raw: u32) -> Opd {
    Opd::Sys(bits(raw, 19, 5) as u16)
}

pub(super) fn branch(i: &mut Ins) -> R {
    let raw = i.raw;
    let top = bits(raw, 31, 29);
    match top {
        0b000 | 0b100 => {
            let off = sext(u64::from(bits(raw, 25, 0)), 26) << 2;
            let t = i.pc.wrapping_add(off as u64);
            let call = top == 0b100;
            mn(i, if call { "bl" } else { "b" });
            i.ops[0] = Opd::Target(t);
            i.target = Some(t);
            i.flow = if call { Flow::Call } else { Flow::Jump };
            if call {
                i.writes |= 1 << 30;
            }
            Ok(())
        }
        0b001 | 0b101 => {
            let sf = top == 0b101;
            let rt = bits(raw, 4, 0);
            let off = sext(u64::from(bits(raw, 23, 5)), 19) << 2;
            if bits(raw, 25, 25) == 0 {
                mn(i, if bits(raw, 24, 24) == 1 { "cbnz" } else { "cbz" });
                let t = i.pc.wrapping_add(off as u64);
                i.ops = [gpr(rt, sf), Opd::Target(t), Opd::None, Opd::None, Opd::None];
                i.target = Some(t);
            } else {
                let off = sext(u64::from(bits(raw, 18, 5)), 14) << 2;
                let bit = (bits(raw, 31, 31) << 5) | bits(raw, 23, 19);
                mn(i, if bits(raw, 24, 24) == 1 { "tbnz" } else { "tbz" });
                let t = i.pc.wrapping_add(off as u64);
                i.ops = [gpr(rt, bit >= 32), Opd::Dec(i64::from(bit)), Opd::Target(t), Opd::None, Opd::None];
                i.target = Some(t);
            }
            i.flow = Flow::Cond;
            Ok(())
        }
        0b010 => {
            if bits(raw, 25, 25) != 0 || bits(raw, 4, 4) != 0 {
                return Err(Why::Other);
            }
            let off = sext(u64::from(bits(raw, 23, 5)), 19) << 2;
            let t = i.pc.wrapping_add(off as u64);
            mn(i, "b");
            i.cc = bits(raw, 3, 0) as u8;
            i.ops[0] = Opd::Target(t);
            i.target = Some(t);
            i.flow = Flow::Cond;
            Ok(())
        }
        0b110 => system(i),
        _ => Err(Why::Reserved),
    }
}

fn system(i: &mut Ins) -> R {
    let raw = i.raw;
    if bits(raw, 31, 24) == 0b1101_0100 {
        // exception generation
        let (opc, ll) = (bits(raw, 23, 21), bits(raw, 1, 0));
        let imm = Opd::Imm(i64::from(bits(raw, 20, 5)));
        let name = match (opc, ll) {
            (0, 1) => "svc",
            (0, 2) => "hvc",
            (0, 3) => "smc",
            (1, 0) => "brk",
            (2, 0) => "hlt",
            (5, 1) => "dcps1",
            (5, 2) => "dcps2",
            (5, 3) => "dcps3",
            _ => return Err(Why::Reserved),
        };
        mn(i, name);
        i.ops[0] = imm;
        if matches!(name, "brk" | "hlt") {
            i.flow = Flow::Trap;
        }
        return Ok(());
    }
    if bits(raw, 31, 22) == 0b11_0101_0100 {
        let l = bits(raw, 21, 21);
        let (crm, op2, rt) = (bits(raw, 11, 8), bits(raw, 7, 5), bits(raw, 4, 0));
        let crn = bits(raw, 15, 12);
        let op1 = bits(raw, 18, 16);
        if bits(raw, 21, 19) == 0 && op1 == 3 && crn == 2 && rt == 31 {
            let code = (crm << 3) | op2;
            if let Some(&(_, n)) = HINTS.iter().find(|&&(c, _)| c == code) {
                mn(i, n);
            } else if code == 36 {
                mn(i, "bti j");
            } else if code == 38 {
                mn(i, "bti jc");
            } else {
                mn(i, "hint");
                i.ops[0] = Opd::Imm(i64::from(code));
            }
            return Ok(());
        }
        if bits(raw, 21, 19) == 0 && op1 == 3 && crn == 3 && rt == 31 {
            match op2 {
                2 => {
                    mn(i, "clrex");
                    if crm != 15 {
                        i.ops[0] = Opd::Imm(i64::from(crm));
                    }
                }
                4 => {
                    mn(i, "dsb");
                    i.ops[0] = barrier(crm);
                }
                5 => {
                    mn(i, "dmb");
                    i.ops[0] = barrier(crm);
                }
                6 => {
                    mn(i, "isb");
                    if crm != 15 {
                        i.ops[0] = Opd::Imm(i64::from(crm));
                    }
                }
                _ => return Err(Why::Other),
            }
            return Ok(());
        }
        if bits(raw, 20, 20) == 1 {
            // msr / mrs (register)
            if l == 1 {
                mn(i, "mrs");
                i.ops = [gpr(rt, true), sysreg(raw), Opd::None, Opd::None, Opd::None];
                w(i, rt);
            } else {
                mn(i, "msr");
                i.ops = [sysreg(raw), gpr(rt, true), Opd::None, Opd::None, Opd::None];
            }
            return Ok(());
        }
        if bits(raw, 21, 19) == 0b001 && l == 0 {
            let name = match (op1, crn, crm, op2) {
                (3, 7, 4, 1) => Some("dc zva"),
                (3, 7, 10, 1) => Some("dc cvac"),
                (3, 7, 11, 1) => Some("dc cvau"),
                (3, 7, 14, 1) => Some("dc civac"),
                (3, 7, 5, 1) => Some("ic ivau"),
                (0, 7, 5, 0) => Some("ic iallu"),
                _ => None,
            };
            if let Some(n) = name {
                mn(i, n);
                if rt != 31 || (crm, op2) != (5, 0) {
                    i.ops[0] = gpr(rt, true);
                }
                return Ok(());
            }
        }
        return Err(Why::Other);
    }
    if bits(raw, 31, 25) == 0b110_1011 {
        let (opc, op2, op3, rn, op4) =
            (bits(raw, 24, 21), bits(raw, 20, 16), bits(raw, 15, 10), bits(raw, 9, 5), bits(raw, 4, 0));
        if op2 != 0b11111 {
            return Err(Why::Reserved);
        }
        match (opc, op3, op4) {
            (0, 0, 0) => {
                mn(i, "br");
                i.ops[0] = gpr(rn, true);
                i.flow = Flow::JumpReg;
            }
            (1, 0, 0) => {
                mn(i, "blr");
                i.ops[0] = gpr(rn, true);
                i.flow = Flow::CallReg;
                i.writes |= 1 << 30;
            }
            (2, 0, 0) => {
                mn(i, "ret");
                if rn != 30 {
                    i.ops[0] = gpr(rn, true);
                }
                i.flow = Flow::Ret;
            }
            (4, 0, 0) if rn == 31 => {
                mn(i, "eret");
                i.flow = Flow::Ret;
            }
            (2, 2, 31) if rn == 31 => {
                mn(i, "retaa");
                i.flow = Flow::Ret;
            }
            (2, 3, 31) if rn == 31 => {
                mn(i, "retab");
                i.flow = Flow::Ret;
            }
            (0, 2, 31) => {
                mn(i, "braaz");
                i.ops[0] = gpr(rn, true);
                i.flow = Flow::JumpReg;
            }
            (1, 2, 31) => {
                mn(i, "blraaz");
                i.ops[0] = gpr(rn, true);
                i.flow = Flow::CallReg;
                i.writes |= 1 << 30;
            }
            _ => return Err(Why::Other),
        }
        return Ok(());
    }
    Err(Why::Other)
}

fn shift_op(kind: u32, amt: u32) -> Opd {
    if amt == 0 && kind == 0 {
        Opd::None
    } else {
        Opd::Shift { kind: kind as u8, amt: amt as u8 }
    }
}

pub(super) fn reg(i: &mut Ins) -> R {
    let raw = i.raw;
    let sf = bits(raw, 31, 31) == 1;
    let (rd, rn, rm) = (bits(raw, 4, 0), bits(raw, 9, 5), bits(raw, 20, 16));
    let none = Opd::None;
    if bits(raw, 28, 28) == 0 {
        let opc = bits(raw, 30, 29);
        let shift = bits(raw, 23, 22);
        let imm6 = bits(raw, 15, 10);
        if !sf && imm6 >= 32 {
            return Err(Why::Reserved);
        }
        if bits(raw, 24, 24) == 0 {
            // logical, shifted register
            let nbit = bits(raw, 21, 21);
            let s = shift_op(shift, imm6);
            w(i, rd);
            if opc == 1 && nbit == 0 && rn == 31 && shift == 0 && imm6 == 0 {
                mn(i, "mov");
                i.ops = [gpr(rd, sf), gpr(rm, sf), none, none, none];
            } else if opc == 1 && nbit == 1 && rn == 31 {
                mn(i, "mvn");
                i.ops = [gpr(rd, sf), gpr(rm, sf), s, none, none];
            } else if opc == 3 && nbit == 0 && rd == 31 {
                mn(i, "tst");
                i.ops = [gpr(rn, sf), gpr(rm, sf), s, none, none];
            } else {
                let names = [["and", "bic"], ["orr", "orn"], ["eor", "eon"], ["ands", "bics"]];
                mn(i, names[opc as usize][nbit as usize]);
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), s, none];
            }
            return Ok(());
        }
        let (sub, s) = (bits(raw, 30, 30) == 1, bits(raw, 29, 29) == 1);
        if bits(raw, 21, 21) == 0 {
            // add/sub, shifted register
            if shift == 3 {
                return Err(Why::Reserved);
            }
            let sh = shift_op(shift, imm6);
            if s && rd == 31 {
                mn(i, if sub { "cmp" } else { "cmn" });
                i.ops = [gpr(rn, sf), gpr(rm, sf), sh, none, none];
            } else if sub && rn == 31 {
                mn(i, if s { "negs" } else { "neg" });
                i.ops = [gpr(rd, sf), gpr(rm, sf), sh, none, none];
                w(i, rd);
            } else {
                mn(i, [["add", "adds"], ["sub", "subs"]][sub as usize][s as usize]);
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), sh, none];
                w(i, rd);
            }
            return Ok(());
        }
        // add/sub, extended register
        if shift != 0 {
            return Err(Why::Reserved);
        }
        let option = bits(raw, 15, 13);
        let amt = bits(raw, 12, 10);
        if amt > 4 {
            return Err(Why::Reserved);
        }
        let rm_x = option & 3 == 3;
        let uses_lsl = (rd == 31 && !s || rn == 31) && option == if sf { 3 } else { 2 };
        let ext = if uses_lsl {
            Opd::Ext { kind: 8, amt: amt as u8, show: false }
        } else {
            Opd::Ext { kind: option as u8, amt: amt as u8, show: false }
        };
        if s && rd == 31 {
            mn(i, if sub { "cmp" } else { "cmn" });
            i.ops = [gsp(rn, sf), gpr(rm, sf && rm_x), ext, none, none];
        } else {
            mn(i, [["add", "adds"], ["sub", "subs"]][sub as usize][s as usize]);
            let d = if s { gpr(rd, sf) } else { gsp(rd, sf) };
            i.ops = [d, gsp(rn, sf), gpr(rm, sf && rm_x), ext, none];
            w(i, rd);
        }
        return Ok(());
    }
    let op2 = bits(raw, 24, 21);
    match op2 {
        0b0000 => {
            if bits(raw, 15, 10) != 0 {
                return Err(Why::Reserved);
            }
            let (sub, s) = (bits(raw, 30, 30) == 1, bits(raw, 29, 29) == 1);
            w(i, rd);
            if sub && rn == 31 {
                mn(i, if s { "ngcs" } else { "ngc" });
                i.ops = [gpr(rd, sf), gpr(rm, sf), none, none, none];
            } else {
                mn(i, [["adc", "adcs"], ["sbc", "sbcs"]][sub as usize][s as usize]);
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), none, none];
            }
            Ok(())
        }
        0b0010 => {
            if bits(raw, 29, 29) != 1 || bits(raw, 10, 10) != 0 || bits(raw, 4, 4) != 0 {
                return Err(Why::Reserved);
            }
            mn(i, if bits(raw, 30, 30) == 1 { "ccmp" } else { "ccmn" });
            let second = if bits(raw, 11, 11) == 1 { Opd::Dec(i64::from(rm)) } else { gpr(rm, sf) };
            i.ops =
                [gpr(rn, sf), second, Opd::Dec(i64::from(bits(raw, 3, 0))), Opd::Cond(bits(raw, 15, 12) as u8), none];
            Ok(())
        }
        0b0100 => {
            if bits(raw, 29, 29) != 0 || bits(raw, 11, 11) != 0 {
                return Err(Why::Reserved);
            }
            let op = bits(raw, 30, 30);
            let o2 = bits(raw, 10, 10);
            let cond = bits(raw, 15, 12);
            w(i, rd);
            let inv = Opd::Cond((cond ^ 1) as u8);
            let same = rn == rm && cond < 14;
            match (op, o2) {
                (0, 0) => {
                    mn(i, "csel");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), Opd::Cond(cond as u8), none];
                }
                (0, 1) if same && rn == 31 => {
                    mn(i, "cset");
                    i.ops = [gpr(rd, sf), inv, none, none, none];
                }
                (0, 1) if same => {
                    mn(i, "cinc");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), inv, none, none];
                }
                (0, 1) => {
                    mn(i, "csinc");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), Opd::Cond(cond as u8), none];
                }
                (1, 0) if same && rn == 31 => {
                    mn(i, "csetm");
                    i.ops = [gpr(rd, sf), inv, none, none, none];
                }
                (1, 0) if same => {
                    mn(i, "cinv");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), inv, none, none];
                }
                (1, 0) => {
                    mn(i, "csinv");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), Opd::Cond(cond as u8), none];
                }
                (1, 1) if same => {
                    mn(i, "cneg");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), inv, none, none];
                }
                _ => {
                    mn(i, "csneg");
                    i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), Opd::Cond(cond as u8), none];
                }
            }
            Ok(())
        }
        0b0110 => {
            w(i, rd);
            let opcode = bits(raw, 15, 10);
            if bits(raw, 30, 30) == 1 {
                if bits(raw, 29, 29) != 0 || rm != 0 {
                    return Err(Why::Other);
                }
                let name = match (opcode, sf) {
                    (0, _) => "rbit",
                    (1, _) => "rev16",
                    (2, true) => "rev32",
                    (2, false) | (3, true) => "rev",
                    (4, _) => "clz",
                    (5, _) => "cls",
                    _ => return Err(Why::Other),
                };
                mn(i, name);
                i.ops = [gpr(rd, sf), gpr(rn, sf), none, none, none];
                return Ok(());
            }
            if bits(raw, 29, 29) != 0 {
                return Err(Why::Reserved);
            }
            let name = match opcode {
                2 => "udiv",
                3 => "sdiv",
                8 => "lsl",
                9 => "lsr",
                10 => "asr",
                11 => "ror",
                16..=19 => ["crc32b", "crc32h", "crc32w", "crc32x"][(opcode - 16) as usize],
                20..=23 => ["crc32cb", "crc32ch", "crc32cw", "crc32cx"][(opcode - 20) as usize],
                _ => return Err(Why::Other),
            };
            mn(i, name);
            let crc = opcode >= 16;
            i.ops = if crc {
                [gpr(rd, false), gpr(rn, false), gpr(rm, opcode & 3 == 3), none, none]
            } else {
                [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), none, none]
            };
            Ok(())
        }
        _ if op2 >= 0b1000 => mul(i, sf, rd, rn, rm),
        _ => Err(Why::Other),
    }
}

fn mul(i: &mut Ins, sf: bool, rd: u32, rn: u32, rm: u32) -> R {
    let raw = i.raw;
    if bits(raw, 30, 29) != 0 {
        return Err(Why::Reserved);
    }
    let ra = bits(raw, 14, 10);
    let o0 = bits(raw, 15, 15);
    let op31 = bits(raw, 23, 21);
    let none = Opd::None;
    w(i, rd);
    match op31 {
        0 => {
            if ra == 31 {
                mn(i, if o0 == 0 { "mul" } else { "mneg" });
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), none, none];
            } else {
                mn(i, if o0 == 0 { "madd" } else { "msub" });
                i.ops = [gpr(rd, sf), gpr(rn, sf), gpr(rm, sf), gpr(ra, sf), none];
            }
        }
        1 | 5 if sf => {
            let (u, base) = if op31 == 5 { ("u", 0) } else { ("s", 0) };
            let _ = base;
            let name = match (u, o0, ra == 31) {
                ("s", 0, true) => "smull",
                ("s", 1, true) => "smnegl",
                ("s", 0, false) => "smaddl",
                ("s", _, _) => "smsubl",
                (_, 0, true) => "umull",
                (_, 1, true) => "umnegl",
                (_, 0, false) => "umaddl",
                _ => "umsubl",
            };
            mn(i, name);
            i.ops = if ra == 31 {
                [gpr(rd, true), gpr(rn, false), gpr(rm, false), none, none]
            } else {
                [gpr(rd, true), gpr(rn, false), gpr(rm, false), gpr(ra, true), none]
            };
        }
        2 | 6 if sf && o0 == 0 && ra == 31 => {
            mn(i, if op31 == 2 { "smulh" } else { "umulh" });
            i.ops = [gpr(rd, true), gpr(rn, true), gpr(rm, true), none, none];
        }
        _ => return Err(Why::Other),
    }
    Ok(())
}
