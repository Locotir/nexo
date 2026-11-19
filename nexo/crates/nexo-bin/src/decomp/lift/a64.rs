//! The AArch64 lifter: one instruction at a time to the same IR the x86 lifter produces.
//!
//! Registers are `Reg(0..=30)` for x0..x30 and `Reg(31)` for sp. The stack pointer and a frame pointer set
//! from it are tracked as offsets from the entry sp, like rsp and rbp on x86. A write to a w register zeroes
//! the upper half. Flags follow the x86 model (`FlagOps` and `FlagRes`); the arm conditions are mapped onto
//! x86 condition codes, see [`A64::cond`]. What is not lifted becomes a `Stmt::Raw`, exactly as on x86.

use super::*;
use crate::a64::switch::{self as a64_switch, Table};
use crate::a64::{decode as decode_word, Flow as AFlow, Ins, Mem, Opd, Why};

#[derive(Clone, Copy)]
struct State {
    sp: Option<i64>,
    fp: Option<i64>,
    /// The flags come from an add-like or logic instruction (carry is a carry, not a NOT-borrow).
    add_flags: bool,
}

struct A64<'a> {
    env: &'a dyn Env,
    pc: u64,
    stmts: Vec<S>,
    st: State,
    saves: BTreeSet<i64>,
    escapes: BTreeSet<i64>,
    raw: usize,
    in_prologue: bool,
    /// Registers holding a known constant inside the current block (adrp, mov, add of those).
    known: [Option<u64>; 32],
    /// Registers holding the pointer stored in an import slot.
    imp: [Option<usize>; 32],
}

const NOPS: &[&str] = &[
    "nop", "hint", "yield", "wfe", "wfi", "sev", "sevl", "bti", "paciasp", "pacibsp", "autiasp", "autibsp", "xpaclri",
    "prfm", "prfum", "dmb", "dsb", "isb", "csdb", "sb",
];

fn width(o: Opd) -> Bits {
    match o {
        Opd::Gpr { x: true, .. } => 64,
        _ => 32,
    }
}

fn fit32(e: Expr) -> Expr {
    if e.bits() > 32 {
        e.trunc(32)
    } else {
        e
    }
}

fn dec(o: Opd) -> Option<i64> {
    match o {
        Opd::Dec(i) | Opd::Imm(i) => Some(i),
        Opd::UImm(u) => Some(u as i64),
        _ => None,
    }
}

fn is_gpr(o: Opd) -> bool {
    matches!(o, Opd::Gpr { .. })
}

fn is_src(o: Opd) -> bool {
    matches!(o, Opd::Gpr { .. } | Opd::Imm(_) | Opd::UImm(_) | Opd::Dec(_))
}

impl A64<'_> {
    fn emit(&mut self, st: Stmt) {
        self.stmts.push(S { addr: self.pc, st });
    }

    fn assign(&mut self, v: Var, e: Expr) {
        self.emit(Stmt::Assign(v, e));
    }

    fn begin(&mut self, st: State, first: bool) {
        self.stmts.clear();
        self.st = st;
        self.in_prologue = first;
        self.known = [None; 32];
        self.imp = [None; 32];
    }

    /// Read register `n` (31 is the zero register unless `sp`).
    fn rd(&self, n: u8, x: bool, sp: bool) -> Expr {
        let bits: Bits = if x { 64 } else { 32 };
        if n == 31 {
            if !sp {
                return konst(0, bits);
            }
            let full = match self.st.sp {
                Some(o) => Expr::StackAddr(o),
                None => Expr::Var(Var::Reg(31), 64),
            };
            return if x { full } else { full.trunc(32) };
        }
        if let Some(v) = self.known[n as usize] {
            return konst(v, bits);
        }
        if n == 29 && x {
            if let Some(o) = self.st.fp {
                return Expr::StackAddr(o);
            }
        }
        Expr::Var(Var::Reg(n), bits)
    }

    /// Write register `n`; a w write zero-extends.
    fn wr(&mut self, n: u8, x: bool, sp: bool, v: Expr) {
        if n == 31 && !sp {
            return;
        }
        let v = if x { v.zext(64) } else { fit32(v).zext(64) };
        let v = simplify(v);
        if n == 31 {
            match v {
                Expr::StackAddr(o) => self.st.sp = Some(o),
                other => {
                    self.st.sp = None;
                    self.assign(Var::Reg(31), other);
                }
            }
            return;
        }
        if n == 29 {
            if let Expr::StackAddr(o) = v {
                self.st.fp = Some(o);
                self.known[29] = None;
                self.imp[29] = None;
                return;
            }
            self.st.fp = None;
        }
        self.known[n as usize] = v.as_const();
        self.imp[n as usize] = None;
        self.assign(Var::Reg(n), v);
    }

    fn write_dest(&mut self, d: Opd, v: Expr) {
        if let Opd::Gpr { n, x, sp } = d {
            self.wr(n, x, sp, v);
        }
    }

    fn val(&self, o: Opd, bits: Bits) -> Expr {
        match o {
            Opd::Gpr { n, x, sp } => self.rd(n, x, sp),
            Opd::Imm(i) | Opd::Dec(i) => konst(i as u64, bits),
            Opd::UImm(u) => konst(u, bits),
            Opd::Target(t) => konst(t, bits),
            _ => Expr::Unknown(bits),
        }
    }

    /// The flexible second operand at `ops[i]`: a register with its shift or extension, or an immediate.
    fn op2(&self, ins: &Ins, i: usize, bits: Bits) -> Expr {
        let o = ins.ops[i];
        let m = ins.ops.get(i + 1).copied().unwrap_or(Opd::None);
        let v = self.val(o, bits);
        match (o, m) {
            (Opd::Gpr { .. }, Opd::Shift { kind, amt }) if amt > 0 => {
                let k = konst(amt as u64, bits);
                match kind {
                    1 => Expr::bin(Bin::Shr, v, k),
                    2 => Expr::bin(Bin::Sar, v, k),
                    3 => Expr::Intr("ror", vec![v, k], bits),
                    _ => Expr::bin(Bin::Shl, v, k),
                }
            }
            (Opd::Gpr { .. }, Opd::Ext { kind, amt, .. }) => {
                let e = if kind < 8 {
                    let sb: Bits = [8, 16, 32, 64][(kind & 3) as usize];
                    let e = if sb < v.bits() { v.trunc(sb) } else { v };
                    if kind < 4 {
                        e.zext(bits)
                    } else {
                        e.sext(bits)
                    }
                } else {
                    v
                };
                if amt > 0 {
                    Expr::bin(Bin::Shl, e, konst(amt as u64, bits))
                } else {
                    e
                }
            }
            (Opd::Imm(n), Opd::Shift { amt, .. }) => konst((n as u64) << amt, bits),
            _ => v,
        }
    }

    // ---- flags -------------------------------------------------------------------------------------------

    fn flag_pair(&mut self, ops: Expr, res: Expr) {
        self.assign(Var::FlagOps, ops);
        self.assign(Var::FlagRes, res);
    }

    /// Set the flags of `a op c` (cmp, subs, cmn, adds); returns the result.
    fn arith_flags(&mut self, op: Bin, kind: FlagKind, a: Expr, c: Expr) -> Expr {
        let r = simplify(Expr::bin(op, a.clone(), c.clone()));
        self.flag_pair(Expr::Flags(kind, b(a), b(c)), Expr::Flags(FlagKind::Res, b(r.clone()), b(r.clone())));
        self.st.add_flags = kind != FlagKind::Sub;
        r
    }

    /// Set the flags of a logic result (tst, ands, bics): carry and overflow are clear.
    fn logic_flags(&mut self, r: Expr) {
        self.flag_pair(
            Expr::Flags(FlagKind::Logic, b(r.clone()), b(r.clone())),
            Expr::Flags(FlagKind::ResLogic, b(r.clone()), b(r)),
        );
        self.st.add_flags = true;
    }

    /// After the destination is written the result flags read it: the operands may be gone.
    fn res_from_dest(&mut self, d: Opd, logic: bool) {
        let Opd::Gpr { n, x, sp } = d else { return };
        if n == 31 && !sp {
            return;
        }
        let rd = self.rd(n, x, sp);
        let kind = if logic { FlagKind::ResLogic } else { FlagKind::Res };
        self.assign(Var::FlagRes, Expr::Flags(kind, b(rd.clone()), b(rd.clone())));
        if logic {
            self.assign(Var::FlagOps, Expr::Flags(FlagKind::Logic, b(rd.clone()), b(rd)));
        }
    }

    /// An arm condition as an x86 one over the flag variables. After subtraction the carry is NOT-borrow, the
    /// x86 `ae`; after an add-like or logic instruction it is the x86 carry, `b`.
    fn cond(&self, cc: u8) -> Expr {
        let add = self.st.add_flags;
        let f = flag_cond;
        match cc & 15 {
            0 => f(Cc::E),
            1 => f(Cc::NE),
            2 => f(if add { Cc::B } else { Cc::AE }),
            3 => f(if add { Cc::AE } else { Cc::B }),
            4 => f(Cc::S),
            5 => f(Cc::NS),
            6 => f(Cc::O),
            7 => f(Cc::NO),
            8 => {
                if add {
                    Expr::bin(Bin::LAnd, f(Cc::B), f(Cc::NE))
                } else {
                    f(Cc::A)
                }
            }
            9 => {
                if add {
                    Expr::bin(Bin::LOr, f(Cc::AE), f(Cc::E))
                } else {
                    f(Cc::BE)
                }
            }
            10 => f(Cc::GE),
            11 => f(Cc::L),
            12 => f(Cc::G),
            13 => f(Cc::LE),
            _ => Expr::Const(1, 1),
        }
    }

    fn branch_cond(&self, ins: &Ins) -> Option<Expr> {
        let o = ins.ops;
        let e = match ins.mn.as_str() {
            "b" if ins.cc != 0xff => self.cond(ins.cc),
            "cbz" | "cbnz" => {
                let Opd::Gpr { n, x, sp } = o[0] else { return None };
                let c = if ins.mn.as_str() == "cbz" { Cmp::Eq } else { Cmp::Ne };
                Expr::Cmp(c, b(self.rd(n, x, sp)), b(konst(0, width(o[0]))))
            }
            "tbz" | "tbnz" => {
                let Opd::Gpr { n, x, sp } = o[0] else { return None };
                let bits = width(o[0]);
                let bit = dec(o[1])?;
                if bit < 0 || bit >= bits as i64 {
                    return None;
                }
                let c = if ins.mn.as_str() == "tbz" { Cmp::Eq } else { Cmp::Ne };
                let m = Expr::bin(Bin::And, self.rd(n, x, sp), konst(1u64 << bit, bits));
                Expr::Cmp(c, b(m), b(konst(0, bits)))
            }
            _ => return None,
        };
        Some(simplify(e))
    }

    // ---- memory ------------------------------------------------------------------------------------------

    fn base_plus(&self, base: u8, off: i64) -> Expr {
        simplify(Expr::add(self.rd(base, true, true), konst(off as u64, 64)))
    }

    /// The address of a memory operand and the writeback (base, offset) to apply after the access.
    fn addr_of(&mut self, m: Mem) -> (Expr, Option<(u8, i64)>) {
        match m {
            Mem::Off { base, off } => (self.base_plus(base, off), None),
            Mem::Pre { base, off } => {
                let nb = self.base_plus(base, off);
                self.wr(base, true, base == 31, nb);
                (self.rd(base, true, true), None)
            }
            Mem::Post { base, off } => (self.rd(base, true, true), Some((base, off))),
            Mem::Reg { base, idx, ext, shift } => {
                let ix = self.rd(idx, ext == 3 || ext == 7, false);
                let ix = match ext {
                    2 => ix.zext(64),
                    6 => ix.sext(64),
                    _ => ix,
                };
                let ix = if shift > 0 { Expr::bin(Bin::Mul, ix, konst(1u64 << shift, 64)) } else { ix };
                (simplify(Expr::add(self.rd(base, true, true), ix)), None)
            }
            Mem::PostReg { base, .. } => (self.rd(base, true, true), None),
        }
    }

    fn writeback(&mut self, wb: Option<(u8, i64)>) {
        if let Some((base, off)) = wb {
            let nb = self.base_plus(base, off);
            self.wr(base, true, base == 31, nb);
        }
    }

    /// Loads and stores of general registers. False when the instruction is not one of them.
    fn mem_ins(&mut self, ins: &Ins, mn: &str) -> bool {
        let o = ins.ops;
        let Opd::Gpr { n: t1, x: x1, .. } = o[0] else { return false };
        let (fixed, signed, store, pair): (Option<Bits>, bool, bool, bool) = match mn {
            "ldr" | "ldur" => (None, false, false, false),
            "str" | "stur" => (None, false, true, false),
            "ldrb" | "ldurb" => (Some(8), false, false, false),
            "strb" | "sturb" => (Some(8), false, true, false),
            "ldrh" | "ldurh" => (Some(16), false, false, false),
            "strh" | "sturh" => (Some(16), false, true, false),
            "ldrsb" | "ldursb" => (Some(8), true, false, false),
            "ldrsh" | "ldursh" => (Some(16), true, false, false),
            "ldrsw" | "ldursw" => (Some(32), true, false, false),
            "ldp" | "ldnp" => (None, false, false, true),
            "stp" | "stnp" => (None, false, true, true),
            "ldpsw" => (Some(32), true, false, true),
            _ => return false,
        };
        let (t2, mi) = if pair {
            match o[1] {
                Opd::Gpr { n, .. } => (n, 2),
                _ => return false,
            }
        } else {
            (0, 1)
        };
        let acc: Bits = fixed.unwrap_or(if x1 { 64 } else { 32 });
        let dest_bits: Bits = if x1 { 64 } else { 32 };
        let base_reg = match o[mi] {
            Opd::Mem(Mem::Off { base, .. } | Mem::Pre { base, .. } | Mem::Post { base, .. }) => Some(base),
            _ => None,
        };
        let (addr, wb) = match o[mi] {
            Opd::Mem(Mem::PostReg { .. }) => return false,
            Opd::Mem(m) => self.addr_of(m),
            Opd::Target(t) if !store && !pair => (konst(t, 64), None),
            _ => return false,
        };
        let count = if pair { 2 } else { 1 };
        let addrs = [addr.clone(), simplify(Expr::add(addr, konst((acc / 8) as u64, 64)))];
        let regs = [t1, t2];
        if store {
            for k in 0..count {
                let v = simplify(self.rd(regs[k], x1, false).trunc(acc));
                if self.in_prologue && x1 && (19..=30).contains(&regs[k]) {
                    if let Expr::StackAddr(off) = addrs[k] {
                        self.saves.insert(off);
                    }
                }
                self.emit(Stmt::Store(addrs[k].clone(), acc, v));
            }
        } else {
            let mut vals = Vec::new();
            for a in addrs.iter().take(count) {
                let ld = Expr::Load(b(a.clone()), acc);
                vals.push(if signed { ld.sext(dest_bits) } else { ld.zext(dest_bits) });
            }
            // `ldp x0, x1, [x0]`: the second register is written first, the base is still needed for its address
            let order: Vec<usize> = if pair && Some(t1) == base_reg { vec![1, 0] } else { (0..count).collect() };
            for k in order {
                self.wr(regs[k], x1, false, vals[k].clone());
            }
            if !pair && acc == 64 && x1 && t1 < 31 {
                if let Some(c) = addrs[0].as_const() {
                    if let Some(i) = self.env.import_slot(c) {
                        self.imp[t1 as usize] = Some(i);
                    }
                }
            }
        }
        self.writeback(wb);
        true
    }

    // ---- calls -------------------------------------------------------------------------------------------

    fn call_name(&self, t: &Target) -> Option<String> {
        match t {
            Target::Direct(a) => self.env.name_of(*a),
            Target::Import(i) => Some(self.env.import_name(*i)),
            Target::Indirect(_) => None,
        }
    }

    fn reg_target(&self, n: u8) -> Target {
        if let Some(i) = self.imp[n as usize] {
            Target::Import(i)
        } else if let Some(c) = self.known[n as usize] {
            Target::Direct(c)
        } else {
            Target::Indirect(self.rd(n, true, false))
        }
    }

    fn call_expr(&self, target: Target) -> Expr {
        Expr::Call(Box::new(Call { target, args: Vec::new(), addr: self.pc, known: false }))
    }

    /// A call: the result lands in x0, everything the callee may destroy is clobbered. True for a call that
    /// does not return.
    fn do_call(&mut self, target: Target) -> bool {
        let noret = self.call_name(&target).is_some_and(|n| is_noreturn(&n));
        let call = self.call_expr(target);
        self.assign(Var::Reg(0), call);
        let abi = Abi::Aapcs64;
        let mut clob: Vec<Var> = abi.caller_saved().iter().map(|&r| Var::Reg(r)).collect();
        for x in 0..abi.xmm_clobbered() {
            clob.push(Var::Xmm(x));
        }
        clob.push(Var::FlagOps);
        clob.push(Var::FlagRes);
        self.emit(Stmt::Clobber(clob));
        for r in 0..31usize {
            if !abi.callee_saved(r as u8) {
                self.known[r] = None;
                self.imp[r] = None;
            }
        }
        noret
    }

    // ---- instructions ------------------------------------------------------------------------------------

    /// Lift one non-terminating instruction (calls included). True when execution does not continue.
    fn lift_ins(&mut self, ins: &Ins) -> bool {
        self.pc = ins.pc;
        let mn = ins.mn.as_str();
        if !matches!(mn, "stp" | "str" | "stur" | "sub" | "add" | "mov" | "movz" | "movk" | "nop" | "hint" | "bti")
            && !mn.starts_with("pac")
        {
            self.in_prologue = false;
        }
        match ins.flow {
            AFlow::Call => return self.do_call(Target::Direct(ins.target.unwrap_or(0))),
            AFlow::CallReg => {
                let Opd::Gpr { n, .. } = ins.ops[0] else {
                    self.raw_ins(ins);
                    return false;
                };
                let t = self.reg_target(n);
                return self.do_call(t);
            }
            _ => {}
        }
        if ins.undecoded.is_some() {
            self.raw_ins(ins);
            return false;
        }
        if NOPS.contains(&mn) {
            return false;
        }
        if !self.try_lift(ins, mn) {
            self.raw_ins(ins);
        }
        false
    }

    /// The instructions with a lifting. False (and nothing emitted) for any other shape.
    fn try_lift(&mut self, ins: &Ins, mn: &str) -> bool {
        let o = ins.ops;
        let Opd::Gpr { n: dn, x: dx, sp: dsp } = o[0] else { return false };
        if o[1..]
            .iter()
            .any(|p| matches!(p, Opd::Fp { .. } | Opd::List { .. } | Opd::FImm(_) | Opd::Name(_) | Opd::Sys(_)))
        {
            return false;
        }
        let bits: Bits = if dx { 64 } else { 32 };
        match mn {
            "mov" => {
                if !is_src(o[1]) {
                    return false;
                }
                let v = self.val(o[1], bits);
                self.wr(dn, dx, dsp, v);
            }
            "movz" | "movn" | "movk" => {
                let imm = dec(o[1]).unwrap_or(0) as u64 & 0xffff;
                let amt = if let Opd::Shift { amt, .. } = o[2] { amt as u32 } else { 0 };
                let v = imm << amt;
                let r = match mn {
                    "movz" => konst(v, bits),
                    "movn" => konst(!v & mask(bits), bits),
                    _ => {
                        let old = self.rd(dn, dx, false);
                        let keep = konst(!(0xffffu64 << amt) & mask(bits), bits);
                        Expr::bin(Bin::Or, Expr::bin(Bin::And, old, keep), konst(v, bits))
                    }
                };
                self.wr(dn, dx, dsp, r);
            }
            "add" | "sub" | "adds" | "subs" => {
                if !is_gpr(o[1]) || !is_src(o[2]) {
                    return false;
                }
                let a = self.val(o[1], bits);
                let c = self.op2(ins, 2, bits);
                let sub = mn.starts_with("sub");
                let (op, kind) = if sub { (Bin::Sub, FlagKind::Sub) } else { (Bin::Add, FlagKind::Add) };
                if mn.ends_with('s') {
                    let r = self.arith_flags(op, kind, a, c);
                    self.wr(dn, dx, dsp, r);
                    self.res_from_dest(o[0], false);
                } else {
                    let r = simplify(Expr::bin(op, a, c));
                    self.wr(dn, dx, dsp, r);
                }
            }
            "cmp" | "cmn" => {
                if !is_src(o[1]) {
                    return false;
                }
                let a = self.rd(dn, dx, dsp);
                let c = self.op2(ins, 1, bits);
                if mn == "cmp" {
                    self.arith_flags(Bin::Sub, FlagKind::Sub, a, c);
                } else {
                    self.arith_flags(Bin::Add, FlagKind::Add, a, c);
                }
            }
            "tst" => {
                if !is_src(o[1]) {
                    return false;
                }
                let a = self.rd(dn, dx, dsp);
                let c = self.op2(ins, 1, bits);
                let r = simplify(Expr::bin(Bin::And, a, c));
                self.logic_flags(r);
            }
            "neg" | "negs" | "mvn" => {
                if !is_gpr(o[1]) {
                    return false;
                }
                let c = self.op2(ins, 1, bits);
                match mn {
                    "mvn" => {
                        let r = simplify(Expr::Un(Un::Not, b(c)));
                        self.wr(dn, dx, dsp, r);
                    }
                    "neg" => {
                        let r = simplify(Expr::bin(Bin::Sub, konst(0, bits), c));
                        self.wr(dn, dx, dsp, r);
                    }
                    _ => {
                        let r = self.arith_flags(Bin::Sub, FlagKind::Sub, konst(0, bits), c);
                        self.wr(dn, dx, dsp, r);
                        self.res_from_dest(o[0], false);
                    }
                }
            }
            "and" | "orr" | "eor" | "ands" | "bic" | "bics" | "orn" | "eon" => {
                if !is_gpr(o[1]) || !is_src(o[2]) {
                    return false;
                }
                let a = self.val(o[1], bits);
                let mut c = self.op2(ins, 2, bits);
                if matches!(mn, "bic" | "bics" | "orn" | "eon") {
                    c = simplify(Expr::Un(Un::Not, b(c)));
                }
                let op = match mn {
                    "and" | "ands" | "bic" | "bics" => Bin::And,
                    "orr" | "orn" => Bin::Or,
                    _ => Bin::Xor,
                };
                let r = simplify(Expr::bin(op, a, c));
                if matches!(mn, "ands" | "bics") {
                    self.logic_flags(r.clone());
                    self.wr(dn, dx, dsp, r);
                    self.res_from_dest(o[0], true);
                } else {
                    self.wr(dn, dx, dsp, r);
                }
            }
            "mul" | "mneg" | "madd" | "msub" => {
                if !is_gpr(o[1]) || !is_gpr(o[2]) {
                    return false;
                }
                let p = Expr::bin(Bin::Mul, self.val(o[1], bits), self.val(o[2], bits));
                let r = match mn {
                    "mul" => p,
                    "mneg" => Expr::Un(Un::Neg, b(p)),
                    _ => {
                        if !is_gpr(o[3]) {
                            return false;
                        }
                        let ra = self.val(o[3], bits);
                        Expr::bin(if mn == "madd" { Bin::Add } else { Bin::Sub }, ra, p)
                    }
                };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "smull" | "umull" | "smaddl" | "umaddl" | "smsubl" | "umsubl" => {
                if !dx || !is_gpr(o[1]) || !is_gpr(o[2]) {
                    return false;
                }
                let sg = mn.starts_with('s');
                let ext = |e: Expr| if sg { e.sext(64) } else { e.zext(64) };
                let p = Expr::bin(Bin::Mul, ext(self.val(o[1], 32)), ext(self.val(o[2], 32)));
                let r = if matches!(mn, "smull" | "umull") {
                    p
                } else {
                    if !is_gpr(o[3]) {
                        return false;
                    }
                    let ra = self.val(o[3], 64);
                    Expr::bin(if mn.ends_with("addl") { Bin::Add } else { Bin::Sub }, ra, p)
                };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "smulh" | "umulh" => {
                if !dx || !is_gpr(o[1]) || !is_gpr(o[2]) {
                    return false;
                }
                let r = Expr::Intr(
                    if mn == "smulh" { "smulh" } else { "umulh" },
                    vec![self.val(o[1], 64), self.val(o[2], 64)],
                    64,
                );
                self.wr(dn, dx, dsp, r);
            }
            "udiv" | "sdiv" => {
                if !is_gpr(o[1]) || !is_gpr(o[2]) {
                    return false;
                }
                let r = Expr::bin(
                    if mn == "udiv" { Bin::UDiv } else { Bin::SDiv },
                    self.val(o[1], bits),
                    self.val(o[2], bits),
                );
                self.wr(dn, dx, dsp, simplify(r));
            }
            "lsl" | "lsr" | "asr" | "ror" => {
                if !is_gpr(o[1]) || !is_src(o[2]) {
                    return false;
                }
                let a = self.val(o[1], bits);
                let k = self.val(o[2], bits);
                let r = match mn {
                    "lsl" => Expr::bin(Bin::Shl, a, k),
                    "lsr" => Expr::bin(Bin::Shr, a, k),
                    "asr" => Expr::bin(Bin::Sar, a, k),
                    _ => Expr::Intr("ror", vec![a, k], bits),
                };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "ubfx" | "sbfx" | "ubfiz" | "sbfiz" => {
                let (Some(lsb), Some(w)) = (dec(o[2]), dec(o[3])) else { return false };
                let bw = bits as i64;
                if !is_gpr(o[1]) || lsb < 0 || w < 1 || lsb + w > bw {
                    return false;
                }
                let a = self.val(o[1], bits);
                let k = |v: i64| konst(v as u64, bits);
                let r = match mn {
                    "ubfx" => Expr::bin(Bin::And, Expr::bin(Bin::Shr, a, k(lsb)), konst(mask(w as Bits), bits)),
                    "sbfx" => Expr::bin(Bin::Sar, Expr::bin(Bin::Shl, a, k(bw - lsb - w)), k(bw - w)),
                    "ubfiz" => Expr::bin(Bin::Shl, Expr::bin(Bin::And, a, konst(mask(w as Bits), bits)), k(lsb)),
                    _ => Expr::bin(Bin::Shl, Expr::bin(Bin::Sar, Expr::bin(Bin::Shl, a, k(bw - w)), k(bw - w)), k(lsb)),
                };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "sxtb" | "sxth" | "sxtw" | "uxtb" | "uxth" => {
                if !is_gpr(o[1]) {
                    return false;
                }
                let sb: Bits = match mn {
                    "sxtb" | "uxtb" => 8,
                    "sxth" | "uxth" => 16,
                    _ => 32,
                };
                let a = self.val(o[1], bits).trunc(sb);
                let r = if mn.starts_with('s') { a.sext(bits) } else { a.zext(bits) };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "csel" | "csinc" | "csinv" | "csneg" => {
                let Opd::Cond(cc) = o[3] else { return false };
                if !is_gpr(o[1]) || !is_gpr(o[2]) {
                    return false;
                }
                let c = self.cond(cc);
                let a = self.val(o[1], bits);
                let m = self.val(o[2], bits);
                let alt = match mn {
                    "csel" => m,
                    "csinc" => Expr::bin(Bin::Add, m, konst(1, bits)),
                    "csinv" => Expr::Un(Un::Not, b(m)),
                    _ => Expr::Un(Un::Neg, b(m)),
                };
                self.wr(dn, dx, dsp, simplify(Expr::Select(b(c), b(a), b(alt))));
            }
            "cset" | "csetm" => {
                let Opd::Cond(cc) = o[1] else { return false };
                let c = self.cond(cc);
                let r = if mn == "cset" {
                    c.zext(bits)
                } else {
                    Expr::Select(b(c), b(konst(mask(bits), bits)), b(konst(0, bits)))
                };
                self.wr(dn, dx, dsp, simplify(r));
            }
            "cinc" | "cinv" | "cneg" => {
                let Opd::Cond(cc) = o[2] else { return false };
                if !is_gpr(o[1]) {
                    return false;
                }
                let c = self.cond(cc);
                let a = self.val(o[1], bits);
                let alt = match mn {
                    "cinc" => Expr::bin(Bin::Add, a.clone(), konst(1, bits)),
                    "cinv" => Expr::Un(Un::Not, b(a.clone())),
                    _ => Expr::Un(Un::Neg, b(a.clone())),
                };
                self.wr(dn, dx, dsp, simplify(Expr::Select(b(c), b(alt), b(a))));
            }
            "adr" | "adrp" => {
                let Some((r, addr)) = (if mn == "adr" { ins.adr } else { ins.adrp }) else { return false };
                self.wr(r, true, false, konst(addr, 64));
            }
            _ => return self.mem_ins(ins, mn),
        }
        true
    }

    /// One memory operand of an instruction that is not lifted: what it reads, and what it keeps in memory.
    /// True when it wrote back a tracked frame pointer.
    fn raw_mem(&mut self, m: Mem, fp0: Option<i64>, uses: &mut Vec<Var>, defs: &mut Vec<Var>) -> bool {
        let (base, ea, wb, idx) = match m {
            Mem::Off { base, off } => (base, Some(off), None, None),
            Mem::Pre { base, off } => (base, Some(off), Some(off), None),
            Mem::Post { base, off } => (base, Some(0), Some(off), None),
            Mem::Reg { base, idx, .. } | Mem::PostReg { base, idx } => (base, None, None, Some(idx)),
        };
        if let Some(i) = idx {
            if i < 31 {
                uses.push(Var::Reg(i));
            }
        }
        let tracked = match base {
            31 => self.st.sp,
            29 => fp0,
            _ => None,
        };
        match (tracked, ea) {
            (Some(t), Some(off)) => {
                // the access may span several slots: leave a window of the frame in memory
                let a = t.wrapping_add(off);
                for k in 0..64 {
                    self.escapes.insert(a.wrapping_add(k));
                }
            }
            (None, _) if base < 31 => uses.push(Var::Reg(base)),
            _ => {}
        }
        let mut kept_fp = false;
        if let Some(w) = wb {
            match base {
                31 => self.st.sp = self.st.sp.map(|s| s.wrapping_add(w)),
                29 if fp0.is_some() => {
                    self.st.fp = fp0.map(|s| s.wrapping_add(w));
                    kept_fp = true;
                }
                n => {
                    defs.push(Var::Reg(n));
                    self.known[n as usize] = None;
                    self.imp[n as usize] = None;
                }
            }
        }
        kept_fp
    }

    /// An instruction this version does not lift: keep its text and what it reads and writes.
    fn raw_ins(&mut self, ins: &Ins) {
        self.raw += 1;
        let mn = ins.mn.as_str();
        let store = mn.starts_with("st");
        let nodest = store
            || mn.starts_with("fcmp")
            || mn.starts_with("fccmp")
            || mn.starts_with("cmp")
            || mn.starts_with("ccm");
        let pair_load = matches!(mn, "ldp" | "ldnp" | "ldxp" | "ldaxp" | "ldpsw");
        let mut uses: Vec<Var> = Vec::new();
        let mut defs: Vec<Var> = Vec::new();
        let mut mem = ins.undecoded.is_some();
        let fp0 = self.st.fp;
        let mut kept_fp = false;
        for (i, op) in ins.ops.iter().enumerate() {
            match *op {
                Opd::Gpr { n, .. } if n < 31 => {
                    let written = (ins.writes >> n) & 1 != 0;
                    if !(written && (i == 0 || (i == 1 && pair_load))) {
                        uses.push(Var::Reg(n));
                    }
                }
                Opd::Fp { n, .. } => {
                    if i == 0 && !nodest {
                        defs.push(Var::Xmm(n));
                    } else {
                        uses.push(Var::Xmm(n));
                    }
                }
                Opd::List { n, count, .. } => {
                    for j in 0..count {
                        let v = Var::Xmm(n.wrapping_add(j) & 31);
                        if i == 0 && !nodest {
                            defs.push(v);
                        } else {
                            uses.push(v);
                        }
                    }
                }
                Opd::Mem(m) => {
                    mem = true;
                    kept_fp |= self.raw_mem(m, fp0, &mut uses, &mut defs);
                }
                _ => {}
            }
        }
        for n in 0..31u8 {
            if (ins.writes >> n) & 1 != 0 {
                defs.push(Var::Reg(n));
                self.known[n as usize] = None;
                self.imp[n as usize] = None;
                if n == 29 && !kept_fp {
                    self.st.fp = None;
                }
            }
        }
        let flags = matches!(
            mn,
            "adc"
                | "adcs"
                | "sbc"
                | "sbcs"
                | "ngc"
                | "ngcs"
                | "ccmp"
                | "ccmn"
                | "fcmp"
                | "fcmpe"
                | "fccmp"
                | "fccmpe"
                | "fcsel"
                | "msr"
                | "mrs"
                | "setf8"
                | "setf16"
                | "rmif"
                | "cfinv"
                | "axflag"
                | "xaflag"
        );
        if flags {
            uses.push(Var::FlagOps);
            uses.push(Var::FlagRes);
            defs.push(Var::FlagOps);
            defs.push(Var::FlagRes);
            self.st.add_flags = false;
        }
        let text = ins.text();
        self.emit(Stmt::Raw { text, uses, defs, mem });
    }
}

fn noreturn_bl(env: &dyn Env, i: &Ins) -> bool {
    i.target.and_then(|t| env.name_of(t)).is_some_and(|n| is_noreturn(&n))
}

/// A `br` that picks its target out of a table: a load with a register index shortly before it.
fn through_table(ins: &[Ins], k: usize, first: usize) -> bool {
    (first..k).rev().take(8).any(|j| {
        ins[j].mn.as_str().starts_with("ldr") && ins[j].ops.iter().any(|o| matches!(o, Opd::Mem(Mem::Reg { .. })))
    })
}

pub(super) fn lift(env: &dyn Env, start: u64, end: u64) -> Lifted {
    let len = (end.saturating_sub(start) as usize) & !3;
    let mut ins: Vec<Ins> = Vec::new();
    if let Some(bytes) = env.read(start, len) {
        for (k, w) in bytes.as_chunks::<4>().0.iter().enumerate() {
            ins.push(decode_word(u32::from_le_bytes(*w), start.wrapping_add(4 * k as u64)));
        }
    }
    let at = |a: u64| -> Option<usize> {
        if a < start || a >= end || !(a - start).is_multiple_of(4) {
            return None;
        }
        let k = ((a - start) / 4) as usize;
        (k < ins.len()).then_some(k)
    };
    let in_fn = |a: u64| a >= start && a < end;
    // 1. blocks: leaders by recursive descent
    let mut leaders: BTreeSet<u64> = BTreeSet::new();
    leaders.insert(start);
    let mut externs: BTreeSet<u64> = BTreeSet::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut work = vec![start];
    let read = |a: u64, n: usize| env.read(a, n).map(<[u8]>::to_vec);
    // the `br` instructions that dispatch through a table of this function, by instruction number
    let mut switches: HashMap<usize, Table> = HashMap::new();
    while let Some(a) = work.pop() {
        let Some(mut k) = at(a) else { continue };
        loop {
            if !seen.insert(k) {
                break;
            }
            let Some(i) = ins.get(k) else { break };
            if matches!(i.undecoded, Some(Why::Reserved)) {
                break;
            }
            match i.flow {
                AFlow::Seq | AFlow::CallReg => k += 1,
                AFlow::Call => {
                    if noreturn_bl(env, i) {
                        break;
                    }
                    k += 1;
                }
                AFlow::Jump => {
                    if let Some(t) = i.target {
                        if in_fn(t) && leaders.insert(t) {
                            work.push(t);
                        }
                    }
                    break;
                }
                AFlow::Cond => {
                    if let Some(t) = i.target {
                        if in_fn(t) {
                            if leaders.insert(t) {
                                work.push(t);
                            }
                        } else {
                            externs.insert(t);
                        }
                    }
                    let nxt = i.pc.wrapping_add(4);
                    if leaders.insert(nxt) {
                        work.push(nxt);
                    }
                    break;
                }
                AFlow::JumpReg => {
                    if let Some(sw) = a64_switch::find(&ins, k, start, end, &read) {
                        for &t in &sw.targets {
                            if leaders.insert(t) {
                                work.push(t);
                            }
                        }
                        switches.insert(k, sw);
                    }
                    break;
                }
                AFlow::Ret | AFlow::Trap => break,
            }
        }
    }
    // what a switch only computes the jump with: not lifted
    let hidden: HashSet<usize> = switches.values().flat_map(|s| s.hidden.iter().copied()).collect();
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
    let mut lf = A64 {
        env,
        pc: start,
        stmts: Vec::new(),
        st: State { sp: Some(0), fp: None, add_flags: false },
        saves: BTreeSet::new(),
        escapes: BTreeSet::new(),
        raw: 0,
        in_prologue: true,
        known: [None; 32],
        imp: [None; 32],
    };
    let mut states: HashMap<usize, State> = HashMap::new();
    states.insert(0, State { sp: Some(0), fp: None, add_flags: false });
    let mut queue = vec![0usize];
    let leader_list: Vec<u64> = leaders.iter().copied().collect();
    let mut unresolved = 0;
    let mut tables = 0;
    while let Some(id) = queue.pop() {
        if blocks[id].is_some() {
            continue;
        }
        let Some(&st0) = states.get(&id) else { continue };
        let baddr = leader_list[id];
        let Some(first) = at(baddr) else {
            blocks[id] = Some(Block { addr: baddr, stmts: Vec::new(), term: Term::Stop, term_addr: baddr });
            continue;
        };
        lf.begin(st0, id == 0);
        let mut k = first;
        let term: (Term, u64);
        let id_of = |a: u64| ids.get(&a).copied().or_else(|| ext_ids.get(&a).copied());
        let mut succ_states: Vec<(usize, State)> = Vec::new();
        loop {
            let Some(i) = ins.get(k) else {
                term = (Term::Stop, baddr);
                break;
            };
            let pc = i.pc;
            lf.pc = pc;
            if k != first && leaders.contains(&pc) {
                let t = ids[&pc];
                succ_states.push((t, lf.st));
                term = (Term::Jump(t), pc);
                break;
            }
            if matches!(i.undecoded, Some(Why::Reserved)) {
                term = (Term::Stop, pc);
                break;
            }
            if hidden.contains(&k) {
                k += 1;
                continue;
            }
            match i.flow {
                AFlow::Cond => {
                    let Some(c) = lf.branch_cond(i) else {
                        term = (Term::Stop, pc);
                        break;
                    };
                    let target = i
                        .target
                        .or_else(|| i.ops.iter().find_map(|o| if let Opd::Target(t) = o { Some(*t) } else { None }));
                    let tid = target.and_then(id_of).unwrap_or(0);
                    let fid = ids.get(&pc.wrapping_add(4)).copied().unwrap_or(0);
                    succ_states.push((tid, lf.st));
                    succ_states.push((fid, lf.st));
                    term = (Term::Branch(c, tid, fid), pc);
                    break;
                }
                AFlow::Jump => {
                    match i.target {
                        Some(t) if in_fn(t) => {
                            let tid = ids[&t];
                            succ_states.push((tid, lf.st));
                            term = (Term::Jump(tid), pc);
                        }
                        Some(t) => {
                            term = (Term::TailCall(lf.call_expr(Target::Direct(t))), pc);
                        }
                        None => term = (Term::Stop, pc),
                    }
                    break;
                }
                AFlow::JumpReg => {
                    let Opd::Gpr { n, .. } = i.ops[0] else {
                        term = (Term::Stop, pc);
                        break;
                    };
                    if let Some(sw) = switches.get(&k) {
                        let idx = lf.rd(sw.idx, sw.wide, false);
                        lf.assign(Var::Tmp(1), idx.zext(64));
                        let scr = Expr::Var(Var::Tmp(1), if sw.wide { 64 } else { 32 });
                        let mut cases: Vec<(u64, BlockId)> = Vec::new();
                        for (v, t) in sw.targets.iter().enumerate() {
                            cases.push((v as u64, ids[t]));
                            succ_states.push((ids[t], lf.st));
                        }
                        tables += 1;
                        term = (Term::Switch(scr, cases, None), pc);
                        break;
                    }
                    let target = lf.reg_target(n);
                    if matches!(target, Target::Import(_)) {
                        term = (Term::TailCall(lf.call_expr(target)), pc);
                        break;
                    }
                    // Only a frame known to be gone makes the jump a tail call, and only when the target is not
                    // picked from a table of this function.
                    let frame_gone = lf.st.sp.is_some_and(|s| s >= 0);
                    if frame_gone && !through_table(&ins, k, first) {
                        term = (Term::TailCall(lf.call_expr(target)), pc);
                        break;
                    }
                    unresolved += 1;
                    term = (Term::Indirect(lf.rd(n, true, false)), pc);
                    break;
                }
                AFlow::Ret => {
                    term = (Term::Return(Some(Expr::Var(Var::Reg(0), 64))), pc);
                    break;
                }
                AFlow::Trap => {
                    term = (Term::Stop, pc);
                    break;
                }
                AFlow::Seq | AFlow::Call | AFlow::CallReg => {
                    let stop = lf.lift_ins(i);
                    k += 1;
                    if stop {
                        term = (Term::Stop, pc);
                        break;
                    }
                }
            }
        }
        for (s, st) in succ_states {
            states.entry(s).or_insert(st);
            queue.push(s);
        }
        blocks[id] = Some(Block { addr: baddr, stmts: std::mem::take(&mut lf.stmts), term: term.0, term_addr: term.1 });
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
        abi: Abi::Aapcs64,
        blocks,
        saves: lf.saves.clone(),
        tables,
        unresolved_jumps: unresolved,
        raw: lf.raw,
        instructions: ins.len(),
    };
    promote_slots(&mut out, &lf.escapes);
    out
}
