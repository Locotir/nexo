//! The empty image, and the parts ELF and PE have in common.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;

use object::{Object, ObjectSection, ObjectSegment, ObjectSymbol, SectionFlags, SegmentFlags, SymbolKind};

use super::names::{demangle, raw_of};
use super::{Export, Format, Hardening, Image, Section, Symbol, MAX_NAME_IN, MAX_SECTIONS, MAX_SYMBOLS};
use crate::names::UserNames;

pub(crate) fn blank(format: Format) -> Image {
    Image {
        path: PathBuf::new(),
        format,
        arm64: false,
        bits: 64,
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
        got: None,
        sorted: Vec::new(),
        rel_ptrs: Vec::new(),
        user: UserNames::default(),
        funcs: OnceLock::new(),
        strings: OnceLock::new(),
        refs: OnceLock::new(),
        dwarf: OnceLock::new(),
        sigs: Default::default(),
        warnings: Vec::new(),
        pdb: None,
        macho: None,
    }
}

/// Sections, symbols and exports: what ELF and PE have in common.
pub(super) fn common<'d, O: Object<'d>>(o: &O, img: &mut Image) {
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
