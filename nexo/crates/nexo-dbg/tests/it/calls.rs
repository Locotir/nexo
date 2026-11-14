#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `calls`: the real target of each indirect call, named, read from the run. The program is a small C file
//! that calls through a function-pointer table; skipped when there is no C compiler.

use nexo_dbg::{open, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn build(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let cc = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))?;
    let src = dir.join("table.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n\
         __attribute__((noinline)) int add_one(int x) { return x + 1; }\n\
         __attribute__((noinline)) int double_it(int x) { return x * 2; }\n\
         int (*volatile table[2])(int) = { add_one, double_it };\n\
         int main(void) {\n    int v = 1;\n    for (int i = 0; i < 4; i++) v = table[i & 1](v);\n    printf(\"%d\\n\", v);\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.join("table");
    let ok = std::process::Command::new(cc)
        .args(["-g", "-O0", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .ok()?
        .success();
    ok.then_some(exe)
}

#[test]
fn the_real_target_of_each_call_through_a_pointer_is_logged_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path()) else {
        eprintln!("no C compiler: skipped");
        return;
    };
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let on = run(&mut s, "calls");
    // the program's own call, and the start-up code's call to the C library's start
    assert!(on.contains("logging 2 indirect calls"), "{on}");
    assert!(run(&mut s, "breaks").contains("indirect-call logging on 2 sites"));
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let log = run(&mut s, "log");
    assert!(log.contains("-> 0x") && log.contains("libc_start_main") || log.contains("__libc_start"), "{log}");
    let seen: Vec<&str> = log.lines().filter(|l| l.contains("main+")).collect();
    assert_eq!(seen.len(), 4, "{log}");
    let names: Vec<&str> = seen
        .iter()
        .map(|l| {
            if l.contains("add_one") {
                "add_one"
            } else if l.contains("double_it") {
                "double_it"
            } else {
                "?"
            }
        })
        .collect();
    assert_eq!(names, ["add_one", "double_it", "add_one", "double_it"], "{log}");
    assert!(run(&mut s, "calls off").contains("off"));
}

#[test]
fn the_call_chain_goes_through_the_c_library_up_to_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path()) else {
        eprintln!("no C compiler: skipped");
        return;
    };
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break add_one");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("backtrace:") || stop.contains("breakpoint"), "{stop}");
    let bt = run(&mut s, "bt");
    eprintln!("{bt}");
    assert!(bt.contains("add_one") && bt.contains("main"), "{bt}");
    assert!(bt.contains("table.c:"), "{bt}");
    assert!(bt.contains("libc_start"), "the chain reaches the C library's start: {bt}");
}
