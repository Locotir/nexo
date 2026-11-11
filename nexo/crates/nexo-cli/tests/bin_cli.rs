//! `nexo bin` run through the binary: output, JSON and exit codes.

use std::path::Path;
use std::process::{Command, Output};

use nexo_bin::fixtures;

fn nexo(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .arg("bin")
        .args(args)
        .current_dir(dir)
        .env("NEXO_CONFIG_DIR", dir)
        .env("NEXO_DATA_DIR", dir.join("data"))
        .env("NEXO_CACHE_DIR", dir.join("cache"))
        .output()
        .expect("nexo runs")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn each_op_prints_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = dir.path().join("prog");
    std::fs::write(&path, &f.bytes).unwrap();
    let p = path.to_str().unwrap();
    let table = format!("{:#x}", f.table);
    let entry = format!("{:#x}", f.a);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["info", p], "ELF64 little-endian, x86-64"),
        (vec!["fn", p, &entry], "call 0x400110  ; helper"),
        (vec!["bytes", p, &table, "16", "--as", "u32"], "0x0000000a 0x00000014"),
        (vec!["strings", p, "--grep", "hello"], "\"hello nexo bin\""),
        (vec!["find", p, "48 8d 3d"], ".text    helper"),
        (vec!["find", p, "--imm", "1337"], "mov eax"),
    ];
    for (args, want) in cases {
        let o = nexo(dir.path(), &args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        assert!(stdout(&o).contains(want), "{args:?}: {}", stdout(&o));
    }
    let o = nexo(dir.path(), &["info", p, "--json"]);
    let j: serde_json::Value = serde_json::from_str(&stdout(&o)).expect("JSON");
    assert_eq!(j["format"], "ELF64");
    assert_eq!(j["counts"]["functions"], 3);
}

#[test]
fn a_file_it_does_not_read_exits_2_with_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, "not a binary\n").unwrap();
    let o = nexo(dir.path(), &["info", txt.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("nexo bin: this is a text file"), "{err}");
    assert!(o.stdout.is_empty());
}

#[test]
fn a_question_without_an_answer_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prog");
    std::fs::write(&path, fixtures::pe64(false).bytes).unwrap();
    let o = nexo(dir.path(), &["fn", path.to_str().unwrap(), "no_such_function"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("no function is named"));
}
