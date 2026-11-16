//! What a compiled file is, in a few words, from its bytes: the facts nexo's tools give when a model meets one
//! (`read`, a shell command that reads it by hand, an `@` attachment, a listing). The kinds are the ones `nexo bin`
//! reads: ELF and PE for x86-64 and x86 (and ELF arm64), Mach-O 64-bit and universal, .NET assemblies, JVM classes
//! and jars, WebAssembly modules. A relocatable object, a core dump, a zip that is not a jar and every other file
//! are `None`.
//!
//! Only bytes are read: nothing is run or loaded. [`kind_of`] takes the bytes a caller already holds; [`kind_at`]
//! reads the first 4 KiB of a file (64 KiB for a PE, whose CLI header says whether it is .NET) and, for an ELF
//! file, its section headers wherever they are.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// What a compiled file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    /// Format, architecture, system and role: `ELF64 x86-64 Linux executable`, `PE32 x86 Windows DLL`,
    /// `.NET executable (PE32)`, `JVM class`, `jar`, `WebAssembly module`.
    pub what: String,
    /// The op of `nexo bin` that reads it first: `overview`, or `managed` for .NET and JVM code.
    pub op: &'static str,
    /// Whether it has no symbol table of its own: known for ELF and PE files only, and only when the bytes reach
    /// the section headers.
    pub stripped: Option<bool>,
}

impl Compiled {
    fn new(what: impl Into<String>, op: &'static str, stripped: Option<bool>) -> Compiled {
        Compiled { what: what.into(), op, stripped }
    }

    /// The facts as one phrase, with the size when known: `ELF64 x86-64 Linux executable, 1.2 MiB, stripped`.
    pub fn facts(&self, size: Option<u64>) -> String {
        let mut s = self.what.clone();
        if let Some(n) = size {
            s.push_str(", ");
            s.push_str(&size_words(n));
        }
        match self.stripped {
            Some(true) => s.push_str(", stripped"),
            Some(false) => s.push_str(", with symbols"),
            None => {}
        }
        s
    }
}

/// A size as people read it: `900 bytes`, `12.5 KiB`, `1.2 MiB`.
pub fn size_words(n: u64) -> String {
    const UNITS: [&str; 3] = ["KiB", "MiB", "GiB"];
    if n < 1024 {
        return format!("{n} bytes");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// The bytes [`kind_at`] reads first; a PE file is read on to the managed head, where its CLI header is.
const HEAD: u64 = 4096;
/// The largest ELF section header table read to tell a stripped file.
const MAX_TABLE: u64 = 1 << 20;

/// What the file whose first bytes (or whole bytes) are `data` is; `None` for anything `nexo bin` does not read.
/// Facts the bytes do not reach (the section headers of an ELF file cut short) are left unknown.
pub fn kind_of(data: &[u8]) -> Option<Compiled> {
    if data.starts_with(crate::wasm::MAGIC) {
        return Some(Compiled::new("WebAssembly module", "overview", None));
    }
    if crate::managed::is_class(data) {
        return Some(Compiled::new("JVM class", "managed", None));
    }
    if is_jar(data) {
        return Some(Compiled::new("jar", "managed", None));
    }
    if data.starts_with(b"\x7fELF") {
        return elf(data).map(|e| e.kind);
    }
    if data.starts_with(b"MZ") {
        return pe(data);
    }
    if crate::macho::opens_head(data) {
        return Some(macho(data));
    }
    None
}

/// What the file at `path` is (see [`kind_of`]). Regular files only, asked before opening: a named pipe nobody
/// writes to is never opened.
pub fn kind_at(path: &Path) -> Option<Compiled> {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return None;
    }
    let mut head = Vec::new();
    (&mut file).take(HEAD).read_to_end(&mut head).ok()?;
    if head.starts_with(b"MZ") {
        (&mut file).take(crate::MANAGED_HEAD as u64 - HEAD).read_to_end(&mut head).ok()?;
    }
    if !head.starts_with(b"\x7fELF") {
        return kind_of(&head);
    }
    let e = elf(&head)?;
    let mut kind = e.kind;
    if kind.stripped.is_none() {
        if let Some(t) = e.table.filter(|t| t.len <= MAX_TABLE) {
            let mut table = Vec::new();
            file.seek(SeekFrom::Start(t.at)).ok()?;
            (&mut file).take(t.len).read_to_end(&mut table).ok()?;
            if table.len() as u64 == t.len {
                kind.stripped = Some(!has_symtab(&table, t.entry, t.le));
            }
        }
    }
    Some(kind)
}

/// A zip whose first entry is a class or under `META-INF/`: a jar. Other zips (a .docx, an archive) are not.
fn is_jar(d: &[u8]) -> bool {
    if !d.starts_with(b"PK\x03\x04") {
        return false;
    }
    let len = d.get(26..28).map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])));
    d.get(30..30 + len).is_some_and(|name| name.starts_with(b"META-INF/") || name.ends_with(b".class"))
}

/// Where an ELF file's section headers are: what [`kind_at`] reads when the head does not reach them.
struct Table {
    at: u64,
    len: u64,
    entry: usize,
    le: bool,
}

struct Elf {
    kind: Compiled,
    table: Option<Table>,
}

fn elf(d: &[u8]) -> Option<Elf> {
    let wide = match d.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let le = match d.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let int = |o: usize, n: usize| -> Option<u64> {
        let b = d.get(o..o + n)?;
        let mut v = 0u64;
        for i in 0..n {
            let byte = if le { b[n - 1 - i] } else { b[i] };
            v = (v << 8) | u64::from(byte);
        }
        Some(v)
    };
    let arch = match (wide, int(18, 2)?) {
        (true, 62) => "x86-64",
        (true, 183) => "arm64",
        (false, 3) => "x86",
        _ => return None,
    };
    // the offsets of the ELF32 and ELF64 headers
    let (phoff, shoff, phent, phnum, shent, shnum) = if wide {
        (int(32, 8)?, int(40, 8)?, int(54, 2)?, int(56, 2)?, int(58, 2)?, int(60, 2)?)
    } else {
        (int(28, 4)?, int(32, 4)?, int(42, 2)?, int(44, 2)?, int(46, 2)?, int(48, 2)?)
    };
    // the program loader a dynamic program names (`PT_INTERP`), when its bytes are at hand
    let interp = (0..phnum.min(64)).find_map(|i| {
        let at = usize::try_from(phoff + i * phent).ok()?;
        if int(at, 4)? != 3 {
            return None;
        }
        let (off, len) = if wide { (int(at + 8, 8)?, int(at + 32, 8)?) } else { (int(at + 4, 4)?, int(at + 16, 4)?) };
        let (off, len) = (usize::try_from(off).ok()?, usize::try_from(len).ok()?);
        d.get(off..off.checked_add(len)?).map(|b| String::from_utf8_lossy(b).into_owned())
    });
    let role = match int(16, 2)? {
        2 => "executable",
        3 if interp.is_some() => "executable",
        3 => "shared library",
        _ => return None,
    };
    let os = match d.get(7)? {
        3 => Some("Linux"),
        9 => Some("FreeBSD"),
        _ => interp.as_deref().and_then(|i| {
            if i.contains("ld-linux") || i.contains("ld-musl") {
                Some("Linux")
            } else if i.starts_with("/system/bin/linker") {
                Some("Android")
            } else if i.contains("ld-elf.so") {
                Some("FreeBSD")
            } else {
                None
            }
        }),
    };
    let bits = if wide { "ELF64" } else { "ELF32" };
    let what = match os {
        Some(os) => format!("{bits} {arch} {os} {role}"),
        None => format!("{bits} {arch} {role}"),
    };
    let entry = usize::try_from(shent).ok().filter(|e| *e >= 8);
    let (stripped, table) = match entry {
        // no section at all: the headers were cut off with the symbols
        _ if shoff == 0 && shnum == 0 => (Some(true), None),
        Some(entry) if shnum > 0 => {
            let len = shnum * shent;
            let inside = usize::try_from(shoff).ok().and_then(|a| d.get(a..a.checked_add(len as usize)?));
            match inside {
                Some(t) => (Some(!has_symtab(t, entry, le)), None),
                None => (None, Some(Table { at: shoff, len, entry, le })),
            }
        }
        _ => (None, None),
    };
    Some(Elf { kind: Compiled::new(what, "overview", stripped), table })
}

/// Whether a section header table holds a symbol table (`SHT_SYMTAB`), the one `strip` takes out.
fn has_symtab(table: &[u8], entry: usize, le: bool) -> bool {
    table.chunks_exact(entry).any(|h| {
        let t = [h[4], h[5], h[6], h[7]];
        (if le { u32::from_le_bytes(t) } else { u32::from_be_bytes(t) }) == 2
    })
}

fn pe(d: &[u8]) -> Option<Compiled> {
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let at = usize::try_from(u32_at(0x3c)?).ok()?;
    if d.get(at..at.checked_add(4)?) != Some(b"PE\0\0") {
        return None;
    }
    let (machine, symbols, chars) = (u16_at(at + 4)?, u32_at(at + 16)?, u16_at(at + 22)?);
    let opt = at + 24;
    let wide = match u16_at(opt)? {
        0x10b => false,
        0x20b => true,
        _ => return None,
    };
    let bits = if wide { "PE32+" } else { "PE32" };
    let role = if chars & 0x2000 != 0 { "DLL" } else { "executable" };
    // the CLI header's directory (the 15th) says the file holds .NET code
    let (count, dirs) = if wide { (opt + 108, opt + 112) } else { (opt + 92, opt + 96) };
    let clr = u32_at(count).is_some_and(|n| n > 14) && u32_at(dirs + 14 * 8).is_some_and(|rva| rva != 0);
    let head = &d[..d.len().min(crate::MANAGED_HEAD)];
    if clr && !crate::is_mixed_mode(head) {
        return Some(Compiled::new(format!(".NET {role} ({bits})"), "managed", None));
    }
    let arch = match (wide, machine) {
        (true, 0x8664) => "x86-64",
        (false, 0x14c) => "x86",
        _ => return None,
    };
    // C++/CLI: the native ops read a PE32+ one, `managed` a PE32 one
    if clr {
        let op = if wide { "overview" } else { "managed" };
        return Some(Compiled::new(format!("{bits} {arch} Windows {role}, mixed-mode .NET"), op, None));
    }
    Some(Compiled::new(format!("{bits} {arch} Windows {role}"), "overview", Some(symbols == 0)))
}

fn macho(d: &[u8]) -> Compiled {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if !d.starts_with(&[0xcf, 0xfa, 0xed, 0xfe]) {
        return Compiled::new("Mach-O universal binary", "overview", None);
    }
    let arch = if u32_at(4) == Some(0x0100_000c) { "arm64" } else { "x86-64" };
    let role = match u32_at(12) {
        Some(2) => "executable",
        Some(6) => "dylib",
        Some(8) => "bundle",
        _ => "file",
    };
    Compiled::new(format!("Mach-O 64-bit {arch} {role}"), "overview", None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_test_program_is_a_native_program_and_its_symbols_are_seen() {
        let exe = std::env::current_exe().unwrap();
        let kind = kind_at(&exe).expect("the test program is read");
        let whole = kind_of(&std::fs::read(&exe).unwrap()).unwrap();
        assert_eq!(kind, whole, "the head and the section headers say what the whole file says");
        assert_eq!(kind.op, "overview");
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(kind.what, "ELF64 x86-64 Linux executable");
        }
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(kind.what, "PE32+ x86-64 Windows executable");
        }
        // only what the bytes reach: the first 4 KiB of an ELF file do not hold its section headers
        let head = &std::fs::read(&exe).unwrap()[..4096];
        assert_eq!(kind_of(head).map(|k| k.what), Some(kind.what));
    }

    #[test]
    fn the_managed_kinds_and_webassembly_are_named_and_other_files_are_not() {
        assert_eq!(kind_of(b"\0asm\x01\0\0\0").unwrap().what, "WebAssembly module");
        let class = kind_of(&[0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52]).unwrap();
        assert_eq!((class.what.as_str(), class.op), ("JVM class", "managed"));
        let mut jar = b"PK\x03\x04".to_vec();
        jar.resize(26, 0);
        jar.extend_from_slice(&9u16.to_le_bytes());
        jar.extend_from_slice(&[0, 0]);
        jar.extend_from_slice(b"META-INF/");
        assert_eq!(kind_of(&jar).unwrap().what, "jar");
        let docx = [&jar[..30], b"[Content_Types].xml"].concat();
        assert_eq!(kind_of(&docx), None, "a zip that is not a jar");
        assert_eq!(kind_of(b"#!/bin/sh\necho hi\n"), None);
        assert_eq!(kind_of(b""), None);
        // a PE32 AnyCPU assembly: its CLI header (IL only) is at 0x200, in the one section
        let mut net = vec![0u8; 0x400];
        net[..2].copy_from_slice(b"MZ");
        net[0x3c] = 0x80;
        net[0x80..0x84].copy_from_slice(b"PE\0\0");
        net[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
        net[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        net[0x94..0x96].copy_from_slice(&224u16.to_le_bytes());
        net[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
        net[0x98 + 92..0x98 + 96].copy_from_slice(&16u32.to_le_bytes());
        net[0x98 + 96 + 112..0x98 + 96 + 116].copy_from_slice(&0x1000u32.to_le_bytes());
        net[0x98 + 96 + 116..0x98 + 96 + 120].copy_from_slice(&72u32.to_le_bytes());
        for (o, v) in [(8, 0x200u32), (12, 0x1000), (16, 0x200), (20, 0x200)] {
            net[0x98 + 224 + o..0x98 + 224 + o + 4].copy_from_slice(&v.to_le_bytes());
        }
        net[0x210..0x214].copy_from_slice(&1u32.to_le_bytes());
        let k = kind_of(&net).unwrap();
        assert_eq!((k.what.as_str(), k.op), (".NET executable (PE32)", "managed"));
        // the same file without its CLI header directory is a 32-bit Windows program
        net[0x98 + 96 + 112..0x98 + 96 + 116].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(kind_of(&net).unwrap().facts(None), "PE32 x86 Windows executable, stripped");
    }

    #[test]
    fn a_32_bit_program_and_a_stripped_one_are_told() {
        // an ELF32 x86 executable with no section headers: what `strip` and `sstrip` leave
        let mut e = vec![0u8; 52];
        e[..4].copy_from_slice(b"\x7fELF");
        (e[4], e[5], e[6], e[7]) = (1, 1, 1, 3);
        e[16..18].copy_from_slice(&2u16.to_le_bytes());
        e[18..20].copy_from_slice(&3u16.to_le_bytes());
        let k = kind_of(&e).unwrap();
        assert_eq!(k.facts(Some(52)), "ELF32 x86 Linux executable, 52 bytes, stripped");
        // a relocatable object is not a program
        e[16..18].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(kind_of(&e), None);
        assert_eq!(size_words(1536), "1.5 KiB");
        assert_eq!(size_words(3 << 20), "3.0 MiB");
    }
}
