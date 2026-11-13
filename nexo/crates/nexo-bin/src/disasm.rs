//! Decoding x86-64 and x86-32 and writing one line per instruction.

use iced_x86::{CodeSize, Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind};

use crate::strings::shorten;
use crate::Image;

/// Longest string shown in an instruction's comment (characters).
pub const COMMENT_STRING: usize = 60;

/// Walk `bytes` (which start at address `ip`) and call `f` for each instruction with the bytes it took.
/// A byte that is not an instruction is passed as an invalid instruction of length 1.
pub(crate) fn walk(bytes: &[u8], ip: u64, f: impl FnMut(&Instruction, &[u8])) {
    walk_bits(64, bytes, ip, f);
}

/// [`walk`] for code of `bits` (64 or 32).
pub(crate) fn walk_bits(bits: u32, bytes: &[u8], ip: u64, mut f: impl FnMut(&Instruction, &[u8])) {
    let mut dec = Decoder::with_ip(bits, bytes, ip, DecoderOptions::NONE);
    while dec.can_decode() {
        let at = dec.position();
        let ins = dec.decode();
        let mut len = ins.len();
        if ins.is_invalid() || len == 0 {
            len = 1;
            if dec.set_position(at + 1).is_err() {
                return;
            }
            dec.set_ip(ip.wrapping_add((at + 1) as u64));
        }
        let end = (at + len).min(bytes.len());
        f(&ins, &bytes[at..end]);
    }
}

pub(crate) fn formatter() -> IntelFormatter {
    let mut f = IntelFormatter::new();
    let o = f.options_mut();
    o.set_hex_prefix("0x");
    o.set_hex_suffix("");
    o.set_space_after_operand_separator(true);
    o.set_branch_leading_zeros(false);
    o.set_uppercase_hex(false);
    o.set_small_hex_numbers_in_decimal(true);
    f
}

/// The address an instruction points at: a near branch target, a RIP-relative memory operand, or in
/// 32-bit code an operand at a fixed address (`[0x804c004]`). The flag is true for memory (a slot or
/// data), false for a branch.
pub(crate) fn target(ins: &Instruction) -> Option<(u64, bool)> {
    if ins.is_invalid() {
        return None;
    }
    if ins.is_ip_rel_memory_operand() {
        return Some((ins.ip_rel_memory_address(), true));
    }
    if matches!(
        ins.flow_control(),
        FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch
    ) && matches!(ins.op0_kind(), OpKind::NearBranch64 | OpKind::NearBranch32)
    {
        return Some((ins.near_branch_target(), false));
    }
    if ins.code_size() == CodeSize::Code32 {
        if let Some((a, true)) = crate::refs::written_address(ins) {
            return Some((a, true));
        }
    }
    None
}

/// The comment for a number in 32-bit code that is the start of a string, a function or an import
/// slot (`push 0x804a008` passes a string): x86-32 code passes addresses as plain numbers.
fn imm_comment(img: &Image, ins: &Instruction) -> Option<String> {
    if ins.code_size() != CodeSize::Code32 {
        return None;
    }
    (0..ins.op_count()).filter(|&i| ins.op_kind(i) == OpKind::Immediate32).find_map(|i| {
        let v = ins.immediate(i);
        let exact = img.string_at(v).is_some_and(|(_, off)| off == 0)
            || img.slots.contains_key(&v)
            || img.func_at(v).is_some_and(|f| f.start == v);
        (v >= 0x1000 && exact).then(|| describe(img, v, false)).flatten()
    })
}

/// What the address is, in a few words: an import, a function, a string or a symbol. Empty when unknown.
pub(crate) fn describe(img: &Image, addr: u64, memory: bool) -> Option<String> {
    if let Some(n) = img.user_name(addr) {
        return Some(n.to_string());
    }
    if memory {
        if let Some(&i) = img.slots.get(&addr) {
            return Some(img.imports[i].name.clone());
        }
    }
    if !memory {
        if let Some(l) = img.label(addr) {
            return Some(l);
        }
        if let Some(&i) = img.slots.get(&addr) {
            return Some(img.imports[i].name.clone());
        }
    }
    if let Some((s, off)) = img.string_at(addr) {
        let shown = format!("{}\"{}\"", if s.wide { "L" } else { "" }, shorten(&s.text, COMMENT_STRING));
        return Some(if off > 0 { format!("{shown}+{off}") } else { shown });
    }
    if let Some((n, off)) = img.symbol_covering(addr) {
        return Some(if off == 0 { n.to_string() } else { format!("{n}+{off:#x}") });
    }
    if memory {
        if let Some(l) = img.label(addr) {
            return Some(l);
        }
    }
    img.section_at(addr).map(|s| format!("{}+{:#x}", s.name, addr - s.addr))
}

/// The comment for an instruction line, if it points at something.
pub(crate) fn comment(img: &Image, ins: &Instruction) -> Option<String> {
    match target(ins) {
        Some((addr, memory)) => describe(img, addr, memory),
        None => imm_comment(img, ins),
    }
}

#[cfg(test)]
thread_local! {
    /// Bytes of executable sections decoded on this thread (tests count passes).
    pub(crate) static DECODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`walk`] over the bytes the file holds for an executable section.
pub(crate) fn walk_code(img: &Image, sec: &crate::image::Section, f: impl FnMut(&Instruction, &[u8])) {
    let bytes = img.section_bytes(sec);
    #[cfg(test)]
    DECODED.with(|c| c.set(c.get() + bytes.len()));
    walk_bits(img.bits, bytes, sec.addr, f);
}

/// One instruction of either architecture, for the listings that show both (`fn`).
pub(crate) enum Line<'a> {
    /// With the address its memory operand names through a register of known value (x86-32
    /// position-independent code, see `pic32`).
    X86(&'a Instruction, Option<u64>),
    A64(&'a crate::a64::Ins, Option<crate::a64::scan::Via>),
}

impl Line<'_> {
    pub fn ip(&self) -> u64 {
        match self {
            Line::X86(i, _) => i.ip(),
            Line::A64(i, _) => i.pc,
        }
    }

    /// Filler between functions.
    pub fn padding(&self) -> bool {
        match self {
            Line::X86(i, _) => matches!(i.mnemonic(), iced_x86::Mnemonic::Int3 | iced_x86::Mnemonic::Nop),
            Line::A64(i, _) => i.is_padding(),
        }
    }

    pub fn text(&self, fmt: &mut IntelFormatter) -> String {
        match self {
            Line::X86(i, _) => {
                let mut s = String::new();
                fmt.format(i, &mut s);
                s
            }
            Line::A64(i, _) => i.text(),
        }
    }

    pub fn comment(&self, img: &Image) -> Option<String> {
        match self {
            Line::X86(i, Some(a)) => describe(img, *a, true).or_else(|| comment(img, i)),
            Line::X86(i, None) => comment(img, i),
            Line::A64(i, via) => crate::a64::view::comment(img, i, *via),
        }
    }
}

/// [`walk`] for the architecture of `img`.
pub(crate) fn walk_lines(img: &Image, bytes: &[u8], ip: u64, mut f: impl FnMut(&Line, &[u8])) {
    if img.arm64 {
        crate::a64::view::walk(bytes, ip, |i, via, raw| f(&Line::A64(i, via), raw));
    } else if img.bits == 32 {
        let mut pic = crate::pic32::Pic32::new();
        walk_bits(32, bytes, ip, |i, raw| {
            let named = pic.resolve(img, i).map(|x| x.0).filter(|&a| img.section_at(a).is_some());
            f(&Line::X86(i, named), raw);
            pic.step(img, i);
        });
    } else {
        walk(bytes, ip, |i, raw| f(&Line::X86(i, None), raw));
    }
}

/// The instruction at `addr` as text, or `None` when the file holds no bytes there.
pub(crate) fn text_at(img: &Image, fmt: &mut IntelFormatter, addr: u64) -> Option<String> {
    if img.arm64 {
        let b = img.read_at(addr, 4)?;
        let w: [u8; 4] = b.try_into().ok()?;
        return Some(crate::a64::decode(u32::from_le_bytes(w), addr).text());
    }
    let bytes = img.read_at(addr, 15)?;
    let ins = Decoder::with_ip(img.bits, bytes, addr, DecoderOptions::NONE).decode();
    if ins.is_invalid() {
        return Some("(bad)".into());
    }
    let mut line = String::new();
    fmt.format(&ins, &mut line);
    Some(line)
}
