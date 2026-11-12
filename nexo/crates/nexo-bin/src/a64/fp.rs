//! Scalar floating point (single, double, half). Vector (Advanced SIMD) forms are not decoded.

use super::text::Mn;
use super::{bits, gpr, Ins, Opd, Why, R};

fn f(n: u32, ty: u32) -> Opd {
    Opd::Fp {
        n: n as u8,
        sz: match ty {
            0 => 2,
            1 => 3,
            _ => 1,
        },
    }
}

fn expand(imm8: u32) -> f64 {
    let sign = if imm8 & 0x80 != 0 { -1.0 } else { 1.0 };
    let e = (imm8 >> 4) & 7;
    let exp: i32 = if e & 4 == 0 { e as i32 + 1 } else { e as i32 - 7 };
    sign * (16.0 + f64::from(imm8 & 15)) / 16.0 * 2f64.powi(exp)
}

pub(super) fn decode(i: &mut Ins) -> R {
    let raw = i.raw;
    if bits(raw, 28, 24) != 0b11110 && bits(raw, 28, 24) != 0b11111 {
        return Err(Why::Simd);
    }
    let sf = bits(raw, 31, 31) == 1;
    if bits(raw, 30, 29) != 0 {
        return Err(Why::Simd);
    }
    let ty = bits(raw, 23, 22);
    if ty == 2 {
        return Err(Why::Reserved);
    }
    let (rd, rn, rm) = (bits(raw, 4, 0), bits(raw, 9, 5), bits(raw, 20, 16));
    let none = Opd::None;
    let mn = |i: &mut Ins, s: &str| i.mn = Mn::new(s);
    if bits(raw, 28, 24) == 0b11111 {
        if sf {
            return Err(Why::Reserved);
        }
        let (o1, o0) = (bits(raw, 21, 21), bits(raw, 15, 15));
        mn(i, [["fmadd", "fmsub"], ["fnmadd", "fnmsub"]][o1 as usize][o0 as usize]);
        i.ops = [f(rd, ty), f(rn, ty), f(rm, ty), f(bits(raw, 14, 10), ty), none];
        return Ok(());
    }
    if bits(raw, 21, 21) == 0 {
        // fixed-point conversions
        let (rmode, opcode, scale) = (bits(raw, 20, 19), bits(raw, 18, 16), bits(raw, 15, 10));
        if !sf && scale < 32 {
            return Err(Why::Reserved);
        }
        let fbits = i64::from(64 - scale);
        match (rmode, opcode) {
            (0, 2) | (0, 3) => {
                mn(i, if opcode == 2 { "scvtf" } else { "ucvtf" });
                i.ops = [f(rd, ty), gpr(rn, sf), Opd::Dec(fbits), none, none];
            }
            (3, 0) | (3, 1) => {
                mn(i, if opcode == 0 { "fcvtzs" } else { "fcvtzu" });
                i.ops = [gpr(rd, sf), f(rn, ty), Opd::Dec(fbits), none, none];
                if rd != 31 {
                    i.writes |= 1 << rd;
                }
            }
            _ => return Err(Why::Other),
        }
        return Ok(());
    }
    if bits(raw, 15, 10) == 0 {
        let (rmode, opcode) = (bits(raw, 20, 19), bits(raw, 18, 16));
        let to_int = |i: &mut Ins, name: &str| {
            mn(i, name);
            i.ops = [gpr(rd, sf), f(rn, ty), none, none, none];
            if rd != 31 {
                i.writes |= 1 << rd;
            }
        };
        match (rmode, opcode) {
            (0, 0) => to_int(i, "fcvtns"),
            (0, 1) => to_int(i, "fcvtnu"),
            (0, 4) => to_int(i, "fcvtas"),
            (0, 5) => to_int(i, "fcvtau"),
            (1, 0) => to_int(i, "fcvtps"),
            (1, 1) => to_int(i, "fcvtpu"),
            (2, 0) => to_int(i, "fcvtms"),
            (2, 1) => to_int(i, "fcvtmu"),
            (3, 0) => to_int(i, "fcvtzs"),
            (3, 1) => to_int(i, "fcvtzu"),
            (0, 2) | (0, 3) => {
                mn(i, if opcode == 2 { "scvtf" } else { "ucvtf" });
                i.ops = [f(rd, ty), gpr(rn, sf), none, none, none];
            }
            (0, 6) if (ty == 0 && !sf) || (ty == 1 && sf) => to_int(i, "fmov"),
            (0, 7) if (ty == 0 && !sf) || (ty == 1 && sf) => {
                mn(i, "fmov");
                i.ops = [f(rd, ty), gpr(rn, sf), none, none, none];
            }
            _ => return Err(Why::Other),
        }
        return Ok(());
    }
    if bits(raw, 14, 10) == 0b10000 {
        let opcode = bits(raw, 20, 15);
        let (name, dty) = match opcode {
            0 => ("fmov", ty),
            1 => ("fabs", ty),
            2 => ("fneg", ty),
            3 => ("fsqrt", ty),
            4 => ("fcvt", 0),
            5 => ("fcvt", 1),
            7 => ("fcvt", 3),
            8 => ("frintn", ty),
            9 => ("frintp", ty),
            10 => ("frintm", ty),
            11 => ("frintz", ty),
            12 => ("frinta", ty),
            14 => ("frintx", ty),
            15 => ("frinti", ty),
            _ => return Err(Why::Other),
        };
        if name == "fcvt" && dty == ty {
            return Err(Why::Reserved);
        }
        mn(i, name);
        i.ops = [f(rd, dty), f(rn, ty), none, none, none];
        return Ok(());
    }
    if bits(raw, 13, 10) == 0b1000 {
        let op = bits(raw, 4, 0);
        mn(i, if op & 0x10 != 0 { "fcmpe" } else { "fcmp" });
        let second = if op & 8 != 0 { Opd::FImm(0.0) } else { f(rm, ty) };
        i.ops = [f(rn, ty), second, none, none, none];
        return Ok(());
    }
    if bits(raw, 12, 10) == 0b100 {
        mn(i, "fmov");
        i.ops = [f(rd, ty), Opd::FImm(expand(bits(raw, 20, 13))), none, none, none];
        return Ok(());
    }
    match bits(raw, 11, 10) {
        1 => {
            mn(i, if bits(raw, 4, 4) == 1 { "fccmpe" } else { "fccmp" });
            i.ops =
                [f(rn, ty), f(rm, ty), Opd::Dec(i64::from(bits(raw, 3, 0))), Opd::Cond(bits(raw, 15, 12) as u8), none];
        }
        2 => {
            let name = match bits(raw, 15, 12) {
                0 => "fmul",
                1 => "fdiv",
                2 => "fadd",
                3 => "fsub",
                4 => "fmax",
                5 => "fmin",
                6 => "fmaxnm",
                7 => "fminnm",
                8 => "fnmul",
                _ => return Err(Why::Other),
            };
            mn(i, name);
            i.ops = [f(rd, ty), f(rn, ty), f(rm, ty), none, none];
        }
        3 => {
            mn(i, "fcsel");
            i.ops = [f(rd, ty), f(rn, ty), f(rm, ty), Opd::Cond(bits(raw, 15, 12) as u8), none];
        }
        _ => return Err(Why::Other),
    }
    Ok(())
}
