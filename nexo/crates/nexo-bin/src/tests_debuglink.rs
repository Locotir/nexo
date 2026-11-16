//! A stripped program and its separate debug file, found by build id and by `.gnu_debuglink`. The programs are
//! compiled here with the C compiler (a skip when there is none), the debug file is the unstripped build of the same
//! source, and the link section is written by a second source file that holds the CRC of that build.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{crc32, Format, Image};

const SRC: &str = "struct item { int id; long score; };\n\
    __attribute__((noinline)) int score_item(struct item *it, int k) { return it->id * k + (int)it->score; }\n\
    int main(int argc, char **argv) { struct item it = { argc, 2 }; (void)argv; return score_item(&it, 3) & 1; }\n";

const BUILD_ID: &str = "0x00112233445566778899aabbccddeeff01020304";

fn cc(dir: &Path, out: &str, args: &[&str], sources: &[&str]) -> bool {
    Command::new("cc")
        .current_dir(dir)
        .args(["-O0", "-no-pie", "-fno-pie"])
        .args(args)
        .args(["-o", out])
        .args(sources)
        .status()
        .is_ok_and(|s| s.success())
}

/// The unstripped build, written as `prog.debug` in `dir`, and its CRC. `None`, with the reason printed, when there
/// is no C compiler or it builds no ELF files: build ids and `.gnu_debuglink` are ELF's, and MinGW on Windows builds
/// PE files and writes a program named without a dot as `prog.exe`.
fn debug_build(dir: &Path) -> Option<u32> {
    std::fs::write(dir.join("a.c"), SRC).ok()?;
    let id = format!("-Wl,--build-id={BUILD_ID}");
    if !cc(dir, "prog.debug", &["-g", &id], &["a.c"]) {
        eprintln!("skipped: no C compiler");
        return None;
    }
    let img = Image::open(&dir.join("prog.debug")).ok()?;
    if img.format != Format::Elf {
        eprintln!("skipped: the system compiler builds {} files, not ELF", img.format_name());
        return None;
    }
    Some(crc32(&std::fs::read(dir.join("prog.debug")).ok()?))
}

/// A stripped build of the same source with the same build id, and (when `link` is given) a `.gnu_debuglink`
/// naming `link` with the CRC `crc`.
fn stripped_build(dir: &Path, out: &str, link: Option<(&str, u32)>) -> bool {
    let id = format!("-Wl,--build-id={BUILD_ID}");
    match link {
        None => cc(dir, out, &["-s", &id], &["a.c"]),
        Some((name, crc)) => {
            cc(dir, out, &["-Wl,--build-id=none"], &["a.c"]) && add_debuglink(&dir.join(out), name, crc)
        }
    }
}

/// Add the section to the file at `path`.
fn add_debuglink(path: &Path, name: &str, crc: u32) -> bool {
    let Some(b) = crate::fixtures::add_gnu_debuglink(&std::fs::read(path).unwrap(), name, crc) else { return false };
    std::fs::write(path, b).is_ok()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn has_protos(img: &Image) -> bool {
    img.dwarf().protos.values().any(|p| p.contains("score_item(struct item *it, i32 k)"))
}

#[test]
fn the_crc_of_a_debuglink_is_the_zlib_one() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    assert_eq!(crc32(b""), 0);
}

#[test]
fn a_stripped_program_takes_its_debug_file_by_build_id_and_says_which() {
    let d = scratch();
    let Some(_) = debug_build(d.path()) else { return };
    assert!(stripped_build(d.path(), "prog", None));
    let plain = Image::open(&d.path().join("prog")).unwrap();
    assert!(plain.debug_info_line().is_some_and(|l| l.contains("none in the file") && l.contains("build id 00112233")));
    assert!(!has_protos(&plain), "nothing to read without the debug file");
    assert!(plain.symbols.iter().all(|s| s.name != "score_item"), "a stripped program has lost its names");

    // the system folder: .build-id/00/112233....debug
    let sys = d.path().join("sys");
    let id = BUILD_ID.trim_start_matches("0x");
    let at = sys.join(".build-id").join(&id[..2]);
    std::fs::create_dir_all(&at).unwrap();
    std::fs::copy(d.path().join("prog.debug"), at.join(format!("{}.debug", &id[2..]))).unwrap();
    let img = Image::open_with_debug_dirs(&d.path().join("prog"), std::slice::from_ref(&sys)).unwrap();
    let line = img.debug_info_line().unwrap();
    assert!(line.contains("found by build id") && line.contains(&format!("{}.debug", &id[2..])), "{line}");
    assert!(has_protos(&img), "types come from the debug file: {:?}", img.dwarf().protos);
    assert!(img.dwarf().structs.contains_key("item"));
    assert!(img.debug_section(".debug_line").len() > 8, "the line table is the debug file's");
    let named = img.symbols.iter().find(|s| s.name == "score_item").expect("the function names come back");
    assert_eq!(img.name_at(named.addr), Some("score_item"));
    assert_eq!(crate::ops::resolve(&img, "score_item").unwrap().start, named.addr);
}

#[test]
fn a_debug_file_with_another_build_id_is_not_taken() {
    let d = scratch();
    let Some(_) = debug_build(d.path()) else { return };
    assert!(Command::new("cc")
        .current_dir(d.path())
        .args(["-g", "-Wl,--build-id=0xffff", "-o", "other.debug", "a.c"])
        .status()
        .unwrap()
        .success());
    assert!(stripped_build(d.path(), "prog", None));
    let sys = d.path().join("sys");
    let id = BUILD_ID.trim_start_matches("0x");
    std::fs::create_dir_all(sys.join(".build-id").join(&id[..2])).unwrap();
    std::fs::copy(
        d.path().join("other.debug"),
        sys.join(".build-id").join(&id[..2]).join(format!("{}.debug", &id[2..])),
    )
    .unwrap();
    let img = Image::open_with_debug_dirs(&d.path().join("prog"), &[sys]).unwrap();
    assert!(img.debug_lookup().unwrap().found.is_none());
    let line = img.debug_info_line().unwrap();
    assert!(line.contains("holds another build id"), "{line}");
}

#[test]
fn a_debuglink_finds_the_file_beside_the_program_in_a_debug_folder_or_under_the_system_folder() {
    let d = scratch();
    let Some(crc) = debug_build(d.path()) else { return };
    assert!(stripped_build(d.path(), "prog", Some(("prog.debug", crc))));
    let own = d.path().join("prog");
    let probe = Image::open_with_debug_dirs(&own, &[]).unwrap();
    // the debug file is in the folder of the program: taken
    let beside = probe.debug_lookup().unwrap();
    assert_eq!(beside.link, Some(("prog.debug".to_string(), crc)));
    let (path, how) = beside.found.clone().expect("found beside the program");
    assert!(path.ends_with("prog.debug") && how.contains("gnu_debuglink") && how.contains("matches"), "{path:?} {how}");
    assert!(has_protos(&probe));

    // moved to the `.debug` subfolder
    std::fs::create_dir(d.path().join(".debug")).unwrap();
    std::fs::rename(d.path().join("prog.debug"), d.path().join(".debug/prog.debug")).unwrap();
    let sub = Image::open_with_debug_dirs(&own, &[]).unwrap();
    assert!(sub.debug_lookup().unwrap().found.as_ref().unwrap().0.ends_with(".debug/prog.debug"));

    // moved under the system folder, in the program's own folder path
    let sys = d.path().join("sys");
    let folder: PathBuf = std::fs::canonicalize(d.path()).unwrap().components().skip(1).collect();
    std::fs::create_dir_all(sys.join(&folder)).unwrap();
    std::fs::rename(d.path().join(".debug/prog.debug"), sys.join(&folder).join("prog.debug")).unwrap();
    let under = Image::open_with_debug_dirs(&own, std::slice::from_ref(&sys)).unwrap();
    assert!(under.debug_lookup().unwrap().found.as_ref().unwrap().0.starts_with(&sys));
    // and nowhere: the answer names the file it wanted and where it looked
    let none = Image::open_with_debug_dirs(&own, &[]).unwrap();
    let line = none.debug_info_line().unwrap();
    assert!(line.contains("gnu_debuglink names prog.debug") && line.contains("does not exist"), "{line}");
}

#[test]
fn a_debug_file_whose_crc_differs_is_skipped_and_the_answer_says_so() {
    let d = scratch();
    let Some(crc) = debug_build(d.path()) else { return };
    assert!(stripped_build(d.path(), "prog", Some(("prog.debug", crc ^ 1))));
    let img = Image::open_with_debug_dirs(&d.path().join("prog"), &[]).unwrap();
    let l = img.debug_lookup().unwrap();
    assert!(l.found.is_none() && !has_protos(&img));
    let line = img.debug_info_line().unwrap();
    assert!(line.contains("has crc") && line.contains("not 0x"), "{line}");
}

#[test]
fn a_program_with_its_own_debug_info_looks_for_no_other_file() {
    let d = scratch();
    let Some(_) = debug_build(d.path()) else { return };
    let img = Image::open_with_debug_dirs(&d.path().join("prog.debug"), &[]).unwrap();
    assert!(img.debug_lookup().is_none() && img.debug_info_line().is_none());
    assert!(has_protos(&img));
}

#[test]
fn info_names_the_debug_file_it_read() {
    let d = scratch();
    let Some(crc) = debug_build(d.path()) else { return };
    assert!(stripped_build(d.path(), "prog", Some(("prog.debug", crc))));
    let img = Image::open(&d.path().join("prog")).unwrap();
    let text = crate::ops::info(&img).text_all();
    assert!(text.contains("debug info  read from") && text.contains("prog.debug"), "{text}");
}
