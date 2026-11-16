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
    let t = ask(&["syms", &path, "--kind", "import"]).unwrap();
    assert!(t.contains("log") && t.contains("env"), "{t}");
}

/// `nexo bin syms FILE --kind import,got` reaches the same table the tool reads, with the IAT slot named.
#[test]
fn syms_from_the_command_line_lists_a_pe_import_with_its_dll_and_slot() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "x.exe", &fixtures::pe64_xref());
    let t = ask(&["syms", &path, "--kind", "import,got"]).unwrap();
    assert!(t.contains("ExitProcess") && t.contains("KERNEL32.dll") && t.contains("__imp_ExitProcess"), "{t}");
    let j = ask(&["syms", &path, "--kind", "got", "--json"]).unwrap();
    assert!(j.contains("\"items\"") && j.contains("__imp_ExitProcess"), "{j}");
    let e = crate::cli::Args::try_parse_from(["nexo", "bin", "syms", &path, "--kind", "everything"]).unwrap_err();
    assert!(e.to_string().contains("possible values"), "{e}");
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

/// The test program and a place in it with a long run of code after it, whatever the OS lays out: the first
/// function of the largest code section, and the end of that section's bytes.
fn exe_code() -> (String, u64, u64) {
    let exe = std::env::current_exe().unwrap();
    let img = Image::open(&exe).unwrap();
    let s = img.sections.iter().filter(|s| s.exec).max_by_key(|s| s.file_size).unwrap();
    let (lo, hi) = (s.addr, s.addr + s.file_size.min(s.size));
    let start = img.funcs().iter().find(|f| f.start >= lo && f.end <= hi).unwrap().start;
    assert!(hi - start >= 0x10000, "{} has {:#x} bytes of code", s.name, hi - start);
    (exe.display().to_string(), start, hi)
}

/// The JSON of one whole answer (no cut).
fn whole_json(args: &[&str]) -> serde_json::Value {
    let mut a = args.to_vec();
    a.extend(["--json", "--lines", "1000000"]);
    serde_json::from_str(&ask(&a).unwrap()).unwrap()
}

#[test]
fn fn_to_and_bytes_pages_cover_the_range_asked() {
    let (exe, start, hi) = exe_code();
    let s = format!("{start:#x}");
    let more = |t: &str| t.lines().any(|l| l.starts_with("... ") && l.contains(" more lines: "));
    // bytes: a short length is its rows, a long one is not cut at the default 120, and neither is a longer one
    let t = ask(&["bytes", &exe, &s, "32"]).unwrap();
    assert_eq!(t.lines().count(), 2, "{t}");
    let t = ask(&["bytes", &exe, &s, "4096"]).unwrap();
    assert!(t.lines().count() == 256 && !more(&t), "{t}");
    let t = ask(&["bytes", &exe, &s, "16384"]).unwrap();
    assert!(t.lines().count() == 1024 && !more(&t), "{} lines", t.lines().count());
    // --lines still bounds it
    assert_eq!(ask(&["bytes", &exe, &s, "4096", "--lines", "50"]).unwrap().lines().count(), 50);
    // fn --to: a range longer than 120 lines comes whole, exactly its lines (the range is grown until it is one)
    let (to, total) = (0x200..0x4000u64.min(hi - start))
        .step_by(0x40)
        .map(|k| format!("{:#x}", start + k))
        .map(|to| {
            let n = whole_json(&["fn", &exe, &s, "--to", &to])["lines"].as_u64().unwrap();
            (to, n)
        })
        .find(|(_, n)| (130..390).contains(n))
        .expect("a range of 130 to 390 lines in the code");
    let t = ask(&["fn", &exe, &s, "--to", &to]).unwrap();
    assert!(t.lines().count() as u64 == total && !more(&t), "{total}: {t}");
    // a range xref that does not fit is cut like any other answer and resumes with --to (grown until the whole
    // answer is longer than the page, however many references this build has)
    let far = std::iter::successors(Some(0x1000u64), |k| (*k < hi - start).then(|| (k * 2).min(hi - start)))
        .map(|k| format!("{:#x}", start + k))
        .find(|far| whole_json(&["xref", &exe, &s, "--to", far])["shown"].as_u64().unwrap() > 10)
        .expect("more than 10 lines of references into the code section");
    let t = ask(&["xref", &exe, &s, "--to", &far, "--lines", "10"]).unwrap();
    assert!(t.lines().count() == 10 && t.lines().last().unwrap().contains(&format!(" --to {far} --from ")), "{t}");
    // without --to, fn keeps the default page
    let t = ask(&["fn", &exe, &s]).unwrap();
    assert!(t.lines().count() <= nexo_bin::DEFAULT_LINES, "{t}");
}

#[test]
fn fn_to_with_lines_120_gives_120_lines_and_the_more_note() {
    let (exe, start, hi) = exe_code();
    let s = format!("{start:#x}");
    // a range longer than the page, grown until it is one
    let (to, total) = std::iter::successors(Some(0x400u64), |k| (*k < hi - start).then(|| (k * 2).min(hi - start)))
        .map(|k| format!("{:#x}", start + k))
        .map(|to| {
            let n = whole_json(&["fn", &exe, &s, "--to", &to])["lines"].as_u64().unwrap();
            (to, n)
        })
        .find(|(_, n)| *n > 130)
        .expect("a range longer than 130 lines in the code");
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
        "info", "fn", "xref", "funcs", "calls", "cfg", "decomp", "proto", "struct", "var", "name", "note", "names",
        "bytes", "strings", "find",
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

#[test]
fn every_address_op_takes_a_symbol_an_offset_and_a_range() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = file(&dir, "elf", &f);
    let (a, b) = (format!("{:#x}", f.a), format!("{:#x}", f.b));
    let a4 = format!("{:#x}", f.a + 4);
    // the same answer, apart from the commands it suggests, which repeat the target as it was written, and the time
    let answer = |args: &[&str]| -> String {
        let keep = |l: &str| l.split("  ").filter(|w| !w.ends_with(" ms")).collect::<Vec<_>>().join("  ");
        ask(args).unwrap().lines().filter(|l| !l.contains("nexo bin ")).map(keep).collect::<Vec<_>>().join("\n")
    };
    let same = |forms: &[Vec<&str>]| {
        let first = answer(&forms[0]);
        assert!(!first.is_empty(), "{:?}", forms[0]);
        for args in &forms[1..] {
            assert_eq!(answer(args), first, "{args:?}");
        }
    };
    // bytes: the old address and length, a symbol with a length, and a range in each form
    let a8 = format!("{a}..{:#x}", f.a + 8);
    same(&[
        vec!["bytes", &path, &a, "8"],
        vec!["bytes", &path, "main", "8"],
        vec!["bytes", &path, "main+8"],
        vec!["bytes", &path, "main..main+8"],
        vec!["bytes", &path, &a8],
    ]);
    same(&[
        vec!["bytes", &path, &a4, "4"],
        vec!["bytes", &path, "main+4", "4"],
        vec!["bytes", &path, "main+4..main+8"],
    ]);
    assert!(ask(&["bytes", &path, ".data", "--as", "u32"]).unwrap().contains("0x12345678"));
    // fn: an address inside, a range as A..B or --to, with numbers or names
    same(&[vec!["fn", &path, &a4], vec!["fn", &path, "main+0x4"], vec!["fn", &path, "main+4"]]);
    same(&[
        vec!["fn", &path, &a, "--to", &b],
        vec!["fn", &path, "main", "--to", "helper"],
        vec!["fn", &path, "main..helper"],
        vec!["fn", &path, &format!("{a}..{b}")],
    ]);
    // xref: a name, an offset, a range
    same(&[vec!["xref", &path, &b], vec!["xref", &path, "helper"], vec!["xref", &path, "helper+0"]]);
    let te = format!("{:#x}", f.text_end);
    same(&[vec!["xref", &path, &a, "--to", &te], vec!["xref", &path, &format!("main..{te}")]]);
    // emu --to and the ops that find a function take the same forms
    same(&[vec!["emu", &path, "main", "--to", &b], vec!["emu", &path, "main", "--to", "helper"]]);
    same(&[vec!["calls", &path, &a4], vec!["calls", &path, "main+4"]]);
    // patch --at: a name with an offset lands on the same file offset as the number
    let at = |n: &str, at: &str| {
        let out = dir.path().join(n).display().to_string();
        let r = ask(&["patch", &path, "--out", &out, "--at", at, "--bytes", "90", "--json"]).unwrap();
        serde_json::from_str::<serde_json::Value>(&r).unwrap()["file_offset"].clone()
    };
    assert_eq!(at("p1", &format!("{:#x}", f.a + 1)), at("p2", "main+1"));
}

#[test]
fn an_unknown_name_is_answered_with_the_close_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = file(&dir, "elf", &fixtures::elf64(false));
    for args in [
        vec!["fn", &path, "mian"],
        vec!["bytes", &path, "mian", "4"],
        vec!["bytes", &path, "mian..helper"],
        vec!["xref", &path, "mian+4"],
        vec!["emu", &path, "main", "--to", "helpr"],
    ] {
        let e = ask(&args).unwrap_err().to_string();
        assert!(e.contains("close names: main") || e.contains("close names: helper"), "{args:?}: {e}");
    }
}

#[test]
fn grep_and_from_page_bytes_and_fn_and_the_page_says_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = file(&dir, "elf", &f);
    // bytes: rows keyed by address; --from starts at the first row at or after it, --grep keeps the keys
    let whole = ask(&["bytes", &path, "main", "64", "--lines", "all"]).unwrap();
    let rows: Vec<&str> = whole.lines().collect();
    assert_eq!(rows.len(), 4, "{whole}");
    let key = rows[2].split_whitespace().next().unwrap();
    let from = ask(&["bytes", &path, "main", "64", "--from", key]).unwrap();
    assert_eq!(from.lines().collect::<Vec<_>>(), rows[2..], "{from}");
    let g = ask(&["bytes", &path, "main", "64", "--grep", &format!("^{key}")]).unwrap();
    assert_eq!(
        g.lines().collect::<Vec<_>>(),
        [format!("... --grep ^{key}: 1 of 4 rows match").as_str(), rows[2]],
        "{g}"
    );
    // fn: rows keyed by instruction address; the answer says how many rows matched of how many
    let whole = ask(&["fn", &path, "main", "--lines", "all"]).unwrap();
    let total = whole.lines().count();
    assert!(total > 3, "{whole}");
    let g = ask(&["fn", &path, "main", "--grep", "ret|jmp"]).unwrap();
    let first = g.lines().next().unwrap();
    assert!(first.starts_with("... --grep ret|jmp: ") && first.ends_with(&format!(" of {total} rows match")), "{g}");
    assert!(g.lines().skip(1).all(|l| l.contains("ret") || l.contains("jmp")), "{g}");
    let at = whole.lines().nth(3).unwrap().split_whitespace().next().unwrap();
    let from = ask(&["fn", &path, "main", "--from", at]).unwrap();
    assert_eq!(from.lines().collect::<Vec<_>>(), whole.lines().skip(3).collect::<Vec<_>>(), "{from}");
    // a cut page names what is left and how to go on, with the filter kept
    let t = ask(&["fn", &path, "main", "--grep", ".", "--lines", "10"]).unwrap();
    if total > 10 {
        let last = t.lines().last().unwrap();
        assert!(last.contains(" more lines: ") && last.contains("--grep"), "{t}");
    }
    // a bad expression and a bad --lines say what was wrong
    assert!(ask(&["fn", &path, "main", "--grep", "("]).unwrap_err().to_string().contains("regular expression"));
    let bad = crate::cli::Args::try_parse_from(["nexo", "bin", "fn", &path, "--lines", "lots"]);
    assert!(bad.unwrap_err().to_string().contains("number or all"));
}

#[test]
fn find_takes_a_range_and_fn_decodes_data_as_code_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = file(&dir, "elf", &f);
    let img = Image::open(std::path::Path::new(&path)).unwrap();
    let (a, b) = (format!("{:#x}", f.a), format!("{:#x}", f.b));
    let hits = |t: &str| -> Vec<String> {
        t.lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|w| w.starts_with("0x"))
            .map(String::from)
            .collect()
    };
    // the bytes of main's first instruction are found inside main, and the same hits come from every form of
    // the range
    let pat = img.read_at(f.a, 2).unwrap().iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    let whole = ask(&["find", &path, &pat]).unwrap();
    let forms = [format!("{a}..{b}"), "main..helper".to_string(), format!("main+{}", f.b - f.a), "main".to_string()];
    let first = ask(&["find", &path, &pat, "--in", &forms[0]]).unwrap();
    assert!(hits(&first).contains(&format!("{:#010x}", f.a)), "{first}");
    assert!(first.contains(&format!("--in {}", forms[0])) || !first.contains("more lines"), "{first}");
    for r in &forms[1..3] {
        assert_eq!(hits(&ask(&["find", &path, &pat, "--in", r]).unwrap()), hits(&first), "{r}");
    }
    assert!(hits(&whole).len() >= hits(&first).len(), "{whole}");
    // a range that holds no match says so; an end before the start says what was wrong
    let e = ask(&["find", &path, &pat, "--in", "helper..main"]).unwrap_err().to_string();
    assert!(e.contains("not after its start"), "{e}");
    // data is refused as code, and decoded when asked
    let refused = ask(&["fn", &path, ".data", "--to", ".data+16"]).unwrap_err().to_string();
    assert!(!refused.is_empty());
    let t = ask(&["fn", &path, ".data", "--to", ".data+16", "--code"]).unwrap();
    assert!(t.lines().next().unwrap().contains("decoded as code on request"), "{t}");
    assert!(t.lines().count() > 1, "{t}");
}

#[test]
fn cfg_gives_the_blocks_of_one_function_and_pages_like_every_op() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = file(&dir, "elf", &f);
    // A is one block: it calls B and C and returns
    let t = ask(&["cfg", &path, &format!("{:#x}", f.a)]).unwrap();
    assert!(t.starts_with("cfg ") && t.contains("blocks 1, edges 2, loops 0"), "{t}");
    assert!(t.contains(&format!("block {:#x}..", f.a)) && t.contains("insns 6  idom -  entry"), "{t}");
    assert!(t.contains("calls ") && t.contains("; ends: ret"), "{t}");
    // the page options are the ones of every op
    let t = ask(&["cfg", &path, &format!("{:#x}", f.a), "--grep", "^block"]).unwrap();
    assert_eq!(t.lines().count(), 2, "{t}");
    assert!(t.starts_with("... --grep ^block: 1 of 4 rows match"), "{t}");
    let t = ask(&["cfg", &path, &format!("{:#x}", f.a), "--from", "1", "--lines", "10"]).unwrap();
    assert!(t.starts_with("block ") && t.lines().count() == 3, "{t}");
    let j: serde_json::Value =
        serde_json::from_str(&ask(&["cfg", &path, &format!("{:#x}", f.a), "--json"]).unwrap()).unwrap();
    assert_eq!(j["op"], "cfg");
    assert_eq!(j["items"][0]["row"], "block");
}

/// Bare x86-64 code as a debugger dumps it: a function that passes a string to the one after it, int3 padding
/// between them, the string after the code.
fn bare_code() -> Vec<u8> {
    let mut b = vec![0x55, 0x48, 0x89, 0xe5]; // push rbp; mov rbp, rsp
    b.extend_from_slice(&[0x48, 0x8d, 0x3d, 0x15, 0, 0, 0]); // lea rdi, [rip+0x15]: the string at +0x20
    b.extend_from_slice(&[0xe8, 0x04, 0, 0, 0]); // call +0x14
    b.extend_from_slice(&[0x5d, 0xc3, 0xcc, 0xcc]); // pop rbp; ret; padding
    b.extend_from_slice(&[0x55, 0x48, 0x89, 0xe5, 0x5d, 0xc3]); // the callee
    b.resize(0x20, 0xcc);
    b.extend_from_slice(b"hello from a dump\0");
    b
}

#[test]
fn raw_reads_bare_code_at_its_base_and_the_next_commands_keep_the_flags() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("region.bin");
    std::fs::write(&p, bare_code()).unwrap();
    let path = p.display().to_string();
    let raw = ["--raw", "--base", "0x7f0000001000"];
    let with = |op: &[&str]| ask(&[op, &raw[..]].concat()).unwrap();

    let f = with(&["funcs", &path]);
    assert!(f.contains("sub_7f0000001000") && f.contains("sub_7f0000001014"), "{f}");
    let x = with(&["xref", &path, "--str", "hello from a dump"]);
    assert!(x.contains("sub_7f0000001000"), "{x}");
    let o = with(&["overview", &path]);
    assert!(o.contains(&format!("{path} --raw --base 0x7f0000001000 --arch x86-64")), "{o}");

    // without --raw the bytes have no signature, and the answer says how to read them
    let e = ask(&["info", &path]).unwrap_err().to_string();
    assert!(e.contains("--raw --base ADDR"), "{e}");
    // --base alone says nothing about the file
    let e =
        crate::cli::Args::try_parse_from(["nexo", "bin", "info", &path, "--base", "0x1000"]).unwrap_err().to_string();
    assert!(e.contains("--raw"), "{e}");
}
