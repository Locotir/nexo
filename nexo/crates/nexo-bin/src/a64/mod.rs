//! AArch64 (ARM64) instruction decoder in plain Rust: the base integer instructions, loads and
//! stores, branches, system instructions and scalar floating point, which is what compilers emit
//! for ordinary C, C++ and Rust. Anything else (most Advanced SIMD, SVE, crypto) comes back as an
//! undecoded word (`.inst 0x...`) with the reason, never as a guess.
//!
//! The decoder is a pure function of one 32-bit word and its address. Text is built only on demand.

mod dp;
mod fp;
mod ls;
mod text;

pub(crate) mod scan;
pub(crate) mod view;

use text::Mn;

/// What an instruction does to the flow of control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Seq,
    /// `bl`: a call to a known address.
    Call,
    /// `b`: an unconditional jump to a known address.
    Jump,
    /// `b.cond`, `cbz`, `cbnz`, `tbz`, `tbnz`.
    Cond,
    /// `blr`: a call through a register.
    CallReg,
    /// `br`: a jump through a register.
    JumpReg,
    /// `ret`, `eret`.
    Ret,
    /// `udf`, `brk`, `hlt`: execution stops here.
    Trap,
}

/// One operand.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Opd {
    None,
    /// A general register; `n == 31` is the zero register, or the stack pointer when `sp`.
    Gpr {
        n: u8,
        x: bool,
        sp: bool,
    },
    /// A floating-point/SIMD register of `sz` (0 b, 1 h, 2 s, 3 d, 4 q).
    Fp {
        n: u8,
        sz: u8,
    },
    /// A list of `count` vector registers starting at `n` (`{v0.16b, v1.16b}`).
    List {
        n: u8,
        count: u8,
        arr: u8,
    },
    /// An immediate written in hex (`#0x10`, negative as `#-0x10`).
    Imm(i64),
    /// An unsigned immediate (logical masks), shown in hex.
    UImm(u64),
    /// An immediate written in decimal (`#16`).
    Dec(i64),
    /// A floating-point immediate (`#1.0000000`).
    FImm(f64),
    /// An address (branch target, `adr`, literal).
    Target(u64),
    Mem(Mem),
    Cond(u8),
    /// `lsl #3`, `asr #1`: shift kind 0 lsl, 1 lsr, 2 asr, 3 ror, 4 msl.
    Shift {
        kind: u8,
        amt: u8,
    },
    /// `uxtw #2`: extend kind 0..7 (uxtb uxth uxtw uxtx sxtb sxth sxtw sxtx), `lsl` alias when `lsl`.
    Ext {
        kind: u8,
        amt: u8,
        show: bool,
    },
    Name(&'static str),
    /// A system register: bits 19..5 of the instruction (o0, op1, CRn, CRm, op2).
    Sys(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mem {
    /// `[xn, #off]` (offset 0 prints `[xn]`).
    Off {
        base: u8,
        off: i64,
    },
    Pre {
        base: u8,
        off: i64,
    },
    Post {
        base: u8,
        off: i64,
    },
    /// `[xn, xm]`, `[xn, xm, lsl #3]`, `[xn, wm, sxtw #2]`.
    Reg {
        base: u8,
        idx: u8,
        ext: u8,
        shift: u8,
    },
    /// `[xn], xm` (SIMD structure loads).
    PostReg {
        base: u8,
        idx: u8,
    },
}

/// A memory access with a plain base and offset: what the page tracker needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemUse {
    pub base: u8,
    pub off: i64,
    pub store: bool,
}

/// Why a word is not decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    Simd,
    Sve,
    Reserved,
    Other,
}

impl Why {
    pub fn word(self) -> &'static str {
        match self {
            Why::Simd => "Advanced SIMD or crypto",
            Why::Sve => "SVE or SME",
            Why::Reserved => "reserved or undefined encoding",
            Why::Other => "not in the decoded subset",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Ins {
    pub pc: u64,
    pub raw: u32,
    pub mn: Mn,
    /// Condition code appended to the mnemonic (`b.eq`), or 0xff.
    pub cc: u8,
    pub ops: [Opd; 5],
    pub flow: Flow,
    /// The destination of a direct branch.
    pub target: Option<u64>,
    /// `adrp`: register and page address.
    pub adrp: Option<(u8, u64)>,
    /// `adr`: register and address. A literal load's address is `target_data`.
    pub adr: Option<(u8, u64)>,
    /// A PC-relative literal the instruction loads from (`ldr x0, =label`).
    pub literal: Option<u64>,
    /// `add rd, rn, #imm`: (rd, rn, imm).
    pub addimm: Option<(u8, u8, u64)>,
    pub mem: Option<MemUse>,
    /// Bit n set: x<n> is written (0..=30).
    pub writes: u32,
    /// Why the word is not decoded, when it is not.
    pub undecoded: Option<Why>,
}

impl Ins {
    fn new(pc: u64, raw: u32) -> Ins {
        Ins {
            pc,
            raw,
            mn: Mn::new(""),
            cc: 0xff,
            ops: [Opd::None; 5],
            flow: Flow::Seq,
            target: None,
            adrp: None,
            adr: None,
            literal: None,
            addimm: None,
            mem: None,
            writes: 0,
            undecoded: None,
        }
    }

    pub fn is_decoded(&self) -> bool {
        self.undecoded.is_none()
    }

    /// `nop`, the padding compilers leave between functions (and `udf #0` zero words).
    pub fn is_padding(&self) -> bool {
        self.raw == 0xd503_201f || self.raw == 0
    }

    /// Whether control never reaches the next instruction from here.
    pub fn ends_block(&self) -> bool {
        matches!(self.flow, Flow::Jump | Flow::JumpReg | Flow::Ret | Flow::Trap)
    }
}

pub(crate) fn bits(raw: u32, hi: u32, lo: u32) -> u32 {
    let w = hi - lo + 1;
    if w >= 32 {
        raw
    } else {
        (raw >> lo) & ((1u32 << w) - 1)
    }
}

pub(crate) fn sext(v: u64, width: u32) -> i64 {
    let s = 64 - width;
    ((v << s) as i64) >> s
}

pub(crate) fn gpr(n: u32, x: bool) -> Opd {
    Opd::Gpr { n: n as u8, x, sp: false }
}

pub(crate) fn gsp(n: u32, x: bool) -> Opd {
    Opd::Gpr { n: n as u8, x, sp: true }
}

/// Decode one word at `pc`.
pub(crate) fn decode(raw: u32, pc: u64) -> Ins {
    let mut i = Ins::new(pc, raw);
    let ok = match bits(raw, 28, 25) {
        0b1000 | 0b1001 => dp::imm(&mut i),
        0b1010 | 0b1011 => dp::branch(&mut i),
        0b0100 | 0b0110 | 0b1100 | 0b1110 => ls::decode(&mut i),
        0b0101 | 0b1101 => dp::reg(&mut i),
        0b0111 | 0b1111 => fp::decode(&mut i),
        0b0010 | 0b0011 => Err(Why::Sve),
        _ => {
            // 0000: reserved space; the zero word is `udf #0`
            if bits(raw, 31, 16) == 0 {
                i.mn = Mn::new("udf");
                i.ops[0] = Opd::Imm(i64::from(raw & 0xffff));
                i.flow = Flow::Trap;
                Ok(())
            } else {
                Err(Why::Reserved)
            }
        }
    };
    if let Err(why) = ok {
        let mut u = Ins::new(pc, raw);
        u.mn = Mn::new(".inst");
        u.ops[0] = Opd::Target(u64::from(raw));
        u.undecoded = Some(why);
        return u;
    }
    i
}

pub(crate) type R = Result<(), Why>;

#[cfg(test)]
pub(crate) mod fixture;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_img;
