//! Every `nexo bin` op answers `--json` the same way: an object with `items`, one flat object per data row.
//! (The rows are called `items` in the JSON; headings and cut lines are not rows there.)

use super::*;
use clap::{CommandFactory, Parser};
use nexo_bin::fixtures;
use serde_json::Value;

/// Ops this test does not run, by name, and why. Every other op of `nexo bin` must be in `cases`: a new op that is
/// in neither list fails `every_op_is_run_or_skipped_by_name`. None of these needs a live process or a network;
/// those ops are in `nexo debug`, not here.
const SKIPPED: &[(&str, &str)] = &[
    ("sigs", "reads the system libc.a and writes a signature file; its own test in tests.rs"),
    ("managed", "needs a .NET or Java file; its own tests in bin_managed.rs"),
];

/// Ops whose fixture answer may be a refusal (exit code 1: the question has no answer on the small fixture);
/// the refusal is reported and the case passes.
const MAY_REFUSE: &[&str] = &["slice", "emu", "proto", "struct", "var"];

struct Case {
    op: &'static str,
    args: Vec<String>,
    /// Keys one item must have all of. Empty: the op only has to answer JSON with an `items` array.
    keys: &'static [&'static str],
}

fn ask_in(data: &Path, args: &[String]) -> Result<String, Error> {
    let mut argv = vec!["nexo".to_string(), "bin".to_string()];
    argv.extend(args.iter().cloned());
    let parsed = crate::cli::Args::try_parse_from(argv).expect("the arguments parse");
    match parsed.cmd {
        Some(crate::cli::Cmd::Bin(b)) => execute_in(&b, data),
        other => panic!("not a bin command: {other:?}"),
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn cases(dir: &Path) -> Vec<Case> {
    let f = fixtures::elf64_xref();
    let p = dir.join("elf");
    std::fs::write(&p, &f.bytes).unwrap();
    let q = dir.join("elf2");
    std::fs::write(&q, &f.bytes).unwrap();
    let out = dir.join("patched").display().to_string();
    let (p, q) = (p.display().to_string(), q.display().to_string());
    let (a, b, s) = (format!("{:#x}", f.a), format!("{:#x}", f.b), format!("{:#x}", f.string));
    let case = |op, parts: &[&str], keys| Case { op, args: argv(parts), keys };
    vec![
        case("asm", &["asm", "nop; ret"], &["text"]),
        case("patch", &["patch", &p, "--out", &out, "--at", &b, "--bytes", "90"], &["text"]),
        case("info", &["info", &p], &["addr", "size", "name"]),
        case("syms", &["syms", &p], &["kind"]),
        case("overview", &["overview", &p], &["kind", "text"]),
        case("crypto", &["crypto", &p], &[]),
        case("slice", &["slice", &p, &b, "--reg", "rax"], &[]),
        case("path", &["path", &p, &a, &b], &["addr", "name"]),
        case("tree", &["tree", &p, &a], &["addr", "name"]),
        case("fn", &["fn", &p, &b], &["addr", "bytes", "text"]),
        case("decomp", &["decomp", &p, &b], &["addr", "text"]),
        case("proto", &["proto", &p, &b, "int f(int x)"], &[]),
        case("struct", &["struct", &p, "pair", "int a; int b"], &[]),
        case("var", &["var", &p, &b, "v1", "count"], &[]),
        case("xref", &["xref", &p, &b], &["addr", "kind"]),
        case("funcs", &["funcs", &p], &["addr", "size", "name"]),
        case("calls", &["calls", &p, &a], &["addr", "name"]),
        case("cfg", &["cfg", &p, &a], &["row"]),
        // name and note come before names, which lists them
        case("name", &["name", &p, &b, "named_b"], &[]),
        case("note", &["note", &p, &b, "a", "short", "note"], &[]),
        case("names", &["names", &p], &["addr", "name"]),
        case("emu", &["emu", &p, &b], &[]),
        case("bytes", &["bytes", &p, &s, "8"], &["addr", "values"]),
        case("strings", &["strings", &p], &["addr", "text"]),
        case("find", &["find", &p, "--str", "hello"], &["addr", "section", "kind"]),
        case("diff", &["diff", &p, &q, "--all"], &["status"]),
    ]
}

#[test]
fn every_op_is_run_or_skipped_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let have: Vec<&str> = cases(dir.path()).iter().map(|c| c.op).collect();
    let mut root = crate::cli::Args::command();
    let bin = root.find_subcommand_mut("bin").expect("bin is a command");
    for op in bin.get_subcommands().map(|c| c.get_name().to_string()) {
        let skipped = SKIPPED.iter().any(|(n, _)| *n == op);
        assert!(op == "help" || skipped || have.contains(&op.as_str()), "{op} is neither run nor skipped by name");
        assert!(!(skipped && have.contains(&op.as_str())), "{op} is both run and skipped");
    }
}

#[test]
fn every_op_answers_json_rows_with_an_object_each() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    for c in cases(dir.path()) {
        let mut args = c.args.clone();
        args.push("--json".into());
        let text = match ask_in(data.path(), &args) {
            Ok(t) => t,
            Err(e) if MAY_REFUSE.contains(&c.op) && e.exit_code() == 1 => {
                eprintln!("{}: refused on the fixture: {e}", c.op);
                continue;
            }
            Err(e) => panic!("{}: {e}", c.op),
        };
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: not JSON ({e}): {text}", c.op));
        let items = v["items"].as_array().unwrap_or_else(|| panic!("{}: no items array: {text}", c.op));
        for it in items {
            let o = it.as_object().unwrap_or_else(|| panic!("{}: a row is not an object: {it}", c.op));
            assert!(!o.is_empty(), "{}: an empty row", c.op);
        }
        if !c.keys.is_empty() {
            assert!(
                items.iter().any(|it| c.keys.iter().all(|k| it.get(*k).is_some())),
                "{}: no row has all of {:?}: {text}",
                c.op,
                c.keys
            );
        }
    }
}

#[test]
fn fields_keep_only_the_named_keys_and_refuse_a_mistake() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let p = dir.path().join("elf");
    std::fs::write(&p, &fixtures::elf64_xref().bytes).unwrap();
    let p = p.display().to_string();
    let run = |extra: &[&str]| {
        let mut a = argv(&["funcs", &p]);
        a.extend(argv(extra));
        ask_in(data.path(), &a)
    };
    let v: Value = serde_json::from_str(&run(&["--json", "--fields", "addr, size"]).unwrap()).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for it in items {
        let mut keys: Vec<&str> = it.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["addr", "size"], "{it}");
        assert!(it["addr"].is_u64() && it["size"].is_u64(), "{it}");
    }
    assert_eq!(v["op"], "funcs");
    let e = run(&["--json", "--fields", "addr,nope"]).unwrap_err().to_string();
    assert!(e.contains("\"nope\"") && e.contains("addr") && e.contains("callers") && e.contains("name"), "{e}");
    let e = run(&["--fields", "addr"]).unwrap_err();
    assert!(e.to_string().contains("needs --json") && e.exit_code() == 2, "{e}");
    // names are plain words, separated by commas
    for bad in ["addr,,size", "", "a b", "addr;size"] {
        let parsed = crate::cli::Args::try_parse_from(["nexo", "bin", "funcs", &p, "--json", "--fields", bad]);
        assert!(parsed.is_err(), "{bad:?} parsed");
    }
    // the same call without --fields is whole
    let whole: Value = serde_json::from_str(&run(&["--json"]).unwrap()).unwrap();
    assert!(whole["items"][0].as_object().unwrap().len() > 2);
}
