//! Tests of names (demangling) and file formats beyond ELF64/PE32+ x86-64. Programs are compiled by the
//! test itself; a missing compiler skips the test with a message and never fails it.

use std::path::Path;
use std::process::Command;

use crate::tests_map::whole;
use crate::*;

fn run_ok(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// A C++ program with a namespaced class, built with g++ (Itanium names).
fn cxx_image(dir: &Path) -> Option<Image> {
    let src = dir.join("p.cpp");
    std::fs::write(
        &src,
        "namespace geo { struct Shape { int n; int area(int k) const; }; \
         __attribute__((noinline)) int Shape::area(int k) const { return n * k + 7; } }\n\
         int main(int argc, char **) { geo::Shape s{argc}; return s.area(3); }\n",
    )
    .ok()?;
    let exe = dir.join("p_cxx");
    if !run_ok(Command::new("g++").args(["-O0", "-fno-pie", "-no-pie", "-o"]).arg(&exe).arg(&src)) {
        eprintln!("skipped: no g++");
        return None;
    }
    Some(Image::open(&exe).unwrap())
}

#[test]
fn an_itanium_name_is_shown_demangled_and_the_raw_name_still_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let Some(img) = cxx_image(dir.path()) else { return };
    let raw = "_ZNK3geo5Shape4areaEi";
    let f = img.funcs().iter().find(|f| f.name.contains("geo::Shape::area")).expect("demangled name");
    assert_eq!(img.raw_name_at(f.start), Some(raw));
    // the raw name finds the function, in fn, xref and funcs --grep
    let by_raw = fn_text(&img, raw);
    let by_name = fn_text(&img, "geo::Shape::area");
    assert!(by_raw.starts_with("fn geo::Shape::area(int) const"), "{by_raw}");
    assert!(by_raw.contains(&format!("; raw {raw}")), "{by_raw}");
    assert_eq!(by_raw, by_name);
    let x = whole(&xref(&img, raw).unwrap());
    assert!(x.contains("geo::Shape::area"), "{x}");
    let l = whole(&func_list(&img, FuncSort::Addr, Some("^_ZNK3geo"), 0).unwrap());
    assert!(l.contains("geo::Shape::area"), "{l}");
    // callers show the demangled name too
    let m = whole(&func(&img, "main", false).unwrap());
    assert!(m.contains("geo::Shape::area"), "{m}");
    let c = whole(&calls(&img, "main").unwrap());
    assert!(c.contains("geo::Shape::area") && !c.contains("_ZNK3geo"), "{c}");
    let d = decomp_window(&img, dir.path(), &["main".to_string()], false, None, usize::MAX).unwrap();
    let d = whole(&d);
    assert!(d.contains("geo::Shape::area") && !d.contains("_ZNK3geo"), "{d}");
    let o = whole(&overview(&img).unwrap());
    assert!(!o.contains("_ZNK3geo"), "{o}");
}

fn fn_text(img: &Image, t: &str) -> String {
    whole(&func(img, t, false).unwrap())
}

#[test]
fn rust_legacy_and_v0_names_are_demangled_and_keep_their_raw_form() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("r.rs");
    std::fs::write(&src, "#[inline(never)] fn tally(n: u32) -> u32 { n * 3 + 1 }\nfn main() { println!(\"{}\", tally(std::env::args().count() as u32)); }\n").unwrap();
    for (tag, extra) in [("legacy", "legacy"), ("v0", "v0")] {
        let exe = dir.path().join(format!("r_{tag}"));
        if !run_ok(
            Command::new("rustc")
                .args(["-C", "opt-level=0", "-C"])
                .arg(format!("symbol-mangling-version={extra}"))
                .arg("-o")
                .arg(&exe)
                .arg(&src),
        ) {
            eprintln!("skipped: no rustc");
            return;
        }
        let img = Image::open(&exe).unwrap();
        let f = img
            .funcs()
            .iter()
            .find(|f| f.name.ends_with("r::tally"))
            .unwrap_or_else(|| panic!("{tag}: tally not found"));
        let raw = img.raw_name_at(f.start).unwrap_or_else(|| panic!("{tag}: no raw name"));
        assert!(raw.starts_with(if tag == "v0" { "_R" } else { "_ZN" }), "{tag}: {raw}");
        assert!(!f.name.contains("17h"), "the hash is dropped: {}", f.name);
        let t = fn_text(&img, raw);
        assert!(t.starts_with("fn "), "{t}");
        assert!(t.contains("r::tally"), "{t}");
    }
}

#[test]
fn msvc_names_are_demangled() {
    let d = crate::image::demangle("?area@Shape@geo@@QEBAHH@Z");
    assert!(d.contains("geo::Shape::area") && d.contains("int"), "{d}");
    // a name that is not valid MSVC stays as written
    assert_eq!(crate::image::demangle("?broken"), "?broken");
}

#[test]
fn a_macho_underscore_before_an_itanium_name_is_dropped() {
    assert_eq!(crate::image::demangle("__ZN3foo3barEv"), "foo::bar()");
}
