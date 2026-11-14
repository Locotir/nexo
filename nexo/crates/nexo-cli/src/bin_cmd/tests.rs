use super::*;
use clap::{CommandFactory, Parser};
use nexo_bin::fixtures::{self, Fixture};

/// Parse `nexo bin ...` the way the binary does and answer it, with names kept in `data`.
fn ask_in(data: &Path, args: &[&str]) -> Result<String, Error> {
    let mut argv = vec!["nexo", "bin"];
    argv.extend_from_slice(args);
    let parsed = crate::cli::Args::try_parse_from(argv).expect("the arguments parse");
    match parsed.cmd {
        Some(crate::cli::Cmd::Bin(b)) => execute_in(&b, data),
        other => panic!("not a bin command: {other:?}"),
    }
}

/// The same with a data dir of its own that nothing else uses.
fn ask(args: &[&str]) -> Result<String, Error> {
    let data = tempfile::tempdir().unwrap();
    ask_in(data.path(), args)
}

fn file(dir: &tempfile::TempDir, name: &str, f: &Fixture) -> String {
    let p = dir.path().join(name);
    std::fs::write(&p, &f.bytes).unwrap();
    p.display().to_string()
}

#[test]
fn sigs_makes_a_file_that_names_library_functions_of_a_stripped_program() {
    let Some(libc) = nexo_bin::sig_libraries().into_iter().find(|p| p.file_name().is_some_and(|n| n == "libc.a"))
    else {
        eprintln!("skipped: no libc.a");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let c = dir.path().join("p.c");
    std::fs::write(&c, "#include <string.h>\n#include <stdio.h>\nint main(int n, char **v) { char b[64]; strcpy(b, v[0]); memset(b, n, 9); printf(\"%zu %s\\n\", strlen(v[n - 1]), b); return b[1]; }\n").unwrap();
    let exe = dir.path().join("p.out");
    let built = std::process::Command::new("cc")
        .args(["-O1", "-static", "-fno-pie", "-no-pie", "-s", "-o"])
        .arg(&exe)
        .arg(&c)
        .output()
        .is_ok_and(|o| o.status.success());
    if !built {
        eprintln!("skipped: cannot link a static program here");
        return;
    }
    let data = dir.path().join("data");
    let exe = exe.display().to_string();
    let made = ask_in(&data, &["sigs", &libc.display().to_string()]).unwrap();
    assert!(made.contains("signatures to") && data.join("sigs/local.sig").is_file(), "{made}");
    let t = ask_in(&data, &["funcs", &exe, "--grep", "strlen|memset", "--lines", "40"]).unwrap();
    assert!(t.contains("[signature exact") && t.contains("not proof"), "{t}");
}

#[test]
fn a_wasm_module_is_found_by_its_signature_and_answers_the_same_ops() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mod.bin");
    std::fs::write(&p, nexo_bin::wasm_fixture::module()).unwrap();
    let path = p.display().to_string();
    let t = ask(&["info", &path]).unwrap();
    assert!(t.contains("WebAssembly module") && t.contains("3 defined functions"), "{t}");
    let t = ask(&["fn", &path, "greet"]).unwrap();
    assert!(t.contains("string? \"hello wasm\"") && t.contains("env.log"), "{t}");
    let t = ask(&["funcs", &path, "--grep", "add", "--json"]).unwrap();
    assert!(t.contains("\"name\": \"demo::add\""), "{t}");
    let t = ask(&["strings", &path, "--grep", "second"]).unwrap();
    assert!(t.contains("second string") && t.contains("<- func[4]"), "{t}");
    let e = ask(&["decomp", &path, "greet"]).unwrap_err().to_string();
    assert!(e.contains("does not read WebAssembly modules yet; use one of: info, overview"), "{e}");
}

#[test]
fn bin_mcp_parses_the_same_change_gate_as_re_mcp() {
    for root in ["bin", "re"] {
        for allow in [false, true] {
            let mut argv = vec!["nexo", root, "--mcp"];
            if allow {
                argv.push("--allow-changes");
            }
            let parsed = crate::cli::Args::try_parse_from(argv).unwrap();
            match parsed.cmd {
                Some(crate::cli::Cmd::Bin(b)) => assert!(b.op.is_none() && b.mcp && b.allow_changes == allow),
                Some(crate::cli::Cmd::Re(r)) => assert!(r.mcp && r.allow_changes == allow),
                _ => panic!("wrong command"),
            }
        }
        assert!(crate::cli::Args::try_parse_from(["nexo", root, "--allow-changes"]).is_err());
    }
    assert!(crate::cli::Args::try_parse_from(["nexo", "bin", "--mcp", "asm", "nop"]).is_err());
}

#[test]
fn assembly_and_copy_patch_parse_exclusive_arguments() {
    assert!(ask(&["asm", "nop; ret", "--bits", "32"]).unwrap().contains("90 c3"));
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let input = file(&dir, "input", &f);
    let out = dir.path().join("output").display().to_string();
    let at = format!("{:#x}", f.b);
    let receipt = ask(&["patch", &input, "--out", &out, "--at", &at, "--bytes", "90 90", "--json"]).unwrap();
    let j: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(j["new"], "90 90");
    assert_eq!(j["file_offset"], Image::open(Path::new(&input)).unwrap().addr_to_offset(f.b).unwrap());
    for args in [
        vec!["patch", &input, "--out", &out, "--nop", "1"],
        vec!["patch", &input, "--out", &out, "--at", &at],
        vec!["patch", &input, "--out", &out, "--at", &at, "--fn", "main", "--nop", "1"],
        vec!["patch", &input, "--out", &out, "--at", &at, "--bytes", "90", "--nop", "1"],
        vec!["asm", "nop", "--bits", "16"],
    ] {
        let mut argv = vec!["nexo", "bin"];
        argv.extend(args);
        assert!(crate::cli::Args::try_parse_from(argv).is_err());
    }
}

#[test]
fn diff_takes_two_builds_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let (a, b) = (file(&dir, "old", &f), file(&dir, "new", &f));
    let t = ask(&["diff", &a, &b]).unwrap();
    assert!(t.contains("0 changed, 0 added, 0 removed, 3 same") && t.contains("no function differs"), "{t}");
    let t = ask(&["diff", &a, &b, "--all", "--json"]).unwrap();
    let j: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(j["op"], "diff");
    assert_eq!(j["items"].as_array().map(Vec::len), Some(3), "{t}");
    let t = ask(&["diff", &a, &b, &format!("{:#x}", f.b)]).unwrap();
    assert!(t.contains("matched by name; same once addresses are set aside"), "{t}");
    let e = ask(&["diff", &a, &dir.path().join("missing").display().to_string()]).unwrap_err();
    assert_eq!(e.exit_code(), 2, "{e}");
}

#[test]
fn info_fn_bytes_strings_and_find_answer_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    for (name, f) in [("elf", fixtures::elf64(false)), ("pe.exe", fixtures::pe64(false))] {
        let path = file(&dir, name, &f);
        let t = ask(&["info", &path]).unwrap();
        assert!(t.contains("x86-64") && t.contains("3 functions"), "{t}");
        let t = ask(&["fn", &path, &format!("{:#x}", f.b), "--bytes"]).unwrap();
        assert!(t.contains("48 8d 3d") && t.contains(fixtures::STRING), "{t}");
        let t = ask(&["bytes", &path, &format!("{:#x}", f.table), "16", "--as", "u32"]).unwrap();
        assert!(t.contains("0x12345678"), "{t}");
        let t = ask(&["strings", &path, "--grep", "nexo"]).unwrap();
        assert!(t.contains(fixtures::STRING), "{t}");
        let t = ask(&["find", &path, "48", "8d", "3d"]).unwrap();
        assert!(t.contains(&format!("{:#010x}", f.b)), "{t}");
        let t = ask(&["find", &path, "--imm", "0x539"]).unwrap();
        assert!(t.contains("mov eax"), "{t}");
    }
}

#[test]
fn overview_answers_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "elf", &fixtures::elf64_xref());
    let t = ask(&["overview", &path]).unwrap();
    assert!(t.contains("overview") && t.contains("next") && t.contains("nexo bin decomp"), "{t}");
    let j = ask(&["overview", &path, "--json"]).unwrap();
    assert!(j.contains("\"op\": \"overview\""), "{j}");
}

#[test]
fn xref_funcs_calls_and_strings_answer_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    for (name, f) in [("elf", fixtures::elf64_xref()), ("pe.exe", fixtures::pe64_xref())] {
        let path = file(&dir, name, &f);
        let b = format!("{:#x}", f.b);
        let t = ask(&["xref", &path, &b]).unwrap();
        assert!(t.contains("; 3 references") && t.contains("calls (2)") && t.contains("pointers in data (1)"), "{t}");
        let t = ask(&["xref", &path, &format!("{:#x}", f.global.unwrap())]).unwrap();
        assert!(t.contains("reads (1)") && t.contains("writes (1)"), "{t}");
        let t = ask(&["xref", &path, "--str", "NEXO", "--i"]).unwrap();
        assert!(t.contains("1 strings, 2 references") && t.contains("ignoring case"), "{t}");
        let t = ask(&["xref", &path, "--str", "NEXO"]).unwrap();
        assert!(t.contains("no string contains"), "{t}");
        let t = ask(&["funcs", &path, "--sort", "callers", "--lines", "10"]).unwrap();
        assert!(
            t.lines().nth(1).unwrap().contains(&format!("{b:>10}"))
                || t.contains(&format!("{:#010x}        16        2", f.b)),
            "{t}"
        );
        let t = ask(&["funcs", &path, "--sort", "size", "--min-size", "20", "--grep", "."]).unwrap();
        assert_eq!(t.lines().count(), 2, "{t}");
        let t = ask(&["calls", &path, &format!("{:#x}", f.a)]).unwrap();
        assert!(t.contains("0 callers (0 call sites), 2 callees"), "{t}");
        let t = ask(&["strings", &path, "--sort", "refs", "--grep", "nexo"]).unwrap();
        assert!(t.contains("     2  \"hello nexo bin\""), "{t}");
        let t = ask(&["fn", &path, &b]).unwrap();
        assert!(t.lines().next().unwrap().ends_with("; refs 2"), "{t}");
        // the options of the new ops reach the answer as JSON too
        let j: serde_json::Value = serde_json::from_str(&ask(&["xref", &path, &b, "--json"]).unwrap()).unwrap();
        assert_eq!(j["op"], "xref");
        assert_eq!(j["items"].as_array().unwrap().len(), 3);
        for bad in [vec!["xref", &path as &str], vec!["funcs", &path, "--sort", "nope"], vec!["xref", &path, "--i"]] {
            let mut argv = vec!["nexo", "bin"];
            argv.extend(bad);
            assert!(crate::cli::Args::try_parse_from(argv).is_err());
        }
        let e = ask(&["funcs", &path, "--grep", "("]).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{e}");
    }
}

#[test]
fn name_note_names_keep_what_they_are_given_between_calls() {
    let dir = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let f = fixtures::elf64_xref();
    let path = file(&dir, "elf", &f);
    let b = format!("{:#x}", f.b);
    let t = ask_in(data.path(), &["name", &path, &b, "load_banner"]).unwrap();
    assert!(t.contains("load_banner"), "{t}");
    ask_in(data.path(), &["note", &path, "load_banner", "prints", "the", "banner"]).unwrap();
    let t = ask_in(data.path(), &["names", &path]).unwrap();
    assert!(t.contains("1 names, 1 notes") && t.contains("load_banner") && t.contains("prints the banner"), "{t}");
    for args in [["fn", &path, "load_banner"], ["xref", &path, "load_banner"], ["calls", &path, "load_banner"]] {
        let t = ask_in(data.path(), &args).unwrap();
        assert!(t.contains("load_banner") && t.contains("; note: prints the banner"), "{args:?}: {t}");
    }
    let t = ask_in(data.path(), &["funcs", &path]).unwrap();
    assert!(t.contains("load_banner"), "{t}");
    // another data dir knows nothing of it
    let t = ask(&["names", &path]).unwrap();
    assert!(t.contains("0 names, 0 notes"), "{t}");
    // refused names exit 1 and leave the file alone
    let e = ask_in(data.path(), &["name", &path, &b, "has space"]).unwrap_err();
    assert_eq!(e.exit_code(), 1, "{e}");
    let t = ask_in(data.path(), &["name", &path, "--clear", &b]).unwrap();
    assert!(t.contains("cleared"), "{t}");
    let t = ask_in(data.path(), &["note", &path, &b, "--clear"]).unwrap();
    assert!(t.contains("cleared"), "{t}");
    assert!(ask_in(data.path(), &["names", &path]).unwrap().contains("0 names, 0 notes"));
    // a broken names file is told on the last line and the answer is still given
    let sha = Image::open(Path::new(&path)).unwrap().sha256.clone();
    let broken = nexo_bin::names_path(data.path(), &sha);
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "{\"names\": ").unwrap();
    let t = ask_in(data.path(), &["fn", &path, &b]).unwrap();
    assert!(t.starts_with("fn ") && t.lines().last().unwrap().starts_with("names file not read: "), "{t}");
    let j: serde_json::Value =
        serde_json::from_str(&ask_in(data.path(), &["fn", &path, &b, "--json"]).unwrap()).unwrap();
    assert!(j["warning"].as_str().unwrap().starts_with("names file not read: "), "{j}");
    let e = ask_in(data.path(), &["name", &path, &b, "fresh_name"]).unwrap_err();
    assert_eq!(e.exit_code(), 1, "{e}");
}

#[test]
fn fn_to_and_the_address_page_answer_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64_xref();
    let path = file(&dir, "elf", &f);
    let end = format!("{:#x}", f.f.unwrap() + 14);
    let t = ask(&["fn", &path, &format!("{:#x}", f.a), "--to", &end]).unwrap();
    assert_eq!(t.lines().filter(|l| l.starts_with("fn ")).count(), 6, "{t}");
    let t = ask(&["fn", &path, &format!("{:#x}", f.e.unwrap() + 10)]).unwrap();
    assert!(t.lines().any(|l| l.starts_with(&format!("> {:#010x}", f.e.unwrap() + 10))), "{t}");
    // the resume command of a cut range keeps --to
    let t = ask(&["fn", &path, &format!("{:#x}", f.a), "--to", &end, "--lines", "10"]).unwrap();
    let last = t.lines().last().unwrap();
    assert!(last.contains(&format!(" --to {end} ")) && last.ends_with("--lines 10"), "{last}");
    // a range that cannot be shown is a usage problem: exit 2
    for (a, to) in [(format!("{:#x}", f.b), format!("{:#x}", f.a)), (format!("{:#x}", f.b), format!("{:#x}", f.string))]
    {
        let e = ask(&["fn", &path, &a, "--to", &to]).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{e}");
    }
}

#[test]
fn xref_to_answers_a_range_through_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64_xref();
    let path = file(&dir, "elf", &f);
    let (lo, hi) = (format!("{:#x}", f.a), format!("{:#x}", f.text_end));
    let t = ask(&["xref", &path, &lo, "--to", &hi]).unwrap();
    assert!(t.starts_with(&format!("xref {lo}..{hi}  ")), "{t}");
    for x in [f.b, f.c] {
        assert!(t.lines().any(|l| l.starts_with(&format!("{x:#x}  function "))), "{x:#x}: {t}");
    }
    let j: serde_json::Value =
        serde_json::from_str(&ask(&["xref", &path, &lo, "--to", &hi, "--json"]).unwrap()).unwrap();
    assert!(j["next"].is_null() && j["addresses"].as_u64().unwrap() >= 2, "{j}");
    // without --to the answer is the one address, as before
    let one = ask(&["xref", &path, &format!("{:#x}", f.b)]).unwrap();
    assert!(one.starts_with(&format!("xref {:#x}  function ", f.b)) && one.contains("calls (2)"), "{one}");
}

#[test]
fn fn_to_and_bytes_pages_cover_the_range_asked() {
    let exe = std::env::current_exe().unwrap().display().to_string();
    let j: serde_json::Value =
        serde_json::from_str(&ask(&["funcs", &exe, "--sort", "size", "--json", "--lines", "10"]).unwrap()).unwrap();
    let start = j["items"][0]["addr"].as_u64().unwrap();
    let s = format!("{start:#x}");
    let more = |t: &str| t.lines().any(|l| l.starts_with("... ") && l.contains(" more lines: "));
    // bytes: a short length is its rows, a long one is not cut at the default 120, a longer one stops at RANGE_LINES
    let t = ask(&["bytes", &exe, &s, "32"]).unwrap();
    assert_eq!(t.lines().count(), 2, "{t}");
    let t = ask(&["bytes", &exe, &s, "4096"]).unwrap();
    assert!(t.lines().count() == 256 && !more(&t), "{t}");
    let t = ask(&["bytes", &exe, &s, "16384"]).unwrap();
    let last = t.lines().last().unwrap();
    assert_eq!(t.lines().count(), RANGE_LINES, "{last}");
    assert!(last.starts_with("... 625 more lines: ") && last.ends_with(&format!("--lines {RANGE_LINES}")), "{last}");
    // --lines still bounds it
    assert_eq!(ask(&["bytes", &exe, &s, "4096", "--lines", "50"]).unwrap().lines().count(), 50);
    // fn --to: a range longer than 120 lines comes whole, exactly its lines
    let lines_of = |to: &str| -> u64 {
        let j: serde_json::Value =
            serde_json::from_str(&ask(&["fn", &exe, &s, "--to", to, "--json", "--lines", "100000"]).unwrap()).unwrap();
        j["lines"].as_u64().unwrap()
    };
    let (to, total) = (0x200..0x1000u64)
        .step_by(0x40)
        .map(|k| format!("{:#x}", start + k))
        .map(|to| {
            let n = lines_of(&to);
            (to, n)
        })
        .find(|(_, n)| (130..RANGE_LINES as u64 - 10).contains(n))
        .expect("a range of 130 to 390 lines in the largest function and after it");
    let t = ask(&["fn", &exe, &s, "--to", &to]).unwrap();
    assert!(t.lines().count() as u64 == total && !more(&t), "{total}: {t}");
    // a range xref that does not fit is cut like any other answer and resumes with --to
    let far = format!("{:#x}", start + 0x80000);
    let t = ask(&["xref", &exe, &s, "--to", &far, "--lines", "10"]).unwrap();
    assert!(t.lines().count() == 10 && t.lines().last().unwrap().contains(&format!(" --to {far} --from ")), "{t}");
    // without --to, fn keeps the default page
    let t = ask(&["fn", &exe, &s]).unwrap();
    assert!(t.lines().count() <= nexo_bin::DEFAULT_LINES, "{t}");
}

#[test]
fn fn_to_with_lines_120_gives_120_lines_and_the_more_note() {
    let exe = std::env::current_exe().unwrap().display().to_string();
    let j: serde_json::Value =
        serde_json::from_str(&ask(&["funcs", &exe, "--sort", "size", "--json", "--lines", "10"]).unwrap()).unwrap();
    let start = j["items"][0]["addr"].as_u64().unwrap();
    let (s, to) = (format!("{start:#x}"), format!("{:#x}", start + 0x1000));
    let whole: serde_json::Value =
        serde_json::from_str(&ask(&["fn", &exe, &s, "--to", &to, "--json", "--lines", "100000"]).unwrap()).unwrap();
    let total = whole["lines"].as_u64().unwrap();
    assert!(total > 130, "the range is longer than the page: {total}");
    let t = ask(&["fn", &exe, &s, "--to", &to, "--lines", "120"]).unwrap();
    let last = t.lines().last().unwrap();
    assert_eq!(t.lines().count(), 120, "{last}");
    assert!(last.starts_with(&format!("... {} more lines: ", total - 119)), "{last}");
    // the next page is 120 lines too, not the range
    assert!(last.ends_with("--lines 120") || last.contains(" --lines 120 "), "{last}");
    let next = last.split(" --from ").nth(1).unwrap().split(' ').next().unwrap();
    let t2 = ask(&["fn", &exe, &s, "--to", &to, "--lines", "120", "--from", next]).unwrap();
    assert_eq!(t2.lines().count() as u64, (total - 119).min(120), "{t2}");
}

#[test]
fn help_has_one_line_for_each_op() {
    let mut root = crate::cli::Args::command();
    let bin = root.find_subcommand_mut("bin").expect("bin is a command");
    let ops: Vec<String> = bin.get_subcommands().map(|c| c.get_name().to_string()).collect();
    for want in [
        "info", "fn", "xref", "funcs", "calls", "decomp", "proto", "struct", "var", "name", "note", "names", "bytes",
        "strings", "find",
    ] {
        assert!(ops.iter().any(|o| o == want), "{want} is missing from {ops:?}");
    }
    let help = bin.render_help().to_string();
    for op in &ops {
        if op == "help" {
            continue;
        }
        let rows = help.lines().filter(|l| l.trim_start().starts_with(&format!("{op} ")) || l.trim() == op).count();
        assert_eq!(rows, 1, "{op}: {help}");
    }
}

#[test]
fn lines_and_json_cut_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = file(&dir, "elf", &f);
    let t = ask(&["info", &path, "--lines", "10"]).unwrap();
    assert_eq!(t.lines().count(), 10);
    let last = t.lines().last().unwrap();
    assert!(
        last.starts_with("... 4 more lines: nexo bin info ")
            && last.contains("--from 0x")
            && last.ends_with("--lines 10"),
        "{last}"
    );
    let from = last.split("--from ").nth(1).unwrap().split(' ').next().unwrap();
    let rest = ask(&["info", &path, "--from", from, "--lines", "10"]).unwrap();
    assert_eq!(rest.lines().count(), 4);
    let j: serde_json::Value =
        serde_json::from_str(&ask(&["info", &path, "--lines", "10", "--json"]).unwrap()).unwrap();
    assert_eq!(j["op"], "info");
    assert_eq!(j["left"], 4);
    assert_eq!(j["items"].as_array().unwrap().len(), 2);
    // the JSON resume command asks for JSON again; apart from that it is the text one
    let next = j["next"].as_str().unwrap();
    assert!(next.contains(" --json --from "), "{next}");
    assert_eq!(next.replace(" --json", ""), last.split(": ").nth(1).unwrap());
}

#[test]
fn a_question_that_cannot_be_answered_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "elf", &fixtures::elf64(false));
    let e = ask(&["fn", &path, "0x10"]).unwrap_err();
    assert_eq!(e.exit_code(), 1);
    assert!(e.to_string().contains("not inside any section"), "{e}");
    // a file that cannot be opened is a file problem: exit 2 like a file that is not read
    let e = ask(&["info", &dir.path().join("missing").display().to_string()]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
}
