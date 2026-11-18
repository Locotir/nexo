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

#[test]
fn rstepi_puts_registers_and_the_stack_back_and_refuses_what_it_cannot_undo() {
    let mut s = start();
    // off: refused, nothing to undo
    assert!(s.exec(&split_words("rstepi")).is_err());
    // the program starts stopped at its entry: plain instructions, no system call in the first few
    run(&mut s, "record 100");
    let regs0 = run(&mut s, "regs");
    let stack0 = run(&mut s, "mem $rsp-64 128");
    run(&mut s, "stepi 6");
    let regs6 = run(&mut s, "regs");
    let ask = s.exec(&split_words("rstepi 7")).unwrap_err().to_string();
    assert!(ask.contains("holds 6 instructions"), "{ask}");
    assert_eq!(run(&mut s, "regs"), regs6, "a refusal changed the registers");
    let out = run(&mut s, "rstepi 6");
    assert!(out.contains("undid 6 instructions"), "{out}");
    assert!(out.contains("never rewound"), "{out}");
    assert_eq!(run(&mut s, "regs"), regs0, "the registers did not come back");
    assert_eq!(run(&mut s, "mem $rsp-64 128"), stack0, "the stack did not come back");
    let st = run(&mut s, "status");
    assert!(st.contains("recording on: 0 entries kept"), "{st}");
    // forward again records again, and `rsi` is the gdb spelling
    run(&mut s, "stepi 2");
    assert!(run(&mut s, "rsi 2").contains("undid 2 instructions"));
    assert_eq!(run(&mut s, "regs"), regs0);
    // no breakpoint or watch: `rcont` goes back as far as the record goes and says so
    run(&mut s, "stepi 3");
    let rc = run(&mut s, "rc");
    assert!(rc.contains("undid 3 instructions") && rc.contains("the record ran out"), "{rc}");
    assert_eq!(run(&mut s, "regs"), regs0);
    run(&mut s, "kill");
}
