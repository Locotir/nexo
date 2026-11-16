//! `syms`, and the places it names: an import's slot (`puts@got`, `__imp_ExitProcess`), and `main` and `entry` in a
//! stripped program. The C programs are built here with the system's compiler (the tests say so and stop when there
//! is none); the PE and Mach-O files are the byte-built fixtures.

use std::path::PathBuf;

use crate::tests_map::{compile, whole};
use crate::*;

const PROG: &str = r#"
#include <stdio.h>
#include <string.h>
int counter = 3;
int helper(const char *s) { return (int)strlen(s) + counter; }
int main(int argc, char **argv) { puts("hello"); return helper(argv[0]) + argc; }
"#;

const DYNAMIC: [&str; 3] = ["-O0", "-fno-pie", "-no-pie"];
const STRIPPED: [&str; 4] = ["-O0", "-fno-pie", "-no-pie", "-s"];

fn syms(img: &Image, kinds: &[&str]) -> String {
    let k: Vec<String> = kinds.iter().map(|s| s.to_string()).collect();
    whole(&syms_window(img, &k, None, usize::MAX).unwrap_or_else(|e| panic!("syms {kinds:?}: {e}")))
}

/// The C program as an ELF file, or `None` (said on stderr) when the system builds something else or nothing.
fn elf(dir: &std::path::Path, flags: &[&str]) -> Option<Image> {
    let img = Image::open(&compile(dir, PROG, flags)?).unwrap();
    if img.format != Format::Elf {
        eprintln!("skipped: the system compiler builds {} files, not ELF", img.format_name());
        return None;
    }
    Some(img)
}

#[test]
fn syms_lists_the_needed_libraries_the_imports_with_library_and_slot_and_the_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = elf(dir.path(), &DYNAMIC) else { return };
    let t = syms(&img, &[]);
    assert!(t.lines().next().unwrap().contains("needed") && t.contains("function symbols"), "{t}");
    assert!(t.lines().any(|l| l.contains("needed") && l.contains("libc.so")), "{t}");
    let puts = t.lines().find(|l| l.contains(" import ") && l.contains("puts")).unwrap_or_else(|| panic!("{t}"));
    assert!(puts.contains("libc.so") && puts.contains("GLIBC_") && puts.contains("puts@got"), "{puts}");
    let slot = img.imports.iter().find(|i| i.name == "puts").and_then(|i| i.slot).unwrap();
    assert!(puts.contains(&format!("slot {slot:#x}")), "{puts}");
    assert!(t.lines().any(|l| l.contains(" func ") && l.ends_with("  main")), "{t}");
    assert!(t.lines().any(|l| l.contains(" func ") && l.ends_with("  helper")), "{t}");
    assert!(t.lines().any(|l| l.contains(" data ") && l.ends_with("  counter")), "{t}");
    // the default kinds leave sections, slots and relocations out, and say how to get them
    assert!(t.contains("--kind section, got, reloc or all"), "{t}");
    assert!(!t.lines().any(|l| l.contains(" section ")), "{t}");
    let o = syms_window(&img, &[], None, usize::MAX).unwrap();
    assert!(o.meta["counts"]["import"].as_u64().unwrap() >= 2, "{}", o.meta);
    let p = o.page_as(None, 1000, true).value;
    let items = p["items"].as_array().unwrap_or_else(|| panic!("{p}"));
    let row = items.iter().find(|r| r["kind"] == "import" && r["name"] == "puts").unwrap();
    assert_eq!(row["slot"], slot, "{row}");
    assert_eq!(row["slot_name"], "puts@got", "{row}");
}

#[test]
fn the_other_kinds_list_sections_slots_and_relocations_and_plt_means_got() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = elf(dir.path(), &DYNAMIC) else { return };
    let slot = img.imports.iter().find(|i| i.name == "puts").and_then(|i| i.slot).unwrap();
    let got = syms(&img, &["plt"]);
    assert!(got.contains("listed: got"), "{got}");
    let row = got.lines().find(|l| l.contains("puts@got")).unwrap_or_else(|| panic!("{got}"));
    assert!(row.starts_with(&format!("{slot:#010x}  got")) && row.contains("slot of puts"), "{row}");
    let sec = syms(&img, &["section"]);
    assert!(sec.lines().any(|l| l.contains(" section ") && l.contains(".text") && l.contains("r-x")), "{sec}");
    let rel = syms(&img, &["reloc"]);
    let jump = if img.arm64 { "R_AARCH64_JUMP_SLOT" } else { "R_X86_64_JUMP_SLOT" };
    let row = rel.lines().find(|l| l.contains(jump) && l.contains("puts")).unwrap_or_else(|| panic!("{rel}"));
    assert!(row.starts_with(&format!("{slot:#010x}  reloc")) && row.contains("[puts@got]"), "{row}");
    let all = syms(&img, &["all"]);
    assert!(all.contains("listed: needed, import, export, func, data, section, got, reloc"), "{all}");
    assert!(!all.contains("also --kind"), "{all}");
    let e = syms_window(&img, &["symbol".into()], None, 100).unwrap_err().to_string();
    assert!(e.contains("not a kind") && e.contains("import"), "{e}");
}

#[test]
fn an_import_slot_is_a_place_for_bytes_xref_and_fn() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = elf(dir.path(), &DYNAMIC) else { return };
    let slot = img.imports.iter().find(|i| i.name == "puts").and_then(|i| i.slot).unwrap();
    assert_eq!(addr_in(&img, "puts@got").unwrap(), slot);
    assert_eq!(range_in(&img, "puts@got").unwrap(), (slot, slot + 8), "a slot is one pointer");
    assert_eq!(addr_in(&img, "puts@got+8").unwrap(), slot + 8);
    let x = whole(&xref_window(&img, "puts@got", None, 1000).unwrap());
    assert!(x.contains(&format!("xref {slot:#x}")), "{x}");
    match img.funcs().iter().find(|f| f.name == "puts@plt") {
        // the stub jumps through the slot, so the function of the slot is the stub
        Some(stub) => {
            let f = whole(&func_window(&img, "puts@got", false, None, 1000).unwrap());
            assert!(f.contains("puts@plt") && f.contains(&format!("{:#x}", stub.start)), "{f}");
            assert!(x.contains("puts@plt"), "{x}");
        }
        None => {
            let e = func_window(&img, "puts@got", false, None, 1000).unwrap_err().to_string();
            assert!(e.contains("holds data, not code") && e.contains("bytes"), "{e}");
        }
    }
}

#[test]
fn main_and_entry_are_places_in_a_stripped_program() {
    let dir = tempfile::tempdir().unwrap();
    let Some(full) = elf(dir.path(), &DYNAMIC) else { return };
    let Some(img) = elf(dir.path(), &STRIPPED) else { return };
    let main = full.symbols.iter().find(|s| s.name == "main").unwrap().addr;
    assert!(!img.symbols.iter().any(|s| s.name == "main"), "the -s build has no symbol main");
    assert_eq!(addr_in(&img, "main").unwrap(), main);
    let f = func_window(&img, "main", false, None, 1000).unwrap();
    assert!(whole(&f).contains(&format!("{main:#x}")), "{}", whole(&f));
    let c = whole(&cfg_op(&img, "main").unwrap());
    assert!(c.contains(&format!("{main:#x}")), "{c}");
    let d = decomp_window(&img, std::path::Path::new("/nonexistent-nexo-data"), &["main".into()], false, None, 1000);
    assert!(d.is_ok(), "{:?}", d.err());
    for name in ["entry", "start", "_start"] {
        assert_eq!(addr_in(&img, name).unwrap(), img.entry, "{name}");
    }
    assert_eq!(func_window(&img, "entry", false, None, 1000).unwrap().meta["start"], full.entry, "entry");
    let t = syms(&img, &[]);
    assert!(t.contains("no function symbols (stripped)") && t.contains("fn") && t.contains(" main"), "{t}");
    // the dynamic symbols are still there: the imports keep their names
    assert!(t.lines().any(|l| l.contains(" import ") && l.contains("puts")), "{t}");
}

#[test]
fn a_static_program_says_it_loads_no_library() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = elf(dir.path(), &["-O0", "-static"]) else { return };
    let t = syms(&img, &[]);
    assert!(t.contains("statically linked"), "{t}");
    assert!(t.contains("needed: none in this file") && t.contains("import: none in this file"), "{t}");
    assert!(t.lines().any(|l| l.contains(" func ") && l.ends_with("  main")), "{t}");
}

#[test]
fn a_pe_import_has_its_dll_and_its_iat_slot_named_imp() {
    let fx = fixtures::pe64_xref();
    let img = Image::from_bytes(fx.bytes, PathBuf::from("/fx/prog.exe")).unwrap();
    let slot = fx.image_base + 0x4050;
    assert_eq!(addr_in(&img, "__imp_ExitProcess").unwrap(), slot);
    assert_eq!(addr_in(&img, "ExitProcess@got").unwrap(), slot, "the name of the other formats works too");
    let t = syms(&img, &["import", "got", "needed"]);
    assert!(t.lines().any(|l| l.contains("needed") && l.contains("KERNEL32.dll")), "{t}");
    let row = t.lines().find(|l| l.contains(" import ") && l.contains("ExitProcess")).unwrap_or_else(|| panic!("{t}"));
    assert!(row.contains("KERNEL32.dll") && row.contains("__imp_ExitProcess"), "{row}");
    assert!(t.contains("1 IAT slots"), "{t}");
    assert!(t.lines().any(|l| l.starts_with(&format!("{slot:#010x}  iat")) && l.contains("__imp_ExitProcess")), "{t}");
    let x = whole(&xref_window(&img, "__imp_ExitProcess", None, 1000).unwrap());
    assert!(x.contains(&format!("{:#x}", fx.c)), "C calls through the slot\n{x}");
    let e = func_window(&img, "__imp_ExitProcess", false, None, 1000).unwrap_err().to_string();
    assert!(e.contains("holds data, not code") && e.contains("__imp_ExitProcess --as ptr"), "{e}");
}

#[test]
fn a_mach_o_import_has_its_library_its_stub_and_its_got_slot() {
    use crate::tests_macho::{thin, GOT, STUB};
    let img = Image::from_bytes(thin(false, false).bytes, PathBuf::from("/fx/prog")).unwrap();
    assert_eq!(addr_in(&img, "puts@got").unwrap(), GOT);
    assert_eq!(addr_in(&img, "_puts@got").unwrap(), GOT, "the name as the file writes it");
    let t = syms(&img, &["needed", "import", "reloc"]);
    assert!(t.lines().any(|l| l.contains("needed") && l.contains("libSystem.B.dylib")), "{t}");
    let row = t.lines().find(|l| l.contains(" import ") && l.contains("puts")).unwrap_or_else(|| panic!("{t}"));
    assert!(row.starts_with(&format!("{STUB:#010x}")) && row.contains("stub puts@plt"), "{row}");
    assert!(row.contains(&format!("slot {GOT:#x} puts@got")), "{row}");
    assert!(t.lines().any(|l| l.starts_with(&format!("{GOT:#010x}  bind")) && l.contains("puts@got")), "{t}");
    let f = whole(&func_window(&img, "puts@got", false, None, 1000).unwrap());
    assert!(f.contains("puts@plt"), "{f}");
}

#[test]
fn a_wasm_module_lists_its_imports_exports_and_named_functions() {
    let kinds = Vec::new();
    let a = WasmAsk { op: "syms", kinds: &kinds, budget: 200, ..WasmAsk::default() };
    let t = wasm_answer_bytes(&crate::wasm_fixture::module(), "/fx/m.wasm", &a).unwrap().page(None, 1000).text();
    assert!(t.contains("fd_write") && t.contains("wasi_snapshot_preview1") && t.contains("func[0]"), "{t}");
    assert!(t.lines().any(|l| l.contains(" export ") && l.contains("_start")), "{t}");
    assert!(t.lines().any(|l| l.contains(" func ") && l.contains("greet")), "{t}");
    let kinds = vec!["reloc".to_string()];
    let a = WasmAsk { op: "syms", kinds: &kinds, budget: 200, ..WasmAsk::default() };
    let t = wasm_answer_bytes(&crate::wasm_fixture::module(), "/fx/m.wasm", &a).unwrap().page(None, 1000).text();
    assert!(t.contains("reloc: not in WebAssembly"), "{t}");
}
