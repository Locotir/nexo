#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A model that types the gdb it already knows: a C++ program with a `std::string` and a `std::vector` (built with the
//! system `g++`, -O2, not stripped, no DWARF, like the programs this debugger is used on) is driven with the shape of a
//! real gdb batch script. Each test skips with a message when there is no `g++` here.

use std::path::{Path, PathBuf};
use std::process::Command;

use nexo_dbg::{gdb_plan, open, script_units, split_words, Kind, Session};

const SOURCE: &str = r#"
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace lc {
struct Keeper {
    std::string tag;
    std::string note;
    std::vector<std::uint8_t> bytes;
};

__attribute__((noipa)) void shuffle(Keeper& k) {
    std::size_t i = 0, j = k.bytes.size();
    while (i + 1 < j) {
        std::swap(k.bytes[i], k.bytes[j - 1]);
        ++i;
        --j;
    }
}

__attribute__((noipa)) int report(Keeper& k, int round) {
    if (__builtin_expect(round < 0, 0)) throw std::runtime_error("negative round");
    std::printf("round %d size %zu first %02x\n", round, k.bytes.size(), static_cast<unsigned>(k.bytes[0]));
    return round;
}
}  // namespace lc

int main(int argc, char** argv) {
    lc::Keeper k;
    k.tag = "tag-that-is-longer-than-fifteen-chars";
    k.note = "short";
    for (int i = 0; i < 16; ++i) k.bytes.push_back(static_cast<std::uint8_t>(0x30 + i));
    int rounds = argc > 1 ? std::atoi(argv[1]) : 0;
    lc::report(k, 1);
    lc::shuffle(k);
    lc::report(k, 2);
    for (int r = 0; r < rounds; ++r) lc::report(k, 3 + r);
    return 0;
}
"#;

/// `lc::report(lc::Keeper&, int)` as the compiler writes it.
const REPORT: &str = "_ZN2lc6reportERNS_6KeeperEi";

struct Built {
    _dir: tempfile::TempDir,
    exe: PathBuf,
}

/// The program, built once per test; `None` (with a message) when there is no `g++`.
fn build() -> Option<Built> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("stdview.cpp");
    std::fs::write(&src, SOURCE).unwrap();
    let exe = dir.path().join("stdview");
    let made = Command::new("g++").args(["-O2", "-std=c++17", "-o"]).arg(&exe).arg(&src).output();
    match made {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            eprintln!("skipped: g++ could not build the test program: {}", String::from_utf8_lossy(&o.stderr));
            return None;
        }
        Err(e) => {
            eprintln!("skipped: no g++ here ({e})");
            return None;
        }
    }
    let bytes = std::fs::read(&exe).unwrap();
    if !bytes.windows(REPORT.len()).any(|w| w == REPORT.as_bytes()) {
        eprintln!("skipped: this g++ does not mangle lc::report as {REPORT}");
        return None;
    }
    Some(Built { _dir: dir, exe })
}

fn start(exe: &Path, args: &[&str]) -> Session {
    let kind = Kind::Run { exe: exe.to_path_buf(), args: args.iter().map(|s| s.to_string()).collect() };
    open(&kind).expect("start").0
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn refused(s: &mut Session, cmd: &str) -> String {
    match s.exec(&split_words(cmd)) {
        Ok(o) => panic!("{cmd} was taken: {}", o.text_all()),
        Err(e) => e.to_string(),
    }
}

/// What the `debug` tool does with a whole gdb script: start the session as the plan says, then run the rest in it.
fn script(exe: &Path, text: &str) -> (Session, String) {
    let parts = script_units(text);
    let plan = gdb_plan(&parts).expect("a plan").expect("the script has run last");
    let w = split_words(&plan.start);
    assert_eq!(w[0], "run", "{w:?}");
    assert!(Path::new(&w[1]) == exe, "{w:?}");
    let mut s = start(exe, &w[2..].iter().map(String::as_str).collect::<Vec<_>>());
    let out = s.exec(&["do".to_string(), plan.rest.join("; ")]).expect("the script runs").text_all();
    (s, out)
}

#[test]
fn the_shape_of_a_real_gdb_batch_script_runs_in_one_call() {
    let Some(b) = build() else { return };
    let exe = b.exe.display();
    // the gdb a person writes for this program: settings, two breakpoints at a function plus an offset, commands that
    // print and let the program go on, and `run` last
    let text = format!(
        r#"set pagination off
set confirm off
file {exe}
break *({REPORT}+0)
commands
silent
printf "\n--- round %d ---\n", $rsi
set $k = $rdi
printf "tag=%s\n", *(char**)$k
printf "begin=%p size=%d\n", *(void**)($k+0x40), (*(char**)($k+0x48) - *(char**)($k+0x40))
x/16bx *(void**)($k+0x40)
x/s *(void**)($k+0x20)
continue
end
run
quit"#
    );
    let (mut s, out) = script(&b.exe, &text);
    assert!(out.contains("exited with code 0"), "{out}");
    // the first hit sees the bytes as written, the second after `shuffle` reversed them
    assert!(out.contains("--- round 1 ---") && out.contains("--- round 2 ---"), "{out}");
    assert!(out.contains("0x30\t0x31\t0x32\t0x33\t0x34\t0x35\t0x36\t0x37"), "{out}");
    assert!(out.contains("0x38\t0x39\t0x3a\t0x3b\t0x3c\t0x3d\t0x3e\t0x3f"), "{out}");
    assert!(out.contains("0x3f\t0x3e\t0x3d\t0x3c\t0x3b\t0x3a\t0x39\t0x38"), "{out}");
    assert!(out.contains("tag=tag-that-is-longer-than-fifteen-chars"), "{out}");
    assert!(out.contains("size=16"), "{out}");
    assert!(out.contains("\"short\""), "{out}");
    // nothing stopped the program in between: two hits, one report
    assert!(!out.contains("breakpoint #1 (thread"), "{out}");
    let log = run(&mut s, "log");
    assert!(log.contains("--- round 2 ---"), "{log}");
}

#[test]
fn gdb_words_answer_in_one_call_each() {
    let Some(b) = build() else { return };
    let mut s = start(&b.exe, &[]);
    run(&mut s, &format!("break *({REPORT}+0)"));
    let hit = run(&mut s, "cont");
    assert!(hit.contains("breakpoint #1"), "{hit}");
    // x/NFU
    let bytes = run(&mut s, "x/16bx *(void**)($rdi+0x40)");
    assert!(bytes.contains("0x30\t0x31\t0x32\t0x33"), "{bytes}");
    let words = run(&mut s, "x/2gx $rdi");
    assert!(words.contains("0x00000000000000") || words.contains("0x0000"), "{words}");
    assert_eq!(run(&mut s, "x/s *(void**)($rdi+0x20)").matches("short").count(), 1);
    // print formats and registers, with and without the $
    assert!(run(&mut s, "p/x $rsi").contains("0x1"), "round is 1");
    assert!(run(&mut s, "p/d rsi").contains("= 1"));
    assert!(run(&mut s, "print/c 0x41").contains("65 'A'") || run(&mut s, "print/c 0x41").contains("'A'"));
    assert!(run(&mut s, "p $rip == $pc").contains("= 0x1"));
    // convenience variables keep a value and its type, and can be used in later expressions
    run(&mut s, "set $k = (void*)$rdi");
    let v = run(&mut s, "p/x *(char**)($k+0x20) == $k+0x30");
    assert!(v.contains("0x1"), "{v}");
    run(&mut s, "set $n = 3");
    assert!(run(&mut s, "print $n * 2").contains("= 0x6"));
    // casts and pointer arithmetic by element size
    assert!(run(&mut s, "p/x *(unsigned char*)(*(void**)($k+0x40)+1)").contains("0x31"));
    assert!(run(&mut s, "p/x *(unsigned short*)*(void**)($k+0x40)").contains("0x3130"));
    // printf
    let f = run(&mut s, "printf \"%d|%s|%5.2f|%#x|%c\\n\", $rsi, *(char**)$rdi, 3, 255, 65");
    assert!(f.contains("1|tag-that-is-longer-than-fifteen-chars|") && f.contains("0xff|A"), "{f}");
    assert!(run(&mut s, "echo hello\\n").contains("hello"));
    // the std::string and std::vector views
    let long = run(&mut s, "print $rdi --as string");
    assert!(long.contains("tag-that-is-longer-than-fifteen-chars") && long.contains("size 37"), "{long}");
    let short = run(&mut s, "print $rdi+0x20 --as string");
    assert!(short.contains("short") && short.contains("inline"), "{short}");
    let vec = run(&mut s, "print $rdi+0x40 --as vector");
    assert!(vec.contains("16 elements") && vec.contains("30 31 32 33"), "{vec}");
    let wrong = refused_or_text(&mut s, "print $rsp --as string");
    assert!(!wrong.is_empty());
    // info
    assert!(run(&mut s, "info registers rip").contains("rip"));
    assert!(run(&mut s, "info registers").contains("rax"));
    assert!(run(&mut s, "info breakpoints").contains("#1"));
    assert!(run(&mut s, "info proc mappings").contains("stdview"));
    assert!(run(&mut s, "info sharedlibrary").contains("libc") || run(&mut s, "info sharedlibrary").contains("lib"));
    assert!(run(&mut s, "bt").contains("#0"));
    // an unknown gdb command names its nearest equivalent
    let t = refused(&mut s, "ptype Keeper");
    assert!(t.contains("ptype") && t.len() > 20, "{t}");
    // delete and the rest of the run
    run(&mut s, "delete 1");
    let end = run(&mut s, "cont --wait 30");
    assert!(end.contains("exited with code 0"), "{end}");
}

fn refused_or_text(s: &mut Session, cmd: &str) -> String {
    match s.exec(&split_words(cmd)) {
        Ok(o) => o.text_all(),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_function_is_found_by_its_names_with_or_without_arguments_and_offsets() {
    let Some(b) = build() else { return };
    let mut s = start(&b.exe, &[]);
    // the hot function wins over its `.cold` part
    for place in [
        "lc::report",
        "'lc::report(lc::Keeper&, int)'",
        "lc::report(lc::Keeper&,int)",
        REPORT,
        &format!("{REPORT}+0x4"),
        "lc::report+0x4",
        &format!("*({REPORT}+4)"),
        "*(lc::report+4)",
    ] {
        let o = refused_or_text(&mut s, &format!("break {place}"));
        assert!(o.contains("breakpoint #") && o.contains("report"), "break {place}: {o}");
    }
    // they are all the same function: the program stops at the first one it reaches
    let hit = run(&mut s, "cont");
    assert!(hit.contains("breakpoint"), "{hit}");
    run(&mut s, "kill");
}

#[test]
fn commands_run_at_every_hit_and_a_hit_cap_brings_the_answer_back() {
    let Some(b) = build() else { return };
    let mut s = start(&b.exe, &["2500"]);
    run(&mut s, &format!("break *({REPORT}+0)"));
    run(&mut s, "commands 1");
    run(&mut s, "silent");
    run(&mut s, "printf \"round %d\\n\", $rsi");
    run(&mut s, "continue");
    let end = run(&mut s, "end");
    assert!(end.contains("goes on"), "{end}");
    let first = run(&mut s, "cont --wait 30");
    assert!(first.contains("breakpoint #1"), "{first}");
    assert!(first.contains("stops after 2000 hits"), "{first}");
    // the answer carries the newest lines and says how many more there are
    assert!(first.contains("log: ") && first.contains("earlier lines"), "{first}");
    let log = run(&mut s, "log");
    assert!(log.contains("round 1999") || log.contains("round 2000"), "{log}");
    // `cont` goes on; the actions keep running; the program ends
    let last = run(&mut s, "cont --wait 30");
    assert!(last.contains("exited with code 0") || last.contains("stops after 2000 hits"), "{last}");
}

#[test]
fn commands_only_hold_what_looks_and_memory_writes_ask() {
    let Some(b) = build() else { return };
    let mut s = start(&b.exe, &[]);
    run(&mut s, "break main");
    run(&mut s, "commands");
    let t = refused(&mut s, "write 0x10 90");
    assert!(t.contains("cannot run inside `commands`"), "{t}");
    // the block was dropped, so the next line is an ordinary command again
    assert!(run(&mut s, "regs").contains("rip"));
    // gdb's ways to write into the program point at the commands that ask
    let w = refused_or_text(&mut s, "set var $rsp = 1");
    assert!(!w.is_empty());
    let m = refused(&mut s, "set {int}0x1000 = 1");
    assert!(m.contains("write"), "{m}");
    run(&mut s, "kill");
}

#[test]
fn after_the_program_ended_the_log_and_history_still_answer_in_a_script() {
    let Some(b) = build() else { return };
    let mut s = start(&b.exe, &[]);
    run(
        &mut s,
        &format!("break *({REPORT}+0) log \"round {{rsi}} tag {{*(char**)rdi:s}} mem {{mem:*(void**)(rdi+0x40):4}}\""),
    );
    let end = run(&mut s, "cont --wait 30");
    assert!(end.contains("exited with code 0"), "{end}");
    let after = run(&mut s, "do \"log; history; status; echo done\"");
    assert!(after.contains("round 0x1") && after.contains("round 0x2"), "{after}");
    assert!(after.contains("tag \"tag-that-is-longer-than-fifteen-chars\""), "{after}");
    assert!(after.contains("30 31 32 33") && after.contains("3f 3e 3d 3c"), "{after}");
    assert!(after.contains("done"), "{after}");
}
