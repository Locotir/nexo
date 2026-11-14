#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `run <exe> <<< "A\x00\xff\n"` carries raw bytes (a NUL, a byte above 0x7f) to the program's input.
//! Skipped without `cc`.

use nexo_dbg::{execute, parse, Host};

const SRC: &str = r#"
#include <stdio.h>
#include <unistd.h>
int main(void) {
    unsigned char b[64];
    ssize_t n = read(0, b, sizeof b);
    printf("got %zd:", n);
    for (ssize_t i = 0; i < n; i++) printf(" %02x", b[i]);
    printf("\n");
    return 0;
}
"#;

fn build(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let src = dir.join("rd.c");
    std::fs::write(&src, SRC).unwrap();
    let exe = dir.join("rd");
    let ok =
        std::process::Command::new("cc").arg("-o").arg(&exe).arg(&src).status().map(|s| s.success()).unwrap_or(false);
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

fn printed(words: &[&str]) -> Option<String> {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let exe = build(work.path())?;
    let exe = exe.to_str().unwrap();
    let mut w = vec!["run", exe];
    w.extend_from_slice(words);
    call(state.path(), &w).unwrap();
    call(state.path(), &["cont", "--wait", "20"]).unwrap();
    Some(call(state.path(), &["out", "--all"]).unwrap())
}

#[test]
fn nul_and_high_bytes_reach_the_program() {
    let Some(out) = printed(&["<<<", "A\\x00\\xff\\n"]) else { return };
    assert!(out.contains("got 5: 41 00 ff 0a 0a"), "{out}");
}

#[test]
fn backslash_quote_and_zero_escapes() {
    let Some(out) = printed(&["<<<", "\\\\\\\"\\0\\x80"]) else { return };
    assert!(out.contains("got 5: 5c 22 00 80 0a"), "{out}");
}

#[test]
fn a_file_with_raw_bytes_is_sent_as_is() {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), [0u8, 0xfe, 0x80, b'z']).unwrap();
    let Some(out) = printed(&["<", f.path().to_str().unwrap()]) else { return };
    assert!(out.contains("got 4: 00 fe 80 7a"), "{out}");
}

#[test]
fn a_bad_hex_escape_is_refused_before_the_program_starts() {
    let state = tempfile::tempdir().unwrap();
    let Err(e) = call(state.path(), &["run", "./nothing", "<<<", "a\\xzz"]) else { panic!("must be refused") };
    assert!(e.to_string().contains("two hex digits"), "{e}");
}
