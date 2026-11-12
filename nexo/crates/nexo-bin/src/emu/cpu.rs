//! The integer core: registers, flags and the instructions compiled C uses. An instruction this file does
//! not know is never skipped: it comes back as [`Event::Unsupported`] and the run stops before it.

use iced_x86::{ConditionCode, Instruction, Mnemonic, OpKind, Register};

use super::mem::{Fault, Mem};

pub(crate) const CF: u64 = 1;
pub(crate) const PF: u64 = 1 << 2;
pub(crate) const AF: u64 = 1 << 4;
pub(crate) const ZF: u64 = 1 << 6;
pub(crate) const SF: u64 = 1 << 7;
pub(crate) const DF: u64 = 1 << 10;
pub(crate) const OF: u64 = 1 << 11;
const ARITH: u64 = CF | PF | AF | ZF | SF | OF;

/// The names of the 16 general registers in encoding order.
pub(crate) const GPR_NAMES: [&str; 16] =
    ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
pub(crate) const RAX: usize = 0;
pub(crate) const RCX: usize = 1;
pub(crate) const RDX: usize = 2;
pub(crate) const RSP: usize = 4;
pub(crate) const RBP: usize = 5;
pub(crate) const RSI: usize = 6;
pub(crate) const RDI: usize = 7;

/// Why an instruction did not run. The program counter is left on the instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Fault(Fault),
    /// An instruction (or a form of one) the emulator does not run.
    Unsupported(String),
    DivideError,
    /// `hlt`, `int3`, `ud2`, `int n`: the program asked to stop or trap.
    Trap(&'static str),
    /// A system call, with the number in rax.
    Syscall(u64),
    /// The step budget ran out inside a `rep` string instruction.
    Budget,
}

impl From<Fault> for Event {
    fn from(f: Fault) -> Event {
        Event::Fault(f)
    }
}

type R<T> = Result<T, Event>;

#[derive(Debug, Clone)]
pub(crate) struct Cpu {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub flags: u64,
    pub xmm: [u128; 16],
    /// The high 128 bits of each ymm register (AVX).
    pub ymmh: [u128; 16],
    pub fs_base: u64,
    pub gs_base: u64,
    /// Steps a `rep` instruction may still take (each repetition counts as one step).
    pub budget: u64,
    /// Repetitions taken by the last `rep` instruction beyond the first.
    pub extra_steps: u64,
}

pub(crate) fn mask(size: usize) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

fn msb(size: usize) -> u64 {
    1u64 << (size * 8 - 1)
}

fn sext(v: u64, size: usize) -> i64 {
    let sh = 64 - size * 8;
    ((v << sh) as i64) >> sh
}

pub(crate) fn gpr_index(r: Register) -> usize {
    r.full_register().number()
}

fn is_high8(r: Register) -> bool {
    matches!(r, Register::AH | Register::CH | Register::DH | Register::BH)
}

impl Cpu {
    pub(crate) fn new() -> Cpu {
        Cpu {
            gpr: [0; 16],
            rip: 0,
            flags: 0x202,
            xmm: [0; 16],
            ymmh: [0; 16],
            fs_base: 0,
            gs_base: 0,
            budget: 0,
            extra_steps: 0,
        }
    }

    fn flag(&self, f: u64) -> bool {
        self.flags & f != 0
    }

    fn set(&mut self, f: u64, on: bool) {
        if on {
            self.flags |= f
        } else {
            self.flags &= !f
        }
    }

    pub(crate) fn reg(&self, r: Register) -> R<u64> {
        if r.is_gpr() {
            let v = self.gpr[gpr_index(r)];
            return Ok(if is_high8(r) { (v >> 8) & 0xff } else { v & mask(r.size()) });
        }
        match r {
            Register::RIP => Ok(self.rip),
            Register::FS => Ok(self.fs_base),
            Register::GS => Ok(self.gs_base),
            Register::ES | Register::CS | Register::SS | Register::DS | Register::None => Ok(0),
            _ => Err(Event::Unsupported(format!("register {r:?}").to_lowercase())),
        }
    }

    pub(crate) fn set_reg(&mut self, r: Register, v: u64) -> R<()> {
        if !r.is_gpr() {
            return Err(Event::Unsupported(format!("write to register {r:?}").to_lowercase()));
        }
        let i = gpr_index(r);
        let old = self.gpr[i];
        self.gpr[i] = match r.size() {
            1 if is_high8(r) => (old & !0xff00) | ((v & 0xff) << 8),
            1 => (old & !0xff) | (v & 0xff),
            2 => (old & !0xffff) | (v & 0xffff),
            4 => v & 0xffff_ffff,
            _ => v,
        };
        Ok(())
    }

    /// The address a memory operand points at (segment base included).
    pub(crate) fn addr(&self, ins: &Instruction, op: u32) -> u64 {
        ins.virtual_address(op, 0, |r, _, _| self.reg(r).ok()).unwrap_or(0)
    }

    /// The size in bytes of operand `op`.
    pub(crate) fn opsize(ins: &Instruction, op: u32) -> usize {
        match ins.op_kind(op) {
            OpKind::Register => ins.op_register(op).size(),
            OpKind::Memory => ins.memory_size().size(),
            _ => 8,
        }
    }

    pub(crate) fn read(&self, mem: &Mem, ins: &Instruction, op: u32, size: usize) -> R<u64> {
        match ins.op_kind(op) {
            OpKind::Register => self.reg(ins.op_register(op)).map(|v| v & mask(size)),
            OpKind::Memory => Ok(mem.read_u(self.addr(ins, op), size)?),
            OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => Ok(ins.immediate(op) & mask(size)),
            k => Err(Event::Unsupported(format!("operand kind {k:?}"))),
        }
    }

    pub(crate) fn write(&mut self, mem: &mut Mem, ins: &Instruction, op: u32, size: usize, v: u64) -> R<()> {
        match ins.op_kind(op) {
            OpKind::Register => self.set_reg(ins.op_register(op), v & mask(size)),
            OpKind::Memory => Ok(mem.write_u(self.addr(ins, op), size, v)?),
            k => Err(Event::Unsupported(format!("write to operand kind {k:?}"))),
        }
    }

    pub(crate) fn push(&mut self, mem: &mut Mem, v: u64) -> R<()> {
        let sp = self.gpr[RSP].wrapping_sub(8);
        mem.write_u(sp, 8, v)?;
        self.gpr[RSP] = sp;
        Ok(())
    }

    pub(crate) fn pop(&mut self, mem: &Mem) -> R<u64> {
        let v = mem.read_u(self.gpr[RSP], 8)?;
        self.gpr[RSP] = self.gpr[RSP].wrapping_add(8);
        Ok(v)
    }

    fn szp(&mut self, r: u64, size: usize) {
        let r = r & mask(size);
        self.set(ZF, r == 0);
        self.set(SF, r & msb(size) != 0);
        self.set(PF, (r as u8).count_ones().is_multiple_of(2));
    }

    fn logic(&mut self, r: u64, size: usize) {
        self.flags &= !(CF | OF | AF);
        self.szp(r, size);
    }

    fn add_flags(&mut self, a: u64, b: u64, carry: u64, size: usize) -> u64 {
        let m = mask(size);
        let full = (a & m) as u128 + (b & m) as u128 + carry as u128;
        let r = full as u64 & m;
        self.set(CF, full > m as u128);
        self.set(OF, (a ^ r) & (b ^ r) & msb(size) != 0);
        self.set(AF, (a ^ b ^ r) & 0x10 != 0);
        self.szp(r, size);
        r
    }

    fn sub_flags(&mut self, a: u64, b: u64, borrow: u64, size: usize) -> u64 {
        let m = mask(size);
        let (a, b) = (a & m, b & m);
        let r = a.wrapping_sub(b).wrapping_sub(borrow) & m;
        self.set(CF, (a as u128) < b as u128 + borrow as u128);
        self.set(OF, (a ^ b) & (a ^ r) & msb(size) != 0);
        self.set(AF, (a ^ b ^ r) & 0x10 != 0);
        self.szp(r, size);
        r
    }

    pub(crate) fn cond(&self, cc: ConditionCode) -> bool {
        let (c, z, s, o, p) = (self.flag(CF), self.flag(ZF), self.flag(SF), self.flag(OF), self.flag(PF));
        match cc {
            ConditionCode::o => o,
            ConditionCode::no => !o,
            ConditionCode::b => c,
            ConditionCode::ae => !c,
            ConditionCode::e => z,
            ConditionCode::ne => !z,
            ConditionCode::be => c || z,
            ConditionCode::a => !c && !z,
            ConditionCode::s => s,
            ConditionCode::ns => !s,
            ConditionCode::p => p,
            ConditionCode::np => !p,
            ConditionCode::l => s != o,
            ConditionCode::ge => s == o,
            ConditionCode::le => z || s != o,
            ConditionCode::g => !z && s == o,
            ConditionCode::None => true,
        }
    }

    /// Run one integer instruction. `rip` already points past it. `Ok(false)` means the mnemonic is not one
    /// this core knows (the caller tries the vector unit next).
    pub(crate) fn exec(&mut self, mem: &mut Mem, ins: &Instruction) -> R<bool> {
        use Mnemonic as M;
        let m = ins.mnemonic();
        match m {
            M::Nop
            | M::Endbr64
            | M::Endbr32
            | M::Pause
            | M::Lfence
            | M::Mfence
            | M::Sfence
            | M::Prefetch
            | M::Prefetchnta
            | M::Prefetcht0
            | M::Prefetcht1
            | M::Prefetcht2
            | M::Prefetchw => {}
            M::Mov => {
                let s = Self::opsize(ins, 0);
                let v = self.read(mem, ins, 1, s)?;
                self.write(mem, ins, 0, s, v)?;
            }
            M::Movzx => {
                let (d, s) = (Self::opsize(ins, 0), Self::opsize(ins, 1));
                let v = self.read(mem, ins, 1, s)?;
                self.write(mem, ins, 0, d, v)?;
            }
            M::Movsx | M::Movsxd => {
                let (d, s) = (Self::opsize(ins, 0), Self::opsize(ins, 1));
                let v = sext(self.read(mem, ins, 1, s)?, s) as u64;
                self.write(mem, ins, 0, d, v)?;
            }
            M::Lea => {
                let d = Self::opsize(ins, 0);
                let a = ins.virtual_address(1, 0, |r, _, _| match r {
                    Register::FS | Register::GS => Some(0),
                    _ => self.reg(r).ok(),
                });
                self.write(mem, ins, 0, d, a.unwrap_or(0))?;
            }
            M::Push => {
                let s = match ins.op0_kind() {
                    OpKind::Register | OpKind::Memory => Self::opsize(ins, 0),
                    _ => 8,
                };
                if s != 8 {
                    return Err(Event::Unsupported(format!("push of {s} bytes")));
                }
                let v = self.read(mem, ins, 0, 8)?;
                self.push(mem, v)?;
            }
            M::Pop => {
                let s = Self::opsize(ins, 0);
                if s != 8 {
                    return Err(Event::Unsupported(format!("pop of {s} bytes")));
                }
                let v = self.pop(mem)?;
                self.write(mem, ins, 0, 8, v)?;
            }
            M::Xchg => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                let b = self.read(mem, ins, 1, s)?;
                self.write(mem, ins, 0, s, b)?;
                self.write(mem, ins, 1, s, a)?;
            }
            M::Add | M::Adc | M::Sub | M::Sbb | M::Cmp | M::And | M::Or | M::Xor | M::Test => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                let b = self.read(mem, ins, 1, s)?;
                let c = self.flag(CF) as u64;
                let r = match m {
                    M::Add => self.add_flags(a, b, 0, s),
                    M::Adc => self.add_flags(a, b, c, s),
                    M::Sub | M::Cmp => self.sub_flags(a, b, 0, s),
                    M::Sbb => self.sub_flags(a, b, c, s),
                    M::And | M::Test => a & b,
                    M::Or => a | b,
                    _ => a ^ b,
                };
                if matches!(m, M::And | M::Or | M::Xor | M::Test) {
                    self.logic(r, s);
                }
                if !matches!(m, M::Cmp | M::Test) {
                    self.write(mem, ins, 0, s, r)?;
                }
            }
            M::Inc | M::Dec => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                let c = self.flag(CF);
                let r = if m == M::Inc { self.add_flags(a, 1, 0, s) } else { self.sub_flags(a, 1, 0, s) };
                self.set(CF, c);
                self.write(mem, ins, 0, s, r)?;
            }
            M::Neg => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                let r = self.sub_flags(0, a, 0, s);
                self.write(mem, ins, 0, s, r)?;
            }
            M::Not => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                self.write(mem, ins, 0, s, !a)?;
            }
            M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr => self.shift(mem, ins, m)?,
            M::Shld | M::Shrd => self.double_shift(mem, ins, m)?,
            M::Imul => self.imul(mem, ins)?,
            M::Mul => {
                let s = Self::opsize(ins, 0);
                let b = self.read(mem, ins, 0, s)? as u128;
                let a = (self.gpr[RAX] & mask(s)) as u128;
                let r = a * b;
                let hi = (r >> (s * 8)) as u64 & mask(s);
                self.store_wide(s, r as u64 & mask(s), hi);
                self.set(CF, hi != 0);
                self.set(OF, hi != 0);
            }
            M::Div | M::Idiv => self.divide(mem, ins, m == M::Idiv)?,
            M::Cbw => {
                let v = sext(self.gpr[RAX] & 0xff, 1) as u64;
                self.set_reg(Register::AX, v)?;
            }
            M::Cwde => {
                let v = sext(self.gpr[RAX] & 0xffff, 2) as u64;
                self.set_reg(Register::EAX, v)?;
            }
            M::Cdqe => self.gpr[RAX] = sext(self.gpr[RAX] & 0xffff_ffff, 4) as u64,
            M::Cwd => {
                let v = if self.gpr[RAX] & 0x8000 != 0 { 0xffff } else { 0 };
                self.set_reg(Register::DX, v)?;
            }
            M::Cdq => {
                let v = if self.gpr[RAX] & 0x8000_0000 != 0 { 0xffff_ffff } else { 0 };
                self.set_reg(Register::EDX, v)?;
            }
            M::Cqo => self.gpr[RDX] = if self.gpr[RAX] >> 63 != 0 { u64::MAX } else { 0 },
            M::Jmp => {
                self.rip = self.branch_target(mem, ins)?;
            }
            M::Call => {
                let t = self.branch_target(mem, ins)?;
                let back = ins.next_ip();
                self.push(mem, back)?;
                self.rip = t;
            }
            M::Ret => {
                let t = self.pop(mem)?;
                if ins.op_count() == 1 {
                    self.gpr[RSP] = self.gpr[RSP].wrapping_add(ins.immediate(0) & 0xffff);
                }
                self.rip = t;
            }
            M::Leave => {
                self.gpr[RSP] = self.gpr[RBP];
                self.gpr[RBP] = self.pop(mem)?;
            }
            M::Jrcxz | M::Jecxz => {
                let s = if m == M::Jrcxz { 8 } else { 4 };
                if self.gpr[RCX] & mask(s) == 0 {
                    self.rip = ins.near_branch_target();
                }
            }
            M::Loop | M::Loope | M::Loopne => {
                self.gpr[RCX] = self.gpr[RCX].wrapping_sub(1);
                let z = self.flag(ZF);
                let go = self.gpr[RCX] != 0 && (m == M::Loop || (m == M::Loope) == z);
                if go {
                    self.rip = ins.near_branch_target();
                }
            }
            M::Bt | M::Bts | M::Btr | M::Btc => self.bit_test(mem, ins, m)?,
            M::Bsf | M::Bsr | M::Tzcnt | M::Lzcnt | M::Popcnt => {
                let s = Self::opsize(ins, 0);
                let v = self.read(mem, ins, 1, s)?;
                let bits = (s * 8) as u64;
                match m {
                    M::Bsf | M::Bsr => {
                        self.set(ZF, v == 0);
                        if v != 0 {
                            let r = if m == M::Bsf { v.trailing_zeros() as u64 } else { 63 - v.leading_zeros() as u64 };
                            self.write(mem, ins, 0, s, r)?;
                        }
                    }
                    M::Tzcnt | M::Lzcnt => {
                        let r = if v == 0 {
                            bits
                        } else if m == M::Tzcnt {
                            v.trailing_zeros() as u64
                        } else {
                            v.leading_zeros() as u64 - (64 - bits)
                        };
                        self.set(CF, v == 0);
                        self.set(ZF, r == 0);
                        self.write(mem, ins, 0, s, r)?;
                    }
                    _ => {
                        let r = v.count_ones() as u64;
                        self.flags &= !ARITH;
                        self.set(ZF, v == 0);
                        self.write(mem, ins, 0, s, r)?;
                    }
                }
            }
            M::Bswap => {
                let s = Self::opsize(ins, 0);
                let v = self.read(mem, ins, 0, s)?;
                let r = if s == 8 { v.swap_bytes() } else { (v as u32).swap_bytes() as u64 };
                self.write(mem, ins, 0, s, r)?;
            }
            M::Cld => self.set(DF, false),
            M::Std => self.set(DF, true),
            M::Clc => self.set(CF, false),
            M::Stc => self.set(CF, true),
            M::Cmc => self.flags ^= CF,
            M::Xadd => {
                let s = Self::opsize(ins, 0);
                let a = self.read(mem, ins, 0, s)?;
                let b = self.read(mem, ins, 1, s)?;
                let r = self.add_flags(a, b, 0, s);
                self.write(mem, ins, 1, s, a)?;
                self.write(mem, ins, 0, s, r)?;
            }
            M::Cmpxchg => {
                let s = Self::opsize(ins, 0);
                let d = self.read(mem, ins, 0, s)?;
                let acc = self.gpr[RAX] & mask(s);
                self.sub_flags(acc, d, 0, s);
                if acc == d {
                    let v = self.read(mem, ins, 1, s)?;
                    self.write(mem, ins, 0, s, v)?;
                } else {
                    let acc_reg = match s {
                        1 => Register::AL,
                        2 => Register::AX,
                        4 => Register::EAX,
                        _ => Register::RAX,
                    };
                    self.set_reg(acc_reg, d)?;
                }
            }
            M::Sahf => {
                let ah = (self.gpr[RAX] >> 8) & 0xd5;
                self.flags = (self.flags & !0xd5) | ah;
            }
            M::Lahf => {
                let v = (self.flags & 0xd5) | 2;
                self.set_reg(Register::AH, v)?;
            }
            M::Hlt => return Err(Event::Trap("hlt")),
            M::Int3 => return Err(Event::Trap("int3")),
            M::Ud2 => return Err(Event::Trap("ud2")),
            M::Int | M::Int1 | M::Into => return Err(Event::Trap("int")),
            M::Syscall => return Err(Event::Syscall(self.gpr[RAX])),
            M::Movsb
            | M::Movsw
            | M::Movsq
            | M::Stosb
            | M::Stosw
            | M::Stosd
            | M::Stosq
            | M::Lodsb
            | M::Lodsw
            | M::Lodsd
            | M::Lodsq
            | M::Cmpsb
            | M::Cmpsw
            | M::Cmpsq
            | M::Scasb
            | M::Scasw
            | M::Scasd
            | M::Scasq => self.string_op(mem, ins, m)?,
            // these two names are also SSE instructions; the string forms have no register operand
            M::Movsd | M::Cmpsd if ins.op0_kind() != OpKind::Register && ins.op1_kind() != OpKind::Register => {
                self.string_op(mem, ins, m)?
            }
            _ => {
                if ins.condition_code() != ConditionCode::None {
                    return self.conditional(mem, ins);
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn conditional(&mut self, mem: &mut Mem, ins: &Instruction) -> R<bool> {
        let ok = self.cond(ins.condition_code());
        // a jcc branches, a setcc has one operand, a cmovcc two (other instructions with a condition, such as
        // the x87 fcmov, are refused below by the operand checks)
        if ins.flow_control() == iced_x86::FlowControl::ConditionalBranch && ins.op0_kind() == OpKind::NearBranch64 {
            if ok {
                self.rip = ins.near_branch_target();
            }
        } else if ins.op_count() == 1 && Self::opsize(ins, 0) == 1 && !ins.op_register(0).is_vector_register() {
            self.write(mem, ins, 0, 1, ok as u64)?;
        } else if ins.op_count() == 2
            && ins.op0_kind() == OpKind::Register
            && ins.op_register(0).is_gpr()
            && matches!(ins.op1_kind(), OpKind::Register | OpKind::Memory)
        {
            let s = Self::opsize(ins, 0);
            // the source is read even when the move does not happen, like the CPU does
            let v = self.read(mem, ins, 1, s)?;
            if ok {
                self.write(mem, ins, 0, s, v)?;
            } else if s == 4 {
                // a 32-bit cmov clears the upper half of the destination either way
                let d = self.read(mem, ins, 0, 4)?;
                self.write(mem, ins, 0, 4, d)?;
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn branch_target(&self, mem: &Mem, ins: &Instruction) -> R<u64> {
        match ins.op0_kind() {
            OpKind::NearBranch64 | OpKind::NearBranch32 | OpKind::NearBranch16 => Ok(ins.near_branch_target()),
            OpKind::Register | OpKind::Memory => self.read(mem, ins, 0, 8),
            k => Err(Event::Unsupported(format!("far branch ({k:?})"))),
        }
    }

    /// Low and high halves of a widening multiply or a divide, into rax/rdx (or al/ah).
    fn store_wide(&mut self, s: usize, lo: u64, hi: u64) {
        if s == 1 {
            let ax = (lo & 0xff) | ((hi & 0xff) << 8);
            let _ = self.set_reg(Register::AX, ax);
        } else {
            let (a, d) = match s {
                2 => (Register::AX, Register::DX),
                4 => (Register::EAX, Register::EDX),
                _ => (Register::RAX, Register::RDX),
            };
            let _ = self.set_reg(a, lo);
            let _ = self.set_reg(d, hi);
        }
    }

    fn imul(&mut self, mem: &mut Mem, ins: &Instruction) -> R<()> {
        let s = Self::opsize(ins, 0);
        let bits = s * 8;
        match ins.op_count() {
            1 => {
                let b = sext(self.read(mem, ins, 0, s)?, s) as i128;
                let a = sext(self.gpr[RAX] & mask(s), s) as i128;
                let r = a * b;
                let lo = r as u64 & mask(s);
                let hi = (r >> bits) as u64 & mask(s);
                self.store_wide(s, lo, hi);
                let fits = sext(lo, s) as i128 == r;
                self.set(CF, !fits);
                self.set(OF, !fits);
            }
            n => {
                let (x, y) = if n == 2 { (0, 1) } else { (1, 2) };
                let a = sext(self.read(mem, ins, x, s)?, s) as i128;
                let b = sext(self.read(mem, ins, y, s)?, s) as i128;
                let r = a * b;
                let lo = r as u64 & mask(s);
                let fits = sext(lo, s) as i128 == r;
                self.szp(lo, s);
                self.set(CF, !fits);
                self.set(OF, !fits);
                self.write(mem, ins, 0, s, lo)?;
            }
        }
        Ok(())
    }

    fn divide(&mut self, mem: &mut Mem, ins: &Instruction, signed: bool) -> R<()> {
        let s = Self::opsize(ins, 0);
        let bits = s * 8;
        let d = self.read(mem, ins, 0, s)?;
        if d == 0 {
            return Err(Event::DivideError);
        }
        let (lo, hi) = if s == 1 {
            (self.gpr[RAX] & 0xff, (self.gpr[RAX] >> 8) & 0xff)
        } else {
            (self.gpr[RAX] & mask(s), self.gpr[RDX] & mask(s))
        };
        let n = ((hi as u128) << bits) | lo as u128;
        if signed {
            let n = ((n << (128 - 2 * bits)) as i128) >> (128 - 2 * bits);
            let d = sext(d, s) as i128;
            let (q, r) = (n / d, n % d);
            let lim = 1i128 << (bits - 1);
            if q >= lim || q < -lim {
                return Err(Event::DivideError);
            }
            self.store_wide(s, q as u64 & mask(s), r as u64 & mask(s));
        } else {
            let (q, r) = (n / d as u128, n % d as u128);
            if q > mask(s) as u128 {
                return Err(Event::DivideError);
            }
            self.store_wide(s, q as u64, r as u64);
        }
        Ok(())
    }

    fn shift(&mut self, mem: &mut Mem, ins: &Instruction, m: Mnemonic) -> R<()> {
        use Mnemonic as M;
        let s = Self::opsize(ins, 0);
        let bits = (s * 8) as u32;
        let a = self.read(mem, ins, 0, s)?;
        let raw = self.read(mem, ins, 1, 1)? as u32;
        let cnt = raw & if s == 8 { 0x3f } else { 0x1f };
        if cnt == 0 {
            return Ok(());
        }
        let top = |v: u64| v & msb(s) != 0;
        let r = match m {
            M::Shl | M::Sal => {
                let wide = (a as u128) << cnt;
                let r = wide as u64 & mask(s);
                let c = (wide >> bits) & 1 != 0;
                self.szp(r, s);
                self.set(CF, c);
                self.set(OF, top(r) != c);
                r
            }
            M::Shr => {
                let r = if cnt >= 64 { 0 } else { a >> cnt };
                let c = cnt <= 64 && (a as u128 >> (cnt - 1)) & 1 != 0;
                self.szp(r, s);
                self.set(CF, c);
                self.set(OF, top(a));
                r
            }
            M::Sar => {
                let v = sext(a, s) as i128;
                let r = (v >> cnt) as u64 & mask(s);
                let c = (v >> (cnt - 1)) & 1 != 0;
                self.szp(r, s);
                self.set(CF, c);
                self.set(OF, false);
                r
            }
            M::Rol | M::Ror => {
                let n = cnt % bits;
                let r = if n == 0 {
                    a
                } else if m == M::Rol {
                    ((a << n) | (a >> (bits - n))) & mask(s)
                } else {
                    ((a >> n) | (a << (bits - n))) & mask(s)
                };
                if m == M::Rol {
                    let c = r & 1 != 0;
                    self.set(CF, c);
                    self.set(OF, top(r) != c);
                } else {
                    self.set(CF, top(r));
                    self.set(OF, top(r) != (r & (msb(s) >> 1) != 0));
                }
                r
            }
            _ => {
                // rcl / rcr: rotate through the carry, one bit at a time
                let n = cnt % (bits + 1);
                let mut v = a;
                let mut c = self.flag(CF);
                for _ in 0..n {
                    if m == M::Rcl {
                        let out = top(v);
                        v = ((v << 1) | c as u64) & mask(s);
                        c = out;
                    } else {
                        let out = v & 1 != 0;
                        v = (v >> 1) | if c { msb(s) } else { 0 };
                        c = out;
                    }
                }
                self.set(CF, c);
                if m == M::Rcl {
                    self.set(OF, top(v) != c);
                } else {
                    self.set(OF, top(v) != (v & (msb(s) >> 1) != 0));
                }
                v
            }
        };
        self.write(mem, ins, 0, s, r)
    }

    fn double_shift(&mut self, mem: &mut Mem, ins: &Instruction, m: Mnemonic) -> R<()> {
        let s = Self::opsize(ins, 0);
        let bits = (s * 8) as u32;
        let d = self.read(mem, ins, 0, s)?;
        let src = self.read(mem, ins, 1, s)?;
        let cnt = (self.read(mem, ins, 2, 1)? as u32) & if s == 8 { 0x3f } else { 0x1f };
        if cnt == 0 {
            return Ok(());
        }
        if cnt > bits {
            return Err(Event::Unsupported(format!("{:?} by {cnt} on {bits} bits", m).to_lowercase()));
        }
        let (r, c) = if m == Mnemonic::Shld {
            let v = ((d as u128) << bits) | src as u128;
            (((v << cnt) >> bits) as u64 & mask(s), (d as u128 >> (bits - cnt)) & 1 != 0)
        } else {
            let v = ((src as u128) << bits) | d as u128;
            ((v >> cnt) as u64 & mask(s), (d as u128 >> (cnt - 1)) & 1 != 0)
        };
        self.szp(r, s);
        self.set(CF, c);
        self.set(OF, (r ^ d) & msb(s) != 0);
        self.write(mem, ins, 0, s, r)
    }

    fn bit_test(&mut self, mem: &mut Mem, ins: &Instruction, m: Mnemonic) -> R<()> {
        let s = Self::opsize(ins, 0);
        let bits = (s * 8) as i64;
        let off = self.read(mem, ins, 1, if ins.op1_kind() == OpKind::Register { s } else { 1 })?;
        let (addr_shift, bit) = if ins.op0_kind() == OpKind::Memory && ins.op1_kind() == OpKind::Register {
            let o = sext(off, s);
            (o.div_euclid(bits) * (s as i64), o.rem_euclid(bits) as u32)
        } else {
            (0, (off % bits as u64) as u32)
        };
        let (v, a) = if ins.op0_kind() == OpKind::Memory {
            let a = self.addr(ins, 0).wrapping_add(addr_shift as u64);
            (mem.read_u(a, s)?, Some(a))
        } else {
            (self.read(mem, ins, 0, s)?, None)
        };
        self.set(CF, (v >> bit) & 1 != 0);
        let r = match m {
            Mnemonic::Bts => v | (1 << bit),
            Mnemonic::Btr => v & !(1 << bit),
            Mnemonic::Btc => v ^ (1 << bit),
            _ => return Ok(()),
        };
        match a {
            Some(a) => mem.write_u(a, s, r)?,
            None => self.write(mem, ins, 0, s, r)?,
        }
        Ok(())
    }

    fn string_op(&mut self, mem: &mut Mem, ins: &Instruction, m: Mnemonic) -> R<()> {
        use Mnemonic as M;
        let size = match m {
            M::Movsb | M::Stosb | M::Lodsb | M::Cmpsb | M::Scasb => 1,
            M::Movsw | M::Stosw | M::Lodsw | M::Cmpsw | M::Scasw => 2,
            M::Movsd | M::Stosd | M::Lodsd | M::Cmpsd | M::Scasd => 4,
            _ => 8,
        };
        let compares =
            matches!(m, M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Cmpsq | M::Scasb | M::Scasw | M::Scasd | M::Scasq);
        let rep = ins.has_rep_prefix() || ins.has_repne_prefix();
        let step = if self.flag(DF) { (size as u64).wrapping_neg() } else { size as u64 };
        let src_base = if ins.op0_kind() == OpKind::MemorySegRSI || ins.op1_kind() == OpKind::MemorySegRSI {
            self.reg(ins.memory_segment())?
        } else {
            0
        };
        let mut first = true;
        loop {
            if rep && self.gpr[RCX] == 0 {
                break;
            }
            if !first {
                if self.budget == 0 {
                    return Err(Event::Budget);
                }
                self.budget -= 1;
                self.extra_steps += 1;
            }
            first = false;
            let (si, di) = (self.gpr[RSI], self.gpr[RDI]);
            match m {
                M::Movsb | M::Movsw | M::Movsd | M::Movsq => {
                    let v = mem.read_u(src_base.wrapping_add(si), size)?;
                    mem.write_u(di, size, v)?;
                    self.gpr[RSI] = si.wrapping_add(step);
                    self.gpr[RDI] = di.wrapping_add(step);
                }
                M::Stosb | M::Stosw | M::Stosd | M::Stosq => {
                    mem.write_u(di, size, self.gpr[RAX] & mask(size))?;
                    self.gpr[RDI] = di.wrapping_add(step);
                }
                M::Lodsb | M::Lodsw | M::Lodsd | M::Lodsq => {
                    let v = mem.read_u(src_base.wrapping_add(si), size)?;
                    let r = match size {
                        1 => Register::AL,
                        2 => Register::AX,
                        4 => Register::EAX,
                        _ => Register::RAX,
                    };
                    self.set_reg(r, v)?;
                    self.gpr[RSI] = si.wrapping_add(step);
                }
                M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Cmpsq => {
                    let a = mem.read_u(src_base.wrapping_add(si), size)?;
                    let b = mem.read_u(di, size)?;
                    self.sub_flags(a, b, 0, size);
                    self.gpr[RSI] = si.wrapping_add(step);
                    self.gpr[RDI] = di.wrapping_add(step);
                }
                _ => {
                    let b = mem.read_u(di, size)?;
                    self.sub_flags(self.gpr[RAX] & mask(size), b, 0, size);
                    self.gpr[RDI] = di.wrapping_add(step);
                }
            }
            if !rep {
                break;
            }
            self.gpr[RCX] = self.gpr[RCX].wrapping_sub(1);
            if compares {
                let z = self.flag(ZF);
                if (ins.has_repe_prefix() && !z) || (ins.has_repne_prefix() && z) {
                    break;
                }
            }
        }
        Ok(())
    }
}
