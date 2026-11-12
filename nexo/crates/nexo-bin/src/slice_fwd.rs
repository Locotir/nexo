//! A forward slice: where a value goes after an instruction. The value is what the instruction writes (the
//! result of a call is rax), or a register or stack slot given by hand as it stands just after that instruction.
//!
//! The walk follows the function's control flow, both sides of every branch, and carries the set of places that
//! still hold the value. It reports each instruction that compares it, passes it to a call, stores it, copies it
//! or computes something from it (the new place is followed too), and says why a place stops being followed:
//! overwritten, lost in a call, stored through a pointer, returned, or a jump it cannot follow.
//!
//! A step is `proven` when the instruction is reached without crossing a conditional branch, and `inferred`
//! when only some paths reach it or the slot is reached through `rsp` (assumed not to move).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use iced_x86::{FlowControl, Instruction, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpKind, Register};
use serde_json::json;

use crate::disasm::{formatter, walk};
use crate::mainfind::clobbered_by_call;
use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::slice::{
    arg_regs, call_name, loc_text, mem_loc, overlaps, parse_mem, parse_reg, pure_write, reads, reg_name, writes, Loc,
};
use crate::xops::target_addr;
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

fn skip_reg(r: Register) -> bool {
    matches!(r.full_register(), Register::RSP | Register::RBP | Register::RIP | Register::None)
}

fn is_copy(m: Mnemonic) -> bool {
    use Mnemonic as M;
    matches!(m, M::Mov | M::Movzx | M::Movsx | M::Movsxd | M::Xchg)
}

/// Where the value written by (or, with `--reg`/`--mem`, held after) the instruction at `at` goes.
pub fn slice_forward(
    img: &Image,
    at: &str,
    reg: Option<&str>,
    mem: Option<&str>,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    img.require_x86("slice --forward")?;
    let at_addr = target_addr(img, at)?;
    let f = img
        .func_at(at_addr)
        .ok_or_else(|| Error::Bad(format!("{at_addr:#x} is not inside a function found in this file")))?;
    let (fstart, fend, fname) = (f.start, f.end, img.func_name(f).to_string());
    let bytes = img
        .read_at(fstart, (fend - fstart) as usize)
        .ok_or_else(|| Error::Bad("the function's bytes are not in the file".into()))?;
    let mut insns: Vec<Instruction> = Vec::new();
    walk(bytes, fstart, |i, _| insns.push(*i));
    let index: HashMap<u64, usize> = insns.iter().enumerate().map(|(k, i)| (i.ip(), k)).collect();
    let at_idx = *index
        .get(&at_addr)
        .ok_or_else(|| Error::Bad(format!("{at_addr:#x} is inside an instruction of {fname}, not at its start")))?;
    let mut info = InstructionInfoFactory::new();
    let mut fmt = formatter();
    let text_of = |i: &Instruction, fmt: &mut iced_x86::IntelFormatter| {
        let mut s = String::new();
        iced_x86::Formatter::format(fmt, i, &mut s);
        s
    };
    let use_ins = insns[at_idx];

    // what to follow
    let mut start: Set = Set::new();
    let what_text;
    if let Some(m) = mem {
        let l = parse_mem(m)?;
        what_text = format!("{} after {at_addr:#x}", loc_text(img, &l));
        start.insert(l);
    } else if let Some(r) = reg {
        let r = parse_reg(r).ok_or_else(|| Error::Bad(format!("{r:?} is not a register name (rax, rdi, r8d, ...)")))?;
        what_text = format!("{} after {at_addr:#x}", reg_name(r.full_register()));
        start.insert(Loc::Reg(r.full_register()));
    } else if matches!(use_ins.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
        start.insert(Loc::Reg(Register::RAX));
        what_text = format!("the result (rax) of the call to {} at {at_addr:#x}", call_name(img, &use_ins));
    } else {
        let i = info.info_options(&use_ins, InstructionInfoOptions::NONE);
        for u in i.used_registers() {
            if writes(u.access()) && !skip_reg(u.register()) {
                start.insert(Loc::Reg(u.register().full_register()));
            }
        }
        if use_ins.mnemonic() != Mnemonic::Lea {
            for m in i.used_memory() {
                if writes(m.access()) {
                    if let Some(l) = mem_loc(&use_ins, m.memory_size().size() as u32) {
                        start.insert(l);
                    }
                }
            }
        }
        if start.is_empty() {
            return Err(Error::Bad(
                "that instruction writes no register or stack slot to follow; give --reg or --mem".into(),
            ));
        }
        what_text = format!(
            "{} written at {at_addr:#x}",
            start.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ")
        );
    }

    let args = arg_regs(img);
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
    let in_fn = |a: u64| (fstart..fend).contains(&a);

    macro_rules! step {
        ($ins:expr, $why:expr, $cert:expr) => {{
            let why: String = $why;
            let text = text_of($ins, &mut fmt);
            steps.entry(($ins.ip(), why.clone())).or_insert(Step { ip: $ins.ip(), text, why, certainty: $cert });
        }};
    }
    macro_rules! stop {
        ($at:expr, $what:expr, $why:expr, $cert:expr) => {{
            let what: String = $what;
            stops.entry(($at, what.clone())).or_insert(Stop { at: $at, what, why: $why.into(), certainty: $cert });
        }};
    }

    while let Some((k, set, crossed)) = work.pop() {
        if set.is_empty() {
            continue;
        }
        // run straight-line code until a branch
        let (mut k, mut set, mut crossed) = (k, set, crossed);
        loop {
            if k >= insns.len() {
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
            let cert = if crossed { "inferred" } else { "proven" };
            let flow = ins.flow_control();
            if matches!(flow, FlowControl::Call | FlowControl::IndirectCall) {
                let name = call_name(img, &ins);
                let i = info.info_options(&ins, InstructionInfoOptions::NO_MEMORY_USAGE);
                let operand_regs: Vec<Register> = i
                    .used_registers()
                    .iter()
                    .filter(|u| reads(u.access()) && !skip_reg(u.register()))
                    .map(|u| u.register().full_register())
                    .collect();
                for l in set.clone() {
                    match l {
                        Loc::Reg(r) => {
                            if operand_regs.contains(&r) {
                                step!(&ins, format!("{} is the address called", reg_name(r)), cert);
                            } else if let Some(n) = args.iter().position(|a| *a == r) {
                                step!(
                                    &ins,
                                    format!("{} is argument {} of the call to {name}", reg_name(r), n + 1),
                                    cert
                                );
                            }
                            if clobbered_by_call(r) {
                                stop!(
                                    ins.ip(),
                                    format!("{} is changed by the call to {name}", reg_name(r)),
                                    "the value in it is lost here; follow the callee's argument with nexo bin decomp",
                                    cert
                                );
                                set.remove(&l);
                            }
                        }
                        Loc::Slot(..) => {}
                        Loc::Global(_) => {
                            stop!(
                                ins.ip(),
                                format!("{} may be changed by the call to {name}", loc_text(img, &l)),
                                "a global is visible to every function",
                                "inferred"
                            );
                            set.remove(&l);
                        }
                    }
                }
                if flow == FlowControl::Call && ins.near_branch_target() == ins.next_ip() {
                    // call next: not a call
                }
                k += 1;
                continue;
            }
            let i = info.info(&ins);
            // places read and written by this instruction
            let mut rd: Vec<Loc> = Vec::new();
            let mut wr: Vec<(Loc, bool)> = Vec::new();
            let mut ptr_store = false;
            // writes to al / ah / ax leave the other bytes of the 64-bit register as they were
            let mut partial: Vec<(Loc, Register)> = Vec::new();
            for u in i.used_registers() {
                if skip_reg(u.register()) {
                    continue;
                }
                let l = Loc::Reg(u.register().full_register());
                if reads(u.access()) {
                    rd.push(l);
                }
                if writes(u.access()) {
                    let part = u.register().is_gpr8() || u.register().is_gpr16();
                    if part {
                        partial.push((l, u.register()));
                    }
                    wr.push((l, pure_write(u.access()) && !part));
                }
            }
            if ins.mnemonic() != Mnemonic::Lea {
                for m in i.used_memory() {
                    match mem_loc(&ins, m.memory_size().size() as u32) {
                        Some(l) => {
                            if reads(m.access()) {
                                rd.push(l);
                            }
                            if writes(m.access()) {
                                wr.push((l, pure_write(m.access())));
                            }
                        }
                        None => {
                            if writes(m.access()) {
                                ptr_store = true;
                            }
                        }
                    }
                }
            }
            let all_hit: Vec<Loc> = rd.iter().filter(|l| set.iter().any(|s| overlaps(s, l))).copied().collect();
            // a register that is only the address of a memory operand is dereferenced, not copied: what the
            // instruction loads through it is another value, and the register itself is unchanged
            let value_regs: Vec<Register> = (0..ins.op_count())
                .filter(|n| ins.op_kind(*n) == OpKind::Register)
                .map(|n| ins.op_register(n).full_register())
                .collect();
            let has_mem = ins.op_kinds().any(|k| k == OpKind::Memory) && ins.mnemonic() != Mnemonic::Lea;
            let (addr_hit, hit): (Vec<Loc>, Vec<Loc>) = all_hit.iter().partition(|l| match l {
                Loc::Reg(r) => {
                    has_mem
                        && (ins.memory_base().full_register() == *r || ins.memory_index().full_register() == *r)
                        && !value_regs.contains(r)
                }
                _ => false,
            });
            let zeroing = matches!(ins.mnemonic(), Mnemonic::Xor | Mnemonic::Sub)
                && ins.op0_kind() == OpKind::Register
                && ins.op1_kind() == OpKind::Register
                && ins.op0_register() == ins.op1_register();
            let rsp_slot = hit.iter().any(|l| matches!(l, Loc::Slot(Register::RSP, ..)));
            let cert = if rsp_slot { "inferred" } else { cert };
            let mut killed: Vec<Loc> = Vec::new();
            if !addr_hit.is_empty() {
                let names = addr_hit.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ");
                let mem = mem_text(&ins);
                let (mem_read, mem_write) = i
                    .used_memory()
                    .iter()
                    .fold((false, false), |(r, w), m| (r || reads(m.access()), w || writes(m.access())));
                let dests: Vec<String> = wr
                    .iter()
                    .map(|(l, _)| *l)
                    .filter(|l| matches!(l, Loc::Reg(_)))
                    .map(|l| loc_text(img, &l))
                    .collect();
                let why = match (mem_read, mem_write) {
                    (_, false) if !dests.is_empty() => {
                        format!("the value at {mem} is loaded into {}", dests.join(", "))
                    }
                    (true, true) => format!("{names} is the address of {mem}, which is read and written"),
                    (_, true) => format!("{names} is the address of a store: {mem} is written"),
                    _ => format!("the value at {mem} is read through {names}"),
                };
                step!(&ins, why, cert);
            }
            for (l, r) in &partial {
                if set.contains(l) && !hit.contains(l) {
                    let part = if r.is_gpr16() {
                        format!("the low 2 bytes ({}) of {} are overwritten", reg_name(*r), loc_text(img, l))
                    } else if matches!(r, Register::AH | Register::BH | Register::CH | Register::DH) {
                        format!("bits 8-15 ({}) of {} are overwritten", reg_name(*r), loc_text(img, l))
                    } else {
                        format!("the low byte ({}) of {} is overwritten", reg_name(*r), loc_text(img, l))
                    };
                    step!(&ins, format!("{part}; the rest of it still holds the value"), cert);
                }
            }
            if !hit.is_empty() && !zeroing {
                let names = hit.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ");
                let mut new_places: Vec<Loc> = Vec::new();
                match ins.mnemonic() {
                    Mnemonic::Cmp | Mnemonic::Test => {
                        let mut why = format!("{names} is compared");
                        if let Some(n) = insns.get(k + 1).filter(|n| n.flow_control() == FlowControl::ConditionalBranch)
                        {
                            why.push_str(&format!(
                                "; {} {:#x} decides on it",
                                format!("{:?}", n.mnemonic()).to_lowercase(),
                                n.near_branch_target()
                            ));
                        }
                        step!(&ins, why, cert);
                    }
                    Mnemonic::Push => step!(&ins, format!("{names} is pushed"), cert),
                    _ if ptr_store => {
                        step!(&ins, format!("{names} is stored through a pointer"), cert);
                        stop!(
                            ins.ip(),
                            format!("stored through a pointer at {:#x}", ins.ip()),
                            "memory reached through a register is not tracked; look for loads of it with nexo bin xref",
                            cert
                        );
                    }
                    _ => {
                        let dests: Vec<Loc> =
                            wr.iter().map(|(l, _)| *l).filter(|l| !hit.contains(l) || !pure_write_of(&wr, l)).collect();
                        let dest_text = dests.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ");
                        if dests.is_empty() {
                            step!(&ins, format!("{names} is used"), cert);
                        } else if ins.mnemonic() == Mnemonic::Lea {
                            step!(&ins, format!("{names} is used to compute the address in {dest_text}"), cert);
                            new_places.extend(dests);
                        } else if is_copy(ins.mnemonic()) {
                            let stored = dests.iter().any(|d| !matches!(d, Loc::Reg(_)));
                            let verb = if stored { "stored to" } else { "copied to" };
                            step!(&ins, format!("{names} is {verb} {dest_text}"), cert);
                            new_places.extend(dests);
                        } else {
                            step!(&ins, format!("{names} feeds {dest_text}"), cert);
                            new_places.extend(dests);
                        }
                    }
                }
                for n in new_places {
                    set.insert(n);
                }
            }
            // a plain write that does not read the place ends it
            for (w, pure) in &wr {
                if *pure && !hit.iter().any(|h| overlaps(h, w)) || (zeroing && *pure) {
                    for s in set.clone() {
                        if overlaps(&s, w) && !(ptr_store && false) {
                            // the value just written is a new one unless it came from the followed value
                            let came = !hit.is_empty()
                                && !zeroing
                                && !matches!(ins.mnemonic(), Mnemonic::Cmp | Mnemonic::Test);
                            if !came {
                                killed.push(s);
                            }
                        }
                    }
                }
            }
            for s in killed {
                if set.remove(&s) {
                    stop!(
                        ins.ip(),
                        format!("{} is overwritten", loc_text(img, &s)),
                        "a new value replaces it; the old one goes no further on this path",
                        cert
                    );
                }
            }
            match flow {
                FlowControl::Return => {
                    if set.contains(&Loc::Reg(Register::RAX)) {
                        step!(&ins, format!("rax is returned from {fname}"), cert);
                        stop!(
                            ins.ip(),
                            format!("returned from {fname}"),
                            "it leaves the function: look at the callers with nexo bin xref",
                            cert
                        );
                    }
                    break;
                }
                FlowControl::UnconditionalBranch => {
                    let t = ins.near_branch_target();
                    if ins.op0_kind() == OpKind::NearBranch64 && in_fn(t) {
                        if let Some(&ti) = index.get(&t) {
                            k = ti;
                            continue;
                        }
                    }
                    let held: Vec<String> = set.iter().map(|l| loc_text(img, l)).collect();
                    stop!(
                        ins.ip(),
                        format!(
                            "jump out of {fname} at {:#x} with {} still holding the value",
                            ins.ip(),
                            held.join(", ")
                        ),
                        "a tail call: the callee may take it as an argument",
                        cert
                    );
                    break;
                }
                FlowControl::ConditionalBranch => {
                    let t = ins.near_branch_target();
                    if let Some(&ti) = index.get(&t).filter(|_| in_fn(t)) {
                        work.push((ti, set.clone(), true));
                    }
                    crossed = true;
                    k += 1;
                    continue;
                }
                FlowControl::IndirectBranch => {
                    stop!(
                        ins.ip(),
                        format!("indirect jump at {:#x}", ins.ip()),
                        "where it lands is not known here (a jump table or a tail call through a register)",
                        "inferred"
                    );
                    break;
                }
                FlowControl::Exception | FlowControl::Interrupt => break,
                _ => {}
            }
            if set.is_empty() {
                break;
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

    let steps: Vec<&Step> = steps.values().collect();
    let mut stops: Vec<&Stop> = stops.values().collect();
    stops.sort_by_key(|s| s.at);
    let mut items: Vec<Row> = Vec::new();
    let row = |text: String, j: Option<serde_json::Value>| Row { key: 0, text, json: j };
    items.push(row(
        format!(
            "forward slice of {what_text} in {fname} {fstart:#x}  ; {} step{}, {} stop{}",
            steps.len(),
            if steps.len() == 1 { "" } else { "s" },
            stops.len(),
            if stops.len() == 1 { "" } else { "s" }
        ),
        None,
    ));
    let mut ordered = steps.clone();
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
        "proven: reached without crossing a conditional branch; inferred: only some paths reach it (or the slot is rsp-relative). Both sides of every branch are followed.".into(),
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

fn pure_write_of(wr: &[(Loc, bool)], l: &Loc) -> bool {
    wr.iter().any(|(w, p)| w == l && *p)
}

/// The memory operand as `[rax+rcx*4+0x10]`.
fn mem_text(ins: &Instruction) -> String {
    let mut s = String::new();
    if ins.memory_base() != Register::None {
        s.push_str(&reg_name(ins.memory_base()));
    }
    if ins.memory_index() != Register::None {
        if !s.is_empty() {
            s.push('+');
        }
        s.push_str(&reg_name(ins.memory_index()));
        if ins.memory_index_scale() > 1 {
            s.push_str(&format!("*{}", ins.memory_index_scale()));
        }
    }
    let d = ins.memory_displacement64() as i64;
    if s.is_empty() {
        s.push_str(&format!("{:#x}", d as u64));
    } else if d < 0 {
        s.push_str(&format!("-{:#x}", -d));
    } else if d > 0 {
        s.push_str(&format!("+{d:#x}"));
    }
    format!("[{s}]")
}
