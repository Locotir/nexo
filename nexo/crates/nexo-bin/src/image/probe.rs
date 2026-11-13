//! Telling what a file is before it is opened, and saying so in words when it is not readable.

use std::path::Path;

use object::read::pe::ImageNtHeaders;
use object::{pe, Architecture, BinaryFormat, Endianness, FileKind, LittleEndian as LE, Object};

use crate::Error;

/// The bytes of a file's start that [`opens`] looks at.
const HEAD: u64 = 4096;

/// Whether `Image::open` would take the file at `path`: an ELF64, PE32+ or Mach-O 64-bit file (not a
/// relocatable object), or an ELF64 arm64 one. 32-bit x86 files open too but are left out here: the
/// hints this answer feeds lead to the debugger, which runs 64-bit programs only. Only the headers in the first 4096 bytes are read, as bytes: the file is not
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
            h.endian().is_ok_and(|e| {
                matches!(h.e_machine(e), object::elf::EM_X86_64 | object::elf::EM_AARCH64)
                    && h.e_type(e) != object::elf::ET_REL
            })
        }),
        Ok(FileKind::Pe64) => {
            let Ok(dos) = pe::ImageDosHeader::parse(&head[..]) else { return false };
            let mut at = u64::from(dos.nt_headers_offset());
            pe::ImageNtHeaders64::parse(&head[..], &mut at)
                .is_ok_and(|(nt, _)| nt.file_header().machine.get(LE) == pe::IMAGE_FILE_MACHINE_AMD64)
        }
        Ok(FileKind::MachO64 | FileKind::MachOFat32 | FileKind::MachOFat64) => crate::macho::opens_head(&head),
        _ => false,
    }
}

pub(super) fn not_a_binary(data: &[u8]) -> Error {
    if data.is_empty() {
        return Error::NotRead("the file is empty, not a binary".into());
    }
    if let Some(w) = crate::damage::short_magic(data) {
        return Error::NotRead(format!("not a readable binary: it {w}"));
    }
    if data.starts_with(b"MZ") {
        if let Some(p) = crate::damage::extent_problem(data) {
            return Error::NotRead(format!("not a readable PE file: {p}"));
        }
    }
    let head = &data[..data.len().min(4096)];
    if std::str::from_utf8(head).is_ok() || head.iter().all(|b| b.is_ascii()) {
        Error::NotRead("this is a text file, not an ELF, PE or Mach-O binary".into())
    } else {
        Error::NotRead("not an ELF, PE or Mach-O binary (unknown file signature)".into())
    }
}

/// What was found, in words: "Mach-O arm64", "ELF32 x86".
pub(super) fn describe_kind(kind: FileKind, data: &[u8]) -> String {
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

/// An architecture in one word, as people write it.
pub(super) fn arch_word(a: Architecture) -> &'static str {
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

/// A parse error with the file's length and, when a header promises bytes the file lacks, where.
pub(super) fn explain(e: Error, data: &[u8]) -> Error {
    match e {
        Error::NotRead(m) => Error::NotRead(match crate::damage::extent_problem(data) {
            Some(p) => format!("{m}; {p}"),
            None => format!("{m} (the file is {} bytes)", data.len()),
        }),
        other => other,
    }
}
