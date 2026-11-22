//! A forward slice on AArch64: where a value goes after an instruction.
//!
//! The same question and the same answer as `crate::slice_fwd` (x86-64), over the same data-flow model as the
//! backward slice in [`super::slice`] (`effects`, `resolve`, `overlaps`, `covers`). The value is what the
//! instruction writes (the result of a call is x0), or a register or stack slot given by hand as it stands just
//! after that instruction.
//!
//! The walk follows the function's control flow, both sides of every branch, and carries the set of places that
//! still hold the value. It reports each instruction that compares it, passes it to a call, stores it, copies it
//! or computes something from it (the new place is followed too), and says why a place stops being followed:
//! overwritten, lost in a call, stored through a pointer, returned, or a jump it cannot follow.
//!
//! A step is `proven` when the instruction is reached without crossing a conditional branch, and `inferred`
//! when only some paths reach it or the slot is reached through `sp` (assumed to move only by constants).
//!
//! What is not followed, and is said so: the flags, vector registers, memory reached through a register that is
//! not `sp`, `x29` or an `adrp` pair, atomics, and words the decoder does not read.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::json;

use super::scan::Via;
use super::slice::{
    call_name, covers, effects, loc_text, overlaps, parse_mem, parse_reg, reg_name, resolve, Loc, Sp, ARG_REGS, FP, LR,
    SP,
};
use super::{view, Flow, Ins, Mem, Opd};
use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::place::addr_in;
use crate::{Error, Image, Result};

/// Instructions visited before the walk gives up and says so.
const MAX_VISITS: usize = 20_000;

type Set = BTreeSet<Loc>;

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

fn add(v: &mut Vec<Loc>, l: Loc) {
    if !v.contains(&l) {
        v.push(l);
    }
}

/// Moves a value without changing it.
fn is_copy(mn: &str) -> bool {
    matches!(mn, "mov" | "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" | "uxtw") || mn.starts_with("ld")
}

/// Where the value written by (or, with `--reg`/`--mem`, held after) the instruction at `at` goes.
pub(crate) fn slice_forward(
    img: &Image,
    at: &str,
    reg: Option<&str>,
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
    let use_ins = insns[at_idx];
    let txt = |v: &[Loc]| v.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ");

    // what to follow
    let mut start: Set = Set::new();
    let what_text;
    if let Some(m) = mem {
        let l = parse_mem(m)?;
        what_text = format!("{} after {at_addr:#x}", loc_text(img, &l));
        start.insert(l);
    } else if let Some(r) = reg {
        let n = parse_reg(r)?;
        what_text = format!("{} after {at_addr:#x}", reg_name(n));
        start.insert(Loc::Reg(n));
    } else if matches!(use_ins.flow, Flow::Call | Flow::CallReg) {
        start.insert(Loc::Reg(0));
        what_text = format!("the result (x0) of the call to {} at {at_addr:#x}", call_name(img, &use_ins));
    } else {
        let fx = effects(&use_ins, vias[at_idx]);
        if let Some(why) = &fx.opaque {
            return Err(Error::Bad(format!("{at_addr:#x}: {why}; give --reg or --mem")));
        }
        for r in fx.defs.iter().chain(fx.rmw.iter()) {
            if *r != FP && *r != LR {
                start.insert(Loc::Reg(*r));
            }
        }
        for m in fx.refs.iter().filter(|m| m.store) {
            if let Some(l) = resolve(m, true) {
                start.insert(l);
            }
        }
        if start.is_empty() {
            return Err(Error::Bad(
                "that instruction writes no register or stack slot to follow; give --reg or --mem".into(),
            ));
        }
        let held: Vec<Loc> = start.iter().copied().collect();
        what_text = format!("{} written at {at_addr:#x}", txt(&held));
    }

    let mut steps: BTreeMap<(u64, String), Step> = BTreeMap::new();
    let mut stops: BTreeMap<(u64, String), Stop> = BTreeMap::new();
    // work item: instruction index, places that hold the value, a conditional branch was crossed
    let mut work: Vec<(usize, Set, bool)> = Vec::new();
    let mut seen: HashMap<usize, Vec<Set>> = HashMap::new();
    if at_idx + 1 < insns.len() {
        work.push((at_idx + 1, start, false));
    }
    let mut visits = 0usize;
    let mut capped = false;
    let index_of = |a: u64| -> Option<usize> {
        if (fstart..fend).contains(&a) && (a - fstart).is_multiple_of(4) {
            Some(((a - fstart) / 4) as usize).filter(|k| *k < insns.len())
        } else {
            None
        }
    };

    macro_rules! step {
        ($ins:expr, $why:expr, $cert:expr) => {{
            let why: String = $why;
            let text = $ins.text();
            steps.entry(($ins.pc, why.clone())).or_insert(Step { ip: $ins.pc, text, why, certainty: $cert });
        }};
    }
    macro_rules! stop {
        ($at:expr, $what:expr, $why:expr, $cert:expr) => {{
            let what: String = $what;
            stops.entry(($at, what.clone())).or_insert(Stop { at: $at, what, why: $why.into(), certainty: $cert });
        }};
    }

    while let Some((k0, set0, crossed0)) = work.pop() {
        let (mut k, mut set, mut crossed) = (k0, set0, crossed0);
        // run straight-line code until a branch
        loop {
            if set.is_empty() || k >= insns.len() {
                break;
            }
            let ins = insns[k];
            let prior = seen.entry(k).or_default();
            if prior.iter().any(|p| set.is_subset(p)) {
                break;
            }
            prior.push(set.clone());
            visits += 1;
            if visits > MAX_VISITS {
                capped = true;
                break;
            }
            let cert0 = if crossed { "inferred" } else { "proven" };
            if matches!(ins.flow, Flow::Call | Flow::CallReg) {
                let name = call_name(img, &ins);
                let called = match (ins.flow, ins.ops[0]) {
                    (Flow::CallReg, Opd::Gpr { n, .. }) => Some(n),
                    _ => None,
                };
                for l in set.clone() {
                    match l {
                        Loc::Reg(r) => {
                            if called == Some(r) {
                                step!(&ins, format!("{} is the address called", reg_name(r)), cert0);
                            } else if r < ARG_REGS {
                                step!(
                                    &ins,
                                    format!("{} is argument {} of the call to {name}", reg_name(r), r + 1),
                                    cert0
                                );
                            }
                            if r <= 18 || r == LR {
                                stop!(
                                    ins.pc,
                                    format!("{} is changed by the call to {name}", reg_name(r)),
                                    "the value in it is lost here; look at the callee's argument with nexo bin fn",
                                    cert0
                                );
                                set.remove(&l);
                            }
                        }
                        Loc::Slot(b, off, _) => {
                            if b == SP && off < 0 {
                                stop!(
                                    ins.pc,
                                    format!("{} may be written by the call to {name}", loc_text(img, &l)),
                                    "the slot is below sp: the callee's frame lives there",
                                    "inferred"
                                );
                                set.remove(&l);
                            }
                        }
                        Loc::Global(..) => {
                            stop!(
                                ins.pc,
                                format!("{} may be changed by the call to {name}", loc_text(img, &l)),
                                "a global is visible to every function",
                                "inferred"
                            );
                            set.remove(&l);
                        }
                    }
                }
                k += 1;
                continue;
            }
            let fx = effects(&ins, vias[k]);
            if let Some(why) = &fx.opaque {
                for l in &set {
                    stop!(
                        ins.pc,
                        format!("{} cannot be followed past {:#x}", loc_text(img, l), ins.pc),
                        why.clone(),
                        cert0
                    );
                }
                break;
            }
            let mn = ins.mn.as_str();
            // registers read as values, and registers that only form the address of a memory operand
            let rmw_op = matches!(mn, "movk" | "bfi" | "bfxil" | "bfc" | "bfm");
            let pair_load = ["ldp", "ldnp", "ldxp", "ldaxp"].iter().any(|p| mn.starts_with(p));
            let written = ins.writes & 0x7fff_ffff;
            let mut val_regs: Vec<u8> = Vec::new();
            let mut addr_regs: Vec<u8> = Vec::new();
            for (n, o) in ins.ops.iter().enumerate() {
                match *o {
                    Opd::Gpr { n: r, .. } if r < 31 => {
                        let dest = (n == 0 || (n == 1 && pair_load)) && written >> r & 1 == 1 && !rmw_op;
                        if !dest {
                            val_regs.push(r);
                        }
                    }
                    Opd::Mem(Mem::Off { base, .. } | Mem::Pre { base, .. } | Mem::Post { base, .. }) => {
                        addr_regs.push(base);
                    }
                    Opd::Mem(Mem::Reg { base, idx, .. } | Mem::PostReg { base, idx }) => {
                        addr_regs.push(base);
                        addr_regs.push(idx);
                    }
                    _ => {}
                }
            }
            let mut hit_regs: Vec<Loc> = Vec::new();
            let mut addr_hit: Vec<Loc> = Vec::new();
            for r in &fx.reads {
                let l = Loc::Reg(*r);
                if set.contains(&l) {
                    if addr_regs.contains(r) && !val_regs.contains(r) {
                        add(&mut addr_hit, l);
                    } else {
                        add(&mut hit_regs, l);
                    }
                }
            }
            let compare = ins.flow == Flow::Cond || matches!(mn, "cmp" | "cmn" | "tst" | "ccmp" | "ccmn");
            let mut new_places: Vec<Loc> = Vec::new();
            // loads of a place that is followed
            let mut hit_loads: Vec<Loc> = Vec::new();
            let mut vector_load = false;
            for (e, m) in fx.refs.iter().enumerate().filter(|(_, m)| !m.store) {
                let Some(l) = resolve(m, false) else { continue };
                if !set.iter().any(|s| overlaps(s, &l)) {
                    continue;
                }
                add(&mut hit_loads, l);
                match ins.ops.get(e) {
                    Some(Opd::Gpr { n, .. }) if *n < 31 => new_places.push(Loc::Reg(*n)),
                    Some(Opd::Fp { .. } | Opd::List { .. }) => vector_load = true,
                    _ => {}
                }
            }
            // stores: where each goes, and whether the data is a followed register
            let first_data = usize::from(mn.starts_with("stx") || mn.starts_with("stlx"));
            let mut stored: Vec<(Loc, bool)> = Vec::new();
            let mut ptr_store = false;
            let mut unknown_off = false;
            let mut blind: Vec<u8> = Vec::new();
            for (e, m) in fx.refs.iter().enumerate().filter(|(_, m)| m.store) {
                let from_held = match ins.ops.get(first_data + e) {
                    Some(Opd::Gpr { n, .. }) if *n < 31 => set.contains(&Loc::Reg(*n)),
                    _ => false,
                };
                match resolve(m, false) {
                    Some(l) => {
                        stored.push((l, from_held));
                        if from_held {
                            new_places.push(l);
                        }
                    }
                    None => {
                        let stack = m.base == SP || m.base == FP;
                        if stack {
                            blind.push(m.base);
                        }
                        if from_held {
                            if stack {
                                unknown_off = true;
                            } else {
                                ptr_store = true;
                            }
                        }
                    }
                }
            }
            // anything else that a followed register feeds is a new place
            if fx.refs.is_empty() && !hit_regs.is_empty() && !compare {
                for r in fx.defs.iter().chain(fx.rmw.iter()) {
                    new_places.push(Loc::Reg(*r));
                }
            }
            let hit_all: Vec<Loc> = hit_regs.iter().chain(hit_loads.iter()).copied().collect();
            let cert = if hit_all.iter().chain(new_places.iter()).any(|l| matches!(l, Loc::Slot(SP, ..))) {
                "inferred"
            } else {
                cert0
            };

            // the address of a followed slot is taken: it can be read or changed through the pointer
            if let Some((rd, rn, imm)) = ins.addimm {
                if (rn == SP || rn == FP) && rd != SP && rd != FP {
                    let probe = Loc::Slot(rn, imm as i64, 1);
                    let taken: Vec<Loc> = set
                        .iter()
                        .filter(|s| matches!(s, Loc::Slot(b, ..) if *b == rn) && overlaps(&probe, s))
                        .copied()
                        .collect();
                    for s in taken {
                        stop!(
                            ins.pc,
                            format!("the address of {} is taken into {}", loc_text(img, &s), reg_name(rd)),
                            "it can be read or changed through that pointer; the slot is not followed further",
                            "inferred"
                        );
                        set.remove(&s);
                    }
                }
            }
            if !addr_hit.is_empty() {
                let has_store = fx.refs.iter().any(|m| m.store);
                let has_load = fx.refs.iter().any(|m| !m.store);
                let kind = match (has_load, has_store) {
                    (true, true) => "a load and a store",
                    (true, false) => "a load",
                    (false, true) => "a store",
                    (false, false) => "a memory access",
                };
                step!(&ins, format!("{} is the address of {kind}", txt(&addr_hit)), cert);
            }
            if compare && !hit_regs.is_empty() {
                let names = txt(&hit_regs);
                let why = if ins.flow == Flow::Cond {
                    let to = ins.target.map(|t| format!(" {t:#x}")).unwrap_or_default();
                    format!("{names} is tested; {}{to} decides on it", ins.mnemonic())
                } else {
                    let mut why = format!("{names} is compared");
                    let next = insns
                        .get(k + 1)
                        .filter(|n| n.flow == Flow::Cond)
                        .and_then(|n| n.target.map(|t| (n.mnemonic(), t)));
                    if let Some((bm, t)) = next {
                        why.push_str(&format!("; {bm} {t:#x} decides on it"));
                    }
                    why
                };
                step!(&ins, why, cert);
            } else if !hit_all.is_empty() {
                let mut reported = false;
                if ptr_store {
                    step!(&ins, format!("{} is stored through a pointer", txt(&hit_regs)), cert);
                    stop!(
                        ins.pc,
                        format!("stored through a pointer at {:#x}", ins.pc),
                        "memory reached through a register is not tracked; look for loads of it with nexo bin xref",
                        cert
                    );
                    reported = true;
                }
                if unknown_off {
                    step!(&ins, format!("{} is stored to a stack slot that is not known", txt(&hit_regs)), cert);
                    stop!(
                        ins.pc,
                        format!("stored through sp or x29 with an offset that is not a constant at {:#x}", ins.pc),
                        "the slot it lands in is not known",
                        cert
                    );
                    reported = true;
                }
                let dests: Vec<Loc> = stored.iter().filter(|(_, h)| *h).map(|(l, _)| *l).collect();
                if !dests.is_empty() {
                    step!(&ins, format!("{} is stored to {}", txt(&hit_regs), txt(&dests)), cert);
                    reported = true;
                }
                if !hit_loads.is_empty() {
                    let regs: Vec<Loc> = new_places.iter().filter(|l| matches!(l, Loc::Reg(_))).copied().collect();
                    if regs.is_empty() {
                        step!(&ins, format!("{} is loaded", txt(&hit_loads)), cert);
                    } else {
                        step!(&ins, format!("{} is copied to {}", txt(&hit_loads), txt(&regs)), cert);
                    }
                    if vector_load {
                        stop!(
                            ins.pc,
                            format!("{} is loaded into a vector register", txt(&hit_loads)),
                            "vector registers are not followed",
                            cert
                        );
                    }
                    reported = true;
                }
                if !reported {
                    let names = txt(&hit_regs);
                    let dests: Vec<Loc> = new_places.iter().filter(|l| !hit_regs.contains(l)).copied().collect();
                    let all_in_place = !new_places.is_empty() && dests.is_empty();
                    if new_places.is_empty() {
                        step!(&ins, format!("{names} is used (flags and vector registers are not followed)"), cert);
                    } else if all_in_place {
                        step!(&ins, format!("{names} is updated in place"), cert);
                    } else if is_copy(&ins.mnemonic()) {
                        step!(&ins, format!("{names} is copied to {}", txt(&dests)), cert);
                    } else {
                        step!(&ins, format!("{names} feeds {}", txt(&dests)), cert);
                    }
                }
            }

            // a register that is replaced without being derived from the value ends
            for r in &fx.defs {
                let l = Loc::Reg(*r);
                if set.contains(&l) && !new_places.contains(&l) {
                    set.remove(&l);
                    stop!(
                        ins.pc,
                        format!("{} is overwritten", reg_name(*r)),
                        "a new value replaces it; the old one goes no further on this path",
                        cert0
                    );
                }
            }
            // a store that lands on a followed slot or global without carrying the value ends it
            for (l, from_held) in &stored {
                if *from_held {
                    continue;
                }
                for s in set.clone().iter().filter(|s| overlaps(s, l)) {
                    let (what, why) = if covers(l, s) {
                        (
                            format!("{} is overwritten", loc_text(img, s)),
                            "a new value replaces it; the old one goes no further on this path",
                        )
                    } else {
                        (
                            format!("{} is partly overwritten", loc_text(img, s)),
                            "a store covers only some of it; the rest is not followed further",
                        )
                    };
                    stop!(ins.pc, what, why, cert0);
                    set.remove(s);
                }
            }
            // a store through sp or x29 with an offset that is not a constant may land on a followed slot
            for b in blind {
                for s in set.clone() {
                    if matches!(s, Loc::Slot(sb, ..) if sb == b) {
                        stop!(
                            ins.pc,
                            format!("{} may be written at {:#x}", loc_text(img, &s), ins.pc),
                            "a store through this base with an offset that is not a constant",
                            "inferred"
                        );
                        set.remove(&s);
                    }
                }
            }
            for n in new_places {
                set.insert(n);
            }
            // sp moves: slots held relative to sp follow it
            match fx.sp {
                Sp::Same => {}
                Sp::By(d) => {
                    set = set
                        .into_iter()
                        .map(|l| match l {
                            Loc::Slot(SP, off, s) => Loc::Slot(SP, off.wrapping_sub(d), s),
                            o => o,
                        })
                        .collect();
                }
                Sp::Unknown => {
                    for s in set.clone() {
                        if matches!(s, Loc::Slot(SP, ..)) {
                            stop!(
                                ins.pc,
                                format!("{} cannot be followed past {:#x}", loc_text(img, &s), ins.pc),
                                "sp changes here by an amount that is not a constant",
                                "inferred"
                            );
                            set.remove(&s);
                        }
                    }
                }
            }
            match ins.flow {
                Flow::Ret => {
                    if set.contains(&Loc::Reg(0)) {
                        step!(&ins, format!("x0 is returned from {fname}"), cert0);
                        stop!(
                            ins.pc,
                            format!("returned from {fname}"),
                            "it leaves the function: look at the callers with nexo bin xref",
                            cert0
                        );
                    }
                    for l in set.iter().filter(|l| matches!(l, Loc::Global(..))) {
                        stop!(
                            ins.pc,
                            format!("{} still holds the value when {fname} returns", loc_text(img, l)),
                            "a global outlives the function: look at its readers with nexo bin xref",
                            cert0
                        );
                    }
                    break;
                }
                Flow::Jump => match ins.target.and_then(index_of) {
                    Some(ti) => {
                        k = ti;
                        continue;
                    }
                    None => {
                        let held: Vec<Loc> = set.iter().copied().collect();
                        stop!(
                            ins.pc,
                            format!("jump out of {fname} at {:#x} with {} still holding the value", ins.pc, txt(&held)),
                            "a tail call: the callee may take it as an argument",
                            cert0
                        );
                        break;
                    }
                },
                Flow::Cond => {
                    if let Some(ti) = ins.target.and_then(index_of) {
                        work.push((ti, set.clone(), true));
                    }
                    crossed = true;
                }
                Flow::JumpReg => {
                    stop!(
                        ins.pc,
                        format!("indirect jump at {:#x}", ins.pc),
                        "where it lands is not known here (a jump table or a tail call through a register)",
                        "inferred"
                    );
                    break;
                }
                Flow::Trap => break,
                _ => {}
            }
            k += 1;
        }
    }
    if capped {
        stop!(
            fstart,
            format!("stopped after {MAX_VISITS} instructions"),
            "the function is too large to follow completely",
            "inferred"
        );
    }

    let mut ordered: Vec<&Step> = steps.values().collect();
    let mut stops: Vec<&Stop> = stops.values().collect();
    stops.sort_by_key(|s| s.at);
    let mut items: Vec<Row> = Vec::new();
    let row = |text: String, j: Option<serde_json::Value>| Row { key: 0, text, json: j };
    items.push(row(
        format!(
            "forward slice of {what_text} in {fname} {fstart:#x}  ; {} step{}, {} stop{}",
            ordered.len(),
            if ordered.len() == 1 { "" } else { "s" },
            stops.len(),
            if stops.len() == 1 { "" } else { "s" }
        ),
        None,
    ));
    ordered.sort_by_key(|s| s.ip);
    for s in &ordered {
        items.push(row(
            format!("{:#010x}  {:<34} ; {} [{}]", s.ip, s.text, s.why, s.certainty),
            Some(json!({"kind": "step", "addr": s.ip, "text": s.text, "why": s.why, "certainty": s.certainty})),
        ));
    }
    if ordered.is_empty() {
        items.push(row("steps: none; nothing reads the value before it is lost".into(), None));
    }
    if stops.is_empty() {
        items.push(row("stops: none".into(), None));
    } else {
        items.push(row("stops:".into(), None));
        for s in &stops {
            items.push(row(
                format!("  {:#010x}  {}  ; {} [{}]", s.at, s.what, s.why, s.certainty),
                Some(json!({"kind": "stop", "addr": s.at, "what": s.what, "why": s.why, "certainty": s.certainty})),
            ));
        }
    }
    items.push(row(
        "proven: reached without crossing a conditional branch; inferred: only some paths reach it (or the slot is sp-relative). Both sides of every branch are followed. A w register is the low half of its x register; the flags and vector registers are not followed.".into(),
        None,
    ));
    let (rows, tail) = cut(&items, from, budget, |r| r.clone());
    let mut rest = crate::page::shell_word(at.trim());
    if let Some(r) = reg {
        rest.push_str(&format!(" --reg {r}"));
    }
    if let Some(m) = mem {
        rest.push_str(&format!(" --mem {}", crate::page::shell_word(m)));
    }
    rest.push_str(" --forward");
    let meta = json!({"function": fstart, "at": at_addr, "steps": ordered.len(), "stops": stops.len()});
    Ok(Output { op: "slice", meta, rows, tail, command: command(img, "slice", &rest) })
}
