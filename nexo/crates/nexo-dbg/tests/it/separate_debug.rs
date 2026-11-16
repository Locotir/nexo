#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A program stripped of its debug info, with the debug file beside it named by `.gnu_debuglink`: the first answer
//! says which file gave the debug info, and names, lines and locals come from it. The programs are compiled here
//! (skipped without `cc`); the link section is written by `nexo_bin::fixtures::add_gnu_debuglink`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = "#include <stdlib.h>\n\
    __attribute__((noinline)) int score(int base, int k) { int total = base * k; return total + 1; }\n\
    int main(int argc, char **argv) { (void)argv; return score(argc + 5, 3) == 19 ? 0 : 1; }\n";

fn cc(dir: &std::path::Path, args: &[&str]) -> bool {
    let ok = std::process::Command::new("cc")
        .args(["-O0", "-no-pie"])
        .args(args)
        .current_dir(dir)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
    }
    ok
}

/// `prog` (no debug info, no symbols, a link to `prog.debug`) and `prog.debug` (the `-g` build) in a new folder.
fn pair(link: bool) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.c"), SRC).unwrap();
    if !(cc(dir.path(), &["-g", "-o", "prog.debug", "a.c"]) && cc(dir.path(), &["-s", "-o", "prog", "a.c"])) {
        return None;
    }
    let exe = dir.path().join("prog");
    if link {
        let crc = nexo_bin::crc32(&std::fs::read(dir.path().join("prog.debug")).unwrap());
        let linked = nexo_bin::fixtures::add_gnu_debuglink(&std::fs::read(&exe).unwrap(), "prog.debug", crc).unwrap();
        std::fs::write(&exe, linked).unwrap();
    }
    Some((dir, exe))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap().text_all()
}

#[test]
fn a_stripped_program_is_debugged_with_the_names_lines_and_locals_of_its_debug_file() {
    let Some((_d, exe)) = pair(true) else { return };
    let want = exe.with_file_name("prog.debug");
    let addr = match nexo_bin::place_of(&nexo_bin::Image::open(&want).unwrap(), "score").unwrap() {
        nexo_bin::Place::Code(a) => a,
        other => panic!("{other:?}"),
    };
    let (mut s, first) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let text = first.text_all();
    assert!(
        text.contains("debug info  read from") && text.contains("prog.debug") && text.contains("gnu_debuglink"),
        "{text}"
    );
    // the stripped program has no symbols of its own: the name and the line come from the debug file
    let b = run(&mut s, "break score");
    assert!(b.contains(&format!("{addr:#x}")) || b.contains("score"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("score") && stop.contains("a.c:2"), "{stop}");
    let locals = run(&mut s, "locals");
    assert!(locals.contains("arg base (int) = 6") && locals.contains("arg k (int) = 3"), "{locals}");
}

#[test]
fn a_stripped_program_without_its_debug_file_says_what_it_looked_for() {
    let Some((d, exe)) = pair(true) else { return };
    std::fs::remove_file(d.path().join("prog.debug")).unwrap();
    let (_s, first) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let text = first.text_all();
    assert!(text.contains("no separate file taken") && text.contains("gnu_debuglink names prog.debug"), "{text}");
}
