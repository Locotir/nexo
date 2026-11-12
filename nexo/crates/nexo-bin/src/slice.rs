//! A backward slice: which instructions of one function produced a value at an instruction.
//!
//! The value is a register, a stack slot or a global that the instruction reads (or the arguments of a
//! call). The walk goes back through the function's instructions in address order and follows what each
//! defining instruction itself needs. It stops, and says why, at a call result, an argument of the function,
//! a global, a load through a pointer, a slot whose address was passed away, or the start of the function.
//!
//! The walk is linear, not a full flow analysis. A step is `proven` when no branch lands between the
//! defining instruction and the place that needed its value (so no other path can have set it), and
//! `inferred` when one does, or when the slot is reached through `rsp` (assumed not to move).

use std::collections::BTreeSet;

use iced_x86::{
    FlowControl, Instruction, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpAccess, OpKind, Register,
};
use serde_json::json;

use crate::disasm::{formatter, walk};
use crate::graph::{node_name, node_of};
use crate::mainfind::clobbered_by_call;
use crate::ops::command;
use crate::page::{cut, Output, Row};
use crate::xops::target_addr;
use crate::{Error, Format, Image, Result};

/// A place a value lives.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Loc {
    Reg(Register),
    /// `[base + disp]` with base rbp or rsp, `size` bytes.
    Slot(Register, i64, u32),
    /// A memory cell at a fixed address.
    Global(u64),
}

#[derive(Clone, Copy)]
struct Want {
    loc: Loc,
    /// The instruction that needs it (a branch landing after the definition and before this one makes the step inferred).
    need: u64,
}

fn overlaps(a: &Loc, b: &Loc) -> bool {
    match (a, b) {
        (Loc::Reg(x), Loc::Reg(y)) => x == y,
        (Loc::Slot(b1, d1, s1), Loc::Slot(b2, d2, s2)) => {
            b1 == b2 && d1 < &(d2 + *s2 as i64) && d2 < &(d1 + *s1 as i64)
        }
        (Loc::Global(x), Loc::Global(y)) => x == y,
        _ => false,
    }
}

fn reg_name(r: Register) -> String {
    format!("{r:?}").to_lowercase()
}

fn loc_text(img: &Image, l: &Loc) -> String {
    match l {
        Loc::Reg(r) => reg_name(*r),
        Loc::Slot(b, d, _) => {
            if *d < 0 {
                format!("[{}-{:#x}]", reg_name(*b), -d)
            } else {
                format!("[{}+{:#x}]", reg_name(*b), d)
            }
        }
        Loc::Global(a) => {
            match img.name_at(*a).map(str::to_string).or_else(|| img.symbol_covering(*a).map(|(n, _)| n.to_string())) {
                Some(n) => format!("global {n} {a:#x}"),
                None => format!("global {a:#x}"),
            }
        }
    }
}

const GPR64: [Register; 16] = [
    Register::RAX,
    Register::RCX,
    Register::RDX,
    Register::RBX,
    Register::RSP,
    Register::RBP,
    Register::RSI,
    Register::RDI,
    Register::R8,
    Register::R9,
    Register::R10,
    Register::R11,
    Register::R12,
    Register::R13,
    Register::R14,
    Register::R15,
];
const NAMES64: [&str; 16] =
    ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
const NAMES32: [&str; 16] = [
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "r8d", "r9d", "r10d", "r11d", "r12d", "r13d", "r14d",
    "r15d",
];

fn parse_reg(s: &str) -> Option<Register> {
    let s = s.trim().to_ascii_lowercase();
    NAMES64.iter().position(|n| *n == s).or_else(|| NAMES32.iter().position(|n| *n == s)).map(|i| GPR64[i])
}

/// `rbp-0x14`, `rsp+8`, `[rbp-20]`.
fn parse_mem(s: &str) -> Result<Loc> {
    let t = s.trim().trim_start_matches('[').trim_end_matches(']').replace(' ', "");
    let bad = || Error::Bad(format!("{s:?} is not a stack slot; write rbp-0x14 or rsp+8"));
    let pos = t.find(['+', '-']).ok_or_else(bad)?;
    let base = parse_reg(&t[..pos]).filter(|r| matches!(r, Register::RBP | Register::RSP)).ok_or_else(bad)?;
    let n = crate::parse_number(&t[pos + 1..]).map_err(|_| bad())? as i64;
    Ok(Loc::Slot(base, if t.as_bytes()[pos] == b'-' { -n } else { n }, 8))
}

fn arg_regs(img: &Image) -> &'static [Register] {
    match img.format {
        Format::Elf => &[Register::RDI, Register::RSI, Register::RDX, Register::RCX, Register::R8, Register::R9],
        Format::Pe => &[Register::RCX, Register::RDX, Register::R8, Register::R9],
    }
}

fn writes(a: OpAccess) -> bool {
    matches!(a, OpAccess::Write | OpAccess::ReadWrite | OpAccess::CondWrite | OpAccess::ReadCondWrite)
}
fn pure_write(a: OpAccess) -> bool {
    matches!(a, OpAccess::Write)
}
fn reads(a: OpAccess) -> bool {
    matches!(a, OpAccess::Read | OpAccess::ReadWrite | OpAccess::CondRead | OpAccess::ReadCondWrite)
}

fn mem_loc(ins: &Instruction, size: u32) -> Option<Loc> {
    if ins.is_ip_rel_memory_operand() {
        return Some(Loc::Global(ins.ip_rel_memory_address()));
    }
    let (b, i) = (ins.memory_base(), ins.memory_index());
    if i == Register::None && matches!(b, Register::RBP | Register::RSP) {
        return Some(Loc::Slot(b, ins.memory_displacement64() as i64, size.max(1)));
    }
    if b == Register::None && i == Register::None {
        return Some(Loc::Global(ins.memory_displacement64()));
    }
    None
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

fn call_name(img: &Image, ins: &Instruction) -> String {
    if ins.op0_kind() == OpKind::NearBranch64 {
        return node_name(img, node_of(img, ins.near_branch_target()));
    }
    if ins.is_ip_rel_memory_operand() {
        let a = ins.ip_rel_memory_address();
        if img.slots.contains_key(&a) {
            return node_name(img, a);
        }
    }
    "an indirect target".into()
}

/// The slice of what the instruction at `at` reads (a register, a stack slot, or the arguments of a call).
pub fn slice_window(
    img: &Image,
    at: &str,
    reg: Option<&str>,
    arg: Option<usize>,
    mem: Option<&str>,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    img.require_x86("slice")?;
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
    let at_idx = insns
        .iter()
        .position(|i| i.ip() == at_addr)
        .ok_or_else(|| Error::Bad(format!("{at_addr:#x} is inside an instruction of {fname}, not at its start")))?;
    let targets: BTreeSet<u64> = insns
        .iter()
        .filter(|i| {
            matches!(i.flow_control(), FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch)
                && i.op0_kind() == OpKind::NearBranch64
        })
        .map(|i| i.near_branch_target())
        .filter(|t| (fstart..fend).contains(t))
        .collect();
    let branch_between = |lo: u64, hi: u64| targets.range(lo + 1..=hi).next().is_some();
    let mut info = InstructionInfoFactory::new();
    let mut fmt = formatter();
    let text_of = |i: &Instruction, fmt: &mut iced_x86::IntelFormatter| {
        let mut s = String::new();
        iced_x86::Formatter::format(fmt, i, &mut s);
        s
    };
    let use_ins = insns[at_idx];

    // what to slice
    let mut tracked: Vec<Want> = Vec::new();
    let what_text;
    let push = |t: &mut Vec<Want>, loc: Loc, need: u64| {
        if !t.iter().any(|w| overlaps(&w.loc, &loc)) {
            t.push(Want { loc, need });
        }
    };
    let skip =
        |r: Register| matches!(r.full_register(), Register::RSP | Register::RBP | Register::RIP | Register::None);
    if let Some(m) = mem {
        let l = parse_mem(m)?;
        what_text = loc_text(img, &l);
        push(&mut tracked, l, at_addr);
    } else if let Some(r) = reg {
        let r = parse_reg(r).ok_or_else(|| Error::Bad(format!("{r:?} is not a register name (rax, rdi, r8d, ...)")))?;
        what_text = reg_name(r.full_register());
        push(&mut tracked, Loc::Reg(r.full_register()), at_addr);
    } else if use_ins.flow_control() == FlowControl::Call || use_ins.flow_control() == FlowControl::IndirectCall {
        let regs = arg_regs(img);
        let n = arg.map_or(regs.len(), |a| a.clamp(1, regs.len()));
        // arguments are the registers set since the previous call (those not set are left alone)
        let mut set: BTreeSet<Register> = BTreeSet::new();
        for j in (0..at_idx).rev() {
            let i = &insns[j];
            if matches!(i.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
                break;
            }
            for u in info.info_options(i, InstructionInfoOptions::NO_MEMORY_USAGE).used_registers() {
                if writes(u.access()) {
                    set.insert(u.register().full_register());
                }
            }
        }
        let picked: Vec<Register> = match arg {
            Some(a) => regs.get(a.saturating_sub(1)).copied().into_iter().collect(),
            None => regs.iter().take(n).copied().filter(|r| set.contains(r)).collect(),
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
        let i = info.info_options(&use_ins, InstructionInfoOptions::NONE);
        for u in i.used_registers() {
            if reads(u.access()) && !skip(u.register()) {
                push(&mut tracked, Loc::Reg(u.register().full_register()), at_addr);
            }
        }
        for m in i.used_memory() {
            if reads(m.access()) {
                if let Some(l) = mem_loc(&use_ins, m.memory_size().size() as u32) {
                    push(&mut tracked, l, at_addr);
                }
            }
        }
        if tracked.is_empty() {
            return Err(Error::Bad(
                "that instruction reads no register or stack slot to follow; give --reg or --mem".into(),
            ));
        }
        what_text = tracked.iter().map(|w| loc_text(img, &w.loc)).collect::<Vec<_>>().join(", ");
    }

    let escaped = |loc: &Loc, before: usize| -> bool {
        insns[..before].iter().any(|i| {
            i.mnemonic() == Mnemonic::Lea
                && match loc {
                    Loc::Slot(b, d, s) => {
                        i.memory_index() == Register::None
                            && i.memory_base() == *b
                            && overlaps(&Loc::Slot(*b, i.memory_displacement64() as i64, 1), &Loc::Slot(*b, *d, *s))
                    }
                    Loc::Global(a) => i.is_ip_rel_memory_operand() && i.ip_rel_memory_address() == *a,
                    Loc::Reg(_) => false,
                }
        })
    };

    let mut steps: Vec<Step> = Vec::new();
    let mut stops: Vec<Stop> = Vec::new();
    for j in (0..at_idx).rev() {
        if tracked.is_empty() {
            break;
        }
        let ins = &insns[j];
        if matches!(ins.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
            let name = call_name(img, ins);
            for w in tracked.clone() {
                let cert = if branch_between(ins.ip(), w.need) { "inferred" } else { "proven" };
                match w.loc {
                    Loc::Reg(Register::RAX) => {
                        stops.push(Stop {
                            at: ins.ip(),
                            what: format!("rax is the result of the call to {name}"),
                            why: "a call result: the callee decides the value".into(),
                            certainty: cert,
                        });
                        steps.push(Step {
                            ip: ins.ip(),
                            text: text_of(ins, &mut fmt),
                            why: format!("call to {name}: its return value is rax"),
                            certainty: cert,
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                    Loc::Reg(r) if clobbered_by_call(r) => {
                        stops.push(Stop {
                            at: ins.ip(),
                            what: format!("{} is changed by the call to {name}", reg_name(r)),
                            why: "the value before the call is lost; this register is not preserved across calls"
                                .into(),
                            certainty: "inferred",
                        });
                        tracked.retain(|x| !overlaps(&x.loc, &w.loc));
                    }
                    Loc::Reg(_) => {}
                    l => {
                        if escaped(&l, j) {
                            stops.push(Stop {
                                at: ins.ip(),
                                what: format!("{} may be written by the call to {name}", loc_text(img, &l)),
                                why: "its address was taken (lea) earlier, so the callee can write it".into(),
                                certainty: "inferred",
                            });
                            tracked.retain(|x| !overlaps(&x.loc, &l));
                        } else if matches!(l, Loc::Global(_)) {
                            stops.push(Stop {
                                at: ins.ip(),
                                what: format!("{} may be written by the call to {name}", loc_text(img, &l)),
                                why: "a global is visible to every function".into(),
                                certainty: "inferred",
                            });
                            tracked.retain(|x| !overlaps(&x.loc, &l));
                        }
                    }
                }
            }
            continue;
        }
        let i = info.info(ins);
        // which tracked locations does this instruction write?
        let mut hit: Vec<(Want, bool)> = Vec::new();
        for u in i.used_registers() {
            if writes(u.access()) {
                let l = Loc::Reg(u.register().full_register());
                for w in tracked.iter().filter(|w| overlaps(&w.loc, &l)) {
                    hit.push((*w, pure_write(u.access())));
                }
            }
        }
        if ins.mnemonic() != Mnemonic::Lea {
            for m in i.used_memory() {
                if writes(m.access()) {
                    if let Some(l) = mem_loc(ins, m.memory_size().size() as u32) {
                        for w in tracked.iter().filter(|w| overlaps(&w.loc, &l)) {
                            hit.push((*w, pure_write(m.access())));
                        }
                    }
                }
            }
        }
        if hit.is_empty() {
            continue;
        }
        let zeroing = matches!(ins.mnemonic(), Mnemonic::Xor | Mnemonic::Sub)
            && ins.op0_kind() == OpKind::Register
            && ins.op1_kind() == OpKind::Register
            && ins.op0_register() == ins.op1_register();
        let mut cert = "proven";
        for (w, _) in &hit {
            if branch_between(ins.ip(), w.need) {
                cert = "inferred";
            }
            if matches!(w.loc, Loc::Slot(Register::RSP, ..)) {
                cert = "inferred";
            }
        }
        let defined: Vec<String> = hit.iter().map(|(w, _)| loc_text(img, &w.loc)).collect();
        // new needs
        let mut needs: Vec<Loc> = Vec::new();
        if !zeroing {
            for u in i.used_registers() {
                if reads(u.access()) && !skip(u.register()) {
                    needs.push(Loc::Reg(u.register().full_register()));
                }
            }
            if ins.mnemonic() != Mnemonic::Lea {
                for m in i.used_memory() {
                    if reads(m.access()) {
                        match mem_loc(ins, m.memory_size().size() as u32) {
                            Some(l) => needs.push(l),
                            None => stops.push(Stop {
                                at: ins.ip(),
                                what: format!("load through a pointer at {:#x}", ins.ip()),
                                why: "memory reached through a register is not tracked; the registers that form the address are followed".into(),
                                certainty: "inferred",
                            }),
                        }
                    }
                }
            }
        }
        // a plain write replaces the location; a read-modify-write keeps it
        for (w, pure) in &hit {
            if *pure || zeroing {
                tracked.retain(|x| !overlaps(&x.loc, &w.loc));
            }
        }
        needs.dedup();
        for n in &needs {
            push(&mut tracked, *n, ins.ip());
        }
        let why = if zeroing {
            format!("sets {} to zero", defined.join(", "))
        } else if needs.is_empty() && !matches!(ins.op1_kind(), OpKind::Register | OpKind::Memory) {
            format!("sets {} to a constant", defined.join(", "))
        } else if needs.is_empty() {
            format!("defines {}", defined.join(", "))
        } else {
            format!(
                "defines {}; needs {}",
                defined.join(", "),
                needs.iter().map(|l| loc_text(img, l)).collect::<Vec<_>>().join(", ")
            )
        };
        steps.push(Step { ip: ins.ip(), text: text_of(ins, &mut fmt), why, certainty: cert });
    }
    // what is still wanted at the start of the function
    for w in &tracked {
        let cert = if branch_between(fstart, w.need) { "inferred" } else { "proven" };
        let (what, why) = match w.loc {
            Loc::Reg(r) => match arg_regs(img).iter().position(|a| *a == r) {
                Some(n) => (
                    format!("{} is argument {} of {fname}", reg_name(r), n + 1),
                    "not written before this point: the caller supplies it".to_string(),
                ),
                None => (
                    format!("{} is not written in {fname} before this point", reg_name(r)),
                    "it holds what the caller left (a preserved register or an uninitialised value)".to_string(),
                ),
            },
            Loc::Slot(_, d, _) if d > 0 => (
                format!("{} is above the frame", loc_text(img, &w.loc)),
                "a stack argument or the return address area: set by the caller".to_string(),
            ),
            l @ Loc::Slot(..) => (
                format!("{} is never written in {fname} before this point", loc_text(img, &l)),
                "uninitialised, or written through a pointer that is not tracked".to_string(),
            ),
            l @ Loc::Global(_) => (
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
            text_of(&use_ins, &mut fmt),
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
        "proven: no branch lands between the definition and its use; inferred: another path may also set it (or the slot is rsp-relative). The walk is linear over the function's code.".into(),
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
