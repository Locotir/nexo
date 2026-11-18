//! Switch jump tables on AArch64: the bound check, the table read and the add that makes the `br` target, found
//! by looking back from a `br xN` and read from the image. Both compilers' shapes are covered:
//!
//! ```text
//! clang  cmp wI, #N ; b.hi default ; adrp xT, JT ; add xT, xT, :lo12:JT ; adr xB, base
//!        ldrb|ldrh wE, [xT, xI{, lsl #s}] ; add xD, xB, xE, lsl #2 ; br xD
//!        ldrsw xE, [xT, xI, lsl #2] ; add xD, xB, xE ; br xD           (32-bit entries, any base)
//! gcc    cmp wI, #N ; b.hi default ; adrp xT, JT ; add xT, xT, :lo12:JT
//!        ldrb|ldrh wE, [xT, wI, uxtw{ #s}] ; adr xB, .Lrtx ; add xD, xB, wE, sxtb|uxtb|sxth #2 ; br xD
//! ```
//!
//! The entry is widened as the load says (zero or sign), then as the final `add` says (shift or extend), and added
//! to the base. Anything that does not read exactly like this is not a switch here: the `br` stays an unresolved
//! indirect jump, as before.

use super::{sext, Flow, Ins, Mem, Opd};

/// Cases read from one table at most.
const MAX_CASES: u64 = 4096;
/// How many instructions back from the `br` the pieces are looked for.
const WINDOW: usize = 32;

/// A recovered switch.
pub(crate) struct Table {
    /// The target of case `0`, `1`, ... (the bound check's `N + 1` of them).
    pub targets: Vec<u64>,
    /// The index register (number) and whether it is the 64-bit view.
    pub idx: u8,
    pub wide: bool,
    /// Instruction numbers (after the bound branch) that only compute the jump: the lifter skips them.
    pub hidden: Vec<usize>,
}

fn sets_flags(m: &str) -> bool {
    matches!(
        m,
        "cmp" | "cmn" | "tst" | "subs" | "adds" | "ands" | "bics" | "adcs" | "sbcs" | "negs" | "ngcs" | "ccmp" | "ccmn"
    )
}

/// The nearest instruction before `before` (not further back than `lo`) that writes `reg`, when nothing that
/// leaves straight-line code lies between them but the bound branch `guard`.
fn last_writer(ins: &[Ins], reg: u8, before: usize, lo: usize, guard: usize) -> Option<usize> {
    if reg >= 31 {
        return None;
    }
    let mut j = before;
    while j > lo {
        j -= 1;
        let i = &ins[j];
        if !i.is_decoded() || (i.flow != Flow::Seq && j != guard) {
            return None;
        }
        if (i.writes >> u32::from(reg)) & 1 == 1 {
            return Some(j);
        }
    }
    None
}

/// The address `reg` holds before instruction `before`, when it comes from `adrp`, `adr` or an `add` of an
/// immediate to such a register. The instructions that built it are added to `used`.
fn address(ins: &[Ins], reg: u8, before: usize, lo: usize, guard: usize, used: &mut Vec<usize>) -> Option<u64> {
    let j = last_writer(ins, reg, before, lo, guard)?;
    let i = &ins[j];
    if let Some((r, a)) = i.adrp.or(i.adr) {
        if r == reg {
            used.push(j);
            return Some(a);
        }
    }
    if let Some((rd, rn, imm)) = i.addimm {
        if rd == reg {
            let base = address(ins, rn, j, lo, guard, used)?;
            used.push(j);
            return Some(base.wrapping_add(imm));
        }
    }
    None
}

/// The switch that the `br` at instruction `k` dispatches through, when it reads like one. `start..end` is the
/// function (every target must be an instruction of it); `read` gives bytes of the image.
pub(crate) fn find(
    ins: &[Ins],
    k: usize,
    start: u64,
    end: u64,
    read: &dyn Fn(u64, usize) -> Option<Vec<u8>>,
) -> Option<Table> {
    let br = ins.get(k)?;
    let Opd::Gpr { n: d, .. } = br.ops[0] else { return None };
    if br.flow != Flow::JumpReg {
        return None;
    }
    let lo = k.saturating_sub(WINDOW);
    // the bound check: `cmp wI, #N ; b.hi default` (or `b.hs` with N + 1), the nearest branch before the `br`
    let guard = (lo..k).rev().find(|&j| ins[j].flow != Flow::Seq)?;
    let g = &ins[guard];
    if g.flow != Flow::Cond || g.mn.as_str() != "b" || !matches!(g.cc, 2 | 8) {
        return None;
    }
    let mut cmp = None;
    for j in (lo..guard).rev().take(6) {
        let m = ins[j].mn.as_str();
        if m == "cmp" {
            cmp = Some(j);
            break;
        }
        if sets_flags(m) || ins[j].flow != Flow::Seq {
            return None;
        }
    }
    let cmp = cmp?;
    let (Opd::Gpr { n: idx, x: wide, .. }, Opd::Imm(bound), Opd::None) =
        (ins[cmp].ops[0], ins[cmp].ops[1], ins[cmp].ops[2])
    else {
        return None;
    };
    let bound = u64::try_from(bound).ok()?;
    let count = if g.cc == 8 { bound + 1 } else { bound };
    if idx >= 31 || count == 0 || count > MAX_CASES {
        return None;
    }
    // the last add: `add xD, xB, xE{, lsl #s | , ext #s}`
    let a = last_writer(ins, d, k, lo, guard)?;
    let add = &ins[a];
    if add.mn.as_str() != "add" {
        return None;
    }
    let (Opd::Gpr { x: true, .. }, Opd::Gpr { n: b, .. }, Opd::Gpr { n: e, .. }) = (add.ops[0], add.ops[1], add.ops[2])
    else {
        return None;
    };
    let (ebits, esigned, eshift): (u32, bool, u32) = match add.ops[3] {
        Opd::None => (64, false, 0),
        Opd::Shift { kind: 0, amt } => (64, false, u32::from(amt)),
        Opd::Ext { kind, amt, .. } if kind < 8 => ([8, 16, 32, 64][usize::from(kind & 3)], kind >= 4, u32::from(amt)),
        Opd::Ext { kind: 8, amt, .. } => (64, false, u32::from(amt)),
        _ => return None,
    };
    // the entry load: `ldrb|ldrh|ldr w|ldrsb|ldrsh|ldrsw xE, [xT, xI{, ext #s}]`
    let l = last_writer(ins, e, a, lo, guard)?;
    let ld = &ins[l];
    let Opd::Gpr { n: ld_dest, x: dest_wide, .. } = ld.ops[0] else { return None };
    let Opd::Mem(Mem::Reg { base: t, idx: li, shift, .. }) = ld.ops[1] else { return None };
    let (width, signed): (usize, bool) = match (ld.mn.as_str(), dest_wide) {
        ("ldrb", _) => (1, false),
        ("ldrh", _) => (2, false),
        ("ldr", false) => (4, false),
        ("ldrsb", _) => (1, true),
        ("ldrsh", _) => (2, true),
        ("ldrsw", true) => (4, true),
        _ => return None,
    };
    if ld_dest != e || shift > 3 || l <= guard {
        return None;
    }
    let mut hidden = vec![a, l];
    // the load's index is the compared register, or a plain copy of it (`mov w8, w0`)
    if li != idx {
        let m = last_writer(ins, li, l, lo, guard)?;
        match (ins[m].mn.as_str(), ins[m].ops[0], ins[m].ops[1]) {
            ("mov", Opd::Gpr { n, .. }, Opd::Gpr { n: src, .. }) if n == li && src == idx && m > guard => {
                hidden.push(m)
            }
            _ => return None,
        }
    }
    // the index must reach the load as compared (the load itself may overwrite it: gcc reuses the register)
    if ins[cmp + 1..l].iter().any(|i| (i.writes >> u32::from(idx)) & 1 == 1) {
        return None;
    }
    let table = address(ins, t, l, lo, guard, &mut hidden)?;
    let base = address(ins, b, a, lo, guard, &mut hidden)?;
    let stride = 1u64 << shift;
    let mut targets = Vec::with_capacity(count as usize);
    for n in 0..count {
        let raw = read(table.wrapping_add(n * stride), width)?;
        if raw.len() < width {
            return None;
        }
        let mut buf = [0u8; 8];
        buf[..width].copy_from_slice(&raw[..width]);
        let mut val = u64::from_le_bytes(buf);
        if signed {
            val = sext(val, (8 * width) as u32) as u64;
            if !dest_wide {
                val &= 0xffff_ffff;
            }
        }
        let wide_val = if ebits >= 64 {
            val
        } else {
            let low = val & ((1u64 << ebits) - 1);
            if esigned {
                sext(low, ebits) as u64
            } else {
                low
            }
        };
        let t = base.wrapping_add(wide_val.wrapping_shl(eshift));
        if !(start..end).contains(&t) || !(t - start).is_multiple_of(4) {
            return None;
        }
        targets.push(t);
    }
    hidden.retain(|&j| j > guard);
    hidden.sort_unstable();
    hidden.dedup();
    Some(Table { targets, idx, wide, hidden })
}
