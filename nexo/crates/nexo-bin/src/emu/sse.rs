//! The vector unit: the SSE2 moves, bit operations and lane arithmetic compiled C uses for copies, xor
//! loops and string scans. Floating point maths is not here: such an instruction stops the run by name.

use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};

use super::cpu::{Cpu, Event};
use super::mem::Mem;

type R<T> = Result<T, Event>;

fn is_xmm(r: Register) -> bool {
    r.is_xmm()
}

/// Operand `op` as a 128-bit value: an xmm register, a general register, memory of the operand's size or
/// an immediate.
fn get(cpu: &Cpu, mem: &Mem, ins: &Instruction, op: u32) -> R<u128> {
    match ins.op_kind(op) {
        OpKind::Register => {
            let r = ins.op_register(op);
            if is_xmm(r) {
                Ok(cpu.xmm[r.number()])
            } else if r.is_gpr() {
                Ok(cpu.reg(r)? as u128)
            } else {
                Err(Event::Unsupported(format!("register {r:?}").to_lowercase()))
            }
        }
        OpKind::Memory => {
            let n = ins.memory_size().size();
            if n == 0 || n > 16 {
                return Err(Event::Unsupported(format!("a {n}-byte vector memory operand")));
            }
            let mut b = [0u8; 16];
            mem.read(cpu.addr(ins, op), &mut b[..n])?;
            Ok(u128::from_le_bytes(b))
        }
        OpKind::Immediate8 => Ok(ins.immediate(op) as u128),
        k => Err(Event::Unsupported(format!("operand kind {k:?}"))),
    }
}

/// Write the low `size` bytes of `v` to operand `op`; an xmm register takes all 128 bits.
fn put(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction, op: u32, size: usize, v: u128) -> R<()> {
    match ins.op_kind(op) {
        OpKind::Register => {
            let r = ins.op_register(op);
            if is_xmm(r) {
                cpu.xmm[r.number()] = v;
                Ok(())
            } else {
                cpu.set_reg(r, v as u64)
            }
        }
        OpKind::Memory => Ok(mem.write(cpu.addr(ins, op), &v.to_le_bytes()[..size])?),
        k => Err(Event::Unsupported(format!("write to operand kind {k:?}"))),
    }
}

fn lane(v: u128, w: usize, i: usize) -> u64 {
    let bits = w * 8;
    ((v >> (i * bits)) & ((1u128 << bits) - 1)) as u64
}

fn sext(v: u64, w: usize) -> i64 {
    let sh = 64 - w * 8;
    ((v << sh) as i64) >> sh
}

/// Lane by lane: `f(a, b)` on each `w`-byte lane of the two values.
fn lanes(a: u128, b: u128, w: usize, f: impl Fn(u64, u64) -> u64) -> u128 {
    let bits = w * 8;
    let m = (1u128 << bits) - 1;
    (0..16 / w).fold(0u128, |acc, i| acc | ((f(lane(a, w, i), lane(b, w, i)) as u128 & m) << (i * bits)))
}

/// Lanes from a list (the first is the lowest).
fn join(vals: &[u64], w: usize) -> u128 {
    let m = (1u128 << (w * 8)) - 1;
    vals.iter().enumerate().fold(0u128, |acc, (i, &v)| acc | ((v as u128 & m) << (i * w * 8)))
}

fn width(mn: M) -> usize {
    match mn {
        M::Paddb
        | M::Psubb
        | M::Pcmpeqb
        | M::Pcmpgtb
        | M::Punpcklbw
        | M::Punpckhbw
        | M::Pminub
        | M::Pmaxub
        | M::Pavgb
        | M::Paddusb
        | M::Psubusb
        | M::Paddsb
        | M::Psubsb => 1,
        M::Paddw
        | M::Psubw
        | M::Pcmpeqw
        | M::Pcmpgtw
        | M::Punpcklwd
        | M::Punpckhwd
        | M::Psllw
        | M::Psrlw
        | M::Psraw
        | M::Pminsw
        | M::Pmaxsw
        | M::Pmullw
        | M::Pmulhw
        | M::Pmulhuw
        | M::Pavgw
        | M::Paddusw
        | M::Psubusw
        | M::Paddsw
        | M::Psubsw => 2,
        M::Paddd
        | M::Psubd
        | M::Pcmpeqd
        | M::Pcmpgtd
        | M::Punpckldq
        | M::Punpckhdq
        | M::Pslld
        | M::Psrld
        | M::Psrad => 4,
        _ => 8,
    }
}

fn sat(v: i64, lo: i64, hi: i64) -> u64 {
    v.clamp(lo, hi) as u64
}

/// Run one vector instruction; `Ok(false)` when it is not one this unit knows.
pub(super) fn exec(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction) -> R<bool> {
    let mn = ins.mnemonic();
    let op0_xmm = ins.op0_kind() == OpKind::Register && is_xmm(ins.op0_register());
    let op1_xmm = ins.op_count() > 1 && ins.op1_kind() == OpKind::Register && is_xmm(ins.op1_register());
    // MMX registers share names with these instructions; they are not modelled
    for op in 0..ins.op_count() {
        if ins.op_kind(op) == OpKind::Register && ins.op_register(op).is_mm() {
            return Err(Event::Unsupported("an MMX register".into()));
        }
    }
    match mn {
        M::Movdqa
        | M::Movdqu
        | M::Movaps
        | M::Movups
        | M::Movapd
        | M::Movupd
        | M::Lddqu
        | M::Movntdq
        | M::Movntps
        | M::Movntpd
        | M::Movntdqa => {
            let v = get(cpu, mem, ins, 1)?;
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Movd | M::Movq => {
            let size = if mn == M::Movd { 4 } else { 8 };
            let v = get(cpu, mem, ins, 1)? & ((1u128 << (size * 8)) - 1);
            // into an xmm register the rest is zeroed; into a general register a 32-bit write zero-extends
            put(cpu, mem, ins, 0, size, v)?;
        }
        M::Movss | M::Movsd => {
            let size = if mn == M::Movss { 4 } else { 8 };
            let m = (1u128 << (size * 8)) - 1;
            let v = get(cpu, mem, ins, 1)? & m;
            if op0_xmm && op1_xmm {
                let d = get(cpu, mem, ins, 0)?;
                put(cpu, mem, ins, 0, 16, (d & !m) | v)?;
            } else {
                put(cpu, mem, ins, 0, size, v)?;
            }
        }
        M::Movlps | M::Movlpd | M::Movhps | M::Movhpd | M::Movhlps | M::Movlhps => {
            let lo = u64::MAX as u128;
            let s = get(cpu, mem, ins, 1)?;
            if !op0_xmm {
                // store of one half
                let half = if matches!(mn, M::Movhps | M::Movhpd) { s >> 64 } else { s & lo };
                put(cpu, mem, ins, 0, 8, half)?;
            } else {
                let d = get(cpu, mem, ins, 0)?;
                let s64 = if mn == M::Movhlps { s >> 64 } else { s & lo };
                let v = match mn {
                    M::Movlps | M::Movlpd | M::Movhlps => (d & !lo) | s64,
                    _ => (d & lo) | (s64 << 64),
                };
                put(cpu, mem, ins, 0, 16, v)?;
            }
        }
        M::Pxor
        | M::Xorps
        | M::Xorpd
        | M::Pand
        | M::Andps
        | M::Andpd
        | M::Por
        | M::Orps
        | M::Orpd
        | M::Pandn
        | M::Andnps
        | M::Andnpd => {
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let v = match mn {
                M::Pxor | M::Xorps | M::Xorpd => d ^ s,
                M::Pand | M::Andps | M::Andpd => d & s,
                M::Por | M::Orps | M::Orpd => d | s,
                _ => !d & s,
            };
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Paddb
        | M::Paddw
        | M::Paddd
        | M::Paddq
        | M::Psubb
        | M::Psubw
        | M::Psubd
        | M::Psubq
        | M::Pcmpeqb
        | M::Pcmpeqw
        | M::Pcmpeqd
        | M::Pcmpgtb
        | M::Pcmpgtw
        | M::Pcmpgtd
        | M::Pminub
        | M::Pmaxub
        | M::Pminsw
        | M::Pmaxsw
        | M::Pmullw
        | M::Pmulhw
        | M::Pmulhuw
        | M::Pavgb
        | M::Pavgw
        | M::Paddusb
        | M::Paddusw
        | M::Psubusb
        | M::Psubusw
        | M::Paddsb
        | M::Paddsw
        | M::Psubsb
        | M::Psubsw => {
            let w = width(mn);
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let ones = u64::MAX;
            let (smin, smax) = (-(1i64 << (w * 8 - 1)), (1i64 << (w * 8 - 1)) - 1);
            let umax = ((1u128 << (w * 8)) - 1) as i64;
            let v = lanes(d, s, w, |a, b| match mn {
                M::Paddb | M::Paddw | M::Paddd | M::Paddq => a.wrapping_add(b),
                M::Psubb | M::Psubw | M::Psubd | M::Psubq => a.wrapping_sub(b),
                M::Pcmpeqb | M::Pcmpeqw | M::Pcmpeqd => {
                    if a == b {
                        ones
                    } else {
                        0
                    }
                }
                M::Pcmpgtb | M::Pcmpgtw | M::Pcmpgtd => {
                    if sext(a, w) > sext(b, w) {
                        ones
                    } else {
                        0
                    }
                }
                M::Pminub => a.min(b),
                M::Pmaxub => a.max(b),
                M::Pminsw => sext(a, w).min(sext(b, w)) as u64,
                M::Pmaxsw => sext(a, w).max(sext(b, w)) as u64,
                M::Pmullw => (sext(a, w) * sext(b, w)) as u64,
                M::Pmulhw => ((sext(a, w) * sext(b, w)) >> 16) as u64,
                M::Pmulhuw => (a * b) >> 16,
                M::Pavgb | M::Pavgw => (a + b + 1) >> 1,
                M::Paddusb | M::Paddusw => sat(a as i64 + b as i64, 0, umax),
                M::Psubusb | M::Psubusw => sat(a as i64 - b as i64, 0, umax),
                M::Paddsb | M::Paddsw => sat(sext(a, w) + sext(b, w), smin, smax),
                _ => sat(sext(a, w) - sext(b, w), smin, smax),
            });
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Pmuludq => {
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let v = join(&[lane(d, 4, 0) * lane(s, 4, 0), lane(d, 4, 2) * lane(s, 4, 2)], 8);
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Psadbw => {
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let sum = |h: usize| (0..8).map(|i| lane(d, 1, h * 8 + i).abs_diff(lane(s, 1, h * 8 + i))).sum::<u64>();
            put(cpu, mem, ins, 0, 16, join(&[sum(0), sum(1)], 8))?;
        }
        M::Pmovmskb | M::Movmskps | M::Movmskpd => {
            let w = match mn {
                M::Pmovmskb => 1,
                M::Movmskps => 4,
                _ => 8,
            };
            let s = get(cpu, mem, ins, 1)?;
            let bits = (0..16 / w).fold(0u64, |acc, i| acc | ((lane(s, w, i) >> (w * 8 - 1)) << i));
            cpu.set_reg(ins.op0_register(), bits)?;
        }
        M::Pshufd => {
            let (s, imm) = (get(cpu, mem, ins, 1)?, ins.immediate(2));
            let v: Vec<u64> = (0..4).map(|i| lane(s, 4, ((imm >> (2 * i)) & 3) as usize)).collect();
            put(cpu, mem, ins, 0, 16, join(&v, 4))?;
        }
        M::Pshuflw | M::Pshufhw => {
            let (s, imm) = (get(cpu, mem, ins, 1)?, ins.immediate(2));
            let base = if mn == M::Pshuflw { 0 } else { 4 };
            let mut v: Vec<u64> = (0..8).map(|i| lane(s, 2, i)).collect();
            for i in 0..4 {
                v[base + i] = lane(s, 2, base + ((imm >> (2 * i)) & 3) as usize);
            }
            put(cpu, mem, ins, 0, 16, join(&v, 2))?;
        }
        M::Shufps => {
            let (d, s, imm) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?, ins.immediate(2));
            let pick = |v: u128, i: u64| lane(v, 4, ((imm >> (2 * i)) & 3) as usize);
            put(cpu, mem, ins, 0, 16, join(&[pick(d, 0), pick(d, 1), pick(s, 2), pick(s, 3)], 4))?;
        }
        M::Punpcklbw
        | M::Punpcklwd
        | M::Punpckldq
        | M::Punpcklqdq
        | M::Punpckhbw
        | M::Punpckhwd
        | M::Punpckhdq
        | M::Punpckhqdq
        | M::Unpcklps
        | M::Unpckhps
        | M::Unpcklpd
        | M::Unpckhpd => {
            let w = match mn {
                M::Punpcklqdq | M::Punpckhqdq | M::Unpcklpd | M::Unpckhpd => 8,
                M::Unpcklps | M::Unpckhps => 4,
                _ => width(mn),
            };
            let high =
                matches!(mn, M::Punpckhbw | M::Punpckhwd | M::Punpckhdq | M::Punpckhqdq | M::Unpckhps | M::Unpckhpd);
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let half = 8 / w;
            let from = if high { half } else { 0 };
            let v: Vec<u64> = (0..half).flat_map(|i| [lane(d, w, from + i), lane(s, w, from + i)]).collect();
            put(cpu, mem, ins, 0, 16, join(&v, w))?;
        }
        M::Packuswb | M::Packsswb | M::Packssdw => {
            let (d, s) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?);
            let (w, lo, hi) = match mn {
                M::Packuswb => (2, 0, 255),
                M::Packsswb => (2, -128, 127),
                _ => (4, -32768, 32767),
            };
            let n = 16 / w;
            let v: Vec<u64> =
                (0..2 * n).map(|i| sat(sext(lane(if i < n { d } else { s }, w, i % n), w), lo, hi)).collect();
            put(cpu, mem, ins, 0, 16, join(&v, w / 2))?;
        }
        M::Psllw | M::Pslld | M::Psllq | M::Psrlw | M::Psrld | M::Psrlq | M::Psraw | M::Psrad => {
            let w = width(mn);
            let d = get(cpu, mem, ins, 0)?;
            let count = (get(cpu, mem, ins, 1)? & u64::MAX as u128) as u64;
            let bits = (w * 8) as u64;
            let v = lanes(d, 0, w, |a, _| match mn {
                M::Psllw | M::Pslld | M::Psllq if count < bits => a << count,
                M::Psrlw | M::Psrld | M::Psrlq if count < bits => a >> count,
                M::Psraw | M::Psrad => (sext(a, w) >> count.min(bits - 1)) as u64,
                _ => 0,
            });
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Pslldq | M::Psrldq => {
            let d = get(cpu, mem, ins, 0)?;
            let n = ins.immediate(1).min(16) as u32 * 8;
            let v = match (mn, n) {
                (_, 128) => 0,
                (M::Pslldq, n) => d << n,
                (_, n) => d >> n,
            };
            put(cpu, mem, ins, 0, 16, v)?;
        }
        M::Pextrw => {
            let (s, imm) = (get(cpu, mem, ins, 1)?, ins.immediate(2));
            // into a general register the word is zero-extended
            put(cpu, mem, ins, 0, 2, lane(s, 2, (imm & 7) as usize) as u128)?;
        }
        M::Pinsrw => {
            let (d, s, imm) = (get(cpu, mem, ins, 0)?, get(cpu, mem, ins, 1)?, ins.immediate(2));
            let sh = (imm & 7) * 16;
            put(cpu, mem, ins, 0, 16, (d & !(0xffffu128 << sh)) | ((s & 0xffff) << sh))?;
        }
        _ => return Ok(false),
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

    #[test]
    fn byte_lanes_compare_and_move_their_mask_to_a_general_register() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        cpu.xmm[0] = u128::from_le_bytes(*b"hello, world!\0\0\0");
        // pxor xmm1,xmm1 ; pcmpeqb xmm1,xmm0 ; pmovmskb eax,xmm1
        run(&[0x66, 0x0f, 0xef, 0xc9, 0x66, 0x0f, 0x74, 0xc8, 0x66, 0x0f, 0xd7, 0xc1], &mut cpu, &mut mem);
        assert_eq!(cpu.gpr[0], 0b1110_0000_0000_0000);
    }

    #[test]
    fn shuffles_unpacks_and_shifts_move_lanes_where_the_manual_says() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        cpu.xmm[0] = join(&[1, 2, 3, 4], 4);
        // pshufd xmm1,xmm0,0x1b (reverse) ; punpckldq xmm0,xmm1 ; psrldq xmm1,4 ; pslld xmm1,1
        run(
            &[
                0x66, 0x0f, 0x70, 0xc8, 0x1b, 0x66, 0x0f, 0x62, 0xc1, 0x66, 0x0f, 0x73, 0xd9, 0x04, 0x66, 0x0f, 0x72,
                0xf1, 0x01,
            ],
            &mut cpu,
            &mut mem,
        );
        assert_eq!(cpu.xmm[0], join(&[1, 4, 2, 3], 4));
        assert_eq!(cpu.xmm[1], join(&[6, 4, 2, 0], 4));
    }
}
