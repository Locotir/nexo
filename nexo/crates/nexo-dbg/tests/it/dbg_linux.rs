#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The debugger against a real program (examples/dbg_target.rs), in this process.

use std::time::Duration;

use nexo_dbg::{open, split_words, Kind, Session, State};

mod common;
use common::target;

fn start(mode: &str) -> (Session, String) {
    let (s, first) = open(&Kind::Run { exe: target(), args: vec![mode.to_string()] }).expect("start");
    let text = first.text_all();
    (s, text)
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn gone(pid: u32) -> bool {
    // a killed child is reaped by the debugger; give the kernel a moment
    for _ in 0..50 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn starts_at_entry_and_leaves_no_process() {
    let (mut s, first) = start("wait");
    assert!(first.contains("stopped at the program's entry"), "{first}");
    let pid = s.t.pid();
    let st = run(&mut s, "status");
    assert!(st.contains("stopped"), "{st}");
    run(&mut s, "kill");
    assert_eq!(s.state, State::Exited);
    drop(s);
    assert!(gone(pid), "process {pid} is still there");
}

#[test]
fn dropping_a_session_kills_the_program() {
    let (s, _) = start("wait");
    let pid = s.t.pid();
    drop(s);
    assert!(gone(pid), "process {pid} is still there");
}

#[test]
fn break_by_name_args_bt_step_finish_and_read_the_global() {
    let (mut s, _) = start("loop");
    let b = run(&mut s, "break dbg_add");
    assert!(b.contains("breakpoint #1") && b.contains("dbg_add"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1") && stop.contains("dbg_add"), "{stop}");
    let args = run(&mut s, "args");
    assert!(args.contains("arg1 rdi = 0x0") && args.contains("arg2 rsi = 0x1"), "{args}");
    let bt = run(&mut s, "bt");
    assert!(bt.lines().count() >= 2 && bt.contains("#1"), "{bt}");
    let st = run(&mut s, "step");
    assert!(st.contains("step"), "{st}");
    let fin = run(&mut s, "finish");
    assert!(fin.contains("finish"), "{fin}");
    // the first call has not stored its result yet: the counter is 0, then 1 after the next call
    let g = run(&mut s, "mem DBG_COUNTER 8 --as u64");
    assert!(g.contains("0x0") || g.contains(" 0"), "{g}");
    run(&mut s, "delete 1");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(&mut s, "out");
    assert!(out.contains("done 15"), "{out}");
    assert_eq!(s.state, State::Exited);
}

#[test]
fn condition_and_hit_count() {
    let (mut s, _) = start("loop");
    run(&mut s, "break dbg_add if rdi == 3");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1"), "{stop}");
    let p = run(&mut s, "print rdi");
    assert!(p.contains("0x3"), "{p}");
    run(&mut s, "kill");
    let (mut s, _) = start("loop");
    run(&mut s, "break dbg_add after 3");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1"), "{stop}");
    let p = run(&mut s, "print rdi");
    assert!(p.contains("0x3"), "{p}");
    run(&mut s, "kill");
}

#[test]
fn logpoint_prints_and_the_program_goes_on() {
    let (mut s, _) = start("loop");
    run(&mut s, "break dbg_add log a={rdi:d} b={rsi:d}");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let log = run(&mut s, "log");
    assert_eq!(log.lines().count(), 5, "{log}");
    assert!(log.contains("a=0 b=1") && log.contains("a=4 b=1"), "{log}");
}

#[test]
fn watch_stops_on_the_write_and_shows_old_and_new() {
    let (mut s, _) = start("loop");
    let w = run(&mut s, "watch DBG_COUNTER 8 w");
    assert!(w.contains("watch #1"), "{w}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("watch #1") && stop.contains("0000000000000000 -> 0000000000000001"), "{stop}");
    run(&mut s, "kill");
}

#[test]
fn a_crash_is_explained() {
    let (mut s, _) = start("crash");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("SIGSEGV"), "{stop}");
    assert!(stop.contains("null pointer"), "{stop}");
    assert!(stop.contains("backtrace:") && stop.contains("dbg_crash"), "{stop}");
    // the source line comes from the program's own debug info: the pc line and the first frame name it
    let at =
        |text: &str| text.lines().find(|l| l.contains("dbg_crash") && l.contains("dbg_target.rs:")).map(str::to_string);
    let pc = stop.lines().find(|l| l.starts_with("pc ")).unwrap_or_default();
    assert!(pc.contains("(examples/dbg_target.rs:"), "{stop}");
    assert!(at(&stop).is_some(), "{stop}");
    let line: u32 = pc
        .split_once("dbg_target.rs:")
        .unwrap()
        .1
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap();
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/dbg_target.rs")).unwrap();
    assert!(src.lines().nth(line as usize - 1).unwrap().contains("the faulting load"), "line {line}: {pc}");
    run(&mut s, "kill");
}

#[test]
fn stdin_goes_in_and_stdout_comes_back() {
    let (mut s, _) = start("echo");
    run(&mut s, "stdin hello there");
    run(&mut s, "stdin --close");
    let end = run(&mut s, "cont --wait 10");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(&mut s, "out --all");
    assert!(out.contains("echo: hello there"), "{out}");
}

#[test]
fn registers_memory_expressions_and_writes() {
    let (mut s, _) = start("loop");
    run(&mut s, "break dbg_add");
    run(&mut s, "cont");
    let r = run(&mut s, "regs");
    assert!(r.contains("rip") && r.contains("rsp") && r.contains("flags"), "{r}");
    let p = run(&mut s, "print rsi + 41");
    assert!(p.contains("0x2a"), "{p}");
    run(&mut s, "setreg rsi 100");
    let p = run(&mut s, "print rsi");
    assert!(p.contains("0x64"), "{p}");
    let d = run(&mut s, "dis");
    assert!(d.contains("=>"), "{d}");
    let m = run(&mut s, "maps");
    assert!(m.contains("[stack]"), "{m}");
    run(&mut s, "delete all");
    let end = run(&mut s, "cont");
    // rsi was 100 for the first call only: 0+100, then 1+1, 2+1, 3+1, 4+1
    assert!(end.contains("exited"), "{end}");
    assert!(run(&mut s, "out").contains("done 114"));
}

#[test]
fn a_script_runs_several_commands_in_one_call() {
    let (mut s, _) = start("loop");
    let o = run(&mut s, "do \"break dbg_add; cont; args; bt 2; kill\"");
    assert!(o.contains("> break dbg_add") && o.contains("arg1 rdi") && o.contains("program killed"), "{o}");
    let o = run(&mut s, "do \"status; nosuchcommand; status\"");
    assert!(o.contains("script stopped at the error"), "{o}");
}

#[test]
fn threads_are_listed() {
    let (mut s, _) = start("thread");
    run(&mut s, "break dbg_add");
    run(&mut s, "cont");
    let t = run(&mut s, "threads");
    assert!(t.contains("thread"), "{t}");
    run(&mut s, "kill");
}

#[test]
fn attach_stops_a_running_program_and_detach_lets_it_go() {
    let mut child = std::process::Command::new(target()).arg("attach").spawn().unwrap();
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(300));
    let (mut s, first) = open(&Kind::Attach(pid)).expect("attach");
    assert!(first.text_all().contains("attached to pid"), "{}", first.text_all());
    let r = run(&mut s, "regs rip");
    assert!(r.contains("rip ="), "{r}");
    run(&mut s, "detach");
    drop(s);
    std::thread::sleep(Duration::from_millis(100));
    // still alive after the detach
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(gone(pid));
}
