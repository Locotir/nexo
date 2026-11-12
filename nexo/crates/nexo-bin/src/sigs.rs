//! Library signatures: recognize statically linked library functions (memcpy, strlen, malloc, the
//! printf family, Rust `core::fmt::write`...) so the model does not spend steps on them.
//!
//! A signature is a hash of the function's instructions with everything the linker changes taken
//! out: branch targets outside the function, RIP-relative and absolute addresses, TLS offsets,
//! addresses used as constants, padding and `endbr64`. It also keeps the instruction count and the
//! number of calls leaving the function. Two kinds of matches exist, and neither is proof:
//! - exact: the whole masked function hashes the same (the function is named, labelled `[signature]`);
//! - partial or ambiguous: only the first instructions match, or several different library names
//!   share the hash (the function keeps `sub_<addr>`, the label says what it could be).
//!
//! Functions under [`MIN_INSNS`] instructions are never matched: too little code to mean anything.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind, Register};
use object::{Object, ObjectSection, ObjectSymbol, SectionKind, SymbolKind};

use crate::{Error, Result};

/// Fewest instructions a function needs to be matched.
pub const MIN_INSNS: u32 = 8;
/// Instructions in the prefix hash that makes a partial match.
const PREFIX_INSNS: u32 = 12;
/// A partial match needs at least this many instructions.
const PARTIAL_MIN: u32 = 24;
/// Functions bigger than this many bytes are not fingerprinted.
const MAX_BYTES: u64 = 1 << 16;
/// Names listed in an ambiguous label.
const MAX_LISTED: usize = 4;
/// Largest signature file read (bytes).
const MAX_SIG_FILE: u64 = 16 << 20;

const BUILTIN: &str = include_str!("sigdata/builtin.sig");

// ---------------------------------------------------------------------------------------------
// Fingerprint

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fp {
    pub full: u64,
    /// Hash of the first [`PREFIX_INSNS`] instructions; 0 when the function is shorter.
    pub prefix: u64,
    pub insns: u32,
    /// Calls to somewhere outside the function.
    pub calls: u32,
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn push(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn is_padding(m: Mnemonic) -> bool {
    matches!(m, Mnemonic::Nop | Mnemonic::Int3 | Mnemonic::Endbr64 | Mnemonic::Endbr32)
}

/// Fingerprint the code in `bytes` (which starts at `start`). `relocs` are the offsets of relocated
/// fields when the bytes come from an object file; `in_image` says whether a value points into the
/// loaded file when they come from a linked one. `None` when the bytes do not decode cleanly.
pub(crate) fn fingerprint(
    bytes: &[u8],
    start: u64,
    relocs: Option<&[u64]>,
    in_image: &dyn Fn(u64) -> bool,
) -> Option<Fp> {
    let end = start + bytes.len() as u64;
    let mut dec = Decoder::with_ip(64, bytes, start, DecoderOptions::NONE);
    let (mut h, mut ph) = (Fnv::new(), Fnv::new());
    let (mut n, mut calls, mut prefix) = (0u32, 0u32, 0u64);
    let mut ins = Instruction::default();
    while dec.can_decode() {
        dec.decode_out(&mut ins);
        if ins.is_invalid() {
            return None;
        }
        let m = ins.mnemonic();
        if is_padding(m) {
            continue;
        }
        let off = ins.ip() - start;
        let reloc = relocs.is_some_and(|r| r.iter().any(|&o| o >= off && o < off + ins.len() as u64));
        let mut t = Fnv::new();
        token(&ins, start, end, reloc, in_image, &mut t, &mut calls);
        h.push(t.0);
        if n < PREFIX_INSNS {
            ph.push(t.0);
        }
        n += 1;
        if n == PREFIX_INSNS {
            prefix = ph.0;
        }
    }
    // trailing padding bytes of other kinds (a lone `ret` after zeros) are the caller's concern
    (n > 0).then_some(Fp { full: h.0, prefix, insns: n, calls })
}

fn token(
    ins: &Instruction,
    start: u64,
    end: u64,
    reloc: bool,
    in_image: &dyn Fn(u64) -> bool,
    t: &mut Fnv,
    calls: &mut u32,
) {
    let m = ins.mnemonic();
    let fc = ins.flow_control();
    let branch = matches!(fc, FlowControl::Call | FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch);
    if branch && ins.op0_kind() == OpKind::NearBranch64 {
        let to = ins.near_branch_target();
        if !reloc && to >= start && to < end {
            t.push(1);
            t.push(m as u64);
            t.push(to - start);
        } else {
            external(fc, t, calls);
        }
        return;
    }
    if matches!(fc, FlowControl::Call | FlowControl::UnconditionalBranch) && ins.op0_kind() == OpKind::Memory {
        // a call or jump through a slot (a GOT entry the linker may turn into a direct branch)
        if ins.is_ip_rel_memory_operand()
            || reloc
            || (ins.memory_base() == Register::None && ins.memory_index() == Register::None)
        {
            external(fc, t, calls);
            return;
        }
    }
    // a load of an address: `mov r, [rip+got]` the linker relaxes into `lea r, [rip+sym]`
    let ripish = ins.is_ip_rel_memory_operand() || (reloc && ins.memory_base() == Register::None);
    if matches!(m, Mnemonic::Mov | Mnemonic::Lea)
        && ripish
        && ins.op0_kind() == OpKind::Register
        && ins.op1_kind() == OpKind::Memory
    {
        t.push(2);
        t.push(ins.op0_register() as u64);
        return;
    }
    t.push(3);
    t.push(m as u64);
    for i in 0..ins.op_count() {
        let k = ins.op_kind(i);
        t.push(k as u64 + 100);
        match k {
            OpKind::Register => t.push(ins.op_register(i) as u64),
            OpKind::Memory => {
                let (base, index) = (ins.memory_base(), ins.memory_index());
                let seg = ins.segment_prefix();
                let tls = matches!(seg, Register::FS | Register::GS);
                let absolute = base == Register::None && index == Register::None;
                let rip = ins.is_ip_rel_memory_operand();
                let masked = rip
                    || reloc
                    || tls
                    || absolute
                    || (base == Register::None && in_image(ins.memory_displacement64()));
                t.push(if rip { 0 } else { base as u64 });
                t.push(index as u64);
                t.push(u64::from(ins.memory_index_scale()));
                t.push(seg as u64);
                t.push(ins.memory_size() as u64);
                if masked {
                    t.push(0x6d61_736b);
                } else {
                    t.push(ins.memory_displacement64());
                }
            }
            OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => {
                let v = ins.immediate(i);
                let wide = matches!(k, OpKind::Immediate32 | OpKind::Immediate64 | OpKind::Immediate32to64);
                if wide && (reloc || in_image(v)) {
                    t.push(0x6164_6472);
                } else {
                    t.push(v);
                }
            }
            _ => {}
        }
    }
}

fn external(fc: FlowControl, t: &mut Fnv, calls: &mut u32) {
    t.push(4);
    t.push(if fc == FlowControl::Call { 1 } else { 2 });
    if fc == FlowControl::Call {
        *calls += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// The set

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub insns: u32,
    pub calls: u32,
    pub full: u64,
    pub prefix: u64,
}

/// A group of signatures.
#[derive(Debug, Clone, Default)]
pub struct SigSet {
    entries: Vec<Entry>,
    by_full: HashMap<u64, Vec<u32>>,
    by_prefix: HashMap<u64, Vec<u32>>,
}

impl SigSet {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn from_entries(mut entries: Vec<Entry>) -> SigSet {
        entries.sort_by(|a, b| (&a.name, a.full).cmp(&(&b.name, b.full)));
        entries.dedup();
        let mut s = SigSet { entries, ..SigSet::default() };
        for (i, e) in s.entries.iter().enumerate() {
            s.by_full.entry(e.full).or_default().push(i as u32);
            if e.prefix != 0 && e.insns >= PARTIAL_MIN {
                s.by_prefix.entry(e.prefix).or_default().push(i as u32);
            }
        }
        s
    }

    /// Read the text form (`name insns calls full prefix`, tab separated, `#` comments).
    pub fn parse(text: &str) -> SigSet {
        let mut v = Vec::new();
        for l in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() != 5 {
                continue;
            }
            let (Ok(insns), Ok(calls), Ok(full), Ok(prefix)) = (
                f[1].parse::<u32>(),
                f[2].parse::<u32>(),
                u64::from_str_radix(f[3], 16),
                u64::from_str_radix(f[4], 16),
            ) else {
                continue;
            };
            v.push(Entry { name: f[0].to_string(), insns, calls, full, prefix });
        }
        SigSet::from_entries(v)
    }

    pub fn to_text(&self, source: &str) -> String {
        let mut s = format!("# nexo-sigs 1 | {source}\n# name, instructions, calls out, hash, prefix hash\n");
        for e in &self.entries {
            s.push_str(&format!("{}\t{}\t{}\t{:016x}\t{:016x}\n", e.name, e.insns, e.calls, e.full, e.prefix));
        }
        s
    }

    /// The signatures shipped in the crate.
    pub fn builtin() -> &'static SigSet {
        static B: OnceLock<SigSet> = OnceLock::new();
        B.get_or_init(|| SigSet::parse(BUILTIN))
    }

    pub fn merged(mut self, other: SigSet) -> SigSet {
        self.entries.extend(other.entries);
        SigSet::from_entries(self.entries)
    }

    /// Every `*.sig` file in `<data dir>/sigs`, merged. Unreadable files are skipped.
    pub fn load_dir(data_dir: &Path) -> SigSet {
        let mut all = SigSet::default();
        let Ok(rd) = std::fs::read_dir(data_dir.join("sigs")) else { return all };
        let mut paths: Vec<_> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sig")).collect();
        paths.sort();
        for p in paths {
            if std::fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_SIG_FILE) {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    all = all.merged(SigSet::parse(&t));
                }
            }
        }
        all
    }
}

// ---------------------------------------------------------------------------------------------
// Matching

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    /// The whole masked function hashes the same as one library name.
    Exact,
    /// The first instructions match one library name, the rest differs (another library version?).
    Partial,
    /// Several library names fit equally.
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigHit {
    pub strength: Strength,
    /// The library name (the first of them when ambiguous).
    pub name: String,
    /// Other names that fit equally (ambiguous only).
    pub others: Vec<String>,
    pub insns: u32,
}

impl SigHit {
    /// The tag shown next to the function: what kind of match, never "proven".
    pub fn label(&self) -> String {
        match self.strength {
            Strength::Exact => format!("[signature exact: {} instructions match the masked library code, not proof]", self.insns),
            Strength::Partial => format!(
                "[signature partial: first {PREFIX_INSNS} of {} instructions match {}, the rest differs; kept as unnamed]",
                self.insns, self.name
            ),
            Strength::Ambiguous => {
                let mut all = vec![self.name.clone()];
                all.extend(self.others.iter().take(MAX_LISTED - 1).cloned());
                let more = self.others.len() + 1 - all.len();
                format!(
                    "[signature ambiguous: {} instructions fit {}{}; kept as unnamed]",
                    self.insns,
                    all.join(" or "),
                    if more > 0 { format!(" (+{more} more)") } else { String::new() }
                )
            }
        }
    }
}

fn distinct(set: &SigSet, ids: &[u32]) -> Vec<String> {
    let mut names: Vec<String> = ids.iter().map(|&i| set.entries[i as usize].name.clone()).collect();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn lookup(sets: &[&SigSet], fp: &Fp) -> Option<SigHit> {
    if fp.insns < MIN_INSNS {
        return None;
    }
    let mut full: Vec<String> = Vec::new();
    let mut part: Vec<String> = Vec::new();
    for s in sets {
        if let Some(ids) = s.by_full.get(&fp.full) {
            let ids: Vec<u32> = ids.iter().copied().filter(|&i| s.entries[i as usize].insns == fp.insns).collect();
            full.extend(distinct(s, &ids));
        }
        if fp.prefix != 0 && fp.insns >= PARTIAL_MIN {
            if let Some(ids) = s.by_prefix.get(&fp.prefix) {
                let ids: Vec<u32> = ids
                    .iter()
                    .copied()
                    .filter(|&i| {
                        let e = &s.entries[i as usize];
                        e.insns * 2 >= fp.insns && fp.insns * 2 >= e.insns && e.calls.abs_diff(fp.calls) <= 2
                    })
                    .collect();
                part.extend(distinct(s, &ids));
            }
        }
    }
    full.sort();
    full.dedup();
    part.sort();
    part.dedup();
    let hit = |strength, mut names: Vec<String>| {
        let name = names.remove(0);
        Some(SigHit { strength, name, others: names, insns: fp.insns })
    };
    match (full.len(), part.len()) {
        (1, _) => hit(Strength::Exact, full),
        (n, _) if n > 1 => hit(Strength::Ambiguous, full),
        (_, 1) => hit(Strength::Partial, part),
        (_, n) if n > 1 => hit(Strength::Ambiguous, part),
        _ => None,
    }
}

/// Fingerprint and look up one function of an image.
pub(crate) fn identify(img: &crate::Image, sets: &[&SigSet], start: u64, end: u64) -> Option<SigHit> {
    if end <= start || end - start > MAX_BYTES {
        return None;
    }
    let bytes = img.read_at(start, (end - start) as usize)?;
    let in_image = |v: u64| img.section_at(v).is_some();
    let fp = fingerprint(bytes, start, None, &in_image)?;
    lookup(sets, &fp)
}

// ---------------------------------------------------------------------------------------------
// Making a set from libraries

/// Which library functions a generated set keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// The common ones: string and memory routines, malloc, stdio, Rust alloc and fmt.
    Common,
    All,
}

const C_EXACT: &[&str] = &[
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "memchr",
    "memrchr",
    "mempcpy",
    "bcmp",
    "bzero",
    "strlen",
    "strnlen",
    "strcmp",
    "strncmp",
    "strcpy",
    "strncpy",
    "stpcpy",
    "stpncpy",
    "strcat",
    "strncat",
    "strchr",
    "strrchr",
    "strchrnul",
    "strstr",
    "strspn",
    "strcspn",
    "strpbrk",
    "strdup",
    "strndup",
    "strtok",
    "strtok_r",
    "strcasecmp",
    "strncasecmp",
    "strcoll",
    "strxfrm",
    "malloc",
    "free",
    "calloc",
    "realloc",
    "reallocarray",
    "posix_memalign",
    "aligned_alloc",
    "memalign",
    "valloc",
    "printf",
    "fprintf",
    "sprintf",
    "snprintf",
    "dprintf",
    "asprintf",
    "vprintf",
    "vfprintf",
    "vsprintf",
    "vsnprintf",
    "vdprintf",
    "vasprintf",
    "puts",
    "fputs",
    "putchar",
    "fputc",
    "putc",
    "fwrite",
    "fread",
    "fopen",
    "fclose",
    "fflush",
    "fgets",
    "fgetc",
    "getc",
    "getchar",
    "getline",
    "scanf",
    "sscanf",
    "fscanf",
    "atoi",
    "atol",
    "atoll",
    "atof",
    "strtol",
    "strtoul",
    "strtoll",
    "strtoull",
    "strtod",
    "strtof",
    "qsort",
    "bsearch",
    "getenv",
    "setenv",
    "exit",
    "abort",
    "atexit",
    "perror",
    "strerror",
    "toupper",
    "tolower",
    "isalpha",
    "isdigit",
    "rand",
    "srand",
    "random",
    "srandom",
    "time",
    "clock",
    "gettimeofday",
    "nanosleep",
    "sleep",
    "usleep",
];

const C_PREFIX: &[&str] = &[
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "memchr",
    "memrchr",
    "mempcpy",
    "strlen",
    "strnlen",
    "strcmp",
    "strncmp",
    "strcpy",
    "strncpy",
    "stpcpy",
    "strcat",
    "strncat",
    "strchr",
    "strrchr",
    "strcasecmp",
    "strstr",
    "wcslen",
    "wcscmp",
    "wcscpy",
    "wmemcpy",
    "wmemset",
    "wmemcmp",
    "int_malloc",
    "int_free",
    "libc_malloc",
    "libc_free",
    "libc_calloc",
    "libc_realloc",
    "vfprintf",
    "vfwprintf",
    "printf_",
    "IO_",
    "malloc_",
    "libc_start_main",
    "libc_setup_tls",
    "libc_init_first",
    "libc_csu_init",
];

const RUST_PREFIX: &[&str] = &[
    "core::fmt::",
    "alloc::alloc::",
    "alloc::fmt::",
    "alloc::raw_vec::",
    "alloc::string::",
    "alloc::slice::",
    "core::panicking::",
    "core::str::",
    "core::slice::memchr",
    "core::num::",
    "core::unicode::",
    "std::io::stdio::",
    "std::io::Write::",
    "std::rt::",
    "std::panicking::",
    "std::process::",
    "std::sys::pal::unix::",
    "std::sys::alloc::",
    "std::alloc::",
    "std::thread::",
    "std::env::",
];

fn keep_name(name: &str) -> bool {
    let bare = name.trim_start_matches('_');
    C_EXACT.contains(&bare)
        || C_PREFIX.iter().any(|p| bare.starts_with(p))
        || RUST_PREFIX.iter().any(|p| name.starts_with(p))
}

/// The name to show for several symbols at one address: the shortest without leading underscores
/// first, unless a kept (common) name is among them.
fn canonical(names: &[String], keep: Keep) -> Option<String> {
    let mut v: Vec<&String> = names.iter().collect();
    v.sort_by_key(|n| (n.trim_start_matches('_').len(), n.len(), n.to_string()));
    match keep {
        Keep::All => v.first().map(|s| s.to_string()),
        Keep::Common => v.into_iter().find(|n| keep_name(n)).cloned(),
    }
}

/// Add the signatures of the functions of one relocatable object to `out`.
fn add_object(data: &[u8], keep: Keep, out: &mut Vec<Entry>) {
    let Ok(file) = object::File::parse(data) else { return };
    if file.format() != object::BinaryFormat::Elf || file.architecture() != object::Architecture::X86_64 {
        return;
    }
    // function symbols grouped by (section, offset)
    let mut groups: BTreeMap<(usize, u64), (u64, Vec<String>)> = BTreeMap::new();
    for s in file.symbols() {
        // STT_GNU_IFUNC (10) is a function too: the resolver the loader runs (`memcpy`, `strlen` in a static libc)
        let ifunc = matches!(s.flags(), object::SymbolFlags::Elf { st_info, .. } if st_info.0 & 0xf == 10);
        if !s.is_definition() || !(matches!(s.kind(), SymbolKind::Text) || ifunc) || s.size() < 16 {
            continue;
        }
        let Some(si) = s.section_index() else { continue };
        let Ok(raw) = s.name() else { continue };
        let name = crate::image::demangle(raw);
        let g = groups.entry((si.0, s.address())).or_insert_with(|| (s.size(), Vec::new()));
        g.0 = g.0.max(s.size());
        g.1.push(name);
    }
    for ((si, off), (size, names)) in groups {
        let Some(name) = canonical(&names, keep) else { continue };
        let Ok(sec) = file.section_by_index(object::SectionIndex(si)) else { continue };
        if sec.kind() != SectionKind::Text || size > MAX_BYTES {
            continue;
        }
        let Ok(d) = sec.data() else { continue };
        let Some(bytes) = d.get(off as usize..(off + size) as usize) else { continue };
        let relocs: Vec<u64> =
            sec.relocations().filter(|(o, _)| *o >= off && *o < off + size).map(|(o, _)| o - off).collect();
        let none = |_: u64| false;
        if let Some(fp) = fingerprint(bytes, 0, Some(&relocs), &none) {
            if fp.insns >= MIN_INSNS {
                out.push(Entry { name, insns: fp.insns, calls: fp.calls, full: fp.full, prefix: fp.prefix });
            }
        }
    }
}

/// Read a static library (`.a`, `.rlib`) or an object file and make signatures of its functions.
pub fn generate(path: &Path, keep: Keep) -> Result<SigSet> {
    let data = std::fs::read(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
    let mut out = Vec::new();
    match object::read::archive::ArchiveFile::parse(&*data) {
        Ok(ar) => {
            for m in ar.members().flatten() {
                if let Ok(d) = m.data(&*data) {
                    add_object(d, keep, &mut out);
                }
            }
        }
        Err(_) => add_object(&data, keep, &mut out),
    }
    if out.is_empty() {
        return Err(Error::NotRead(format!(
            "{} holds no x86-64 ELF object with functions to sign (give a .a, .rlib or .o)",
            path.display()
        )));
    }
    Ok(SigSet::from_entries(out))
}

/// Libraries on this machine worth signing: static C libraries and the installed Rust std.
pub fn find_libraries() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for p in [
        "/usr/lib/libc.a",
        "/usr/lib64/libc.a",
        "/usr/lib/x86_64-linux-gnu/libc.a",
        "/usr/lib/musl/lib/libc.a",
        "/usr/lib/x86_64-linux-musl/libc.a",
        "/usr/local/musl/lib/libc.a",
    ] {
        // /usr/lib64 is often a link to /usr/lib: list a file once
        if let Ok(real) = std::fs::canonicalize(p) {
            if !v.contains(&real) {
                v.push(real);
            }
        }
    }
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(root) = sysroot {
        let lib = Path::new(&root).join("lib/rustlib/x86_64-unknown-linux-gnu/lib");
        if let Ok(rd) = std::fs::read_dir(lib) {
            let mut rl: Vec<_> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().is_some_and(|x| x == "rlib")
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| ["libstd-", "libcore-", "liballoc-"].iter().any(|k| n.starts_with(k)))
                })
                .collect();
            rl.sort();
            v.extend(rl);
        }
    }
    v
}
