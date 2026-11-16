//! Telling what a file is before it is opened, and saying so in words when it is not readable.

use object::{Architecture, BinaryFormat, FileKind, Object};

use crate::Error;

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
