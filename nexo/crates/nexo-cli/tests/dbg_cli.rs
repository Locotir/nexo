#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `nexo dbg` run through the binary: a session lives between calls and nothing is left behind.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn nexo(tmp: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nexo"))
        .arg("dbg")
        .args(args)
        .env("NEXO_TMP", tmp)
        .env("NEXO_CONFIG_DIR", tmp)
        .env("NEXO_DATA_DIR", tmp.join("data"))
        .env("NEXO_CACHE_DIR", tmp.join("cache"))
        .output()
        .expect("nexo runs")
}

fn out(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn sleep_bin() -> PathBuf {
    ["/usr/bin/sleep", "/bin/sleep"].iter().map(PathBuf::from).find(|p| p.is_file()).expect("a sleep program")
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

fn wait_gone(pid: u32) -> bool {
    for _ in 0..150 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    false
}

fn pid_of(text: &str) -> u32 {
    let l = text.lines().find(|l| l.contains("started pid")).unwrap_or_else(|| panic!("{text}"));
    l.rsplit(' ').next().unwrap().parse().unwrap()
}

#[test]
fn a_session_lives_between_calls_and_end_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let sleep = sleep_bin();
    let first = nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]);
    let text = out(&first);
    assert!(first.status.success(), "{text}");
    assert!(
        text.starts_with("debug ") && text.contains(" stop entry ") && text.contains("stopped at the program's entry"),
        "{text}"
    );
    let pid = pid_of(&text);
    assert!(alive(pid));
    // the next calls find the session by themselves (it is the only one)
    let regs = out(&nexo(&tmp, &["regs"]));
    assert!(regs.contains("rip") && regs.contains("rsp"), "{regs}");
    let bt = out(&nexo(&tmp, &["bt"]));
    assert!(bt.contains("#0"), "{bt}");
    let maps = out(&nexo(&tmp, &["maps"]));
    assert!(maps.contains("[stack]"), "{maps}");
    let bad = nexo(&tmp, &["nosuchcommand"]);
    assert_eq!(bad.status.code(), Some(2), "{}", out(&bad));
    let list = out(&nexo(&tmp, &["sessions"]));
    assert!(list.contains(&format!("pid {pid}")), "{list}");
    let json = out(&nexo(&tmp, &["regs", "--json"]));
    assert!(json.contains("\"session\"") && json.contains("\"state\": \"stopped\""), "{json}");
    let ended = out(&nexo(&tmp, &["end"]));
    assert!(ended.contains("ended"), "{ended}");
    assert!(wait_gone(pid), "the program {pid} is still there after `end`");
    let after = nexo(&tmp, &["regs"]);
    assert!(!after.status.success());
}

#[test]
fn a_program_that_ends_is_reported_and_kill_works() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let sleep = sleep_bin();
    let first = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "0.2"]));
    let pid = pid_of(&first);
    let cont = out(&nexo(&tmp, &["cont", "--wait", "10"]));
    assert!(cont.contains("exited with code 0"), "{cont}");
    assert!(wait_gone(pid));
    let regs = nexo(&tmp, &["regs"]);
    assert!(out(&regs).contains("the program has ended"), "{}", out(&regs));
    nexo(&tmp, &["end"]);
    let again = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    let pid2 = pid_of(&again);
    let k = out(&nexo(&tmp, &["kill"]));
    assert!(k.contains("killed"), "{k}");
    assert!(wait_gone(pid2));
    nexo(&tmp, &["end"]);
}

#[test]
fn the_program_goes_when_its_owner_goes() {
    let dir = tempfile::tempdir().unwrap();
    // the owner is the process named in the scratch folder's name
    let mut owner = Command::new(sleep_bin()).arg("60").spawn().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", owner.id()));
    let first = out(&nexo(&tmp, &["run", sleep_bin().to_str().unwrap(), "60"]));
    let pid = pid_of(&first);
    assert!(alive(pid));
    owner.kill().unwrap();
    owner.wait().unwrap();
    assert!(wait_gone(pid), "the program {pid} outlived its owner");
}

#[test]
fn a_file_that_is_not_a_program_fails_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-t", std::process::id()));
    let f = dir.path().join("notes.txt");
    std::fs::write(&f, "hello").unwrap();
    let o = nexo(&tmp, &["run", f.to_str().unwrap()]);
    assert!(!o.status.success());
    let t = out(&o);
    assert!(t.contains("nexo dbg:"), "{t}");
    let o = nexo(&tmp, &["run", "/no/such/file"]);
    assert!(out(&o).contains("is not a file"), "{}", out(&o));
}

#[test]
fn what_a_session_keeps_comes_back_with_the_next_one_of_the_same_binary() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-k", std::process::id()));
    let sleep = sleep_bin();
    let first = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    assert!(!first.contains("known about this binary"), "{first}");
    let saved = out(&nexo(&tmp, &["note", "crash: findings go here"]));
    assert_eq!(saved.lines().filter(|l| l.starts_with("saved:")).count(), 1, "{saved}");
    assert!(nexo(&tmp, &["end"]).status.success());
    let again = out(&nexo(&tmp, &["run", sleep.to_str().unwrap(), "60"]));
    assert!(again.contains("known about this binary") && again.contains("crash: findings go here"), "{again}");
    assert!(nexo(&tmp, &["end"]).status.success());
}

#[test]
fn a_core_file_that_is_not_one_is_refused_through_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(format!("nexo-tmp-{}-c", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let o = nexo(&tmp, &["core", sleep_bin().to_str().unwrap()]);
    let t = out(&o);
    assert!(!o.status.success() && t.contains("not a core file"), "{t}");
    let o = nexo(&tmp, &["core"]);
    assert!(out(&o).contains("usage: core <file>"), "{}", out(&o));
}
