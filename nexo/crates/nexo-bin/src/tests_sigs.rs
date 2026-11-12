//! Library signatures: a statically linked, stripped C program gets its libc functions named.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sigs::{generate, Keep, SigSet, Strength};
use crate::Image;

const SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
__attribute__((noinline)) size_t my_strlen(const char *s) { size_t n = 0; while (s[n]) n++; return n; }
__attribute__((noinline)) void *my_memcpy(void *d, const void *s, size_t n) {
  char *o = d; const char *i = s; while (n--) *o++ = *i++; return d;
}
int main(int argc, char **argv) {
  char buf[256]; char *h = malloc(argc * 100 + 64);
  strcpy(buf, argv[0]);
  memcpy(h, buf, strlen(buf) + 1);
  memset(buf, argc, sizeof buf);
  printf("%s %zu %zu %d\n", h, my_strlen(h), (size_t)strlen(argv[argc - 1]), strcmp(h, argv[0]));
  my_memcpy(buf, h, 20);
  free(h);
  return buf[3];
}
"#;

fn libc_a() -> Option<PathBuf> {
    crate::sigs::find_libraries().into_iter().find(|p| p.file_name().is_some_and(|n| n == "libc.a"))
}

/// A stripped static program, or `None` (with a message) when this machine cannot build one.
fn build(dir: &Path, stripped: bool) -> Option<PathBuf> {
    let c = dir.join("a.c");
    std::fs::write(&c, SRC).unwrap();
    let exe = dir.join("a.out");
    let mut flags = vec!["-O1", "-static", "-fno-pie", "-no-pie"];
    if stripped {
        flags.push("-s");
    }
    let ok = ["cc", "gcc", "clang"].iter().any(|cc| {
        Command::new(cc).args(&flags).arg("-o").arg(&exe).arg(&c).output().map(|o| o.status.success()).unwrap_or(false)
    });
    if !ok || !exe.is_file() {
        eprintln!("skipped: no C compiler that links statically here (needs the static libc)");
        return None;
    }
    Some(exe)
}

fn named<'a>(img: &'a Image, name: &str) -> Option<&'a crate::funcs::Func> {
    img.funcs().iter().find(|f| f.name == name)
}

#[test]
fn a_stripped_static_program_gets_memcpy_and_strlen_named() {
    let Some(lib) = libc_a() else {
        eprintln!("skipped: no libc.a");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), true) else { return };
    let set = generate(&lib, Keep::Common).unwrap();
    assert!(set.len() > 20, "{} signatures", set.len());
    let mut img = Image::open(&exe).unwrap();
    assert!(img.symbols.is_empty(), "the sample is stripped");
    img.add_sigs(set);
    let exact: Vec<String> = img
        .funcs()
        .iter()
        .filter(|f| f.sig.as_ref().is_some_and(|h| h.strength == Strength::Exact))
        .map(|f| f.name.clone())
        .collect();
    println!("exact: {exact:?}");
    for f in img.funcs().iter().filter(|f| f.sig.as_ref().is_some_and(|h| h.strength != Strength::Exact)) {
        println!("{:#x} {}", f.start, img.sig_label(f.start).unwrap());
    }
    let has = |stem: &str| exact.iter().any(|n| n.trim_start_matches('_').starts_with(stem));
    assert!(has("memcpy") || has("memmove"), "{exact:?}");
    assert!(has("strlen"), "{exact:?}");
    assert!(has("memset"), "{exact:?}");
    // the label says it is a signature and not proof
    let f = img.funcs().iter().find(|f| f.name.contains("strlen")).unwrap();
    let label = img.sig_label(f.start).unwrap();
    assert!(label.starts_with("[signature exact") && label.contains("not proof"), "{label}");
    // the name reaches `fn` and `funcs`
    let out = crate::func_view(&img, &crate::FnQuery::new(&f.name)).unwrap().text_all();
    assert!(out.contains("[signature exact"), "{out}");
    let list = crate::func_list_window(&img, crate::FuncSort::parse("callers").unwrap(), Some("strlen"), 0, None, 50)
        .unwrap()
        .text_all();
    assert!(list.contains("[signature exact"), "{list}");
}

#[test]
fn lookalike_functions_of_the_program_are_not_named() {
    let Some(lib) = libc_a() else {
        eprintln!("skipped: no libc.a");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), false) else { return };
    let set = generate(&lib, Keep::All).unwrap();
    let mut img = Image::open(&exe).unwrap();
    img.add_sigs(set);
    for name in ["my_strlen", "my_memcpy", "main"] {
        let f = named(&img, name).unwrap_or_else(|| panic!("{name} is in the symbols"));
        assert!(f.sig.is_none(), "{name} got {:?}", f.sig);
    }
}

#[test]
fn the_shipped_set_is_not_empty_and_parses_back() {
    let b = crate::sigs::SigSet::builtin();
    assert!(!b.is_empty(), "the shipped signature set is empty");
    let again = crate::sigs::SigSet::parse(&b.to_text("test"));
    assert_eq!(again.len(), b.len());
}

#[test]
fn a_signature_file_round_trips() {
    let Some(lib) = libc_a() else {
        eprintln!("skipped: no libc.a");
        return;
    };
    let set = generate(&lib, Keep::Common).unwrap();
    let again = SigSet::parse(&set.to_text("test"));
    assert_eq!(set.len(), again.len());
}

#[test]
fn a_stripped_rust_program_gets_core_fmt_write_named() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("r.rs");
    std::fs::write(
        &src,
        "fn main() { let n = std::env::args().count(); let s = format!(\"{} args {:?}\", n, std::env::args().collect::<Vec<_>>()); println!(\"{s}\"); }\n",
    )
    .unwrap();
    let exe = dir.path().join("r.out");
    let ok = Command::new("rustc")
        .args(["-C", "opt-level=1", "-C", "strip=symbols", "-o"])
        .arg(&exe)
        .arg(&src)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok || !exe.is_file() {
        eprintln!("skipped: rustc is not here");
        return;
    }
    let libs: Vec<PathBuf> =
        crate::sigs::find_libraries().into_iter().filter(|p| p.extension().is_some_and(|x| x == "rlib")).collect();
    if libs.is_empty() {
        eprintln!("skipped: no Rust std rlibs found");
        return;
    }
    let mut set = SigSet::default();
    for l in &libs {
        set = set.merged(generate(l, Keep::Common).unwrap());
    }
    let mut img = Image::open(&exe).unwrap();
    img.add_sigs(set);
    let exact: Vec<&str> = img
        .funcs()
        .iter()
        .filter(|f| f.sig.as_ref().is_some_and(|h| h.strength == Strength::Exact))
        .map(|f| f.name.as_str())
        .collect();
    println!("{exact:?}");
    assert!(exact.contains(&"core::fmt::write"), "{exact:?}");
}
