//! Loads and stores: single registers, pairs, literals, exclusives, atomics, and the SIMD
//! structure loads compilers use for copies.

use super::text::Mn;
use super::{bits, gpr, gsp, sext, Ins, Mem, MemUse, Opd, Why, R};

fn w(i: &mut Ins, n: u32) {
    if n != 31 {
        i.writes |= 1 << n;
    }
}

fn mn(i: &mut Ins, s: &str) {
    i.mn = Mn::new(s);
}

pub(super) fn decode(i: &mut Ins) -> R {
    let raw = i.raw;
    let v = bits(raw, 26, 26) == 1;
    match bits(raw, 29, 27) {
        0b001 if !v => exclusive(i),
        0b001 => structure(i),
        0b011 => literal(i, v),
        0b101 => pair(i, v),
        0b111 => single(i, v),
        _ => Err(Why::Other),
    }
}

fn size_suffix(size: u32) -> &'static str {
    match size {
        0 => "b",
        1 => "h",
        _ => "",
    }
}

fn exclusive(i: &mut Ins) -> R {
    let raw = i.raw;
    let size = bits(raw, 31, 30);
    let (o2, l, o1, o0) = (bits(raw, 23, 23), bits(raw, 22, 22), bits(raw, 21, 21), bits(raw, 15, 15));
    let (rs, rt2, rn, rt) = (bits(raw, 20, 16), bits(raw, 14, 10), bits(raw, 9, 5), bits(raw, 4, 0));
    let x = size == 3;
    let sfx = size_suffix(size);
    let base = Opd::Mem(Mem::Off { base: rn as u8, off: 0 });
    let none = Opd::None;
    match (o2, o1) {
        (0, 0) => {
            let name = match (l, o0) {
                (0, 0) => "stxr",
                (0, _) => "stlxr",
                (1, 0) => "ldxr",
                _ => "ldaxr",
            };
            i.mn = Mn::new(name).push(sfx);
            if l == 0 {
                i.ops = [gpr(rs, false), gpr(rt, x), base, none, none];
                w(i, rs);
            } else {
                i.ops = [gpr(rt, x), base, none, none, none];
                w(i, rt);
            }
        }
        (0, 1) if size >= 2 => {
            let name = match (l, o0) {
                (0, 0) => "stxp",
                (0, _) => "stlxp",
                (1, 0) => "ldxp",
                _ => "ldaxp",
            };
            mn(i, name);
            if l == 0 {
                i.ops = [gpr(rs, false), gpr(rt, x), gpr(rt2, x), base, none];
                w(i, rs);
            } else {
                i.ops = [gpr(rt, x), gpr(rt2, x), base, none, none];
                w(i, rt);
                w(i, rt2);
            }
        }
        (1, 0) => {
            let name = match (l, o0) {
                (0, 0) => "stllr",
                (0, _) => "stlr",
                (1, 0) => "ldlar",
                _ => "ldar",
            };
            if rs != 31 || rt2 != 31 {
                return Err(Why::Reserved);
            }
            i.mn = Mn::new(name).push(sfx);
            i.ops = [gpr(rt, x), base, none, none, none];
            if l == 1 {
                w(i, rt);
            }
        }
        (1, 1) if rt2 == 31 => {
            let a = if l == 1 { "a" } else { "" };
            let r = if o0 == 1 { "l" } else { "" };
            i.mn = Mn::new("cas").push(a).push(r).push(sfx);
            i.ops = [gpr(rs, x), gpr(rt, x), base, none, none];
            w(i, rs);
        }
        _ => return Err(Why::Other),
    }
    Ok(())
}

fn literal(i: &mut Ins, v: bool) -> R {
    let raw = i.raw;
    let opc = bits(raw, 31, 30);
    let rt = bits(raw, 4, 0);
    let t = i.pc.wrapping_add((sext(u64::from(bits(raw, 23, 5)), 19) << 2) as u64);
    i.literal = Some(t);
    i.target = None;
    let d = if v {
        match opc {
            0 => Opd::Fp { n: rt as u8, sz: 2 },
            1 => Opd::Fp { n: rt as u8, sz: 3 },
            2 => Opd::Fp { n: rt as u8, sz: 4 },
            _ => return Err(Why::Reserved),
        }
    } else {
        match opc {
            0 => gpr(rt, false),
            1 => gpr(rt, true),
            2 => gpr(rt, true),
            _ => {
                mn(i, "prfm");
                i.ops = [prefetch(rt), Opd::Target(t), Opd::None, Opd::None, Opd::None];
                return Ok(());
            }
        }
    };
    mn(i, if !v && opc == 2 { "ldrsw" } else { "ldr" });
    i.ops = [d, Opd::Target(t), Opd::None, Opd::None, Opd::None];
    if !v {
        w(i, rt);
    }
    Ok(())
}

fn prefetch(rt: u32) -> Opd {
    const T: [&str; 24] = [
        "pldl1keep",
        "pldl1strm",
        "pldl2keep",
        "pldl2strm",
        "pldl3keep",
        "pldl3strm",
        "#6",
        "#7",
        "plil1keep",
        "plil1strm",
        "plil2keep",
        "plil2strm",
        "plil3keep",
        "plil3strm",
        "#14",
        "#15",
        "pstl1keep",
        "pstl1strm",
        "pstl2keep",
        "pstl2strm",
        "pstl3keep",
        "pstl3strm",
        "#22",
        "#23",
    ];
    match T.get(rt as usize) {
        Some(s) => Opd::Name(s),
        None => Opd::Dec(i64::from(rt)),
    }
}

fn fpreg(n: u32, sz: u8) -> Opd {
    Opd::Fp { n: n as u8, sz }
}

fn pair(i: &mut Ins, v: bool) -> R {
    let raw = i.raw;
    let opc = bits(raw, 31, 30);
    let (ty, l) = (bits(raw, 24, 23), bits(raw, 22, 22) == 1);
    let imm7 = sext(u64::from(bits(raw, 21, 15)), 7);
    let (rt2, rn, rt) = (bits(raw, 14, 10), bits(raw, 9, 5), bits(raw, 4, 0));
    let (scale, a, b, sw) = if v {
        let sz = match opc {
            0 => 2u8,
            1 => 3,
            2 => 4,
            _ => return Err(Why::Reserved),
        };
        (u32::from(sz), fpreg(rt, sz), fpreg(rt2, sz), false)
    } else {
        match opc {
            0 => (2, gpr(rt, false), gpr(rt2, false), false),
            1 if l => (2, gpr(rt, true), gpr(rt2, true), true),
            2 => (3, gpr(rt, true), gpr(rt2, true), false),
            _ => return Err(Why::Reserved),
        }
    };
    let off = imm7 << scale;
    let (name, mem) = match ty {
        0 => (if l { "ldnp" } else { "stnp" }, Mem::Off { base: rn as u8, off }),
        1 => (
            if sw {
                "ldpsw"
            } else if l {
                "ldp"
            } else {
                "stp"
            },
            Mem::Post { base: rn as u8, off },
        ),
        2 => (
            if sw {
                "ldpsw"
            } else if l {
                "ldp"
            } else {
                "stp"
            },
            Mem::Off { base: rn as u8, off },
        ),
        _ => (
            if sw {
                "ldpsw"
            } else if l {
                "ldp"
            } else {
                "stp"
            },
            Mem::Pre { base: rn as u8, off },
        ),
    };
    mn(i, name);
    i.ops = [a, b, Opd::Mem(mem), Opd::None, Opd::None];
    if matches!(mem, Mem::Off { .. }) {
        i.mem = Some(MemUse { base: rn as u8, off, store: !l });
    }
    if !v && l {
        w(i, rt);
        w(i, rt2);
    }
    if matches!(mem, Mem::Pre { .. } | Mem::Post { .. }) {
        w(i, rn);
    }
    Ok(())
}

/// The register, mnemonic stem and access size (log2 bytes) of a single load or store.
struct Access {
    name: &'static str,
    reg: Opd,
    scale: u32,
    load: bool,
    prefetch: bool,
}

fn access(v: bool, size: u32, opc: u32, rt: u32, kind: u32) -> Option<Access> {
    // kind: 0 normal, 1 unscaled (ur), 2 unprivileged (tr)
    let pick = |n: [&'static str; 3]| n[kind as usize];
    if v {
        if kind == 2 {
            return None;
        }
        let (sz, load) = match (size, opc) {
            (s, 0) => (s as u8, false),
            (s, 1) => (s as u8, true),
            (0, 2) => (4, false),
            (0, 3) => (4, true),
            _ => return None,
        };
        let name = if load { pick(["ldr", "ldur", "ldtr"]) } else { pick(["str", "stur", "sttr"]) };
        return Some(Access { name, reg: fpreg(rt, sz), scale: u32::from(sz), load, prefetch: false });
    }
    let (name, x, load) = match (size, opc) {
        (0, 0) => (pick(["strb", "sturb", "sttrb"]), false, false),
        (0, 1) => (pick(["ldrb", "ldurb", "ldtrb"]), false, true),
        (0, 2) => (pick(["ldrsb", "ldursb", "ldtrsb"]), true, true),
        (0, 3) => (pick(["ldrsb", "ldursb", "ldtrsb"]), false, true),
        (1, 0) => (pick(["strh", "sturh", "sttrh"]), false, false),
        (1, 1) => (pick(["ldrh", "ldurh", "ldtrh"]), false, true),
        (1, 2) => (pick(["ldrsh", "ldursh", "ldtrsh"]), true, true),
        (1, 3) => (pick(["ldrsh", "ldursh", "ldtrsh"]), false, true),
        (2, 0) => (pick(["str", "stur", "sttr"]), false, false),
        (2, 1) => (pick(["ldr", "ldur", "ldtr"]), false, true),
        (2, 2) => (pick(["ldrsw", "ldursw", "ldtrsw"]), true, true),
        (3, 0) => (pick(["str", "stur", "sttr"]), true, false),
        (3, 1) => (pick(["ldr", "ldur", "ldtr"]), true, true),
        (3, 2) if kind != 2 => {
            return Some(Access {
                name: pick(["prfm", "prfum", ""]),
                reg: prefetch(rt),
                scale: 3,
                load: false,
                prefetch: true,
            })
        }
        _ => return None,
    };
    Some(Access { name, reg: gpr(rt, x), scale: size, load, prefetch: false })
}

fn single(i: &mut Ins, v: bool) -> R {
    let raw = i.raw;
    let size = bits(raw, 31, 30);
    let opc = bits(raw, 23, 22);
    let (rn, rt) = (bits(raw, 9, 5), bits(raw, 4, 0));
    let none = Opd::None;
    if bits(raw, 24, 24) == 1 {
        let a = access(v, size, opc, rt, 0).ok_or(Why::Reserved)?;
        let off = (u64::from(bits(raw, 21, 10)) << a.scale) as i64;
        return finish(i, &a, Mem::Off { base: rn as u8, off }, rt);
    }
    if bits(raw, 21, 21) == 0 {
        let imm9 = sext(u64::from(bits(raw, 20, 12)), 9);
        let (kind, mem) = match bits(raw, 11, 10) {
            0 => (1, Mem::Off { base: rn as u8, off: imm9 }),
            1 => (0, Mem::Post { base: rn as u8, off: imm9 }),
            2 => (2, Mem::Off { base: rn as u8, off: imm9 }),
            _ => (0, Mem::Pre { base: rn as u8, off: imm9 }),
        };
        let a = access(v, size, opc, rt, kind).ok_or(Why::Reserved)?;
        return finish(i, &a, mem, rt);
    }
    match bits(raw, 11, 10) {
        2 => {
            let a = access(v, size, opc, rt, 0).ok_or(Why::Reserved)?;
            let option = bits(raw, 15, 13);
            if !matches!(option, 2 | 3 | 6 | 7) {
                return Err(Why::Reserved);
            }
            let s = bits(raw, 12, 12);
            let shift = if s == 1 { a.scale as u8 } else { 0 };
            let m = Mem::Reg { base: rn as u8, idx: bits(raw, 20, 16) as u8, ext: option as u8, shift };
            finish(i, &a, m, rt)
        }
        0 if !v => {
            // LSE atomics
            let (aq, rl, rs, o3, opc3) =
                (bits(raw, 23, 23), bits(raw, 22, 22), bits(raw, 20, 16), bits(raw, 15, 15), bits(raw, 14, 12));
            let stem = if o3 == 1 {
                if opc3 != 0 {
                    return Err(Why::Other);
                }
                "swp"
            } else {
                ["ldadd", "ldclr", "ldeor", "ldset", "ldsmax", "ldsmin", "ldumax", "ldumin"][opc3 as usize]
            };
            i.mn = Mn::new(stem)
                .push(if aq == 1 { "a" } else { "" })
                .push(if rl == 1 { "l" } else { "" })
                .push(size_suffix(size));
            let x = size == 3;
            i.ops = [gpr(rs, x), gpr(rt, x), Opd::Mem(Mem::Off { base: rn as u8, off: 0 }), none, none];
            w(i, rt);
            Ok(())
        }
        _ => Err(Why::Other),
    }
}

fn finish(i: &mut Ins, a: &Access, mem: Mem, rt: u32) -> R {
    i.mn = Mn::new(a.name);
    i.ops = [a.reg, Opd::Mem(mem), Opd::None, Opd::None, Opd::None];
    let base = match mem {
        Mem::Off { base, off } | Mem::Pre { base, off } => {
            if matches!(mem, Mem::Off { .. }) {
                i.mem = Some(MemUse { base, off, store: !a.load && !a.prefetch });
            }
            u32::from(base)
        }
        Mem::Post { base, .. } | Mem::PostReg { base, .. } | Mem::Reg { base, .. } => u32::from(base),
    };
    if a.load && matches!(a.reg, Opd::Gpr { .. }) {
        w(i, rt);
    }
    if matches!(mem, Mem::Pre { .. } | Mem::Post { .. }) {
        w(i, base);
    }
    Ok(())
}

/// `ld1`..`ld4` / `st1`..`st4` of whole registers.
fn structure(i: &mut Ins) -> R {
    let raw = i.raw;
    if bits(raw, 31, 31) != 0 || bits(raw, 24, 24) != 0 {
        return Err(Why::Simd);
    }
    let q = bits(raw, 30, 30);
    let post = bits(raw, 23, 23) == 1;
    let load = bits(raw, 22, 22) == 1;
    let rm = bits(raw, 20, 16);
    let opcode = bits(raw, 15, 12);
    let size = bits(raw, 11, 10);
    let (stem, count) = match opcode {
        0b0111 => ("1", 1u8),
        0b1010 => ("1", 2),
        0b0110 => ("1", 3),
        0b0010 => ("1", 4),
        0b1000 => ("2", 2),
        0b0100 => ("3", 3),
        0b0000 => ("4", 4),
        _ => return Err(Why::Simd),
    };
    if size == 3 && q == 0 && stem != "1" {
        return Err(Why::Reserved);
    }
    let arr = (size * 2 + q) as u8;
    let (rn, rt) = (bits(raw, 9, 5), bits(raw, 4, 0));
    i.mn = Mn::new(if load { "ld" } else { "st" }).push(stem);
    let bytes = i64::from(count) * if q == 1 { 16 } else { 8 };
    let mem = if post {
        if rm == 31 {
            Mem::Post { base: rn as u8, off: bytes }
        } else {
            Mem::PostReg { base: rn as u8, idx: rm as u8 }
        }
    } else {
        Mem::Off { base: rn as u8, off: 0 }
    };
    i.ops = [Opd::List { n: rt as u8, count, arr }, Opd::Mem(mem), Opd::None, Opd::None, Opd::None];
    if post {
        w(i, rn);
    }
    let _ = gsp;
    Ok(())
}
