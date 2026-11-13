//! Machine code to the IR: decode a function, find its blocks, lift each instruction.
//!
//! Stack addresses are folded while lifting: the stack pointer (and a frame pointer set from it) is
//! tracked as an offset from its value at entry, so `[rsp+8]` is already `StackAddr(-0x20)`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use iced_x86::{ConditionCode, FlowControl, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind, Register};

use super::ir::*;
use crate::disasm::{formatter, walk};

mod lifter;
mod slots;
mod switch;
use self::slots::*;
use self::switch::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Abi {
    SysV,
    Win64,
}

impl Abi {
    pub fn arg_regs(self) -> &'static [u8] {
        match self {
            Abi::SysV => &[7, 6, 2, 1, 8, 9],
            Abi::Win64 => &[1, 2, 8, 9],
        }
    }

    pub fn caller_saved(self) -> &'static [u8] {
        match self {
            Abi::SysV => &[1, 2, 6, 7, 8, 9, 10, 11],
            Abi::Win64 => &[1, 2, 8, 9, 10, 11],
        }
    }

    pub fn callee_saved(self, r: u8) -> bool {
        match self {
            Abi::SysV => matches!(r, 3 | 5 | 12..=15),
            Abi::Win64 => matches!(r, 3 | 5 | 6 | 7 | 12..=15),
        }
    }

    /// Number of xmm registers a call clobbers.
    pub fn xmm_clobbered(self) -> u8 {
        match self {
            Abi::SysV => 16,
            Abi::Win64 => 6,
        }
    }

    pub fn arg_count_max(self) -> usize {
        self.arg_regs().len()
    }

    /// Offset from the entry stack pointer where the first stack argument sits.
    pub fn stack_arg_base(self) -> i64 {
        match self {
            Abi::SysV => 8,
            Abi::Win64 => 40,
        }
    }
}

/// What the lifter needs to know about the file around the function.
pub trait Env {
    fn read(&self, addr: u64, len: usize) -> Option<&[u8]>;
    /// The name of the function or import a call lands on (no `@plt`).
    fn name_of(&self, addr: u64) -> Option<String>;
    /// The import whose pointer slot is at this address.
    fn import_slot(&self, slot: u64) -> Option<usize>;
    fn import_name(&self, i: usize) -> String;
    fn image_base(&self) -> u64;
    fn is_pe(&self) -> bool;
}

const NORETURN: &[&str] = &[
    "exit",
    "_exit",
    "_Exit",
    "abort",
    "__stack_chk_fail",
    "__assert_fail",
    "__assert",
    "longjmp",
    "_longjmp",
    "__longjmp_chk",
    "pthread_exit",
    "ExitProcess",
    "ExitThread",
    "TerminateProcess",
    "__fortify_fail",
    "__chk_fail",
    "g_assertion_message",
    "g_assertion_message_expr",
    "g_error",
    "__cxa_throw",
    "__cxa_rethrow",
    "quick_exit",
    "_Unwind_Resume",
    "__security_check_cookie_fail",
    "__report_gsfailure",
    "RaiseException",
];

pub fn is_noreturn(name: &str) -> bool {
    NORETURN.contains(&name)
}

#[derive(Debug, Clone)]
pub struct Lifted {
    pub start: u64,
    pub end: u64,
    pub abi: Abi,
    pub blocks: Vec<Block>,
    /// Offsets (from the entry stack pointer) of slots that hold a saved callee register.
    pub saves: BTreeSet<i64>,
    /// Jump tables found (count), and indirect jumps left alone.
    pub tables: usize,
    pub unresolved_jumps: usize,
    /// Instructions that were not lifted.
    pub raw: usize,
    pub instructions: usize,
}

struct SwitchInfo {
    /// Index (into the instruction list) of the first instruction the switch hides.
    hide_from: usize,
    targets: Vec<u64>,
    /// Where the index sits before the hidden instructions: a register (64-bit number).
    idx_reg: u8,
    idx_bits: Bits,
    /// Default target and bound, from the compare that guards the jump.
    bound: Option<u64>,
}

struct Decoded {
    ins: Vec<Instruction>,
    raw: Vec<(usize, usize)>,
    by_ip: HashMap<u64, usize>,
}

fn decode(env: &dyn Env, start: u64, end: u64) -> Decoded {
    let len = end.saturating_sub(start) as usize;
    let mut ins = Vec::new();
    let mut by_ip = HashMap::new();
    if let Some(bytes) = env.read(start, len) {
        walk(bytes, start, |i, b| {
            by_ip.insert(i.ip(), ins.len());
            let _ = b;
            ins.push(*i);
        });
    }
    Decoded { ins, raw: Vec::new(), by_ip }
}

fn reg_num(r: Register) -> Option<(u8, Bits, bool)> {
    use Register::*;
    if r.is_gpr64() {
        return Some((r.number() as u8, 64, false));
    }
    if r.is_gpr32() {
        return Some((r.number() as u8, 32, false));
    }
    if r.is_gpr16() {
        return Some((r.number() as u8, 16, false));
    }
    if r.is_gpr8() {
        let high = matches!(r, AH | CH | DH | BH);
        let n = if high { (r as u32 - AH as u32) as u8 } else { r.full_register().number() as u8 };
        // iced numbers the 8-bit registers by position in their own group (spl is 8, r14b is 18)
        return Some((n, 8, high));
    }
    Option::None
}

fn gpr_number(r: Register) -> u8 {
    r.full_register().number() as u8
}

fn mem_size_bits(i: &Instruction) -> Bits {
    let s = i.memory_size().size();
    if s == 0 {
        64
    } else {
        (s * 8).min(128) as Bits
    }
}

fn cc_of(i: &Instruction) -> Cc {
    let c = i.condition_code();
    if c == ConditionCode::None {
        Cc::E
    } else {
        Cc(c as u8 - 1)
    }
}

fn flag_cond(cc: Cc) -> Expr {
    Expr::Cond(cc, Box::new(Expr::Var(Var::FlagOps, 0)), Box::new(Expr::Var(Var::FlagRes, 0)))
}

#[derive(Clone, Copy, Default)]
struct State {
    rsp: Option<i64>,
    rbp: Option<i64>,
}

struct Lifter<'a> {
    env: &'a dyn Env,
    abi: Abi,
    start: u64,
    end: u64,
    stmts: Vec<S>,
    st: State,
    saves: BTreeSet<i64>,
    escapes: BTreeSet<i64>,
    raw: usize,
    in_prologue: bool,
    info: InstructionInfoFactory,
    fmt: iced_x86::IntelFormatter,
}

fn b(e: Expr) -> Box<Expr> {
    Box::new(e)
}

const RAX_V: Var = Var::Reg(0);

fn is_cond_jump(ins: &Instruction) -> bool {
    ins.flow_control() == FlowControl::ConditionalBranch
}

pub fn lift(env: &dyn Env, start: u64, end: u64, abi: Abi) -> Lifted {
    let d = decode(env, start, end);
    let in_fn = |a: u64| a >= start && a < end;
    // 1. blocks: leaders by recursive descent
    let mut leaders: BTreeSet<u64> = BTreeSet::new();
    leaders.insert(start);
    let mut seen: HashSet<usize> = HashSet::new();
    let mut work = vec![start];
    let mut switches: HashMap<usize, SwitchInfo> = HashMap::new();
    let mut externs: BTreeSet<u64> = BTreeSet::new();
    while let Some(a) = work.pop() {
        let Some(&first) = d.by_ip.get(&a) else { continue };
        let mut k = first;
        loop {
            if !seen.insert(k) {
                break;
            }
            let Some(ins) = d.ins.get(k) else { break };
            if ins.is_invalid() {
                break;
            }
            match ins.flow_control() {
                FlowControl::Next | FlowControl::Interrupt | FlowControl::XbeginXabortXend => k += 1,
                FlowControl::Call | FlowControl::IndirectCall => {
                    let noret = match ins.op0_kind() {
                        OpKind::NearBranch64 => env.name_of(ins.near_branch_target()).is_some_and(|n| is_noreturn(&n)),
                        OpKind::Memory if ins.is_ip_rel_memory_operand() => env
                            .import_slot(ins.ip_rel_memory_address())
                            .is_some_and(|i| is_noreturn(&env.import_name(i))),
                        _ => false,
                    };
                    if noret {
                        break;
                    }
                    k += 1;
                }
                FlowControl::UnconditionalBranch => {
                    let t = ins.near_branch_target();
                    if in_fn(t) && leaders.insert(t) {
                        work.push(t);
                    }
                    break;
                }
                FlowControl::ConditionalBranch => {
                    let t = ins.near_branch_target();
                    if in_fn(t) {
                        if leaders.insert(t) {
                            work.push(t);
                        }
                    } else {
                        externs.insert(t);
                    }
                    let nxt = ins.next_ip();
                    if leaders.insert(nxt) {
                        work.push(nxt);
                    }
                    break;
                }
                FlowControl::IndirectBranch => {
                    if let Some(sw) = find_switch(env, &d, k, start, end) {
                        for &t in &sw.targets {
                            if leaders.insert(t) {
                                work.push(t);
                            }
                        }
                        if sw.hide_from < k {
                            // the hidden instructions may start a block of their own: no, they stay in this block
                        }
                        switches.insert(k, sw);
                    }
                    break;
                }
                FlowControl::Return | FlowControl::Exception => break,
            }
        }
    }
    // The first leader inside a straight run splits it: a leader reached by fall-through starts a block.
    let ids: BTreeMap<u64, usize> = leaders.iter().enumerate().map(|(i, &a)| (a, i)).collect();
    let n_local = ids.len();
    let ext_ids: BTreeMap<u64, usize> = externs.iter().enumerate().map(|(i, &a)| (a, n_local + i)).collect();
    let mut blocks: Vec<Option<Block>> = vec![None; n_local + ext_ids.len()];
    for (&a, &id) in &ext_ids {
        blocks[id] = Some(Block {
            addr: a,
            stmts: Vec::new(),
            term: Term::TailCall(Expr::Call(Box::new(Call {
                target: Target::Direct(a),
                args: Vec::new(),
                addr: a,
                known: false,
            }))),
            term_addr: a,
        });
    }
    let mut lf = Lifter {
        env,
        abi,
        start,
        end,
        stmts: Vec::new(),
        st: State::default(),
        saves: BTreeSet::new(),
        escapes: BTreeSet::new(),
        raw: 0,
        in_prologue: true,
        info: InstructionInfoFactory::new(),
        fmt: formatter(),
    };
    let mut states: HashMap<usize, State> = HashMap::new();
    states.insert(0, State { rsp: Some(0), rbp: None });
    let mut queue = vec![0usize];
    let leader_list: Vec<u64> = leaders.iter().copied().collect();
    let mut tables = 0;
    let mut unresolved = 0;
    while let Some(id) = queue.pop() {
        if blocks[id].is_some() {
            continue;
        }
        let Some(&st0) = states.get(&id) else { continue };
        let baddr = leader_list[id];
        let Some(&first) = d.by_ip.get(&baddr) else {
            blocks[id] = Some(Block { addr: baddr, stmts: Vec::new(), term: Term::Stop, term_addr: baddr });
            continue;
        };
        lf.stmts.clear();
        lf.st = st0;
        lf.in_prologue = id == 0;
        let mut k = first;
        let term: Option<(Term, u64)>;
        let id_of = |a: u64| ids.get(&a).copied().or_else(|| ext_ids.get(&a).copied());
        let mut succ_states: Vec<(usize, State)> = Vec::new();
        loop {
            let Some(ins) = d.ins.get(k) else {
                term = Some((Term::Stop, baddr));
                break;
            };
            let ip = ins.ip();
            if k != first && leaders.contains(&ip) {
                let t = ids[&ip];
                succ_states.push((t, lf.st));
                term = Some((Term::Jump(t), ip));
                break;
            }
            if ins.is_invalid() {
                term = Some((Term::Stop, ip));
                break;
            }
            // a switch hides its table instructions: the index is saved, the rest is skipped
            if let Some((swk, sw)) = switches.iter().find(|(_, sw)| sw.hide_from == k) {
                let jmp_ip = d.ins[*swk].ip();
                let idx = Expr::Var(Var::Reg(sw.idx_reg), sw.idx_bits);
                lf.assign(ip, Var::Tmp(1), idx.zext(64));
                let scr = Expr::Var(Var::Tmp(1), sw.idx_bits);
                let mut cases: Vec<(u64, BlockId)> = Vec::new();
                for (v, t) in sw.targets.iter().enumerate() {
                    cases.push((v as u64, ids[t]));
                    succ_states.push((ids[t], lf.st));
                }
                tables += 1;
                term = Some((Term::Switch(scr, cases, None), jmp_ip));
                break;
            }
            match ins.flow_control() {
                FlowControl::ConditionalBranch => {
                    let t = ins.near_branch_target();
                    let tid = id_of(t).unwrap_or(0);
                    let fid = ids.get(&ins.next_ip()).copied().unwrap_or(0);
                    succ_states.push((tid, lf.st));
                    succ_states.push((fid, lf.st));
                    let c = flag_cond(cc_of(ins));
                    let c = if ins.mnemonic() == Mnemonic::Jrcxz || ins.mnemonic() == Mnemonic::Jecxz {
                        Expr::Cmp(Cmp::Eq, b(Expr::Var(Var::Reg(1), 64)), b(Expr::Const(0, 64)))
                    } else {
                        c
                    };
                    term = Some((Term::Branch(c, tid, fid), ip));
                    break;
                }
                FlowControl::UnconditionalBranch => {
                    let t = ins.near_branch_target();
                    if in_fn(t) {
                        let tid = ids[&t];
                        succ_states.push((tid, lf.st));
                        term = Some((Term::Jump(tid), ip));
                    } else {
                        let call = lf.call_expr(ins);
                        term = Some((Term::TailCall(call), ip));
                    }
                    break;
                }
                FlowControl::IndirectBranch => {
                    let call = lf.call_expr(ins);
                    if let Expr::Call(c) = &call {
                        if matches!(c.target, Target::Import(_)) {
                            term = Some((Term::TailCall(call), ip));
                            break;
                        }
                    }
                    // Only a frame known to be gone makes the jump a tail call (an unknown rsp is not "gone"), and
                    // only when the target is not picked from a table of this function: a jump through a table
                    // or a register loaded from one is a switch we could not recover, so it stays unresolved.
                    let frame_gone = lf.st.rsp.is_some_and(|r| r >= 0);
                    if frame_gone && !jumps_through_a_table(env, &d, k, start, end) {
                        term = Some((Term::TailCall(call), ip));
                        break;
                    }
                    unresolved += 1;
                    let t = lf.read(ins, 0, 64);
                    term = Some((Term::Indirect(t), ip));
                    break;
                }
                FlowControl::Return => {
                    term = Some((Term::Return(Some(Expr::Var(RAX_V, 64))), ip));
                    break;
                }
                FlowControl::Exception => {
                    term = Some((Term::Stop, ip));
                    break;
                }
                _ => {
                    let stop = lf.lift(ins);
                    k += 1;
                    if stop {
                        term = Some((Term::Stop, ip));
                        break;
                    }
                    // a noreturn call ends the block
                    if let Some(S { st: Stmt::Assign(_, e), .. }) = lf.stmts.iter().rev().nth(1) {
                        if ins.flow_control() == FlowControl::Call || ins.flow_control() == FlowControl::IndirectCall {
                            if let Some(n) = lf.call_target_name(e) {
                                if is_noreturn(&n) {
                                    term = Some((Term::Stop, ip));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        let (t, ta) = term.unwrap_or((Term::Stop, baddr));
        for (s, st) in succ_states {
            states.entry(s).or_insert(st);
            queue.push(s);
        }
        blocks[id] = Some(Block { addr: baddr, stmts: std::mem::take(&mut lf.stmts), term: t, term_addr: ta });
    }
    let blocks: Vec<Block> = blocks
        .into_iter()
        .enumerate()
        .map(|(i, x)| {
            x.unwrap_or(Block {
                addr: leader_list.get(i).copied().unwrap_or(0),
                stmts: Vec::new(),
                term: Term::Stop,
                term_addr: 0,
            })
        })
        .collect();
    let mut out = Lifted {
        start,
        end,
        abi,
        blocks,
        saves: lf.saves.clone(),
        tables,
        unresolved_jumps: unresolved,
        raw: lf.raw,
        instructions: d.ins.len(),
    };
    promote_slots(&mut out, &lf.escapes);
    let _ = (lf.start, lf.end, is_cond_jump);
    out
}
