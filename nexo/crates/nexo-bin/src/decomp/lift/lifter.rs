//! The per-function lifter: one x86-64 instruction at a time to IR statements.

use super::*;

impl<'a> Lifter<'a> {
    fn emit(&mut self, addr: u64, st: Stmt) {
        self.stmts.push(S { addr, st });
    }

    pub(super) fn assign(&mut self, addr: u64, v: Var, e: Expr) {
        self.emit(addr, Stmt::Assign(v, e));
    }

    fn reg_read(&self, r: Register) -> Expr {
        match reg_num(r) {
            Some((n, bits, false)) => {
                if n == 4 && bits == 64 {
                    if let Some(o) = self.st.rsp {
                        return Expr::StackAddr(o);
                    }
                }
                if n == 5 && bits == 64 {
                    if let Some(o) = self.st.rbp {
                        return Expr::StackAddr(o);
                    }
                }
                Expr::Var(Var::Reg(n), bits)
            }
            Some((n, _, true)) => Expr::Intr("high8", vec![Expr::Var(Var::Reg(n), 64)], 8),
            None if r.is_xmm() => Expr::Var(Var::Xmm(r.number() as u8), 64),
            None => Expr::Unknown(64),
        }
    }

    fn mem_addr(&mut self, i: &Instruction) -> Expr {
        let mut e: Option<Expr> = None;
        if i.is_ip_rel_memory_operand() {
            e = Some(Expr::Const(i.ip_rel_memory_address(), 64));
        } else if i.memory_base() != Register::None {
            let base = i.memory_base();
            let bv = if base.is_gpr64() {
                self.reg_read(base)
            } else {
                self.reg_read(base.full_register()).trunc(32).zext(64)
            };
            e = Some(bv);
        }
        if i.memory_index() != Register::None {
            let ix = i.memory_index();
            let mut v =
                if ix.is_gpr64() { self.reg_read(ix) } else { self.reg_read(ix.full_register()).trunc(32).zext(64) };
            let sc = i.memory_index_scale() as u64;
            if sc != 1 {
                v = Expr::bin(Bin::Mul, v, Expr::Const(sc, 64));
            }
            e = Some(match e {
                Some(x) => Expr::add(x, v),
                None => v,
            });
        }
        if !i.is_ip_rel_memory_operand() {
            let d = i.memory_displacement64();
            let disp = if d == 0 { None } else { Some(Expr::Const(d, 64)) };
            if let Some(d) = disp {
                e = Some(match e {
                    Some(x) => Expr::add(x, d),
                    None => d,
                });
            }
        }
        let mut e = e.unwrap_or(Expr::Const(0, 64));
        match i.segment_prefix() {
            Register::FS => e = Expr::add(Expr::SegBase(0), e),
            Register::GS => e = Expr::add(Expr::SegBase(1), e),
            _ => {}
        }
        simplify(e)
    }

    fn op_bits(&self, i: &Instruction, n: u32) -> Bits {
        match i.op_kind(n) {
            OpKind::Register => reg_num(i.op_register(n)).map(|x| x.1).unwrap_or(64),
            OpKind::Memory => mem_size_bits(i),
            _ => 0,
        }
    }

    fn imm(&self, i: &Instruction, n: u32, bits: Bits) -> Expr {
        konst(i.immediate(n), if bits == 0 { 64 } else { bits })
    }

    /// Read operand `n`; an immediate takes `hint` bits.
    pub(super) fn read(&mut self, i: &Instruction, n: u32, hint: Bits) -> Expr {
        match i.op_kind(n) {
            OpKind::Register => self.reg_read(i.op_register(n)),
            OpKind::Memory => {
                let a = self.mem_addr(i);
                self.escape_check(&a);
                a.load(mem_size_bits(i))
            }
            OpKind::NearBranch64 => Expr::Const(i.near_branch_target(), 64),
            _ => self.imm(i, n, hint),
        }
    }

    fn escape_check(&mut self, _a: &Expr) {}

    fn write_reg(&mut self, addr: u64, r: Register, v: Expr) {
        match reg_num(r) {
            Some((n, 64, _)) => {
                if n == 4 || (n == 5 && self.st.rbp.is_some()) {
                    // stack pointer arithmetic is tracked, not emitted
                    if n == 5 {
                        self.st.rbp = None;
                        self.assign(addr, Var::Reg(5), v.zext(64));
                    }
                    return;
                }
                if n == 5 {
                    self.st.rbp = None;
                }
                self.assign(addr, Var::Reg(n), v.zext(64));
            }
            Some((n, 32, _)) => {
                if n == 5 {
                    self.st.rbp = None;
                }
                self.assign(addr, Var::Reg(n), v.trunc(32).zext(64));
            }
            Some((n, w, false)) => {
                if n == 5 {
                    self.st.rbp = None;
                }
                let old = Expr::Var(Var::Reg(n), 64);
                self.assign(addr, Var::Reg(n), Expr::Insert(b(old), b(v.trunc(w))));
            }
            Some((n, _, true)) => {
                // ah, ch, dh, bh: keep the other bits through an intrinsic
                let old = Expr::Var(Var::Reg(n), 64);
                self.assign(addr, Var::Reg(n), Expr::Intr("set_high8", vec![old, v.trunc(8)], 64));
            }
            None if r.is_xmm() => {
                let n = r.number() as u8;
                self.assign(addr, Var::Xmm(n), v);
            }
            None => {}
        }
    }

    fn write(&mut self, addr: u64, i: &Instruction, n: u32, v: Expr) {
        match i.op_kind(n) {
            OpKind::Register => self.write_reg(addr, i.op_register(n), v),
            OpKind::Memory => {
                let a = self.mem_addr(i);
                let w = mem_size_bits(i);
                self.emit(addr, Stmt::Store(a, w, v.trunc(w)));
            }
            _ => {}
        }
    }

    fn set_flags(&mut self, addr: u64, ops: Expr, res: Expr) {
        self.assign(addr, Var::FlagOps, ops);
        self.assign(addr, Var::FlagRes, res);
    }

    /// After an instruction that writes its destination register, the result flags read the
    /// register: its operands may be overwritten, the register is what the next test sees.
    fn res_from_dest(&mut self, addr: u64, ins: &Instruction, bits: Bits, logic: bool) {
        if ins.op0_kind() != OpKind::Register || ins.op0_register().is_xmm() || bits < 8 {
            return;
        }
        let rd = self.read(ins, 0, bits);
        let kind = if logic { FlagKind::ResLogic } else { FlagKind::Res };
        self.assign(addr, Var::FlagRes, Expr::Flags(kind, b(rd.clone()), b(rd.clone())));
        if logic {
            self.assign(addr, Var::FlagOps, Expr::Flags(FlagKind::Logic, b(rd.clone()), b(rd)));
        }
    }

    fn flags_sub(&mut self, addr: u64, a: Expr, c: Expr) {
        let r = simplify(Expr::bin(Bin::Sub, a.clone(), c.clone()));
        self.set_flags(addr, Expr::Flags(FlagKind::Sub, b(a), b(c)), Expr::Flags(FlagKind::Res, b(r.clone()), b(r)));
    }

    fn flags_res(&mut self, addr: u64, r: Expr, logic: bool) {
        let ops = Expr::Flags(if logic { FlagKind::Logic } else { FlagKind::Res }, b(r.clone()), b(r.clone()));
        let res = Expr::Flags(if logic { FlagKind::ResLogic } else { FlagKind::Res }, b(r.clone()), b(r));
        self.set_flags(addr, ops, res);
    }

    fn raw(&mut self, ins: &Instruction) {
        self.raw += 1;
        let mut text = String::new();
        iced_x86::Formatter::format(&mut self.fmt, ins, &mut text);
        let mut uses = Vec::new();
        let mut defs = Vec::new();
        let mut mem = false;
        let info = self.info.info(ins);
        for u in info.used_registers() {
            let r = u.register();
            let v = if r.is_gpr() {
                let n = gpr_number(r);
                if n == 4 {
                    continue;
                }
                Var::Reg(n)
            } else if r.is_xmm() || r.is_ymm() || r.is_zmm() {
                Var::Xmm(r.number() as u8)
            } else {
                continue;
            };
            let partial = r.is_gpr() && r.size() < 4;
            match u.access() {
                OpAccess::Read | OpAccess::CondRead => uses.push(v),
                OpAccess::Write | OpAccess::CondWrite => {
                    defs.push(v);
                    if partial {
                        uses.push(v);
                    }
                }
                OpAccess::ReadWrite | OpAccess::ReadCondWrite => {
                    uses.push(v);
                    defs.push(v);
                }
                _ => {}
            }
        }
        for m in info.used_memory() {
            if matches!(
                m.access(),
                OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
            ) {
                mem = true;
            }
            // anything that touches the frame leaves its slot in memory
            let base = m.base();
            if (base == Register::RSP && self.st.rsp.is_some()) || (base == Register::RBP && self.st.rbp.is_some()) {
                let off = if base == Register::RSP { self.st.rsp } else { self.st.rbp }.unwrap_or(0);
                self.escapes.insert(off.wrapping_add(m.displacement() as i64));
            }
        }
        if ins.rflags_written() != 0 {
            defs.push(Var::FlagOps);
            defs.push(Var::FlagRes);
        }
        if ins.rflags_read() != 0 {
            uses.push(Var::FlagOps);
            uses.push(Var::FlagRes);
        }
        if ins.has_rep_prefix() || ins.has_repne_prefix() {
            mem = true;
        }
        let addr = ins.ip();
        self.emit(addr, Stmt::Raw { text, uses, defs, mem });
    }

    pub(super) fn call_expr(&mut self, ins: &Instruction) -> Expr {
        let target = match ins.op0_kind() {
            OpKind::NearBranch64 => {
                let t = ins.near_branch_target();
                Target::Direct(t)
            }
            OpKind::Memory
                if ins.is_ip_rel_memory_operand() && self.env.import_slot(ins.ip_rel_memory_address()).is_some() =>
            {
                Target::Import(self.env.import_slot(ins.ip_rel_memory_address()).unwrap_or(0))
            }
            _ => {
                let t = self.read(ins, 0, 64);
                Target::Indirect(t)
            }
        };
        Expr::Call(Box::new(Call { target, args: Vec::new(), addr: ins.ip(), known: false }))
    }

    pub(super) fn call_target_name(&self, e: &Expr) -> Option<String> {
        if let Expr::Call(c) = e {
            return match &c.target {
                Target::Direct(a) => self.env.name_of(*a),
                Target::Import(i) => Some(self.env.import_name(*i)),
                Target::Indirect(_) => None,
            };
        }
        None
    }

    fn do_call(&mut self, ins: &Instruction) -> bool {
        let addr = ins.ip();
        let call = self.call_expr(ins);
        let noret = self.call_target_name(&call).is_some_and(|n| is_noreturn(&n));
        self.assign(addr, RAX, call);
        let mut clob: Vec<Var> = self.abi.caller_saved().iter().map(|&r| Var::Reg(r)).collect();
        for x in 0..self.abi.xmm_clobbered() {
            clob.push(Var::Xmm(x));
        }
        clob.push(Var::FlagOps);
        clob.push(Var::FlagRes);
        self.emit(addr, Stmt::Clobber(clob));
        noret
    }

    /// Lift one non-terminating instruction (calls included). Returns true when execution does not continue.
    pub(super) fn lift(&mut self, ins: &Instruction) -> bool {
        use Mnemonic as M;
        let addr = ins.ip();
        let m = ins.mnemonic();
        if m != M::Push && m != M::Sub && m != M::Nop && m != M::Endbr64 && m != M::Mov {
            self.in_prologue = false;
        }
        match m {
            M::Nop
            | M::Endbr64
            | M::Int3
            | M::Pause
            | M::Lfence
            | M::Mfence
            | M::Sfence
            | M::Prefetcht0
            | M::Prefetchw => {}
            M::Fnop => {}
            M::Mov => {
                // `mov rbp, rsp` starts a frame
                if ins.op0_kind() == OpKind::Register && ins.op0_register() == Register::RBP {
                    if let Some(o) = self.reg_to_off(ins) {
                        self.st.rbp = Some(o);
                        return false;
                    }
                }
                if ins.op0_kind() == OpKind::Register && ins.op0_register() == Register::RSP {
                    // mov rsp, rbp
                    if ins.op1_kind() == OpKind::Register && ins.op1_register() == Register::RBP {
                        if let Some(o) = self.st.rbp {
                            self.st.rsp = Some(o);
                            return false;
                        }
                    }
                    self.st.rsp = None;
                    self.raw(ins);
                    return false;
                }
                let bits = self.op_bits(ins, 0);
                let v = self.read(ins, 1, bits);
                self.write(addr, ins, 0, v);
            }
            M::Movzx => {
                let bits = self.op_bits(ins, 0);
                let v = self.read(ins, 1, 0).zext(bits);
                self.write(addr, ins, 0, v);
            }
            M::Movsx | M::Movsxd => {
                let bits = self.op_bits(ins, 0);
                let v = self.read(ins, 1, 0).sext(bits);
                self.write(addr, ins, 0, v);
            }
            M::Lea => {
                let a = self.mem_addr(ins);
                let bits = self.op_bits(ins, 0);
                let a = if let Expr::StackAddr(_) = a { a } else { a.trunc(bits.max(8)) };
                self.write(addr, ins, 0, a);
            }
            M::Add | M::Sub | M::And | M::Or | M::Xor => {
                // frame bookkeeping on the stack pointer
                if ins.op0_kind() == OpKind::Register && ins.op0_register() == Register::RSP {
                    if let (Some(o), true) =
                        (self.st.rsp, ins.op1_kind() != OpKind::Register && ins.op1_kind() != OpKind::Memory)
                    {
                        let v = ins.immediate(1) as i64;
                        match m {
                            M::Add => self.st.rsp = Some(o.wrapping_add(v)),
                            M::Sub => self.st.rsp = Some(o.wrapping_sub(v)),
                            _ => self.st.rsp = None,
                        }
                        return false;
                    }
                    self.st.rsp = None;
                    self.raw(ins);
                    return false;
                }
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                let (op, kind) = match m {
                    M::Add => (Bin::Add, FlagKind::Add),
                    M::Sub => (Bin::Sub, FlagKind::Sub),
                    M::And => (Bin::And, FlagKind::Logic),
                    M::Or => (Bin::Or, FlagKind::Logic),
                    _ => (Bin::Xor, FlagKind::Logic),
                };
                let c = if c.bits() != bits && c.bits() != 0 { c.sext(bits).trunc(bits) } else { c };
                let r = simplify(Expr::bin(op, a.clone(), c.clone()));
                match kind {
                    FlagKind::Logic => self.flags_res(addr, r.clone(), true),
                    FlagKind::Sub => self.flags_sub(addr, a, c),
                    _ => {
                        let ops = Expr::Flags(kind, b(a), b(c));
                        self.set_flags(addr, ops, Expr::Flags(FlagKind::Res, b(r.clone()), b(r.clone())));
                    }
                }
                self.write(addr, ins, 0, r);
                self.res_from_dest(addr, ins, bits, kind == FlagKind::Logic);
            }
            M::Inc | M::Dec => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let (r, c) = if m == M::Inc {
                    (Expr::add(a.clone(), Expr::Const(1, bits)), Expr::Const(mask(bits), bits))
                } else {
                    (Expr::bin(Bin::Sub, a.clone(), Expr::Const(1, bits)), Expr::Const(1, bits))
                };
                let r = simplify(r);
                self.set_flags(
                    addr,
                    Expr::Flags(FlagKind::SubNoCf, b(a), b(c)),
                    Expr::Flags(FlagKind::Res, b(r.clone()), b(r.clone())),
                );
                self.write(addr, ins, 0, r);
                self.res_from_dest(addr, ins, bits, false);
            }
            M::Neg => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let r = simplify(Expr::Un(Un::Neg, b(a.clone())));
                self.set_flags(
                    addr,
                    Expr::Flags(FlagKind::Sub, b(Expr::Const(0, bits)), b(a)),
                    Expr::Flags(FlagKind::Res, b(r.clone()), b(r.clone())),
                );
                self.write(addr, ins, 0, r);
            }
            M::Not => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let r = simplify(Expr::Un(Un::Not, b(a)));
                self.write(addr, ins, 0, r);
            }
            M::Cmp => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                let c = if c.bits() != bits && c.bits() != 0 { c.sext(bits).trunc(bits) } else { c };
                self.flags_sub(addr, a, c);
            }
            M::Test => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                let r = simplify(Expr::bin(Bin::And, a, c));
                self.flags_res(addr, r, true);
            }
            M::Bt => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                self.assign(addr, Var::FlagOps, Expr::Flags(FlagKind::Bt, b(a), b(c)));
                self.assign(
                    addr,
                    Var::FlagRes,
                    Expr::Flags(FlagKind::Bt, b(Expr::Const(0, bits)), b(Expr::Const(0, bits))),
                );
            }
            M::Shl | M::Sal | M::Shr | M::Sar => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = if ins.op_count() > 1 { self.read(ins, 1, 8) } else { Expr::Const(1, 8) };
                let c = c.zext(bits.max(8)).trunc(bits);
                let op = match m {
                    M::Shl | M::Sal => Bin::Shl,
                    M::Shr => Bin::Shr,
                    _ => Bin::Sar,
                };
                let r = simplify(Expr::bin(op, a, c));
                self.flags_res(addr, r.clone(), false);
                self.write(addr, ins, 0, r);
            }
            M::Rol | M::Ror => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, 8).zext(bits.max(8)).trunc(bits);
                let name = if m == M::Rol { "rotl" } else { "rotr" };
                self.write(addr, ins, 0, Expr::Intr(name, vec![a, c], bits));
            }
            M::Imul if ins.op_count() >= 2 => {
                let bits = self.op_bits(ins, 0);
                let (x, y) = if ins.op_count() == 3 {
                    (self.read(ins, 1, bits), self.read(ins, 2, bits))
                } else {
                    (self.read(ins, 0, bits), self.read(ins, 1, bits))
                };
                let y = if y.bits() != bits { y.sext(bits).trunc(bits) } else { y };
                let r = simplify(Expr::bin(Bin::Mul, x, y));
                self.flags_res(addr, r.clone(), false);
                self.write(addr, ins, 0, r);
            }
            M::Imul | M::Mul => {
                let bits = self.op_bits(ins, 0);
                let x = self.read(ins, 0, bits);
                let a = Expr::Var(RAX_V, bits);
                let hi = Expr::Intr(if m == M::Mul { "umulh" } else { "smulh" }, vec![a.clone(), x.clone()], bits);
                let lo = simplify(Expr::bin(Bin::Mul, a, x));
                if bits == 8 {
                    self.raw(ins);
                } else {
                    self.assign(addr, RDX, hi.zext(64));
                    self.assign(addr, RAX, lo.zext(64));
                    self.assign(
                        addr,
                        Var::FlagOps,
                        Expr::Flags(FlagKind::Res, b(Expr::Const(0, 1)), b(Expr::Const(0, 1))),
                    );
                    self.assign(
                        addr,
                        Var::FlagRes,
                        Expr::Flags(FlagKind::Res, b(Expr::Const(0, 1)), b(Expr::Const(0, 1))),
                    );
                }
            }
            M::Div | M::Idiv => {
                let bits = self.op_bits(ins, 0);
                if bits < 32 {
                    self.raw(ins);
                } else {
                    let d = self.read(ins, 0, bits);
                    let n = Expr::Var(RAX_V, bits);
                    let (q, r) = if m == M::Div { (Bin::UDiv, Bin::UMod) } else { (Bin::SDiv, Bin::SMod) };
                    let rem = Expr::bin(r, n.clone(), d.clone());
                    let quo = Expr::bin(q, n, d);
                    self.assign(addr, RDX, simplify(rem).zext(64));
                    self.assign(addr, RAX, simplify(quo).zext(64));
                }
            }
            M::Cdq | M::Cqo => {
                let bits = if m == M::Cdq { 32 } else { 64 };
                let e = simplify(Expr::bin(Bin::Sar, Expr::Var(RAX_V, bits), Expr::Const(bits as u64 - 1, bits)));
                self.assign(addr, RDX, e.zext(64));
            }
            M::Cdqe => {
                self.assign(addr, RAX, Expr::Var(RAX_V, 32).sext(64));
            }
            M::Cwde => {
                self.assign(addr, RAX, Expr::Var(RAX_V, 16).sext(32).zext(64));
            }
            M::Cbw => {
                let e = Expr::Var(RAX_V, 8).sext(16);
                self.write_reg(addr, Register::AX, e);
            }
            M::Cwd => {
                let e = simplify(Expr::bin(Bin::Sar, Expr::Var(RAX_V, 16), Expr::Const(15, 16)));
                self.write_reg(addr, Register::DX, e);
            }
            M::Xchg => {
                let bits = self.op_bits(ins, 0);
                if ins.op0_kind() == OpKind::Register
                    && ins.op1_kind() == OpKind::Register
                    && ins.op0_register() == ins.op1_register()
                {
                    return false;
                }
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                self.assign(addr, Var::Tmp(0), a.zext(64));
                self.write(addr, ins, 0, c);
                let t = Expr::Var(Var::Tmp(0), bits);
                self.write(addr, ins, 1, t);
            }
            M::Seta
            | M::Setae
            | M::Setb
            | M::Setbe
            | M::Sete
            | M::Setg
            | M::Setge
            | M::Setl
            | M::Setle
            | M::Setne
            | M::Setno
            | M::Setnp
            | M::Setns
            | M::Seto
            | M::Setp
            | M::Sets => {
                let c = flag_cond(cc_of(ins));
                self.write(addr, ins, 0, c.zext(8));
            }
            M::Cmova
            | M::Cmovae
            | M::Cmovb
            | M::Cmovbe
            | M::Cmove
            | M::Cmovg
            | M::Cmovge
            | M::Cmovl
            | M::Cmovle
            | M::Cmovne
            | M::Cmovno
            | M::Cmovnp
            | M::Cmovns
            | M::Cmovo
            | M::Cmovp
            | M::Cmovs => {
                let bits = self.op_bits(ins, 0);
                let src = self.read(ins, 1, bits);
                let old = self.read(ins, 0, bits);
                let c = flag_cond(cc_of(ins));
                let sel = Expr::Select(b(c), b(src), b(old));
                self.write(addr, ins, 0, sel);
            }
            M::Sbb
                if ins.op_count() == 2
                    && ins.op0_kind() == OpKind::Register
                    && ins.op1_kind() == OpKind::Register
                    && ins.op0_register() == ins.op1_register() =>
            {
                let bits = self.op_bits(ins, 0);
                let c = flag_cond(Cc::B).zext(bits);
                self.write(addr, ins, 0, simplify(Expr::Un(Un::Neg, b(c))));
            }
            M::Adc | M::Sbb => {
                let bits = self.op_bits(ins, 0);
                let a = self.read(ins, 0, bits);
                let c = self.read(ins, 1, bits);
                let c = if c.bits() != bits && c.bits() != 0 { c.sext(bits).trunc(bits) } else { c };
                let cf = flag_cond(Cc::B).zext(bits);
                let (op, other) = if m == M::Adc { (Bin::Add, Bin::Add) } else { (Bin::Sub, Bin::Sub) };
                let r = simplify(Expr::bin(other, simplify(Expr::bin(op, a, c)), cf));
                self.set_flags(addr, Expr::Unknown(0), Expr::Flags(FlagKind::Res, b(r.clone()), b(r.clone())));
                self.write(addr, ins, 0, r);
            }
            M::Push => {
                let v = self.read(ins, 0, 64);
                let Some(o) = self.st.rsp else {
                    self.raw(ins);
                    return false;
                };
                let o = o - 8;
                self.st.rsp = Some(o);
                if self.in_prologue && ins.op0_kind() == OpKind::Register {
                    let r = gpr_number(ins.op0_register());
                    if self.abi.callee_saved(r) {
                        self.saves.insert(o);
                    }
                }
                self.emit(addr, Stmt::Store(Expr::StackAddr(o), 64, v));
            }
            M::Pop => {
                let Some(o) = self.st.rsp else {
                    self.raw(ins);
                    return false;
                };
                self.st.rsp = Some(o + 8);
                let v = Expr::StackAddr(o).load(64);
                if ins.op0_register() == Register::RBP {
                    self.st.rbp = None;
                }
                self.write_reg(addr, ins.op0_register(), v);
            }
            M::Leave => {
                if let Some(o) = self.st.rbp {
                    self.st.rsp = Some(o + 8);
                    let v = Expr::StackAddr(o).load(64);
                    self.st.rbp = None;
                    self.assign(addr, Var::Reg(5), v);
                } else {
                    self.st.rsp = None;
                    self.raw(ins);
                }
            }
            M::Call => return self.do_call(ins),
            M::Movss | M::Movsd | M::Movaps | M::Movapd | M::Movups | M::Movdqa | M::Movdqu | M::Movq | M::Movd
                if self.simple_xmm(ins) =>
            {
                self.xmm_move(ins);
            }
            M::Pxor | M::Xorps | M::Xorpd
                if ins.op0_kind() == OpKind::Register
                    && ins.op1_kind() == OpKind::Register
                    && ins.op0_register() == ins.op1_register() =>
            {
                self.write_reg(addr, ins.op0_register(), Expr::Const(0, 64));
            }
            M::Addsd | M::Subsd | M::Mulsd | M::Divsd | M::Addss | M::Subss | M::Mulss | M::Divss => {
                let w: Bits = if matches!(m, M::Addsd | M::Subsd | M::Mulsd | M::Divsd) { 64 } else { 32 };
                let a = self.xmm_read(ins, 0, w);
                let c = self.xmm_read(ins, 1, w);
                let op = match m {
                    M::Addsd | M::Addss => Bin::FAdd,
                    M::Subsd | M::Subss => Bin::FSub,
                    M::Mulsd | M::Mulss => Bin::FMul,
                    _ => Bin::FDiv,
                };
                self.write_reg(addr, ins.op0_register(), Expr::bin(op, a, c));
            }
            M::Cvtsi2sd | M::Cvtsi2ss => {
                let bits = self.op_bits(ins, 1).max(32);
                let src = self.read(ins, 1, bits);
                let w: Bits = if m == M::Cvtsi2sd { 64 } else { 32 };
                self.write_reg(
                    addr,
                    ins.op0_register(),
                    Expr::Intr(if w == 64 { "(double)" } else { "(float)" }, vec![src], w),
                );
            }
            M::Cvttsd2si | M::Cvttss2si | M::Cvtsd2si | M::Cvtss2si => {
                let bits = self.op_bits(ins, 0);
                let w: Bits = if matches!(m, M::Cvttsd2si | M::Cvtsd2si) { 64 } else { 32 };
                let src = self.xmm_read(ins, 1, w);
                self.write(addr, ins, 0, Expr::Intr("(int)", vec![src], bits));
            }
            M::Cvtss2sd => {
                let src = self.xmm_read(ins, 1, 32);
                self.write_reg(addr, ins.op0_register(), Expr::Intr("(double)", vec![src], 64));
            }
            M::Cvtsd2ss => {
                let src = self.xmm_read(ins, 1, 64);
                self.write_reg(addr, ins.op0_register(), Expr::Intr("(float)", vec![src], 32));
            }
            M::Ucomisd | M::Comisd | M::Ucomiss | M::Comiss => {
                let w: Bits = if matches!(m, M::Ucomisd | M::Comisd) { 64 } else { 32 };
                let a = self.xmm_read(ins, 0, w);
                let c = self.xmm_read(ins, 1, w);
                self.assign(addr, Var::FlagOps, Expr::Flags(FlagKind::FCmp, b(a.clone()), b(c.clone())));
                self.assign(addr, Var::FlagRes, Expr::Flags(FlagKind::FCmp, b(a), b(c)));
            }
            _ => self.raw(ins),
        }
        false
    }

    /// `mov rbp, rsp` and `lea rbp, [rsp+x]`: the offset the frame pointer holds.
    fn reg_to_off(&self, ins: &Instruction) -> Option<i64> {
        if ins.op1_kind() == OpKind::Register && ins.op1_register() == Register::RSP {
            return self.st.rsp;
        }
        None
    }

    fn simple_xmm(&self, ins: &Instruction) -> bool {
        let reg_ok = |n: u32| match ins.op_kind(n) {
            OpKind::Register => {
                let r = ins.op_register(n);
                r.is_xmm() || r.is_gpr64() || r.is_gpr32()
            }
            OpKind::Memory => true,
            _ => false,
        };
        // packed moves of 16 bytes are left raw
        let packed = matches!(
            ins.mnemonic(),
            Mnemonic::Movaps | Mnemonic::Movapd | Mnemonic::Movups | Mnemonic::Movdqa | Mnemonic::Movdqu
        );
        if packed {
            let x = |n: u32| {
                ins.op_kind(n) == OpKind::Memory || (ins.op_kind(n) == OpKind::Register && ins.op_register(n).is_xmm())
            };
            return x(0) && x(1);
        }
        reg_ok(0) && reg_ok(1)
    }

    fn xmm_bits(ins: &Instruction) -> Bits {
        match ins.mnemonic() {
            Mnemonic::Movaps | Mnemonic::Movapd | Mnemonic::Movups | Mnemonic::Movdqa | Mnemonic::Movdqu => 128,
            Mnemonic::Movss => 32,
            Mnemonic::Movd => 32,
            _ => 64,
        }
    }

    fn xmm_read(&mut self, ins: &Instruction, n: u32, w: Bits) -> Expr {
        match ins.op_kind(n) {
            OpKind::Register => {
                let r = ins.op_register(n);
                if r.is_xmm() {
                    Expr::Var(Var::Xmm(r.number() as u8), w)
                } else {
                    self.reg_read(r)
                }
            }
            OpKind::Memory => {
                let a = self.mem_addr(ins);
                a.load(w)
            }
            _ => Expr::Unknown(w),
        }
    }

    fn xmm_move(&mut self, ins: &Instruction) {
        let addr = ins.ip();
        let w = Self::xmm_bits(ins);
        let v = self.xmm_read(ins, 1, w);
        match ins.op_kind(0) {
            OpKind::Register => {
                let r = ins.op_register(0);
                if r.is_xmm() {
                    let v = if ins.op_kind(1) == OpKind::Register && !ins.op_register(1).is_xmm() {
                        Expr::Intr("as_float", vec![v], w)
                    } else {
                        v
                    };
                    self.assign(addr, Var::Xmm(r.number() as u8), v);
                } else {
                    let v = if v.bits() != w { v.trunc(w) } else { v };
                    self.write_reg(addr, r, Expr::Intr("as_int", vec![v], w).zext(64));
                }
            }
            OpKind::Memory => {
                let a = self.mem_addr(ins);
                self.emit(addr, Stmt::Store(a, w, v.trunc(w)));
            }
            _ => self.raw(ins),
        }
    }
}
