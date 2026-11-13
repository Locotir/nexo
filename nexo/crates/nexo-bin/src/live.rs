//! What a debugger needs from an image whose program is running: the place a person means, a few
//! words for an address, and the text of an instruction that was read from memory. Nothing here
//! starts, opens or touches a process: the caller reads the bytes and passes them in.

use iced_x86::{Decoder, DecoderOptions, FlowControl, Formatter, InstructionInfoFactory, OpAccess, OpKind, Register};

use crate::disasm::{describe, formatter, target};
use crate::xops::target_addr;
use crate::{parse_addr, Error, Image, Result};

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

/// Decode the instruction at the start of `bytes`. `reg` gives the value of a register by its
/// lower-case 64-bit name (`rax`, `fs_base`, `gs_base`); it may answer `None` for every one.
pub fn decode_live(bytes: &[u8], ip: u64, reg: &dyn Fn(&str) -> Option<u64>) -> Option<LiveIns> {
    if bytes.is_empty() {
        return None;
    }
    let ins = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE).decode();
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
    // the debugger runs 64-bit programs only
    if img.arm64 || img.bits == 32 {
        return (Vec::new(), 0);
    }
    let (mut out, mut more) = (Vec::new(), 0usize);
    for s in img
        .sections
        .iter()
        .filter(|s| s.exec && s.alloc && !s.name.starts_with(".plt") && s.name != ".init" && s.name != ".fini")
    {
        let bytes = img.section_bytes(s);
        let mut d = Decoder::with_ip(64, bytes, s.addr, DecoderOptions::NONE);
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

/// Where the indirect call at the start of `bytes` (at `ip` in the running program) goes right now: the
/// register's value, or the 8 bytes `read` gives at the slot. `None` for anything that is not an indirect call.
pub fn indirect_call_target(
    bytes: &[u8],
    ip: u64,
    reg: &dyn Fn(&str) -> Option<u64>,
    read: &dyn Fn(u64) -> Option<u64>,
) -> Option<u64> {
    let ins = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE).decode();
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

/// The place a person means: a number, a name (function, symbol, import, a name given with
/// `nexo bin name`), or either one followed by `+<offset>`. Addresses are the file's own.
pub fn place_of(img: &Image, target: &str) -> Result<Place> {
    let t = target.trim();
    let (head, off) = match t.rsplit_once('+') {
        Some((h, o)) if !h.trim().is_empty() => match parse_addr(o) {
            Ok(n) => (h.trim(), n),
            Err(_) => (t, 0),
        },
        _ => (t, 0),
    };
    let base = target_addr(img, head)?;
    let addr =
        base.checked_add(off).ok_or_else(|| Error::Bad(format!("{t:?} is past the end of the address space")))?;
    if img.is_exec(addr) {
        return Ok(Place::Code(addr));
    }
    if off == 0 && img.imports.iter().any(|i| i.slot == Some(addr)) {
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
