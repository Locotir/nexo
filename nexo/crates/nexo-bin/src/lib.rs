//! `nexo-bin`: reads ELF and PE x86-64 files as bytes and answers questions about them in
//! bounded pieces: headers, one function's disassembly, typed data bytes, strings, pattern search.
//!
//! The file is only read. It is never run, loaded or handed to another program (`nexo-dbg` is the
//! crate that runs programs; it asks this one for names and instruction text). No function here
//! prints: each op returns an [`Output`] that has a text form and a JSON form, and
//! [`Output::page`] cuts either one to a line budget, saying exactly what is left and how to get it.

mod caps;
mod crypto;
mod decomp;
mod disasm;
mod dwarf;
mod emu;
mod fnview;
mod funcs;
mod graph;
mod hookinfo;
mod image;
mod live;
mod mainfind;
mod names;
mod ops;
mod overview;
mod page;
mod refs;
mod slice;
mod strings;
mod xops;

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_map;
#[cfg(test)]
mod tests_x;

pub use crypto::{crypto_window, scan as crypto_scan, CryptoHit};
pub use decomp::api::{decomp_window, proto_set, struct_set, var_set};
pub use emu::{emulate, limits as emu_limits, EmuSpec};
pub use fnview::{func_view, FnQuery, LEAD_IN, MAX_RANGE};
pub use graph::{path_window, tree_window};
pub use hookinfo::{arg_count, block_starts, import_sites, libc_proto, parse_proto, proto_at, ImportSite, Proto};
pub use image::{opens, Export, Format, Hardening, Image, Import, Section, Symbol};
pub use live::{
    decode_live, describe_addr, indirect_call_target, indirect_calls, nearest_symbol, place_of, LiveIns, LiveMem,
    Place, MAX_INDIRECT_CALLS,
};
pub use names::{attach as attach_names, brief as names_brief, file_path as names_path, MAX_ENTRIES as MAX_NAMES};
pub use ops::{
    bytes, find_bytes, find_imm, func, func_window, info, parse_pattern, strings, strings_window, typed_rows, ElemType,
    StrSort,
};
pub use overview::{overview, overview_window};
pub use page::{Output, Page, Row, DEFAULT_LINES, MIN_LINES};
pub use refs::{Kind, Ref, Refs, MAX_REFS};
pub use slice::slice_window;
pub use xops::{
    calls, calls_window, func_list, func_list_window, name_clear, name_set, names_list, names_list_window, note_clear,
    note_set, xref, xref_str, xref_str_window, xref_window, FuncSort,
};

use std::fmt;

/// Why an op could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file is not one this crate reads (not a binary, another format, another architecture).
    NotRead(String),
    /// The file could not be opened or read.
    Io(String),
    /// The question cannot be answered: an unknown name, an address outside the file, a bad argument.
    Bad(String),
    /// The arguments cannot be used together or point outside what the op takes (a `--to` before its start).
    Usage(String),
}

impl Error {
    /// The exit code `nexo bin` ends with: 2 for a file that is not read (or cannot be opened), 1 for a
    /// question that has no answer.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::NotRead(_) | Error::Io(_) | Error::Usage(_) => 2,
            Error::Bad(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotRead(s) | Error::Io(s) | Error::Bad(s) | Error::Usage(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A number written as `0x401000` (hex) or plain digits (decimal); `_` may separate digits. A trailing `h` is not accepted.
pub fn parse_number(s: &str) -> Result<i128> {
    let t = s.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i128::from_str_radix(&h.replace('_', ""), 16)
    } else {
        t.replace('_', "").parse::<i128>()
    };
    match v {
        Ok(v) if v.unsigned_abs() <= u64::MAX as u128 => Ok(if neg { -v } else { v }),
        _ => Err(Error::Bad(format!("{s:?} is not a number (write 0x401000 for hex, plain digits for decimal)"))),
    }
}

/// An address: a non-negative number that fits 64 bits.
pub fn parse_addr(s: &str) -> Result<u64> {
    let v = parse_number(s)?;
    u64::try_from(v).map_err(|_| Error::Bad(format!("{s:?} is not an address")))
}
