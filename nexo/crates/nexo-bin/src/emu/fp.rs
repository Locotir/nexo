//! Floating point in the vector unit: scalar and packed single and double arithmetic, square roots, min and
//! max, compares, and the conversions between integers and floats, in their SSE and VEX (AVX) forms. The
//! arithmetic is IEEE as the host does it. Rounding is the default (nearest even) and the exception flags
//! are not modelled. An instruction this file does not know goes on to the AVX integer forms (vex.rs) and
//! from there stops the run by name.

use iced_x86::{EncodingKind, Instruction, Mnemonic as M, OpKind, Register};

use super::cpu::{Cpu, Event};
use super::mem::Mem;

type R<T> = Result<T, Event>;

/// A vector register as a whole: the low 128 bits (xmm) and the high 128 bits of its ymm.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct V {
    pub lo: u128,
    pub hi: u128,
}

fn bad(what: impl Into<String>) -> Event {
    Event::Unsupported(what.into())
}

pub(super) fn reg_index(r: Register) -> R<usize> {
    let n = r.number();
    if n >= 16 {
        return Err(bad(format!("register {r:?} (AVX-512)").to_lowercase()));
    }
    Ok(n)
}

/// Operand `op` as a whole value: an xmm or ymm register, or memory of the operand's size (up to 32 bytes),
/// zero-extended.
pub(super) fn rd(cpu: &Cpu, mem: &Mem, ins: &Instruction, op: u32) -> R<V> {
    match ins.op_kind(op) {
        OpKind::Register => {
            let r = ins.op_register(op);
            if r.is_xmm() {
                Ok(V { lo: cpu.xmm[reg_index(r)?], hi: 0 })
            } else if r.is_ymm() {
                let i = reg_index(r)?;
                Ok(V { lo: cpu.xmm[i], hi: cpu.ymmh[i] })
            } else {
                Err(bad(format!("register {r:?}").to_lowercase()))
            }
        }
        OpKind::Memory => {
            let n = ins.memory_size().size();
            if n == 0 || n > 32 {
                return Err(bad(format!("a {n}-byte vector memory operand")));
            }
            let mut b = [0u8; 32];
            mem.read(cpu.addr(ins, op), &mut b[..n])?;
            Ok(V {
                lo: u128::from_le_bytes(b[..16].try_into().unwrap_or([0; 16])),
                hi: u128::from_le_bytes(b[16..].try_into().unwrap_or([0; 16])),
            })
        }
        k => Err(bad(format!("operand kind {k:?}"))),
    }
}

/// Write `v` to operand `op`: all of an xmm or ymm register (a VEX write to an xmm register clears the top
/// half of its ymm; the old SSE forms leave it), or as many bytes as the memory operand has.
pub(super) fn wr(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction, op: u32, v: V, vex: bool) -> R<()> {
    match ins.op_kind(op) {
        OpKind::Register => {
            let r = ins.op_register(op);
            if r.is_xmm() {
                let i = reg_index(r)?;
                cpu.xmm[i] = v.lo;
                if vex {
                    cpu.ymmh[i] = 0;
                }
                Ok(())
            } else if r.is_ymm() {
                let i = reg_index(r)?;
                cpu.xmm[i] = v.lo;
                cpu.ymmh[i] = v.hi;
                Ok(())
            } else {
                Err(bad(format!("write to register {r:?}").to_lowercase()))
            }
        }
        OpKind::Memory => {
            let n = ins.memory_size().size();
            if n == 0 || n > 32 {
                return Err(bad(format!("a {n}-byte vector memory operand")));
            }
            let mut b = [0u8; 32];
            b[..16].copy_from_slice(&v.lo.to_le_bytes());
            b[16..].copy_from_slice(&v.hi.to_le_bytes());
            Ok(mem.write(cpu.addr(ins, op), &b[..n])?)
        }
        k => Err(bad(format!("write to operand kind {k:?}"))),
    }
}

/// Bytes the destination register holds: 16 for xmm, 32 for ymm.
pub(super) fn reg_bytes(ins: &Instruction, op: u32) -> usize {
    if ins.op_kind(op) == OpKind::Register && ins.op_register(op).is_ymm() {
        32
    } else {
        16
    }
}

pub(super) fn g32(v: V, i: usize) -> u32 {
    let h = if i < 4 { v.lo } else { v.hi };
    (h >> ((i % 4) * 32)) as u32
}

pub(super) fn s32(v: &mut V, i: usize, x: u32) {
    let sh = (i % 4) * 32;
    let h = if i < 4 { &mut v.lo } else { &mut v.hi };
    *h = (*h & !(0xffff_ffffu128 << sh)) | ((x as u128) << sh);
}

pub(super) fn g64(v: V, i: usize) -> u64 {
    let h = if i < 2 { v.lo } else { v.hi };
    (h >> ((i % 2) * 64)) as u64
}

pub(super) fn s64(v: &mut V, i: usize, x: u64) {
    let sh = (i % 2) * 64;
    let h = if i < 2 { &mut v.lo } else { &mut v.hi };
    *h = (*h & !((u64::MAX as u128) << sh)) | ((x as u128) << sh);
}

/// Lane `i` as a double: a single is widened (exactly).
fn fl(v: V, i: usize, dbl: bool) -> f64 {
    if dbl {
        f64::from_bits(g64(v, i))
    } else {
        f32::from_bits(g32(v, i)) as f64
    }
}

/// Store a double into lane `i`, narrowed to a single when `dbl` is false.
fn put_fl(v: &mut V, i: usize, dbl: bool, x: f64) {
    if dbl {
        s64(v, i, x.to_bits());
    } else {
        s32(v, i, (x as f32).to_bits());
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
}

/// (operation, double?, scalar?) for the arithmetic mnemonics.
fn arith(mn: M) -> Option<(Op, bool, bool)> {
    use Op::*;
    Some(match mn {
        M::Addss | M::Vaddss => (Add, false, true),
        M::Addsd | M::Vaddsd => (Add, true, true),
        M::Addps | M::Vaddps => (Add, false, false),
        M::Addpd | M::Vaddpd => (Add, true, false),
        M::Subss | M::Vsubss => (Sub, false, true),
        M::Subsd | M::Vsubsd => (Sub, true, true),
        M::Subps | M::Vsubps => (Sub, false, false),
        M::Subpd | M::Vsubpd => (Sub, true, false),
        M::Mulss | M::Vmulss => (Mul, false, true),
        M::Mulsd | M::Vmulsd => (Mul, true, true),
        M::Mulps | M::Vmulps => (Mul, false, false),
        M::Mulpd | M::Vmulpd => (Mul, true, false),
        M::Divss | M::Vdivss => (Div, false, true),
        M::Divsd | M::Vdivsd => (Div, true, true),
        M::Divps | M::Vdivps => (Div, false, false),
        M::Divpd | M::Vdivpd => (Div, true, false),
        M::Minss | M::Vminss => (Min, false, true),
        M::Minsd | M::Vminsd => (Min, true, true),
        M::Minps | M::Vminps => (Min, false, false),
        M::Minpd | M::Vminpd => (Min, true, false),
        M::Maxss | M::Vmaxss => (Max, false, true),
        M::Maxsd | M::Vmaxsd => (Max, true, true),
        M::Maxps | M::Vmaxps => (Max, false, false),
        M::Maxpd | M::Vmaxpd => (Max, true, false),
        _ => return None,
    })
}

/// A single or double operation done in double precision gives the correctly rounded single result for
/// + - * / and sqrt (a double holds more than twice the digits of a single plus two), so one code path serves both.
fn apply(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => a / b,
        // x86 returns the second operand when either is a NaN or both are zeros
        Op::Min => {
            if a < b {
                a
            } else {
                b
            }
        }
        Op::Max => {
            if a > b {
                a
            } else {
                b
            }
        }
    }
}

/// For a scalar instruction: (operand holding the untouched upper lanes, operand holding the source).
fn scalar_ops(ins: &Instruction) -> (u32, u32) {
    if ins.op_count() == 3 {
        (1, 2)
    } else {
        (0, 1)
    }
}

const CF: u64 = 1;
const PF: u64 = 4;
const AF: u64 = 0x10;
const ZF: u64 = 0x40;
const SF: u64 = 0x80;
const OF: u64 = 0x800;

/// The integer an x86 conversion gives, or the "integer indefinite" (the lowest value) when it does not fit.
fn to_int(x: f64, trunc: bool, wide: bool) -> u64 {
    let t = if trunc { x.trunc() } else { x.round_ties_even() };
    if wide {
        if t.is_nan() || !(-(2f64.powi(63))..2f64.powi(63)).contains(&t) {
            return 1u64 << 63;
        }
        t as i64 as u64
    } else {
        if t.is_nan() || !(-2147483648.0..2147483648.0).contains(&t) {
            return 1u64 << 31;
        }
        t as i32 as u32 as u64
    }
}

/// Run one floating point instruction; `Ok(false)` when it is not one this unit knows.
pub(super) fn exec(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction) -> R<bool> {
    let mn = ins.mnemonic();
    let vex = ins.encoding() == EncodingKind::VEX;
    if let Some((op, dbl, scalar)) = arith(mn) {
        let n = ins.op_count();
        let (a, b) = if n == 3 {
            (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?)
        } else {
            (rd(cpu, mem, ins, 0)?, rd(cpu, mem, ins, 1)?)
        };
        let mut r = if scalar { a } else { V::default() };
        let lanes = if scalar { 1 } else { reg_bytes(ins, 0) / if dbl { 8 } else { 4 } };
        for i in 0..lanes {
            put_fl(&mut r, i, dbl, apply(op, fl(a, i, dbl), fl(b, i, dbl)));
        }
        wr(cpu, mem, ins, 0, r, vex)?;
        return Ok(true);
    }
    match mn {
        M::Sqrtss | M::Vsqrtss | M::Sqrtsd | M::Vsqrtsd | M::Sqrtps | M::Vsqrtps | M::Sqrtpd | M::Vsqrtpd => {
            let dbl = matches!(mn, M::Sqrtsd | M::Vsqrtsd | M::Sqrtpd | M::Vsqrtpd);
            let scalar = matches!(mn, M::Sqrtss | M::Vsqrtss | M::Sqrtsd | M::Vsqrtsd);
            let (base, src) = if scalar { scalar_ops(ins) } else { (1, 1) };
            let s = rd(cpu, mem, ins, src)?;
            let mut r = if scalar { rd(cpu, mem, ins, base)? } else { V::default() };
            let lanes = if scalar { 1 } else { reg_bytes(ins, 0) / if dbl { 8 } else { 4 } };
            for i in 0..lanes {
                put_fl(&mut r, i, dbl, fl(s, i, dbl).sqrt());
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        M::Comiss | M::Vcomiss | M::Ucomiss | M::Vucomiss | M::Comisd | M::Vcomisd | M::Ucomisd | M::Vucomisd => {
            let dbl = matches!(mn, M::Comisd | M::Vcomisd | M::Ucomisd | M::Vucomisd);
            let (a, b) = (fl(rd(cpu, mem, ins, 0)?, 0, dbl), fl(rd(cpu, mem, ins, 1)?, 0, dbl));
            let f = if a.is_nan() || b.is_nan() {
                ZF | PF | CF
            } else if a > b {
                0
            } else if a < b {
                CF
            } else {
                ZF
            };
            cpu.flags = (cpu.flags & !(CF | PF | AF | ZF | SF | OF)) | f;
        }
        M::Cmpss | M::Vcmpss | M::Cmpsd | M::Vcmpsd | M::Cmpps | M::Vcmpps | M::Cmppd | M::Vcmppd => {
            let dbl = matches!(mn, M::Cmpsd | M::Vcmpsd | M::Cmppd | M::Vcmppd);
            let scalar = matches!(mn, M::Cmpss | M::Vcmpss | M::Cmpsd | M::Vcmpsd);
            let n = ins.op_count();
            let (a, b) = if n == 4 {
                (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?)
            } else {
                (rd(cpu, mem, ins, 0)?, rd(cpu, mem, ins, 1)?)
            };
            let pred = ins.immediate(n - 1) & 15;
            let mut r = if scalar { a } else { V::default() };
            let lanes = if scalar { 1 } else { reg_bytes(ins, 0) / if dbl { 8 } else { 4 } };
            for i in 0..lanes {
                let (x, y) = (fl(a, i, dbl), fl(b, i, dbl));
                let (lt, eq, gt, un) = (x < y, x == y, x > y, x.is_nan() || y.is_nan());
                let t = match pred {
                    0 => eq,
                    1 => lt,
                    2 => lt || eq,
                    3 => un,
                    4 => !eq,
                    5 => !lt,
                    6 => !(lt || eq),
                    7 => !un,
                    8 => eq || un,
                    9 => !(gt || eq),
                    10 => !gt,
                    11 => false,
                    12 => !eq && !un,
                    13 => gt || eq,
                    14 => gt,
                    _ => true,
                };
                if dbl {
                    s64(&mut r, i, if t { u64::MAX } else { 0 });
                } else {
                    s32(&mut r, i, if t { u32::MAX } else { 0 });
                }
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        // integer to float, one value
        M::Cvtsi2ss | M::Vcvtsi2ss | M::Cvtsi2sd | M::Vcvtsi2sd => {
            let dbl = matches!(mn, M::Cvtsi2sd | M::Vcvtsi2sd);
            let (base, src) = scalar_ops(ins);
            let size = Cpu::opsize(ins, src);
            let raw = cpu.read(mem, ins, src, size)?;
            let x = if size == 8 { raw as i64 } else { raw as u32 as i32 as i64 };
            let mut r = rd(cpu, mem, ins, base)?;
            // from an integer straight to a single: no stop at a double on the way
            if dbl {
                s64(&mut r, 0, (x as f64).to_bits());
            } else {
                s32(&mut r, 0, (x as f32).to_bits());
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        // float to integer, one value
        M::Cvtss2si
        | M::Vcvtss2si
        | M::Cvttss2si
        | M::Vcvttss2si
        | M::Cvtsd2si
        | M::Vcvtsd2si
        | M::Cvttsd2si
        | M::Vcvttsd2si => {
            let dbl = matches!(mn, M::Cvtsd2si | M::Vcvtsd2si | M::Cvttsd2si | M::Vcvttsd2si);
            let trunc = matches!(mn, M::Cvttss2si | M::Vcvttss2si | M::Cvttsd2si | M::Vcvttsd2si);
            let x = fl(rd(cpu, mem, ins, 1)?, 0, dbl);
            let wide = ins.op0_register().size() == 8;
            cpu.set_reg(ins.op0_register(), to_int(x, trunc, wide))?;
        }
        M::Cvtss2sd | M::Vcvtss2sd | M::Cvtsd2ss | M::Vcvtsd2ss => {
            let to_dbl = matches!(mn, M::Cvtss2sd | M::Vcvtss2sd);
            let (base, src) = scalar_ops(ins);
            let x = fl(rd(cpu, mem, ins, src)?, 0, !to_dbl);
            let mut r = rd(cpu, mem, ins, base)?;
            put_fl(&mut r, 0, to_dbl, x);
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        // packed conversions
        M::Cvtdq2ps | M::Vcvtdq2ps | M::Cvtps2dq | M::Vcvtps2dq | M::Cvttps2dq | M::Vcvttps2dq => {
            let s = rd(cpu, mem, ins, 1)?;
            let mut r = V::default();
            for i in 0..reg_bytes(ins, 0) / 4 {
                let x = g32(s, i);
                let y = match mn {
                    M::Cvtdq2ps | M::Vcvtdq2ps => (x as i32 as f32).to_bits(),
                    _ => to_int(f32::from_bits(x) as f64, matches!(mn, M::Cvttps2dq | M::Vcvttps2dq), false) as u32,
                };
                s32(&mut r, i, y);
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        M::Cvtdq2pd | M::Vcvtdq2pd | M::Cvtps2pd | M::Vcvtps2pd => {
            let from_int = matches!(mn, M::Cvtdq2pd | M::Vcvtdq2pd);
            let s = rd(cpu, mem, ins, 1)?;
            let mut r = V::default();
            for i in 0..reg_bytes(ins, 0) / 8 {
                let x = g32(s, i);
                let d = if from_int { x as i32 as f64 } else { f32::from_bits(x) as f64 };
                s64(&mut r, i, d.to_bits());
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        M::Cvtpd2ps | M::Vcvtpd2ps | M::Cvtpd2dq | M::Vcvtpd2dq | M::Cvttpd2dq | M::Vcvttpd2dq => {
            let s = rd(cpu, mem, ins, 1)?;
            let src_bytes = match ins.op_kind(1) {
                OpKind::Register => ins.op_register(1).size(),
                _ => ins.memory_size().size(),
            };
            let mut r = V::default();
            for i in 0..src_bytes / 8 {
                let d = f64::from_bits(g64(s, i));
                let y = match mn {
                    M::Cvtpd2ps | M::Vcvtpd2ps => (d as f32).to_bits(),
                    _ => to_int(d, matches!(mn, M::Cvttpd2dq | M::Vcvttpd2dq), false) as u32,
                };
                s32(&mut r, i, y);
            }
            wr(cpu, mem, ins, 0, r, vex)?;
        }
        _ => return super::vex::exec(cpu, mem, ins),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn run(code: &[u8], cpu: &mut Cpu, mem: &mut Mem) {
        let mut d = Decoder::with_ip(64, code, 0x1000, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            assert!(exec(cpu, mem, &ins).unwrap(), "{ins:?}");
        }
    }

    fn dbl(x: f64) -> u128 {
        x.to_bits() as u128
    }

    #[test]
    fn scalar_double_arithmetic_keeps_the_upper_lane_and_vex_copies_it_from_the_first_source() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        cpu.xmm[0] = dbl(1.5) | (7u128 << 64);
        cpu.xmm[1] = dbl(2.0);
        cpu.xmm[2] = dbl(10.0) | (9u128 << 64);
        // addsd xmm0,xmm1 ; vmulsd xmm3,xmm2,xmm0
        run(&[0xf2, 0x0f, 0x58, 0xc1, 0xc5, 0xeb, 0x59, 0xd8], &mut cpu, &mut mem);
        assert_eq!(cpu.xmm[0], dbl(3.5) | (7u128 << 64));
        assert_eq!(cpu.xmm[3], dbl(35.0) | (9u128 << 64));
    }

    #[test]
    fn comisd_sets_the_flags_the_manual_gives_for_less_equal_greater_and_unordered() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        let cases = [(1.0, 2.0, CF), (2.0, 2.0, ZF), (3.0, 2.0, 0), (f64::NAN, 2.0, ZF | PF | CF)];
        for (a, b, want) in cases {
            cpu.xmm[0] = dbl(a);
            cpu.xmm[1] = dbl(b);
            // comisd xmm0,xmm1
            run(&[0x66, 0x0f, 0x2f, 0xc1], &mut cpu, &mut mem);
            assert_eq!(cpu.flags & (CF | PF | ZF | OF | SF | AF), want, "{a} vs {b}");
        }
    }

    #[test]
    fn conversions_round_to_even_truncate_and_give_the_indefinite_value_when_out_of_range() {
        assert_eq!(to_int(2.5, false, false), 2);
        assert_eq!(to_int(3.5, false, false), 4);
        assert_eq!(to_int(-2.7, true, false) as u32 as i32, -2);
        assert_eq!(to_int(3e9, true, false), 0x8000_0000);
        assert_eq!(to_int(f64::NAN, true, true), 1 << 63);
        assert_eq!(to_int(3e9, true, true), 3_000_000_000);
    }

    #[test]
    fn packed_single_minimum_returns_the_second_operand_for_a_nan_like_the_hardware() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        let f = |a: f32, b: f32, c: f32, d: f32| {
            (a.to_bits() as u128)
                | ((b.to_bits() as u128) << 32)
                | ((c.to_bits() as u128) << 64)
                | ((d.to_bits() as u128) << 96)
        };
        cpu.xmm[0] = f(1.0, f32::NAN, 5.0, 0.0);
        cpu.xmm[1] = f(2.0, 3.0, f32::NAN, -0.0);
        // minps xmm0,xmm1
        run(&[0x0f, 0x5d, 0xc1], &mut cpu, &mut mem);
        let x = cpu.xmm[0];
        assert_eq!(f32::from_bits(x as u32), 1.0);
        assert_eq!(f32::from_bits((x >> 32) as u32), 3.0);
        assert!(f32::from_bits((x >> 64) as u32).is_nan());
        assert!(f32::from_bits((x >> 96) as u32).is_sign_negative());
    }
}
