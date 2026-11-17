//! AArch64 files through the commands: fixtures built in `fixture.rs`, no compiler needed.

use std::path::PathBuf;

use super::fixture::{self, Arm};
use crate::*;

fn open(f: &Arm) -> Image {
    Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("the fixture opens")
}

fn whole(o: &Output) -> String {
    o.page(None, 1000).text()
}

fn spans(img: &Image) -> Vec<(u64, u64)> {
    img.funcs().iter().map(|f| (f.start, f.end)).collect()
}

/// An address as a row prints it.
fn pad(a: u64) -> String {
    format!("{a:#010x}")
}

fn hex(a: u64) -> String {
    format!("{a:#x}")
}

#[test]
fn an_aarch64_elf_opens_and_says_arm64() {
    let f = fixture::elf(true);
    let img = open(&f);
    assert_eq!(img.arch_name(), "arm64");
    assert_eq!(img.entry, f.a);
    let o = info(&img);
    assert_eq!(o.meta["arch"], "arm64");
    assert!(whole(&o).contains("arm64"));
}

#[test]
fn functions_come_from_symbols_and_eh_frame() {
    let f = fixture::elf(true);
    let img = open(&f);
    assert_eq!(spans(&img), [(f.a, f.b), (f.b, f.c), (f.c, f.d), (f.d, f.e), (f.e, f.f), (f.f, f.text_end)]);
    assert_eq!(img.funcs()[0].name, "main");
    assert_eq!(img.funcs()[1].name, "helper");
    assert_eq!(img.funcs()[3].name, format!("sub_{:x}", f.d));
}

#[test]
fn without_eh_frame_calls_and_prologues_find_the_rest() {
    let f = fixture::elf(false);
    let img = open(&f);
    // F has no prologue and nobody calls it: it stays inside E, said plainly by the test
    assert_eq!(spans(&img), [(f.a, f.b), (f.b, f.c), (f.c, f.d), (f.d, f.e), (f.e, f.text_end)]);
}

#[test]
fn fn_lists_aarch64_instructions_with_what_they_point_at() {
    let f = fixture::elf(true);
    let img = open(&f);
    let main = whole(&func_view(&img, &FnQuery::new("main")).unwrap());
    assert!(main.contains("fn main"), "{main}");
    assert!(main.contains("stp x29, x30, [sp, #-16]!"), "{main}");
    assert!(main.contains(&format!("bl {:#x}  ; helper", f.b)), "{main}");
    assert!(main.contains(&format!("bl {:#x}", f.c)), "{main}");
    assert!(main.contains("ret"), "{main}");
    let helper = whole(&func_view(&img, &FnQuery::new("helper")).unwrap());
    assert!(helper.contains(&format!("adrp x0, {:#x}", f.string & !0xfff)), "{helper}");
    assert!(helper.contains(&format!("add x0, x0, #{:#x}  ; \"hello nexo bin\"", f.string & 0xfff)), "{helper}");
    assert!(helper.contains("mov w0, #0x539"), "{helper}");
    let d = whole(&func_view(&img, &FnQuery::new(&hex(f.d))).unwrap());
    assert!(d.contains("ldp x29, x30, [sp], #32"), "{d}");
}

#[test]
fn xref_and_calls_follow_adrp_pairs_and_branches() {
    let f = fixture::elf(true);
    let img = open(&f);
    let g = whole(&xref(&img, &hex(f.global)).unwrap());
    assert!(g.contains(&pad(f.e + 12)) && g.contains("write"), "{g}");
    assert!(g.contains(&pad(f.f + 4)) && g.contains("read"), "{g}");
    let s = whole(&xref(&img, &hex(f.string)).unwrap());
    assert!(s.contains(&pad(f.b + 4)) && s.contains(&pad(f.f + 12)), "{s}");
    let b = whole(&xref(&img, "helper").unwrap());
    assert!(b.contains(&pad(f.a + 8)) && b.contains(&pad(f.e + 16)), "{b}");
    let c = whole(&calls(&img, "main").unwrap());
    assert!(c.contains("helper") && c.contains(&pad(f.c)), "{c}");
    // pointers stored in data count too
    assert!(whole(&xref(&img, &hex(f.b)).unwrap()).contains(&pad(f.ptrs)));
}

#[test]
fn commands_that_read_x86_only_say_so_with_the_address() {
    let f = fixture::elf(true);
    let img = open(&f);
    let dir = std::env::temp_dir();
    let e = decomp_window(&img, &dir, &["main".to_string()], false, None, usize::MAX).unwrap_err();
    assert!(e.to_string().contains("does not read AArch64"), "{e}");
    assert!(e.to_string().contains(&hex(f.a)), "{e}");
    assert!(import_sites(&img).is_empty());
}

/// A program compiled for AArch64 by clang and linked by lld, when both are installed (the test
/// does nothing otherwise).
#[test]
fn a_clang_built_program_reads_back() {
    let have = |t: &str| std::process::Command::new(t).arg("--version").output().is_ok_and(|o| o.status.success());
    if !have("clang") || !have("ld.lld") {
        eprintln!("skipped: clang or ld.lld is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("p.c");
    let out = dir.path().join("p");
    std::fs::write(
        &src,
        "int g;\n\
         __attribute__((noinline)) int helper(int x) { return x * 3 + 1; }\n\
         __attribute__((noinline)) int work(int n) { g += n; return helper(n) + \"hello arm\"[n]; }\n\
         void _start(void) { work(2); for (;;) {} }\n",
    )
    .unwrap();
    let built = std::process::Command::new("clang")
        .args(["--target=aarch64-linux-gnu", "-O1", "-ffreestanding", "-nostdlib", "-static", "-fuse-ld=lld", "-o"])
        .arg(&out)
        .arg(&src)
        .output()
        .unwrap();
    if !built.status.success() {
        eprintln!("skipped: clang cannot build for aarch64 here: {}", String::from_utf8_lossy(&built.stderr));
        return;
    }
    let img = Image::open(&out).unwrap();
    assert_eq!(img.arch_name(), "arm64");
    let work = whole(&func_view(&img, &FnQuery::new("work")).unwrap());
    assert!(work.contains("bl ") && work.contains("helper"), "{work}");
    let helper = img.funcs().iter().find(|f| f.name == "helper").expect("helper is a function").start;
    let callers = whole(&xref(&img, "helper").unwrap());
    assert!(callers.contains("call") && callers.contains("work"), "{callers}");
    assert!(img.refs().callers(helper) >= 1);
    let g = img.symbols.iter().find(|s| s.name == "g").expect("g is a symbol").addr;
    let t = whole(&xref(&img, &hex(g)).unwrap());
    assert!(t.contains("read") && t.contains("write") && t.contains("work"), "{t}");
    assert!(whole(&xref_str(&img, "hello arm", false).unwrap()).contains("work"));
}

/// A function with a loop and an `if` inside it, as words:
/// ```text
/// +00 mov w0, #0        +04 mov w1, #42
/// +08 cmp w0, #10       +0c b.ge +20
/// +10 tbz w0, #0, +18   +14 add w1, w1, #1
/// +18 add w0, w0, #1    +1c b +08
/// +20 ret
/// ```
const LOOP_FN: [u32; 9] = [
    fixture::mov_w_imm(0, 0),
    fixture::mov_w_imm(1, 42),
    0x7100_281f,
    0x5400_00aa,
    0x3600_0040,
    0x1100_0421,
    0x1100_0400,
    0x17ff_fffb,
    fixture::RET,
];

fn loop_image() -> (Image, u64) {
    let f = fixture::elf_with(true, &LOOP_FN);
    (open(&f), f.g)
}

#[test]
fn cfg_of_a_loop_with_an_if_has_the_blocks_edges_and_loop() {
    let (img, g) = loop_image();
    let o = cfg_op(&img, &hex(g)).unwrap();
    let t = whole(&o);
    assert!(t.contains("blocks 6, edges 7, loops 1, instructions 9"), "{t}");
    let block = |lo: u64, hi: u64, n: usize| format!("block {lo:#x}..{hi:#x}  insns {n}");
    for (lo, hi, n) in [(0, 8, 2), (8, 0x10, 2), (0x10, 0x14, 1), (0x14, 0x18, 1), (0x18, 0x20, 2), (0x20, 0x24, 1)] {
        assert!(t.contains(&block(g + lo, g + hi, n)), "{lo:#x} {t}");
    }
    assert!(
        t.contains(&format!(
            "block {:#x}..{:#x}  insns 1  idom {:#x}  in loop {:#x}",
            g + 0x14,
            g + 0x18,
            g + 0x10,
            g + 8
        )),
        "{t}"
    );
    for (from, to, kind) in [
        (0, 8, "fallthrough"),
        (8, 0x20, "cond-true"),
        (8, 0x10, "cond-false"),
        (0x10, 0x18, "cond-true"),
        (0x10, 0x14, "cond-false"),
        (0x14, 0x18, "fallthrough"),
        (0x18, 8, "jump  ; back edge"),
    ] {
        assert!(t.contains(&format!("edge {:#x} -> {:#x}  {kind}", g + from, g + to)), "{from:#x} {to:#x} {t}");
    }
    assert!(t.contains(&format!("loop header {:#x}  4 blocks:", g + 8)), "{t}");
    assert!(t.contains("ends: ret"), "{t}");
    // the JSON rows are the x86 shape
    let back = o.rows.iter().filter_map(|r| r.json.as_ref()).find(|j| j["row"] == "edge" && j["back"] == true);
    let back = back.expect("a back edge row");
    assert_eq!((back["from"].as_u64(), back["to"].as_u64()), (Some(g + 0x18), Some(g + 8)));
    assert_eq!(back["kind"], "jump");
    assert_eq!(o.meta["loops"], 1);
}

#[test]
fn cfg_shows_calls_and_leaves_through_a_tail_branch() {
    let f = fixture::elf(true);
    let img = open(&f);
    let t = whole(&cfg_op(&img, "main").unwrap());
    assert!(t.contains("calls helper"), "{t}");
    assert!(t.contains(&format!("edge {:#x} -> {:#x}  call-return (helper)", f.a + 8, f.a + 12)), "{t}");
    assert!(t.contains("ends: ret"), "{t}");
}

#[test]
fn find_imm_matches_decoded_immediates() {
    let f = fixture::elf(true);
    let img = open(&f);
    let t = whole(&find_imm(&img, 1337).unwrap());
    assert!(t.contains(&pad(f.b + 8)) && t.contains("mov w0, #0x539"), "{t}");
    let (img, g) = loop_image();
    let t = whole(&find_imm(&img, 42).unwrap());
    assert!(t.contains(&pad(g + 4)) && t.contains("mov w1, #0x2a"), "{t}");
    // `cmp w0, #10` is a `subs` with an immediate
    assert!(whole(&find_imm(&img, 10).unwrap()).contains(&pad(g + 8)));
    assert!(!whole(&find_imm(&img, 43).unwrap()).contains(&pad(g + 4)));
}

#[test]
fn patch_nop_writes_aarch64_nops_and_refuses_what_is_wrong() {
    let f = fixture::elf(true);
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in");
    std::fs::write(&input, &f.bytes).unwrap();
    let img = Image::open(&input).unwrap();
    let out = dir.path().join("out");
    patch(&img, &out, PatchTarget::Address(f.c), PatchData::Nop(8)).unwrap();
    let again = Image::open(&out).unwrap();
    assert_eq!(again.read_at(f.c, 8).unwrap(), [0x1f, 0x20, 0x03, 0xd5, 0x1f, 0x20, 0x03, 0xd5]);
    let err = |at: u64, data: PatchData| {
        patch(&img, &dir.path().join("never"), PatchTarget::Address(at), data).unwrap_err().to_string()
    };
    assert!(err(f.c + 2, PatchData::Nop(4)).contains("4-aligned"));
    assert!(err(f.c, PatchData::Nop(6)).contains("multiple of 4"));
    assert!(err(f.c, PatchData::Asm("nop")).contains("does not assemble AArch64"));
    assert!(!dir.path().join("never").exists());
}
