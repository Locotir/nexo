#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The debugger on a C program built with `-g`: breakpoints by file:line and by function (after the prologue).
//! Skipped when no C compiler is on this machine.

use std::path::{Path, PathBuf};

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = include_str!("fixtures/shapes.c");

/// The program built into a fresh directory, or `None` without `cc`.
fn build(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("shapes.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.join("shapes");
    let ok = std::process::Command::new("cc")
        .args(["-g", "-O0", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    (ok && exe.is_file()).then_some(exe)
}

fn line_of(mark: &str) -> usize {
    SRC.lines().position(|l| l.contains(mark)).unwrap() + 1
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn start(args: &[&str]) -> Option<(Session, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path()) else {
        eprintln!("no C compiler: skipped");
        return None;
    };
    let (s, _) = open(&Kind::Run { exe, args: args.iter().map(|a| a.to_string()).collect() }).expect("start");
    Some((s, dir))
}

#[test]
fn break_at_a_file_and_line_and_at_a_function_after_its_prologue() {
    let Some((mut s, _dir)) = start(&[]) else { return };
    run(&mut s, "stdin --close");
    let store = line_of("STORE");
    let o = run(&mut s, &format!("break shapes.c:{store}"));
    assert!(o.contains(&format!("shapes.c:{store}")), "{o}");
    let o = run(&mut s, "break plot");
    // the prologue is past: the stop is on the first line of the body, not the line of `int plot(...) {`
    assert!(o.contains(&format!("shapes.c:{}", line_of("int idx"))), "{o}");
    let o = run(&mut s, "cont");
    assert!(o.contains(&format!("shapes.c:{}", line_of("int idx"))), "{o}");
    let o = run(&mut s, "cont");
    assert!(o.contains(&format!("shapes.c:{store}")), "{o}");
    let e = s.exec(&split_words("break nosuch.c:3")).unwrap_err().to_string();
    assert!(e.contains("shapes.c"), "{e}");
    let e = s.exec(&split_words("break plot; cont")).unwrap_err().to_string();
    assert!(e.contains("do \"a; b\""), "{e}");
    // `*plot` is the entry itself, before the prologue
    let o = run(&mut s, "break *plot");
    assert!(o.contains(&format!("shapes.c:{}", line_of("int plot("))), "{o}");
}

#[test]
fn locals_print_and_a_crash_speak_in_the_programs_own_names() {
    let Some((mut s, _dir)) = start(&["crash"]) else { return };
    run(&mut s, "stdin \"3\"");
    run(&mut s, "stdin --close");
    run(&mut s, &format!("break shapes.c:{}", line_of("STORE")));
    let o = run(&mut s, "cont");
    // the stop shows the line's own text and the locals in one line
    assert!(o.contains("rows[p->y][p->x] = c;"), "{o}");
    assert!(o.contains("locals: p = ") && o.contains("c = 7") && o.contains("idx = 11"), "{o}");
    let o = run(&mut s, "args");
    assert!(o.contains("p (struct pt *) = 0x") && o.contains("{x = 3, y = 2, tag = \"ab\"}"), "{o}");
    assert!(o.contains("c (int) = 7"), "{o}");
    let o = run(&mut s, "print p->tag");
    assert!(o.contains("p->tag (char [8]") && o.contains("= \"ab\""), "{o}");
    let o = run(&mut s, "print p->y");
    assert!(o.contains("= 2"), "{o}");
    let o = run(&mut s, "print greeting");
    assert!(o.contains("\"hello grid\""), "{o}");
    let o = run(&mut s, "print rows[p->y][1]");
    assert!(o.contains("(int at 0x") && o.contains("= 0"), "{o}");
    let o = run(&mut s, "print rows[5][0]");
    assert!(o.contains("outside the array (4 elements)"), "{o}");
    let o = run(&mut s, "print *p");
    assert!(o.contains("x = 3"), "{o}");
    let o = run(&mut s, "locals");
    assert!(o.contains("in plot:") && o.contains("arg c (int) = 7") && o.contains("idx (int) = 11"), "{o}");
    let e = s.exec(&split_words("print p->nope")).unwrap_err().to_string();
    assert!(e.contains("its fields: x, y, tag"), "{e}");
    // registers still print as before
    let o = run(&mut s, "print rsp");
    assert!(o.contains("rsp = 0x"), "{o}");
    // on to the crash: the cause line comes with main's locals
    run(&mut s, "delete all");
    let o = run(&mut s, "cont");
    assert!(o.contains("SIGSEGV"), "{o}");
    assert!(o.contains("*bad = got;") && o.contains("bad = NULL") && o.contains("got = 11"), "{o}");
}
