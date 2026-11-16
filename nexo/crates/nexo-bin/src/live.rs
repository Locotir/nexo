//! What a debugger needs from an image whose program is running: the place a person means, a few
//! words for an address, and the text of an instruction that was read from memory. Nothing here
//! starts, opens or touches a process: the caller reads the bytes and passes them in.

use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Formatter, InstructionInfoFactory, Mnemonic, OpAccess, OpKind, Register,
};

use crate::disasm::{describe, formatter, target};
use crate::place::{addr_in, AddrExpr};
use crate::{Image, Result};

/// One memory place an instruction touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveMem {
    /// The address, when the registers given were enough to work it out.
    pub addr: Option<u64>,
    /// Bytes touched (0 when the instruction does not say).
    pub size: usize,
    pub read: bool,
    pub write: bool,
}

/// One instruction decoded from bytes that sit at `ip` in a running program.
#[derive(Debug, Clone)]
pub struct LiveIns {
    /// Bytes the instruction takes (1 for a byte that is not an instruction).
    pub len: usize,
    pub text: String,
    pub invalid: bool,
    pub call: bool,
    pub ret: bool,
    /// A near branch target or a RIP-relative address; the flag is true for memory.
    pub target: Option<(u64, bool)>,
    /// The memory it reads or writes, the stack of `push`, `call` and `ret` included.
    pub mem: Vec<LiveMem>,
}

/// Decode the x86-64 instruction at the start of `bytes` ([`decode_live_in`] with 64).
pub fn decode_live(bytes: &[u8], ip: u64, reg: &dyn Fn(&str) -> Option<u64>) -> Option<LiveIns> {
    decode_live_in(64, bytes, ip, reg)
}

/// Decode the instruction at the start of `bytes` as code of `bits` (64 or 32). `reg` gives the value of a
/// register by its lower-case 64-bit name (`rax`, `fs_base`, `gs_base`; 32-bit code that uses `eax` asks for
/// `rax`); it may answer `None` for every one.
pub fn decode_live_in(bits: u32, bytes: &[u8], ip: u64, reg: &dyn Fn(&str) -> Option<u64>) -> Option<LiveIns> {
    if bytes.is_empty() {
        return None;
    }
    let ins = Decoder::with_ip(bits, bytes, ip, DecoderOptions::NONE).decode();
    if ins.is_invalid() {
        return Some(LiveIns {
            len: 1,
            text: "(bad)".into(),
            invalid: true,
            call: false,
            ret: false,
            target: None,
            mem: Vec::new(),
        });
    }
    let mut text = String::new();
    formatter().format(&ins, &mut text);
    let value = |r: Register, _: usize, _: usize| -> Option<u64> {
        if r.is_segment_register() {
            return match r {
                Register::FS => reg("fs_base"),
                Register::GS => reg("gs_base"),
                _ => Some(0),
            };
        }
        let full = format!("{:?}", r.full_register()).to_ascii_lowercase();
        let v = reg(&full)?;
        Some(match r.size() {
            4 => v & 0xffff_ffff,
            2 => v & 0xffff,
            1 => v & 0xff,
            _ => v,
        })
    };
    let mut factory = InstructionInfoFactory::new();
    let mem = factory
        .info(&ins)
        .used_memory()
        .iter()
        .filter_map(|m| {
            let (read, write) = match m.access() {
                OpAccess::Read | OpAccess::CondRead => (true, false),
                OpAccess::Write | OpAccess::CondWrite => (false, true),
                OpAccess::ReadWrite | OpAccess::ReadCondWrite => (true, true),
                _ => return None,
            };
            Some(LiveMem { addr: m.virtual_address(0, value), size: m.memory_size().size(), read, write })
        })
        .collect();
    let flow = ins.flow_control();
    Some(LiveIns {
        len: ins.len(),
        text,
        invalid: false,
        call: matches!(flow, FlowControl::Call | FlowControl::IndirectCall),
        ret: flow == FlowControl::Return,
        target: target(&ins),
        mem,
    })
}

/// How many indirect calls `indirect_calls` lists at most.
pub const MAX_INDIRECT_CALLS: usize = 4000;

/// The addresses (as in the file) of the instructions that call through a register or a memory slot
/// (`call rax`, `call [rbx+8]`), found by reading the code sections once from start to end. At most
/// `MAX_INDIRECT_CALLS`; the second value is how many more there were.
pub fn indirect_calls(img: &Image) -> (Vec<u64>, usize) {
    // the debugger runs x86 programs only; a 32-bit one's code is read as 32-bit code
    if img.arm64 {
        return (Vec::new(), 0);
    }
    let bits = if img.bits == 32 { 32 } else { 64 };
    let (mut out, mut more) = (Vec::new(), 0usize);
    for s in img
        .sections
        .iter()
        .filter(|s| s.exec && s.alloc && !s.name.starts_with(".plt") && s.name != ".init" && s.name != ".fini")
    {
        let bytes = img.section_bytes(s);
        let mut d = Decoder::with_ip(bits, bytes, s.addr, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            if !ins.is_invalid() && ins.flow_control() == FlowControl::IndirectCall {
                if out.len() < MAX_INDIRECT_CALLS {
                    out.push(ins.ip());
                } else {
                    more += 1;
                }
            }
        }
    }
    (out, more)
}

/// [`indirect_call_target_in`] for x86-64 code.
pub fn indirect_call_target(
    bytes: &[u8],
    ip: u64,
    reg: &dyn Fn(&str) -> Option<u64>,
    read: &dyn Fn(u64) -> Option<u64>,
) -> Option<u64> {
    indirect_call_target_in(64, bytes, ip, reg, read)
}

/// Where the indirect call at the start of `bytes` (at `ip` in the running program, code of `bits`) goes right
/// now: the register's value, or the pointer `read` gives at the slot (`read` reads 8 bytes for 64-bit code, 4
/// for 32-bit code). `None` for anything that is not an indirect call.
pub fn indirect_call_target_in(
    bits: u32,
    bytes: &[u8],
    ip: u64,
    reg: &dyn Fn(&str) -> Option<u64>,
    read: &dyn Fn(u64) -> Option<u64>,
) -> Option<u64> {
    let ins = Decoder::with_ip(bits, bytes, ip, DecoderOptions::NONE).decode();
    if ins.is_invalid() || ins.flow_control() != FlowControl::IndirectCall {
        return None;
    }
    let value = |r: Register, _: usize, _: usize| -> Option<u64> {
        if r.is_segment_register() {
            return match r {
                Register::FS => reg("fs_base"),
                Register::GS => reg("gs_base"),
                _ => Some(0),
            };
        }
        reg(&format!("{:?}", r.full_register()).to_ascii_lowercase())
    };
    match ins.op0_kind() {
        OpKind::Register => {
            let r = ins.op0_register();
            let v = value(r, 0, 0)?;
            Some(if r.size() == 4 { v & 0xffff_ffff } else { v })
        }
        OpKind::Memory => {
            read(InstructionInfoFactory::new().info(&ins).used_memory().first()?.virtual_address(0, value)?)
        }
        _ => None,
    }
}

/// What a name or a number points at in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Code in this file.
    Code(u64),
    /// An import whose code is in another file: the slot that holds its address once the program is loaded.
    ImportSlot(u64),
    /// Data in this file.
    Data(u64),
}

impl Place {
    pub fn addr(self) -> u64 {
        match self {
            Place::Code(a) | Place::ImportSlot(a) | Place::Data(a) => a,
        }
    }
}

/// The place a person means, written as [`crate::place::AddrExpr`]: a number, a name (function, symbol, import,
/// section, a name given with `nexo bin name`), either one with `+<offset>` or `-<offset>`. Addresses are the file's own.
pub fn place_of(img: &Image, target: &str) -> Result<Place> {
    let addr = addr_in(img, target)?;
    let off = matches!(AddrExpr::parse(target)?, AddrExpr::Name { off, .. } if off != 0);
    if img.is_exec(addr) {
        return Ok(Place::Code(addr));
    }
    if !off && img.imports.iter().any(|i| i.slot == Some(addr)) {
        return Ok(Place::ImportSlot(addr));
    }
    Ok(Place::Data(addr))
}

/// What a file address is, in a few words: a function and offset, an import, a string or a symbol.
/// `memory` is true when the address is read or written rather than run.
pub fn describe_addr(img: &Image, addr: u64, memory: bool) -> Option<String> {
    describe(img, addr, memory)
}

/// The symbol or export that holds `addr`, with the offset into it, without finding every function
/// of the file (a shared library is named this way).
pub fn nearest_symbol(img: &Image, addr: u64) -> Option<(String, u64)> {
    img.symbol_covering(addr).map(|(n, off)| (n.to_string(), off))
}

/// What the code of a function does to one stack slot before a stop (`slot_use`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotUse {
    /// An instruction before the stop writes to the slot.
    Stored,
    /// Nothing before the stop writes to it, takes its address or can be reached again from after the stop.
    Untouched,
    /// Its address is taken before the stop (`lea`), so a callee may have filled it.
    AddressTaken,
    /// The code could not be read or decoded, or a jump from after the stop goes back to before it.
    Unclear,
}

/// Whether the code at `[lo, hi)` (`bytes`, the program's own bytes, at `lo`) writes to the `size` bytes at
/// `slot` before `stop`. Only a function with a frame pointer is judged (`push rbp; mov rbp, rsp` at its entry,
/// after an optional `endbr64`): `rbp` is the value the caller gives, what it holds for the whole body, since at
/// the stop it may already be restored. `push`, `pop` and `call` write the stack through `rsp` and are skipped.
pub fn slot_use(bytes: &[u8], lo: u64, stop: u64, slot: u64, size: u64, rbp: u64) -> SlotUse {
    let body = bytes.strip_prefix(&[0xf3, 0x0f, 0x1e, 0xfa]).unwrap_or(bytes);
    if !(body.starts_with(&[0x55, 0x48, 0x89, 0xe5]) || body.starts_with(&[0x55, 0x48, 0x8b, 0xec])) {
        return SlotUse::Unclear;
    }
    let hit = |a: u64, n: u64| a < slot.saturating_add(size.max(1)) && slot < a.saturating_add(n.max(1));
    let value = |r: Register, _: usize, _: usize| -> Option<u64> {
        if r.is_segment_register() {
            // flat addressing; fs and gs are thread data, never a frame slot
            return (!matches!(r, Register::FS | Register::GS)).then_some(0);
        }
        let name = format!("{:?}", r.full_register()).to_ascii_lowercase();
        (name == "rbp").then_some(rbp)
    };
    let mut d = Decoder::with_ip(64, bytes, lo, DecoderOptions::NONE);
    let mut factory = InstructionInfoFactory::new();
    let mut taken = false;
    while d.can_decode() {
        let ins = d.decode();
        if ins.is_invalid() {
            return SlotUse::Unclear;
        }
        if ins.ip() >= stop {
            // a branch from here back to or before the stop could run the earlier code, and a store in the
            // code after the stop, again
            let back = matches!(ins.flow_control(), FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch)
                && matches!(ins.op0_kind(), OpKind::NearBranch64)
                && ins.near_branch_target() <= stop;
            if back {
                return SlotUse::Unclear;
            }
            continue;
        }
        if matches!(ins.mnemonic(), Mnemonic::Push | Mnemonic::Pop | Mnemonic::Call | Mnemonic::Ret) {
            continue;
        }
        for m in factory.info(&ins).used_memory() {
            let Some(a) = m.virtual_address(0, value) else { continue };
            let n = m.memory_size().size() as u64;
            match m.access() {
                OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite if hit(a, n) => {
                    return SlotUse::Stored;
                }
                OpAccess::NoMemAccess if ins.mnemonic() == Mnemonic::Lea && hit(a, 1) => taken = true,
                _ => {}
            }
        }
        if ins.has_rep_prefix()
            && matches!(
                ins.mnemonic(),
                Mnemonic::Stosb
                    | Mnemonic::Stosd
                    | Mnemonic::Stosq
                    | Mnemonic::Movsb
                    | Mnemonic::Movsd
                    | Mnemonic::Movsq
            )
        {
            // a block fill or copy: where it writes is in rdi, which is not known here
            return SlotUse::Unclear;
        }
    }
    if taken {
        SlotUse::AddressTaken
    } else {
        SlotUse::Untouched
    }
}
