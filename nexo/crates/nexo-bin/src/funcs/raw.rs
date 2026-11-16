//! Where functions start and stop in bare code (`--raw`), which has no symbols, unwind tables or section ends to
//! say it. A start is a prologue right after code that cannot fall through (a `ret`, a jump, padding); an end is
//! the last instruction no jump of the function reaches past, plus the padding that runs up to the next function.

use std::collections::HashSet;

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic};

use crate::a64::Flow;
use crate::disasm::target;
use crate::Image;

/// Filler compilers put between functions: `nop` of any length, `int3`, and zero bytes (`add [rax], al`).
pub(super) fn x86_padding(ins: &Instruction, bytes: &[u8]) -> bool {
    matches!(ins.mnemonic(), Mnemonic::Nop | Mnemonic::Int3) || bytes == [0, 0]
}

/// An instruction after which the next one is not reached by falling through.
pub(super) fn x86_ends(ins: &Instruction) -> bool {
    ins.is_invalid()
        || ins.mnemonic() == Mnemonic::Hlt
        || matches!(
            ins.flow_control(),
            FlowControl::Return
                | FlowControl::UnconditionalBranch
                | FlowControl::IndirectBranch
                | FlowControl::Exception
        )
}

/// Bytes that open a function: `endbr64`/`endbr32`, `push rbp; mov rbp, rsp` (or the 32-bit form), in 32-bit code
/// `lea ecx, [esp+4]` (gcc's `main` that realigns the stack) and in 64-bit code `sub rsp, N`. A block after an early
/// `ret` that a conditional branch reaches is not a start; the caller checks that.
pub(super) fn x86_prologue(img: &Image, at: u64) -> bool {
    let Some(b) = img.read_at(at, 4) else { return false };
    let starts = |p: &[u8]| b.starts_with(p);
    if starts(&[0xf3, 0x0f, 0x1e, 0xfa]) || starts(&[0xf3, 0x0f, 0x1e, 0xfb]) {
        return true;
    }
    if img.bits == 32 {
        return starts(&[0x55, 0x89, 0xe5]) || starts(&[0x55, 0x8b, 0xec]) || starts(&[0x8d, 0x4c, 0x24, 0x04]);
    }
    starts(&[0x55, 0x48, 0x89, 0xe5])
        || starts(&[0x55, 0x48, 0x8b, 0xec])
        || starts(&[0x48, 0x83, 0xec])
        || starts(&[0x48, 0x81, 0xec])
}

/// Prologue starts found during one linear pass over a section of bare code, leaving out the targets of conditional
/// branches (a block after an early `ret`); an unconditional jump to a prologue is a tail call, so its target stays.
#[derive(Default)]
pub(super) struct X86Starts {
    /// The last instruction ended the code before it, or was padding after such an end.
    open: bool,
    after_call: bool,
    jumped: HashSet<u64>,
}

impl X86Starts {
    pub(super) fn new() -> X86Starts {
        X86Starts { open: true, ..X86Starts::default() }
    }

    /// The start this instruction opens, if any.
    pub(super) fn step(&mut self, img: &Image, ins: &Instruction, bytes: &[u8]) -> Option<u64> {
        let pad = x86_padding(ins, bytes);
        let found =
            (self.open && !pad && !self.jumped.contains(&ins.ip()) && x86_prologue(img, ins.ip())).then(|| ins.ip());
        if ins.flow_control() == FlowControl::ConditionalBranch {
            if let Some((t, false)) = target(ins) {
                self.jumped.insert(t);
            }
        }
        // padding after a call that does not return (exit, abort) also closes the code before it
        self.open = x86_ends(ins) || (pad && (self.open || self.after_call));
        self.after_call = ins.flow_control() == FlowControl::Call;
        found
    }
}

/// Where the x86 function at `start` ends, at most `limit` (the next start or the end of the bytes).
pub(super) fn x86_code_end(img: &Image, start: u64, limit: u64) -> u64 {
    let Some(bytes) = img.read_at(start, (limit - start) as usize) else { return limit };
    let mut dec = Decoder::with_ip(img.bits, bytes, start, DecoderOptions::NONE);
    let mut ins = Instruction::default();
    let mut reach = start;
    // just past the instruction that ended the code, while only padding has followed it
    let mut ended: Option<u64> = None;
    while dec.can_decode() {
        let at = dec.position();
        dec.decode_out(&mut ins);
        if ins.is_invalid() {
            // bytes that are no instruction: the code before them is the function
            return ended.unwrap_or(if ins.ip() > start { ins.ip() } else { limit });
        }
        let b = &bytes[at..(at + ins.len()).min(bytes.len())];
        if let Some(e) = ended {
            if x86_padding(&ins, b) {
                continue;
            }
            return e;
        }
        if matches!(ins.flow_control(), FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch) {
            if let Some((t, false)) = target(&ins) {
                if t > reach && t < limit {
                    reach = t;
                }
            }
        }
        let next = ins.next_ip();
        let rest = &bytes[(at + ins.len()).min(bytes.len())..];
        // a call that `int3` or zero bytes follow does not return (exit, abort, __stack_chk_fail)
        let no_return =
            ins.flow_control() == FlowControl::Call && (rest.first() == Some(&0xcc) || rest.starts_with(&[0, 0]));
        if next > reach && (x86_ends(&ins) || no_return) {
            ended = Some(next);
        }
    }
    limit
}

/// Where the AArch64 function at `start` ends, at most `limit`. Words the decoder does not know (most SIMD) are
/// still code here: only a branch that does not fall through, past every branch target, ends a function.
pub(super) fn a64_code_end(img: &Image, start: u64, limit: u64) -> u64 {
    let Some(bytes) = img.read_at(start, (limit - start) as usize) else { return limit };
    let mut reach = start;
    let mut ended: Option<u64> = None;
    for (k, w) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let pc = start + 4 * k as u64;
        let ins = crate::a64::decode(u32::from_le_bytes(*w), pc);
        if let Some(e) = ended {
            if ins.is_padding() {
                continue;
            }
            return e;
        }
        if matches!(ins.flow, Flow::Jump | Flow::Cond) {
            if let Some(t) = ins.target.filter(|&t| t > reach && t < limit) {
                reach = t;
            }
        }
        if pc + 4 > reach && ins.ends_block() {
            ended = Some(pc + 4);
        }
    }
    limit
}
