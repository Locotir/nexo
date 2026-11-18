//! A backward slice on AArch64: which instructions of one function produced a value at an instruction.
//!
//! The same question and the same answer as `crate::slice` (x86-64), with the AArch64 counterparts:
//! registers are x0..x30 (a `w` register is the low half of its `x` register, and a write to `w` replaces the
//! whole `x`), `sp` and `x29` are the bases of stack slots, a call (`bl`, `blr`) returns in x0, changes x1..x18
//! and x30 and keeps x19..x29 (AAPCS64), arguments are x0..x7, a global comes from an `adrp` pair worked out by
//! [`super::scan::Tracker`], and `movz`/`movk` chains are followed through the register they fill.
//!
//! What is not followed, and is said so: the flags, memory reached through a register that is not `sp`, `x29`
//! or an `adrp` pair, vector registers, atomics, and words the decoder does not read.

use std::collections::BTreeSet;

use serde_json::json;

use super::scan::Via;
use super::{view, Flow, Ins, Mem, Opd};
use crate::graph::{node_name, node_of};
use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::place::addr_in;
use crate::{Error, Image, Result};

pub(super) const SP: u8 = 31;
pub(super) const FP: u8 = 29;
pub(super) const LR: u8 = 30;
/// x0..x7 carry the first eight integer arguments.
pub(super) const ARG_REGS: u8 = 8;

/// A place a value lives.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(super) enum Loc {
    /// x0..x30.
    Reg(u8),
    /// `[base + disp]` with base sp (31) or x29, `size` bytes.
    Slot(u8, i64, u32),
    /// `size` bytes at a fixed address.
    Global(u64, u32),
}

#[derive(Clone, Copy)]
struct Want {
    loc: Loc,
    /// The instruction that needs it (a branch landing after the definition and before this one makes the step inferred).
    need: u64,
}

pub(super) fn overlaps(a: &Loc, b: &Loc) -> bool {
    match (a, b) {
        (Loc::Reg(x), Loc::Reg(y)) => x == y,
        (Loc::Slot(b1, d1, s1), Loc::Slot(b2, d2, s2)) => {
            b1 == b2 && *d1 < *d2 + i64::from(*s2) && *d2 < *d1 + i64::from(*s1)
        }
        (Loc::Global(x, s1), Loc::Global(y, s2)) => *x < *y + u64::from(*s2) && *y < *x + u64::from(*s1),
        _ => false,
    }
}

/// Whether a write of `store` replaces everything `held` stands for.
pub(super) fn covers(store: &Loc, held: &Loc) -> bool {
    match (store, held) {
        (Loc::Slot(b1, d1, s1), Loc::Slot(b2, d2, s2)) => {
            b1 == b2 && *d1 <= *d2 && *d2 + i64::from(*s2) <= *d1 + i64::from(*s1)
        }
        (Loc::Global(x, s1), Loc::Global(y, s2)) => *x <= *y && *y + u64::from(*s2) <= *x + u64::from(*s1),
        _ => false,
    }
}

pub(super) fn reg_name(n: u8) -> String {
    format!("x{n}")
}

fn base_name(b: u8) -> &'static str {
    if b == SP {
        "sp"
    } else {
        "x29"
    }
}

pub(super) fn loc_text(img: &Image, l: &Loc) -> String {
    match l {
        Loc::Reg(n) => reg_name(*n),
        Loc::Slot(b, d, _) => {
            if *d < 0 {
                format!("[{}-{:#x}]", base_name(*b), -d)
            } else {
                format!("[{}+{:#x}]", base_name(*b), d)
            }
        }
        Loc::Global(a, _) => {
            match img.name_at(*a).map(str::to_string).or_else(|| img.symbol_covering(*a).map(|(n, _)| n.to_string())) {
                Some(n) => format!("global {n} {a:#x}"),
                None => format!("global {a:#x}"),
            }
        }
    }
}

/// `x0`..`x30`, `w0`..`w30`, `fp`, `lr`.
pub(super) fn parse_reg(s: &str) -> Result<u8> {
    let t = s.trim().to_ascii_lowercase();
    match t.as_str() {
        "sp" | "wsp" => {
            return Err(Error::Bad(
                "sp is the stack pointer, not a value to follow; give --mem sp+0x10 for a slot".into(),
            ))
        }
        "xzr" | "wzr" => return Err(Error::Bad("the zero register always reads 0; there is nothing to follow".into())),
        "fp" => return Ok(FP),
        "lr" => return Ok(LR),
        _ => {}
    }
    let bad = || Error::Bad(format!("{s:?} is not a register name (x0..x30, w0..w30, fp, lr)"));
    let rest = t.strip_prefix('x').or_else(|| t.strip_prefix('w')).ok_or_else(bad)?;
    match rest.parse::<u8>() {
        Ok(n) if n <= 30 && !rest.starts_with('+') => Ok(n),
        _ => Err(bad()),
    }
}

/// `sp+0x10`, `x29-0x20`, `[sp, #16]`.
pub(super) fn parse_mem(s: &str) -> Result<Loc> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']').replace([' ', '#'], "");
    let bad = || Error::Bad(format!("{s:?} is not a stack slot; write sp+0x10 or x29-0x20"));
    let (name, off) = match t.find(['+', '-']) {
        Some(pos) => {
            let n = crate::parse_number(&t[pos + 1..]).map_err(|_| bad())? as i64;
            (&t[..pos], if t.as_bytes()[pos] == b'-' { -n } else { n })
        }
        None => (t.as_str(), 0),
    };
    let base = match name.to_ascii_lowercase().as_str() {
        "sp" => SP,
        "x29" | "fp" => FP,
        _ => return Err(bad()),
    };
    Ok(Loc::Slot(base, off, 8))
}

/// How an instruction moves sp.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Sp {
    #[default]
    Same,
    /// sp_after = sp_before + n.
    By(i64),
    Unknown,
}

/// One memory access of an instruction with a base register.
pub(super) struct Mref {
    pub(super) base: u8,
    /// Offset from the base as sp stood before the instruction, and after its write-back.
    pub(super) before: Option<i64>,
    pub(super) after: Option<i64>,
    /// 0 when the extent is not known (a vector structure access).
    pub(super) size: u32,
    /// The address when an `adrp` pair or a literal gives it.
    pub(super) via: Option<u64>,
    pub(super) store: bool,
}

/// Where an access lands: a global, or a slot of sp or x29. `after` picks the offset as sp stands after the instruction.
pub(super) fn resolve(m: &Mref, after: bool) -> Option<Loc> {
    if let Some(a) = m.via {
        return Some(Loc::Global(a, m.size.max(1)));
    }
    if m.size == 0 {
        return None;
    }
    match (m.base, if after { m.after } else { m.before }) {
        (SP | FP, Some(d)) => Some(Loc::Slot(m.base, d, m.size)),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Fx {
    /// x registers read (x29 left out: it is a base, like rbp on x86).
    pub(super) reads: Vec<u8>,
    /// x registers replaced.
    pub(super) defs: Vec<u8>,
    /// x registers changed from their old value (`movk`, a written-back base).
    pub(super) rmw: Vec<u8>,
    pub(super) refs: Vec<Mref>,
    pub(super) sp: Sp,
    /// Why the instruction cannot be followed at all.
    pub(super) opaque: Option<String>,
    pub(super) zeroing: bool,
    /// An address made from `adrp`, `adr` or an `add` on an `adrp` result.
    pub(super) addr: Option<u64>,
}

fn is_pair(mn: &str) -> bool {
    ["ldp", "stp", "ldnp", "stnp", "ldxp", "stxp", "ldaxp", "stlxp"].iter().any(|p| mn.starts_with(p))
}

fn is_atomic(mn: &str) -> bool {
    ["cas", "swp", "ldadd", "ldclr", "ldeor", "ldset", "ldsmax", "ldsmin", "ldumax", "ldumin"]
        .iter()
        .any(|p| mn.starts_with(p))
}

/// Bytes moved by each register of a load or store (0 for a vector list).
fn data_size(mn: &str, ops: &[Opd; 5]) -> u32 {
    let d = usize::from(mn.starts_with("stx") || mn.starts_with("stlx"));
    match ops[d] {
        Opd::Gpr { x, .. } => {
            if mn.ends_with('b') {
                1
            } else if mn.ends_with('h') {
                2
            } else if matches!(mn, "ldrsw" | "ldursw" | "ldtrsw" | "ldpsw") || !x {
                4
            } else {
                8
            }
        }
        Opd::Fp { sz, .. } => 1u32 << u32::from(sz.min(4)),
        _ => 0,
    }
}

fn mem_effects(fx: &mut Fx, ins: &Ins, mn: &str, m: Mem, via: Option<Via>) {
    let (base, before, wb, wb_unknown) = match m {
        Mem::Off { base, off } => (base, Some(off), None, false),
        Mem::Pre { base, off } => (base, Some(off), Some(off), false),
        Mem::Post { base, off } => (base, Some(0), Some(off), false),
        Mem::Reg { base, idx, .. } => {
            if idx < 31 {
                fx.reads.push(idx);
            }
            (base, None, None, false)
        }
        Mem::PostReg { base, idx } => {
            if idx < 31 {
                fx.reads.push(idx);
            }
            (base, None, None, true)
        }
    };
    let known = matches!(via, Some(Via::Read(_) | Via::Write(_)));
    if base < 31 && !known {
        fx.reads.push(base);
    }
    if wb.is_some() || wb_unknown {
        if base == SP {
            fx.sp = match wb {
                Some(d) => Sp::By(d),
                None => Sp::Unknown,
            };
        } else if base < 31 {
            fx.rmw.push(base);
            fx.reads.push(base);
        }
    }
    let (load, store) = (mn.starts_with("ld"), mn.starts_with("st"));
    if !load && !store {
        return;
    }
    let size = data_size(mn, &ins.ops);
    let count: u32 = if is_pair(mn) { 2 } else { 1 };
    let via_addr = match via {
        Some(Via::Read(a)) if load => Some(a),
        Some(Via::Write(a)) if store => Some(a),
        _ => None,
    };
    for e in 0..count {
        let k = i64::from(e) * i64::from(size);
        let mut b = before.map(|o| o + k);
        let mut a = b.map(|o| o - if base == SP { wb.unwrap_or(0) } else { 0 });
        if (base == FP && (wb.is_some() || wb_unknown)) || (base == SP && wb_unknown) {
            b = None;
            a = None;
        }
        fx.refs.push(Mref {
            base,
            before: b,
            after: a,
            size,
            via: via_addr.map(|v| v + u64::from(e) * u64::from(size)),
            store,
        });
    }
}

/// What one instruction reads, replaces and touches in memory.
pub(super) fn effects(ins: &Ins, via: Option<Via>) -> Fx {
    let mut fx = Fx::default();
    if !ins.is_decoded() {
        fx.opaque = Some("a word the decoder does not read: it may write anything".into());
        return fx;
    }
    let mn = ins.mn.as_str();
    if is_atomic(mn) {
        fx.opaque = Some("an atomic read-modify-write: not followed".into());
        return fx;
    }
    let writes = ins.writes & 0x7fff_ffff;
    let pair_load = ["ldp", "ldnp", "ldxp", "ldaxp"].iter().any(|p| mn.starts_with(p));
    let rmw_op = matches!(mn, "movk" | "bfi" | "bfxil" | "bfc" | "bfm");
    for (k, o) in ins.ops.iter().enumerate() {
        match *o {
            Opd::Gpr { n, .. } if n < 31 => {
                let dest = k == 0 || (k == 1 && pair_load);
                if dest && writes >> n & 1 == 1 {
                    if rmw_op {
                        fx.rmw.push(n);
                        fx.reads.push(n);
                    } else {
                        fx.defs.push(n);
                    }
                } else {
                    fx.reads.push(n);
                }
            }
            Opd::Mem(m) => mem_effects(&mut fx, ins, mn, m, via),
            Opd::List { .. } => {
                // a vector structure access: its extent is not known
            }
            _ => {}
        }
    }
    if let Some(l) = ins.literal {
        if mn.starts_with("ld") {
            fx.refs.push(Mref {
                base: 255,
                before: None,
                after: None,
                size: data_size(mn, &ins.ops),
                via: Some(l),
                store: false,
            });
        }
    }
    for n in 0..31u8 {
        if writes >> n & 1 == 1 && !fx.defs.contains(&n) && !fx.rmw.contains(&n) {
            fx.defs.push(n);
        }
    }
    // a write to sp that is not a memory write-back
    if matches!(ins.ops[0], Opd::Gpr { n: 31, sp: true, .. }) && !matches!(mn, "cmp" | "cmn") {
        let same = matches!(ins.ops[1], Opd::Gpr { n: 31, sp: true, .. });
        fx.sp = match (mn, ins.ops[2]) {
            ("add", Opd::Imm(v)) if same && ins.ops[3] == Opd::None => Sp::By(v),
            ("sub", Opd::Imm(v)) if same && ins.ops[3] == Opd::None => Sp::By(-v),
            _ => Sp::Unknown,
        };
    }
    fx.zeroing = matches!(mn, "eor" | "sub")
        && matches!(ins.ops[1], Opd::Gpr { n, .. } if n < 31)
        && ins.ops[1] == ins.ops[2]
        && ins.ops[3] == Opd::None;
    fx.addr = match via {
        Some(Via::Addr(a)) => Some(a),
        _ => ins.adrp.map(|(_, p)| p).or(ins.adr.map(|(_, a)| a)),
    };
    if fx.addr.is_some() || fx.zeroing {
        fx.reads.clear();
    }
    fx.reads.retain(|&n| n != FP);
    fx.reads.sort_unstable();
    fx.reads.dedup();
    fx
}

struct Step {
    ip: u64,
    text: String,
    why: String,
    certainty: &'static str,
}

struct Stop {
    at: u64,
    what: String,
    why: String,
    certainty: &'static str,
}

pub(super) fn call_name(img: &Image, ins: &Ins) -> String {
    match (ins.flow, ins.target) {
        (Flow::Call, Some(t)) => node_name(img, node_of(img, t)),
        _ => "an indirect target".into(),
    }
}

/// The slice of what the instruction at `at` reads (a register, a stack slot, or the arguments of a call).
pub(crate) fn slice_window(
    img: &Image,
    at: &str,
    reg: Option<&str>,
    arg: Option<usize>,
    mem: Option<&str>,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let at_addr = addr_in(img, at)?;
    let f = img
        .func_at(at_addr)
        .ok_or_else(|| Error::Bad(format!("{at_addr:#x} is not inside a function found in this file")))?;
    let (fstart, fend, fname) = (f.start, f.end, img.func_name(f).to_string());
    let bytes = img
        .read_at(fstart, (fend - fstart) as usize)
        .ok_or_else(|| Error::Bad("the function's bytes are not in the file".into()))?;
    let mut insns: Vec<Ins> = Vec::new();
    let mut vias: Vec<Option<Via>> = Vec::new();
    view::walk(bytes, fstart, |i, v, _| {
        insns.push(*i);
        vias.push(v);
    });
    let at_idx = insns
        .iter()
        .position(|i| i.pc == at_addr)
        .ok_or_else(|| Error::Bad(format!("{at_addr:#x} is inside an instruction of {fname}, not at its start")))?;
    let targets: BTreeSet<u64> = insns
        .iter()
        .filter(|i| matches!(i.flow, Flow::Jump | Flow::Cond))
        .filter_map(|i| i.target)
        .filter(|t| (fstart..fend).contains(t))
        .collect();
    let branch_between = |lo: u64, hi: u64| hi > lo && targets.range(lo + 1..=hi).next().is_some();
    let use_ins = insns[at_idx];

    // what to slice
    let mut tracked: Vec<Want> = Vec::new();
    let what_text;
    let push = |t: &mut Vec<Want>, loc: Loc, need: u64| {
        if !t.iter().any(|w| overlaps(&w.loc, &loc)) {
            t.push(Want { loc, need });
        }
    };
    if let Some(m) = mem {
        let l = parse_mem(m)?;
        what_text = loc_text(img, &l);
        push(&mut tracked, l, at_addr);
    } else if let Some(r) = reg {
        let n = parse_reg(r)?;
        what_text = if r.trim().to_ascii_lowercase().starts_with('w') {
            format!("{} (w{n} is its low half)", reg_name(n))
        } else {
            reg_name(n)
        };
        push(&mut tracked, Loc::Reg(n), at_addr);
    } else if matches!(use_ins.flow, Flow::Call | Flow::CallReg) {
        // arguments are the registers set since the previous call (those not set are left alone)
        let mut set: u32 = 0;
        for j in (0..at_idx).rev() {
            let i = &insns[j];
            if matches!(i.flow, Flow::Call | Flow::CallReg) {
                break;
            }
            set |= i.writes;
        }
        let picked: Vec<u8> = match arg {
            Some(a) if a == 0 || a > usize::from(ARG_REGS) => {
                return Err(Error::Bad(format!(
                    "--arg {a}: AArch64 passes the first {ARG_REGS} integer arguments in x0..x7; give 1..={ARG_REGS}"
                )))
            }
            Some(a) => vec![(a - 1) as u8],
            None => (0..ARG_REGS).filter(|n| set >> u32::from(*n) & 1 == 1).collect(),
        };
        if picked.is_empty() {
            return Err(Error::Bad("no argument register is set since the previous call; give --reg or --mem".into()));
        }
        what_text = format!(
            "arguments of the call to {} ({})",
            call_name(img, &use_ins),
            picked.iter().map(|r| reg_name(*r)).collect::<Vec<_>>().join(", ")
        );
        for r in picked {
            push(&mut tracked, Loc::Reg(r), at_addr);
        }
    } else {
        let fx = effects(&use_ins, vias[at_idx]);
        for r in &fx.reads {
            push(&mut tracked, Loc::Reg(*r), at_addr);
        }
        for m in fx.refs.iter().filter(|m| !m.store) {
            if let Some(l) = resolve(m, false) {
                push(&mut tracked, l, at_addr);
            }
        }
        if tracked.is_empty() {
            return Err(Error::Bad(
                "that instruction reads no register or stack slot to follow; give --reg or --mem".into(),
            ));
        }
        what_text = tracked.iter().map(|w| loc_text(img, &w.loc)).collect::<Vec<_>>().join(", ");
    }

    // whether the address of a slot or global was taken before instruction `before`
    let escaped = |loc: &Loc, before: usize| -> bool {
        (0..before).any(|k| match loc {
            Loc::Slot(b, d, s) => insns[k].addimm.is_some_and(|(_, rn, imm)| {
                rn == *b && overlaps(&Loc::Slot(*b, imm as i64, 1), &Loc::Slot(*b, *d, *s))
            }),
            Loc::Global(a, s) => matches!(vias[k], Some(Via::Addr(x)) if (*a..*a + u64::from(*s)).contains(&x)),
            Loc::Reg(_) => false,
        })
    };

    let mut steps: Vec<Step> = Vec::new();
    let mut stops: Vec<Stop> = Vec::new();
    for j in (0..at_idx).rev() {
        if tracked.is_empty() {
            break;
        }
        let ins = &insns[j];
        if matches!(ins.flow, Flow::Call | Flow::CallReg) {
            let name = call_name(img, ins);
            for w in tracked.clone() {
                let cert = if branch_between(ins.pc, w.need) { "inferred" } else { "proven" };
                match w.loc {
                    Loc::Reg(0) => {
                        stops.push(Stop {
                            at: ins.pc,
                            what: format!("x0 is the result of the call to {name}"),
                            why: "a call result: the callee decides the value".into(),
                            certainty: cert,
                        });
                        steps.push(Step {
                            ip: ins.pc,
                            text: ins.text(),
                            why: format!("call to {name}: its return value is x0"),
                            certainty: cert,
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                    Loc::Reg(LR) => {
                        stops.push(Stop {
                            at: ins.pc,
                            what: format!("x30 is the return address set by the call to {name}"),
                            why: "the call writes the address after it into x30".into(),
                            certainty: cert,
                        });
                        steps.push(Step {
                            ip: ins.pc,
                            text: ins.text(),
                            why: format!("call to {name}: x30 becomes {:#x}", ins.pc + 4),
                            certainty: cert,
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                    Loc::Reg(r) if r <= 18 => {
                        stops.push(Stop {
                            at: ins.pc,
                            what: format!("{} is changed by the call to {name}", reg_name(r)),
                            why: "the value before the call is lost; this register is not preserved across calls"
                                .into(),
                            certainty: "inferred",
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                    Loc::Reg(_) => {}
                    l => {
                        let why = if escaped(&l, j) {
                            Some("its address was taken earlier, so the callee can write it")
                        } else if matches!(l, Loc::Global(..)) {
                            Some("a global is visible to every function")
                        } else {
                            None
                        };
                        if let Some(why) = why {
                            stops.push(Stop {
                                at: ins.pc,
                                what: format!("{} may be written by the call to {name}", loc_text(img, &l)),
                                why: why.into(),
                                certainty: "inferred",
                            });
                            tracked.retain(|x| !overlaps(&x.loc, &l));
                        }
                    }
                }
            }
            continue;
        }
        let fx = effects(ins, vias[j]);
        if let Some(why) = &fx.opaque {
            for w in &tracked {
                stops.push(Stop {
                    at: ins.pc,
                    what: format!("{} cannot be followed past {:#x}", loc_text(img, &w.loc), ins.pc),
                    why: why.clone(),
                    certainty: "inferred",
                });
            }
            tracked.clear();
            break;
        }
        // which tracked locations does this instruction write? (pure: the old value is gone)
        let mut hit: Vec<(Want, bool)> = Vec::new();
        for w in &tracked {
            if let Loc::Reg(r) = w.loc {
                if fx.defs.contains(&r) {
                    hit.push((*w, true));
                } else if fx.rmw.contains(&r) {
                    hit.push((*w, false));
                }
            }
        }
        let mut blind: Vec<u8> = Vec::new();
        for m in fx.refs.iter().filter(|m| m.store) {
            match resolve(m, true) {
                Some(l) => {
                    for w in tracked.iter().filter(|w| overlaps(&w.loc, &l)) {
                        hit.push((*w, covers(&l, &w.loc)));
                    }
                }
                None if m.base == SP || m.base == FP => blind.push(m.base),
                None => {}
            }
        }
        let mut cert = "proven";
        for (w, _) in &hit {
            if branch_between(ins.pc, w.need) || matches!(w.loc, Loc::Slot(SP, ..)) {
                cert = "inferred";
            }
        }
        let defined: Vec<String> = hit.iter().map(|(w, _)| loc_text(img, &w.loc)).collect();
        // new needs, as offsets from sp before this instruction
        let mut needs: Vec<Loc> = Vec::new();
        let mut loaded_blind = false;
        if !hit.is_empty() {
            needs.extend(fx.reads.iter().map(|r| Loc::Reg(*r)));
            for m in fx.refs.iter().filter(|m| !m.store) {
                match resolve(m, false) {
                    Some(l) => needs.push(l),
                    None => {
                        loaded_blind = true;
                        stops.push(Stop {
                            at: ins.pc,
                            what: format!("load through a pointer at {:#x}", ins.pc),
                            why: "memory reached through a register is not tracked; the registers that form the address are followed".into(),
                            certainty: "inferred",
                        });
                    }
                }
            }
            for (w, pure) in &hit {
                if *pure {
                    tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                }
            }
        }
        // a store whose place is not known, through sp or x29, may land on a tracked slot
        for b in blind {
            for w in tracked.clone() {
                if matches!(w.loc, Loc::Slot(wb, ..) if wb == b) {
                    stops.push(Stop {
                        at: ins.pc,
                        what: format!("{} may be written at {:#x}", loc_text(img, &w.loc), ins.pc),
                        why: "a store through this base with an offset that is not a constant".into(),
                        certainty: "inferred",
                    });
                    tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                }
            }
        }
        // sp moves: slots held relative to sp follow it
        match fx.sp {
            Sp::Same => {}
            Sp::By(d) => {
                for w in tracked.iter_mut() {
                    if let Loc::Slot(SP, off, s) = w.loc {
                        w.loc = Loc::Slot(SP, off + d, s);
                    }
                }
            }
            Sp::Unknown => {
                for w in tracked.clone() {
                    if matches!(w.loc, Loc::Slot(SP, ..)) {
                        stops.push(Stop {
                            at: ins.pc,
                            what: format!("{} cannot be followed past {:#x}", loc_text(img, &w.loc), ins.pc),
                            why: "sp changes here by an amount that is not a constant".into(),
                            certainty: "inferred",
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                }
            }
        }
        if hit.is_empty() {
            continue;
        }
        needs.dedup();
        for n in &needs {
            push(&mut tracked, *n, ins.pc);
        }
        let defined_text = defined.join(", ");
        let why = if fx.zeroing {
            format!("sets {defined_text} to zero")
        } else if let (Some(a), true) = (fx.addr, needs.is_empty()) {
            format!("sets {defined_text} to the address {a:#x}")
        } else if needs.is_empty() && !loaded_blind && fx.refs.iter().all(|m| m.store) {
            format!("sets {defined_text} to a constant")
        } else if needs.is_empty() {
            format!("defines {defined_text}")
        } else {
            format!(
                "defines {defined_text}; needs {}",
                needs.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ")
            )
        };
        steps.push(Step { ip: ins.pc, text: ins.text(), why, certainty: cert });
    }
    // what is still wanted at the start of the function
    for w in &tracked {
        let cert = if branch_between(fstart, w.need) { "inferred" } else { "proven" };
        let (what, why) = match w.loc {
            Loc::Reg(r) if r < ARG_REGS => (
                format!("{} is argument {} of {fname}", reg_name(r), r + 1),
                "not written before this point: the caller supplies it".to_string(),
            ),
            Loc::Reg(r) => (
                format!("{} is not written in {fname} before this point", reg_name(r)),
                "it holds what the caller left (a preserved register or an uninitialised value)".to_string(),
            ),
            Loc::Slot(b, d, _) if (b == SP && d >= 0) || (b == FP && d >= 16) => (
                format!("{} is above the frame", loc_text(img, &w.loc)),
                "a stack argument or the caller's area: set by the caller".to_string(),
            ),
            l @ Loc::Slot(..) => (
                format!("{} is never written in {fname} before this point", loc_text(img, &l)),
                "uninitialised, or written through a pointer that is not tracked".to_string(),
            ),
            l @ Loc::Global(..) => (
                format!("{} is not written in {fname} before this point", loc_text(img, &l)),
                "a global: other functions may write it, look at its references with xref".to_string(),
            ),
        };
        stops.push(Stop { at: fstart, what, why, certainty: cert });
    }
    steps.sort_by_key(|s| s.ip);
    stops.sort_by_key(|s| std::cmp::Reverse(s.at));

    let mut items: Vec<Row> = Vec::new();
    let row = |text: String, j: Option<serde_json::Value>| Row { key: 0, text, json: j };
    items.push(row(
        format!(
            "slice of {what_text} at {at_addr:#x} ({}) in {fname} {fstart:#x}  ; {} instruction{}, {} stop{}",
            use_ins.text(),
            steps.len(),
            if steps.len() == 1 { "" } else { "s" },
            stops.len(),
            if stops.len() == 1 { "" } else { "s" }
        ),
        None,
    ));
    for s in &steps {
        items.push(row(
            format!("{:#010x}  {:<34} ; {} [{}]", s.ip, s.text, s.why, s.certainty),
            Some(json!({"kind": "step", "addr": s.ip, "text": s.text, "why": s.why, "certainty": s.certainty})),
        ));
    }
    if stops.is_empty() {
        items.push(row("stops: none; every place that was followed ended in a constant".into(), None));
    } else {
        items.push(row("stops:".into(), None));
        for s in &stops {
            items.push(row(
                format!("  {}  ; {} [{}]", s.what, s.why, s.certainty),
                Some(json!({"kind": "stop", "addr": s.at, "what": s.what, "why": s.why, "certainty": s.certainty})),
            ));
        }
    }
    items.push(row(
        "proven: no branch lands between the definition and its use; inferred: another path may also set it (or the slot is sp-relative). The walk is linear over the function's code; a w register is the low half of its x register, flags and vector registers are not followed.".into(),
        None,
    ));
    let (rows, tail) = cut(&items, from, budget, |r| r.clone());
    let mut rest = crate::page::shell_word(at.trim());
    if let Some(r) = reg {
        rest.push_str(&format!(" --reg {r}"));
    }
    if let Some(a) = arg {
        rest.push_str(&format!(" --arg {a}"));
    }
    if let Some(m) = mem {
        rest.push_str(&format!(" --mem {}", crate::page::shell_word(m)));
    }
    let meta = json!({"function": fstart, "at": at_addr, "steps": steps.len(), "stops": stops.len()});
    Ok(Output { op: "slice", meta, rows, tail, command: command(img, "slice", &rest) })
}
