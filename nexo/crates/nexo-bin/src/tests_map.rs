//! Tests of the map: `overview`, the call graph and the recognised constants, on small C programs that
//! the test compiles itself (skipped, with a message, when there is no C compiler).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::*;

/// A program that calls through a chain: main -> top -> mid -> leaf, plus a side branch to the network.
pub(crate) const CHAIN: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
__attribute__((noinline)) int leaf(int x) { return x * 3 + 1; }
__attribute__((noinline)) int mid(int x) { return leaf(x) + leaf(x + 1); }
__attribute__((noinline)) int top(int x) { puts("top reached"); return mid(x); }
__attribute__((noinline)) int other(int x) { return x - 1; }
__attribute__((noinline)) int net(const char *h) {
    int s = socket(2, 1, 0);
    FILE *f = fopen(h, "r");
    if (f) fclose(f);
    return s;
}
int main(int argc, char **argv) {
    puts("usage: prog");
    if (argc > 3) return net(argv[1]);
    return top(argc) + other(argc);
}
"#;

/// Compile `src` with the given flags into `dir`; None (and a message) when no compiler builds it.
pub(crate) fn compile(dir: &Path, src: &str, flags: &[&str]) -> Option<PathBuf> {
    let c = dir.join("prog.c");
    std::fs::write(&c, src).ok()?;
    let exe = dir.join(format!("prog{}", flags.join("").replace(['-', '=', ' '], "")));
    let exe = exe.with_extension("exe");
    let ok = ["cc", "gcc", "clang"].iter().any(|cc| {
        Command::new(cc).args(flags).arg("-o").arg(&exe).arg(&c).output().map(|o| o.status.success()).unwrap_or(false)
    });
    if !ok || !exe.is_file() {
        eprintln!("skipped: no C compiler that builds the sample");
        return None;
    }
    Some(exe)
}

pub(crate) fn whole(o: &Output) -> String {
    o.page(None, usize::MAX / 2).text()
}

fn addr_of(img: &Image, name: &str) -> u64 {
    img.symbols.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("symbol {name}")).addr
}

#[test]
fn overview_finds_the_real_main_with_its_evidence_even_when_stripped() {
    let dir = tempfile::tempdir().unwrap();
    for flags in
        [&["-O0", "-fno-pie", "-no-pie"][..], &["-O0", "-fPIE", "-pie"][..], &["-O0", "-fno-pie", "-no-pie", "-s"][..]]
    {
        let Some(exe) = compile(dir.path(), CHAIN, flags) else { return };
        let img = Image::open(&exe).unwrap();
        let o = overview(&img).unwrap();
        let t = whole(&o);
        let mains = o.meta["main"].as_array().unwrap();
        let first = &mains[0];
        assert_eq!(first["strength"], "proven", "{flags:?}\n{t}");
        let reference = Image::open(&compile(dir.path(), CHAIN, &["-O0", "-fno-pie", "-no-pie"]).unwrap()).unwrap();
        if !flags.contains(&"-s") && !flags.contains(&"-pie") {
            assert_eq!(first["addr"], addr_of(&img, "main"), "{t}");
        }
        if flags.contains(&"-s") {
            // stripped: no symbol, but the address is the one the unstripped build has for its own layout
            assert!(img.symbols.iter().all(|s| s.name != "main"));
            assert_eq!(first["addr"], addr_of(&reference, "main"), "same code, same layout\n{t}");
        }
        let ev = first["evidence"].as_str().unwrap();
        assert!(ev.contains("__libc_start_main") && ev.contains("rdi"), "{ev}");
        assert!(t.contains("network") && t.contains("socket("), "{t}");
        assert!(t.contains("files") && t.contains("fopen("), "{t}");
        assert!(t.contains("\"usage: prog\""), "{t}");
        assert!(t.contains("nexo bin decomp"), "{t}");
        assert!(t.lines().count() < 40, "one bounded call:\n{t}");
    }
}

#[test]
fn overview_calls_a_symbol_a_symbol_not_proof() {
    let f = crate::fixtures::elf64_xref();
    let img = Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).unwrap();
    let o = overview(&img).unwrap();
    let t = whole(&o);
    assert_eq!(o.meta["main"][0]["strength"], "symbol", "{t}");
    assert!(t.contains("[symbol] the symbol table names"), "{t}");
    assert!(t.contains("nexo bin decomp"), "{t}");
}

#[test]
#[cfg_attr(windows, ignore = "rustc keeps Windows symbols in a .pdb, not read yet")]
fn overview_follows_a_rust_program_to_its_own_main() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hello.rs");
    std::fs::write(&src, "fn greet(n: u32) -> u32 { println!(\"rust says hi {n}\"); n + 1 }\nfn main() { let a = std::env::args().count() as u32; println!(\"{}\", greet(a)); }\n").unwrap();
    let exe = dir.path().join("hello");
    let ok = Command::new("rustc")
        .args(["-C", "opt-level=0", "-o"])
        .arg(&exe)
        .arg(&src)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipped: no rustc");
        return;
    }
    let img = Image::open(&exe).unwrap();
    let o = overview(&img).unwrap();
    let t = whole(&o);
    let hit =
        o.meta["main"].as_array().unwrap().iter().find(|m| m["role"] == "Rust main").unwrap_or_else(|| panic!("{t}"));
    assert_eq!(hit["strength"], "proven", "{t}");
    let f = img.funcs().iter().find(|f| f.start == hit["addr"].as_u64().unwrap()).unwrap();
    assert!(f.name.ends_with("hello::main"), "{}\n{t}", f.name);
    assert!(hit["evidence"].as_str().unwrap().contains("std::rt::lang_start"), "{t}");
}

#[test]
fn overview_finds_main_in_a_windows_program_and_calls_it_inferred() {
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path().join("w.c");
    std::fs::write(&c, "#include <stdio.h>\n#include <windows.h>\n__attribute__((noinline)) int work(int x) { Sleep(x); return x * 2; }\nint main(int argc, char **argv) { printf(\"win main\\n\"); return work(argc); }\n").unwrap();
    for strip in [false, true] {
        let exe = dir.path().join(if strip { "w_s.exe" } else { "w.exe" });
        let mut cmd = Command::new("x86_64-w64-mingw32-gcc");
        cmd.args(["-O0", "-o"]).arg(&exe).arg(&c);
        if strip {
            cmd.arg("-s");
        }
        if !cmd.output().map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("skipped: no mingw");
            return;
        }
        let img = Image::open(&exe).unwrap();
        let o = overview(&img).unwrap();
        let t = whole(&o);
        let mains = o.meta["main"].as_array().unwrap();
        let first = mains.first().unwrap_or_else(|| panic!("{t}"));
        assert_eq!(first["strength"], "inferred", "{t}");
        let printf_users = img.strings().iter().find(|s| s.text == "win main").expect("string");
        let refs: Vec<_> = img.refs().within(printf_users.addr, printf_users.byte_len).iter().collect();
        let holder = img.func_at(refs[0].src).unwrap();
        assert_eq!(first["addr"], holder.start, "main is the function that uses its string\n{t}");
    }
}

fn chain_image(dir: &Path) -> Option<Image> {
    let exe = compile(dir, CHAIN, &["-O0", "-fno-pie", "-no-pie"])?;
    Some(Image::open(&exe).unwrap())
}

#[test]
fn path_follows_a_chain_of_calls_and_names_each_call_site() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = chain_image(dir.path()) else { return };
    let o = path_window(&img, "main", "leaf", false, 3, None, 200).unwrap();
    let t = whole(&o);
    assert_eq!(o.meta["hops"], 3, "{t}");
    for n in ["main", "top", "mid", "leaf"] {
        assert!(t.contains(n), "{n} missing\n{t}");
    }
    assert!(t.matches("--call at 0x").count() == 3, "{t}");
    // an import is a node too
    let o = path_window(&img, "main", "socket", false, 3, None, 200).unwrap();
    assert!(whole(&o).contains("net"), "{}", whole(&o));
}

#[test]
fn path_says_no_route_and_what_it_could_not_follow() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = chain_image(dir.path()) else { return };
    let o = path_window(&img, "leaf", "main", false, 3, None, 200).unwrap();
    let t = whole(&o);
    assert!(t.contains("no route found") && t.contains("reaches"), "{t}");
    assert_eq!(o.meta["routes"], 0);
    let o = path_window(&img, "top", "other", false, 3, None, 200).unwrap();
    assert!(whole(&o).contains("no route found"));
}

#[test]
fn tree_shows_callees_and_callers_to_the_depth_asked() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = chain_image(dir.path()) else { return };
    let o = tree_window(&img, "main", false, 3, false, None, 200).unwrap();
    let t = whole(&o);
    assert!(t.contains("    mid ") && t.contains("      leaf "), "{t}");
    assert!(t.contains("x2"), "mid calls leaf twice\n{t}");
    let o = tree_window(&img, "main", false, 1, false, None, 200).unwrap();
    assert!(whole(&o).contains("more callees"), "{}", whole(&o));
    let o = tree_window(&img, "leaf", true, 3, false, None, 200).unwrap();
    let t = whole(&o);
    assert!(t.contains("  mid ") && t.contains("    top ") && t.contains("      main "), "{t}");
    // a tree is paged like every answer
    let p = tree_window(&img, "main", false, 3, false, None, 10).unwrap().page(None, 10);
    assert!(p.next.is_some_and(|n| n.contains("tree") && n.contains("--from")));
}

/// A program with tables typed by hand from the standards (the oracle): SHA-256 K, a CRC-32 table built
/// at compile time is not used, so the first and last rows are typed, a base64 alphabet and the ChaCha
/// constant. `sha_block` and `b64` use them; `lonely` has a table nothing refers to.
const CRYPTO: &str = r#"
#include <stdio.h>
#include <string.h>
#include <stdint.h>
static const uint32_t K[64] = {
0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};
static const char B64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
static const uint8_t SIGMA[16] = "expand 32-byte k";
static const uint32_t WEAK[64] = {0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
 1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56};
__attribute__((noinline)) uint32_t sha_round(uint32_t a, int i) { return a + K[i & 63]; }
__attribute__((noinline)) char b64(int i) { return B64[i & 63]; }
__attribute__((noinline)) int sigma_ok(const uint8_t *s) { return memcmp(s, SIGMA, 16) == 0; }
__attribute__((noinline)) uint32_t weak(int i) { return WEAK[i & 63]; }
int main(int argc, char **argv) {
    printf("%u %c %d %u\n", sha_round(argc, argc), b64(argc), sigma_ok((const uint8_t *)argv[0]), weak(argc));
    return 0;
}
"#;

#[test]
fn crypto_finds_tables_with_evidence_and_does_not_name_a_table_that_is_not_there() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), CRYPTO, &["-O1", "-fno-pie", "-no-pie"]) else { return };
    let img = Image::open(&exe).unwrap();
    let hits = crypto_scan(&img);
    let sha: Vec<_> = hits.iter().filter(|h| h.id == "sha256_K").collect();
    assert_eq!(sha.len(), 2, "one full table and one prefix-only table: {hits:#?}");
    let full = sha.iter().find(|h| h.strength == "full").expect("the full table");
    let part = sha.iter().find(|h| h.strength == "prefix").expect("the altered table is only a prefix");
    assert_eq!(part.len, 32, "the matching prefix is eight words");
    let user = img.func_at(full.users[0].0).unwrap();
    assert!(img.func_name(user).contains("sha_round") || user.start == addr_of(&img, "sha_round"), "{full:#?}");
    for id in ["base64_alphabet", "chacha_sigma"] {
        assert!(hits.iter().any(|h| h.id == id && h.strength == "full"), "{id} missing: {hits:#?}");
    }
    // no AES, MD5 or CRC table in this program, and nothing is named for them
    for id in ["aes_sbox", "md5_T", "crc32_table", "sha512_K"] {
        assert!(!hits.iter().any(|h| h.id == id), "{id} must not be reported: {hits:#?}");
    }
}

#[test]
fn crypto_save_names_tables_and_notes_users_without_replacing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), CRYPTO, &["-O1", "-fno-pie", "-no-pie"]) else { return };
    let data = tempfile::tempdir().unwrap();
    let mut img = Image::open(&exe).unwrap();
    let round = addr_of(&img, "sha_round");
    // the person already noted one function and named nothing
    names::set_note(&img, data.path(), round, "my own note").unwrap();
    let _ = attach_names(&mut img, data.path());
    let o = crypto_window(&img, Some(data.path()), None, 200).unwrap();
    let t = whole(&o);
    assert!(t.contains("--save:") && t.contains("kept the note"), "{t}");
    let mut img = Image::open(&exe).unwrap();
    let _ = attach_names(&mut img, data.path());
    assert_eq!(img.note_at(round), Some("my own note"));
    let full = crypto_scan(&img).into_iter().find(|h| h.id == "sha256_K" && h.strength == "full").unwrap();
    assert_eq!(img.user_name(full.addr), Some("sha256_K"));
    // a second run names nothing twice and changes nothing
    let o2 = crypto_window(&img, Some(data.path()), None, 200).unwrap();
    assert!(whole(&o2).contains("kept the name sha256_K"), "{}", whole(&o2));
    let b64 = addr_of(&img, "b64");
    assert!(img.note_at(b64).is_some_and(|n| n.contains("base64")), "{:?}", img.note_at(b64));
}

/// A function whose result is built from a constant, an argument, a global, a call and a stack slot.
const SLICE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
int g_limit = 7;
__attribute__((noinline)) int source(int x) { return x ^ 0x55; }
__attribute__((noinline)) int mix(int a, int b) {
    int local = a * 3;
    int k = 40;
    int r = source(b);
    local = local + k + g_limit;
    return printf("%d\n", local + r);
}
int main(int argc, char **argv) { return mix(argc, argc + 1) + (int)(long)argv; }
"#;

fn call_to(img: &Image, func: &str, callee: &str) -> u64 {
    let t = img.func_at(addr_of(img, func)).unwrap();
    img.refs()
        .from(t.start, t.end)
        .find(|r| {
            r.kind == crate::Kind::Call
                && crate::graph::node_name(img, crate::graph::node_of(img, r.target)).starts_with(callee)
        })
        .unwrap_or_else(|| panic!("no call to {callee} in {func}"))
        .src
}

#[test]
fn slice_follows_registers_and_slots_back_to_their_sources_and_says_where_it_stops() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = compile(dir.path(), SLICE, &["-O0", "-fno-pie", "-no-pie"]) else { return };
    let img = Image::open(&exe).unwrap();
    // the arguments of printf: the format string and the sum
    let printf_call = call_to(&img, "mix", "printf");
    let o = slice_window(&img, &format!("{printf_call:#x}"), None, Some(2), None, None, 200).unwrap();
    let t = whole(&o);
    assert!(t.contains("global g_limit"), "the global is a stop\n{t}");
    assert!(t.contains("result of the call to source"), "the call result is a stop with the callee named\n{t}");
    assert!(t.contains("[proven]"), "{t}");
    assert!(t.contains("add eax, eax") || t.contains("imul") || t.contains("lea"), "a*3 is in the slice\n{t}");
    assert!(
        t.contains("is argument 1 of mix") || t.contains("is argument 2 of mix") || t.contains("never written"),
        "{t}"
    );
    // the constant 40 is a step that ends in a constant
    assert!(t.contains("sets ") && t.contains("to a constant"), "{t}");
    // the call itself is a step, and the slice never includes the unrelated main
    assert!(!t.contains("main"), "{t}");
    // a slot given by hand
    let o = slice_window(&img, &format!("{printf_call:#x}"), None, None, Some("rbp-0x4"), None, 200);
    assert!(o.is_ok());
    // an address inside an instruction is refused
    assert!(slice_window(&img, &format!("{:#x}", printf_call + 1), None, None, None, None, 50).is_err());
}

#[test]
fn slice_marks_a_value_set_on_two_paths_as_inferred() {
    let dir = tempfile::tempdir().unwrap();
    let src = r#"
#include <stdio.h>
__attribute__((noinline)) int pick(int c) { int v; if (c) v = 5; else v = 9; return printf("%d", v); }
int main(int argc, char **argv) { (void)argv; return pick(argc); }
"#;
    let Some(exe) = compile(dir.path(), src, &["-O0", "-fno-pie", "-no-pie"]) else { return };
    let img = Image::open(&exe).unwrap();
    let call = call_to(&img, "pick", "printf");
    let t = whole(&slice_window(&img, &format!("{call:#x}"), None, Some(2), None, None, 200).unwrap());
    assert!(t.contains("[inferred]"), "a branch joins before the use\n{t}");
}
