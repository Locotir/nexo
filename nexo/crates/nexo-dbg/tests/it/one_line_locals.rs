#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A function written on one line has its local's declaration on the stop's line, so the line cannot say whether the
//! stop is before the store. The code does: no instruction between the entry and the stop writes to the slot. This
//! must hold with and without `endbr64` at the entry (`-fcf-protection`, which CI's gcc adds). Skipped without `cc`.

use nexo_dbg::{open, split_words, Kind, Session};

const SRC: &str = r#"#include <stdlib.h>
__attribute__((noinline)) char *slot_alloc(int n) { char *p = malloc(n); return p; }
int main(void) {
    char *a = slot_alloc(8);
    char *b = slot_alloc(16);
    free(a);
    free(b);
    return 0;
}
"#;

fn session(flag: &str) -> Option<(tempfile::TempDir, Session)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/n.c"), SRC).unwrap();
    let ok = std::process::Command::new("cc")
        .args(["-g", "-O0", flag, "-o", "n", "src/n.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no C compiler: skipped");
        return None;
    }
    let (s, _) = open(&Kind::Run { exe: dir.path().join("n"), args: vec![] }).expect("start");
    Some((dir, s))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn p_of(text: &str) -> String {
    text.lines()
        .find(|l| l.contains("p = ") && l.starts_with("locals:"))
        .unwrap_or_else(|| panic!("{text}"))
        .to_string()
}

fn one(flag: &str) {
    let Some((_d, mut s)) = session(flag) else { return };
    run(&mut s, "break slot_alloc");
    // the second call leaves the first call's pointer in the slot: it must be marked too
    for call in 0..2 {
        let stop = run(&mut s, "cont");
        let first = p_of(&stop);
        assert!(first.contains("(not assigned yet: declared at src/n.c:2,"), "call {call}: {first}");
        assert!(
            first.contains("no instruction between the function's entry and the stop writes to its slot"),
            "{first}"
        );
        // step on: the mark goes once the store ran, and never comes back
        let mut seen_assigned = false;
        for _ in 0..8 {
            run(&mut s, "next");
            let at = run(&mut s, "locals");
            // out of the function: the walk is over
            let Some(l) = at.lines().find(|l| l.starts_with("p (")) else { break };
            let marked = l.contains("(not assigned yet") || l.contains("(may not be assigned yet");
            if seen_assigned {
                assert!(!marked, "mark came back: {l}");
            }
            if !marked {
                seen_assigned = true;
            }
        }
        assert!(seen_assigned, "the mark never went in 8 steps");
    }
}

#[test]
fn a_one_line_function_marks_its_local_before_the_store_by_address() {
    one("-fcf-protection=none");
}

#[test]
fn the_same_with_endbr64_at_the_entry() {
    one("-fcf-protection=full");
}
