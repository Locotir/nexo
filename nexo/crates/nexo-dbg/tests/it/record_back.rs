#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `record` and `back` against a real program (examples/dbg_target.rs): five single steps make five rows, and the
//! pc differs between them. The record only looks back: the program is where the last step left it.

use nexo_dbg::{open, split_words, Kind, Session};

use crate::common::target;

fn start() -> Session {
    open(&Kind::Run { exe: target(), args: vec!["wait".to_string()] }).expect("start").0
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

#[test]
fn five_steps_after_record_give_five_rows_and_the_pc_moves() {
    let mut s = start();
    assert!(run(&mut s, "back").contains("nothing recorded"));
    let on = run(&mut s, "record 100");
    assert!(on.contains("recording the last 100"), "{on}");
    let st = run(&mut s, "status");
    assert!(st.contains("recording on: 0 entries kept, 0 dropped"), "{st}");
    run(&mut s, "stepi 5");
    let back = run(&mut s, "back 5");
    let rows: Vec<&str> = back.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(rows.len(), 5, "{back}");
    let mut pcs: Vec<&str> = rows.iter().filter_map(|r| r.split_whitespace().nth(1)).collect();
    pcs.sort_unstable();
    pcs.dedup();
    assert!(pcs.len() >= 2, "the pc did not change: {back}");
    // newest first: the numbers go down
    assert!(rows[0].starts_with("#5 ") && rows[4].starts_with("#1 "), "{back}");
    assert!(back.contains("does not undo"), "{back}");
    let st = run(&mut s, "status");
    assert!(st.contains("recording on: 5 entries kept"), "{st}");
    let off = run(&mut s, "record stop");
    assert!(off.contains("recording ended"), "{off}");
    assert!(run(&mut s, "status").contains("recording off: 5 entries kept"));
    run(&mut s, "kill");
}

#[test]
fn back_reg_and_mem_refuse_what_they_cannot_answer() {
    let mut s = start();
    run(&mut s, "record");
    run(&mut s, "stepi 3");
    assert!(s.exec(&split_words("back --reg nosuch")).is_err());
    assert!(s.exec(&split_words("back --reg rax --mem 0x1000")).is_err());
    let none = run(&mut s, "back --mem 0x10");
    assert!(none.contains("none in the record"), "{none}");
    run(&mut s, "kill");
}
