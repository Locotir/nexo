#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A local's stack slot exists for the whole call, so before the code reaches its declaration, or outside the
//! block that declares it, the slot holds what an earlier call or pass left there. The stop report, `locals` and
//! `print` still show that value, with the fact beside it; parameters and a stop past the assignment are as before,
//! and `args` agrees with `locals`. Skipped without `cc`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"#include <string.h>
__attribute__((noinline)) unsigned long store(char *p, const char *s) {
    unsigned long l = strlen(s);
    if (l > 16) l = 16;
    memcpy(p, s, l);
    p[l] = 0;
    unsigned long late = l * 2;
    return late;
}
__attribute__((noinline)) int sum(int n) {
    int t = 0;
    for (int i = 0; i < n; i++) {
        int sq = i * i;
        t += sq;
    }
    return t;
}
int main(void) {
    char buf[32];
    store(buf, "zoe");
    store(buf, "maximilian-xavier");
    return sum(4) == 14 ? 0 : 1;
}
"#;

fn session() -> Option<(tempfile::TempDir, Session)> {
    let dir = tempfile::tempdir().unwrap();
    // built as src/p.c, so its places read `src/p.c:3`
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/p.c"), SRC).unwrap();
    let ok = std::process::Command::new("cc")
        .args(["-g", "-O0", "-o", "p", "src/p.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    let (s, _) = open(&Kind::Run { exe: dir.path().join("p"), args: vec![] }).expect("start");
    Some((dir, s))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn locals_of(stop: &str) -> &str {
    stop.lines().find(|l| l.starts_with("locals:")).unwrap_or_else(|| panic!("no locals line: {stop}"))
}

#[test]
fn a_local_declared_later_shows_its_old_slot_with_where_it_is_declared_and_where_the_stop_is() {
    let Some((_d, mut s)) = session() else { return };
    // the second call: l and late still hold the first call's 3 and 6
    run(&mut s, "break store");
    run(&mut s, "cont");
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.contains("\"maximilian-xavier\""), "{l}");
    assert!(l.contains(", l = 3 (not assigned yet: declared at src/p.c:3, stopped at the start of that line)"), "{l}");
    assert!(l.contains(", late = 6 (not assigned yet: declared at src/p.c:7, stopped at src/p.c:3)"), "{l}");
    // parameters are stored by the prologue: never marked
    assert!(l.starts_with("locals: p = 0x") && !l.split(", l = ").next().unwrap().contains("(not"), "{l}");
    let locals = run(&mut s, "locals");
    assert!(
        locals.contains("l (long unsigned int) = 3 (not assigned yet: declared at src/p.c:3, stopped at the start"),
        "{locals}"
    );
    assert!(
        locals.contains("late (long unsigned int) = 6 (not assigned yet: declared at src/p.c:7, stopped at src/p.c:3)"),
        "{locals}"
    );
    let p = run(&mut s, "print late");
    assert!(p.contains("= 6 (not assigned yet: declared at src/p.c:7, stopped at src/p.c:3)"), "{p}");
    // args and locals say the same of each parameter
    let args = run(&mut s, "args");
    for line in args.lines().filter(|a| a.starts_with("p (") || a.starts_with("s (")) {
        assert!(locals.lines().any(|x| x == format!("arg {line}")), "{line}\n{locals}");
    }
    assert!(!args.contains("(not"), "{args}");
    // one line on, l is assigned and late is not
    run(&mut s, "delete all");
    run(&mut s, "break p.c:4");
    run(&mut s, "cont");
    let at = run(&mut s, "locals");
    assert!(at.contains("l (long unsigned int) = 17\n") || at.ends_with("l (long unsigned int) = 17"), "{at}");
    assert!(
        at.contains("late (long unsigned int) = 6 (not assigned yet: declared at src/p.c:7, stopped at src/p.c:4)"),
        "{at}"
    );
}

#[test]
fn past_the_assignment_the_values_show_as_before() {
    let Some((_d, mut s)) = session() else { return };
    run(&mut s, "break p.c:8");
    run(&mut s, "cont");
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.contains(", l = 16, late = 32"), "{l}");
    assert!(!l.contains("(not"), "{l}");
    let p = run(&mut s, "print l");
    assert!(p.ends_with("= 16") || p.contains("= 16\n"), "{p}");
}

#[test]
fn a_block_local_outside_its_block_is_shown_as_not_live_and_in_a_loop_as_this_pass() {
    let Some((_d, mut s)) = session() else { return };
    // after the loop: i and sq hold their last values, outside their blocks
    run(&mut s, "break p.c:16");
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.contains("n = 4, t = 14"), "{l}");
    assert!(l.contains("i = 4 (not live here: its block is src/p.c:12-14, stopped at src/p.c:16)"), "{l}");
    assert!(l.contains("sq = 9 (not live here: its block is src/p.c:13-14, stopped at src/p.c:16)"), "{l}");
    let p = run(&mut s, "print sq");
    assert!(p.contains("= 9 (not live here: its block is src/p.c:13-14, stopped at src/p.c:16)"), "{p}");
    // the loop body on its second pass: sq still holds the first pass's 0
    let Some((_d2, mut s)) = session() else { return };
    run(&mut s, "break p.c:13");
    run(&mut s, "cont");
    let stop = run(&mut s, "cont");
    let l = locals_of(&stop);
    assert!(l.contains("i = 1"), "{l}");
    assert!(
        l.contains("sq = 0 (not assigned yet in this pass: declared at src/p.c:13, stopped at the start of that line)"),
        "{l}"
    );
    assert!(!l.contains("i = 1 (not"), "{l}");
}
