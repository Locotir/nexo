//! Reading an ELF file.

use std::collections::HashMap;

use object::read::elf::FileHeader;
use object::{Architecture, Endianness, Object, ObjectSymbol, ObjectSymbolTable};

use super::load::{blank, common};
use super::names::{demangle, raw_of};
use super::probe::arch_word;
use super::{Format, Image, Import, MAX_IMPORTS, MAX_RELOCS};
use crate::{Error, Result};

/// The pointer the loader stores for a relocation, when it is one into the image: the base plus the
/// addend (`RELATIVE`), the resolver an `IRELATIVE` slot starts from, or a defined symbol plus the addend
/// (`R_X86_64_64`).
pub(crate) fn reloc_pointer(r_type: object::elf::RelocationType, defined: Option<u64>, addend: i64) -> Option<u64> {
    use object::elf::{
        R_AARCH64_ABS64, R_AARCH64_IRELATIVE, R_AARCH64_RELATIVE, R_X86_64_64, R_X86_64_IRELATIVE, R_X86_64_RELATIVE,
    };
    match r_type {
        R_X86_64_RELATIVE | R_X86_64_IRELATIVE | R_AARCH64_RELATIVE | R_AARCH64_IRELATIVE => Some(addend as u64),
        R_X86_64_64 | R_AARCH64_ABS64 => defined.map(|s| s.wrapping_add(addend as u64)),
        _ => None,
    }
}

pub(super) fn load_elf<Elf: FileHeader<Endian = Endianness>>(data: &[u8]) -> Result<Image> {
    let elf = object::read::elf::ElfFile::<Elf>::parse(data)
        .map_err(|e| Error::NotRead(format!("not a readable ELF file: {e}")))?;
    let bits = if elf.is_64() { 64 } else { 32 };
    match (bits, elf.architecture()) {
        (64, Architecture::X86_64 | Architecture::Aarch64) | (32, Architecture::I386) => {}
        (64, a) => {
            return Err(Error::NotRead(format!("ELF64 {} is not read yet (only x86-64 and arm64 are)", arch_word(a))))
        }
        (_, Architecture::X86_64_X32) => {
            return Err(Error::NotRead(
                "ELF32 x86-64 (the x32 ABI) is not read yet (32-bit ELF files are read for x86 only)".into(),
            ))
        }
        (_, a) => {
            return Err(Error::NotRead(format!(
                "ELF32 {} is not read yet (32-bit ELF files are read for x86 only)",
                arch_word(a)
            )))
        }
    }
    if elf.elf_header().e_type(elf.endian()) == object::elf::ET_REL {
        return Err(Error::NotRead(
            "ELF relocatable object is not read yet (a .o or .ko file has no addresses)".into(),
        ));
    }
    let mut img = blank(Format::Elf);
    img.bits = bits;
    img.arm64 = elf.architecture() == Architecture::Aarch64;
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
            // the numbers of relocation types are per machine; an ELF32 REL relocation keeps its value
            // in the slot itself, which the scan of 4-byte data words already reads
            let object::RelocationFlags::Elf { r_type } = r.flags() else { continue };
            if bits == 32 {
                continue;
            }
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
        DF_1_NOW, DF_BIND_NOW, DT_BIND_NOW, DT_FLAGS, DT_FLAGS_1, DT_PLTGOT, ET_DYN, PF_X, PT_DYNAMIC, PT_GNU_RELRO,
        PT_GNU_STACK,
    };
    use object::read::elf::{Dyn, ProgramHeader};
    let e = elf.endian();
    let mut base = u64::MAX;
    let (mut stack, mut relro, mut dynamic) = (None, false, None);
    for ph in elf.elf_program_headers() {
        let t = ph.p_type(e);
        if t == object::elf::PT_LOAD {
            base = base.min(ph.p_vaddr(e).into());
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
        let val: u64 = d.d_val(e).into();
        if tag == DT_PLTGOT && bits == 32 && val != 0 {
            img.got = Some(val);
        }
        if tag == DT_BIND_NOW
            || (tag == DT_FLAGS && val & DF_BIND_NOW.0 != 0)
            || (tag == DT_FLAGS_1 && val & DF_1_NOW.0 != 0)
        {
            now = true;
        }
    }
    img.image_base = if base == u64::MAX { 0 } else { base };
    let pie = elf.elf_header().e_type(e) == ET_DYN;
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
