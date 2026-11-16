//! Bare code (`--raw`): the bytes of a program this test builds, cut out of it the way a debugger dumps a region
//! of memory, must read back with the same functions and strings as the program itself. x86-64 and x86-32 come
//! from gcc (each skipped, with a message, when it cannot build), arm64 from clang with lld.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::*;

fn whole(o: &Output) -> String {
    o.page(None, usize::MAX / 2).text()
}

const SAMPLE: &str = r#"
#include <stdio.h>
static const char KEY[] = "raw-key-0451";
static const unsigned TABLE[4] = {0xc0ffee11u, 2, 3, 4};
__attribute__((noinline)) unsigned mix(unsigned x) { return x * 0x5a17u + TABLE[x & 3]; }
__attribute__((noinline)) unsigned hash(const char *s) {
    unsigned h = 0;
    while (*s) h = mix(h ^ (unsigned char)*s++);
    return h;
}
/* never called directly: only its prologue after hash's ret shows where it starts */
__attribute__((noinline)) int twice(int x) { return x * 2; }
int (*volatile op)(int) = twice;
__attribute__((noinline)) int check(const char *s) {
    if (hash(s) == hash(KEY)) { puts("raw: access granted"); return 1; }
    puts("raw: denied");
    return 0;
}
int main(int argc, char **argv) { return check(argc > 1 ? argv[1] : "") + op(argc); }
__attribute__((noinline)) int after_main(int x) { return x + 1; }
"#;

/// Functions that have the next one right after them in the program, so their ends are known exactly.
const COMPARED: [&str; 5] = ["mix", "hash", "twice", "check", "main"];

fn build(cc: &str, dir: &Path, src: &str, name: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join(format!("{name}.c"));
    std::fs::write(&c, src).ok()?;
    let exe = dir.join(name);
    let ok =
        Command::new(cc).args(flags).arg("-o").arg(&exe).arg(&c).output().map(|o| o.status.success()).unwrap_or(false);
    // Where `gcc` is clang for macOS it builds a Mach-O file, which holds no ELF sections to cut: that counts as
    // "cannot build", so the test skips with its message.
    let elf = std::fs::read(&exe).map(|b| b.starts_with(b"\x7fELF")).unwrap_or(false);
    (ok && elf).then_some(exe)
}

/// What a debugger dumps from `lo` to `hi` of the running program: each loaded section's bytes at its address,
/// zeros between them.
fn dump(img: &Image, lo: u64, hi: u64) -> Vec<u8> {
    let mut buf = vec![0u8; (hi - lo) as usize];
    for s in img.sections.iter().filter(|s| s.alloc && s.addr >= lo && s.addr < hi) {
        let b = img.section_bytes(s);
        let at = (s.addr - lo) as usize;
        let n = b.len().min(buf.len() - at);
        buf[at..at + n].copy_from_slice(&b[..n]);
    }
    buf
}

/// The program's code sections through its `.rodata`, read as bare code at the address they ran at.
fn cut(elf: &Image, dir: &Path, arch: &str) -> (Image, u64) {
    let base = elf.code_sections().map(|s| s.addr).min().unwrap();
    let ro = elf.sections.iter().find(|s| s.name == ".rodata").unwrap();
    let path = dir.join(format!("dump-{base:x}.bin"));
    std::fs::write(&path, dump(elf, base, ro.addr + ro.size)).unwrap();
    (Image::open_raw(&path, base, Some(arch)).unwrap(), base)
}

fn func<'a>(img: &'a Image, name: &str) -> &'a crate::funcs::Func {
    img.funcs().iter().find(|f| f.name == name).unwrap_or_else(|| panic!("function {name}"))
}

fn same_functions(elf: &Image, raw: &Image, names: &[&str], tag: &str) {
    for n in names {
        let f = func(elf, n);
        let r = raw.func_at(f.start).unwrap_or_else(|| {
            let near: Vec<String> = raw.funcs().iter().map(|g| format!("{:#x}..{:#x}", g.start, g.end)).collect();
            panic!("{tag}: no function holds {n} at {:#x}; found {}", f.start, near.join(" "))
        });
        assert_eq!((r.start, r.end), (f.start, f.end), "{tag}: {n} ({:#x}..{:#x} in the program)", f.start, f.end);
    }
}

fn same_strings(elf: &Image, raw: &Image, tag: &str) -> usize {
    let ro = elf.sections.iter().find(|s| s.name == ".rodata").unwrap();
    let mut n = 0;
    for s in elf.strings().iter().filter(|s| ro.contains(s.addr)) {
        let (r, off) =
            raw.string_at(s.addr).unwrap_or_else(|| panic!("{tag}: no string at {:#x} {:?}", s.addr, s.text));
        assert_eq!((r.addr, r.text.as_str(), off), (s.addr, s.text.as_str(), 0), "{tag}");
        n += 1;
    }
    n
}

#[test]
fn a_dump_of_a_gcc_x86_64_program_reads_back_with_its_functions_strings_and_references() {
    let dir = tempfile::tempdir().unwrap();
    let variants: [(&str, &[&str]); 2] = [("abs", &["-O0", "-fno-pie", "-no-pie"]), ("pie", &["-O0", "-fPIE", "-pie"])];
    for (tag, flags) in variants {
        let Some(exe) = build("gcc", dir.path(), SAMPLE, tag, flags) else {
            eprintln!("SKIPPED gcc {tag}: gcc did not build the sample, nothing was checked");
            continue;
        };
        let elf = Image::open(&exe).unwrap();
        let (raw, base) = cut(&elf, dir.path(), "x86-64");
        assert_eq!((raw.format, raw.bits, raw.arm64, raw.entry), (Format::Raw, 64, false, base), "{tag}");
        same_functions(&elf, &raw, &COMPARED, tag);
        assert!(same_strings(&elf, &raw, tag) >= 3, "{tag}: the three strings of the sample");

        let check = func(&elf, "check").start;
        let x = whole(&xref_str(&raw, "raw: access granted", false).unwrap());
        assert!(x.contains(&format!("sub_{check:x}")), "{tag}: the string's reader\n{x}");
        let data = tempfile::tempdir().unwrap();
        let d = whole(&decomp_window(&raw, data.path(), &[format!("{check:#x}")], false, None, 200).unwrap());
        assert!(d.contains("raw: access granted") && d.contains("raw-key-0451"), "{tag}\n{d}");
        let g = whole(&cfg_op(&raw, &format!("{check:#x}")).unwrap());
        assert!(g.contains("block"), "{tag}\n{g}");
        let f = whole(&find_imm(&raw, 0x5a17).unwrap());
        assert!(f.contains(&format!("sub_{:x}", func(&elf, "mix").start)), "{tag}: the multiplier in mix\n{f}");
        let f = whole(&find_imm(&raw, 0xc0ff_ee11).unwrap());
        assert!(f.contains("data u32"), "{tag}: TABLE outside every function\n{f}");

        // the next commands open the bytes the same way again
        let again = format!("--raw --base {base:#x} --arch x86-64");
        let o = whole(&overview(&raw).unwrap());
        assert!(o.contains(&again), "{tag}\n{o}");
        let i = whole(&info(&raw));
        assert!(i.contains("raw code little-endian, x86-64") && i.contains("System V"), "{tag}\n{i}");
        // the string data sits after the code ends, outside every function
        let key = elf.strings().iter().find(|s| s.text == "raw-key-0451").unwrap().addr;
        let e = func_view(&raw, &FnQuery::new(&format!("{key:#x}"))).unwrap_err().to_string();
        assert!(e.contains("after the end of") && e.contains(&again), "{tag}: {e}");
    }
}

#[test]
fn a_dump_of_a_gcc_x86_32_program_reads_back_with_its_functions_and_strings() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build("gcc", dir.path(), SAMPLE, "m32", &["-m32", "-O0", "-fno-pie", "-no-pie"]) else {
        eprintln!("SKIPPED gcc -m32: no 32-bit C library here (gcc-multilib), nothing was checked");
        return;
    };
    let elf = Image::open(&exe).unwrap();
    let (raw, base) = cut(&elf, dir.path(), "x86-32");
    assert_eq!((raw.bits, raw.arch_name()), (32, "x86"));
    same_functions(&elf, &raw, &COMPARED, "m32");
    assert!(same_strings(&elf, &raw, "m32") >= 3);
    let check = func(&elf, "check").start;
    let x = whole(&xref_str(&raw, "raw: access granted", false).unwrap());
    assert!(x.contains(&format!("sub_{check:x}")), "{x}");
    // decomp reads x86-64 only: the answer names what does read x86-32
    let data = tempfile::tempdir().unwrap();
    let e = decomp_window(&raw, data.path(), &[format!("{check:#x}")], false, None, 200).unwrap_err().to_string();
    assert!(e.contains("x86-32 is not supported yet") && e.contains("strings"), "{e}");
    assert!(whole(&overview(&raw).unwrap()).contains(&format!("--raw --base {base:#x} --arch x86-32")));
}

/// No C library: the program only calls itself, so clang and lld build it for arm64 on any machine.
const SAMPLE_A64: &str = r#"
static const char KEY[] = "raw-key-0451";
static const char OK[] = "raw: access granted";
static volatile const char *sink;
__attribute__((noinline)) static unsigned mix(unsigned x) { return x * 0x5a17u + 7; }
__attribute__((noinline)) static unsigned hash(const char *s) {
    unsigned h = 0;
    while (*s) h = mix(h ^ (unsigned char)*s++);
    return h;
}
__attribute__((noinline)) static int check(const char *s) {
    if (hash(s) == hash(KEY)) { sink = OK; return 1; }
    return 0;
}
void _start(void) { check(KEY); for (;;) {} }
"#;

#[test]
fn a_dump_of_an_arm64_program_reads_back_with_its_functions_and_strings() {
    let dir = tempfile::tempdir().unwrap();
    let flags = [
        "--target=aarch64-linux-gnu",
        "-O0",
        "-ffreestanding",
        "-nostdlib",
        "-static",
        "-fuse-ld=lld",
        // the code first, as in a dump that starts at the code
        "-Wl,--section-start=.text=0x210000",
        "-Wl,--section-start=.rodata=0x211000",
    ];
    let Some(exe) = build("clang", dir.path(), SAMPLE_A64, "a64", &flags) else {
        eprintln!("SKIPPED arm64: clang with lld did not build the sample, nothing was checked");
        return;
    };
    let elf = Image::open(&exe).unwrap();
    let (raw, base) = cut(&elf, dir.path(), "arm64");
    assert_eq!((raw.arm64, raw.arch_name(), base), (true, "arm64", 0x210000));
    same_functions(&elf, &raw, &["_start", "check", "hash", "mix"], "arm64");
    assert!(same_strings(&elf, &raw, "arm64") >= 2);
    let check = func(&elf, "check").start;
    let x = whole(&xref_str(&raw, "raw: access granted", false).unwrap());
    assert!(x.contains(&format!("sub_{check:x}")), "{x}");
    // cfg reads x86-64 only: the answer names what does read arm64
    let e = cfg_op(&raw, &format!("{check:#x}")).unwrap_err().to_string();
    assert!(e.contains("does not read AArch64 code yet") && e.contains("strings"), "{e}");
}

#[test]
fn raw_bytes_name_the_arch_words_and_refuse_a_base_past_the_address_space() {
    let code = vec![0x90u8, 0xc3];
    let e = Image::from_raw(code.clone(), "x".into(), 0, Some("mips")).err().expect("an error").to_string();
    assert!(e.contains("x86-64, x86-32 or arm64"), "{e}");
    let e = Image::from_raw(code.clone(), "x".into(), 0xffff_ffff, Some("x86-32")).err().expect("an error").to_string();
    assert!(e.contains("32-bit address space"), "{e}");
    let e = Image::from_raw(Vec::new(), "x".into(), 0, None).err().expect("an error").to_string();
    assert!(e.contains("empty"), "{e}");
    let img = Image::from_raw(code, "x".into(), 0x1000, None).unwrap();
    assert_eq!((img.arch_name(), img.entry, img.funcs().len()), ("x86-64", 0x1000, 1));
    // a file that is not a known format says how to read it as bare code
    let e = Image::from_bytes(vec![0x55, 0x48, 0x89, 0xe5, 0xc3, 0xff, 0x00, 0x90], "x".into())
        .err()
        .expect("an error")
        .to_string();
    assert!(e.contains("--raw --base ADDR"), "{e}");
}

/// A caller at the first byte and its callee 0x10 bytes on, as bare x86-64: `call +0x10; ret`, int3 padding,
/// `mov eax, 2; ret`. Read at base 0 the callee is at 0x10; read at base 0x10 the caller is.
fn caller_and_callee() -> Vec<u8> {
    let mut b = vec![0xe8, 0x0b, 0, 0, 0, 0xc3];
    b.resize(0x10, 0xcc);
    b.extend_from_slice(&[0xb8, 0x02, 0, 0, 0, 0xc3]);
    b
}

#[test]
fn the_same_bytes_read_at_another_base_or_arch_keep_no_names_or_decompiled_code_of_the_first_read() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let path = dir.path().join("dump.bin");
    std::fs::write(&path, caller_and_callee()).unwrap();
    let decomp = |img: &Image| whole(&decomp_window(img, data.path(), &["0x10".to_string()], false, None, 50).unwrap());

    // first read at the default base 0: 0x10 is the callee, named and decompiled (which fills the cache)
    let mut at0 = Image::open_raw(&path, 0, None).unwrap();
    assert_eq!(attach_names(&mut at0, data.path()), None);
    name_set(&at0, data.path(), "0x10", "two").unwrap();
    assert_eq!(attach_names(&mut at0, data.path()), None);
    assert_eq!(at0.user_name(0x10), Some("two"));
    let d0 = decomp(&at0);
    assert!(d0.contains("two") && !d0.contains("sub_20"), "{d0}");

    // the same file at base 0x10: 0x10 is now the caller, and nothing saved for the other read applies to it
    let mut at10 = Image::open_raw(&path, 0x10, None).unwrap();
    assert_eq!(at0.sha256, at10.sha256);
    assert_eq!(attach_names(&mut at10, data.path()), None);
    assert_eq!(at10.user_name(0x10), None);
    let d10 = decomp(&at10);
    assert!(d10.contains("sub_20") && !d10.contains("two"), "{d10}");

    // the first read still finds what it saved
    let mut again = Image::open_raw(&path, 0, None).unwrap();
    assert_eq!(attach_names(&mut again, data.path()), None);
    assert_eq!(again.user_name(0x10), Some("two"));
    assert_eq!(decomp(&again), d0);

    // another arch, and a file format read as bare code, are other keys too
    let as32 = Image::open_raw(&path, 0, Some("x86-32")).unwrap();
    assert_ne!(as32.store_key(), at0.store_key());
    let elf = crate::fixtures::elf64(false);
    let fmt = Image::from_bytes(elf.bytes.clone(), "f".into()).unwrap();
    let bare = Image::from_raw(elf.bytes, "f".into(), fmt.image_base, None).unwrap();
    assert_eq!((fmt.store_key(), bare.sha256.as_str()), (fmt.sha256.clone(), fmt.sha256.as_str()));
    assert_ne!(bare.store_key(), fmt.store_key());
}
