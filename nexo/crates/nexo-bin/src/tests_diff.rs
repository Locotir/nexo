//! Tests of `diff`: two builds of a small C program the test compiles itself (skipped, with a message,
//! when there is no C compiler). The second build adds a bounds check to one function, adds a function
//! and links the files in another order, so everything else moves to another address.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::tests_map::whole;
use crate::*;

const ITEMS_V1: &str = r#"
int table[8] = {3, 1, 4, 1, 5, 9, 2, 6};
__attribute__((noinline)) int get_item(int i) { return table[i] * 2 + 1; }
"#;

const ITEMS_V2: &str = r#"
int table[8] = {3, 1, 4, 1, 5, 9, 2, 6};
__attribute__((noinline)) int added_fn(int x) { return x * 5 + table[1]; }
__attribute__((noinline)) int get_item(int i) {
    if (i < 0 || i >= 8) return added_fn(i);
    return table[i] * 2 + 1;
}
"#;

const SUMS: &str = r#"
#include <stdio.h>
int bias = 7;
__attribute__((noinline)) int weight(int i) { return i * 3 + bias; }
__attribute__((noinline)) int sum_to(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) s += weight(i);
    puts("sum");
    return s;
}
"#;

const MAIN: &str = r#"
#include <stdio.h>
#include <stdlib.h>
int get_item(int i);
int sum_to(int n);
int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 3;
    printf("%d %d\n", get_item(n), sum_to(n));
    return 0;
}
"#;

/// Compile the files, in this order, into `dir/name`; None (and a message) when no compiler builds them.
fn build(dir: &Path, name: &str, files: &[(&str, &str)], flags: &[&str]) -> Option<PathBuf> {
    let src = dir.join(format!("{name}.src"));
    std::fs::create_dir_all(&src).ok()?;
    let mut paths = Vec::new();
    for (file, text) in files {
        let p = src.join(file);
        std::fs::write(&p, text).ok()?;
        paths.push(p);
    }
    let exe = dir.join(name);
    let ok = ["cc", "gcc", "clang"].iter().any(|cc| {
        Command::new(cc)
            .args(flags)
            .arg("-o")
            .arg(&exe)
            .args(&paths)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    if !ok || !exe.is_file() {
        eprintln!("skipped: no C compiler that builds the sample");
        return None;
    }
    Some(exe)
}

/// The old and the new build: v2 links the files the other way round.
fn builds(dir: &Path, tag: &str, flags: &[&str]) -> Option<(PathBuf, PathBuf)> {
    let v1 = build(dir, &format!("v1{tag}"), &[("main.c", MAIN), ("items.c", ITEMS_V1), ("sums.c", SUMS)], flags)?;
    let v2 = build(dir, &format!("v2{tag}"), &[("sums.c", SUMS), ("items.c", ITEMS_V2), ("main.c", MAIN)], flags)?;
    Some((v1, v2))
}

fn addr(img: &Image, name: &str) -> u64 {
    img.symbols.iter().find(|s| s.name == name && s.func).unwrap_or_else(|| panic!("symbol {name}")).addr
}

fn items(o: &Output) -> Vec<Value> {
    o.page(None, usize::MAX / 2).value["items"].as_array().cloned().unwrap_or_default()
}

fn with(rows: &[Value], status: &str) -> Vec<Value> {
    rows.iter().filter(|r| r["status"] == status).cloned().collect()
}

/// `a` and `b` are diffed; the addresses come from the unstripped builds `ra` and `rb`, which have the
/// same layout as the stripped ones.
fn check(a: &Image, b: &Image, ra: &Image, rb: &Image, what: &str) {
    let o = diff_window(a, b, None, None, false, None, DEFAULT_LINES).unwrap();
    let t = whole(&o);
    let rows = items(&o);
    let changed = with(&rows, "changed");
    assert_eq!(changed.len(), 1, "{what}: exactly one function changed\n{t}");
    assert_eq!(changed[0]["a_addr"], addr(ra, "get_item"), "{what}\n{t}");
    assert_eq!(changed[0]["b_addr"], addr(rb, "get_item"), "{what}\n{t}");
    assert!(changed[0]["delta"].as_i64().unwrap() > 0, "the bounds check makes it larger\n{t}");
    let added = with(&rows, "added");
    assert_eq!(added.len(), 1, "{what}: one function added\n{t}");
    assert_eq!(added[0]["b_addr"], addr(rb, "added_fn"), "{what}\n{t}");
    assert!(with(&rows, "removed").is_empty(), "{what}: nothing removed\n{t}");
    assert!(with(&rows, "same").is_empty(), "{what}: same ones only with --all\n{t}");
    assert_eq!(o.meta["changed"], 1, "{what}\n{t}");
    assert!(t.contains("1 changed, 1 added, 0 removed"), "{what}\n{t}");
    assert!(t.contains("next: nexo bin diff "), "{what}: the next step is one command\n{t}");

    // every other function moved and is the same
    let o = diff_window(a, b, None, None, true, None, DEFAULT_LINES * 8).unwrap();
    let t = whole(&o);
    let rows = items(&o);
    for name in ["sum_to", "weight", "main"] {
        let (x, y) = (addr(ra, name), addr(rb, name));
        assert_ne!(x, y, "{what}: the link order moves {name}");
        let row = rows.iter().find(|r| r["a_addr"] == x).unwrap_or_else(|| panic!("{what}: {name} listed\n{t}"));
        assert_eq!(row["status"], "same", "{what}: {name} only moved\n{t}");
        assert_eq!(row["b_addr"], y, "{what}: {name} paired with itself\n{t}");
    }
    assert_eq!(with(&rows, "changed").len(), 1, "{what}\n{t}");
}

#[test]
fn diff_names_the_changed_function_the_added_one_and_calls_the_moved_ones_the_same() {
    let dir = tempfile::tempdir().unwrap();
    for (tag, flags) in [
        ("pie", &["-O1", "-fno-inline", "-fPIE", "-pie"][..]),
        ("fixed", &["-O1", "-fno-inline", "-fno-pie", "-no-pie"][..]),
    ] {
        let Some((v1, v2)) = builds(dir.path(), tag, flags) else { return };
        let strip: Vec<&str> = flags.iter().copied().chain(["-s"]).collect();
        let Some((s1, s2)) = builds(dir.path(), &format!("{tag}-s"), &strip) else { return };
        let (a, b) = (Image::open(&v1).unwrap(), Image::open(&v2).unwrap());
        let (sa, sb) = (Image::open(&s1).unwrap(), Image::open(&s2).unwrap());
        check(&a, &b, &a, &b, &format!("{tag} with symbols"));
        check(&sa, &sb, &a, &b, &format!("{tag} stripped"));

        // the same file twice: nothing differs
        let o = diff_window(&a, &a, None, None, false, None, DEFAULT_LINES).unwrap();
        let t = whole(&o);
        assert!(t.contains("0 changed, 0 added, 0 removed") && t.contains("no function differs"), "{t}");
        assert!(t.contains("the two files are the same bytes"), "{t}");
    }
}

#[test]
fn diff_of_one_function_shows_the_new_instructions_with_both_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let flags = ["-O1", "-fno-inline", "-fno-pie", "-no-pie"];
    let Some((v1, v2)) = builds(dir.path(), "one", &flags) else { return };
    let (a, b) = (Image::open(&v1).unwrap(), Image::open(&v2).unwrap());
    let (ga, gb) = (addr(&a, "get_item"), addr(&b, "get_item"));
    let o = diff_window(&a, &b, Some("get_item"), None, false, None, DEFAULT_LINES).unwrap();
    let t = whole(&o);
    assert!(t.starts_with("diff get_item: a "), "{t}");
    assert!(t.contains(&format!("{ga:#010x}")) && t.contains(&format!("{gb:#010x}")), "both addresses\n{t}");
    assert!(t.lines().any(|l| l.starts_with("+ ") && l.contains("added_fn")), "the new call is a + line\n{t}");
    assert!(t.lines().any(|l| l.starts_with("= ")), "the kept instructions are = lines\n{t}");
    assert!(!t.lines().any(|l| l.starts_with("- ")), "nothing of the old body is lost\n{t}");
    assert!(t.contains(&format!("nexo bin fn {} {ga:#x}", v1.display())), "{t}");
    assert_eq!(o.meta["status"], "changed");
    // the same function asked by its address in b
    let by_b = diff_window(&a, &b, Some(&format!("b:{gb:#x}")), None, false, None, DEFAULT_LINES).unwrap();
    assert_eq!(by_b.meta["a_addr"], ga, "{}", whole(&by_b));

    // a function that only moved: one line says so
    let o = diff_window(&a, &b, Some("sum_to"), None, false, None, DEFAULT_LINES).unwrap();
    let t = whole(&o);
    assert_eq!(o.meta["status"], "same", "{t}");
    assert!(t.contains("same: ") && t.contains("--all lists them"), "{t}");
    let o = diff_window(&a, &b, Some("sum_to"), None, true, None, DEFAULT_LINES).unwrap();
    let t = whole(&o);
    assert!(t.lines().filter(|l| l.starts_with("= ")).count() > 3 && !t.contains("\n- ") && !t.contains("\n+ "), "{t}");

    // the added one is only in b, with the next command
    let o = diff_window(&a, &b, Some("added_fn"), None, false, None, DEFAULT_LINES).unwrap();
    let t = whole(&o);
    assert!(t.contains("is only in b") && t.contains("(added)") && t.contains("nexo bin fn "), "{t}");

    // a name in neither and an address in neither say so
    let e = diff_window(&a, &b, Some("no_such_fn"), None, false, None, DEFAULT_LINES).unwrap_err();
    assert!(e.to_string().contains("b has no such function either"), "{e}");
    let e = diff_window(&a, &b, Some("0x10"), None, false, None, DEFAULT_LINES).unwrap_err();
    assert!(e.to_string().contains("b:0x10"), "{e}");
}

#[test]
fn diff_filters_and_cuts_its_list_like_the_other_ops() {
    let dir = tempfile::tempdir().unwrap();
    let flags = ["-O1", "-fno-inline", "-fno-pie", "-no-pie"];
    let Some((v1, v2)) = builds(dir.path(), "list", &flags) else { return };
    let (a, b) = (Image::open(&v1).unwrap(), Image::open(&v2).unwrap());
    let o = diff_window(&a, &b, None, Some("^sum"), true, None, DEFAULT_LINES).unwrap();
    let rows = items(&o);
    assert_eq!(rows.len(), 1, "{}", whole(&o));
    assert_eq!(rows[0]["name_a"], "sum_to");
    assert!(o.command.ends_with(" --grep '^sum' --all"), "{}", o.command);
    let o = diff_window(&a, &b, None, Some("nothing_like_this"), false, None, DEFAULT_LINES).unwrap();
    assert!(whole(&o).contains("no function matches"), "{}", whole(&o));

    // the list with every function, cut to ten lines, says how to get the rest
    let o = diff_window(&a, &b, None, None, true, None, 10).unwrap();
    let p = o.page(None, 10);
    assert_eq!(p.lines.len(), 10);
    let last = p.lines.last().unwrap();
    assert!(last.contains("more lines: nexo bin diff ") && last.contains("--all --from 0x9 --lines 10"), "{last}");

    let e = diff_window(&a, &b, None, Some("(("), false, None, DEFAULT_LINES).unwrap_err();
    assert!(e.to_string().contains("bad regular expression"), "{e}");
    let e = diff_window(&a, &b, Some("get_item"), Some("x"), false, None, DEFAULT_LINES).unwrap_err();
    assert!(matches!(e, Error::Usage(_)), "{e}");
}
