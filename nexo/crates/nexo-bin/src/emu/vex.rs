//! The VEX forms (AVX and AVX2) compilers emit with -mavx2 for vector loops: moves, broadcasts, the integer
//! and bit operations on xmm and ymm, lane extraction and insertion, and vzeroupper. The lane arithmetic is
//! the SSE code in sse.rs run once per 128-bit half, which is how the in-lane AVX2 instructions are defined.
//! An EVEX (AVX-512) instruction, or a VEX one not listed here, stops the run by name.

use iced_x86::{EncodingKind, Instruction, Mnemonic as M, OpKind};

use super::cpu::{Cpu, Event};
use super::fp::{g32, g64, rd, reg_bytes, s32, s64, wr, V};
use super::mem::Mem;
use super::sse::{lane, lane_op};

type R<T> = Result<T, Event>;

/// The legacy SSE mnemonic whose lane arithmetic a VEX mnemonic shares.
fn legacy(mn: M) -> Option<M> {
    Some(match mn {
        M::Vpaddb => M::Paddb,
        M::Vpaddw => M::Paddw,
        M::Vpaddd => M::Paddd,
        M::Vpaddq => M::Paddq,
        M::Vpsubb => M::Psubb,
        M::Vpsubw => M::Psubw,
        M::Vpsubd => M::Psubd,
        M::Vpsubq => M::Psubq,
        M::Vpcmpeqb => M::Pcmpeqb,
        M::Vpcmpeqw => M::Pcmpeqw,
        M::Vpcmpeqd => M::Pcmpeqd,
        M::Vpcmpgtb => M::Pcmpgtb,
        M::Vpcmpgtw => M::Pcmpgtw,
        M::Vpcmpgtd => M::Pcmpgtd,
        M::Vpminub => M::Pminub,
        M::Vpmaxub => M::Pmaxub,
        M::Vpminsw => M::Pminsw,
        M::Vpmaxsw => M::Pmaxsw,
        M::Vpminsd => M::Pminsd,
        M::Vpmaxsd => M::Pmaxsd,
        M::Vpminud => M::Pminud,
        M::Vpmaxud => M::Pmaxud,
        M::Vpmullw => M::Pmullw,
        M::Vpmulld => M::Pmulld,
        M::Vpmulhw => M::Pmulhw,
        M::Vpmulhuw => M::Pmulhuw,
        M::Vpmuludq => M::Pmuludq,
        M::Vpsadbw => M::Psadbw,
        M::Vpavgb => M::Pavgb,
        M::Vpavgw => M::Pavgw,
        M::Vpaddusb => M::Paddusb,
        M::Vpaddusw => M::Paddusw,
        M::Vpsubusb => M::Psubusb,
        M::Vpsubusw => M::Psubusw,
        M::Vpaddsb => M::Paddsb,
        M::Vpaddsw => M::Paddsw,
        M::Vpsubsb => M::Psubsb,
        M::Vpsubsw => M::Psubsw,
        M::Vpand => M::Pand,
        M::Vpandn => M::Pandn,
        M::Vpor => M::Por,
        M::Vpxor => M::Pxor,
        M::Vandps => M::Andps,
        M::Vandpd => M::Andpd,
        M::Vandnps => M::Andnps,
        M::Vandnpd => M::Andnpd,
        M::Vorps => M::Orps,
        M::Vorpd => M::Orpd,
        M::Vxorps => M::Xorps,
        M::Vxorpd => M::Xorpd,
        M::Vpshufb => M::Pshufb,
        M::Vpshufd => M::Pshufd,
        M::Vpshuflw => M::Pshuflw,
        M::Vpshufhw => M::Pshufhw,
        M::Vshufps => M::Shufps,
        M::Vshufpd => M::Shufpd,
        M::Vpunpcklbw => M::Punpcklbw,
        M::Vpunpcklwd => M::Punpcklwd,
        M::Vpunpckldq => M::Punpckldq,
        M::Vpunpcklqdq => M::Punpcklqdq,
        M::Vpunpckhbw => M::Punpckhbw,
        M::Vpunpckhwd => M::Punpckhwd,
        M::Vpunpckhdq => M::Punpckhdq,
        M::Vpunpckhqdq => M::Punpckhqdq,
        M::Vunpcklps => M::Unpcklps,
        M::Vunpckhps => M::Unpckhps,
        M::Vunpcklpd => M::Unpcklpd,
        M::Vunpckhpd => M::Unpckhpd,
        M::Vpacksswb => M::Packsswb,
        M::Vpackuswb => M::Packuswb,
        M::Vpackssdw => M::Packssdw,
        M::Vpackusdw => M::Packusdw,
        M::Vpsllw => M::Psllw,
        M::Vpslld => M::Pslld,
        M::Vpsllq => M::Psllq,
        M::Vpsrlw => M::Psrlw,
        M::Vpsrld => M::Psrld,
        M::Vpsrlq => M::Psrlq,
        M::Vpsraw => M::Psraw,
        M::Vpsrad => M::Psrad,
        M::Vpslldq => M::Pslldq,
        M::Vpsrldq => M::Psrldq,
        _ => return None,
    })
}

fn mask_of(w: usize) -> u128 {
    (1u128 << (w * 8)) - 1
}

/// `n` copies of the low `w` bytes of `x` across a 128-bit lane.
fn splat(x: u128, w: usize) -> u128 {
    (0..16 / w).fold(0u128, |acc, i| acc | ((x & mask_of(w)) << (i * w * 8)))
}

fn sel128(a: V, b: V, code: u64) -> u128 {
    if code & 8 != 0 {
        return 0;
    }
    match code & 3 {
        0 => a.lo,
        1 => a.hi,
        2 => b.lo,
        _ => b.hi,
    }
}

/// The sign bit of each `w`-byte lane of a vector of `bytes` bytes, lowest lane in bit 0.
fn sign_mask(v: V, w: usize, bytes: usize) -> u64 {
    let per = 16 / w;
    let half = |x: u128| (0..per).fold(0u64, |acc, i| acc | ((lane(x, w, i) >> (w * 8 - 1)) << i));
    half(v.lo) | if bytes == 32 { half(v.hi) << per } else { 0 }
}

/// Run one VEX instruction; `Ok(false)` when it is not one this unit knows.
pub(super) fn exec(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction) -> R<bool> {
    let mn = ins.mnemonic();
    match ins.encoding() {
        EncodingKind::VEX => {}
        EncodingKind::EVEX => {
            return Err(Event::Unsupported("an AVX-512 (EVEX) instruction".into()));
        }
        _ => {
            return match mn {
                M::Ptest => {
                    ptest(cpu, mem, ins)?;
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
    }
    let n = ins.op_count();
    let bytes = reg_bytes(ins, 0);
    if let Some(base) = legacy(mn) {
        // (first source, second source or count or shuffle source, immediate)
        let imm_op = n - 1;
        let (a, b, imm) = match mn {
            M::Vpshufd | M::Vpshuflw | M::Vpshufhw => (V::default(), rd(cpu, mem, ins, 1)?, ins.immediate(imm_op)),
            M::Vshufps | M::Vshufpd => (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?, ins.immediate(imm_op)),
            M::Vpslldq | M::Vpsrldq => (rd(cpu, mem, ins, 1)?, V::default(), ins.immediate(imm_op)),
            M::Vpsllw | M::Vpslld | M::Vpsllq | M::Vpsrlw | M::Vpsrld | M::Vpsrlq | M::Vpsraw | M::Vpsrad => {
                let count = if ins.op_kind(2) == OpKind::Immediate8 {
                    ins.immediate(2) as u128
                } else {
                    rd(cpu, mem, ins, 2)?.lo & u64::MAX as u128
                };
                (rd(cpu, mem, ins, 1)?, V { lo: count, hi: count }, 0)
            }
            _ => (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?, 0),
        };
        let lo = lane_op(base, a.lo, b.lo, imm).ok_or_else(|| Event::Unsupported(format!("{mn:?}").to_lowercase()))?;
        let hi = if bytes == 32 { lane_op(base, a.hi, b.hi, imm).unwrap_or(0) } else { 0 };
        wr(cpu, mem, ins, 0, V { lo, hi }, true)?;
        return Ok(true);
    }
    match mn {
        M::Vzeroupper => cpu.ymmh = [0; 16],
        M::Vzeroall => {
            cpu.ymmh = [0; 16];
            cpu.xmm = [0; 16];
        }
        M::Vmovdqa
        | M::Vmovdqu
        | M::Vmovaps
        | M::Vmovups
        | M::Vmovapd
        | M::Vmovupd
        | M::Vlddqu
        | M::Vmovntdq
        | M::Vmovntps
        | M::Vmovntpd
        | M::Vmovntdqa => {
            let v = rd(cpu, mem, ins, 1)?;
            wr(cpu, mem, ins, 0, v, true)?;
        }
        M::Vmovd | M::Vmovq => {
            let size = if mn == M::Vmovd { 4 } else { 8 };
            let v = match ins.op_kind(1) {
                OpKind::Register if ins.op_register(1).is_gpr() => cpu.reg(ins.op_register(1))? as u128,
                _ => rd(cpu, mem, ins, 1)?.lo,
            } & mask_of(size);
            match ins.op_kind(0) {
                OpKind::Register if ins.op_register(0).is_gpr() => cpu.set_reg(ins.op_register(0), v as u64)?,
                OpKind::Memory => mem.write(cpu.addr(ins, 0), &v.to_le_bytes()[..size])?,
                _ => wr(cpu, mem, ins, 0, V { lo: v, hi: 0 }, true)?,
            }
        }
        M::Vmovss | M::Vmovsd => {
            let size = if mn == M::Vmovss { 4 } else { 8 };
            let m = mask_of(size);
            if n == 3 {
                let (a, b) = (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?);
                wr(cpu, mem, ins, 0, V { lo: (a.lo & !m) | (b.lo & m), hi: 0 }, true)?;
            } else if ins.op_kind(0) == OpKind::Memory {
                let v = rd(cpu, mem, ins, 1)?.lo & m;
                mem.write(cpu.addr(ins, 0), &v.to_le_bytes()[..size])?;
            } else {
                let v = rd(cpu, mem, ins, 1)?.lo & m;
                wr(cpu, mem, ins, 0, V { lo: v, hi: 0 }, true)?;
            }
        }
        M::Vmovlps | M::Vmovlpd | M::Vmovhps | M::Vmovhpd | M::Vmovhlps | M::Vmovlhps => {
            let lo64 = u64::MAX as u128;
            if ins.op_kind(0) == OpKind::Memory {
                let s = rd(cpu, mem, ins, 1)?.lo;
                let half = if matches!(mn, M::Vmovhps | M::Vmovhpd) { s >> 64 } else { s & lo64 };
                mem.write(cpu.addr(ins, 0), &half.to_le_bytes()[..8])?;
            } else {
                let (a, b) = (rd(cpu, mem, ins, 1)?.lo, rd(cpu, mem, ins, 2)?.lo);
                let v = match mn {
                    M::Vmovlps | M::Vmovlpd => (a & !lo64) | (b & lo64),
                    M::Vmovhps | M::Vmovhpd => (a & lo64) | ((b & lo64) << 64),
                    M::Vmovhlps => (a & !lo64) | (b >> 64),
                    _ => (a & lo64) | ((b & lo64) << 64),
                };
                wr(cpu, mem, ins, 0, V { lo: v, hi: 0 }, true)?;
            }
        }
        M::Vpmovmskb | M::Vmovmskps | M::Vmovmskpd => {
            let w = match mn {
                M::Vpmovmskb => 1,
                M::Vmovmskps => 4,
                _ => 8,
            };
            let s = rd(cpu, mem, ins, 1)?;
            let src_bytes = if ins.op_register(1).is_ymm() { 32 } else { 16 };
            cpu.set_reg(ins.op0_register(), sign_mask(s, w, src_bytes))?;
        }
        M::Vbroadcastss | M::Vbroadcastsd | M::Vpbroadcastb | M::Vpbroadcastw | M::Vpbroadcastd | M::Vpbroadcastq => {
            let w = match mn {
                M::Vpbroadcastb => 1,
                M::Vpbroadcastw => 2,
                M::Vbroadcastss | M::Vpbroadcastd => 4,
                _ => 8,
            };
            let one = splat(rd(cpu, mem, ins, 1)?.lo, w);
            wr(cpu, mem, ins, 0, V { lo: one, hi: if bytes == 32 { one } else { 0 } }, true)?;
        }
        M::Vbroadcasti128 | M::Vbroadcastf128 => {
            let x = rd(cpu, mem, ins, 1)?.lo;
            wr(cpu, mem, ins, 0, V { lo: x, hi: x }, true)?;
        }
        M::Vextracti128 | M::Vextractf128 => {
            let s = rd(cpu, mem, ins, 1)?;
            let half = if ins.immediate(2) & 1 == 0 { s.lo } else { s.hi };
            wr(cpu, mem, ins, 0, V { lo: half, hi: 0 }, true)?;
        }
        M::Vinserti128 | M::Vinsertf128 => {
            let (mut a, b) = (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?);
            if ins.immediate(3) & 1 == 0 {
                a.lo = b.lo;
            } else {
                a.hi = b.lo;
            }
            wr(cpu, mem, ins, 0, a, true)?;
        }
        M::Vperm2i128 | M::Vperm2f128 => {
            let (a, b, imm) = (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?, ins.immediate(3));
            wr(cpu, mem, ins, 0, V { lo: sel128(a, b, imm & 0xf), hi: sel128(a, b, (imm >> 4) & 0xf) }, true)?;
        }
        M::Vpermq | M::Vpermpd => {
            let (s, imm) = (rd(cpu, mem, ins, 1)?, ins.immediate(2));
            let mut r = V::default();
            for i in 0..4 {
                s64(&mut r, i, g64(s, ((imm >> (2 * i)) & 3) as usize));
            }
            wr(cpu, mem, ins, 0, r, true)?;
        }
        M::Vpermd | M::Vpermps => {
            let (idx, s) = (rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?);
            let mut r = V::default();
            for i in 0..8 {
                s32(&mut r, i, g32(s, (g32(idx, i) & 7) as usize));
            }
            wr(cpu, mem, ins, 0, r, true)?;
        }
        M::Vptest => ptest(cpu, mem, ins)?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// ptest / vptest: ZF when `a & b` is zero, CF when `!a & b` is zero; the other flags are cleared.
fn ptest(cpu: &mut Cpu, mem: &Mem, ins: &Instruction) -> R<()> {
    let (a, b) = (rd(cpu, mem, ins, 0)?, rd(cpu, mem, ins, 1)?);
    let zf = (a.lo & b.lo) == 0 && (a.hi & b.hi) == 0;
    let cf = (!a.lo & b.lo) == 0 && (!a.hi & b.hi) == 0;
    cpu.flags = (cpu.flags & !0x8d5) | if zf { 0x40 } else { 0 } | if cf { 1 } else { 0 };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn run(code: &[u8], cpu: &mut Cpu, mem: &mut Mem) {
        let mut d = Decoder::with_ip(64, code, 0x1000, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            let ok = super::super::fp::exec(cpu, mem, &ins).unwrap();
            assert!(ok, "{ins:?}");
        }
    }

    #[test]
    fn ymm_add_works_on_both_halves_and_vzeroupper_clears_the_high_halves() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        let four = |a: u32| (0..4).fold(0u128, |acc, i| acc | ((a + i) as u128) << (32 * i));
        cpu.xmm[1] = four(1);
        cpu.ymmh[1] = four(5);
        cpu.xmm[2] = four(10);
        cpu.ymmh[2] = four(20);
        // vpaddd ymm0,ymm1,ymm2
        run(&[0xc5, 0xf5, 0xfe, 0xc2], &mut cpu, &mut mem);
        assert_eq!(cpu.xmm[0], (0..4).fold(0u128, |acc, i| acc | ((11 + 2 * i) as u128) << (32 * i)));
        assert_eq!(cpu.ymmh[0], (0..4).fold(0u128, |acc, i| acc | ((25 + 2 * i) as u128) << (32 * i)));
        // vzeroupper
        run(&[0xc5, 0xf8, 0x77], &mut cpu, &mut mem);
        assert_eq!(cpu.ymmh[0], 0);
        assert_eq!(cpu.xmm[0], (0..4).fold(0u128, |acc, i| acc | ((11 + 2 * i) as u128) << (32 * i)));
    }

    #[test]
    fn broadcast_fills_every_lane_and_extract_insert_move_the_halves() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        cpu.xmm[1] = 0xdead_beef;
        // vpbroadcastd ymm0,xmm1 ; vextracti128 xmm2,ymm0,1 ; vinserti128 ymm3,ymm0,xmm1,1
        run(
            &[0xc4, 0xe2, 0x7d, 0x58, 0xc1, 0xc4, 0xe3, 0x7d, 0x39, 0xc2, 0x01, 0xc4, 0xe3, 0x7d, 0x38, 0xd9, 0x01],
            &mut cpu,
            &mut mem,
        );
        let all = 0xdead_beefu128 * 0x0000_0001_0000_0001_0000_0001_0000_0001;
        assert_eq!(cpu.xmm[0], all);
        assert_eq!(cpu.ymmh[0], all);
        assert_eq!(cpu.xmm[2], all);
        assert_eq!(cpu.xmm[3], all);
        assert_eq!(cpu.ymmh[3], 0xdead_beef);
    }

    #[test]
    fn an_avx512_instruction_stops_by_name_instead_of_being_guessed() {
        let mut cpu = Cpu::new();
        let mut mem = Mem::default();
        // vpaddd zmm0,zmm1,zmm2 (EVEX)
        let code = [0x62, 0xf1, 0x75, 0x48, 0xfe, 0xc2];
        let ins = Decoder::with_ip(64, &code, 0x1000, DecoderOptions::NONE).decode();
        let r = super::super::fp::exec(&mut cpu, &mut mem, &ins);
        assert!(matches!(r, Err(Event::Unsupported(m)) if m.contains("AVX-512")));
    }
}
