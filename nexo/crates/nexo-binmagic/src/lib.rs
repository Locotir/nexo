//! The cheap question "is this file a program `nexo bin` reads?", answered from its first bytes only.
//!
//! These probes sit apart from `nexo-bin` so that the crates which only ask the question (the agent's hints)
//! do not compile the disassembler, the debug-info readers and the decompiler to do it. `nexo-bin` re-exports
//! everything here, so its own users see the same names.

mod kind;
mod macho;
mod managed;
mod probe;

pub use kind::{kind_at, kind_of, size_words, Compiled};
pub use macho::{
    be32, opens_head as macho_opens_head, u32_at, CPU_ARM64, CPU_X86_64, FAT_MAGIC, FAT_MAGIC_64, MAX_SLICES,
    MH_MAGIC_64,
};
pub use managed::{
    is_class, is_dotnet, is_managed, is_mixed_mode, looks_like_zip, pe_cli, section_offset, PeCli, HEAD as MANAGED_HEAD,
};
pub use probe::opens;

/// The four bytes every WebAssembly module starts with.
pub const WASM_MAGIC: &[u8; 4] = b"\0asm";
