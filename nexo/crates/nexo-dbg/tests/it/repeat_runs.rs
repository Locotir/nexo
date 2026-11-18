#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]
//! `run --repeat N --until crash|exit|output~TEXT`: the runs are made from scratch, the first one that matches stays
//! live, and a repeat with no match ends like an ordinary exit. Skipped without `cc`.

use nexo_dbg::{execute, parse, Host};

const SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    printf("hello\n");
    return argc > 1 ? atoi(argv[1]) : 0;
}
"#;

fn build(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let src = dir.join("rep.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.join("rep");
    let ok = std::process::Command::new("cc").arg("-o").arg(&exe).arg(&src).status().is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    Some(exe)
}

fn call(dir: &std::path::Path, words: &[&str]) -> Result<String, nexo_dbg::Error> {
    let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    execute(&parse(&w)?, dir, &Host::Thread, "nexo dbg")
}

fn repeated(code: &str, flags: &[&str]) -> Option<String> {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let exe = build(work.path())?;
    let mut w = vec!["run"];
    w.extend_from_slice(flags);
    w.extend([exe.to_str().unwrap(), code]);
    Some(call(state.path(), &w).unwrap())
}

#[test]
fn a_nonzero_exit_ends_the_repeat_on_the_first_run() {
    let Some(out) = repeated("3", &["--repeat", "4", "--until", "exit"]) else { return };
    assert!(out.contains("run 1 of 4 matched exit"), "{out}");
    assert!(out.contains("earlier runs: none"), "{out}");
    assert!(out.contains("exited with code 3"), "{out}");
}

#[test]
fn no_match_says_so_with_the_tally() {
    let Some(out) = repeated("0", &["--repeat", "3", "--until", "exit"]) else { return };
    assert!(out.contains("3 runs, none matched exit"), "{out}");
    assert!(out.contains("runs: 3 x exit 0"), "{out}");
}

#[test]
fn the_output_text_is_looked_for_in_every_run() {
    let Some(out) = repeated("0", &["--repeat", "2", "--until", "output~hell"]) else { return };
    assert!(out.contains("run 1 of 2 matched output~hell"), "{out}");
}

#[test]
fn a_cap_is_said() {
    let Some(out) = repeated("5", &["--repeat", "5000", "--until", "exit"]) else { return };
    assert!(out.contains("--repeat 5000 is capped to 1000 runs"), "{out}");
}
