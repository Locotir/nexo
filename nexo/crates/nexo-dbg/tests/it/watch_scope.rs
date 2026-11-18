#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A watch on a local ends, with one stop, when its function returns (a deeper call of the same function returning
//! to the same place does not end it); a place worked out from an expression is told once with its address and size;
//! an operator's result is refused. Skipped without `cc`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"
int tab[4];
__attribute__((noinline)) int work(int n) {
    int acc = 0;
    for (int i = 0; i < n; i++) acc += i + 1;
    return acc;
}
__attribute__((noinline)) int down(int n) {
    int mine = n * 10;
    if (n > 0) down(n - 1);
    mine += 1;
    return mine;
}
int main(void) {
    tab[2] = 7;
    int r = work(3);
    int d = down(2);
    return r + d == 0;
}
"#;

fn session() -> Option<(tempfile::TempDir, Session)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("s.c"), SRC).unwrap();
    let ok = std::process::Command::new("cc")
        .args(["-g", "-O0", "-o", "s", "s.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("SKIPPED: no C compiler, so nothing was checked");
        return None;
    }
    let (s, _) = open(&Kind::Run { exe: dir.path().join("s"), args: vec![] }).expect("start");
    Some((dir, s))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// `cont` until a stop says the watch was deleted: the stops on the way, and the last one.
fn until_deleted(s: &mut Session) -> (Vec<String>, String) {
    let mut seen = Vec::new();
    for _ in 0..16 {
        let stop = run(s, "cont");
        if stop.contains("deleted") {
            return (seen, stop);
        }
        seen.push(stop);
    }
    panic!("the watch was never deleted: {seen:#?}");
}

#[test]
fn a_watch_on_a_local_ends_with_one_stop_when_its_function_returns() {
    let Some((_d, mut s)) = session() else { return };
    run(&mut s, "break work");
    run(&mut s, "cont");
    let w = run(&mut s, "watch acc");
    assert!(w.contains("`acc` is int at 0x") && w.contains("4 bytes") && w.contains("frame of work"), "{w}");
    assert!(run(&mut s, "breaks").contains("until work returns"));
    let (hits, end) = until_deleted(&mut s);
    assert!(hits.len() >= 3 && hits.iter().all(|h| h.contains("watch #2") && h.contains("->")), "{hits:#?}");
    assert!(end.contains("watch #2 deleted: `acc` went out of scope (frame of work returned)"), "{end}");
    assert!(!run(&mut s, "breaks").contains("watch 0x"), "the watch is gone");
}

#[test]
fn a_deeper_call_of_the_same_function_returning_to_the_same_place_does_not_end_the_watch() {
    let Some((_d, mut s)) = session() else { return };
    run(&mut s, "break down");
    // the frame of `down(1)`: its return address is inside `down`, where `down(0)` returns to as well
    run(&mut s, "cont");
    run(&mut s, "cont");
    run(&mut s, "watch mine");
    // `mine` of `down(1)` is written twice (10, then 11 after the inner call came back); `down(0)` writes its own
    // slot, and its return lands at the same address with the stack pointer lower, which does not end the watch
    let (hits, end) = until_deleted(&mut s);
    let own = hits.iter().filter(|h| h.contains("watch #2")).count();
    assert_eq!(own, 2, "{hits:#?}");
    assert!(end.contains("`mine` went out of scope (frame of down returned)"), "{end}");
}

#[test]
fn location_watch_is_not_tied_to_the_frame() {
    let Some((_d, mut s)) = session() else { return };
    run(&mut s, "break work");
    run(&mut s, "cont");
    let w = run(&mut s, "watch -l acc");
    assert!(w.contains("`acc` is int at 0x") && !w.contains("frame of work"), "{w}");
    assert!(!run(&mut s, "breaks").contains("returns"));
}

#[test]
fn an_element_and_a_cast_deref_are_worked_out_once_and_an_operator_is_refused() {
    let Some((_d, mut s)) = session() else { return };
    run(&mut s, "break work");
    run(&mut s, "cont");
    let w = run(&mut s, "watch tab[2]");
    assert!(w.contains("4 bytes") && w.contains("not again"), "{w}");
    let addr = run(&mut s, "print &tab[2]");
    let hex = addr.split("0x").nth(1).and_then(|r| r.split_whitespace().next()).expect("an address").to_string();
    let w = run(&mut s, &format!("watch *(unsigned*)0x{hex}"));
    assert!(w.contains("4 bytes") && w.contains("worked out once now"), "{w}");
    let e = s.exec(&split_words("watch acc+n")).unwrap_err().to_string();
    assert!(e.contains("no place in memory"), "{e}");
    let r = run(&mut s, "rwatch tab[2]");
    assert!(r.contains("r/w"), "{r}");
}
