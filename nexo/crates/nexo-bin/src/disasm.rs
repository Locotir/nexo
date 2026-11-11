//! Decoding x86-64 and writing one line per instruction.

use iced_x86::{Decoder, DecoderOptions, FlowControl, Formatter, Instruction, IntelFormatter, OpKind};

use crate::strings::shorten;
use crate::Image;

/// Longest string shown in an instruction's comment (characters).
pub const COMMENT_STRING: usize = 60;

/// Walk `bytes` (which start at address `ip`) and call `f` for each instruction with the bytes it took.
/// A byte that is not an instruction is passed as an invalid instruction of length 1.
pub(crate) fn walk(bytes: &[u8], ip: u64, mut f: impl FnMut(&Instruction, &[u8])) {
    let mut dec = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
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

/// The address an instruction points at: a near branch target, or a RIP-relative memory operand.
/// The flag is true for memory (a slot or data), false for a branch.
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
    ) && ins.op0_kind() == OpKind::NearBranch64
    {
        return Some((ins.near_branch_target(), false));
    }
    None
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
    let (addr, memory) = target(ins)?;
    describe(img, addr, memory)
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
    walk(bytes, sec.addr, f);
}

/// The instruction at `addr` as text, or `None` when the file holds no bytes there.
pub(crate) fn text_at(img: &Image, fmt: &mut IntelFormatter, addr: u64) -> Option<String> {
    let bytes = img.read_at(addr, 15)?;
    let ins = Decoder::with_ip(64, bytes, addr, DecoderOptions::NONE).decode();
    if ins.is_invalid() {
        return Some("(bad)".into());
    }
    let mut line = String::new();
    fmt.format(&ins, &mut line);
    Some(line)
}
