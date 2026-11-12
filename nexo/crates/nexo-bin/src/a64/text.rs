//! Writing an instruction as a line of assembly, in the usual `mnemonic op, op, op` form.

use super::{Ins, Mem, Opd};

/// A mnemonic kept inline, so decoding allocates nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mn {
    buf: [u8; 14],
    len: u8,
}

impl Mn {
    pub fn new(s: &str) -> Mn {
        let mut m = Mn { buf: [0; 14], len: 0 };
        m.push(s);
        m
    }

    pub fn push(&mut self, s: &str) -> Mn {
        for &b in s.as_bytes() {
            if (self.len as usize) < self.buf.len() {
                self.buf[self.len as usize] = b;
                self.len += 1;
            }
        }
        *self
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }
}

const CONDS: [&str; 16] =
    ["eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al", "nv"];
const SHIFTS: [&str; 5] = ["lsl", "lsr", "asr", "ror", "msl"];
const EXTS: [&str; 8] = ["uxtb", "uxth", "uxtw", "uxtx", "sxtb", "sxth", "sxtw", "sxtx"];
const ARR: [&str; 8] = ["8b", "16b", "4h", "8h", "2s", "4s", "1d", "2d"];

pub(crate) fn cond_name(c: u8) -> &'static str {
    CONDS[(c & 15) as usize]
}

fn hex(v: i64) -> String {
    if v < 0 {
        format!("#-{:#x}", v.unsigned_abs())
    } else {
        format!("#{v:#x}")
    }
}

fn reg(n: u8, x: bool, sp: bool) -> String {
    match (n, sp) {
        (31, true) => (if x { "sp" } else { "wsp" }).into(),
        (31, false) => (if x { "xzr" } else { "wzr" }).into(),
        _ => format!("{}{n}", if x { 'x' } else { 'w' }),
    }
}

fn fpreg(n: u8, sz: u8) -> String {
    format!("{}{n}", ['b', 'h', 's', 'd', 'q'][(sz as usize).min(4)])
}

fn memtext(m: Mem) -> String {
    let b = |n: u8| reg(n, true, true);
    match m {
        Mem::Off { base, off: 0 } => format!("[{}]", b(base)),
        Mem::Off { base, off } => format!("[{}, #{off}]", b(base)),
        Mem::Pre { base, off } => format!("[{}, #{off}]!", b(base)),
        Mem::Post { base, off } => format!("[{}], #{off}", b(base)),
        Mem::PostReg { base, idx } => format!("[{}], {}", b(base), reg(idx, true, false)),
        Mem::Reg { base, idx, ext, shift } => {
            let wide = ext & 1 == 1;
            let name = match ext {
                2 => "uxtw",
                3 => "lsl",
                6 => "sxtw",
                _ => "sxtx",
            };
            let r = reg(idx, wide, false);
            match (name, shift) {
                ("lsl", 0) => format!("[{}, {r}]", b(base)),
                (n, 0) => format!("[{}, {r}, {n}]", b(base)),
                (n, s) => format!("[{}, {r}, {n} #{s}]", b(base)),
            }
        }
    }
}

fn float(v: f64) -> String {
    format!("#{v:.8}")
}

fn operand(o: &Opd) -> Option<String> {
    Some(match *o {
        Opd::None => return None,
        Opd::Gpr { n, x, sp } => reg(n, x, sp),
        Opd::Fp { n, sz } => fpreg(n, sz),
        Opd::List { n, count, arr } => {
            let items: Vec<String> = (0..count)
                .map(|k| {
                    format!(
                        "v{}.{}",
                        (n + k) % 32,
                        if arr >= 16 { ["b", "h", "s", "d"][(arr - 16) as usize & 3] } else { ARR[arr as usize & 7] }
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Opd::Imm(v) => hex(v),
        Opd::UImm(v) => format!("#{v:#x}"),
        Opd::Dec(v) => format!("#{v}"),
        Opd::FImm(v) => float(v),
        Opd::Target(a) => format!("{a:#x}"),
        Opd::Mem(m) => memtext(m),
        Opd::Cond(c) => cond_name(c).to_string(),
        Opd::Shift { kind, amt } => format!("{} #{amt}", SHIFTS[(kind as usize).min(4)]),
        Opd::Ext { kind, amt, show } => {
            let name = if kind == 8 { "lsl" } else { EXTS[kind as usize & 7] };
            if amt == 0 && !show {
                if kind == 8 {
                    return None;
                }
                name.to_string()
            } else {
                format!("{name} #{amt}")
            }
        }
        Opd::Name(s) => s.to_string(),
        Opd::Sys(k) => sysreg_name(k),
    })
}

impl Ins {
    /// The mnemonic with its condition: `b.eq`.
    pub fn mnemonic(&self) -> String {
        if self.cc != 0xff {
            format!("{}.{}", self.mn.as_str(), cond_name(self.cc))
        } else {
            self.mn.as_str().to_string()
        }
    }

    /// The operands, comma-separated.
    pub fn operands(&self) -> String {
        let parts: Vec<String> = self.ops.iter().filter_map(operand).collect();
        parts.join(", ")
    }

    /// The whole instruction as text: `ldr x0, [x1, #8]`.
    pub fn text(&self) -> String {
        let ops = self.operands();
        if ops.is_empty() {
            self.mnemonic()
        } else {
            format!("{} {ops}", self.mnemonic())
        }
    }
}

fn sysreg_name(k: u16) -> String {
    let (o0, op1, crn, crm, op2) = ((k >> 14) & 1, (k >> 11) & 7, (k >> 7) & 15, (k >> 3) & 15, k & 7);
    let known = match (o0, op1, crn, crm, op2) {
        (1, 3, 4, 2, 0) => "nzcv",
        (1, 3, 4, 4, 0) => "fpcr",
        (1, 3, 4, 4, 1) => "fpsr",
        (1, 3, 13, 0, 2) => "tpidr_el0",
        (1, 3, 13, 0, 3) => "tpidrro_el0",
        (1, 3, 14, 0, 0) => "cntfrq_el0",
        (1, 3, 14, 0, 1) => "cntpct_el0",
        (1, 3, 14, 0, 2) => "cntvct_el0",
        (1, 3, 0, 0, 1) => "ctr_el0",
        (1, 3, 0, 0, 7) => "dczid_el0",
        (1, 0, 0, 0, 0) => "midr_el1",
        (1, 0, 0, 0, 5) => "mpidr_el1",
        _ => "",
    };
    if known.is_empty() {
        format!("s{}_{op1}_c{crn}_c{crm}_{op2}", 2 + o0)
    } else {
        known.to_string()
    }
}
