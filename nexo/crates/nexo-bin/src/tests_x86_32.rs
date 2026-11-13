//! x86-32 files: an ELF32 the test builds byte by byte (always), small C programs built with `gcc -m32`
//! (skipped, with a message, when the machine has no 32-bit C library) and a PE32 built with the i686
//! mingw compiler (skipped when it is not installed).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::*;

fn whole(o: &Output) -> String {
    o.page(None, usize::MAX / 2).text()
}

fn func_names(img: &Image) -> Vec<String> {
    img.funcs().iter().map(|f| f.name.clone()).collect()
}

fn fn_text(img: &Image, name: &str) -> String {
    whole(&func_view(img, &FnQuery::new(name)).unwrap_or_else(|e| panic!("fn {name}: {e}")))
}

/// Code of the hand-built file: `_start` calls `main`, which pushes the address of a string and calls `helper`.
const BASE: u32 = 0x0804_8000;
const TEXT_OFF: u32 = 0x60;
const RODATA_OFF: u32 = 0x80;
const TEXT: &str = "hand built x86-32";

/// A minimal ELF32 i386 executable: one load segment, .text, .rodata, .symtab, .strtab, .shstrtab.
fn elf32_by_hand() -> Vec<u8> {
    let text_at = BASE + TEXT_OFF;
    let str_at = BASE + RODATA_OFF;
    let (start, main, helper) = (text_at, text_at + 6, text_at + 24);
    let mut code = Vec::new();
    code.push(0xe8); // call main
    code.extend_from_slice(&(main.wrapping_sub(start + 5)).to_le_bytes());
    code.push(0xf4); // hlt
    code.extend_from_slice(&[0x55, 0x89, 0xe5]); // push ebp; mov ebp, esp
    code.push(0x68); // push str
    code.extend_from_slice(&str_at.to_le_bytes());
    code.push(0xe8); // call helper
    code.extend_from_slice(&(helper.wrapping_sub(main + 13)).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, 0x04, 0x5d, 0xc3]); // add esp, 4; pop ebp; ret
    code.extend_from_slice(&[0x8b, 0x44, 0x24, 0x04, 0xc3]); // mov eax, [esp+4]; ret
    assert_eq!(code.len(), 29);

    let mut f = vec![0u8; TEXT_OFF as usize];
    f.extend_from_slice(&code);
    f.resize(RODATA_OFF as usize, 0);
    f.extend_from_slice(TEXT.as_bytes());
    f.push(0);
    let load_end = f.len() as u32;

    let strtab = b"\0_start\0main\0helper\0";
    let sym = |name: u32, value: u32, size: u32| {
        let mut s = Vec::new();
        s.extend_from_slice(&name.to_le_bytes());
        s.extend_from_slice(&value.to_le_bytes());
        s.extend_from_slice(&size.to_le_bytes());
        s.extend_from_slice(&[0x12, 0]); // global function
        s.extend_from_slice(&1u16.to_le_bytes()); // in .text
        s
    };
    f.resize(f.len().next_multiple_of(4), 0);
    let symtab_off = f.len() as u32;
    f.extend_from_slice(&[0u8; 16]);
    f.extend_from_slice(&sym(1, start, 6));
    f.extend_from_slice(&sym(8, main, 18));
    f.extend_from_slice(&sym(13, helper, 5));
    let strtab_off = f.len() as u32;
    f.extend_from_slice(strtab);
    let shstr = b"\0.text\0.rodata\0.symtab\0.strtab\0.shstrtab\0";
    let shstr_off = f.len() as u32;
    f.extend_from_slice(shstr);
    f.resize(f.len().next_multiple_of(4), 0);
    let shoff = f.len() as u32;
    // name, type, flags, addr, offset, size, link, info, align, entsize
    let sections: [[u32; 10]; 6] = [
        [0; 10],
        [1, 1, 6, text_at, TEXT_OFF, code.len() as u32, 0, 0, 16, 0],
        [7, 1, 2, str_at, RODATA_OFF, TEXT.len() as u32 + 1, 0, 0, 1, 0],
        [15, 2, 0, 0, symtab_off, 64, 4, 1, 4, 16],
        [23, 3, 0, 0, strtab_off, strtab.len() as u32, 0, 0, 1, 0],
        [31, 3, 0, 0, shstr_off, shstr.len() as u32, 0, 0, 1, 0],
    ];
    for s in sections {
        for v in s {
            f.extend_from_slice(&v.to_le_bytes());
        }
    }

    let mut h = Vec::new();
    h.extend_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for v in [2u16, 3] {
        h.extend_from_slice(&v.to_le_bytes()); // ET_EXEC, EM_386
    }
    for v in [1u32, start, 52, shoff, 0] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    for v in [52u16, 32, 1, 40, 6, 5] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    // PT_LOAD, offset, vaddr, paddr, filesz, memsz, flags r-x, align
    for v in [1u32, 0, BASE, BASE, load_end, load_end, 5, 0x1000] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    f[..h.len()].copy_from_slice(&h);
    f
}

#[test]
fn a_hand_built_elf32_reads_as_x86_32_with_its_functions_calls_and_string_reference() {
    let img = Image::from_bytes(elf32_by_hand(), PathBuf::from("/fx/elf32")).expect("the ELF32 opens");
    assert_eq!((img.bits, img.arch_name(), img.format_name()), (32, "x86", "ELF32"));
    let o = info(&img);
    let t = whole(&o);
    assert!(t.contains("ELF32 little-endian, x86"), "{t}");
    assert_eq!((o.meta["format"].as_str(), o.meta["bits"].as_u64()), (Some("ELF32"), Some(32)));

    let names = func_names(&img);
    for n in ["_start", "main", "helper"] {
        assert!(names.iter().any(|f| f == n), "{n} in {names:?}");
    }
    let m = fn_text(&img, "main");
    assert!(m.contains("push ebp") && m.contains("mov ebp, esp") && !m.contains("rbp"), "{m}");
    assert!(m.contains("call") && m.contains("helper"), "{m}");
    assert!(m.contains(&format!("\"{TEXT}\"")), "the pushed address is named as the string\n{m}");
    let h = fn_text(&img, "helper");
    assert!(h.contains("mov eax, [esp+4]"), "{h}");

    let c = whole(&calls(&img, "main").unwrap());
    assert!(c.contains("helper"), "{c}");
    let x = whole(&xref_str(&img, TEXT, false).unwrap());
    assert!(x.contains("main"), "{x}");
    let s = whole(&strings_window(&img, None, StrSort::Addr, None, usize::MAX).unwrap());
    assert!(s.contains(TEXT), "{s}");
    let fb = whole(&find_bytes(&img, "55 89 e5").unwrap());
    assert!(fb.contains("main"), "{fb}");
    let fi = whole(&find_imm(&img, i128::from(BASE + RODATA_OFF)).unwrap());
    assert!(fi.contains("main"), "{fi}");
    let b = whole(&bytes(&img, u64::from(BASE + TEXT_OFF + 6 + 4), 4, ElemType::Ptr).unwrap());
    assert!(b.contains(&format!("{:#010x}", BASE + RODATA_OFF)) && b.contains(TEXT), "{b}");
    let hits = crate::mainfind::find_main(&img);
    assert!(hits.iter().any(|h| h.addr == u64::from(BASE + TEXT_OFF + 6)), "{hits:?}");

    let main_at = BASE + TEXT_OFF + 6;
    let d = decomp_window(&img, Path::new("/nonexistent"), &["main".into()], false, None, 100).unwrap_err().to_string();
    assert!(d.contains("x86-32 is not supported") && d.contains(&format!("{main_at:#x}")), "{d}");
    let e = emulate(&img, &EmuSpec::new("main")).unwrap_err().to_string();
    assert!(e.contains("x86-32 is not supported"), "{e}");
    let sl = slice_window(&img, "main", Some("eax"), None, None, None, 100).unwrap_err().to_string();
    assert!(sl.contains("x86-32 is not supported"), "{sl}");
    // the debugger hooks stay empty: the debugger runs 64-bit programs only
    assert!(
        import_sites(&img).is_empty() && block_starts(&img, u64::from(main_at), u64::from(main_at + 18)).is_empty()
    );
    assert!(!opens_bytes_path(&img), "32-bit files are not offered to the 64-bit debugger");
}

#[test]
fn an_elf32_for_another_machine_is_named_and_not_read() {
    let mut x32 = elf32_by_hand();
    x32[18] = 62; // EM_X86_64 in a 32-bit file: the x32 ABI
    let e = Image::from_bytes(x32, "/x".into()).err().unwrap().to_string();
    assert!(
        e.starts_with("ELF32 x86-64 (the x32 ABI) is not read yet (32-bit ELF files are read for x86 only)"),
        "{e}"
    );
    let mut arm = elf32_by_hand();
    arm[18] = 40; // EM_ARM
    let e = Image::from_bytes(arm, "/x".into()).err().unwrap().to_string();
    assert!(e.starts_with("ELF32 ") && e.contains("is not read yet (32-bit ELF files are read for x86 only)"), "{e}");
}

/// `opens` reads the file from disk; write the hand-built bytes and ask.
fn opens_bytes_path(img: &Image) -> bool {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("elf32");
    std::fs::write(&p, &img.data).unwrap();
    opens(&p)
}

const SAMPLE: &str = r#"
#include <stdio.h>
const char *banner = "pointer in data";
__attribute__((noinline)) int helper(int x) { return x * 3 + 0x1234567; }
int main(int argc, char **argv) {
    (void)argv;
    puts("hello from x86-32");
    puts(banner);
    return helper(argc);
}
"#;

fn build(cc: &str, dir: &Path, name: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join("prog.c");
    std::fs::write(&c, SAMPLE).ok()?;
    let exe = dir.join(name);
    let ok =
        Command::new(cc).args(flags).arg("-o").arg(&exe).arg(&c).output().map(|o| o.status.success()).unwrap_or(false);
    (ok && exe.is_file()).then_some(exe)
}

fn addr_of(img: &Image, name: &str) -> u64 {
    img.funcs().iter().find(|f| f.name == name).unwrap_or_else(|| panic!("function {name}")).start
}

/// What every built sample must answer, ELF32 or PE32.
fn check_sample(img: &Image, tag: &str) {
    assert_eq!((img.bits, img.arch_name()), (32, "x86"), "{tag}");
    let names = func_names(img);
    for n in ["main", "helper"] {
        assert!(names.iter().any(|f| f == n), "{tag}: {n} in {names:?}");
    }
    let m = fn_text(img, "main");
    assert!(m.contains("esp") && !m.contains("rsp") && !m.contains("rbp"), "{tag}: 32-bit registers\n{m}");
    let c = whole(&calls(img, "main").unwrap());
    assert!(c.contains("puts") && c.contains("helper"), "{tag}\n{c}");
    let x = whole(&xref_str(img, "hello from x86-32", false).unwrap());
    assert!(x.contains("main"), "{tag}: the string reference\n{x}");
    let fi = whole(&find_imm(img, 0x1234567).unwrap());
    assert!(fi.contains("helper"), "{tag}\n{fi}");
    let s = whole(&strings_window(img, Some("pointer in data"), StrSort::Addr, None, usize::MAX).unwrap());
    assert!(s.contains("pointer in data"), "{tag}\n{s}");
    let banner = img.symbols.iter().find(|s| s.name == "banner").map(|s| s.addr);
    let banner = banner.unwrap_or_else(|| panic!("{tag}: the symbol banner"));
    let b = whole(&bytes(img, banner, 4, ElemType::Ptr).unwrap());
    assert!(b.contains("pointer in data"), "{tag}: a 4-byte pointer\n{b}");
    let ov = whole(&overview(img).unwrap());
    assert!(ov.contains(img.format_name()) && ov.contains("main"), "{tag}\n{ov}");
}

#[test]
fn gcc_m32_programs_read_as_x86_32_with_main_imports_strings_and_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let variants: [(&str, &[&str]); 3] = [
        ("pic", &["-m32", "-O0", "-no-pie"]),
        ("abs", &["-m32", "-O0", "-fno-pie", "-no-pie"]),
        ("pie", &["-m32", "-O0", "-fPIE", "-pie"]),
    ];
    let mut built = 0;
    for (tag, flags) in variants {
        let Some(exe) = build("gcc", dir.path(), tag, flags) else {
            eprintln!("SKIPPED gcc -m32 {tag}: no 32-bit C library here (gcc-multilib), nothing was checked");
            continue;
        };
        built += 1;
        let img = Image::open(&exe).unwrap();
        assert_eq!(img.format_name(), "ELF32", "{tag}");
        let t = whole(&info(&img));
        assert!(t.contains("ELF32 little-endian, x86"), "{tag}\n{t}");
        check_sample(&img, tag);
        let m = fn_text(&img, "main");
        assert!(m.contains("puts@plt"), "{tag}: the call names the import\n{m}");
        let main = addr_of(&img, "main");
        let hits = crate::mainfind::find_main(&img);
        assert!(hits.iter().any(|h| h.addr == main && h.strength == "proven"), "{tag}: {hits:?}");

        // the same program without its symbols: main is still found from the call to __libc_start_main
        let stripped = format!("{tag}-s");
        let flags_s: Vec<&str> = flags.iter().copied().chain(["-s"]).collect();
        let exe = build("gcc", dir.path(), &stripped, &flags_s).expect("builds stripped too");
        let img = Image::open(&exe).unwrap();
        let hits = crate::mainfind::find_main(&img);
        assert_eq!(hits.first().map(|h| h.addr), Some(main), "{stripped}: {hits:?}");
        let c = whole(&calls(&img, &format!("{main:#x}")).unwrap());
        assert!(c.contains("puts"), "{stripped}\n{c}");
    }
    if built == 0 {
        eprintln!("SKIPPED every gcc -m32 sample; the hand-built ELF32 test still ran");
    }
}

#[test]
fn an_i686_mingw_pe32_reads_as_x86_32_with_its_iat_imports() {
    let cc = "i686-w64-mingw32-gcc";
    if Command::new(cc).arg("--version").output().is_err() {
        eprintln!("SKIPPED an_i686_mingw_pe32: no {cc} here, so nothing was checked");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = build(cc, dir.path(), "prog.exe", &["-O0"]).expect("i686 mingw builds the sample");
    let img = Image::open(&exe).unwrap();
    assert_eq!(img.format_name(), "PE32");
    let t = whole(&info(&img));
    assert!(t.contains("PE32 little-endian, x86"), "{t}");
    let puts = img.imports.iter().find(|i| i.name == "puts").expect("puts is imported");
    let slot = puts.slot.expect("the IAT slot of puts");
    assert!(slot > img.image_base && slot < img.image_base + 0x10_0000, "{slot:#x}");
    // the IAT slot holds 4 bytes: the slot after it is the next import, not the high half of this one
    assert!(
        img.slots.contains_key(&slot) && img.slots.get(&(slot + 4)).is_some_and(|&i| img.imports[i].name != "puts")
    );
    check_sample(&img, "pe32");
    assert!(img.symbols.iter().any(|s| s.name == "main" && s.raw == "_main"), "decorated names come back plain");
}
