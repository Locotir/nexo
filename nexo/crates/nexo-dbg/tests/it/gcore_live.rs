#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `gcore` of a live stopped program, opened again as a core file.

use nexo_dbg::{open, split_words, Kind, Session, State};

use crate::common;
use common::target;

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

#[test]
fn the_core_of_a_live_program_opens_with_the_same_registers_and_call_chain() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, _) = open(&Kind::Run { exe: target(), args: vec!["crash".into()] }).expect("start");
    let cont = run(&mut s, "cont --wait 10");
    assert!(cont.contains("SIGSEGV"), "{cont}");
    let (regs, bt) = (run(&mut s, "regs"), run(&mut s, "bt"));
    let path = dir.path().join("live.core");
    let said = run(&mut s, &format!("gcore {}", path.display()));
    assert!(said.contains("core written") && said.contains("threads"), "{said}");
    let _ = run(&mut s, "kill");
    let (mut c, _) = open(&Kind::Core { file: path, exe: Some(target()) }).expect("open the core");
    assert_eq!(c.state, State::Stopped);
    assert_eq!(run(&mut c, "regs"), regs);
    let names = |t: &str| t.lines().filter_map(|l| l.split_whitespace().nth(2).map(str::to_string)).collect::<Vec<_>>();
    let (again, live) = (names(&run(&mut c, "bt")), names(&bt));
    assert!(again.len() >= live.len() && again[..live.len()] == live[..], "{again:?} {live:?}");
    assert!(run(&mut c, "mem rsp 16").contains("0x"));
}
