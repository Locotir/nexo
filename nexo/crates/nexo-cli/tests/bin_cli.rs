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

fn xref_file(dir: &Path) -> (String, fixtures::Fixture) {
    let f = fixtures::elf64_xref();
    let path = dir.join("prog");
    std::fs::write(&path, &f.bytes).unwrap();
    (path.to_str().unwrap().to_string(), f)
}

#[test]
fn the_reference_ops_print_and_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    let (p, f) = xref_file(dir.path());
    let b = format!("{:#x}", f.b);
    let g = format!("{:#x}", f.global.unwrap());
    let asked = format!("{:#x}", f.e.unwrap() + 10);
    let end = format!("{:#x}", f.f.unwrap() + 14);
    let a = format!("{:#x}", f.a);
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["xref", &p, &b], "; 3 references"),
        (vec!["xref", &p, &g], "writes (1)"),
        (vec!["xref", &p, "--str", "nexo"], "1 strings, 2 references"),
        (vec!["funcs", &p, "--sort", "addr"], "main"),
        (vec!["funcs", &p, "--grep", "^main$", "--min-size", "1"], "main"),
        (vec!["calls", &p, "helper"], "2 callers (2 call sites), 0 callees"),
        (vec!["strings", &p, "--sort", "refs"], "     2  \"hello nexo bin\""),
        (vec!["fn", &p, &b], "; refs 2"),
        (vec!["fn", &p, &a, "--to", &end], "fn sub_400130"),
        (vec!["fn", &p, &asked], "> 0x0040013a"),
        (vec!["names", &p], "0 names, 0 notes"),
    ];
    for (args, want) in cases {
        let o = nexo(dir.path(), &args);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        assert!(stdout(&o).contains(want), "{args:?}: {}", stdout(&o));
    }
    // --timing goes to stderr and leaves the answer alone
    let o = nexo(dir.path(), &["xref", &p, &b, "--timing"]);
    assert!(o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.starts_with("timing: references built in ") && err.lines().count() == 1, "{err}");
    assert!(!stdout(&o).contains("timing"));
}

#[test]
fn usage_problems_exit_2_and_unanswerable_questions_exit_1() {
    let dir = tempfile::tempdir().unwrap();
    let (p, f) = xref_file(dir.path());
    let (b, a) = (format!("{:#x}", f.b), format!("{:#x}", f.a));
    let o = nexo(dir.path(), &["fn", &p, &b, "--to", &a]);
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stderr).lines().count(), 1);
    assert!(o.stdout.is_empty());
    let o = nexo(dir.path(), &["xref", &p, "no_such_name"]);
    assert_eq!(o.status.code(), Some(1));
    let o = nexo(dir.path(), &["name", &p, &b, "has space"]);
    assert_eq!(o.status.code(), Some(1));
    // the parser's own refusal keeps its exit code
    let o = nexo(dir.path(), &["funcs", &p, "--sort", "nope"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn names_stay_between_processes_and_sixteen_at_once_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (p, f) = xref_file(dir.path());
    let b = format!("{:#x}", f.b);
    let o = nexo(dir.path(), &["name", &p, &b, "load_banner"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = nexo(dir.path(), &["fn", &p, "load_banner"]);
    assert!(stdout(&o).starts_with("fn load_banner "), "{}", stdout(&o));
    // 16 processes at once, each with its own address
    let base = f.string - 16;
    let handles: Vec<_> = (0..16u64)
        .map(|i| {
            let (dir, p) = (dir.path().to_path_buf(), p.clone());
            std::thread::spawn(move || {
                let at = format!("{:#x}", base + i);
                if i % 2 == 0 {
                    nexo(&dir, &["name", &p, &at, &format!("writer_{i}")])
                } else {
                    nexo(&dir, &["note", &p, &at, &format!("note from {i}")])
                }
            })
        })
        .collect();
    for h in handles {
        let o = h.join().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let o = nexo(dir.path(), &["names", &p, "--lines", "100"]);
    assert!(stdout(&o).contains("9 names, 8 notes"), "{}", stdout(&o));
    // a file nexo cannot read is named on the last line, the answer is still given, and nothing is overwritten
    let sha = std::fs::read_dir(dir.path().join("data").join("bin"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    std::fs::write(&sha, "{\"names\":").unwrap();
    let o = nexo(dir.path(), &["fn", &p, &b]);
    assert!(o.status.success());
    assert!(stdout(&o).lines().last().unwrap().starts_with("names file not read: "), "{}", stdout(&o));
    let o = nexo(dir.path(), &["name", &p, &b, "fresh_name"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&sha).unwrap(), "{\"names\":");
}
