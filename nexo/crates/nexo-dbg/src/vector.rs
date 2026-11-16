//! The vector registers out of the bytes [`crate::Target::vector_state`] gives: xmm0-15, the upper halves of ymm0-15
//! where the system hands them, and the x87 stack (a 32-bit program returns a float in st0). Each backend gives its
//! own system's layout, and each one holds an FXSAVE area: st0 at byte 32, xmm0 at byte 160, 16 bytes apart.

use crate::platform::Os;

const FX_ST: usize = 32;
const FX_XMM: usize = 160;
const FX_LEN: usize = 512;
/// The XSAVE header's XSTATE_BV (which parts hold a value) after the FXSAVE area, and the standard (uncompacted)
/// place of the upper ymm halves, which is the layout ptrace hands.
const XS_BV: usize = 512;
const XS_YMM: usize = 576;
const BV_SSE: u64 = 1 << 1;
const BV_YMM: u64 = 1 << 2;
/// `x86_avx_state64`: `x86_float_state64` (524 bytes), 64 reserved bytes, then ymmh0-15.
const MAC_YMM: usize = 588;

/// Where the parts sit in one backend's bytes.
#[derive(Debug, Clone, Copy)]
struct Layout {
    fx: usize,
    /// The XSAVE header's XSTATE_BV: a part whose bit is clear holds its initial value (zero), whatever the bytes say.
    bv: Option<usize>,
    ymm: Option<usize>,
}

fn layout(os: Os, b: &[u8]) -> Option<Layout> {
    let l = match os {
        // linux.rs: a kind byte, then the XSAVE area (0) or the FXSAVE one (1)
        Os::Linux if b.first() == Some(&0) && b.len() >= 1 + XS_YMM + 256 => {
            Layout { fx: 1, bv: Some(1 + XS_BV), ymm: Some(1 + XS_YMM) }
        }
        Os::Linux => Layout { fx: 1, bv: None, ymm: None },
        // windows.rs: a whole CONTEXT, whose FltSave is the FXSAVE area
        Os::Windows => Layout { fx: 0x100, bv: None, ymm: None },
        // macos.rs: x86_avx_state64 or x86_float_state64, both after 8 reserved bytes
        Os::MacOS => Layout { fx: 8, bv: None, ymm: (b.len() >= MAC_YMM + 256).then_some(MAC_YMM) },
    };
    (b.len() >= l.fx + FX_LEN).then_some(l)
}

/// A register of the vector unit, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VReg {
    Xmm(usize),
    Ymm(usize),
    St(usize),
}

impl VReg {
    /// `xmm0` .. `xmm15`, `ymm0` .. `ymm15`, `st0` .. `st7` (also `$xmm0`, `st(0)`).
    pub fn parse(name: &str) -> Option<VReg> {
        let n = name.trim_start_matches('$').to_ascii_lowercase().replace(['(', ')'], "");
        let num = |p: &str, max: usize| n.strip_prefix(p).and_then(|d| d.parse::<usize>().ok()).filter(|&i| i < max);
        num("xmm", 16).map(VReg::Xmm).or_else(|| num("ymm", 16).map(VReg::Ymm)).or_else(|| num("st", 8).map(VReg::St))
    }
}

/// The vector registers of one thread, as the backend's bytes hold them.
#[derive(Debug, Clone)]
pub struct Vector {
    l: Layout,
    bytes: Vec<u8>,
}

impl Vector {
    /// `None` when the bytes are not the layout this system's backend gives.
    pub fn new(os: Os, bytes: Vec<u8>) -> Option<Vector> {
        Some(Vector { l: layout(os, &bytes)?, bytes })
    }

    /// The bytes to hand back to [`crate::Target::set_vector_state`].
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn bv(&self) -> u64 {
        self.l.bv.map_or(u64::MAX, |at| u64::from_le_bytes(self.bytes[at..at + 8].try_into().unwrap_or_default()))
    }

    fn part(&self, at: usize, live: u64) -> [u8; 16] {
        if self.bv() & live == 0 {
            return [0; 16];
        }
        self.bytes[at..at + 16].try_into().unwrap_or_default()
    }

    /// xmm`i`, 16 little-endian bytes.
    pub fn xmm(&self, i: usize) -> [u8; 16] {
        self.part(self.l.fx + FX_XMM + 16 * i, BV_SSE)
    }

    /// ymm`i`, 32 bytes; `None` where the system gave no upper halves.
    pub fn ymm(&self, i: usize) -> Option<[u8; 32]> {
        let hi = self.part(self.l.ymm? + 16 * i, BV_YMM);
        let mut v = [0u8; 32];
        v[..16].copy_from_slice(&self.xmm(i));
        v[16..].copy_from_slice(&hi);
        Some(v)
    }

    pub fn has_ymm(&self) -> bool {
        self.l.ymm.is_some()
    }

    /// st`i` (stack order, st0 the top) as a double; `None` when that slot of the x87 stack is empty.
    pub fn st(&self, i: usize) -> Option<f64> {
        let fx = self.l.fx;
        let top = (u16::from_le_bytes([self.bytes[fx + 2], self.bytes[fx + 3]]) >> 11 & 7) as usize;
        // FXSAVE's abridged tag word has one bit per physical register, set when it holds a value
        if self.bytes[fx + 4] >> ((top + i) & 7) & 1 == 0 {
            return None;
        }
        let at = fx + FX_ST + 16 * i;
        Some(f80_to_f64(self.bytes[at..at + 10].try_into().unwrap_or_default()))
    }

    /// Replace the value in st0; false when the x87 stack is empty there (no float is on it to replace).
    pub fn set_st0(&mut self, x: f64) -> bool {
        if self.st(0).is_none() {
            return false;
        }
        let at = self.l.fx + FX_ST;
        self.bytes[at..at + 10].copy_from_slice(&f64_to_f80(x));
        true
    }

    /// Mark a part as holding a value; the first time, its other registers are set to their initial zero so stale
    /// bytes of the buffer do not come alive.
    fn live(&mut self, base: usize, len: usize, bit: u64) {
        let Some(at) = self.l.bv else { return };
        let bv = self.bv();
        if bv & bit == 0 {
            self.bytes[base..base + len].fill(0);
            self.bytes[at..at + 8].copy_from_slice(&(bv | bit).to_le_bytes());
        }
    }

    pub fn set_xmm(&mut self, i: usize, v: [u8; 16]) {
        let base = self.l.fx + FX_XMM;
        self.live(base, 16 * 16, BV_SSE);
        self.bytes[base + 16 * i..base + 16 * i + 16].copy_from_slice(&v);
    }

    /// Set all 32 bytes of ymm`i`; false where the system gave no upper halves.
    pub fn set_ymm(&mut self, i: usize, v: [u8; 32]) -> bool {
        let Some(base) = self.l.ymm else { return false };
        self.set_xmm(i, v[..16].try_into().unwrap_or_default());
        self.live(base, 16 * 16, BV_YMM);
        self.bytes[base + 16 * i..base + 16 * i + 16].copy_from_slice(&v[16..]);
        true
    }
}

/// An x87 80-bit extended value as the nearest double.
pub fn f80_to_f64(b: [u8; 10]) -> f64 {
    let mant = u64::from_le_bytes(b[..8].try_into().unwrap_or_default());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = if se & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from(se & 0x7fff);
    if exp == 0x7fff {
        return if mant << 1 == 0 { sign * f64::INFINITY } else { f64::NAN };
    }
    if mant == 0 {
        return sign * 0.0;
    }
    // the mantissa carries its integer bit: mant / 2^63 is in [1, 2) for a normal value
    sign * (mant as f64 * pow2(-63)) * pow2(exp.max(1) - 16383)
}

/// 2 to the power `e`, exact over the whole range of a double (subnormal powers too), 0 or infinity past it.
fn pow2(e: i32) -> f64 {
    match e {
        1024.. => f64::INFINITY,
        -1022.. => f64::from_bits(((e + 1023) as u64) << 52),
        -1074.. => f64::from_bits(1 << (e + 1074)),
        _ => 0.0,
    }
}

/// A double as an x87 80-bit extended value; every double is one exactly.
pub fn f64_to_f80(x: f64) -> [u8; 10] {
    let bits = x.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp = (bits >> 52 & 0x7ff) as i32;
    let frac = bits & ((1 << 52) - 1);
    let (e, m) = match (exp, frac) {
        (0, 0) => (0, 0),
        (0x7ff, _) => (0x7fff, 1 << 63 | frac << 11),
        // a subnormal double: the wider exponent holds it normalised
        (0, _) => (15372 - frac.leading_zeros() as i32, frac << frac.leading_zeros()),
        _ => (exp - 1023 + 16383, 1 << 63 | frac << 11),
    };
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&m.to_le_bytes());
    b[8..].copy_from_slice(&(sign | e as u16).to_le_bytes());
    b
}

/// How the bytes of a vector register are read: the type of its lanes, lowest lane first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lanes {
    F64,
    F32,
    U8,
    U16,
    U32,
    U64,
    I32,
    I64,
}

/// The spellings `--as` takes.
pub const LANE_NAMES: &str = "f64|f32|u8x16|u16x8|u32x4|u64x2|i32x4|i64x2";

impl Lanes {
    pub fn parse(s: &str) -> Option<Lanes> {
        Some(match s.to_ascii_lowercase().as_str() {
            "f64" | "double" | "f64x2" | "f64x4" | "v2_double" => Lanes::F64,
            "f32" | "float" | "f32x4" | "f32x8" | "v4_float" => Lanes::F32,
            "u8" | "u8x16" | "u8x32" | "v16_int8" => Lanes::U8,
            "u16" | "u16x8" | "u16x16" | "v8_int16" => Lanes::U16,
            "u32" | "u32x4" | "u32x8" | "v4_int32" => Lanes::U32,
            "u64" | "u64x2" | "u64x4" | "v2_int64" => Lanes::U64,
            "i32" | "i32x4" | "i32x8" => Lanes::I32,
            "i64" | "i64x2" | "i64x4" => Lanes::I64,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            Lanes::U8 => 1,
            Lanes::U16 => 2,
            Lanes::F32 | Lanes::U32 | Lanes::I32 => 4,
            Lanes::F64 | Lanes::U64 | Lanes::I64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lanes::F64 => "f64",
            Lanes::F32 => "f32",
            Lanes::U8 => "u8",
            Lanes::U16 => "u16",
            Lanes::U32 => "u32",
            Lanes::U64 => "u64",
            Lanes::I32 => "i32",
            Lanes::I64 => "i64",
        }
    }

    /// One lane's bytes as text: floats as Rust prints them back exactly, unsigned lanes in hex, signed in decimal.
    fn text(self, b: &[u8]) -> String {
        let mut w = [0u8; 8];
        w[..b.len()].copy_from_slice(b);
        let v = u64::from_le_bytes(w);
        match self {
            Lanes::F64 => float_text(f64::from_bits(v)),
            Lanes::F32 => format!("{:?}", f32::from_bits(v as u32)),
            Lanes::U8 | Lanes::U16 | Lanes::U32 | Lanes::U64 => format!("{v:#x}"),
            Lanes::I32 => (v as u32 as i32).to_string(),
            Lanes::I64 => (v as i64).to_string(),
        }
    }

    /// `bytes` as `[lane0, lane1, ...]`.
    pub fn show(self, bytes: &[u8]) -> String {
        let v: Vec<String> = bytes.chunks(self.size()).map(|c| self.text(c)).collect();
        format!("[{}]", v.join(", "))
    }

    /// One lane's value from text: a float for f32/f64, a number (decimal, hex, negative) for the others.
    pub fn value(self, s: &str) -> Option<Vec<u8>> {
        let n = self.size();
        let b = match self {
            Lanes::F64 => parse_float(s)?.to_bits().to_le_bytes().to_vec(),
            Lanes::F32 => (parse_float(s)? as f32).to_bits().to_le_bytes().to_vec(),
            _ => {
                let v = match s.strip_prefix('-') {
                    Some(m) => (nexo_bin::parse_addr(m).ok()? as i64).wrapping_neg() as u64,
                    None => nexo_bin::parse_addr(s).ok()?,
                };
                v.to_le_bytes()[..n].to_vec()
            }
        };
        Some(b)
    }
}

/// A double as text that reads back to the same value: `1.5`, `3.0`, `1e300`, `NaN`, `inf`.
pub fn float_text(x: f64) -> String {
    format!("{x:?}")
}

/// A float written by a person: `1.5`, `-2`, `1e-3`, `inf`, `nan`.
pub fn parse_float(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// The 16 or 32 bytes of a register as one hex number, highest byte first.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in bytes.iter().rev() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Linux XSAVE buffer as linux.rs hands it, with xmm`i` = i in every byte and the ymm upper halves = 0x80 + i.
    fn linux_xsave(bv: u64) -> Vec<u8> {
        let mut b = vec![0u8; 1 + 1024];
        for i in 0..16 {
            b[1 + FX_XMM + 16 * i..1 + FX_XMM + 16 * i + 16].fill(i as u8);
            b[1 + XS_YMM + 16 * i..1 + XS_YMM + 16 * i + 16].fill(0x80 + i as u8);
        }
        b[1 + XS_BV..1 + XS_BV + 8].copy_from_slice(&bv.to_le_bytes());
        b
    }

    #[test]
    fn xmm_and_ymm_come_out_of_a_linux_xsave_buffer_by_its_state_bits() {
        let v = Vector::new(Os::Linux, linux_xsave(BV_SSE | BV_YMM)).unwrap();
        assert_eq!(v.xmm(3), [3; 16]);
        let y = v.ymm(5).unwrap();
        assert_eq!((&y[..16], &y[16..]), (&[5u8; 16][..], &[0x85u8; 16][..]));
        // a part whose bit is clear holds zeros, whatever stale bytes the buffer has
        let mut v = Vector::new(Os::Linux, linux_xsave(BV_SSE)).unwrap();
        assert_eq!(v.ymm(5).unwrap()[16..], [0; 16]);
        assert!(v.set_ymm(1, [7; 32]));
        assert_eq!(v.ymm(1).unwrap(), [7; 32]);
        // the other upper halves start at zero, not at the stale bytes
        assert_eq!(v.ymm(2).unwrap()[16..], [0; 16]);
        assert_eq!(v.bv() & BV_YMM, BV_YMM);
    }

    #[test]
    fn the_fxsave_only_layouts_have_xmm_and_no_ymm() {
        let mut fx = vec![1u8; 1 + 512];
        fx[1 + FX_XMM..1 + FX_XMM + 16].fill(9);
        let v = Vector::new(Os::Linux, fx).unwrap();
        assert_eq!((v.xmm(0), v.has_ymm(), v.ymm(0)), ([9; 16], false, None));
        let mut ctx = vec![0u8; 0x4d0];
        ctx[0x1a0..0x1b0].fill(4);
        assert_eq!(Vector::new(Os::Windows, ctx).unwrap().xmm(0), [4; 16]);
        let mut mac = vec![0u8; 524];
        mac[168..184].fill(6);
        assert_eq!(Vector::new(Os::MacOS, mac).unwrap().xmm(0), [6; 16]);
        assert!(Vector::new(Os::Windows, vec![0; 100]).is_none());
    }

    #[test]
    fn st0_reads_an_x87_value_and_an_empty_slot_has_none() {
        let mut fx = vec![1u8; 1 + 512];
        fx[1..].fill(0);
        // TOP = 7, its tag bit set; 1.5 is 0x3fff exponent with mantissa 0xc000_0000_0000_0000
        fx[1 + 2..1 + 4].copy_from_slice(&(7u16 << 11).to_le_bytes());
        fx[1 + 4] = 1 << 7;
        fx[1 + FX_ST..1 + FX_ST + 8].copy_from_slice(&0xc000_0000_0000_0000u64.to_le_bytes());
        fx[1 + FX_ST + 8..1 + FX_ST + 10].copy_from_slice(&0x3fffu16.to_le_bytes());
        let v = Vector::new(Os::Linux, fx).unwrap();
        assert_eq!((v.st(0), v.st(1)), (Some(1.5), None));
        for x in [1.5, -0.1, 1e300, 5e-324, 0.0, f64::INFINITY] {
            assert_eq!(f80_to_f64(f64_to_f80(x)), x, "{x}");
        }
        let mut v = v;
        assert!(v.set_st0(-2.25) && v.st(0) == Some(-2.25));
    }

    #[test]
    fn lanes_show_and_take_values() {
        let b: Vec<u8> = [2.5f64.to_bits().to_le_bytes(), (-1.0f64).to_bits().to_le_bytes()].concat();
        assert_eq!(Lanes::F64.show(&b), "[2.5, -1.0]");
        assert_eq!(Lanes::U64.show(&b), "[0x4004000000000000, 0xbff0000000000000]");
        assert_eq!(Lanes::parse("u32x4"), Some(Lanes::U32));
        assert_eq!(Lanes::F32.value("1.5"), Some(1.5f32.to_bits().to_le_bytes().to_vec()));
        assert_eq!(Lanes::I32.value("-2"), Some((-2i32).to_le_bytes().to_vec()));
        assert_eq!(Lanes::I32.show(&(-2i32).to_le_bytes()), "[-2]");
        assert_eq!(VReg::parse("$xmm15"), Some(VReg::Xmm(15)));
        assert_eq!(VReg::parse("st(0)"), Some(VReg::St(0)));
        assert_eq!(VReg::parse("xmm16"), None);
        assert_eq!(hex(&[1, 2]), "0x0201");
        assert_eq!(float_text(1e300), "1e300");
    }
}
