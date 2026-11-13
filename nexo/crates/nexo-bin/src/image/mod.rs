//! Opening a file: headers, sections, symbols, imports, exports, hardening. Bytes only.

mod load;
mod load_elf;
mod load_pe;
mod names;
mod open;
mod probe;
mod query;

pub(crate) use load::blank;
#[cfg(test)]
pub(crate) use load_elf::reloc_pointer;
pub(crate) use names::{demangle, raw_of};
pub use probe::opens;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::funcs::Func;
use crate::names::UserNames;
use crate::refs::Refs;
use crate::strings::Str;

/// The largest file read (bytes).
const MAX_FILE: u64 = 1 << 30;
const MAX_SECTIONS: usize = 4096;
const MAX_SYMBOLS: usize = 500_000;
const MAX_IMPORTS: usize = 200_000;
/// The most dynamic relocations read.
const MAX_RELOCS: usize = 4_000_000;
const MAX_NAME: usize = 256;
/// Longest raw (mangled) name that is demangled.
const MAX_NAME_IN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Elf,
    Pe,
    MachO,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Elf => "ELF64",
            Format::Pe => "PE32+",
            Format::MachO => "Mach-O 64-bit",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub index: usize,
    pub name: String,
    /// Virtual address (with the image base for PE).
    pub addr: u64,
    /// Size in memory.
    pub size: u64,
    /// Where the bytes start in the file, and how many the file holds (0 for .bss).
    pub file_off: u64,
    pub file_size: u64,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    /// Loaded into memory (ELF sections without the ALLOC flag, like debug info, are not).
    pub alloc: bool,
}

impl Section {
    /// `rwx` letters, `-` for a flag that is off.
    pub fn flags(&self) -> String {
        format!(
            "{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.exec { 'x' } else { '-' }
        )
    }

    /// How many bytes at `addr` the file holds, as an end address.
    pub(crate) fn backed_len(&self) -> u64 {
        self.size.min(self.file_size)
    }

    pub(crate) fn contains(&self, addr: u64) -> bool {
        addr >= self.addr && addr - self.addr < self.size
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    /// The name as the file writes it, when `name` is the demangled form of it (empty otherwise).
    pub raw: String,
    pub addr: u64,
    pub size: u64,
    pub func: bool,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub lib: Option<String>,
    pub name: String,
    /// The mangled name, when `name` is its demangled form (empty otherwise).
    pub raw: String,
    /// The address of the slot the loader fills (GOT entry or IAT entry), when known.
    pub slot: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub name: String,
    /// The mangled name, when `name` is its demangled form (empty otherwise).
    pub raw: String,
    pub addr: u64,
}

/// Hardening read from the headers: ordered (name, value) pairs.
#[derive(Debug, Clone, Default)]
pub struct Hardening {
    pub items: Vec<(&'static str, String)>,
}

pub struct Image {
    pub path: PathBuf,
    pub format: Format,
    /// AArch64 code (the default is x86-64).
    pub arm64: bool,
    /// 32 for x86-32 code (ELF32 i386, PE32), 64 otherwise: the width of an address and of the code.
    pub bits: u32,
    pub entry: u64,
    pub image_base: u64,
    pub sections: Vec<Section>,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub hardening: Hardening,
    pub sha256: String,
    pub(crate) data: Vec<u8>,
    /// Exact-address names: function symbols, exports, then data symbols.
    pub(crate) names: BTreeMap<u64, String>,
    /// Size of the symbol each entry of `names` came from (0 for an export).
    sizes: HashMap<u64, u64>,
    /// Mangled names by address, for the names that were demangled (so the raw name can still be searched).
    pub(crate) raws: HashMap<u64, String>,
    /// Import slot address to index in `imports`.
    pub(crate) slots: HashMap<u64, usize>,
    /// PE exception directory (RVA, size).
    pub(crate) exception_dir: Option<(u64, u64)>,
    /// ELF32 x86: the address the PLT and position-independent code keep in a register (DT_PLTGOT, the
    /// start of `.got.plt`), so `[ebx+0xc]` in a PLT stub names its import slot.
    pub(crate) got: Option<u64>,
    sorted: Vec<usize>,
    /// Pointers the loader writes (ELF relative relocations): (slot, value).
    pub(crate) rel_ptrs: Vec<(u64, u64)>,
    /// Names and notes the person gave (see `names`).
    pub(crate) user: UserNames,
    funcs: OnceLock<Vec<Func>>,
    strings: OnceLock<Vec<Str>>,
    refs: OnceLock<Refs>,
    dwarf: OnceLock<crate::dwarf::Dwarf>,
    /// Library signatures added to the shipped ones (see `sigs`).
    pub(crate) sigs: crate::sigs::SigSet,
    /// Things wrong with the file that did not stop it from opening (cut short, a table that points out).
    pub warnings: Vec<String>,
    /// What the .pdb next to a PE file gave (names, lines, types), when there is one.
    pub(crate) pdb: Option<crate::pdbread::Pdb>,
    /// What a Mach-O file says beyond the common parts (kind, LC_MAIN, slices, libraries).
    pub(crate) macho: Option<Box<crate::macho::Macho>>,
}
