//! The tables `syms` reads again from the file's bytes, because the image does not keep them: an ELF file's
//! DT_NEEDED, SONAME and RUNPATH entries, its symbol versions and its dynamic relocations; a PE file's
//! delay-load imports and base relocations. Relocations are only kept when they are listed, and counted
//! otherwise, since a large program has millions of them.

use std::collections::HashMap;

use object::read::elf::{ElfFile, FileHeader, ProgramHeader};
use object::read::pe::{ImageNtHeaders, PeFile};
use object::{pe, Endianness, LittleEndian as LE, Object, ObjectSymbol, RelocationFlags, RelocationTarget};

use crate::image::{Format, Image};

/// Longest library, version or symbol name kept.
const MAX_NAME: usize = 256;
/// Most relocations or delay-load imports read.
const MAX_ROWS: usize = 4_000_000;
/// Most dynamic entries and PE relocation blocks read, so a broken table cannot keep the reader going.
const MAX_ENTRIES: usize = 1 << 20;

#[derive(Default)]
pub(crate) struct Tables {
    /// Libraries the loader loads first, in the file's order.
    pub needed: Vec<String>,
    /// ELF DT_SONAME, or a Mach-O library's own install name.
    pub soname: Option<String>,
    /// `RUNPATH dir` or `RPATH dir`: where the loader looks for the needed libraries first.
    pub search: Option<String>,
    /// ELF: the file has a dynamic segment (a static program has none).
    pub dynamic: bool,
    /// ELF import (its name as the file writes it) to the library file and version its symbol version names.
    pub versions: HashMap<String, (Option<String>, String)>,
    /// PE delay-load imports: (DLL, name, IAT slot).
    pub delay: Vec<(String, String, u64)>,
    pub relocs: Vec<Reloc>,
    pub reloc_count: usize,
    /// ELF dynamic symbol names by index, for the relocations against a symbol.
    pub dynsym: Vec<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct Reloc {
    pub at: u64,
    /// The type number as the file writes it (`R_X86_64_*`, `IMAGE_REL_BASED_*`).
    pub ty: u32,
    /// The dynamic symbol it names, if any.
    pub sym: Option<u32>,
    pub addend: i64,
    /// Bytes written, 0 when the reader does not know.
    pub size: u8,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).chars().take(MAX_NAME).collect()
}

/// The tables of `img`; `relocs` keeps each relocation, otherwise they are only counted.
pub(crate) fn read(img: &Image, relocs: bool) -> Tables {
    let mut t = Tables::default();
    match (img.format, img.bits) {
        (Format::Elf, 64) => elf::<object::elf::FileHeader64<Endianness>>(&img.data, relocs, &mut t),
        (Format::Elf, _) => elf::<object::elf::FileHeader32<Endianness>>(&img.data, relocs, &mut t),
        (Format::Pe, 64) => pe_tables::<pe::ImageNtHeaders64>(img, relocs, &mut t),
        (Format::Pe, _) => pe_tables::<pe::ImageNtHeaders32>(img, relocs, &mut t),
        (Format::MachO, _) => {
            if let Some(m) = &img.macho {
                t.needed = m.libs.clone();
                t.soname = m.id.clone();
            }
            t.dynamic = true;
        }
    }
    t
}

fn elf<Elf: FileHeader<Endian = Endianness>>(data: &[u8], relocs: bool, t: &mut Tables) {
    let Ok(f) = ElfFile::<Elf>::parse(data) else { return };
    let e = f.endian();
    // the program headers decide: a file whose section headers were cut off still has its dynamic segment
    t.dynamic = f.elf_program_headers().iter().any(|p| p.p_type(e) == object::elf::PT_DYNAMIC);
    if let Ok(table) = f.elf_dynamic_table() {
        for d in table.iter().take(MAX_ENTRIES) {
            let s = || table.string(d).ok().map(text);
            match d.tag {
                object::elf::DT_NEEDED => t.needed.extend(s()),
                object::elf::DT_SONAME => t.soname = s(),
                // the loader ignores RPATH when RUNPATH is there
                object::elf::DT_RUNPATH => t.search = s().map(|p| format!("RUNPATH {p}")),
                object::elf::DT_RPATH if t.search.is_none() => t.search = s().map(|p| format!("RPATH {p}")),
                _ => {}
            }
        }
    }
    let versions = f.elf_section_table().versions(e, f.data()).ok().flatten();
    for s in f.dynamic_symbols().take(MAX_ROWS) {
        let name = s.name().unwrap_or("");
        if relocs {
            let i = s.index().0;
            if t.dynsym.len() <= i {
                t.dynsym.resize(i + 1, String::new());
            }
            t.dynsym[i] = text(name.as_bytes());
        }
        if !s.is_undefined() || name.is_empty() {
            continue;
        }
        let Some(vt) = &versions else { continue };
        if let Ok(Some(v)) = vt.version(vt.version_index(e, s.index()).index()) {
            t.versions.insert(name.to_string(), (v.file().map(text), text(v.name())));
        }
    }
    let Some(it) = f.dynamic_relocations() else { return };
    for (at, r) in it.take(MAX_ROWS) {
        t.reloc_count += 1;
        if !relocs {
            continue;
        }
        let RelocationFlags::Elf { r_type } = r.flags() else { continue };
        let sym = match r.target() {
            RelocationTarget::Symbol(i) => u32::try_from(i.0).ok(),
            _ => None,
        };
        t.relocs.push(Reloc { at, ty: r_type.0, sym, addend: r.addend(), size: r.size() / 8 });
    }
}

fn pe_tables<Pe: ImageNtHeaders>(img: &Image, relocs: bool, t: &mut Tables) {
    for l in img.imports.iter().filter_map(|i| i.lib.as_ref()) {
        if !t.needed.iter().any(|n| n.eq_ignore_ascii_case(l)) {
            t.needed.push(l.clone());
        }
    }
    let Ok(f) = PeFile::<Pe>::parse(&*img.data) else { return };
    let (base, width) = (img.image_base, u64::from(img.bits / 8));
    if let Ok(Some(dt)) = f.delay_load_import_table() {
        if let Ok(mut descs) = dt.descriptors() {
            'all: while let Ok(Some(d)) = descs.next() {
                // attribute bit 0 clear: an old table written with addresses instead of RVAs, which the reader
                // below would misread
                if d.attributes.get(LE) & 1 == 0 {
                    continue;
                }
                let dll = dt.name(d.dll_name_rva.get(LE)).map(text).unwrap_or_default();
                if !dll.is_empty() && !t.needed.iter().any(|n| n.eq_ignore_ascii_case(&dll)) {
                    t.needed.push(dll.clone());
                }
                let iat = base.wrapping_add(u64::from(d.import_address_table_rva.get(LE)));
                let Ok(mut thunks) = dt.thunks(d.import_name_table_rva.get(LE)) else { continue };
                let mut i = 0u64;
                while let Ok(Some(th)) = thunks.next::<Pe>() {
                    let name = match dt.import::<Pe>(th) {
                        Ok(object::read::pe::Import::Name(_, n)) => text(n),
                        Ok(object::read::pe::Import::Ordinal(o)) => format!("ordinal#{o}"),
                        Err(_) => break,
                    };
                    t.delay.push((dll.clone(), name, iat.wrapping_add(i * width)));
                    i += 1;
                    if t.delay.len() >= MAX_ROWS {
                        break 'all;
                    }
                }
            }
        }
    }
    let Ok(Some(mut blocks)) = f.data_directories().relocation_blocks(f.data(), &f.section_table()) else {
        return;
    };
    let mut n = 0;
    while let Ok(Some(block)) = blocks.next() {
        n += 1;
        for r in block {
            let size = match r.typ {
                pe::IMAGE_REL_BASED_ABSOLUTE => continue,
                pe::IMAGE_REL_BASED_DIR64 => 8,
                pe::IMAGE_REL_BASED_HIGHLOW => 4,
                _ => 0,
            };
            t.reloc_count += 1;
            if relocs && t.relocs.len() < MAX_ROWS {
                let at = base.wrapping_add(u64::from(r.virtual_address));
                t.relocs.push(Reloc { at, ty: u32::from(r.typ.0), sym: None, addend: 0, size });
            }
        }
        if n >= MAX_ENTRIES || t.reloc_count >= MAX_ROWS {
            break;
        }
    }
}

/// The name of a relocation type of `img`'s machine (`R_X86_64_JUMP_SLOT`, `DIR64`), else `type N`.
pub(crate) fn reloc_type(img: &Image, ty: u32) -> String {
    use object::elf::*;
    let name = match img.format {
        Format::Pe => match pe::BaseRelocationType(ty as u16) {
            pe::IMAGE_REL_BASED_DIR64 => Some("DIR64"),
            pe::IMAGE_REL_BASED_HIGHLOW => Some("HIGHLOW"),
            _ => None,
        },
        Format::MachO => None,
        // the numbers overlap between machines, so each one has its own table
        Format::Elf if img.arm64 => match RelocationType(ty) {
            R_AARCH64_ABS64 => Some("R_AARCH64_ABS64"),
            R_AARCH64_COPY => Some("R_AARCH64_COPY"),
            R_AARCH64_GLOB_DAT => Some("R_AARCH64_GLOB_DAT"),
            R_AARCH64_JUMP_SLOT => Some("R_AARCH64_JUMP_SLOT"),
            R_AARCH64_RELATIVE => Some("R_AARCH64_RELATIVE"),
            R_AARCH64_TLS_DTPMOD => Some("R_AARCH64_TLS_DTPMOD"),
            R_AARCH64_TLS_DTPREL => Some("R_AARCH64_TLS_DTPREL"),
            R_AARCH64_TLS_TPREL => Some("R_AARCH64_TLS_TPREL"),
            R_AARCH64_TLSDESC => Some("R_AARCH64_TLSDESC"),
            R_AARCH64_IRELATIVE => Some("R_AARCH64_IRELATIVE"),
            _ => None,
        },
        Format::Elf if img.bits == 32 => match RelocationType(ty) {
            R_386_32 => Some("R_386_32"),
            R_386_PC32 => Some("R_386_PC32"),
            R_386_COPY => Some("R_386_COPY"),
            R_386_GLOB_DAT => Some("R_386_GLOB_DAT"),
            R_386_JMP_SLOT => Some("R_386_JMP_SLOT"),
            R_386_RELATIVE => Some("R_386_RELATIVE"),
            R_386_TLS_TPOFF => Some("R_386_TLS_TPOFF"),
            R_386_TLS_DTPMOD32 => Some("R_386_TLS_DTPMOD32"),
            R_386_TLS_DTPOFF32 => Some("R_386_TLS_DTPOFF32"),
            R_386_TLS_DESC => Some("R_386_TLS_DESC"),
            R_386_IRELATIVE => Some("R_386_IRELATIVE"),
            _ => None,
        },
        Format::Elf => match RelocationType(ty) {
            R_X86_64_64 => Some("R_X86_64_64"),
            R_X86_64_COPY => Some("R_X86_64_COPY"),
            R_X86_64_GLOB_DAT => Some("R_X86_64_GLOB_DAT"),
            R_X86_64_JUMP_SLOT => Some("R_X86_64_JUMP_SLOT"),
            R_X86_64_RELATIVE => Some("R_X86_64_RELATIVE"),
            R_X86_64_DTPMOD64 => Some("R_X86_64_DTPMOD64"),
            R_X86_64_DTPOFF64 => Some("R_X86_64_DTPOFF64"),
            R_X86_64_TPOFF64 => Some("R_X86_64_TPOFF64"),
            R_X86_64_TLSDESC => Some("R_X86_64_TLSDESC"),
            R_X86_64_IRELATIVE => Some("R_X86_64_IRELATIVE"),
            _ => None,
        },
    };
    name.map_or_else(|| format!("type {ty}"), String::from)
}

/// The pointer the loader writes for a relocation that needs no symbol: the base plus the addend (`RELATIVE`,
/// and `IRELATIVE`'s resolver), or for a PE base relocation and an ELF32 REL entry, the value the slot holds.
pub(crate) fn reloc_value(img: &Image, r: &Reloc) -> Option<u64> {
    use object::elf::*;
    // `read_at` gives up to `n` bytes: a slot cut by the section's end or the file's end (a carved or broken file)
    // has no value to show
    let read = |n: usize| {
        let b = img.read_at(r.at, n).filter(|b| n <= 8 && b.len() == n)?;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(u64::from_le_bytes(v))
    };
    match img.format {
        Format::Pe => read(usize::from(r.size)).filter(|_| r.size == 4 || r.size == 8),
        Format::MachO => None,
        Format::Elf => {
            let ty = RelocationType(r.ty);
            let relative = if img.arm64 {
                matches!(ty, R_AARCH64_RELATIVE | R_AARCH64_IRELATIVE)
            } else if img.bits == 32 {
                matches!(ty, R_386_RELATIVE | R_386_IRELATIVE)
            } else {
                matches!(ty, R_X86_64_RELATIVE | R_X86_64_IRELATIVE)
            };
            match (relative, img.bits) {
                (false, _) => None,
                // REL: the addend is the word in the slot
                (true, 32) => read(4),
                (true, _) => Some(r.addend as u64),
            }
        }
    }
}
