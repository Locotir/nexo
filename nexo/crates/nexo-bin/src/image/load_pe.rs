//! Reading a PE file.

use object::read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile};
use object::{pe, Architecture, LittleEndian as LE, Object};

use super::load::{blank, common};
use super::names::undecorate_x86;
use super::probe::arch_word;
use super::{Format, Image, Import, MAX_IMPORTS, MAX_NAME, MAX_SECTIONS};
use crate::{Error, Result};

pub(super) fn load_pe<Pe: ImageNtHeaders>(data: &[u8]) -> Result<Image> {
    let pe = PeFile::<Pe>::parse(data).map_err(|e| Error::NotRead(format!("not a readable PE file: {e}")))?;
    let bits: u32 = if pe.is_64() { 64 } else { 32 };
    let (class, want) = if bits == 64 { ("PE32+", Architecture::X86_64) } else { ("PE32", Architecture::I386) };
    if pe.architecture() != want {
        return Err(Error::NotRead(format!(
            "{class} {} is not read yet (only {} is)",
            arch_word(pe.architecture()),
            arch_word(want)
        )));
    }
    let mut img = blank(Format::Pe);
    img.bits = bits;
    common(&pe, &mut img);
    if bits == 32 {
        for s in img.symbols.iter_mut().filter(|s| s.raw.is_empty()) {
            if let Some(n) = undecorate_x86(&s.name) {
                s.raw = std::mem::replace(&mut s.name, n);
            }
        }
    }
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
    // x86-32 has no exception directory of function ranges (its SEH is a chain on the stack)
    if let Some(d) = pe.data_directory(pe::IMAGE_DIRECTORY_ENTRY_EXCEPTION).filter(|_| bits == 64) {
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
                let width = u64::from(bits / 8);
                while let Ok(Some(t)) = thunks.next::<Pe>() {
                    let name = match table.import::<Pe>(t) {
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
                        slot: Some(img.image_base.wrapping_add(first).wrapping_add(i * width)),
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
    if bits == 32 {
        // a 64-bit address space only
        img.hardening.items.retain(|(k, _)| *k != "high_entropy_aslr");
    }
    Ok(img)
}
