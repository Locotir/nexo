#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! One type view for `bin` and `debug`: the same `--as` text over the same bytes gives the same rows, for base
//! types, arrays, a struct named with `struct` and a struct from DWARF; a type nothing knows says what was looked at.
//! The tests share one data dir; each builds its own file (a global carries the test's name), so none sees another's names.
//! Skipped (with a line saying so) when there is no C compiler.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = "struct pt { int x; int y; char tag[8]; unsigned short v[3]; };\n\
    struct pt origin = { 7, -2, \"hello\", { 1, 2, 3 } };\n\
    unsigned nums[4] = { 10, 20, 30, 40 };\n\
    const char greeting[] = \"hi there\";\n\
    int main(void) { return origin.x + (int)nums[0] + greeting[0]; }\n";

fn data_dir() -> &'static Path {
    // A fixed folder under the target dir, emptied once per run: a static `TempDir` is never dropped.
    static D: OnceLock<PathBuf> = OnceLock::new();
    let d = D.get_or_init(|| {
        let p = Path::new(env!("CARGO_TARGET_TMPDIR")).join("type_view_data");
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    });
    nexo_dbg::set_data_dir(d.clone());
    d
}

fn run(s: &mut Session, cmd: &str) -> nexo_bin::Output {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}"))
}

/// The C file built to `t` (linked at a fixed address, so file and memory addresses agree).
fn build(test: &str, debug_info: bool) -> Option<(PathBuf, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("SKIPPED {test}: no C compiler, so nothing was checked");
        return None;
    };
    let src = dir.path().join("t.c");
    // a global named for the test makes every test's file its own, so its own saved names
    std::fs::write(&src, format!("{SRC}int salt_{test} = 1;\n")).unwrap();
    let exe = dir.path().join("t");
    let mut c = std::process::Command::new(cc);
    c.args(["-O0", "-fno-pie", "-no-pie"]);
    if debug_info {
        c.arg("-g");
    }
    assert!(c.arg("-o").arg(&exe).arg(&src).status().unwrap().success());
    Some((exe, dir))
}

/// The file as `bin` reads it and the program stopped in `main` as `debug` reads it, with the same `struct` names.
fn both(exe: &Path, structs: &[(&str, &str)]) -> (nexo_bin::Image, Session) {
    let dd = data_dir();
    let img = nexo_bin::Image::open(exe).unwrap();
    for (name, fields) in structs {
        nexo_bin::struct_set(&img, dd, name, Some(fields)).unwrap();
    }
    let mut img = nexo_bin::Image::open(exe).unwrap();
    nexo_bin::attach_names(&mut img, dd);
    let (mut s, _) = open(&Kind::Run { exe: exe.to_path_buf(), args: vec![] }).expect("start");
    run(&mut s, "break main");
    run(&mut s, "cont");
    (img, s)
}

fn addr(img: &nexo_bin::Image, sym: &str) -> u64 {
    nexo_bin::any_addr_in(img, sym).unwrap_or_else(|e| panic!("{sym}: {e}"))
}

#[test]
fn the_same_as_text_gives_the_same_rows_in_bin_and_debug() {
    let Some((exe, _d)) = build("the_same_as_text_gives_the_same_rows", false) else { return };
    let (img, mut s) = both(&exe, &[("recs", "0:i32:x 4:i32:y 8:u16:a")]);
    let (origin, nums, hi) = (addr(&img, "origin"), addr(&img, "nums"), addr(&img, "greeting"));
    // (type text, place, length that is given in both tools)
    let table: [(&str, u64, u64); 12] = [
        ("u8", nums, 16),
        ("u16", nums, 16),
        ("u32", nums, 16),
        ("i32", origin, 16),
        ("u64", nums, 16),
        ("f32", nums, 16),
        ("char[8]", origin + 8, 8),
        ("u32[4]", nums, 16),
        ("u16[3]", origin + 16, 6),
        ("str", hi, 9),
        ("struct recs", origin, 12),
        ("recs[2]", origin, 24),
    ];
    for (ty, at, len) in table {
        let file = nexo_bin::bytes(&img, at, len, ty).unwrap_or_else(|e| panic!("bin {ty}: {e}"));
        let live = run(&mut s, &format!("mem {at:#x} {len} --as \"{ty}\""));
        assert!(!file.rows.is_empty(), "{ty}");
        assert_eq!(live.text_all(), file.text_all(), "{ty}");
        assert_eq!(
            live.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            file.rows.iter().map(|r| r.key).collect::<Vec<_>>(),
            "{ty}"
        );
    }
}

#[test]
fn a_struct_named_with_struct_reads_the_same_in_both() {
    let Some((exe, _d)) = build("a_struct_named_with_struct_reads_the_same", false) else { return };
    let (img, mut s) = both(&exe, &[("rec", "0:i32:x 4:i32:y 8:char[8]:tag")]);
    let origin = addr(&img, "origin");
    for text in ["struct rec", "rec"] {
        let file = nexo_bin::bytes(&img, origin, 16, text).unwrap();
        let live = run(&mut s, &format!("mem {origin:#x} --as \"{text}\""));
        assert_eq!(live.text_all(), file.text_all(), "{text}");
        let t = file.text_all();
        assert!(t.contains("rec  (16 bytes)"), "{t}");
        assert!(row(&t, "+0x00").contains("x ") && row(&t, "+0x00").contains("= 7"), "{t}");
        assert!(row(&t, "+0x04").contains("= -2"), "{t}");
        assert!(row(&t, "+0x08").contains("tag") && row(&t, "+0x08").contains("\"hello\""), "{t}");
    }
}

#[test]
fn a_dwarf_struct_and_an_array_read_the_same_in_both() {
    let Some((exe, _d)) = build("a_dwarf_struct_and_an_array_read_the_same", true) else { return };
    let (img, mut s) = both(&exe, &[]);
    let (origin, nums) = (addr(&img, "origin"), addr(&img, "nums"));
    // no length: the struct is as long as its type
    let file = nexo_bin::bytes(&img, origin, 24, "struct pt").unwrap();
    let live = run(&mut s, &format!("mem {origin:#x} --as \"struct pt\""));
    assert_eq!(live.text_all(), file.text_all());
    let t = live.text_all();
    assert!(t.contains("pt  (24 bytes)"), "{t}");
    assert!(row(&t, "+0x00").contains("= 7"), "{t}");
    assert!(row(&t, "+0x04").contains("= -2"), "{t}");
    assert!(row(&t, "+0x08").contains("tag") && row(&t, "+0x08").contains("\"hello\""), "{t}");
    assert!(row(&t, "+0x10").contains("u16[3]") && row(&t, "+0x10").contains("[1, 2, 3]"), "{t}");
    // `ptype` says the same fields without reading memory
    let p = run(&mut s, "ptype struct pt").text_all();
    for want in ["struct pt", "24 bytes", "tag", "v"] {
        assert!(p.contains(want), "{want}: {p}");
    }
    // an array of base types: one row of the elements, the same in both
    let file = nexo_bin::bytes(&img, nums, 16, "u32[4]").unwrap();
    let live = run(&mut s, &format!("mem {nums:#x} --as u32[4]"));
    assert_eq!(live.text_all(), file.text_all());
    let t = live.text_all();
    for want in ["0x0000000a", "0x00000014", "0x0000001e", "0x00000028"] {
        assert!(t.contains(want), "{want}: {t}");
    }
    // `print` of an address as a type
    let pr = run(&mut s, &format!("print {origin:#x} --as \"struct pt\"")).text_all();
    assert_eq!(pr, live_struct(&mut s, origin), "{pr}");
}

fn row<'a>(text: &'a str, at: &str) -> &'a str {
    text.lines().find(|l| l.contains(at)).unwrap_or_else(|| panic!("no row at {at}: {text}"))
}

fn live_struct(s: &mut Session, at: u64) -> String {
    run(s, &format!("mem {at:#x} --as \"struct pt\"")).text_all()
}

#[test]
fn a_type_nothing_knows_says_what_was_looked_at() {
    let Some((exe, _d)) = build("a_type_nothing_knows_says_what_was_looked_at", false) else { return };
    let (img, mut s) = both(&exe, &[]);
    let nums = addr(&img, "nums");
    let file = nexo_bin::bytes(&img, nums, 16, "foo").unwrap_err().to_string();
    assert!(file.starts_with("no type `foo`: not a base type, not named with `struct`"), "{file}");
    assert!(file.contains("no debug info"), "{file}");
    let live = s.exec(&split_words(&format!("mem {nums:#x} 16 --as foo"))).unwrap_err().to_string();
    assert!(live.contains(&file), "{live} vs {file}");
    let p = s.exec(&split_words("ptype foo")).unwrap_err().to_string();
    assert!(p.contains("no type `foo`"), "{p}");
    // a wrong form is not called unknown
    let e = nexo_bin::bytes(&img, nums, 16, "u32[x]").unwrap_err().to_string();
    assert!(e.contains("not a type"), "{e}");
}
