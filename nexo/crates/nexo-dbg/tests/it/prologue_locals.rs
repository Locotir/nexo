#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A stop at a function's first instruction (`break *addr`) is before its prologue has stored the parameters, so
//! their stack slots still hold the previous call's values: the stop report, `locals`, `args` and `print` show
//! the parameters from the argument registers instead and say the slots are not stored yet. `break <name>` stops
//! past the prologue, where nothing changes. Skipped without `cc`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
#include <string.h>
static char pool[64];
static unsigned long used;
static char *slot_alloc(void) { char *p = pool + used; used += 16; return p; }
__attribute__((noinline)) void store(char *p, const char *s) {
    unsigned long l = strlen(s);
    if (l > 16) l = 16;
    memcpy(p, s, l);
    p[l] = 0;
}
int main(void) {
    store(slot_alloc(), "zoe");
    store(slot_alloc(), "maximilian-xavier");
    return pool[0] == 'z' ? 0 : 1;
}
"#;

/// `helper` built without debug info, called from a `main` built with it.
const BARE: &str = "int helper(int a, int b) { int t = a * b; return t + 1; }\n";
const MAIN: &str = "int helper(int a, int b);\nint main(void) { return helper(6, 7) == 43 ? 0 : 1; }\n";

fn cc(dir: &std::path::Path, args: &[&str]) -> bool {
    let ok = std::process::Command::new("cc").args(args).current_dir(dir).status().is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
    }
    ok
}

fn names() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("names.c"), SRC).unwrap();
    cc(dir.path(), &["-g", "-O0", "-o", "names", "names.c"]).then(|| {
        let exe = dir.path().join("names");
        (dir, exe)
    })
}

fn code_at(exe: &std::path::Path, name: &str) -> u64 {
    match nexo_bin::place_of(&nexo_bin::Image::open(exe).unwrap(), name).unwrap() {
        nexo_bin::Place::Code(a) => a,
        other => panic!("{other:?}"),
    }
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap().text_all()
}

fn locals_of(stop: &str) -> &str {
    stop.lines().find(|l| l.starts_with("locals:")).unwrap_or_else(|| panic!("no locals line: {stop}"))
}

#[test]
fn at_the_entry_by_address_the_parameters_come_from_the_registers_not_the_old_slots() {
    let Some((_d, exe)) = names() else { return };
    let store = code_at(&exe, "store");
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, &format!("break *{store:#x}"));
    run(&mut s, "cont");
    // the second call: its slots still hold the first call's "zoe"
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.starts_with("locals: not stored yet (stopped at store+0x0, the prologue ends at store+0x"), "{l}");
    assert!(l.contains("; parameters in registers: p (rdi) = 0x"), "{l}");
    assert!(l.contains("s (rsi) = 0x") && l.contains("\"maximilian-xavier\""), "{l}");
    assert!(!l.contains("zoe") && !l.contains(" l = "), "{l}");
    let locals = run(&mut s, "locals");
    assert!(locals.contains("in store (stopped at store+0x0, the prologue ends at store+0x"), "{locals}");
    assert!(locals.contains("\"maximilian-xavier\" (in rsi)"), "{locals}");
    assert!(locals.contains("l (long unsigned int) = not stored yet"), "{locals}");
    let args = run(&mut s, "args");
    assert!(args.contains("the parameters as the call passed them"), "{args}");
    assert!(args.contains("s (const char *) = 0x") && args.contains("(in rsi)") && !args.contains("zoe"), "{args}");
    assert!(!args.contains("arg1 "), "{args}");
    let p = s.exec(&split_words("print s")).map(|o| o.text_all()).unwrap_or_else(|e| e.to_string());
    assert!(p.contains("s is not stored yet (stopped at store+0x0"), "{p}");
    // one step on is still in the prologue; the stop at its end shows the slots
    let end = l.split("the prologue ends at store+").nth(1).and_then(|r| r.split(')').next()).unwrap().to_string();
    let end = u64::from_str_radix(end.trim_start_matches("0x"), 16).unwrap();
    run(&mut s, "delete all");
    run(&mut s, &format!("break *{:#x}", store + end));
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.starts_with("locals: p = 0x") && l.contains("\"maximilian-xavier\""), "{l}");
}

#[test]
fn break_by_name_stops_past_the_prologue_and_shows_the_slots_as_before() {
    let Some((_d, exe)) = names() else { return };
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break store");
    run(&mut s, "cont");
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.starts_with("locals: p = 0x"), "{l}");
    assert!(l.contains(", s = 0x") && l.contains("\"maximilian-xavier\", l = "), "{l}");
    assert!(!l.contains("not stored"), "{l}");
    let args = run(&mut s, "args");
    assert!(args.contains("s (const char *) = 0x") && !args.contains("as the call passed"), "{args}");
}

#[test]
fn a_function_without_line_info_stops_at_its_entry_and_shows_no_locals() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bare.c"), BARE).unwrap();
    std::fs::write(dir.path().join("main.c"), MAIN).unwrap();
    if !cc(dir.path(), &["-g0", "-O0", "-c", "bare.c"])
        || !cc(dir.path(), &["-g", "-O0", "-o", "p", "main.c", "bare.o"])
    {
        return;
    }
    let exe = dir.path().join("p");
    let helper = code_at(&exe, "helper");
    for spec in ["helper".to_string(), format!("*{helper:#x}")] {
        let (mut s, _) = open(&Kind::Run { exe: exe.clone(), args: vec![] }).expect("start");
        run(&mut s, &format!("break {spec}"));
        let stop = run(&mut s, "cont");
        // no line info: a name stops at the entry itself, and nothing is said about variables it has none of
        assert!(stop.contains("helper") && !stop.contains("helper+"), "{spec}: {stop}");
        assert!(!stop.contains("locals:"), "{spec}: {stop}");
        let args = run(&mut s, "args");
        assert!(args.contains("arg1 rdi = 0x6 (6)") && args.contains("arg2 rsi = 0x7 (7)"), "{spec}: {args}");
    }
}
