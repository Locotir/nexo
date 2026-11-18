//! The AArch64 side of `nexo bin emu`: the registers x0..x30, sp, pc and NZCV, and the integer
//! instructions that pure functions are made of (arithmetic and logic, bitfields, conditional select,
//! multiply and divide, loads and stores, pairs, literals and branches). It runs the decoder's `Ins`
//! by mnemonic and operand shape, shares the memory, the library stubs, `--stub`, the limits and the
//! answer format with the x86 emulator, runs `svc #0` as the Linux system calls of `sys.rs` (no effect on the
//! host), and stops by name on what it does not run (other `svc`, SIMD and floating point, atomics, system registers other than `tpidr_el0`): it never guesses.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use serde_json::json;

use super::mem::Mem;
use super::report::{body_rows, fault_text, num, num_at};
use super::{
    configure, hook, libc, start_of, sys, value_in, EmuSpec, Machine, Rows, Stop, MAX_SECS, MAX_STEPS, MAX_TRACE,
    RET_MAGIC, STACK_TOP,
};
use crate::a64::{decode, Flow, Ins, Mem as AMem, Opd, Why};
use crate::page::{shell_word, Output};
use crate::{Error, Image};

type Res<T> = std::result::Result<T, Stop>;

/// `tpidr_el0` as the decoder numbers system registers (o0 op1 CRn CRm op2): the thread pointer.
const TPIDR_EL0: u16 = (1 << 14) | (3 << 11) | (13 << 7) | 2;

/// Where a library stub finds its arguments (AAPCS64: x0..x7, then the stack at sp).
#[derive(Clone, Copy)]
pub(super) struct Abi {
    pub(super) x: [u64; 8],
    pub(super) sp: u64,
}

/// The processor.
struct Arm {
    x: [u64; 31],
    sp: u64,
    pc: u64,
    n: bool,
    z: bool,
    c: bool,
    v: bool,
}

/// An instruction whose operands are not a shape this emulator runs (the caller names the instruction).
fn shape() -> Stop {
    Stop::Unsupported(String::new())
}

fn mask(v: u64, x: bool) -> u64 {
    if x {
        v
    } else {
        v & 0xffff_ffff
    }
}

/// The low `w` bits set.
fn low(w: u32) -> u64 {
    if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}

/// Sign-extend the low `w` bits of `v` to 64.
fn sext(v: u64, w: u32) -> u64 {
    if w >= 64 || w == 0 {
        v
    } else {
        let s = 64 - w;
        (((v << s) as i64) >> s) as u64
    }
}

fn is_x(o: &Opd) -> bool {
    matches!(*o, Opd::Gpr { x: true, .. })
}

fn shift_amt(o: &Opd) -> u32 {
    match *o {
        Opd::Shift { amt, .. } => u32::from(amt),
        _ => 0,
    }
}

/// For a single load or store mnemonic: (load, size in bytes or 0 for the register's width, sign-extend).
fn single_kind(mn: &str) -> Option<(bool, u8, bool)> {
    let (load, rest) = match mn.strip_prefix("ld") {
        Some(r) => (true, r),
        None => (false, mn.strip_prefix("st")?),
    };
    let rest = rest.strip_prefix("ur").or_else(|| rest.strip_prefix("tr")).or_else(|| rest.strip_prefix('r'))?;
    let (size, signed) = match rest {
        "" => (0, false),
        "b" => (1, false),
        "h" => (2, false),
        "sb" => (1, true),
        "sh" => (2, true),
        "sw" => (4, true),
        _ => return None,
    };
    Some((load, size, signed))
}

impl Arm {
    fn new() -> Arm {
        Arm { x: [0; 31], sp: 0, pc: 0, n: false, z: false, c: false, v: false }
    }

    /// x0..x30 and sp, in one array (index 31 is sp).
    fn regs(&self) -> [u64; 32] {
        let mut r = [0u64; 32];
        r[..31].copy_from_slice(&self.x);
        r[31] = self.sp;
        r
    }

    fn nzcv(&self) -> u64 {
        (u64::from(self.n) << 3) | (u64::from(self.z) << 2) | (u64::from(self.c) << 1) | u64::from(self.v)
    }

    fn set_nzcv(&mut self, f: u64) {
        self.n = f & 8 != 0;
        self.z = f & 4 != 0;
        self.c = f & 2 != 0;
        self.v = f & 1 != 0;
    }

    /// Register `n`: 31 is the zero register, or sp when `sp`.
    fn reg(&self, n: u8, sp: bool) -> u64 {
        if n < 31 {
            self.x[n as usize]
        } else if sp {
            self.sp
        } else {
            0
        }
    }

    /// The value of a register or immediate operand, at the width the operand names.
    fn get(&self, o: &Opd) -> Res<u64> {
        match *o {
            Opd::Gpr { n, x, sp } => Ok(mask(self.reg(n, sp), x)),
            Opd::Imm(v) | Opd::Dec(v) => Ok(v as u64),
            Opd::UImm(v) | Opd::Target(v) => Ok(v),
            _ => Err(shape()),
        }
    }

    /// Write a register operand: a w register gets the low 32 bits and a zero upper half; xzr takes it and drops it.
    fn put(&mut self, o: &Opd, v: u64) -> Res<()> {
        match *o {
            Opd::Gpr { n, x, sp } => {
                let v = mask(v, x);
                if n < 31 {
                    self.x[n as usize] = v;
                } else if sp {
                    self.sp = v;
                }
                Ok(())
            }
            _ => Err(shape()),
        }
    }

    /// `v` shifted by the operand `sh` (none, or lsl/lsr/asr/ror by an amount) at width x or w.
    fn shifted(&self, v: u64, sh: &Opd, x: bool) -> Res<u64> {
        match *sh {
            Opd::None => Ok(mask(v, x)),
            Opd::Shift { kind, amt } => {
                let size = if x { 64 } else { 32 };
                let amt = u32::from(amt);
                if amt >= size {
                    return Err(shape());
                }
                let v = mask(v, x);
                let r = match kind {
                    0 => v << amt,
                    1 => v >> amt,
                    2 => {
                        if x {
                            ((v as i64) >> amt) as u64
                        } else {
                            u64::from(((v as u32 as i32) >> amt) as u32)
                        }
                    }
                    3 => {
                        if x {
                            v.rotate_right(amt)
                        } else {
                            u64::from((v as u32).rotate_right(amt))
                        }
                    }
                    _ => return Err(shape()),
                };
                Ok(mask(r, x))
            }
            _ => Err(shape()),
        }
    }

    /// The second operand of add/sub: an immediate or a register, shifted or extended.
    fn operand2(&self, o: &Opd, sh: &Opd, x: bool) -> Res<u64> {
        let v = self.get(o)?;
        if let Opd::Ext { kind, amt, .. } = *sh {
            let e = match kind {
                0 => v & 0xff,
                1 => v & 0xffff,
                2 => v & 0xffff_ffff,
                4 => sext(v, 8),
                5 => sext(v, 16),
                6 => sext(v, 32),
                _ => v,
            };
            return Ok(mask(e << u32::from(amt), x));
        }
        self.shifted(v, sh, x)
    }

    /// `a + b + cin` at width x or w (both already cut to it); sets NZCV when `set`.
    fn add(&mut self, a: u64, b: u64, cin: u64, x: bool, set: bool) -> u64 {
        let (res, carry) = if x {
            let (s1, c1) = a.overflowing_add(b);
            let (s2, c2) = s1.overflowing_add(cin);
            (s2, c1 || c2)
        } else {
            let s = a + b + cin;
            (s & 0xffff_ffff, (s >> 32) != 0)
        };
        if set {
            let top = if x { 63 } else { 31 };
            self.n = ((res >> top) & 1) == 1;
            self.z = res == 0;
            self.c = carry;
            self.v = ((((a ^ res) & (b ^ res)) >> top) & 1) == 1;
        }
        res
    }

    /// The N and Z flags of a logical result; C and V clear.
    fn logic_flags(&mut self, res: u64, x: bool) {
        self.n = ((res >> if x { 63 } else { 31 }) & 1) == 1;
        self.z = res == 0;
        self.c = false;
        self.v = false;
    }

    /// Whether condition code `cc` holds.
    fn holds(&self, cc: u8) -> bool {
        let r = match cc >> 1 {
            0 => self.z,
            1 => self.c,
            2 => self.n,
            3 => self.v,
            4 => self.c && !self.z,
            5 => self.n == self.v,
            6 => self.n == self.v && !self.z,
            _ => true,
        };
        if (cc & 1) == 1 && cc != 15 {
            !r
        } else {
            r
        }
    }

    /// The address of a memory operand, and the new value of the base register for pre and post index.
    fn address(&self, m: &AMem) -> Res<(u64, Option<(u8, u64)>)> {
        Ok(match *m {
            AMem::Off { base, off } => (self.reg(base, true).wrapping_add(off as u64), None),
            AMem::Pre { base, off } => {
                let a = self.reg(base, true).wrapping_add(off as u64);
                (a, Some((base, a)))
            }
            AMem::Post { base, off } => {
                let a = self.reg(base, true);
                (a, Some((base, a.wrapping_add(off as u64))))
            }
            AMem::Reg { base, idx, ext, shift } => {
                let r = self.reg(idx, false);
                let e = match ext {
                    2 => r & 0xffff_ffff,
                    6 => sext(r, 32),
                    _ => r,
                };
                (self.reg(base, true).wrapping_add(e << u32::from(shift)), None)
            }
            AMem::PostReg { .. } => return Err(shape()),
        })
    }

    fn set_base(&mut self, wb: Option<(u8, u64)>) {
        if let Some((b, v)) = wb {
            if b < 31 {
                self.x[b as usize] = v;
            } else {
                self.sp = v;
            }
        }
    }

    /// `ldr`, `str` and their byte, half, signed, unscaled and literal forms.
    fn single(&mut self, mem: &mut Mem, ins: &Ins, load: bool, size: u8, signed: bool) -> Res<()> {
        let Opd::Gpr { x: rx, .. } = ins.ops[0] else { return Err(shape()) };
        let (a, wb) = match ins.ops[1] {
            Opd::Mem(m) => self.address(&m)?,
            Opd::Target(t) => (t, None),
            _ => return Err(shape()),
        };
        let sz: usize = if size != 0 {
            usize::from(size)
        } else if rx {
            8
        } else {
            4
        };
        if load {
            let mut v = mem.read_u(a, sz).map_err(Stop::Fault)?;
            if signed {
                v = sext(v, 8 * sz as u32);
            }
            self.put(&ins.ops[0], v)?;
        } else {
            let v = self.get(&ins.ops[0])?;
            mem.write_u(a, sz, v).map_err(Stop::Fault)?;
        }
        self.set_base(wb);
        Ok(())
    }

    /// `ldp`, `stp`, `ldpsw`, `ldnp`, `stnp`.
    fn pair(&mut self, mem: &mut Mem, ins: &Ins, load: bool, sw: bool) -> Res<()> {
        let Opd::Gpr { x: rx, .. } = ins.ops[0] else { return Err(shape()) };
        let Opd::Mem(m) = ins.ops[2] else { return Err(shape()) };
        let sz: usize = if sw || !rx { 4 } else { 8 };
        let (a, wb) = self.address(&m)?;
        let a2 = a.wrapping_add(sz as u64);
        if load {
            let mut v1 = mem.read_u(a, sz).map_err(Stop::Fault)?;
            let mut v2 = mem.read_u(a2, sz).map_err(Stop::Fault)?;
            if sw {
                v1 = sext(v1, 32);
                v2 = sext(v2, 32);
            }
            self.put(&ins.ops[0], v1)?;
            self.put(&ins.ops[1], v2)?;
        } else {
            let v1 = self.get(&ins.ops[0])?;
            let v2 = self.get(&ins.ops[1])?;
            mem.write_u(a, sz, v1).map_err(Stop::Fault)?;
            mem.write_u(a2, sz, v2).map_err(Stop::Fault)?;
        }
        self.set_base(wb);
        Ok(())
    }

    /// Run one instruction; pc is already the next one, and a branch overwrites it.
    fn exec(&mut self, mem: &mut Mem, ins: &Ins) -> Res<()> {
        self.pc = ins.pc.wrapping_add(4);
        match self.step(mem, ins) {
            Err(Stop::Unsupported(d)) if d.is_empty() => Err(Stop::Unsupported(ins.text())),
            r => r,
        }
    }

    fn step(&mut self, mem: &mut Mem, ins: &Ins) -> Res<()> {
        let ops = &ins.ops;
        let mn = ins.mn.as_str();
        let x = is_x(&ops[0]);
        match mn {
            // hints that change nothing a function can see (pointer authentication is not modelled: signing
            // and checking are both skipped, so a signed return address stays valid)
            "nop" | "yield" | "dgh" | "esb" | "csdb" | "xpaclri" | "pacia1716" | "pacib1716" | "autia1716"
            | "autib1716" | "paciaz" | "paciasp" | "pacibz" | "pacibsp" | "autiaz" | "autiasp" | "autibz"
            | "autibsp" | "bti" | "bti c" | "bti j" | "bti jc" | "hint" | "dmb" | "dsb" | "isb" | "clrex" | "prfm"
            | "prfum" => Ok(()),
            "mov" => {
                let v = self.get(&ops[1])?;
                self.put(&ops[0], v)
            }
            "mvn" => {
                let v = self.shifted(self.get(&ops[1])?, &ops[2], x)?;
                self.put(&ops[0], !v)
            }
            "movz" => {
                let v = self.get(&ops[1])? << shift_amt(&ops[2]);
                self.put(&ops[0], v)
            }
            "movn" => {
                let v = self.get(&ops[1])? << shift_amt(&ops[2]);
                self.put(&ops[0], !v)
            }
            "movk" => {
                let sh = shift_amt(&ops[2]);
                let old = self.get(&ops[0])?;
                let v = (old & !(0xffffu64 << sh)) | ((self.get(&ops[1])? & 0xffff) << sh);
                self.put(&ops[0], v)
            }
            "adr" | "adrp" => {
                let v = self.get(&ops[1])?;
                self.put(&ops[0], v)
            }
            "add" | "adds" | "sub" | "subs" | "cmp" | "cmn" | "neg" | "negs" => {
                let sub = matches!(mn, "sub" | "subs" | "cmp" | "neg" | "negs");
                let set = matches!(mn, "adds" | "subs" | "cmp" | "cmn" | "negs");
                let has_d = !matches!(mn, "cmp" | "cmn");
                let neg = matches!(mn, "neg" | "negs");
                let k = usize::from(has_d);
                let (a, bi) = if neg { (0, 1) } else { (self.get(&ops[k])?, k + 1) };
                let b = self.operand2(&ops[bi], &ops[bi + 1], x)?;
                let r = if sub { self.add(a, mask(!b, x), 1, x, set) } else { self.add(a, b, 0, x, set) };
                if has_d {
                    self.put(&ops[0], r)?;
                }
                Ok(())
            }
            "adc" | "adcs" | "sbc" | "sbcs" | "ngc" | "ngcs" => {
                let sub = mn.starts_with("sb") || mn.starts_with("ng");
                let neg = mn.starts_with("ng");
                let (a, bi) = if neg { (0, 1) } else { (self.get(&ops[1])?, 2) };
                let b = self.get(&ops[bi])?;
                let cin = u64::from(self.c);
                let set = mn.ends_with('s');
                let r = if sub { self.add(a, mask(!b, x), cin, x, set) } else { self.add(a, b, cin, x, set) };
                self.put(&ops[0], r)
            }
            "and" | "ands" | "orr" | "eor" | "bic" | "bics" | "orn" | "eon" | "tst" => {
                let has_d = mn != "tst";
                let k = usize::from(has_d);
                let a = self.get(&ops[k])?;
                let mut b = self.shifted(self.get(&ops[k + 1])?, &ops[k + 2], x)?;
                if matches!(mn, "bic" | "bics" | "orn" | "eon") {
                    b = mask(!b, x);
                }
                let r = match mn {
                    "and" | "ands" | "bic" | "bics" | "tst" => a & b,
                    "orr" | "orn" => a | b,
                    _ => a ^ b,
                };
                if matches!(mn, "ands" | "bics" | "tst") {
                    self.logic_flags(r, x);
                }
                if has_d {
                    self.put(&ops[0], r)?;
                }
                Ok(())
            }
            "lsl" | "lsr" | "asr" | "ror" => {
                let v = self.get(&ops[1])?;
                let size: u64 = if x { 64 } else { 32 };
                let amt = match ops[2] {
                    Opd::Gpr { .. } => self.get(&ops[2])? % size,
                    Opd::Dec(n) => (n as u64) % size,
                    _ => return Err(shape()),
                };
                let kind = match mn {
                    "lsl" => 0,
                    "lsr" => 1,
                    "asr" => 2,
                    _ => 3,
                };
                let r = self.shifted(v, &Opd::Shift { kind, amt: amt as u8 }, x)?;
                self.put(&ops[0], r)
            }
            "ubfx" | "sbfx" => {
                let lsb = self.get(&ops[2])? as u32;
                let w = self.get(&ops[3])? as u32;
                let mut v = (self.get(&ops[1])? >> lsb) & low(w);
                if mn == "sbfx" {
                    v = sext(v, w);
                }
                self.put(&ops[0], v)
            }
            "ubfiz" | "sbfiz" => {
                let lsb = self.get(&ops[2])? as u32;
                let w = self.get(&ops[3])? as u32;
                let mut v = self.get(&ops[1])? & low(w);
                if mn == "sbfiz" {
                    v = sext(v, w);
                }
                self.put(&ops[0], v << lsb)
            }
            "bfi" => {
                let lsb = self.get(&ops[2])? as u32;
                let m = low(self.get(&ops[3])? as u32) << lsb;
                let v = (self.get(&ops[0])? & !m) | ((self.get(&ops[1])? << lsb) & m);
                self.put(&ops[0], v)
            }
            "bfxil" => {
                let lsb = self.get(&ops[2])? as u32;
                let m = low(self.get(&ops[3])? as u32);
                let v = (self.get(&ops[0])? & !m) | ((self.get(&ops[1])? >> lsb) & m);
                self.put(&ops[0], v)
            }
            "bfc" => {
                let lsb = self.get(&ops[1])? as u32;
                let m = low(self.get(&ops[2])? as u32) << lsb;
                let v = self.get(&ops[0])? & !m;
                self.put(&ops[0], v)
            }
            "sxtb" | "sxth" | "sxtw" => {
                let w = match mn {
                    "sxtb" => 8,
                    "sxth" => 16,
                    _ => 32,
                };
                let v = sext(self.get(&ops[1])?, w);
                self.put(&ops[0], v)
            }
            "uxtb" | "uxth" => {
                let v = self.get(&ops[1])? & if mn == "uxtb" { 0xff } else { 0xffff };
                self.put(&ops[0], v)
            }
            "extr" => {
                let (hi, lo) = (self.get(&ops[1])?, self.get(&ops[2])?);
                let lsb = self.get(&ops[3])? as u32;
                let size = if x { 64 } else { 32 };
                let v = if lsb == 0 { lo } else { (lo >> lsb) | (hi << (size - lsb)) };
                self.put(&ops[0], v)
            }
            "csel" | "csinc" | "csinv" | "csneg" => {
                let Opd::Cond(c) = ops[3] else { return Err(shape()) };
                let v = if self.holds(c) {
                    self.get(&ops[1])?
                } else {
                    let m = self.get(&ops[2])?;
                    match mn {
                        "csel" => m,
                        "csinc" => m.wrapping_add(1),
                        "csinv" => !m,
                        _ => m.wrapping_neg(),
                    }
                };
                self.put(&ops[0], v)
            }
            "cset" | "csetm" => {
                let Opd::Cond(c) = ops[1] else { return Err(shape()) };
                let v = if !self.holds(c) {
                    0
                } else if mn == "csetm" {
                    u64::MAX
                } else {
                    1
                };
                self.put(&ops[0], v)
            }
            "cinc" | "cinv" | "cneg" => {
                let Opd::Cond(c) = ops[2] else { return Err(shape()) };
                let n = self.get(&ops[1])?;
                let v = if !self.holds(c) {
                    n
                } else {
                    match mn {
                        "cinc" => n.wrapping_add(1),
                        "cinv" => !n,
                        _ => n.wrapping_neg(),
                    }
                };
                self.put(&ops[0], v)
            }
            "ccmp" | "ccmn" => {
                let Opd::Cond(c) = ops[3] else { return Err(shape()) };
                let Opd::Dec(f) = ops[2] else { return Err(shape()) };
                if self.holds(c) {
                    let a = self.get(&ops[0])?;
                    let b = mask(self.get(&ops[1])?, x);
                    if mn == "ccmp" {
                        self.add(a, mask(!b, x), 1, x, true);
                    } else {
                        self.add(a, b, 0, x, true);
                    }
                } else {
                    self.set_nzcv(f as u64);
                }
                Ok(())
            }
            "mul" | "mneg" | "madd" | "msub" => {
                let p = self.get(&ops[1])?.wrapping_mul(self.get(&ops[2])?);
                let v = match mn {
                    "mul" => p,
                    "mneg" => p.wrapping_neg(),
                    "madd" => self.get(&ops[3])?.wrapping_add(p),
                    _ => self.get(&ops[3])?.wrapping_sub(p),
                };
                self.put(&ops[0], v)
            }
            "smull" | "smnegl" | "smaddl" | "smsubl" | "umull" | "umnegl" | "umaddl" | "umsubl" => {
                let (a, b) = (self.get(&ops[1])?, self.get(&ops[2])?);
                let p = if mn.starts_with('s') {
                    (sext(a, 32) as i64).wrapping_mul(sext(b, 32) as i64) as u64
                } else {
                    a.wrapping_mul(b)
                };
                let acc = if matches!(ops[3], Opd::None) { 0 } else { self.get(&ops[3])? };
                let v = if mn.ends_with("negl") || mn.ends_with("subl") {
                    acc.wrapping_sub(p)
                } else {
                    acc.wrapping_add(p)
                };
                self.put(&ops[0], v)
            }
            "smulh" | "umulh" => {
                let (a, b) = (self.get(&ops[1])?, self.get(&ops[2])?);
                let hi = if mn == "smulh" {
                    ((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64
                } else {
                    ((u128::from(a) * u128::from(b)) >> 64) as u64
                };
                self.put(&ops[0], hi)
            }
            "udiv" | "sdiv" => {
                let (a, b) = (self.get(&ops[1])?, self.get(&ops[2])?);
                let v = if b == 0 {
                    0
                } else if mn == "udiv" {
                    a / b
                } else if x {
                    (a as i64).wrapping_div(b as i64) as u64
                } else {
                    u64::from((a as u32 as i32).wrapping_div(b as u32 as i32) as u32)
                };
                self.put(&ops[0], v)
            }
            "clz" | "cls" | "rbit" | "rev" | "rev16" | "rev32" => {
                let a = self.get(&ops[1])?;
                let v = match mn {
                    "clz" => u64::from(if x { a.leading_zeros() } else { (a as u32).leading_zeros() }),
                    "cls" => {
                        let size = if x { 64 } else { 32 };
                        let msb = (a >> (size - 1)) & 1;
                        let mut n = 0;
                        for i in (0..size - 1).rev() {
                            if (a >> i) & 1 != msb {
                                break;
                            }
                            n += 1;
                        }
                        n
                    }
                    "rbit" => {
                        if x {
                            a.reverse_bits()
                        } else {
                            u64::from((a as u32).reverse_bits())
                        }
                    }
                    "rev" => {
                        if x {
                            a.swap_bytes()
                        } else {
                            u64::from((a as u32).swap_bytes())
                        }
                    }
                    "rev16" => ((a & 0x00ff_00ff_00ff_00ff) << 8) | ((a >> 8) & 0x00ff_00ff_00ff_00ff),
                    _ => u64::from((a as u32).swap_bytes()) | (u64::from(((a >> 32) as u32).swap_bytes()) << 32),
                };
                self.put(&ops[0], v)
            }
            "b" => {
                let t = ins.target.ok_or_else(shape)?;
                if ins.cc == 0xff || self.holds(ins.cc) {
                    self.pc = t;
                }
                Ok(())
            }
            "bl" => {
                self.x[30] = ins.pc.wrapping_add(4);
                self.pc = ins.target.ok_or_else(shape)?;
                Ok(())
            }
            "br" => {
                self.pc = self.get(&ops[0])?;
                Ok(())
            }
            "blr" => {
                let t = self.get(&ops[0])?;
                self.x[30] = ins.pc.wrapping_add(4);
                self.pc = t;
                Ok(())
            }
            "ret" => {
                self.pc = if matches!(ops[0], Opd::None) { self.x[30] } else { self.get(&ops[0])? };
                Ok(())
            }
            "cbz" | "cbnz" => {
                let zero = self.get(&ops[0])? == 0;
                if zero == (mn == "cbz") {
                    self.pc = ins.target.ok_or_else(shape)?;
                }
                Ok(())
            }
            "tbz" | "tbnz" => {
                let v = self.get(&ops[0])?;
                let bit = self.get(&ops[1])? as u32;
                let set = ((v >> bit) & 1) == 1;
                if set == (mn == "tbnz") {
                    self.pc = ins.target.ok_or_else(shape)?;
                }
                Ok(())
            }
            "mrs" => match ops[1] {
                Opd::Sys(TPIDR_EL0) => self.put(&ops[0], 0),
                _ => Err(shape()),
            },
            "brk" => Err(Stop::Trap("breakpoint instruction (brk)")),
            "hlt" => Err(Stop::Trap("halt instruction (hlt)")),
            "udf" => Err(Stop::Trap("undefined instruction (udf)")),
            "ldp" | "ldnp" => self.pair(mem, ins, true, false),
            "stp" | "stnp" => self.pair(mem, ins, false, false),
            "ldpsw" => self.pair(mem, ins, true, true),
            _ => match single_kind(mn) {
                Some((load, size, signed)) => self.single(mem, ins, load, size, signed),
                None => Err(shape()),
            },
        }
    }
}

impl Machine<'_> {
    /// Run the stub of import `i` for a call made with the AArch64 registers in `c`: the result goes in x0
    /// and the run goes on at the return address in x30.
    fn call_import_a64(&mut self, c: &mut Arm, i: usize) -> Res<()> {
        let name = self.imports[i].clone();
        let site = c.x[30].wrapping_sub(4);
        let mut x = [0u64; 8];
        x.copy_from_slice(&c.x[..8]);
        self.a64 = Some(Abi { x, sp: c.sp });
        let (x0, text) = if let Some(&v) = self.overrides.get(&name) {
            (v, format!("{name}(...) = {v:#x} (--stub)"))
        } else {
            match libc::call(self, &name) {
                Some(Ok((v, args, shown))) => {
                    let shown = shown.unwrap_or_else(|| format!("{v:#x}"));
                    (v, format!("{name}({args}) = {shown}"))
                }
                Some(Err(stop)) => {
                    self.log_call(site, format!("{name}(...) stopped the run"));
                    return Err(stop);
                }
                None => return Err(Stop::UnknownImport { name, site }),
            }
        };
        self.log_call(site, text);
        c.x[0] = x0;
        c.pc = c.x[30];
        Ok(())
    }
}

/// `x0`..`x30` (also `fp`, `lr`) as an index.
fn reg_index(name: &str) -> Option<usize> {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "fp" => Some(29),
        "lr" => Some(30),
        _ => n.strip_prefix('x').and_then(|d| d.parse::<usize>().ok()).filter(|&k| k < 31),
    }
}

/// What the run left behind, for the answer.
struct Done {
    stop: Stop,
    start: u64,
    steps: u64,
    t0: Instant,
    initial: [u64; 32],
    args: Vec<String>,
    trace: VecDeque<(Ins, Vec<(usize, u64)>)>,
}

/// How `--hook` names the registers here: x0..x30 and sp.
const A64_REGS: hook::Regs =
    hook::Regs { find: reg_index, name: reg_name, pc: "pc", range: "x0..x30", sp: 31, count: 32 };

impl Arm {
    /// `svc #0`: a Linux system call. The number is in x8, the arguments in x0..x5 and the answer goes in x0
    /// (a negative errno on error). Any other `svc` immediate is not a Linux system call and stops the run.
    fn svc(&mut self, m: &mut Machine, ins: &Ins) -> Res<()> {
        if !matches!(ins.ops[0], Opd::Imm(0) | Opd::UImm(0)) {
            return Err(Stop::Unsupported(ins.text()));
        }
        self.pc = ins.pc.wrapping_add(4);
        let a = [self.x[0], self.x[1], self.x[2], self.x[3], self.x[4], self.x[5]];
        self.x[0] = m.syscall_a64(self.x[8], a, ins.pc)?;
        Ok(())
    }
}

fn reg_name(r: usize) -> String {
    if r == 31 {
        "sp".to_string()
    } else {
        format!("x{r}")
    }
}

/// Run the function or stretch of AArch64 code `spec` names and describe what happened.
pub(super) fn emulate(img: &Image, spec: &EmuSpec) -> crate::Result<Output> {
    if spec.abi.is_some() {
        return Err(Error::Usage("--abi does not apply to arm64: it always uses AAPCS64 (x0..x7)".into()));
    }
    let t0 = Instant::now();
    let start = start_of(img, &spec.target)?;
    let mut m = Machine::new(img, false)?;
    configure(img, spec, &mut m)?;
    let mut c = Arm::new();
    let mut stack_args = Vec::new();
    let mut shown = Vec::new();
    for (i, s) in spec.args.iter().enumerate() {
        let (v, what) = value_in(&mut m, s, format!("arg{}", i + 1))?;
        let at = if i < 8 {
            c.x[i] = v;
            format!("x{i}")
        } else {
            stack_args.push(v);
            format!("stack arg {}", stack_args.len())
        };
        shown.push(match what {
            Some(w) => format!("{at}={v:#x} ({w})"),
            None => format!("{at}={}", num_at(img, v)),
        });
    }
    for s in &spec.regs {
        let (n, v) = s
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--reg {s:?}: write NAME=VALUE, like --reg x19=5")))?;
        let r = reg_index(n)
            .ok_or_else(|| Error::Usage(format!("--reg {s:?}: {n} is not a 64-bit general register (x0..x30)")))?;
        let (v, what) = value_in(&mut m, v, format!("x{r}"))?;
        c.x[r] = v;
        shown.push(match what {
            Some(w) => format!("x{r}={v:#x} ({w})"),
            None => format!("x{r}={}", num_at(img, v)),
        });
    }
    for s in &spec.hooks {
        let (a, act) = hook::parse(img, &mut m, s, &A64_REGS)?;
        m.hooks.at.entry(a).or_default().push(act);
    }
    // the stack: arguments past x7 at sp, and the return address in x30
    let mut sp = STACK_TOP - 0x4000;
    sp -= 8 * stack_args.len() as u64;
    sp &= !15;
    for (k, v) in stack_args.iter().enumerate() {
        let _ = m.mem.load(sp + 8 * k as u64, &v.to_le_bytes());
    }
    c.sp = sp;
    c.x[30] = RET_MAGIC;
    c.pc = start;
    m.rsp0 = sp;
    m.a64 = Some(Abi { x: [0; 8], sp });
    let initial = c.regs();

    let budget = spec.steps.clamp(1, MAX_STEPS);
    let limit = std::time::Duration::from_secs(spec.secs.clamp(1, MAX_SECS));
    let trace_n = spec.trace.min(MAX_TRACE);
    let mut trace: VecDeque<(Ins, Vec<(usize, u64)>)> = VecDeque::with_capacity(trace_n + 1);
    let mut cache: HashMap<u64, Ins> = HashMap::new();
    let mut steps: u64 = 0;
    let mut next_clock: u64 = 0;
    let stop = loop {
        let pc = c.pc;
        if pc == RET_MAGIC {
            break Stop::Returned;
        }
        if Some(pc) == spec.to && steps > 0 {
            break Stop::Reached(pc);
        }
        if !m.hooks.at.is_empty() && m.hooks.at.contains_key(&pc) {
            let mut r = c.regs();
            m.run_hooks(pc, &mut r, &A64_REGS);
            c.x.copy_from_slice(&r[..31]);
            c.sp = r[31];
        }
        if !m.hooks.breaks.is_empty() && m.hooks.breaks.contains(&pc) {
            break Stop::Break(pc);
        }
        if let Some(i) = m.import_at(pc) {
            match m.call_import_a64(&mut c, i) {
                Ok(()) => continue,
                Err(s) => break s,
            }
        }
        if steps >= budget {
            break Stop::Steps;
        }
        if steps >= next_clock {
            next_clock = steps + 4096;
            if t0.elapsed() > limit {
                break Stop::Time;
            }
        }
        if pc & 3 != 0 {
            break Stop::Abort(format!("the program counter {pc:#x} is not a multiple of 4"));
        }
        if m.mem.code_written {
            cache.clear();
            m.mem.code_written = false;
        }
        let ins = match cache.get(&pc) {
            Some(i) => *i,
            None => {
                let mut buf = [0u8; 15];
                let n = match m.mem.fetch(pc, &mut buf) {
                    Ok(n) => n,
                    Err(f) => break Stop::Fault(f),
                };
                if n < 4 {
                    break Stop::Undecodable;
                }
                let ins = decode(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), pc);
                cache.insert(pc, ins);
                ins
            }
        };
        if let Some(why) = ins.undecoded {
            break match why {
                Why::Reserved => Stop::Undecodable,
                other => Stop::Unsupported(format!("{} ({})", ins.text(), other.word())),
            };
        }
        if matches!(ins.flow, Flow::Call | Flow::CallReg) {
            m.last_call = pc;
        }
        let before = c.regs();
        let done = if ins.mn.as_str() == "svc" { c.svc(&mut m, &ins) } else { c.exec(&mut m.mem, &ins) };
        steps += 1;
        if trace_n > 0 {
            let now = c.regs();
            let changed = (0..31).filter(|&r| now[r] != before[r]).map(|r| (r, now[r])).collect();
            if trace.len() == trace_n {
                trace.pop_front();
            }
            trace.push_back((ins, changed));
        }
        if let Err(e) = done {
            c.pc = pc;
            break e;
        }
    };
    let d = Done { stop, start, steps, t0, initial, args: shown, trace };
    Ok(report(img, spec, &m, &c, &d))
}

fn report(img: &Image, spec: &EmuSpec, m: &Machine, c: &Arm, d: &Done) -> Output {
    let at = |a: u64| match img.label(a) {
        Some(l) => format!("{a:#x} {l}"),
        None => format!("{a:#x}"),
    };
    let pc = c.pc;
    let cur = || {
        let mut buf = [0u8; 15];
        match m.mem.fetch(pc, &mut buf) {
            Ok(n) if n >= 4 => {
                format!("`{}`", decode(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]), pc).text())
            }
            _ => "(no code there)".to_string(),
        }
    };
    let words = || {
        let mut w = vec![shell_word(&spec.target)];
        w.extend(spec.args.iter().map(|a| shell_word(a)));
        w.join(" ")
    };
    let rerun = |extra: &str| format!("{} {extra}", crate::ops::command(img, "emu", &words()));
    let steps = d.steps;
    let stop_text = match &d.stop {
        Stop::Returned => "returned to the caller".to_string(),
        Stop::Reached(a) => format!("reached --to {}", at(*a)),
        Stop::Steps => format!(
            "step cap ({steps} steps) at {}: a loop that does not end, or a long one (raise it: {})",
            at(pc),
            rerun(&format!("--steps {}", (spec.steps.saturating_mul(10)).min(MAX_STEPS)))
        ),
        Stop::Time => format!("time cap ({} s) at {} after {steps} steps", spec.secs, at(pc)),
        Stop::UnknownImport { name, site } => format!(
            "call to import `{name}` (no stub) from {}; give it a return value: {}",
            at(*site),
            rerun(&format!("--stub {name}=0"))
        ),
        Stop::Unsupported(t) => {
            format!("unsupported on arm64: {t} at {} (stopped before it; nothing was skipped)", at(pc))
        }
        Stop::Undecodable => {
            format!("a word that does not decode as an instruction at {}: {}", at(pc), cur())
        }
        Stop::Fault(f) => fault_text(m, f, &at(pc), &cur()),
        Stop::Trap(t) => format!("{t} at {}: the program traps here", at(pc)),
        Stop::Break(a) => format!("reached --break {}", at(*a)),
        Stop::Syscall(n) => format!(
            "system call {n} ({}) at {}: emu does not emulate it (it does read, write, exit, exit_group, brk, mmap, munmap, mprotect, getpid, gettid, clock_gettime, gettimeofday, getrandom)",
            sys::name_of_a64(*n),
            at(pc)
        ),
        Stop::Exit(code) => format!("the program called exit({code})"),
        Stop::Abort(why) => why.clone(),
        Stop::LibFault { call, fault } => format!(
            "{call}: {} {:#x} ({}), {}",
            fault.access.word(),
            fault.addr,
            m.place(fault.addr),
            if fault.mapped { "not allowed there" } else { "nothing mapped there" }
        ),
        other => format!("stopped: {}", other.kind()),
    };
    let mut rows = Rows(Vec::new());
    rows.push(format!(
        "emu {} from {}  arch arm64  abi aapcs64  {steps} steps  {:.1} ms",
        img.path.display(),
        at(d.start),
        d.t0.elapsed().as_secs_f64() * 1000.0
    ));
    if !d.args.is_empty() {
        rows.push(format!("in        {}", d.args.join("  ")));
    }
    rows.push(format!("stop      {stop_text}"));
    let returned = d.stop == Stop::Returned;
    let x0 = c.x[0];
    if returned {
        let w0 = x0 & 0xffff_ffff;
        let signed = w0 as u32 as i32;
        let w = if signed < 0 { format!("{w0:#x} (as int {signed})") } else { num(w0) };
        rows.push(format!("return    x0={} w0={w}", num(x0)));
    } else {
        rows.push(format!("at        {} {}", at(pc), cur()));
    }
    let now = c.regs();
    let changed: Vec<String> = (0..31)
        .filter(|&r| now[r] != d.initial[r] || !returned)
        .filter(|&r| now[r] != 0 || d.initial[r] != 0)
        .map(|r| format!("x{r}={:#x}", now[r]))
        .collect();
    if !changed.is_empty() {
        let head = if returned { "regs out " } else { "regs     " };
        rows.push(format!("{head} {}", changed.join(" ")));
    }
    if matches!(d.stop, Stop::Break(_)) {
        let names: Vec<&str> =
            [(c.n, "N"), (c.z, "Z"), (c.c, "C"), (c.v, "V")].iter().filter(|(set, _)| *set).map(|(_, n)| *n).collect();
        rows.push(format!(
            "flags     nzcv={:#x} {}",
            c.nzcv(),
            if names.is_empty() { "(none set)".to_string() } else { names.join(" ") }
        ));
        rows.push(format!("stack top sp={:#x} (8 qwords)", c.sp));
        for k in 0..8u64 {
            match m.mem.read_u(c.sp.wrapping_add(8 * k), 8) {
                Ok(v) => rows.push(format!("  sp+{:#04x}  {v:#018x}", 8 * k)),
                Err(_) => {
                    rows.push(format!("  sp+{:#04x}  (not mapped)", 8 * k));
                    break;
                }
            }
        }
    }
    body_rows(img, spec, m, &mut rows);
    if !d.trace.is_empty() {
        rows.push(format!("trace     last {} of {steps} steps", d.trace.len()));
        for (ins, ch) in &d.trace {
            let mut line = format!("  {}  {}", at(ins.pc), ins.text());
            if !ch.is_empty() {
                let regs: Vec<String> = ch.iter().map(|(r, v)| format!("{}={v:#x}", reg_name(*r))).collect();
                line.push_str(&format!("  -> {}", regs.join(" ")));
            }
            rows.push(line);
        }
    }
    let exit_code = match d.stop {
        Stop::Exit(code) => Some(code),
        _ => None,
    };
    let mut regs = serde_json::Map::new();
    for (r, v) in now.iter().enumerate() {
        regs.insert(reg_name(r), json!(v));
    }
    let meta = json!({
        "file": img.path.display().to_string(),
        "arch": "arm64",
        "start": d.start,
        "abi": "aapcs64",
        "steps": steps,
        "stop": {"kind": d.stop.kind(), "text": stop_text, "pc": pc, "place": img.label(pc)},
        "return": returned.then_some(x0),
        "exit": exit_code,
        "nzcv": c.nzcv(),
        "calls_total": m.calls_total,
        "calls_not_listed": m.unlogged,
        "hooks_total": m.hooks.total,
        "hooks": m.hooks.log.iter().map(|h| json!({"addr": h.addr, "lines": h.lines})).collect::<Vec<_>>(),
        "regs": regs,
        "stdout": String::from_utf8_lossy(&m.stdout),
        "stderr": String::from_utf8_lossy(&m.stderr),
        "calls": m.calls.iter().map(|k| json!({"site": k.site, "call": k.text})).collect::<Vec<_>>(),
    });
    let rows = rows.0;
    Output { op: "emu", meta, rows, tail: 0, command: crate::ops::command(img, "emu", &words()) }
}
