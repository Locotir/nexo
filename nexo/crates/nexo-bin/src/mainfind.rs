//! Finding the real `main`: the function the C runtime (or Rust's `lang_start`) hands control to.
//!
//! Every answer carries its evidence: which instruction passes the address and to what. A symbol named
//! `main` is reported as a symbol, never as proof; a pattern found in the C runtime of a Windows file is
//! labelled inferred.

use std::collections::BTreeSet;

use iced_x86::{
    FlowControl, Instruction, InstructionInfoFactory, InstructionInfoOptions, Mnemonic, OpAccess, OpKind, Register,
};

use crate::disasm::{walk, walk_bits};
use crate::pic32::Pic32;
use crate::refs::{written_address, Kind};
use crate::{Format, Image};

/// One candidate for the program's own entry point, with the reason it is believed.
#[derive(Debug, Clone)]
pub struct MainHit {
    pub addr: u64,
    /// `main`, `WinMain`, `Rust main` and so on.
    pub role: &'static str,
    /// How sure the tool is: `proven` (an instruction passes this address to the runtime start call),
    /// `inferred` (a pattern of the runtime), `symbol` (the file names it so).
    pub strength: &'static str,
    pub evidence: String,
}

/// Registers a call may overwrite (System V and Windows x64 together, so the answer holds for both).
pub(crate) fn clobbered_by_call(r: Register) -> bool {
    matches!(
        r.full_register(),
        Register::RAX
            | Register::RCX
            | Register::RDX
            | Register::RSI
            | Register::RDI
            | Register::R8
            | Register::R9
            | Register::R10
            | Register::R11
    )
}

/// Where the last write to `reg` before the instruction at `at` (inside the function that starts at
/// `start`) got its value, when it is a plain address or number: `(value, address of that instruction)`.
/// None when the value is not a constant, when a call in between could have changed the register, or when a
/// branch from elsewhere lands between the write and `at` (the write might not have run).
pub(crate) fn reg_const_before(img: &Image, start: u64, at: u64, reg: Register) -> Option<(u64, u64)> {
    let len = usize::try_from(at.checked_sub(start)?).ok()?.min(1 << 20);
    let bytes = img.read_at(start, len)?;
    let reg = reg.full_register();
    let mut info = InstructionInfoFactory::new();
    let mut targets: BTreeSet<u64> = BTreeSet::new();
    // (value if constant, address of the writing instruction)
    let mut last: Option<(Option<u64>, u64)> = None;
    walk(bytes, start, |ins, _| {
        if ins.is_invalid() {
            return;
        }
        if matches!(ins.flow_control(), FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch)
            && ins.op0_kind() == OpKind::NearBranch64
        {
            targets.insert(ins.near_branch_target());
        }
        if ins.flow_control() == FlowControl::Call || ins.flow_control() == FlowControl::IndirectCall {
            if clobbered_by_call(reg) {
                last = Some((None, ins.ip()));
            }
            return;
        }
        let i = info.info_options(ins, InstructionInfoOptions::NO_MEMORY_USAGE);
        let writes = i.used_registers().iter().any(|u| {
            u.register().full_register() == reg
                && matches!(
                    u.access(),
                    OpAccess::Write | OpAccess::ReadWrite | OpAccess::CondWrite | OpAccess::ReadCondWrite
                )
        });
        if writes {
            last = Some((constant_of(ins, reg), ins.ip()));
        }
    });
    let (Some(v), at_ins) = last? else { return None };
    // a branch from somewhere else into the stretch after the write: the write may have been skipped
    if targets.range(at_ins + 1..=at).next().is_some() {
        return None;
    }
    Some((v, at_ins))
}

/// x86-32: the value given as the first argument of the call at `at` (inside the function that starts at
/// `start`), when it is a plain address or number: `(value, address of the instruction that gives it)`.
/// The first argument of a cdecl or stdcall call is the last one pushed (or stored at `[esp]`); a
/// register or a memory slot counts when position-independent code makes its value known. None when a
/// call in between takes the pushes, or a branch from elsewhere lands between the push and `at`.
pub(crate) fn first_push_before(img: &Image, start: u64, at: u64) -> Option<(u64, u64)> {
    let len = usize::try_from(at.checked_sub(start)?).ok()?.min(1 << 20);
    let bytes = img.read_at(start, len)?;
    let mut pic = Pic32::new();
    let mut targets: BTreeSet<u64> = BTreeSet::new();
    let mut last: Option<(Option<u64>, u64)> = None;
    let word = |a: u64| img.read_at(a, 4).and_then(|b| b.try_into().ok()).map(|b| u64::from(u32::from_le_bytes(b)));
    walk_bits(32, bytes, start, |ins, _| {
        if !ins.is_invalid() {
            if matches!(ins.flow_control(), FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch)
                && ins.op0_kind() == OpKind::NearBranch32
            {
                targets.insert(ins.near_branch_target());
            }
            if matches!(ins.flow_control(), FlowControl::Call | FlowControl::IndirectCall) {
                last = None;
            }
            let operand = |op: u32| match ins.op_kind(op) {
                OpKind::Immediate32 | OpKind::Immediate8to32 => Some(ins.immediate(op) & 0xffff_ffff),
                OpKind::Register => pic.value(ins.op_register(op)).map(u64::from),
                _ => None,
            };
            if ins.mnemonic() == Mnemonic::Push {
                let v = match ins.op0_kind() {
                    OpKind::Memory => {
                        pic.resolve(img, ins).or_else(|| written_address(ins)).filter(|x| x.1).and_then(|x| word(x.0))
                    }
                    _ => operand(0),
                };
                last = Some((v, ins.ip()));
            } else if ins.mnemonic() == Mnemonic::Mov
                && ins.op0_kind() == OpKind::Memory
                && ins.memory_base() == Register::ESP
                && ins.memory_index() == Register::None
                && ins.memory_displacement32() == 0
            {
                last = Some((operand(1), ins.ip()));
            }
        }
        pic.step(img, ins);
    });
    let (Some(v), at_ins) = last? else { return None };
    if targets.range(at_ins + 1..=at).next().is_some() {
        return None;
    }
    Some((v, at_ins))
}

/// The first argument of the call at `at` when it is a constant, with the words that say how it is given:
/// `(value, address of the instruction, "sets rdi = 0x401136")`. `reg` is the register of that argument
/// in x86-64 code; x86-32 code pushes it.
fn first_arg(img: &Image, start: u64, at: u64, reg: Register) -> Option<(u64, u64, String)> {
    if img.bits == 32 {
        let (v, w) = first_push_before(img, start, at)?;
        return Some((v, w, format!("pushes {v:#x} as the first argument")));
    }
    let (v, w) = reg_const_before(img, start, at, reg)?;
    Some((v, w, format!("sets {} = {v:#x}", format!("{reg:?}").to_lowercase())))
}

/// Where a jump that is not a function of its own goes (`__wrap_main: jmp main` in the start code of
/// x86-32 glibc): the target of an unconditional near jump at `a` when `a` starts no function, else `a`
/// itself, with a note for the evidence.
fn through_jump(img: &Image, a: u64) -> (u64, String) {
    let starts = |x: u64| img.funcs().binary_search_by_key(&x, |f| f.start).is_ok();
    if starts(a) {
        return (a, String::new());
    }
    let Some(b) = img.read_at(a, 15).or_else(|| img.read_at(a, 5)) else { return (a, String::new()) };
    let ins = iced_x86::Decoder::with_ip(img.bits, b, a, iced_x86::DecoderOptions::NONE).decode();
    if ins.mnemonic() == Mnemonic::Jmp && matches!(ins.op0_kind(), OpKind::NearBranch32 | OpKind::NearBranch64) {
        let t = ins.near_branch_target();
        if starts(t) {
            return (t, format!(", which jumps to {t:#x}"));
        }
    }
    (a, String::new())
}

/// The constant an instruction puts in `reg` (a number or an address), when it is one.
fn constant_of(ins: &Instruction, reg: Register) -> Option<u64> {
    if ins.op0_kind() != OpKind::Register || ins.op0_register().full_register() != reg {
        return None;
    }
    match ins.mnemonic() {
        Mnemonic::Mov => match ins.op1_kind() {
            OpKind::Immediate32 | OpKind::Immediate32to64 | OpKind::Immediate64 | OpKind::Immediate8to64 => {
                Some(ins.immediate(1))
            }
            _ => None,
        },
        Mnemonic::Lea => {
            if ins.is_ip_rel_memory_operand() {
                Some(ins.ip_rel_memory_address())
            } else if ins.memory_base() == Register::None && ins.memory_index() == Register::None {
                Some(ins.memory_displacement64())
            } else {
                None
            }
        }
        Mnemonic::Xor | Mnemonic::Sub
            if ins.op1_kind() == OpKind::Register && ins.op1_register().full_register() == reg =>
        {
            Some(0)
        }
        _ => None,
    }
}

/// Names the C runtime of Windows calls to get `argc` and `argv`, then the program's main follows.
const ARGV_SOURCES: [&str; 10] = [
    "__p__environ",
    "__p__wenviron",
    "__getmainargs",
    "__wgetmainargs",
    "_get_initial_narrow_environment",
    "_get_initial_wide_environment",
    "__p___argv",
    "__p___wargv",
    "__p___argc",
    "_get_narrow_winmain_command_line",
];

/// Runtime helpers that are not the program's main.
fn is_runtime_helper(name: &str) -> bool {
    if name == "__main" {
        return true;
    }
    let n = name.trim_start_matches('_');
    [
        "cexit",
        "exit",
        "amsg_exit",
        "initterm",
        "fpreset",
        "security",
        "set_app_type",
        "atexit",
        "onexit",
        "scrt",
        "pei386",
        "mingw",
        "invalid_parameter",
        "setusermatherr",
        "acrt",
        "guard",
        "tls",
        "fmode",
        "commode",
        "setargv",
        "p__",
    ]
    .iter()
    .any(|h| n.starts_with(h))
}

fn import_named(img: &Image, addr: u64) -> Option<&str> {
    if let Some(&i) = img.slots.get(&addr) {
        return Some(&img.imports[i].name);
    }
    let f = img.funcs().iter().find(|f| f.start == addr)?;
    f.name
        .strip_suffix("@plt")
        .or_else(|| img.slots.values().map(|&i| img.imports[i].name.as_str()).find(|n| *n == f.name))
}

/// The addresses a call to the import or function called `name` can target (its PLT stub, slot, or start).
fn targets_named(img: &Image, pred: impl Fn(&str) -> bool) -> Vec<u64> {
    let mut v: Vec<u64> = Vec::new();
    for imp in img.imports.iter().filter(|i| pred(i.name.split('@').next().unwrap_or(&i.name))) {
        v.extend(imp.slot);
        v.extend(
            img.funcs().iter().filter(|f| f.name.strip_suffix("@plt") == Some(imp.name.as_str())).map(|f| f.start),
        );
        // a thunk named after the import (PE)
        v.extend(img.funcs().iter().filter(|f| f.name == imp.name).map(|f| f.start));
    }
    v.extend(img.funcs().iter().filter(|f| pred(&f.name) && !f.name.ends_with("@plt")).map(|f| f.start));
    v.sort_unstable();
    v.dedup();
    v
}

fn name_of(img: &Image, a: u64) -> String {
    img.func_at(a).map(|f| img.func_name(f).to_string()).unwrap_or_else(|| format!("{a:#x}"))
}

/// Everything found for the program's main, strongest first.
pub fn find_main(img: &Image) -> Vec<MainHit> {
    // Mach-O: LC_MAIN names main itself
    let lc_main = img.macho.as_ref().and_then(|m| m.main).map(|a| MainHit {
        addr: a,
        role: "main",
        strength: "proven",
        evidence: format!("LC_MAIN names {a:#x}: the loader calls main there"),
    });
    if img.arm64 {
        return lc_main.into_iter().collect();
    }
    let mut hits: Vec<MainHit> = lc_main.into_iter().collect();
    let _ = img.refs();
    let refs = img.refs();
    let code_start = |a: u64| img.funcs().binary_search_by_key(&a, |f| f.start).is_ok();

    // Rust: the first argument of std::rt::lang_start is the program's main
    let lang = targets_named(img, |n| n == "std::rt::lang_start" || n.starts_with("std::rt::lang_start::<"));
    for t in lang {
        for r in refs.at(t).iter().filter(|r| r.kind == Kind::Call) {
            let Some(h) = img.func_at(r.src) else { continue };
            let reg = if img.format == Format::Pe { Register::RCX } else { Register::RDI };
            if let Some((v, w, what)) = first_arg(img, h.start, r.src, reg) {
                if code_start(v) {
                    hits.push(MainHit {
                        addr: v,
                        role: "Rust main",
                        strength: "proven",
                        evidence: format!(
                            "{w:#x} {what} before the call to std::rt::lang_start at {:#x} (in {})",
                            r.src,
                            name_of(img, h.start)
                        ),
                    });
                }
            }
        }
    }

    // C on ELF: the first argument of __libc_start_main
    if img.format == Format::Elf {
        let lsm = targets_named(img, |n| n.starts_with("__libc_start_main"));
        let mut found_call = false;
        for t in lsm {
            for r in refs.at(t).iter().filter(|r| r.kind == Kind::Call) {
                found_call = true;
                let Some(h) = img.func_at(r.src) else { continue };
                if let Some((v, w, what)) = first_arg(img, h.start, r.src, Register::RDI) {
                    let (v, via) = through_jump(img, v);
                    if code_start(v) {
                        hits.push(MainHit {
                            addr: v,
                            role: "main",
                            strength: "proven",
                            evidence: format!(
                                "{w:#x} {what}{via} before the call to __libc_start_main at {:#x} (in {})",
                                r.src,
                                name_of(img, h.start)
                            ),
                        });
                    }
                }
            }
        }
        if !found_call && hits.is_empty() {
            if let Some(h) = entry_fallback(img) {
                hits.push(h);
            }
        }
    }

    // Windows: the call that follows the argv source calls in the C runtime
    if img.format == Format::Pe {
        hits.extend(pe_crt_main(img));
    }

    // the file's own symbols
    for s in img
        .symbols
        .iter()
        .filter(|s| s.func && matches!(s.name.as_str(), "main" | "wmain" | "WinMain" | "wWinMain" | "_main"))
    {
        hits.push(MainHit {
            addr: s.addr,
            role: if s.name.contains("Win") { "WinMain" } else { "main" },
            strength: "symbol",
            evidence: format!("the symbol table names {:#x} {}", s.addr, s.name),
        });
    }
    // one line per address: the strongest evidence stays, the others are added to it
    let rank = |s: &str| match s {
        "proven" => 0,
        "inferred" => 1,
        _ => 2,
    };
    hits.sort_by_key(|h| rank(h.strength));
    let mut out: Vec<MainHit> = Vec::new();
    for h in hits {
        if let Some(o) = out.iter_mut().find(|o| o.addr == h.addr) {
            if o.strength != h.strength || !o.evidence.contains(&h.evidence) {
                o.evidence.push_str(&format!("; also {}", h.evidence));
            }
        } else {
            out.push(h);
        }
    }
    out
}

/// A stripped static file: the entry function passes a function address as the first argument of its
/// last call (in rdi, or pushed in x86-32 code). Inferred only.
fn entry_fallback(img: &Image) -> Option<MainHit> {
    let f = img.func_at(img.entry)?;
    let r = img.refs().from(f.start, f.end).filter(|r| r.kind == Kind::Call).last()?;
    let (v, w, what) = first_arg(img, f.start, r.src, Register::RDI)?;
    let (v, via) = through_jump(img, v);
    img.funcs().binary_search_by_key(&v, |x| x.start).ok()?;
    Some(MainHit {
        addr: v,
        role: "main",
        strength: "inferred",
        evidence: format!(
            "the entry function {what}{via} at {w:#x} before its last call at {:#x}; that call has no name \
             (stripped), probably __libc_start_main",
            r.src
        ),
    })
}

/// The C runtime start of a Windows file: a function within three calls of the entry that gets argc/argv,
/// then calls the program's main.
fn pe_crt_main(img: &Image) -> Vec<MainHit> {
    let refs = img.refs();
    let argv_targets = targets_named(img, |n| ARGV_SOURCES.contains(&n));
    let Some(entry) = img.func_at(img.entry) else { return Vec::new() };
    // functions within three direct calls of the entry
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut level = vec![entry.start];
    seen.insert(entry.start);
    let mut order = vec![entry.start];
    for _ in 0..3 {
        let mut next = Vec::new();
        for s in &level {
            let Some(f) = img.funcs().binary_search_by_key(s, |f| f.start).ok().map(|i| &img.funcs()[i]) else {
                continue;
            };
            for r in refs.from(f.start, f.end).filter(|r| matches!(r.kind, Kind::Call | Kind::Jump)) {
                if let Some(g) = img.func_at(r.target) {
                    if seen.insert(g.start) {
                        next.push(g.start);
                        order.push(g.start);
                    }
                }
            }
        }
        level = next;
    }
    let mut out = Vec::new();
    for &s in &order {
        let Some(f) = img.func_at(s) else { continue };
        let calls: Vec<_> = refs.from(f.start, f.end).filter(|r| r.kind == Kind::Call).collect();
        let Some(last_arg) = calls.iter().rposition(|r| argv_targets.contains(&r.target)) else { continue };
        let source = import_named(img, calls[last_arg].target).unwrap_or("an argv call").to_string();
        let winmain = source == "_get_narrow_winmain_command_line";
        for r in &calls[last_arg + 1..] {
            if import_named(img, r.target).is_some() || img.slots.contains_key(&r.target) {
                continue;
            }
            let Some(t) = img.func_at(r.target).filter(|t| t.start == r.target) else { continue };
            if is_runtime_helper(img.func_name(t)) && !t.name.starts_with("sub_") {
                continue;
            }
            out.push(MainHit {
                addr: t.start,
                role: if winmain { "WinMain" } else { "main" },
                strength: "inferred",
                evidence: format!(
                    "{} calls {source} at {:#x}, then the first call to a program function is at {:#x} to {:#x} \
                     (C runtime pattern, entry {:#x} -> {})",
                    img.func_name(f),
                    calls[last_arg].src,
                    r.src,
                    t.start,
                    img.entry,
                    img.func_name(f)
                ),
            });
            break;
        }
        if !out.is_empty() {
            return out;
        }
    }
    // no argv call to anchor on (the runtime reads them another way): the call that is given argc, argv and
    // envp (rcx, rdx, r8) in a function that goes on to call exit; x86-64 only (x86-32 pushes them)
    for &s in order.iter().filter(|_| img.bits == 64) {
        let Some(f) = img.func_at(s) else { continue };
        let calls: Vec<_> = refs.from(f.start, f.end).filter(|r| r.kind == Kind::Call).collect();
        let exit_at = calls.iter().position(|r| import_named(img, r.target).is_some_and(|n| EXITS.contains(&n)));
        let Some(exit_at) = exit_at else { continue };
        for r in &calls[..exit_at] {
            let Some(t) = img.func_at(r.target).filter(|t| t.start == r.target) else { continue };
            if import_named(img, t.start).is_some()
                || (is_runtime_helper(img.func_name(t)) && !t.name.starts_with("sub_"))
            {
                continue;
            }
            let set =
                [Register::RCX, Register::RDX, Register::R8].iter().all(|&g| reg_written_near(img, f.start, r.src, g));
            if set {
                out.push(MainHit {
                    addr: t.start,
                    role: "main",
                    strength: "inferred",
                    evidence: format!(
                        "{} sets rcx, rdx and r8 (argc, argv, envp) before the call at {:#x} to {:#x} and calls exit \
                         afterwards (C runtime pattern, entry {:#x})",
                        img.func_name(f),
                        r.src,
                        t.start,
                        img.entry
                    ),
                });
            }
        }
        if !out.is_empty() {
            // the last such call before the exit is main; earlier ones are start-up helpers
            let last = out.pop().into_iter().collect();
            return last;
        }
    }
    out
}

/// Names of the calls that end a program.
const EXITS: [&str; 6] = ["exit", "_exit", "_cexit", "ExitProcess", "quick_exit", "_Exit"];

/// Whether one of the ten instructions before `at` writes `reg` (the full register).
fn reg_written_near(img: &Image, start: u64, at: u64, reg: Register) -> bool {
    let lo = at.saturating_sub(48).max(start);
    let Some(bytes) = img.read_at(lo, (at - lo) as usize) else { return false };
    let mut info = InstructionInfoFactory::new();
    let mut found = false;
    walk(bytes, lo, |ins, _| {
        let i = info.info_options(ins, InstructionInfoOptions::NO_MEMORY_USAGE);
        found |= i.used_registers().iter().any(|u| {
            u.register().full_register() == reg
                && matches!(u.access(), OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite)
        });
    });
    found
}
