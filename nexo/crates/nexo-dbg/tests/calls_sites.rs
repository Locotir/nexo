#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `calls <sites>`: naming sites replaces what was logged (and says so), repeats count once, and an address
//! that is not an indirect call is refused with the next step. The program is a small C file; skipped (with a
//! line saying so) when there is no C compiler.

use nexo_dbg::{open, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// A session on a program with one indirect call (in `apply`), and that call's address.
fn start(test: &str) -> Option<(Session, String, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("SKIPPED {test}: no C compiler, so nothing was checked");
        return None;
    };
    let src = dir.path().join("t.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n\
         __attribute__((noinline)) int add_one(int x) { return x + 1; }\n\
         int (*volatile table[1])(int) = { add_one };\n\
         __attribute__((noinline)) int apply(int i, int v) { return table[i](v); }\n\
         int main(void) {\n    printf(\"%d\\n\", apply(0, 1));\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("t");
    assert!(std::process::Command::new(cc)
        .args(["-O0", "-fno-pie", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let dis = run(&mut s, "dis apply 20");
    let site = dis
        .lines()
        .find(|l| l.contains("call"))
        .and_then(|l| l.split_whitespace().find(|w| w.starts_with("0x")))
        .unwrap_or_else(|| panic!("no call in apply\n{dis}"))
        .trim_end_matches(':')
        .to_string();
    Some((s, site, dir))
}

#[test]
fn the_same_site_named_twice_is_logged_once_and_the_answer_says_so() {
    let Some((mut s, site, _d)) = start("the_same_site_named_twice") else { return };
    let on = run(&mut s, &format!("calls {site} {site}"));
    assert!(on.contains("logging 1 indirect call site:"), "{on}");
    assert!(on.contains("1 repeated site ignored"), "{on}");
    assert!(run(&mut s, "breaks").contains("indirect-call logging on 1 sites"));
}

#[test]
fn naming_sites_says_what_it_replaced() {
    let Some((mut s, site, _d)) = start("naming_sites_says_what_it_replaced") else { return };
    let all = run(&mut s, "calls");
    assert!(all.contains("logging 2 indirect calls"), "{all}");
    let one = run(&mut s, &format!("calls {site}"));
    assert!(one.contains("logging 1 indirect call site:"), "{one}");
    assert!(one.contains("replaced the 2 sites that were being logged"), "{one}");
    // nothing was on before: nothing to say about replacing
    run(&mut s, "calls off");
    let fresh = run(&mut s, &format!("calls {site}"));
    assert!(!fresh.contains("replaced"), "{fresh}");
}

#[test]
fn an_address_that_is_not_an_indirect_call_is_refused_and_logging_is_left_as_it_was() {
    let Some((mut s, site, _d)) = start("an_address_that_is_not_an_indirect_call") else { return };
    run(&mut s, &format!("calls {site}"));
    let err =
        s.exec(&split_words("calls apply")).expect_err("the first instruction of apply is not a call").to_string();
    assert!(err.contains("apply") && err.contains("is not an indirect call"), "{err}");
    assert!(err.contains("`dis") && err.contains("`calls` alone"), "the next step is named: {err}");
    let err2 = s.exec(&split_words(&format!("calls {site} apply"))).expect_err("one bad site refuses all").to_string();
    assert!(err2.contains("is not an indirect call"), "{err2}");
    // the one site logged before is still the only one
    assert!(run(&mut s, "breaks").contains("indirect-call logging on 1 sites"));
}
