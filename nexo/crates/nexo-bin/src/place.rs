//! One way to write an address or a range, for every op of `nexo bin` and every command of `debug`.
//!
//! The text is parsed here into an expression ([`AddrExpr`], [`RangeExpr`]); each tool resolves it against what
//! it has. `nexo bin` resolves it against the file and answers the file's own address (the link address, the one
//! `bin` prints). The debugger resolves the same names against the same image and adds the load bias (where the
//! program was loaded minus its link address), so one symbol names the same code in both. Registers and memory
//! reads (`$rip`, `$rsp+0x20`, `*ptr`) exist only in a running program: the debugger evaluates them with its own
//! expression code, and `bin` says so.

use std::collections::HashSet;

use crate::ops::{func_at_addr, numeric, resolve};
use crate::{parse_addr, Error, Image, Result};

/// What an address may be, for error messages.
pub const ADDR_FORMS: &str = "an address is 0x401000, 4198400, a name (function, symbol, import, section like .text), \
     name+0x10 or name-0x10; puts@got and __imp_puts are the slot an import's address is written to; main and \
     entry name those places in a stripped file too";
/// What a range may be, for error messages.
pub const RANGE_FORMS: &str =
    "a range is A..B (B not included), A+LEN, or a function or section name (all of it); A and B are addresses";

/// One address as a person writes it, before it is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddrExpr {
    /// `0x401000` or decimal digits.
    Num(u64),
    /// A function, symbol, import, user or section name, with an offset (`main+0x10`, `.data-8`).
    Name { name: String, off: i64 },
    /// A register, a memory read or a cast (`$rip`, `$rsp+0x20`, `*ptr`): only a running program answers it.
    Live(String),
}

impl AddrExpr {
    pub fn parse(text: &str) -> Result<AddrExpr> {
        let t = text.trim();
        if t.is_empty() {
            return Err(Error::Bad(format!("give an address: {ADDR_FORMS}")));
        }
        if numeric(t) {
            return parse_addr(t).map(AddrExpr::Num);
        }
        if t.starts_with(['$', '*', '(']) {
            return Ok(AddrExpr::Live(t.into()));
        }
        let (name, off) = split_off(t).unwrap_or((t, 0));
        Ok(AddrExpr::Name { name: name.into(), off })
    }
}

/// A stretch of addresses as a person writes it. The ends stay text, so each tool resolves them its own way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeExpr {
    /// `A..B`: from A up to B, B not included.
    Between(String, String),
    /// `A+LEN`: LEN bytes from A.
    Len(String, u64),
    /// A function (its whole body, or the one holding a number) or a section (all of it).
    Whole(String),
}

impl RangeExpr {
    pub fn parse(text: &str) -> Result<RangeExpr> {
        let t = text.trim();
        if let Some((a, b)) = t.split_once("..") {
            let (a, b) = (a.trim(), b.trim());
            if a.is_empty() || b.is_empty() {
                return Err(Error::Bad(format!("{t:?} needs both ends: {RANGE_FORMS}")));
            }
            return Ok(RangeExpr::Between(a.into(), b.into()));
        }
        if t.is_empty() {
            return Err(Error::Bad(format!("give a range: {RANGE_FORMS}")));
        }
        if !t.starts_with(['$', '*', '(']) {
            if let Some((head, n)) = split_off(t).filter(|&(_, n)| n > 0) {
                return Ok(RangeExpr::Len(head.into(), n as u64));
            }
        }
        Ok(RangeExpr::Whole(t.into()))
    }
}

/// True when `text` is written as a range in a slot that also takes one address: only `A..B` says so there,
/// since `name+0x10` is an address.
pub fn is_span(text: &str) -> bool {
    text.contains("..")
}

/// `name+0x10` or `name-16` split at the last sign whose right side is a number.
fn split_off(t: &str) -> Option<(&str, i64)> {
    let i = t.rfind(['+', '-'])?;
    let (head, rest) = (t[..i].trim(), t[i + 1..].trim());
    if head.is_empty() || !numeric(rest) {
        return None;
    }
    let n = i64::try_from(parse_addr(rest).ok()?).ok()?;
    Some((head, if t.as_bytes()[i] == b'-' { -n } else { n }))
}

/// The file address `text` names in `img` (see [`AddrExpr`]).
pub fn addr_in(img: &Image, text: &str) -> Result<u64> {
    // the references first: one decode pass gives them and the function list
    let _ = img.refs();
    let t = text.trim();
    match AddrExpr::parse(t)? {
        AddrExpr::Num(a) => match img.section_at(a) {
            Some(_) => Ok(a),
            None => Err(Error::Bad(format!("{a:#x} is not inside any section"))),
        },
        AddrExpr::Name { name, off: 0 } => name_addr(img, &name),
        AddrExpr::Name { name, off } => {
            let base = match name_addr(img, &name) {
                Ok(a) => a,
                // a name that itself ends in +N or -N
                Err(e) => return exact_addr(img, t).ok_or(e),
            };
            base.checked_add_signed(off).ok_or_else(|| Error::Bad(format!("{t:?} is outside the address space")))
        }
        AddrExpr::Live(_) => Err(Error::Bad(format!(
            "{t:?} needs a running program (registers and memory reads work in debug); {ADDR_FORMS}"
        ))),
    }
}

/// Like [`addr_in`], but a number is taken as written: the end of a range just past a section, or memory the
/// file does not have (the emulator's stack). The op that reads it says what is wrong with it.
pub fn any_addr_in(img: &Image, text: &str) -> Result<u64> {
    match AddrExpr::parse(text)? {
        AddrExpr::Num(a) => Ok(a),
        _ => addr_in(img, text),
    }
}

/// The addresses `text` covers in `img`, end not included (see [`RangeExpr`]).
pub fn range_in(img: &Image, text: &str) -> Result<(u64, u64)> {
    let (lo, hi) = match RangeExpr::parse(text)? {
        RangeExpr::Between(a, b) => (addr_in(img, &a)?, any_addr_in(img, &b)?),
        RangeExpr::Len(a, n) => {
            let lo = addr_in(img, &a)?;
            (lo, lo.saturating_add(n))
        }
        RangeExpr::Whole(t) => match img.sections.iter().find(|s| s.name == t) {
            Some(s) => (s.addr, s.addr.saturating_add(s.size)),
            // a slot is one pointer of data, not the function its stub belongs to
            None => match known_place(img, &t) {
                Some(Known::Slot(_, slot)) => (slot, slot.saturating_add(img.ptr_size() as u64)),
                _ => {
                    let f = resolve(img, &t)?;
                    (f.start, f.end)
                }
            },
        },
    };
    if hi <= lo {
        return Err(Error::Usage(format!(
            "{:?} ends at {hi:#x}, not after its start {lo:#x}; {RANGE_FORMS}",
            text.trim()
        )));
    }
    Ok((lo, hi))
}

/// A name: a user name, a function, a symbol, an import (its PLT stub or slot), an export or a section; then
/// a function found by part of its name.
fn name_addr(img: &Image, t: &str) -> Result<u64> {
    match exact_addr(img, t) {
        Some(a) => Ok(a),
        None => resolve(img, t).map(|f| f.start),
    }
}

/// The address of a name written exactly as `img` has it.
fn exact_addr(img: &Image, t: &str) -> Option<u64> {
    if let Some((&a, _)) = img.user.names.iter().find(|(_, n)| n.as_str() == t) {
        return Some(a);
    }
    if let Some(f) = img.funcs().iter().find(|f| f.name == t) {
        return Some(f.start);
    }
    if let Some(a) = img.addr_of_raw(t) {
        if let Some(f) = img.func_at(a).filter(|f| f.start == a) {
            return Some(f.start);
        }
        return Some(a);
    }
    // an ELF import is called through its PLT stub: that is the address its call sites target
    if img.imports.iter().any(|i| i.name == t) {
        if let Some(f) = img.funcs().iter().find(|f| f.name.strip_suffix("@plt") == Some(t)) {
            return Some(f.start);
        }
    }
    if let Some(slot) = img.imports.iter().find(|i| i.name == t).and_then(|i| i.slot) {
        return Some(slot);
    }
    if let Some(s) = img.symbols.iter().find(|s| s.name == t) {
        return Some(s.addr);
    }
    if let Some(e) = img.exports.iter().find(|e| e.name == t) {
        return Some(e.addr);
    }
    if let Some(s) = img.sections.iter().find(|s| s.name == t) {
        return Some(s.addr);
    }
    known_place(img, t).map(|k| match k {
        Known::Slot(_, a) | Known::Main(a) | Known::Entry(a) => a,
    })
}

/// A place the file's tables do not name but that has a name all the same.
pub(crate) enum Known {
    /// An import's slot (`puts@got`, `__imp_ExitProcess`): the import's index and the slot's address.
    Slot(usize, u64),
    /// `main`, found from the code that starts the program.
    Main(u64),
    /// `entry`, `start` or `_start`: the entry point the file's header gives.
    Entry(u64),
}

/// The place `t` names when it is an import's slot, `main` or the entry point. A name the file itself has comes
/// first, so this is asked only after the file's own names gave nothing.
pub(crate) fn known_place(img: &Image, t: &str) -> Option<Known> {
    if let Some(i) = slot_import(img, t) {
        return img.imports[i].slot.map(|s| Known::Slot(i, s));
    }
    match t {
        "main" => main_addr(img).map(Known::Main),
        "entry" | "start" | "_start" => img.section_at(img.entry).map(|_| Known::Entry(img.entry)),
        _ => None,
    }
}

/// The import whose slot `t` names: `NAME@got` on every format, or `__imp_NAME` (the name Windows linkers give the
/// slot; `__imp__NAME` on 32-bit Windows, whose C names start with `_`).
pub(crate) fn slot_import(img: &Image, t: &str) -> Option<usize> {
    let want = t.strip_suffix("@got").or_else(|| t.strip_prefix("__imp_"))?;
    let at = |w: &str| {
        img.imports.iter().position(|i| i.slot.is_some() && (i.name == w || (!i.raw.is_empty() && i.raw == w)))
    };
    at(want).or_else(|| want.strip_prefix('_').and_then(at))
}

/// The name of an import's slot, the one [`slot_import`] reads back: `__imp_NAME` on PE, `NAME@got` elsewhere.
pub(crate) fn slot_name(img: &Image, imp: &crate::Import) -> String {
    if img.format == crate::Format::Pe {
        format!("__imp_{}", imp.name)
    } else {
        format!("{}@got", imp.name)
    }
}

/// The program's `main`: the C `main` when the start code shows one, else the first one found (`WinMain`,
/// Rust's main).
pub(crate) fn main_addr(img: &Image) -> Option<u64> {
    let hits = crate::mainfind::find_main(img);
    hits.iter().find(|h| h.role == "main").or(hits.first()).map(|h| h.addr)
}

/// The stub that jumps through import `i`'s slot (`puts@plt`; on PE a stub carries the import's own name).
pub(crate) fn stub_of(img: &Image, i: usize) -> Option<&crate::funcs::Func> {
    let imp = &img.imports[i];
    if img.format == crate::Format::Pe {
        img.funcs().iter().find(|f| f.name == imp.name && Some(f.start) != imp.slot)
    } else {
        img.funcs().iter().find(|f| f.name.strip_suffix("@plt") == Some(imp.name.as_str()))
    }
}

/// The function `text` points into, for an address written with an offset, a section or a register.
pub(crate) fn func_of<'a>(img: &'a Image, text: &str) -> Result<&'a crate::funcs::Func> {
    func_at_addr(img, addr_in(img, text)?)
}

/// Up to three names of `img` closest to `t` (by spelling, or that contain it), for an unknown name.
pub fn close_names(img: &Image, t: &str) -> Vec<String> {
    let want = t.to_ascii_lowercase();
    let limit = (want.chars().count() / 3).max(2);
    let mut seen = HashSet::new();
    let mut best: Vec<(usize, String)> = Vec::new();
    let funcs = img.funcs();
    let names = funcs
        .iter()
        .map(|f| img.func_name(f))
        .chain(img.symbols.iter().map(|s| s.name.as_str()))
        .chain(img.exports.iter().map(|e| e.name.as_str()))
        .chain(img.imports.iter().map(|i| i.name.as_str()))
        .chain(img.sections.iter().map(|s| s.name.as_str()))
        .chain(img.user.names.values().map(String::as_str));
    for n in names {
        if n.is_empty() || n.len() > 160 || n == t || !seen.insert(n) {
            continue;
        }
        let l = n.to_ascii_lowercase();
        let d = if want.len() >= 3 && l.contains(&want) {
            1 + (l.len() - want.len()) / 16
        } else if l.len().abs_diff(want.len()) > limit {
            continue;
        } else {
            distance(&want, &l)
        };
        if d <= limit {
            best.push((d, n.to_string()));
        }
    }
    best.sort();
    best.into_iter().take(3).map(|(_, n)| n).collect()
}

/// `"; close names: a, b"`, or nothing when no name is close.
pub(crate) fn close_note(img: &Image, t: &str) -> String {
    let c = close_names(img, t);
    if c.is_empty() {
        String::new()
    } else {
        format!("; close names: {}", c.join(", "))
    }
}

/// Edit distance (insert, delete, change one character).
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (prev + usize::from(ca != cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}
