//! A small AArch64 assembler for patching: the instructions a hand patch needs, written the way the printer
//! (`text.rs`) prints them, so text copied from `nexo bin fn` assembles back. Every instruction is one
//! little-endian 32-bit word. Anything that does not fit in exactly one word is refused with its reason.

use crate::{Error, Result};

type Res<T> = std::result::Result<T, String>;

/// Assemble `text` (instructions separated by `;` or newlines) placed at `at`. PC-relative targets are absolute
/// addresses, resolved against `at + 4 * index`.
pub(crate) fn assemble(text: &str, at: u64) -> Result<Vec<u8>> {
    if !at.is_multiple_of(4) {
        return Err(Error::Bad(format!("{at:#x} is not 4-aligned: AArch64 instructions start on multiples of 4")));
    }
    let mut out = Vec::new();
    let mut index = 0u64;
    for part in text.split([';', '\n']) {
        let line = part.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let pc = at.wrapping_add(4 * index);
        let word = one(line, pc).map_err(|m| Error::Bad(format!("cannot assemble `{line}`: {m}")))?;
        out.extend_from_slice(&word.to_le_bytes());
        index += 1;
    }
    if out.is_empty() {
        return Err(Error::Bad("no instruction to assemble".into()));
    }
    Ok(out)
}

/// A parsed register: number, 64-bit, written as `sp`/`wsp`, written as `xzr`/`wzr`.
#[derive(Clone, Copy)]
struct Rg {
    n: u32,
    x: bool,
    sp: bool,
    zr: bool,
}

fn rg(n: u32, x: bool, sp: bool, zr: bool) -> Rg {
    Rg { n, x, sp, zr }
}

fn reg(s: &str) -> Res<Rg> {
    let l = s.trim().to_ascii_lowercase();
    match l.as_str() {
        "sp" => return Ok(rg(31, true, true, false)),
        "wsp" => return Ok(rg(31, false, true, false)),
        "xzr" => return Ok(rg(31, true, false, true)),
        "wzr" => return Ok(rg(31, false, false, true)),
        "fp" => return Ok(rg(29, true, false, false)),
        "lr" => return Ok(rg(30, true, false, false)),
        _ => {}
    }
    let (x, digits) = if let Some(d) = l.strip_prefix('x') {
        (true, d)
    } else if let Some(d) = l.strip_prefix('w') {
        (false, d)
    } else {
        return Err(format!("`{s}` is not a register"));
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("`{s}` is not a register"));
    }
    match digits.parse::<u32>() {
        Ok(n) if n <= 30 => Ok(rg(n, x, false, false)),
        _ => Err(format!("`{s}` is not a register (x0 to x30)")),
    }
}

/// The register number, refusing `sp`.
fn no_sp(r: Rg) -> Res<u32> {
    if r.sp {
        Err("sp is not allowed here".into())
    } else {
        Ok(r.n)
    }
}

/// The register number, refusing `xzr`/`wzr` (the slot means `sp` at 31).
fn no_zr(r: Rg) -> Res<u32> {
    if r.zr {
        Err("xzr/wzr is not allowed here (register 31 means sp in this slot)".into())
    } else {
        Ok(r.n)
    }
}

/// A 64-bit register that is not `sp`.
fn xreg(s: &str) -> Res<u32> {
    let r = reg(s)?;
    if !r.x {
        return Err(format!("`{s}` must be a 64-bit register"));
    }
    no_sp(r)
}

/// A number: `#0x10`, `16`, `#-0x10`, `-16`.
fn num(s: &str) -> Res<i128> {
    let t = s.trim();
    let t = t.strip_prefix('#').unwrap_or(t).trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let hex = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"));
    let mag = match hex {
        Some(h) if !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()) => u64::from_str_radix(h, 16).ok(),
        Some(_) => None,
        None if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) => body.parse::<u64>().ok(),
        None => None,
    };
    match mag {
        Some(m) if neg => Ok(-i128::from(m)),
        Some(m) => Ok(i128::from(m)),
        None => Err(format!("`{s}` is not a number")),
    }
}

fn is_imm(s: &str) -> bool {
    s.bytes().next().is_some_and(|b| b == b'#' || b == b'-' || b == b'+' || b.is_ascii_digit())
}

/// `lsl #12`, `asr #3`: (kind 0 lsl, 1 lsr, 2 asr, 3 ror; amount 0 to 63).
fn shift(t: &str) -> Res<(u32, u32)> {
    let mut it = t.split_whitespace();
    let name = it.next().unwrap_or("").to_ascii_lowercase();
    let kind = match name.as_str() {
        "lsl" => 0,
        "lsr" => 1,
        "asr" => 2,
        "ror" => 3,
        _ => return Err(format!("`{t}` is not a shift (lsl, lsr, asr, ror) with an amount")),
    };
    let (Some(amt), None) = (it.next(), it.next()) else {
        return Err(format!("`{t}` needs exactly an amount: `{name} #n`"));
    };
    let a = num(amt)?;
    if !(0..=63).contains(&a) {
        return Err(format!("shift amount {a} is out of range"));
    }
    Ok((kind, a as u32))
}

fn arity(ops: &[&str], lo: usize, hi: usize) -> Res<()> {
    if (lo..=hi).contains(&ops.len()) {
        Ok(())
    } else if lo == hi {
        Err(format!("expects {lo} operand(s), got {}", ops.len()))
    } else {
        Err(format!("expects {lo} to {hi} operands, got {}", ops.len()))
    }
}

/// Split at commas outside brackets: `x0, [x1, #8]!` gives two operands.
fn split_ops(s: &str) -> Res<Vec<&str>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        if depth < 0 {
            return Err("unbalanced brackets".into());
        }
    }
    if depth != 0 {
        return Err("unbalanced brackets".into());
    }
    out.push(s[start..].trim());
    if out.iter().any(|p| p.is_empty()) {
        return Err("empty operand".into());
    }
    Ok(out)
}

/// The byte distance from `pc` to an absolute target address.
fn target(s: &str, pc: u64) -> Res<i128> {
    let a = num(s)?;
    if !(0..=i128::from(u64::MAX)).contains(&a) {
        return Err(format!("`{s}` is not an address"));
    }
    Ok(a - i128::from(pc))
}

/// A signed `width`-bit word offset for a byte distance.
fn word_off(delta: i128, width: u32) -> Res<u32> {
    if delta % 4 != 0 {
        return Err(format!("the target is {delta} bytes away, not a multiple of 4"));
    }
    let q = delta / 4;
    let lim = 1i128 << (width - 1);
    if !(-lim..lim).contains(&q) {
        return Err(format!("the target is {delta} bytes away; this form reaches {} bytes either way", lim * 4));
    }
    Ok((q & ((1i128 << width) - 1)) as u32)
}

fn cond(c: &str) -> Res<u32> {
    const NAMES: [&str; 16] =
        ["eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al", "nv"];
    match c {
        "cs" => Ok(2),
        "cc" => Ok(3),
        _ => NAMES.iter().position(|n| *n == c).map(|p| p as u32).ok_or_else(|| format!("`{c}` is not a condition")),
    }
}

/// The bit pattern of an immediate for a 32- or 64-bit register (negative numbers as two's complement).
fn pattern(v: i128, sf: bool) -> Res<u64> {
    let (lo, hi, mask) = if sf {
        (-(1i128 << 63), i128::from(u64::MAX), u64::MAX)
    } else {
        (-(1i128 << 31), i128::from(u32::MAX), u64::from(u32::MAX))
    };
    if !(lo..=hi).contains(&v) {
        return Err(format!("{v} does not fit a {}-bit register", if sf { 64 } else { 32 }));
    }
    Ok((v as u64) & mask)
}

/// The (N, immr, imms) of a logical immediate, or None when the value is not a rotated run of ones repeated.
fn bitmask(v: u64, sf: bool) -> Option<(u32, u32, u32)> {
    let size: u32 = if sf { 64 } else { 32 };
    let mask = if sf { u64::MAX } else { u64::from(u32::MAX) };
    let v = v & mask;
    if v == 0 || v == mask {
        return None;
    }
    let mut e = size;
    while e > 2 {
        let h = e / 2;
        let m = (1u64 << h) - 1;
        if (v & m) == ((v >> h) & m) {
            e = h;
        } else {
            break;
        }
    }
    let emask = if e == 64 { u64::MAX } else { (1u64 << e) - 1 };
    let elem = v & emask;
    let ones = elem.count_ones();
    if ones == 0 || ones >= e {
        return None;
    }
    let run = (1u64 << ones) - 1;
    for r in 0..e {
        let rot = if r == 0 { run } else { ((run >> r) | (run << (e - r))) & emask };
        if rot == elem {
            let imms = (0u32.wrapping_sub(2 * e) & 0x3f) | (ones - 1);
            return Some((u32::from(e == 64), r, imms));
        }
    }
    None
}

fn one(line: &str, pc: u64) -> Res<u32> {
    let (mn, rest) = match line.split_once(char::is_whitespace) {
        Some((m, r)) => (m, r.trim()),
        None => (line, ""),
    };
    let mn = mn.to_ascii_lowercase();
    let ops = split_ops(rest)?;
    let ops = ops.as_slice();
    if let Some(c) = mn.strip_prefix("b.") {
        arity(ops, 1, 1)?;
        let c = cond(c)?;
        let off = word_off(target(ops[0], pc)?, 19)?;
        return Ok(0x5400_0000 | (off << 5) | c);
    }
    match mn.as_str() {
        "nop" => {
            arity(ops, 0, 0)?;
            Ok(0xd503_201f)
        }
        "brk" | "svc" => {
            arity(ops, 1, 1)?;
            let v = num(ops[0])?;
            if !(0..=0xffff).contains(&v) {
                return Err(format!("{v} does not fit 16 bits"));
            }
            let base = if mn == "brk" { 0xd420_0000 } else { 0xd400_0001 };
            Ok(base | ((v as u32) << 5))
        }
        "ret" => {
            arity(ops, 0, 1)?;
            let r = if ops.is_empty() { 30 } else { xreg(ops[0])? };
            Ok(0xd65f_0000 | (r << 5))
        }
        "br" | "blr" => {
            arity(ops, 1, 1)?;
            let r = xreg(ops[0])?;
            Ok((if mn == "br" { 0xd61f_0000 } else { 0xd63f_0000 }) | (r << 5))
        }
        "b" | "bl" => {
            arity(ops, 1, 1)?;
            let off = word_off(target(ops[0], pc)?, 26)?;
            Ok((if mn == "b" { 0x1400_0000 } else { 0x9400_0000 }) | off)
        }
        "cbz" | "cbnz" => {
            arity(ops, 2, 2)?;
            let rt = reg(ops[0])?;
            let n = no_sp(rt)?;
            let off = word_off(target(ops[1], pc)?, 19)?;
            let nz = u32::from(mn == "cbnz");
            Ok(0x3400_0000 | (u32::from(rt.x) << 31) | (nz << 24) | (off << 5) | n)
        }
        "tbz" | "tbnz" => {
            arity(ops, 3, 3)?;
            let rt = reg(ops[0])?;
            let n = no_sp(rt)?;
            let bit = num(ops[1])?;
            if !(0..=63).contains(&bit) {
                return Err(format!("bit {bit} is out of range (0 to 63)"));
            }
            let bit = bit as u32;
            if bit >= 32 && !rt.x {
                return Err(format!("bit {bit} needs an x register"));
            }
            let off = word_off(target(ops[2], pc)?, 14)?;
            let nz = u32::from(mn == "tbnz");
            Ok(0x3600_0000 | ((bit >> 5) << 31) | (nz << 24) | ((bit & 31) << 19) | (off << 5) | n)
        }
        "adr" | "adrp" => {
            arity(ops, 2, 2)?;
            let rd = reg(ops[0])?;
            if !rd.x {
                return Err("the destination must be an x register".into());
            }
            let n = no_sp(rd)?;
            let a = num(ops[1])?;
            if !(0..=i128::from(u64::MAX)).contains(&a) {
                return Err(format!("`{}` is not an address", ops[1]));
            }
            let (delta, base) = if mn == "adr" {
                (a - i128::from(pc), 0x1000_0000u32)
            } else {
                if a % 0x1000 != 0 {
                    return Err(format!("{a:#x} is not a page address (adrp takes a multiple of 0x1000)"));
                }
                ((a - i128::from(pc & !0xfff)) / 0x1000, 0x9000_0000u32)
            };
            let lim = 1i128 << 20;
            if !(-lim..lim).contains(&delta) {
                return Err(format!(
                    "the target is out of range ({delta} {})",
                    if mn == "adr" { "bytes" } else { "pages" }
                ));
            }
            let imm = (delta & 0x1f_ffff) as u32;
            Ok(base | ((imm & 3) << 29) | ((imm >> 2) << 5) | n)
        }
        "mov" => mov(ops),
        "movz" | "movn" | "movk" => movewide(&mn, ops),
        "add" | "adds" | "sub" | "subs" | "cmp" | "cmn" => addsub(&mn, ops),
        "and" | "orr" | "eor" | "ands" | "tst" => logic(&mn, ops),
        "ldr" | "str" | "ldrb" | "strb" | "ldrh" | "strh" | "ldrsw" => single(&mn, ops, pc),
        "ldp" | "stp" => pair(&mn, ops),
        _ => Err(
            "unknown mnemonic (supported: nop ret br blr b bl b.cond cbz cbnz tbz tbnz adr adrp mov movz movn movk \
                  add adds sub subs cmp cmn and orr eor ands tst ldr str ldrb strb ldrh strh ldrsw ldp stp brk svc)"
                .into(),
        ),
    }
}

fn mov(ops: &[&str]) -> Res<u32> {
    arity(ops, 2, 2)?;
    let rd = reg(ops[0])?;
    let top = u32::from(rd.x) << 31;
    if is_imm(ops[1]) {
        return mov_imm(rd, pattern(num(ops[1])?, rd.x)?);
    }
    let rs = reg(ops[1])?;
    if rs.x != rd.x {
        return Err("the two registers differ in size".into());
    }
    if rd.sp || rs.sp {
        // add rd, rs, #0
        let (d, s) = (no_zr(rd)?, no_zr(rs)?);
        return Ok(0x1100_0000 | top | (s << 5) | d);
    }
    Ok(0x2a00_0000 | top | (rs.n << 16) | (31 << 5) | rd.n)
}

/// `mov rd, #v` as one word: movz, movn, or orr with a bitmask immediate.
fn mov_imm(rd: Rg, v: u64) -> Res<u32> {
    let sf = rd.x;
    let top = u32::from(sf) << 31;
    let mask = if sf { u64::MAX } else { u64::from(u32::MAX) };
    if !rd.sp {
        for (base, val) in [(0x5280_0000u32, v), (0x1280_0000u32, !v & mask)] {
            for hw in 0..(if sf { 4u32 } else { 2 }) {
                if val & !(0xffffu64 << (16 * hw)) == 0 {
                    let imm = ((val >> (16 * hw)) & 0xffff) as u32;
                    return Ok(top | base | (hw << 21) | (imm << 5) | rd.n);
                }
            }
        }
    }
    if rd.zr {
        return Err(format!("{v:#x} needs an orr immediate, which cannot write xzr/wzr"));
    }
    match bitmask(v, sf) {
        Some((n, immr, imms)) => Ok(0x3200_0000 | top | (n << 22) | (immr << 16) | (imms << 10) | (31 << 5) | rd.n),
        None => {
            Err(format!("{v:#x} cannot be made by one instruction (movz, movn or a bitmask orr); use movz and movk"))
        }
    }
}

fn movewide(mn: &str, ops: &[&str]) -> Res<u32> {
    arity(ops, 2, 3)?;
    let rd = reg(ops[0])?;
    let n = no_sp(rd)?;
    let imm = num(ops[1])?;
    if !(0..=0xffff).contains(&imm) {
        return Err(format!("{imm} does not fit 16 bits"));
    }
    let hw = match ops.get(2) {
        None => 0,
        Some(t) => {
            let (kind, amt) = shift(t)?;
            if kind != 0 || !amt.is_multiple_of(16) || amt > if rd.x { 48 } else { 16 } {
                return Err(format!("`{t}` is not allowed: use lsl #0, #16{}", if rd.x { ", #32 or #48" } else { "" }));
            }
            amt / 16
        }
    };
    let base = match mn {
        "movz" => 0x5280_0000u32,
        "movn" => 0x1280_0000,
        _ => 0x7280_0000,
    };
    Ok(base | (u32::from(rd.x) << 31) | (hw << 21) | ((imm as u32) << 5) | n)
}

fn addsub(mn: &str, ops: &[&str]) -> Res<u32> {
    let (sub, s, cmp) = match mn {
        "add" => (0u32, 0u32, false),
        "adds" => (0, 1, false),
        "sub" => (1, 0, false),
        "subs" => (1, 1, false),
        "cmp" => (1, 1, true),
        _ => (0, 1, true),
    };
    let (rd, rn, op2, sh) = if cmp {
        arity(ops, 2, 3)?;
        let rn = reg(ops[0])?;
        (rg(31, rn.x, false, true), rn, ops[1], ops.get(2).copied())
    } else {
        arity(ops, 3, 4)?;
        (reg(ops[0])?, reg(ops[1])?, ops[2], ops.get(3).copied())
    };
    let sf = rd.x;
    if rn.x != sf {
        return Err("the registers differ in size".into());
    }
    let base = (u32::from(sf) << 31) | (sub << 30) | (s << 29);
    if is_imm(op2) {
        let d = if s == 1 { no_sp(rd)? } else { no_zr(rd)? };
        let n = no_zr(rn)?;
        let imm = num(op2)?;
        if !(0..=0xfff).contains(&imm) {
            return Err(format!("immediate {imm} does not fit 12 bits (write `#n, lsl #12` for multiples of 0x1000)"));
        }
        let sh12 = match sh {
            None => 0,
            Some(t) => match shift(t)? {
                (0, 0) => 0,
                (0, 12) => 1,
                _ => return Err(format!("`{t}` is not allowed: only lsl #0 or lsl #12")),
            },
        };
        return Ok(0x1100_0000 | base | (sh12 << 22) | ((imm as u32) << 10) | (n << 5) | d);
    }
    let rm = reg(op2)?;
    if rm.x != sf {
        return Err("the registers differ in size".into());
    }
    let m = no_sp(rm)?;
    if rd.sp || rn.sp {
        // extended register: the only form that reads or writes sp
        let d = if s == 1 { no_sp(rd)? } else { no_zr(rd)? };
        let n = no_zr(rn)?;
        let amt = match sh {
            None => 0,
            Some(t) => match shift(t)? {
                (0, a) if a <= 4 => a,
                _ => return Err(format!("`{t}` is not allowed with sp: only lsl #0 to #4")),
            },
        };
        let option = if sf { 3 } else { 2 };
        return Ok(0x0b20_0000 | base | (m << 16) | (option << 13) | (amt << 10) | (n << 5) | d);
    }
    let (kind, amt) = match sh {
        None => (0, 0),
        Some(t) => shift(t)?,
    };
    if kind == 3 {
        return Err("ror is not allowed in add and sub".into());
    }
    if amt >= if sf { 64 } else { 32 } {
        return Err(format!("shift amount {amt} is too big for the register size"));
    }
    let (d, n) = (no_sp(rd)?, no_sp(rn)?);
    Ok(0x0b00_0000 | base | (kind << 22) | (m << 16) | (amt << 10) | (n << 5) | d)
}

fn logic(mn: &str, ops: &[&str]) -> Res<u32> {
    let (opc, tst) = match mn {
        "and" => (0u32, false),
        "orr" => (1, false),
        "eor" => (2, false),
        "ands" => (3, false),
        _ => (3, true),
    };
    let (rd, rn, op2, sh) = if tst {
        arity(ops, 2, 3)?;
        let rn = reg(ops[0])?;
        (rg(31, rn.x, false, true), rn, ops[1], ops.get(2).copied())
    } else {
        arity(ops, 3, 4)?;
        (reg(ops[0])?, reg(ops[1])?, ops[2], ops.get(3).copied())
    };
    let sf = rd.x;
    if rn.x != sf {
        return Err("the registers differ in size".into());
    }
    let top = u32::from(sf) << 31;
    let n = no_sp(rn)?;
    if is_imm(op2) {
        if sh.is_some() {
            return Err("an immediate takes no shift".into());
        }
        let d = if opc == 3 { no_sp(rd)? } else { no_zr(rd)? };
        let v = pattern(num(op2)?, sf)?;
        let Some((nb, immr, imms)) = bitmask(v, sf) else {
            return Err(format!("{v:#x} is not an AArch64 bitmask immediate"));
        };
        return Ok(0x1200_0000 | top | (opc << 29) | (nb << 22) | (immr << 16) | (imms << 10) | (n << 5) | d);
    }
    let rm = reg(op2)?;
    if rm.x != sf {
        return Err("the registers differ in size".into());
    }
    let m = no_sp(rm)?;
    let d = no_sp(rd)?;
    let (kind, amt) = match sh {
        None => (0, 0),
        Some(t) => shift(t)?,
    };
    if amt >= if sf { 64 } else { 32 } {
        return Err(format!("shift amount {amt} is too big for the register size"));
    }
    Ok(0x0a00_0000 | top | (opc << 29) | (kind << 22) | (m << 16) | (amt << 10) | (n << 5) | d)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Idx {
    Off,
    Pre,
    Post,
}

/// `[xN]`, `[xN, #imm]`, `[xN, #imm]!` or (with the next operand) `[xN], #imm`: base, kind, offset.
fn address(rest: &[&str]) -> Res<(u32, Idx, i128)> {
    arity(rest, 1, 2)?;
    let m = rest[0];
    let Some(close) = m.find(']').filter(|_| m.starts_with('[')) else {
        return Err(format!("`{m}` is not a memory operand like [x0, #8]"));
    };
    let inner = &m[1..close];
    let tail = m[close + 1..].trim();
    let mut parts = inner.split(',').map(str::trim);
    let base = reg(parts.next().unwrap_or(""))?;
    if !base.x || base.zr {
        return Err("the base must be an x register or sp".into());
    }
    let off = match parts.next() {
        None => None,
        Some(p) if is_imm(p) => Some(num(p)?),
        Some(p) => return Err(format!("`{p}`: only an immediate offset is supported")),
    };
    if parts.next().is_some() {
        return Err("only [xN, #imm] is supported".into());
    }
    match (tail, rest.get(1)) {
        ("", None) => Ok((base.n, Idx::Off, off.unwrap_or(0))),
        ("!", None) => Ok((base.n, Idx::Pre, off.unwrap_or(0))),
        ("", Some(p)) => {
            if off.is_some() {
                return Err("the post-index form is [xN], #imm".into());
            }
            Ok((base.n, Idx::Post, num(p)?))
        }
        _ => Err(format!("`{m}` is not a supported memory operand")),
    }
}

fn single(mn: &str, ops: &[&str], pc: u64) -> Res<u32> {
    arity(ops, 2, 3)?;
    let rt = reg(ops[0])?;
    let t = no_sp(rt)?;
    let load = mn.starts_with("ld");
    let (size, opc) = match mn {
        "ldr" | "str" => (if rt.x { 3u32 } else { 2 }, u32::from(load)),
        "ldrb" | "strb" | "ldrh" | "strh" => {
            if rt.x {
                return Err("the register must be a w register".into());
            }
            (u32::from(mn.contains('h')), u32::from(load))
        }
        _ => {
            if !rt.x {
                return Err("ldrsw needs an x register".into());
            }
            (2, 2)
        }
    };
    if load && matches!(mn, "ldr" | "ldrsw") && !ops[1].starts_with('[') {
        arity(ops, 2, 2)?;
        let off = word_off(target(ops[1], pc)?, 19)?;
        let lop = if mn == "ldrsw" { 2 } else { u32::from(rt.x) };
        return Ok(0x1800_0000 | (lop << 30) | (off << 5) | t);
    }
    let (base, idx, off) = address(&ops[1..])?;
    if idx == Idx::Off {
        let bytes = 1i128 << size;
        if off < 0 || off % bytes != 0 {
            return Err(format!(
                "offset {off} must be a non-negative multiple of {bytes} (negative or unaligned offsets need ldur/stur, \
                 which this assembler does not do)"
            ));
        }
        let q = off / bytes;
        if q > 4095 {
            return Err(format!("offset {off} is too big (at most {})", 4095 * bytes));
        }
        return Ok(0x3900_0000 | (size << 30) | (opc << 22) | ((q as u32) << 10) | (base << 5) | t);
    }
    if !(-256..=255).contains(&off) {
        return Err(format!("offset {off} is out of range (-256 to 255)"));
    }
    if base != 31 && base == t {
        return Err("the base register is also the transfer register: unpredictable".into());
    }
    let kind = if idx == Idx::Pre { 3 } else { 1 };
    Ok(0x3800_0000 | (size << 30) | (opc << 22) | (((off as u32) & 0x1ff) << 12) | (kind << 10) | (base << 5) | t)
}

fn pair(mn: &str, ops: &[&str]) -> Res<u32> {
    arity(ops, 3, 4)?;
    let (a, b) = (reg(ops[0])?, reg(ops[1])?);
    let (t, t2) = (no_sp(a)?, no_sp(b)?);
    if a.x != b.x {
        return Err("the two registers differ in size".into());
    }
    let load = mn == "ldp";
    if load && t == t2 {
        return Err("a load pair into the same register twice is unpredictable".into());
    }
    let (base, idx, off) = address(&ops[2..])?;
    let scale = if a.x { 3u32 } else { 2 };
    let bytes = 1i128 << scale;
    if off % bytes != 0 {
        return Err(format!("offset {off} must be a multiple of {bytes}"));
    }
    let q = off / bytes;
    if !(-64..=63).contains(&q) {
        return Err(format!("offset {off} is out of range ({} to {})", -64 * bytes, 63 * bytes));
    }
    if idx != Idx::Off && base != 31 && (base == t || base == t2) {
        return Err("the base register is also a transfer register: unpredictable".into());
    }
    let ty = match idx {
        Idx::Post => 1u32,
        Idx::Off => 2,
        Idx::Pre => 3,
    };
    let opc = if a.x { 2u32 } else { 0 };
    Ok(0x2800_0000
        | (opc << 30)
        | (ty << 23)
        | (u32::from(load) << 22)
        | (((q as u32) & 0x7f) << 15)
        | (t2 << 10)
        | (base << 5)
        | t)
}
