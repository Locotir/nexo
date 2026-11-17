//! FMA3, the fused multiply-add forms compilers emit for `a * b + c` from -march=x86-64-v3 (Haswell and Zen)
//! up: vfmadd, vfmsub, vfnmadd, vfnmsub, vfmaddsub and vfmsubadd, each in the 132, 213 and 231 operand orders,
//! scalar and packed, single and double, on xmm and ymm. Every lane rounds once, as the hardware does, so a
//! fused result can differ from a separate multiply and add in the last bit. The AMD-only four-operand FMA4
//! forms are not here and still stop the run by name.

use iced_x86::{Instruction, Mnemonic as M};

use super::cpu::{Cpu, Event};
use super::fp::{g32, g64, rd, reg_bytes, s32, s64, wr, V};
use super::mem::Mem;

type R<T> = Result<T, Event>;

/// Which lanes subtract the addend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Add,
    Sub,
    /// vfmaddsub: even lanes subtract, odd lanes add
    SubEven,
    /// vfmsubadd: even lanes add, odd lanes subtract
    SubOdd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Form {
    /// vfnm*: the product is negated before the addend goes in
    neg: bool,
    sign: Sign,
    /// The operands (0 is the destination) that are the two factors and the addend, in that order.
    order: [usize; 3],
    dbl: bool,
    scalar: bool,
}

/// What an FMA3 mnemonic computes, read from its name; None for every other mnemonic.
fn form(mn: M) -> Option<Form> {
    let name = format!("{mn:?}").to_lowercase();
    let rest = name.strip_prefix("vf")?;
    // the longer names first: "maddsub" starts with "madd"
    let kinds = [
        ("maddsub", false, Sign::SubEven),
        ("msubadd", false, Sign::SubOdd),
        ("nmadd", true, Sign::Add),
        ("nmsub", true, Sign::Sub),
        ("madd", false, Sign::Add),
        ("msub", false, Sign::Sub),
    ];
    let (neg, sign, rest) = kinds.iter().find_map(|&(k, neg, sign)| rest.strip_prefix(k).map(|r| (neg, sign, r)))?;
    let order = match rest.get(..3)? {
        "132" => [0, 2, 1],
        "213" => [1, 0, 2],
        "231" => [1, 2, 0],
        _ => return None,
    };
    let (dbl, scalar) = match &rest[3..] {
        "ps" => (false, false),
        "pd" => (true, false),
        "ss" => (false, true),
        "sd" => (true, true),
        _ => return None,
    };
    if scalar && matches!(sign, Sign::SubEven | Sign::SubOdd) {
        return None;
    }
    Some(Form { neg, sign, order, dbl, scalar })
}

/// A NaN in: x86 gives back the first NaN among the destination, the second and the third operand (in that
/// order, whatever the 132/213/231 order), made quiet, and the negating forms leave its sign alone.
fn nan64(ops: [f64; 3]) -> Option<f64> {
    ops.iter().find(|x| x.is_nan()).map(|x| f64::from_bits(x.to_bits() | (1 << 51)))
}

fn nan32(ops: [f32; 3]) -> Option<f32> {
    ops.iter().find(|x| x.is_nan()).map(|x| f32::from_bits(x.to_bits() | (1 << 22)))
}

/// Run one FMA3 instruction; `Ok(false)` when it is not one.
pub(super) fn exec(cpu: &mut Cpu, mem: &mut Mem, ins: &Instruction) -> R<bool> {
    let Some(f) = form(ins.mnemonic()) else {
        return Ok(false);
    };
    let ops = [rd(cpu, mem, ins, 0)?, rd(cpu, mem, ins, 1)?, rd(cpu, mem, ins, 2)?];
    let [x, y, z] = f.order.map(|o| ops[o]);
    // a scalar form keeps the destination's other lanes
    let mut r = if f.scalar { ops[0] } else { V::default() };
    let lanes = if f.scalar { 1 } else { reg_bytes(ins, 0) / if f.dbl { 8 } else { 4 } };
    for i in 0..lanes {
        let sub = match f.sign {
            Sign::Add => false,
            Sign::Sub => true,
            Sign::SubEven => i % 2 == 0,
            Sign::SubOdd => i % 2 == 1,
        };
        // The signs go on the inputs: the product's sign is exact, and -(a * b - c) would turn the +0 of
        // a zero sum into -0.
        if f.dbl {
            let [a, b, c] = [x, y, z].map(|v| f64::from_bits(g64(v, i)));
            let in_order = [0, 1, 2].map(|o| f64::from_bits(g64(ops[o], i)));
            let v = nan64(in_order).unwrap_or_else(|| {
                let a = if f.neg { -a } else { a };
                a.mul_add(b, if sub { -c } else { c })
            });
            s64(&mut r, i, v.to_bits());
        } else {
            let [a, b, c] = [x, y, z].map(|v| f32::from_bits(g32(v, i)));
            let in_order = [0, 1, 2].map(|o| f32::from_bits(g32(ops[o], i)));
            let v = nan32(in_order).unwrap_or_else(|| {
                let a = if f.neg { -a } else { a };
                a.mul_add(b, if sub { -c } else { c })
            });
            s32(&mut r, i, v.to_bits());
        }
    }
    wr(cpu, mem, ins, 0, r, true)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Code, Encoder, Register as Rg};

    fn run(ins: Instruction, cpu: &mut Cpu) {
        let mut enc = Encoder::new(64);
        enc.encode(&ins, 0x1000).unwrap();
        let bytes = enc.take_buffer();
        let back = iced_x86::Decoder::with_ip(64, &bytes, 0x1000, iced_x86::DecoderOptions::NONE).decode();
        let mut mem = Mem::default();
        assert!(super::super::fp::exec(cpu, &mut mem, &back).unwrap(), "{back}");
    }

    fn i3(code: Code, a: Rg, b: Rg, c: Rg) -> Instruction {
        Instruction::with3(code, a, b, c).unwrap()
    }

    fn d2(lo: f64, hi: f64) -> u128 {
        (lo.to_bits() as u128) | ((hi.to_bits() as u128) << 64)
    }

    fn lane_d(v: u128, i: usize) -> f64 {
        f64::from_bits((v >> (64 * i)) as u64)
    }

    #[test]
    fn the_names_give_the_operand_order_the_signs_and_the_lane_size() {
        let f = form(M::Vfmadd231sd).unwrap();
        assert_eq!((f.order, f.neg, f.sign, f.dbl, f.scalar), ([1, 2, 0], false, Sign::Add, true, true));
        let f = form(M::Vfnmsub132ps).unwrap();
        assert_eq!((f.order, f.neg, f.sign, f.dbl, f.scalar), ([0, 2, 1], true, Sign::Sub, false, false));
        assert_eq!(form(M::Vfmaddsub213pd).unwrap().sign, Sign::SubEven);
        assert_eq!(form(M::Vfmsubadd231ps).unwrap().sign, Sign::SubOdd);
        // FMA4 and the rest are not FMA3
        for mn in [M::Vfmaddps, M::Vfmaddsd, M::Vaddps, M::Vfrczps, M::Fmul] {
            assert_eq!(form(mn), None, "{mn:?}");
        }
    }

    #[test]
    fn a_fused_multiply_add_rounds_once() {
        // (1 + 2^-27)(1 - 2^-27) - 1 is -2^-54 exactly; a separate multiply rounds the product to 1 and gives 0
        let (a, b) = (1.0 + 2f64.powi(-27), 1.0 - 2f64.powi(-27));
        let mut cpu = Cpu::new();
        cpu.xmm[0] = d2(-1.0, 7.0);
        cpu.xmm[1] = d2(a, 0.0);
        cpu.xmm[2] = d2(b, 0.0);
        // xmm0 = xmm1 * xmm2 + xmm0, the upper lane kept
        run(i3(Code::VEX_Vfmadd231sd_xmm_xmm_xmmm64, Rg::XMM0, Rg::XMM1, Rg::XMM2), &mut cpu);
        assert_eq!(lane_d(cpu.xmm[0], 0), -(2f64.powi(-54)));
        assert_eq!(lane_d(cpu.xmm[0], 1), 7.0);
        assert_ne!(a * b - 1.0, -(2f64.powi(-54)), "the unfused form loses it");
    }

    #[test]
    fn the_three_orders_take_their_operands_from_the_right_places() {
        for (code, want) in [
            // 132: dst * third + second
            (Code::VEX_Vfmadd132pd_xmm_xmm_xmmm128, 2.0 * 5.0 + 3.0),
            // 213: second * dst + third
            (Code::VEX_Vfmadd213pd_xmm_xmm_xmmm128, 3.0 * 2.0 + 5.0),
            // 231: second * third + dst
            (Code::VEX_Vfmadd231pd_xmm_xmm_xmmm128, 3.0 * 5.0 + 2.0),
            (Code::VEX_Vfmsub231pd_xmm_xmm_xmmm128, 3.0 * 5.0 - 2.0),
            (Code::VEX_Vfnmadd231pd_xmm_xmm_xmmm128, -(3.0 * 5.0) + 2.0),
            (Code::VEX_Vfnmsub231pd_xmm_xmm_xmmm128, -(3.0 * 5.0) - 2.0),
        ] {
            let mut cpu = Cpu::new();
            cpu.ymmh[0] = 99;
            cpu.xmm[0] = d2(2.0, 2.0);
            cpu.xmm[1] = d2(3.0, 3.0);
            cpu.xmm[2] = d2(5.0, 5.0);
            run(i3(code, Rg::XMM0, Rg::XMM1, Rg::XMM2), &mut cpu);
            assert_eq!((lane_d(cpu.xmm[0], 0), lane_d(cpu.xmm[0], 1)), (want, want), "{code:?}");
            assert_eq!(cpu.ymmh[0], 0, "a VEX write to xmm clears the upper half");
        }
    }

    #[test]
    fn ymm_single_forms_and_the_alternating_signs_cover_every_lane() {
        let mut cpu = Cpu::new();
        let fill = |x: f32| (0..4).fold(0u128, |acc, i| acc | ((x.to_bits() as u128) << (32 * i)));
        for r in [0, 1, 2] {
            cpu.xmm[r] = fill([1.0, 2.0, 10.0][r]);
            cpu.ymmh[r] = fill([1.0, 2.0, 10.0][r]);
        }
        // ymm0 = ymm1 * ymm2 -/+ ymm0, lane by lane: 20 - 1 on even lanes, 20 + 1 on odd ones
        run(i3(Code::VEX_Vfmaddsub231ps_ymm_ymm_ymmm256, Rg::YMM0, Rg::YMM1, Rg::YMM2), &mut cpu);
        let lane = |i: usize| {
            let h = if i < 4 { cpu.xmm[0] } else { cpu.ymmh[0] };
            f32::from_bits((h >> (32 * (i % 4))) as u32)
        };
        let got: Vec<f32> = (0..8).map(lane).collect();
        assert_eq!(got, [19.0, 21.0, 19.0, 21.0, 19.0, 21.0, 19.0, 21.0]);
    }

    #[test]
    fn a_nan_comes_back_quiet_and_with_its_own_sign() {
        let mut cpu = Cpu::new();
        let snan = f64::from_bits(0x7ff0_0000_0000_0001);
        cpu.xmm[0] = d2(1.0, 0.0);
        cpu.xmm[1] = d2(snan, 0.0);
        cpu.xmm[2] = d2(2.0, 0.0);
        run(i3(Code::VEX_Vfnmadd231sd_xmm_xmm_xmmm64, Rg::XMM0, Rg::XMM1, Rg::XMM2), &mut cpu);
        assert_eq!(cpu.xmm[0] as u64, 0x7ff8_0000_0000_0001);
    }
}
