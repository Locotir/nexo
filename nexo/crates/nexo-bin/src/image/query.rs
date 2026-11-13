//! What an opened file answers: sections, addresses, names, functions, strings, references.

use super::{Format, Image, Section};
use crate::funcs::Func;
use crate::refs::Refs;
use crate::strings::Str;
use crate::{Error, Result};

impl Image {
    /// `file.c:12` when a line of source starts exactly at `addr` (needs a .pdb).
    pub fn src_line(&self, addr: u64) -> Option<String> {
        let p = self.pdb.as_ref()?;
        let i = p.lines.binary_search_by_key(&addr, |l| l.addr).ok()?;
        let l = p.lines[i];
        let f = p.files.get(l.file as usize)?;
        Some(format!("{}:{}", f.rsplit(['\\', '/']).next().unwrap_or(f), l.line))
    }

    /// The loaded section that holds `addr`.
    pub fn section_at(&self, addr: u64) -> Option<&Section> {
        let n = self.sorted.partition_point(|&i| self.sections[i].addr <= addr);
        // sections may overlap (ELF .tbss): look a few back
        self.sorted[..n].iter().rev().take(4).map(|&i| &self.sections[i]).find(|s| s.contains(addr))
    }

    /// The bytes of a section the file holds.
    pub fn section_bytes(&self, s: &Section) -> &[u8] {
        let end = s.file_off.saturating_add(s.backed_len()).min(self.data.len() as u64);
        let start = s.file_off.min(end);
        &self.data[start as usize..end as usize]
    }

    /// Address to file offset, when the file holds a byte for it.
    pub fn addr_to_offset(&self, addr: u64) -> Option<u64> {
        let s = self.section_at(addr)?;
        let d = addr - s.addr;
        (d < s.backed_len() && s.file_off.checked_add(d)? < self.data.len() as u64).then(|| s.file_off + d)
    }

    /// File offset to address (the first loaded section that holds the offset).
    pub fn offset_to_addr(&self, off: u64) -> Option<u64> {
        self.sections.iter().filter(|s| s.alloc && s.file_size > 0).find_map(|s| {
            let end = s.file_off.checked_add(s.backed_len())?;
            (off >= s.file_off && off < end).then(|| s.addr + (off - s.file_off))
        })
    }

    /// Up to `len` bytes at `addr`, as many as the file holds in that one section. `None` when none.
    pub fn read_at(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let s = self.section_at(addr)?;
        let bytes = self.section_bytes(s);
        let d = (addr - s.addr) as usize;
        if d >= bytes.len() {
            return None;
        }
        let end = d.saturating_add(len).min(bytes.len());
        Some(&bytes[d..end])
    }

    /// The name of a symbol, export or import slot that sits exactly at `addr`.
    pub fn name_at(&self, addr: u64) -> Option<&str> {
        self.names
            .get(&addr)
            .map(String::as_str)
            .or_else(|| self.slots.get(&addr).map(|&i| self.imports[i].name.as_str()))
    }

    /// The mangled name written in the file for the name at `addr`, when it was demangled.
    pub fn raw_name_at(&self, addr: u64) -> Option<&str> {
        self.raws.get(&addr).map(String::as_str)
    }

    /// The address whose mangled name is exactly `raw` (a symbol, an export or an import slot).
    pub(crate) fn addr_of_raw(&self, raw: &str) -> Option<u64> {
        if !raw.starts_with('_') && !raw.starts_with('?') {
            return None;
        }
        self.raws.iter().filter(|(_, r)| r.as_str() == raw).map(|(a, _)| *a).min()
    }

    /// The nearest named data or code symbol at or below `addr` and inside its size.
    pub(crate) fn symbol_covering(&self, addr: u64) -> Option<(&str, u64)> {
        let (a, n) = self.names.range(..=addr).next_back()?;
        let size = self.sizes.get(a).copied().unwrap_or(0);
        (addr == *a || addr - a < size).then(|| (n.as_str(), addr - a))
    }

    pub fn is_exec(&self, addr: u64) -> bool {
        self.section_at(addr).is_some_and(|s| s.exec)
    }

    /// The functions found (see the lane notes): sorted, no duplicates.
    pub fn funcs(&self) -> &[Func] {
        self.funcs.get_or_init(|| crate::funcs::find(self))
    }

    /// Names and types the file carries as DWARF (empty when it has none).
    pub(crate) fn dwarf(&self) -> &crate::dwarf::Dwarf {
        self.dwarf.get_or_init(|| {
            let mut d = crate::dwarf::read(&self.data);
            if let Some(p) = &self.pdb {
                for (k, v) in &p.types.protos {
                    d.protos.entry(*k).or_insert_with(|| v.clone());
                }
                for (k, v) in &p.types.structs {
                    d.structs.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            d
        })
    }

    /// The strings found in the data sections, sorted by address.
    pub fn strings(&self) -> &[Str] {
        self.strings.get_or_init(|| crate::strings::scan(self))
    }

    /// Every reference found (see [`Refs`]), built on the first call. When the functions are not
    /// found yet, the same decode pass finds them too; ask for the references before the functions
    /// where both are needed.
    pub fn refs(&self) -> &Refs {
        self.refs.get_or_init(|| {
            let t0 = std::time::Instant::now();
            let mut c = crate::refs::Collector::new();
            if self.funcs.get().is_some() {
                crate::funcs::decode_refs(self, &mut c);
            } else {
                let f = crate::funcs::find_with(self, Some(&mut c));
                let _ = self.funcs.set(f);
            }
            crate::refs::build(self, c, t0)
        })
    }

    /// Put a list made by hand in place of the real one (tests only).
    #[cfg(test)]
    pub(crate) fn set_refs_for_test(&self, r: Refs) {
        let _ = self.funcs();
        let _ = self.refs.set(r);
    }

    /// The name of a function to show: the one the person gave, else the one found.
    pub fn func_name<'a>(&'a self, f: &'a Func) -> &'a str {
        self.user.names.get(&f.start).map_or(f.name.as_str(), String::as_str)
    }

    /// Add library signatures to the shipped ones (before the functions are first asked for).
    pub fn add_sigs(&mut self, set: crate::SigSet) {
        self.sigs = std::mem::take(&mut self.sigs).merged(set);
    }

    /// The signature label of the function at `addr`: `[signature exact: ...]` and so on.
    pub fn sig_label(&self, addr: u64) -> Option<String> {
        let f = self.funcs().binary_search_by_key(&addr, |f| f.start).ok()?;
        self.funcs()[f].sig.as_ref().map(|h| h.label())
    }

    /// The name the person gave to `addr`, if any.
    pub fn user_name(&self, addr: u64) -> Option<&str> {
        self.user.names.get(&addr).map(String::as_str)
    }

    /// The note the person left at `addr`, if any.
    pub fn note_at(&self, addr: u64) -> Option<&str> {
        self.user.notes.get(&addr).map(String::as_str)
    }

    /// The function that holds `addr`.
    pub fn func_at(&self, addr: u64) -> Option<&Func> {
        let f = self.funcs();
        let n = f.partition_point(|x| x.start <= addr);
        f[..n].last().filter(|x| addr < x.end)
    }

    /// The string that holds `addr`, with the offset into it.
    pub fn string_at(&self, addr: u64) -> Option<(&Str, u64)> {
        let s = self.strings();
        let n = s.partition_point(|x| x.addr <= addr);
        s[..n].last().filter(|x| addr - x.addr < x.byte_len).map(|x| (x, addr - x.addr))
    }

    /// `name` or `name+0x1c` for an address inside a function.
    pub fn label(&self, addr: u64) -> Option<String> {
        let f = self.func_at(addr)?;
        let name = self.func_name(f);
        Some(if addr == f.start { name.to_string() } else { format!("{name}+{:#x}", addr - f.start) })
    }

    pub fn arch_name(&self) -> &'static str {
        if self.arm64 {
            "arm64"
        } else if self.bits == 32 {
            "x86"
        } else {
            "x86-64"
        }
    }

    /// The format with its class: `ELF64`, `ELF32`, `PE32+` or `PE32`.
    pub fn format_name(&self) -> &'static str {
        match (self.format, self.bits) {
            (Format::Elf, 32) => "ELF32",
            (Format::Pe, 32) => "PE32",
            (f, _) => f.name(),
        }
    }

    /// For the commands that read x86-64 only: an error that says so, instead of a wrong answer.
    pub fn require_x86(&self, what: &str) -> Result<()> {
        if self.arm64 {
            return Err(Error::Bad(format!(
                "{what} does not read AArch64 code yet (fn, funcs, xref and calls do); no output is better than a wrong one"
            )));
        }
        if self.bits == 32 {
            return Err(Error::Bad(format!(
                "{what}: x86-32 is not supported yet (info, overview, funcs, fn, xref, calls, strings, bytes and find \
                 read it); no output is better than a wrong one"
            )));
        }
        Ok(())
    }

    /// The bytes of an address in this file (4 for x86-32, 8 otherwise).
    pub fn ptr_size(&self) -> usize {
        if self.bits == 32 {
            4
        } else {
            8
        }
    }

    pub fn code_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.alloc && s.exec && s.file_size > 0)
    }
}
