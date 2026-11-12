//! Opening a file: headers, sections, symbols, imports, exports, hardening. Bytes only.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile64};
use object::{
    pe, Architecture, BinaryFormat, Endianness, FileKind, LittleEndian as LE, Object, ObjectSection, ObjectSegment,
    ObjectSymbol, ObjectSymbolTable, SectionFlags, SegmentFlags, SymbolKind,
};
use sha2::{Digest, Sha256};

use crate::funcs::Func;
use crate::names::UserNames;
use crate::refs::Refs;
use crate::strings::Str;
use crate::{Error, Result};

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
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Elf => "ELF64",
            Format::Pe => "PE32+",
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
    sorted: Vec<usize>,
    /// Pointers the loader writes (ELF relative relocations): (slot, value).
    pub(crate) rel_ptrs: Vec<(u64, u64)>,
    /// Names and notes the person gave (see `names`).
    pub(crate) user: UserNames,
    funcs: OnceLock<Vec<Func>>,
    strings: OnceLock<Vec<Str>>,
    refs: OnceLock<Refs>,
    dwarf: OnceLock<crate::dwarf::Dwarf>,
}

impl Image {
    /// Read the file and parse its headers.
    pub fn open(path: &Path) -> Result<Image> {
        let meta = std::fs::metadata(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(Error::NotRead(format!("{} is not a file", path.display())));
        }
        if meta.len() > MAX_FILE {
            return Err(Error::NotRead(format!(
                "{} is {} bytes; files over 1 GiB are not read",
                path.display(),
                meta.len()
            )));
        }
        let data = std::fs::read(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
        Image::from_bytes(data, path.to_path_buf())
    }

    /// Parse bytes already in memory.
    pub fn from_bytes(data: Vec<u8>, path: PathBuf) -> Result<Image> {
        let kind = FileKind::parse(&data[..]).map_err(|_| not_a_binary(&data))?;
        let mut img = match kind {
            FileKind::Elf64 => load_elf(&data)?,
            FileKind::Pe64 => load_pe(&data)?,
            other => return Err(Error::NotRead(format!("{} is not read yet", describe_kind(other, &data)))),
        };
        img.path = path;
        img.sha256 = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        img.data = data;
        img.finish();
        Ok(img)
    }

    fn finish(&mut self) {
        // function names win over exports, exports over data names
        let mut names: BTreeMap<u64, String> = BTreeMap::new();
        // the size of the symbol that gave each name, so `symbol_covering` is a lookup
        let mut sizes: HashMap<u64, u64> = HashMap::new();
        for s in self.symbols.iter().filter(|s| s.func) {
            if let Entry::Vacant(v) = names.entry(s.addr) {
                v.insert(s.name.clone());
                sizes.insert(s.addr, s.size);
            }
        }
        for e in &self.exports {
            names.entry(e.addr).or_insert_with(|| e.name.clone());
        }
        for s in self.symbols.iter().filter(|s| !s.func) {
            if let Entry::Vacant(v) = names.entry(s.addr) {
                v.insert(s.name.clone());
                sizes.insert(s.addr, s.size);
            }
        }
        let mut raws: HashMap<u64, String> = HashMap::new();
        for s in self.symbols.iter().filter(|s| !s.raw.is_empty()) {
            raws.entry(s.addr).or_insert_with(|| s.raw.clone());
        }
        for e in self.exports.iter().filter(|e| !e.raw.is_empty()) {
            raws.entry(e.addr).or_insert_with(|| e.raw.clone());
        }
        for i in self.imports.iter().filter(|i| !i.raw.is_empty()) {
            if let Some(slot) = i.slot {
                raws.entry(slot).or_insert_with(|| i.raw.clone());
            }
        }
        self.raws = raws;
        self.names = names;
        self.sizes = sizes;
        self.slots = self.imports.iter().enumerate().filter_map(|(i, im)| im.slot.map(|s| (s, i))).collect();
        let mut sorted: Vec<usize> =
            (0..self.sections.len()).filter(|&i| self.sections[i].alloc && self.sections[i].size > 0).collect();
        sorted.sort_by_key(|&i| (self.sections[i].addr, i));
        self.sorted = sorted;
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
        self.dwarf.get_or_init(|| crate::dwarf::read(&self.data))
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
        "x86-64"
    }

    pub fn code_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.alloc && s.exec && s.file_size > 0)
    }
}

/// The bytes of a file's start that [`opens`] looks at.
const HEAD: u64 = 4096;

/// Whether `Image::open` would take the file at `path`: an ELF64 or PE32+ x86-64 file (not a
/// relocatable object). Only the headers in the first 4096 bytes are read, as bytes: the file is not
/// run, not loaded and not read whole, so a large file costs the same as a small one. A path that
/// leaves through a symlink is read the same way. `false` for anything else, including a missing file.
/// The kind of file is asked before it is opened: opening a named pipe that nobody writes to never returns.
pub fn opens(path: &Path) -> bool {
    use object::read::elf::FileHeader;
    use std::io::Read;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else { return false };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = Vec::new();
    if file.take(HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    match FileKind::parse(&head[..]) {
        Ok(FileKind::Elf64) => object::elf::FileHeader64::<Endianness>::parse(&head[..]).is_ok_and(|h| {
            h.endian().is_ok_and(|e| h.e_machine(e) == object::elf::EM_X86_64 && h.e_type(e) != object::elf::ET_REL)
        }),
        Ok(FileKind::Pe64) => {
            let Ok(dos) = pe::ImageDosHeader::parse(&head[..]) else { return false };
            let mut at = u64::from(dos.nt_headers_offset());
            pe::ImageNtHeaders64::parse(&head[..], &mut at)
                .is_ok_and(|(nt, _)| nt.file_header().machine.get(LE) == pe::IMAGE_FILE_MACHINE_AMD64)
        }
        _ => false,
    }
}

fn not_a_binary(data: &[u8]) -> Error {
    if data.is_empty() {
        return Error::NotRead("the file is empty, not a binary".into());
    }
    let head = &data[..data.len().min(4096)];
    if std::str::from_utf8(head).is_ok() || head.iter().all(|b| b.is_ascii()) {
        Error::NotRead("this is a text file, not an ELF or PE binary".into())
    } else {
        Error::NotRead("not an ELF or PE binary (unknown file signature)".into())
    }
}

/// What was found, in words: "Mach-O arm64", "ELF32 x86".
fn describe_kind(kind: FileKind, data: &[u8]) -> String {
    let of = |f: BinaryFormat| {
        object::File::parse(data).ok().filter(|o| o.format() == f).map(|o| arch_word(o.architecture()))
    };
    match kind {
        FileKind::Elf32 => format!("ELF32 {}", of(BinaryFormat::Elf).unwrap_or("")).trim().to_string(),
        FileKind::Pe32 => format!("PE32 {}", of(BinaryFormat::Pe).unwrap_or("x86")).trim().to_string(),
        FileKind::MachO32 | FileKind::MachO64 => {
            format!("Mach-O {}", of(BinaryFormat::MachO).unwrap_or("")).trim().to_string()
        }
        FileKind::MachOFat32 | FileKind::MachOFat64 => "Mach-O universal binary".into(),
        FileKind::Archive => "static library (ar archive)".into(),
        FileKind::Coff | FileKind::CoffBig | FileKind::CoffImport => "COFF object file".into(),
        FileKind::DyldCache => "dyld shared cache".into(),
        FileKind::Xcoff32 | FileKind::Xcoff64 => "XCOFF".into(),
        FileKind::Elf64 => "ELF64".into(),
        FileKind::Pe64 => "PE32+".into(),
        _ => "this file format".into(),
    }
}

fn blank(format: Format) -> Image {
    Image {
        path: PathBuf::new(),
        format,
        entry: 0,
        image_base: 0,
        sections: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        hardening: Hardening::default(),
        sha256: String::new(),
        data: Vec::new(),
        names: BTreeMap::new(),
        sizes: HashMap::new(),
        raws: HashMap::new(),
        slots: HashMap::new(),
        exception_dir: None,
        sorted: Vec::new(),
        rel_ptrs: Vec::new(),
        user: UserNames::default(),
        funcs: OnceLock::new(),
        strings: OnceLock::new(),
        refs: OnceLock::new(),
        dwarf: OnceLock::new(),
    }
}

/// Demangle Rust (legacy and v0), Itanium C++ and MSVC C++ names; anything else stays as written. The
/// whole name is demangled (up to `MAX_NAME_IN` characters) and then the result is cut to `MAX_NAME`.
pub(crate) fn demangle(raw: &str) -> String {
    let cut = |t: &str| t.chars().take(MAX_NAME).collect::<String>();
    let raw: String = raw.chars().take(MAX_NAME_IN).collect();
    // Mach-O writes every C symbol with one more underscore in front
    let body = raw.strip_prefix("__Z").map(|_| &raw[1..]).unwrap_or(&raw);
    if body.starts_with("_Z") || body.starts_with("_R") {
        if let Ok(d) = rustc_demangle::try_demangle(body) {
            return cut(&format!("{d:#}"));
        }
        if body.starts_with("_Z") {
            if let Ok(s) = cpp_demangle::Symbol::new(body.as_bytes()) {
                if let Ok(t) = s.demangle() {
                    return cut(&t);
                }
            }
        }
    }
    if raw.starts_with('?') {
        if let Ok(t) = msvc_demangler::demangle(&raw, msvc_demangler::DemangleFlags::llvm()) {
            return cut(&t);
        }
    }
    cut(&raw)
}

/// The mangled name to keep next to a demangled one: empty when demangling changed nothing.
fn raw_of(raw: &str, shown: &str) -> String {
    if raw == shown {
        String::new()
    } else {
        raw.chars().take(MAX_NAME_IN).collect()
    }
}

/// Sections, symbols and exports: what ELF and PE have in common.
fn common<'d, O: Object<'d>>(o: &O, img: &mut Image) {
    for s in o.sections().take(MAX_SECTIONS) {
        let (read, write, exec, alloc) = match s.flags() {
            SectionFlags::Elf { sh_flags, .. } => {
                let f = sh_flags.0;
                (f & 2 != 0, f & 1 != 0, f & 4 != 0, f & 2 != 0)
            }
            SectionFlags::Coff { characteristics } => {
                let characteristics = characteristics.0;
                (
                    characteristics & 0x4000_0000 != 0,
                    characteristics & 0x8000_0000 != 0,
                    characteristics & 0x2000_0000 != 0 || characteristics & 0x20 != 0,
                    true,
                )
            }
            _ => (true, false, false, false),
        };
        let (file_off, file_size) = s.file_range().unwrap_or((0, 0));
        img.sections.push(Section {
            index: s.index().0,
            name: s.name().unwrap_or("").chars().take(64).collect(),
            addr: s.address(),
            size: s.size(),
            file_off,
            file_size,
            read,
            write,
            exec,
            alloc,
        });
    }
    if img.sections.is_empty() {
        // no section headers (a stripped ELF): the loadable segments stand in
        for (i, g) in o.segments().enumerate().take(MAX_SECTIONS) {
            let p_flags = match g.flags() {
                SegmentFlags::Elf { p_flags, .. } => p_flags.0,
                _ => 0,
            };
            let (file_off, file_size) = g.file_range();
            img.sections.push(Section {
                index: i,
                name: format!("LOAD{i}"),
                addr: g.address(),
                size: g.size(),
                file_off,
                file_size,
                read: p_flags & 4 != 0,
                write: p_flags & 2 != 0,
                exec: p_flags & 1 != 0,
                alloc: true,
            });
        }
    }
    // a hostile header can put a section at the very top of the address space: keep `addr + size`
    // inside u64 so that no address sum below can overflow
    for s in &mut img.sections {
        s.size = s.size.min(u64::MAX - s.addr);
    }
    for s in o.symbols().chain(o.dynamic_symbols()).take(MAX_SYMBOLS) {
        if !s.is_definition() || s.address() == 0 {
            continue;
        }
        let kind = s.kind();
        if !matches!(kind, SymbolKind::Text | SymbolKind::Data | SymbolKind::Unknown | SymbolKind::Label) {
            continue;
        }
        let Ok(raw) = s.name() else { continue };
        if raw.is_empty() {
            continue;
        }
        let name = demangle(raw);
        img.symbols.push(Symbol {
            raw: raw_of(raw, &name),
            name,
            addr: s.address(),
            size: s.size(),
            func: kind == SymbolKind::Text,
        });
    }
    if let Ok(ex) = o.exports() {
        for e in ex.take(MAX_SYMBOLS).flatten() {
            let (object::read::NameOrOrdinal::Name(n), object::read::ExportTarget::Address { address }) =
                (e.name(), e.target())
            else {
                continue;
            };
            let name = String::from_utf8_lossy(n).chars().take(MAX_NAME_IN).collect::<String>();
            if !name.is_empty() {
                let shown = demangle(&name);
                img.exports.push(Export { raw: raw_of(&name, &shown), name: shown, addr: address });
            }
        }
    }
    img.entry = o.entry();
}

/// The pointer the loader stores for a relocation, when it is one into the image: the base plus the
/// addend (`RELATIVE`), the resolver an `IRELATIVE` slot starts from, or a defined symbol plus the addend
/// (`R_X86_64_64`).
pub(crate) fn reloc_pointer(r_type: object::elf::RelocationType, defined: Option<u64>, addend: i64) -> Option<u64> {
    use object::elf::{R_X86_64_64, R_X86_64_IRELATIVE, R_X86_64_RELATIVE};
    match r_type {
        R_X86_64_RELATIVE | R_X86_64_IRELATIVE => Some(addend as u64),
        R_X86_64_64 => defined.map(|s| s.wrapping_add(addend as u64)),
        _ => None,
    }
}

fn load_elf(data: &[u8]) -> Result<Image> {
    let elf = object::read::elf::ElfFile64::<Endianness>::parse(data)
        .map_err(|e| Error::NotRead(format!("not a readable ELF file: {e}")))?;
    if elf.architecture() != Architecture::X86_64 {
        return Err(Error::NotRead(format!(
            "ELF64 {} is not read yet (only x86-64 is)",
            arch_word(elf.architecture())
        )));
    }
    if elf.elf_header().e_type.get(elf.endian()) == object::elf::ET_REL {
        return Err(Error::NotRead(
            "ELF relocatable object is not read yet (a .o or .ko file has no addresses)".into(),
        ));
    }
    let mut img = blank(Format::Elf);
    common(&elf, &mut img);
    // imports: undefined dynamic symbols; the slot comes from the relocation that fills it
    let mut slot_of = HashMap::new();
    if let Some(relocs) = elf.dynamic_relocations() {
        for (off, r) in relocs.take(MAX_RELOCS) {
            let mut defined = None;
            if let object::RelocationTarget::Symbol(idx) = r.target() {
                if slot_of.len() < MAX_IMPORTS {
                    slot_of.entry(idx).or_insert(off);
                }
                defined = elf
                    .dynamic_symbol_table()
                    .and_then(|t| t.symbol_by_index(idx).ok())
                    .filter(|s| !s.is_undefined())
                    .map(|s| s.address());
            }
            let object::RelocationFlags::Elf { r_type } = r.flags() else { continue };
            if let Some(v) = reloc_pointer(r_type, defined, r.addend()) {
                if img.rel_ptrs.len() < MAX_RELOCS {
                    img.rel_ptrs.push((off, v));
                }
            }
        }
    }
    for s in elf.dynamic_symbols().take(MAX_IMPORTS) {
        if !s.is_undefined() {
            continue;
        }
        let Ok(name) = s.name() else { continue };
        if name.is_empty() {
            continue;
        }
        let shown = demangle(name);
        img.imports.push(Import {
            lib: None,
            raw: raw_of(name, &shown),
            name: shown,
            slot: slot_of.get(&s.index()).copied(),
        });
    }
    // headers
    use object::elf::{
        DF_1_NOW, DF_BIND_NOW, DT_BIND_NOW, DT_FLAGS, DT_FLAGS_1, ET_DYN, PF_X, PT_DYNAMIC, PT_GNU_RELRO, PT_GNU_STACK,
    };
    use object::read::elf::{Dyn, ProgramHeader};
    let e = elf.endian();
    let mut base = u64::MAX;
    let (mut stack, mut relro, mut dynamic) = (None, false, None);
    for ph in elf.elf_program_headers() {
        let t = ph.p_type(e);
        if t == object::elf::PT_LOAD {
            base = base.min(ph.p_vaddr(e));
        } else if t == PT_GNU_STACK {
            stack = Some(ph.p_flags(e).0 & PF_X.0 != 0);
        } else if t == PT_GNU_RELRO {
            relro = true;
        } else if t == PT_DYNAMIC {
            dynamic = ph.dynamic(e, data).ok().flatten();
        }
    }
    let mut now = false;
    for d in dynamic.unwrap_or(&[]).iter().take(4096) {
        let tag = d.d_tag(e);
        let val = d.d_val(e);
        if tag == DT_BIND_NOW
            || (tag == DT_FLAGS && val & DF_BIND_NOW.0 != 0)
            || (tag == DT_FLAGS_1 && val & DF_1_NOW.0 != 0)
        {
            now = true;
        }
    }
    img.image_base = if base == u64::MAX { 0 } else { base };
    let pie = elf.elf_header().e_type.get(e) == ET_DYN;
    img.hardening.items = vec![
        ("pie", if pie { "yes (position independent)".into() } else { "no (fixed base)".into() }),
        (
            "nx_stack",
            match stack {
                Some(false) => "yes".into(),
                Some(true) => "no (stack is executable)".into(),
                None => "no (no GNU_STACK header, so the stack is executable)".into(),
            },
        ),
        (
            "relro",
            if !relro {
                "none".into()
            } else if now {
                "full".into()
            } else {
                "partial".into()
            },
        ),
    ];
    Ok(img)
}

fn load_pe(data: &[u8]) -> Result<Image> {
    let pe = PeFile64::parse(data).map_err(|e| Error::NotRead(format!("not a readable PE file: {e}")))?;
    if pe.architecture() != Architecture::X86_64 {
        return Err(Error::NotRead(format!("PE32+ {} is not read yet (only x86-64 is)", arch_word(pe.architecture()))));
    }
    let mut img = blank(Format::Pe);
    common(&pe, &mut img);
    // the loader maps a section whose virtual size is 0 with the size of its raw data; the generic
    // reader reports 0 bytes for it, so read the raw header
    for (i, h) in pe.section_table().iter().enumerate().take(MAX_SECTIONS) {
        if h.virtual_size.get(LE) != 0 {
            continue;
        }
        let raw = u64::from(h.size_of_raw_data.get(LE));
        if let Some(s) = img.sections.iter_mut().find(|s| s.index == i + 1) {
            s.file_off = u64::from(h.pointer_to_raw_data.get(LE));
            s.file_size = raw;
            s.size = raw.min(u64::MAX - s.addr);
        }
    }
    let oh = pe.nt_headers().optional_header();
    img.image_base = oh.image_base();
    if let Some(d) = pe.data_directory(pe::IMAGE_DIRECTORY_ENTRY_EXCEPTION) {
        let (rva, size) = (u64::from(d.virtual_address.get(LE)), u64::from(d.size.get(LE)));
        if rva != 0 && size != 0 {
            img.exception_dir = Some((rva, size));
        }
    }
    // imports, with the address of each IAT slot
    if let Ok(Some(table)) = pe.import_table() {
        if let Ok(mut descs) = table.descriptors() {
            'all: while let Ok(Some(d)) = descs.next() {
                let lib = table
                    .name(d.name.get(LE))
                    .ok()
                    .map(|n| String::from_utf8_lossy(n).chars().take(MAX_NAME).collect::<String>());
                let first = u64::from(d.first_thunk.get(LE));
                let lookup = match d.original_first_thunk.get(LE) {
                    0 => d.first_thunk.get(LE),
                    v => v,
                };
                let Ok(mut thunks) = table.thunks(lookup) else { continue };
                let mut i = 0u64;
                while let Ok(Some(t)) = thunks.next::<pe::ImageNtHeaders64>() {
                    let name = match table.import::<pe::ImageNtHeaders64>(t) {
                        Ok(object::read::pe::Import::Name(_, n)) => {
                            String::from_utf8_lossy(n).chars().take(MAX_NAME).collect()
                        }
                        Ok(object::read::pe::Import::Ordinal(o)) => format!("ordinal#{o}"),
                        Err(_) => break,
                    };
                    img.imports.push(Import {
                        lib: lib.clone(),
                        raw: String::new(),
                        name,
                        slot: Some(img.image_base.wrapping_add(first).wrapping_add(i * 8)),
                    });
                    i += 1;
                    if img.imports.len() >= MAX_IMPORTS {
                        break 'all;
                    }
                }
            }
        }
    }
    let dll = oh.dll_characteristics();
    let yes_no = |bit: pe::DllFlags| if dll.contains(bit) { "yes" } else { "no" }.to_string();
    img.hardening.items = vec![
        ("aslr", yes_no(pe::IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE)),
        ("high_entropy_aslr", yes_no(pe::IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA)),
        ("dep", yes_no(pe::IMAGE_DLLCHARACTERISTICS_NX_COMPAT)),
        ("cfg", yes_no(pe::IMAGE_DLLCHARACTERISTICS_GUARD_CF)),
    ];
    Ok(img)
}

/// An architecture in one word, as people write it.
fn arch_word(a: Architecture) -> &'static str {
    match a {
        Architecture::Aarch64 | Architecture::Aarch64_Ilp32 => "arm64",
        Architecture::Arm => "arm",
        Architecture::I386 => "x86",
        Architecture::X86_64 | Architecture::X86_64_X32 => "x86-64",
        Architecture::Riscv64 => "riscv64",
        Architecture::Riscv32 => "riscv32",
        Architecture::PowerPc64 => "ppc64",
        Architecture::Mips | Architecture::Mips64 => "mips",
        _ => "unknown arch",
    }
}
