//! Mach-O files (macOS, iOS): thin 64-bit x86-64 and arm64 files, and one slice of a universal (fat)
//! file, picked with `--arch` or else the first one that is x86-64 or arm64.
//!
//! Read: segments and sections, the symbol table (the `_` every C name carries in front is taken off; the
//! name as written stays searchable), imports with the slot the loader fills (indirect symbol table,
//! chained fixups or classic bind info), one function `name@plt` per stub (the name ELF gives the same
//! thing, so xref, calls and decomp treat both alike), the entry from LC_MAIN (which is main itself), the
//! function starts LC_FUNCTION_STARTS lists for stripped files, and the exports trie. Bytes only.

use std::collections::{HashMap, HashSet};

use object::read::macho::{DyldChainedFixups, Fixup};
use object::LittleEndian as LE;
use serde_json::{json, Value};

use crate::image::{blank, demangle, raw_of, Export, Format, Image, Import, Section, Symbol};
use crate::{Error, Result};

const MAX_SLICES: usize = 64;
const MAX_CMDS: u32 = 16_384;
const MAX_SECTIONS: usize = 4096;
const MAX_SYMBOLS: usize = 500_000;
const MAX_IMPORTS: usize = 200_000;
const MAX_FIXUPS: usize = 4_000_000;
const MAX_STARTS: usize = 1_000_000;
const MAX_TRIE_NODES: usize = 500_000;
const MAX_NAME_IN: usize = 4096;
/// The longest stub any linker writes is 16 bytes (arm64e); more is a damaged header.
const MAX_STUB: u64 = 64;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_CIGAM_64: u32 = 0xcffa_edfe;
const MH_MAGIC: u32 = 0xfeed_face;
const MH_CIGAM: u32 = 0xcefa_edfe;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;

const CPU_X86_64: u32 = 0x0100_0007;
const CPU_ARM64: u32 = 0x0100_000c;
const CPU_SUBTYPE_MASK: u32 = 0x00ff_ffff;
const CPU_SUBTYPE_ARM64E: u32 = 2;
const CPU_SUBTYPE_X86_64_H: u32 = 8;

const LC_REQ_DYLD: u32 = 0x8000_0000;
const LC_SYMTAB: u32 = 0x2;
const LC_UNIXTHREAD: u32 = 0x5;
const LC_DYSYMTAB: u32 = 0xb;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_ID_DYLIB: u32 = 0xd;
const LC_LOAD_WEAK_DYLIB: u32 = 0x18 | LC_REQ_DYLD;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UUID: u32 = 0x1b;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const LC_REEXPORT_DYLIB: u32 = 0x1f | LC_REQ_DYLD;
const LC_LAZY_LOAD_DYLIB: u32 = 0x20;
const LC_DYLD_INFO: u32 = 0x22;
const LC_DYLD_INFO_ONLY: u32 = 0x22 | LC_REQ_DYLD;
const LC_LOAD_UPWARD_DYLIB: u32 = 0x23 | LC_REQ_DYLD;
const LC_VERSION_MIN_MACOSX: u32 = 0x24;
const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
const LC_FUNCTION_STARTS: u32 = 0x26;
const LC_MAIN: u32 = 0x28 | LC_REQ_DYLD;
const LC_ENCRYPTION_INFO_64: u32 = 0x2c;
const LC_VERSION_MIN_TVOS: u32 = 0x2f;
const LC_VERSION_MIN_WATCHOS: u32 = 0x30;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x33 | LC_REQ_DYLD;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x34 | LC_REQ_DYLD;

const S_ZEROFILL: u32 = 0x1;
const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
const S_LAZY_SYMBOL_POINTERS: u32 = 0x7;
const S_SYMBOL_STUBS: u32 = 0x8;
const S_GB_ZEROFILL: u32 = 0xc;
const S_LAZY_DYLIB_SYMBOL_POINTERS: u32 = 0x10;
const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x400;
const S_ATTR_DEBUG: u32 = 0x0200_0000;

const N_STAB: u8 = 0xe0;
const N_PEXT: u8 = 0x10;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;
const N_UNDF: u8 = 0x0;
const N_SECT: u8 = 0xe;
const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;

const MH_ALLOW_STACK_EXECUTION: u32 = 0x2_0000;
const MH_PIE: u32 = 0x20_0000;

/// What a Mach-O file says beyond what every format has.
#[derive(Debug, Clone, Default)]
pub(crate) struct Macho {
    /// `executable`, `dynamic library` and so on.
    pub(crate) kind: &'static str,
    /// The address LC_MAIN names: the program's own main, which the loader calls.
    pub(crate) main: Option<u64>,
    /// Every slice of a universal file (empty for a thin file), and the one read.
    pub(crate) slices: Vec<Slice>,
    pub(crate) picked: usize,
    /// The `--arch` the person gave, repeated in the next commands.
    pub(crate) arch_arg: Option<String>,
    /// The bytes of the file the sha256 covers: the slice read.
    pub(crate) hashed: (usize, usize),
    /// Libraries loaded (LC_LOAD_DYLIB and kin), in ordinal order.
    pub(crate) libs: Vec<String>,
    /// The library's own install name (LC_ID_DYLIB).
    pub(crate) id: Option<String>,
    /// `macOS 14.0`, from LC_BUILD_VERSION or LC_VERSION_MIN_*.
    pub(crate) platform: Option<String>,
    /// `chained`, `classic` or `none`: how the loader fills pointers.
    pub(crate) fixups: &'static str,
    /// Function starts from LC_FUNCTION_STARTS.
    pub(crate) starts: Vec<u64>,
    pub(crate) uuid: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Slice {
    pub(crate) arch: String,
    pub(crate) off: u64,
    pub(crate) size: u64,
}

fn u32_at(b: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}
fn be32(b: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_be_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn be64(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_be_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// `n` bytes at `o`, cut at the end of `b` (empty when `o` is past it).
fn bytes_at(b: &[u8], o: u64, n: u64) -> &[u8] {
    let start = usize::try_from(o).unwrap_or(usize::MAX).min(b.len());
    let end = start.saturating_add(usize::try_from(n).unwrap_or(usize::MAX)).min(b.len());
    &b[start..end]
}

/// The NUL-terminated text at `o` (at most `max` bytes), lossily as UTF-8.
fn cstr(b: &[u8], o: u64, max: usize) -> String {
    let s = bytes_at(b, o, max as u64);
    let s = &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())];
    String::from_utf8_lossy(s).into_owned()
}

/// A 16-byte segment or section name.
fn name16(b: &[u8]) -> String {
    cstr(b, 0, 16)
}

fn uleb(b: &[u8], p: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*p)?;
        *p += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The architecture of a CPU type, as people write it.
fn cpu_word(cpu: u32, sub: u32) -> String {
    match (cpu, sub & CPU_SUBTYPE_MASK) {
        (CPU_X86_64, CPU_SUBTYPE_X86_64_H) => "x86-64h",
        (CPU_X86_64, _) => "x86-64",
        (CPU_ARM64, CPU_SUBTYPE_ARM64E) => "arm64e",
        (CPU_ARM64, _) => "arm64",
        (0x0200_000c, _) => "arm64_32",
        (7, _) => "x86",
        (12, _) => "arm",
        (18, _) => "ppc",
        (0x0100_0012, _) => "ppc64",
        _ => return format!("cpu {cpu:#x}"),
    }
    .into()
}

/// `--arch` in the words `cpu_word` uses; `None` for a word that names no architecture read.
fn arch_key(a: &str) -> Option<&'static str> {
    match a.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "x86-64" | "amd64" | "x64" => Some("x86-64"),
        "x86-64h" => Some("x86-64h"),
        "arm64" | "aarch64" => Some("arm64"),
        "arm64e" => Some("arm64e"),
        _ => None,
    }
}

fn family(word: &str) -> &str {
    match word {
        "x86-64h" => "x86-64",
        "arm64e" => "arm64",
        w => w,
    }
}

fn bad_arch(a: &str) -> Error {
    let a: String = a.chars().take(40).collect();
    Error::Usage(format!("--arch {a:?}: write x86-64 or arm64 (arm64e and x86-64h pick those slices exactly)"))
}

/// Read a thin Mach-O file or one slice of a universal one.
pub(crate) fn load(data: &[u8], arch: Option<&str>) -> Result<Image> {
    match be32(data, 0) {
        Some(FAT_MAGIC | FAT_MAGIC_64) => load_fat(data, arch),
        _ => {
            let img = load_thin(data, 0)?;
            if let Some(a) = arch {
                check_arch(&img, a)?;
            }
            Ok(img)
        }
    }
}

/// `--arch` on a file that is not universal: it must name the file's own architecture.
pub(crate) fn check_arch(img: &Image, a: &str) -> Result<()> {
    let key = arch_key(a).ok_or_else(|| bad_arch(a))?;
    if family(key) != img.arch_name() {
        return Err(Error::Usage(format!(
            "--arch {a}: this file holds {} code only; --arch picks a slice of a universal (fat) Mach-O file",
            img.arch_name()
        )));
    }
    Ok(())
}

fn load_fat(data: &[u8], arch: Option<&str>) -> Result<Image> {
    let wide = be32(data, 0) == Some(FAT_MAGIC_64);
    let cut = || Error::NotRead("not a readable Mach-O universal file: its slice table is cut short".into());
    let n = be32(data, 4).ok_or_else(cut)? as usize;
    if n == 0 {
        return Err(Error::NotRead("a Mach-O universal file with no slices: nothing to read".into()));
    }
    if n > MAX_SLICES {
        return Err(Error::NotRead(format!(
            "not a readable Mach-O universal file: it claims {n} slices (real ones have a handful)"
        )));
    }
    let entry = if wide { 32 } else { 20 };
    let mut slices = Vec::with_capacity(n);
    let mut cpus = Vec::with_capacity(n);
    for i in 0..n as u64 {
        let h = 8 + i * entry;
        let (cpu, sub) = (be32(data, h).ok_or_else(cut)?, be32(data, h + 4).ok_or_else(cut)?);
        let (off, size) = if wide {
            (be64(data, h + 8).ok_or_else(cut)?, be64(data, h + 16).ok_or_else(cut)?)
        } else {
            (u64::from(be32(data, h + 8).ok_or_else(cut)?), u64::from(be32(data, h + 12).ok_or_else(cut)?))
        };
        slices.push(Slice { arch: cpu_word(cpu, sub), off, size });
        cpus.push(cpu);
    }
    let list = slice_list(&slices);
    let pick = match arch {
        Some(a) => {
            let key = arch_key(a).ok_or_else(|| bad_arch(a))?;
            slices
                .iter()
                .position(|s| s.arch == key)
                .or_else(|| slices.iter().position(|s| family(&s.arch) == key))
                .ok_or_else(|| {
                    Error::Usage(format!("--arch {a}: this universal file holds {list}; pick one of those"))
                })?
        }
        None => cpus.iter().position(|&c| c == CPU_X86_64 || c == CPU_ARM64).ok_or_else(|| {
            Error::NotRead(format!(
                "Mach-O universal binary with {list}: none of them is x86-64 or arm64, the ones read"
            ))
        })?,
    };
    let s = &slices[pick];
    let start = usize::try_from(s.off).ok().filter(|&o| o < data.len()).ok_or_else(|| {
        Error::NotRead(format!(
            "not a readable Mach-O universal file: its {} slice starts at {:#x}, past the end of the file",
            s.arch, s.off
        ))
    })?;
    let end = start.saturating_add(usize::try_from(s.size).unwrap_or(usize::MAX)).min(data.len());
    let mut img = load_thin(&data[start..end], start as u64)
        .map_err(|e| prefix(e, &format!("the {} slice at {:#x}: ", s.arch, s.off)))?;
    if let Some(m) = img.macho.as_mut() {
        m.slices = slices;
        m.picked = pick;
        m.arch_arg = arch.map(|_| m.slices[pick].arch.clone());
    }
    Ok(img)
}

/// A load error with where the file is cut, when its headers promise bytes it does not have.
pub(crate) fn with_extent(e: Error, data: &[u8]) -> Error {
    match (e, crate::damage::extent_problem(data)) {
        (Error::NotRead(m), Some(p)) => Error::NotRead(format!("{m}; {p}")),
        (e, _) => e,
    }
}

/// The architectures of a universal file, the first 8 and a count of the rest.
fn slice_list(slices: &[Slice]) -> String {
    let mut s = slices.iter().take(8).map(|s| s.arch.as_str()).collect::<Vec<_>>().join(", ");
    if slices.len() > 8 {
        s.push_str(&format!(" and {} more", slices.len() - 8));
    }
    s
}

fn prefix(e: Error, p: &str) -> Error {
    match e {
        Error::NotRead(m) => Error::NotRead(format!("{p}{m}")),
        other => other,
    }
}

/// What a segment command says.
struct Seg {
    name: String,
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    prot: u32,
}

/// One entry of the symbol table.
struct Nlist {
    raw: String,
    ntype: u8,
    sect: u8,
    desc: u16,
    value: u64,
}

impl Nlist {
    fn kind(&self) -> u8 {
        self.ntype & N_TYPE
    }
    fn is_import(&self) -> bool {
        self.ntype & N_STAB == 0 && self.kind() == N_UNDF && self.ntype & N_EXT != 0 && self.value == 0
    }
}

/// The name shown for a name as written: one `_` off the front, then demangled.
fn shown(raw: &str) -> String {
    demangle(raw.strip_prefix('_').unwrap_or(raw))
}

/// The last part of a library path: `libSystem.B.dylib`, `Foundation`.
fn short_lib(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Where the linkedit data of one load command is: (offset, size), both from the start of the slice.
type Range = (u64, u64);

#[derive(Default)]
struct Cmds {
    symtab: Option<(u64, u64, u64, u64)>,
    indirect: Option<Range>,
    main_off: Option<u64>,
    thread_pc: Option<u64>,
    function_starts: Option<Range>,
    chained: Option<Range>,
    trie: Option<Range>,
    /// Offset of an LC_DYLD_INFO(_ONLY) command in the slice.
    dyld_info: Option<usize>,
    signed: bool,
    encrypted: Option<(u64, u64)>,
}

fn version(v: u32) -> String {
    let (a, b, c) = (v >> 16, (v >> 8) & 0xff, v & 0xff);
    if c == 0 {
        format!("{a}.{b}")
    } else {
        format!("{a}.{b}.{c}")
    }
}

fn platform(p: u32) -> String {
    match p {
        1 => "macOS".into(),
        2 => "iOS".into(),
        3 => "tvOS".into(),
        4 => "watchOS".into(),
        5 => "bridgeOS".into(),
        6 => "Mac Catalyst".into(),
        7 => "iOS simulator".into(),
        8 => "tvOS simulator".into(),
        9 => "watchOS simulator".into(),
        10 => "DriverKit".into(),
        11 => "visionOS".into(),
        12 => "visionOS simulator".into(),
        p => format!("platform {p}"),
    }
}

fn load_thin(d: &[u8], base: u64) -> Result<Image> {
    let magic =
        u32_at(d, 0).ok_or_else(|| Error::NotRead("not a readable Mach-O file: it is cut in its magic".into()))?;
    match magic {
        MH_MAGIC_64 => {}
        MH_CIGAM_64 => {
            return Err(Error::NotRead(
                "big-endian Mach-O (PowerPC) is not read (only 64-bit x86-64 and arm64 are)".into(),
            ))
        }
        MH_MAGIC | MH_CIGAM => {
            return Err(Error::NotRead("32-bit Mach-O is not read (only 64-bit x86-64 and arm64 are)".into()))
        }
        m => return Err(Error::NotRead(format!("not a readable Mach-O file: unknown magic {m:#010x}"))),
    }
    if d.len() < 32 {
        return Err(Error::NotRead(format!(
            "not a readable Mach-O file: it is {} bytes and its header alone is 32 bytes",
            d.len()
        )));
    }
    let word = |o: u64| u32_at(d, o).unwrap_or(0);
    let (cpu, sub, ftype, ncmds, sizeofcmds, flags) = (word(4), word(8), word(12), word(16), word(20), word(24));
    if cpu != CPU_X86_64 && cpu != CPU_ARM64 {
        return Err(Error::NotRead(format!(
            "Mach-O {} is not read yet (only x86-64 and arm64 are)",
            cpu_word(cpu, sub)
        )));
    }
    let kind = match ftype {
        2 => "executable",
        3 => "fixed VM library",
        5 => "preloaded executable",
        6 => "dynamic library",
        7 => "dynamic linker",
        8 => "bundle",
        9 => "dynamic library stub",
        0xb => "kernel extension",
        1 => {
            return Err(Error::NotRead(
                "Mach-O object file (.o) is not read yet: it has no final addresses (read the linked program)".into(),
            ))
        }
        4 => return Err(Error::NotRead("a Mach-O core dump is not read (it is a memory image, not a program)".into())),
        0xa => {
            return Err(Error::NotRead(
                "a dSYM file holds debug info but no code: read the program it belongs to".into(),
            ))
        }
        0xc => return Err(Error::NotRead("a Mach-O kernel collection (fileset) is not read yet".into())),
        t => return Err(Error::NotRead(format!("Mach-O file type {t} is unknown; not read"))),
    };
    let mut img = blank(Format::MachO);
    img.arm64 = cpu == CPU_ARM64;
    let mut m = Macho { kind, hashed: (base as usize, d.len()), fixups: "none", ..Macho::default() };
    let mut segs: Vec<Seg> = Vec::new();
    // section flags, reserved1 and reserved2, by section (same order as img.sections)
    let mut extra: Vec<(u32, u32, u32)> = Vec::new();
    let mut c = Cmds::default();
    let end = (32 + u64::from(sizeofcmds)).min(d.len() as u64);
    let mut off = 32u64;
    for i in 0..ncmds.min(MAX_CMDS) {
        let (Some(cmd), Some(size)) = (u32_at(d, off).filter(|_| off + 8 <= end), u32_at(d, off + 4)) else {
            img.warnings.push(format!(
                "the header promises {ncmds} load commands but only {i} fit in its {sizeofcmds}-byte command area"
            ));
            break;
        };
        let size = u64::from(size);
        if size < 8 || off + size > end {
            img.warnings.push(format!(
                "load command #{i} at {off:#x} says it is {size} bytes, which leaves the command area; the commands after it are not read"
            ));
            break;
        }
        let b = &d[off as usize..(off + size) as usize];
        let f32 = |o: u64| u32_at(b, o);
        let f64 = |o: u64| u64_at(b, o);
        let linkedit = || Some((u64::from(f32(8)?), u64::from(f32(12)?)));
        match cmd {
            LC_SEGMENT_64 if b.len() >= 72 => {
                let seg = Seg {
                    name: name16(&b[8..24]),
                    vmaddr: f64(24).unwrap_or(0),
                    vmsize: f64(32).unwrap_or(0),
                    fileoff: f64(40).unwrap_or(0),
                    filesize: f64(48).unwrap_or(0),
                    prot: f32(60).unwrap_or(0),
                };
                let nsects = f32(64).unwrap_or(0) as usize;
                let room = (b.len() - 72) / 80;
                if nsects > room {
                    img.warnings.push(format!(
                        "segment {} claims {nsects} sections but its command holds {room}; the rest are not read",
                        seg.name
                    ));
                }
                for k in 0..nsects.min(room) {
                    if img.sections.len() >= MAX_SECTIONS {
                        break;
                    }
                    let s = &b[72 + k * 80..72 + (k + 1) * 80];
                    let at = |o: u64| u32_at(s, o).unwrap_or(0);
                    let (sect, segname) = (name16(&s[0..16]), name16(&s[16..32]));
                    let (addr, size) = (u64_at(s, 32).unwrap_or(0), u64_at(s, 40).unwrap_or(0));
                    let (offset, sflags) = (u64::from(at(48)), at(64));
                    let ty = sflags & 0xff;
                    let zerofill = matches!(ty, S_ZEROFILL | S_GB_ZEROFILL | S_THREAD_LOCAL_ZEROFILL);
                    let code = sflags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0
                        || ty == S_SYMBOL_STUBS
                        || sect == "__text";
                    let file_size =
                        if zerofill || offset == 0 { 0 } else { size.min((d.len() as u64).saturating_sub(offset)) };
                    img.sections.push(Section {
                        index: img.sections.len() + 1,
                        name: format!("{segname},{sect}"),
                        addr,
                        size: size.min(u64::MAX - addr),
                        file_off: if file_size == 0 { 0 } else { base + offset },
                        file_size,
                        read: seg.prot & 1 != 0,
                        write: seg.prot & 2 != 0,
                        exec: seg.prot & 4 != 0 && code,
                        alloc: sflags & S_ATTR_DEBUG == 0 && segname != "__DWARF",
                    });
                    extra.push((sflags, at(68), at(72)));
                }
                if segs.len() < MAX_SECTIONS {
                    segs.push(seg);
                }
            }
            LC_SYMTAB => {
                if let (Some(a), Some(n), Some(s), Some(z)) = (f32(8), f32(12), f32(16), f32(20)) {
                    c.symtab = Some((a.into(), n.into(), s.into(), z.into()));
                }
            }
            LC_DYSYMTAB => {
                if let (Some(a), Some(n)) = (f32(56), f32(60)) {
                    c.indirect = Some((a.into(), n.into()));
                }
            }
            LC_MAIN => c.main_off = f64(8),
            LC_UNIXTHREAD => {
                // x86_THREAD_STATE64 (4): rip is the 17th register; ARM_THREAD_STATE64 (6): pc is the 33rd
                c.thread_pc = match (f32(8), img.arm64) {
                    (Some(4), false) => f64(16 + 16 * 8),
                    (Some(6), true) => f64(16 + 32 * 8),
                    _ => None,
                };
            }
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB | LC_LAZY_LOAD_DYLIB | LC_LOAD_UPWARD_DYLIB => {
                if m.libs.len() < 4096 {
                    m.libs.push(f32(8).map(|o| cstr(b, o.into(), 1024)).unwrap_or_default());
                }
            }
            LC_ID_DYLIB => m.id = f32(8).map(|o| cstr(b, o.into(), 1024)),
            LC_UUID if b.len() >= 24 => {
                let u: String = b[8..24].iter().map(|x| format!("{x:02x}")).collect();
                m.uuid = Some(format!("{}-{}-{}-{}-{}", &u[..8], &u[8..12], &u[12..16], &u[16..20], &u[20..]));
            }
            LC_CODE_SIGNATURE => c.signed = true,
            LC_FUNCTION_STARTS => c.function_starts = linkedit(),
            LC_DYLD_CHAINED_FIXUPS => c.chained = linkedit(),
            LC_DYLD_EXPORTS_TRIE => c.trie = linkedit(),
            LC_DYLD_INFO | LC_DYLD_INFO_ONLY if b.len() >= 48 => c.dyld_info = Some(off as usize),
            LC_ENCRYPTION_INFO_64 => {
                if let (Some(o), Some(n), Some(id)) = (f32(8), f32(12), f32(16)) {
                    if id != 0 {
                        c.encrypted = Some((o.into(), n.into()));
                    }
                }
            }
            LC_BUILD_VERSION => {
                if let (Some(p), Some(v)) = (f32(8), f32(12)) {
                    m.platform = Some(format!("{} {}", platform(p), version(v)));
                }
            }
            LC_VERSION_MIN_MACOSX | LC_VERSION_MIN_IPHONEOS | LC_VERSION_MIN_TVOS | LC_VERSION_MIN_WATCHOS => {
                let os = match cmd {
                    LC_VERSION_MIN_MACOSX => "macOS",
                    LC_VERSION_MIN_IPHONEOS => "iOS",
                    LC_VERSION_MIN_TVOS => "tvOS",
                    _ => "watchOS",
                };
                if m.platform.is_none() {
                    m.platform = f32(8).map(|v| format!("{os} {}", version(v)));
                }
            }
            _ => {}
        }
        off += size;
    }
    if img.sections.is_empty() {
        // no sections: the mapped segments stand in
        for g in segs.iter().filter(|g| g.vmsize > 0 && g.prot != 0 && g.name != "__LINKEDIT") {
            let file_size = g.filesize.min(g.vmsize).min((d.len() as u64).saturating_sub(g.fileoff));
            img.sections.push(Section {
                index: img.sections.len() + 1,
                name: g.name.clone(),
                addr: g.vmaddr,
                size: g.vmsize.min(u64::MAX - g.vmaddr),
                file_off: if file_size == 0 { 0 } else { base + g.fileoff },
                file_size,
                read: g.prot & 1 != 0,
                write: g.prot & 2 != 0,
                exec: g.prot & 4 != 0,
                alloc: true,
            });
            extra.push((0, 0, 0));
        }
    }
    img.image_base = segs
        .iter()
        .find(|g| g.name == "__TEXT")
        .or_else(|| segs.iter().find(|g| g.fileoff == 0 && g.filesize > 0))
        .map_or(0, |g| g.vmaddr);
    // LC_MAIN gives a file offset; the segment that maps it gives the address
    if let Some(o) = c.main_off {
        match segs.iter().find(|g| o >= g.fileoff && o - g.fileoff < g.filesize) {
            Some(g) => {
                let a = g.vmaddr.wrapping_add(o - g.fileoff);
                m.main = Some(a);
                img.entry = a;
            }
            None => {
                img.warnings.push(format!("LC_MAIN names file offset {o:#x}, which no segment maps; entry unknown"))
            }
        }
    } else if let Some(pc) = c.thread_pc {
        img.entry = pc;
    }

    // the symbol table
    let mut nl: Vec<Nlist> = Vec::new();
    if let Some((symoff, nsyms, stroff, strsize)) = c.symtab {
        let strtab = bytes_at(d, stroff, strsize);
        let table = bytes_at(d, symoff, nsyms.saturating_mul(16));
        for e in table.as_chunks::<16>().0.iter().take(MAX_SYMBOLS) {
            let strx = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            nl.push(Nlist {
                raw: cstr(strtab, strx.into(), MAX_NAME_IN),
                ntype: e[4],
                sect: e[5],
                desc: u16::from_le_bytes([e[6], e[7]]),
                value: u64::from_le_bytes([e[8], e[9], e[10], e[11], e[12], e[13], e[14], e[15]]),
            });
        }
    }
    let defined = |s: &Nlist| s.ntype & N_STAB == 0 && s.kind() == N_SECT && !s.raw.is_empty();
    // external names first: where a local label shares the address, the exported name is the one shown
    for ext in [true, false] {
        for s in nl.iter().filter(|s| defined(s) && (s.ntype & N_EXT != 0) == ext) {
            let func = img.sections.get(usize::from(s.sect).wrapping_sub(1)).is_some_and(|x| x.exec);
            let name = shown(&s.raw);
            img.symbols.push(Symbol { raw: raw_of(&s.raw, &name), name, addr: s.value, size: 0, func });
        }
    }
    let lib_of =
        |ordinal: Option<u32>| ordinal.and_then(|o| m.libs.get((o as usize).wrapping_sub(1))).map(|p| short_lib(p));
    // imports: the undefined names, then the slots the loader fills for them
    let mut by_raw: HashMap<String, usize> = HashMap::new();
    let mut of_sym: HashMap<u32, usize> = HashMap::new();
    for (i, s) in nl.iter().enumerate() {
        if !s.is_import() || s.raw.is_empty() || img.imports.len() >= MAX_IMPORTS {
            continue;
        }
        let k = *by_raw.entry(s.raw.clone()).or_insert_with(|| {
            let name = shown(&s.raw);
            let ordinal = u32::from(s.desc >> 8);
            img.imports.push(Import { lib: lib_of(Some(ordinal)), raw: raw_of(&s.raw, &name), name, slot: None });
            img.imports.len() - 1
        });
        of_sym.insert(i as u32, k);
    }
    let indirect: Vec<u32> = match c.indirect {
        Some((o, n)) => bytes_at(d, o, n.saturating_mul(4))
            .as_chunks::<4>()
            .0
            .iter()
            .take(MAX_FIXUPS)
            .map(|w| u32::from_le_bytes(*w))
            .collect(),
        None => Vec::new(),
    };
    let entry_of = |idx: u64| {
        indirect
            .get(usize::try_from(idx).ok()?)
            .copied()
            .filter(|e| e & (INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS) == 0)
    };
    // non-lazy pointers (__got) first: code that reads an import directly goes through them
    for want in [true, false] {
        for (s, &(sflags, r1, _)) in img.sections.iter().zip(&extra) {
            let ty = sflags & 0xff;
            let wanted = if want {
                ty == S_NON_LAZY_SYMBOL_POINTERS
            } else {
                ty == S_LAZY_SYMBOL_POINTERS || ty == S_LAZY_DYLIB_SYMBOL_POINTERS
            };
            if !wanted {
                continue;
            }
            for i in 0..(s.size / 8).min(MAX_FIXUPS as u64) {
                let Some(k) = entry_of(u64::from(r1) + i).and_then(|e| of_sym.get(&e)) else { continue };
                img.imports[*k].slot.get_or_insert(s.addr + 8 * i);
            }
        }
    }
    // one function per stub, named after the import it jumps to
    let mut stubs: Vec<Symbol> = Vec::new();
    for (s, &(sflags, r1, r2)) in img.sections.iter().zip(&extra) {
        let size = u64::from(r2);
        if sflags & 0xff != S_SYMBOL_STUBS || size == 0 || size > MAX_STUB {
            continue;
        }
        for i in 0..(s.size / size).min(MAX_FIXUPS as u64) {
            let Some(e) = entry_of(u64::from(r1) + i) else { continue };
            let name = match of_sym.get(&e) {
                Some(&k) => img.imports[k].name.clone(),
                None => match nl.get(e as usize).filter(|n| !n.raw.is_empty()) {
                    Some(n) => shown(&n.raw),
                    None => continue,
                },
            };
            stubs.push(Symbol {
                name: format!("{name}@plt"),
                raw: String::new(),
                addr: s.addr + size * i,
                size,
                func: true,
            });
        }
    }
    // the pointers the loader fills: chained fixups (newer files) or bind opcodes (older ones)
    if let Some((o, n)) = c.chained {
        m.fixups = "chained";
        chained(d, (o, n), &segs, &mut img, &mut by_raw, &lib_of);
    } else if let Some(at) = c.dyld_info {
        m.fixups = "classic";
        classic(d, at, &segs, &mut img, &mut by_raw, &lib_of);
        if c.trie.is_none() {
            let b = &d[at..];
            c.trie = u32_at(b, 40).zip(u32_at(b, 44)).map(|(o, n)| (o.into(), n.into()));
        }
    }
    // exports: the trie when there is one, else the external names of the symbol table
    if let Some((o, n)) = c.trie.filter(|t| t.1 > 0) {
        for (raw, addr) in trie(bytes_at(d, o, n), img.image_base) {
            let name = shown(&raw);
            img.exports.push(Export { raw: raw_of(&raw, &name), name, addr });
        }
    } else {
        for s in nl.iter().filter(|s| defined(s) && s.ntype & N_EXT != 0 && s.ntype & N_PEXT == 0) {
            let name = shown(&s.raw);
            img.exports.push(Export { raw: raw_of(&s.raw, &name), name, addr: s.value });
        }
    }
    if let Some((o, n)) = c.function_starts {
        let b = bytes_at(d, o, n);
        let (mut p, mut a) = (0usize, img.image_base);
        while m.starts.len() < MAX_STARTS {
            match uleb(b, &mut p) {
                Some(0) | None => break,
                Some(delta) => {
                    a = a.wrapping_add(delta);
                    m.starts.push(a);
                }
            }
        }
    }
    img.symbols.extend(stubs);
    // a stripped program still has LC_MAIN, and it names main exactly
    if let Some(a) = m.main.filter(|&a| !img.symbols.iter().any(|s| s.addr == a)) {
        img.symbols.push(Symbol { name: "main".into(), raw: String::new(), addr: a, size: 0, func: true });
    }
    sizes(&mut img, &m.starts);
    let yes_no = |b: bool| if b { "yes" } else { "no" }.to_string();
    img.hardening.items = vec![
        ("pie", if flags & MH_PIE != 0 { "yes (position independent)".into() } else { "no (fixed base)".into() }),
        (
            "nx_stack",
            if flags & MH_ALLOW_STACK_EXECUTION != 0 { "no (stack is executable)".into() } else { "yes".into() },
        ),
        ("code_signature", if c.signed { "yes (present, not checked)".into() } else { "no".into() }),
        ("encrypted", yes_no(c.encrypted.is_some())),
    ];
    if img.arm64 && sub & CPU_SUBTYPE_MASK == CPU_SUBTYPE_ARM64E {
        img.hardening.items.push(("pointer_auth", "yes (arm64e: pointers carry a signature)".into()));
    }
    if let Some((o, n)) = c.encrypted {
        img.warnings.push(format!(
            "the code is encrypted (LC_ENCRYPTION_INFO_64, {n:#x} bytes at file offset {o:#x}): disassembly and \
             decompilation of that range show ciphertext, not instructions"
        ));
    }
    img.macho = Some(Box::new(m));
    Ok(img)
}

/// Symbol sizes: to the next symbol or listed function start in the same section, else the section end.
fn sizes(img: &mut Image, starts: &[u64]) {
    let mut marks: Vec<u64> = img.symbols.iter().map(|s| s.addr).chain(starts.iter().copied()).collect();
    marks.sort_unstable();
    marks.dedup();
    let secs: Vec<(u64, u64)> = img.sections.iter().map(|s| (s.addr, s.addr + s.size)).collect();
    for s in img.symbols.iter_mut().filter(|s| s.size == 0) {
        let Some(&(_, end)) = secs.iter().find(|&&(a, e)| s.addr >= a && s.addr < e) else { continue };
        let next = marks[marks.partition_point(|&a| a <= s.addr)..].first().copied().filter(|&n| n < end);
        s.size = next.unwrap_or(end) - s.addr;
    }
}

/// The import named `raw`, added when the symbol table did not list it.
fn import_index(
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    raw: &[u8],
    lib: Option<String>,
) -> Option<usize> {
    let raw = String::from_utf8_lossy(&raw[..raw.len().min(MAX_NAME_IN)]).into_owned();
    if raw.is_empty() {
        return None;
    }
    if let Some(&k) = by_raw.get(&raw) {
        return Some(k);
    }
    if img.imports.len() >= MAX_IMPORTS {
        return None;
    }
    let name = shown(&raw);
    img.imports.push(Import { lib, raw: raw_of(&raw, &name), name, slot: None });
    by_raw.insert(raw, img.imports.len() - 1);
    Some(img.imports.len() - 1)
}

fn chained(
    d: &[u8],
    (o, n): Range,
    segs: &[Seg],
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    lib_of: &dyn Fn(Option<u32>) -> Option<String>,
) {
    let fixups = match DyldChainedFixups::parse(LE, bytes_at(d, o, n)) {
        Ok(f) => f,
        Err(e) => {
            img.warnings.push(format!("the chained fixups at {o:#x} are not readable ({e}); imports have no slots"));
            return;
        }
    };
    let mut names: Vec<(&[u8], Option<u32>)> = Vec::new();
    if let Ok(mut it) = fixups.imports(LE) {
        while names.len() < MAX_IMPORTS {
            match it.next() {
                Ok(Some(i)) => names.push((i.name, i.dylib.index())),
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the chained fixups import table stops early: {e}"));
                    break;
                }
            }
        }
    }
    let Ok(mut it) = fixups.segments(LE) else {
        img.warnings.push("the chained fixups list no readable segment starts; imports have no slots".into());
        return;
    };
    let mut count = 0usize;
    for _ in 0..segs.len().max(1) {
        let seg = match it.next() {
            Ok(Some(s)) => s,
            Ok(None) => break,
            Err(e) => {
                img.warnings.push(format!("the chained fixups stop early: {e}"));
                break;
            }
        };
        let Some(g) = segs.get(seg.index() as usize) else { continue };
        let mut fx = seg.fixups(LE, img.image_base, bytes_at(d, g.fileoff, g.filesize));
        while count < MAX_FIXUPS {
            count += 1;
            match fx.next() {
                Ok(Some((at, Fixup::Bind(b)))) => {
                    let Some(&(raw, lib)) = names.get(b.ordinal as usize) else { continue };
                    if let Some(k) = import_index(img, by_raw, raw, lib_of(lib)) {
                        img.imports[k].slot.get_or_insert(g.vmaddr.wrapping_add(at));
                    }
                }
                Ok(Some((at, Fixup::Rebase(r)))) => {
                    img.rel_ptrs.push((g.vmaddr.wrapping_add(at), img.image_base.wrapping_add(r.target_offset)));
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the chained fixups of segment {} stop early: {e}", g.name));
                    break;
                }
            }
        }
    }
}

fn classic(
    d: &[u8],
    at: usize,
    segs: &[Seg],
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    lib_of: &dyn Fn(Option<u32>) -> Option<String>,
) {
    let Ok((cmd, _)) = object::pod::from_bytes::<object::macho::DyldInfoCommand<LE>>(&d[at..]) else {
        img.warnings.push(format!("the dyld info command at {at:#x} is not readable; imports may lack slots"));
        return;
    };
    let mut count = 0usize;
    for which in 0..3 {
        let it = match which {
            0 => cmd.binds(LE, d, 8),
            1 => cmd.lazy_binds(LE, d, 8),
            _ => cmd.weak_binds(LE, d, 8),
        };
        let Ok(mut it) = it else { continue };
        while count < MAX_FIXUPS {
            count += 1;
            match it.next() {
                Ok(Some(b)) => {
                    let Some(g) = segs.get(usize::from(b.segment_index)) else { continue };
                    if let Some(k) = import_index(img, by_raw, b.symbol, lib_of(b.dylib.index())) {
                        img.imports[k].slot.get_or_insert(g.vmaddr.wrapping_add(b.segment_offset));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the bind info stops early: {e}"));
                    break;
                }
            }
        }
    }
}

/// The exports trie: (name as written, address). Each node is read once, so a damaged trie whose
/// edges loop or share nodes ends; the work is bounded by the node count.
fn trie(d: &[u8], base: u64) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack: Vec<(usize, Vec<u8>)> = vec![(0, Vec::new())];
    while let Some((node, prefix)) = stack.pop() {
        if out.len() >= MAX_SYMBOLS || seen.len() >= MAX_TRIE_NODES || !seen.insert(node) {
            continue;
        }
        let mut p = node;
        let Some(info) = uleb(d, &mut p).and_then(|n| usize::try_from(n).ok()) else { continue };
        let children = p.saturating_add(info);
        if info > 0 && !prefix.is_empty() {
            if let Some(flags) = uleb(d, &mut p) {
                // re-exports (0x08) point into another library; thread-local ones (kind 1) are no address
                let kind = flags & 0x3;
                if flags & 0x08 == 0 && kind != 1 {
                    if let Some(v) = uleb(d, &mut p) {
                        let addr = if kind == 2 { v } else { base.wrapping_add(v) };
                        out.push((String::from_utf8_lossy(&prefix).into_owned(), addr));
                    }
                }
            }
        }
        let mut p = children;
        let Some(&n) = d.get(p) else { continue };
        p += 1;
        for _ in 0..n {
            let len = d.get(p..).map_or(0, |r| r.iter().position(|&c| c == 0).unwrap_or(r.len()));
            let edge = d.get(p..p + len).unwrap_or(&[]);
            p = p.saturating_add(len + 1);
            let Some(child) = uleb(d, &mut p).and_then(|c| usize::try_from(c).ok()) else { break };
            if prefix.len() + edge.len() <= MAX_NAME_IN {
                let mut name = prefix.clone();
                name.extend_from_slice(edge);
                stack.push((child, name));
            }
        }
    }
    out
}

/// Whether the first bytes of a file are a Mach-O file `Image::open` reads: a thin x86-64 or arm64
/// program or library, or a universal file with such a slice.
pub(crate) fn opens_head(head: &[u8]) -> bool {
    match be32(head, 0) {
        Some(FAT_MAGIC | FAT_MAGIC_64) => {
            let n = be32(head, 4).unwrap_or(0) as u64;
            let entry = if be32(head, 0) == Some(FAT_MAGIC_64) { 32 } else { 20 };
            (1..=MAX_SLICES as u64).contains(&n)
                && !crate::is_managed(&head[..head.len().min(crate::MANAGED_HEAD)])
                && (0..n).any(|i| matches!(be32(head, 8 + i * entry), Some(CPU_X86_64 | CPU_ARM64)))
        }
        _ => {
            u32_at(head, 0) == Some(MH_MAGIC_64)
                && matches!(u32_at(head, 4), Some(CPU_X86_64 | CPU_ARM64))
                && matches!(u32_at(head, 12), Some(2 | 3 | 5 | 6 | 7 | 8 | 9 | 0xb))
        }
    }
}

/// ` --arch arm64` when the person picked a slice, for the next commands; empty otherwise.
pub(crate) fn arch_flag(img: &Image) -> String {
    img.macho.as_ref().and_then(|m| m.arch_arg.as_deref()).map_or(String::new(), |a| format!(" --arch {a}"))
}

/// The lines `info` adds for a Mach-O file.
pub(crate) fn info_lines(img: &Image) -> Vec<String> {
    let Some(m) = img.macho.as_deref() else { return Vec::new() };
    let mut l = Vec::new();
    let mut kind = m.kind.to_string();
    if let Some(p) = &m.platform {
        kind.push_str(&format!(" for {p}"));
    }
    if let Some(id) = &m.id {
        kind.push_str(&format!(", install name {id}"));
    }
    let main = m.main.map_or("no LC_MAIN".into(), |a| format!("LC_MAIN main at {a:#x}"));
    l.push(format!("macho       {kind}; {main}; fixups {}; {} function starts listed", m.fixups, m.starts.len()));
    if !m.libs.is_empty() {
        let shown: Vec<String> = m.libs.iter().take(12).map(|p| short_lib(p)).collect();
        let more = if m.libs.len() > 12 { format!(" ... {} more", m.libs.len() - 12) } else { String::new() };
        l.push(format!("libraries   {}{more}", shown.join(", ")));
    }
    if !m.slices.is_empty() {
        let list: Vec<String> =
            m.slices
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if i == m.picked {
                        format!("{} (read; the sha256 is of this slice)", s.arch)
                    } else {
                        s.arch.clone()
                    }
                })
                .collect();
        let other = m.slices.iter().enumerate().find(|&(i, s)| i != m.picked && arch_key(&s.arch).is_some());
        let hint = other.map_or(String::new(), |(_, s)| {
            format!("; read another: {}", crate::ops::command_arch(img, "info", &s.arch))
        });
        l.push(format!("slices      universal file: {}{hint}", list.join(", ")));
    }
    if let Some(u) = &m.uuid {
        l.push(format!("uuid        {u}"));
    }
    l
}

/// The same facts for `info --json`.
pub(crate) fn info_json(img: &Image) -> Option<Value> {
    let m = img.macho.as_deref()?;
    Some(json!({
        "kind": m.kind,
        "platform": m.platform,
        "install_name": m.id,
        "main": m.main,
        "fixups": m.fixups,
        "function_starts": m.starts.len(),
        "libraries": m.libs,
        "slices": m.slices.iter().map(|s| json!({"arch": s.arch, "offset": s.off, "size": s.size})).collect::<Vec<_>>(),
        "slice_read": (!m.slices.is_empty()).then_some(m.picked),
        "uuid": m.uuid,
    }))
}
