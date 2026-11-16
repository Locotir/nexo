#![cfg(all(target_os = "macos", target_arch = "x86_64"))]
//! The debugger against a real program on macOS (examples/dbg_target.rs, and a small C file built with `cc`), in
//! this process: the tests of dbg_linux.rs, the same commands and the same answers. `run` debugs a copy of the
//! program signed with get-task-allow, so these tests need no Developer Mode, no root and no password.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nexo_dbg::{open, split_words, Kind, Session, State};

use crate::common;
use common::target;

fn start(mode: &str) -> (Session, String) {
    let (s, first) = open(&Kind::Run { exe: target(), args: vec![mode.to_string()] }).expect("start");
    let text = first.text_all();
    (s, text)
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process is there.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn gone(pid: u32) -> bool {
    // a killed child is reaped by the debugger; give the kernel a moment
    for _ in 0..50 {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// The signed copies `run` left beside a program.
fn copies(exe: &Path) -> Vec<PathBuf> {
    let name = exe.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::read_dir(exe.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|f| f.to_string_lossy().starts_with(&format!(".{name}.nexo-dbg-{}", std::process::id())))
        })
        .collect()
}

#[test]
fn starts_at_entry_and_leaves_no_process() {
    let (mut s, first) = start("wait");
    assert!(first.contains("stopped at the program's entry: dyld's call of main"), "{first}");
    let pid = s.t.pid();
    let st = run(&mut s, "status");
    assert!(st.contains("stopped"), "{st}");
    run(&mut s, "kill");
    assert_eq!(s.state, State::Exited);
    drop(s);
    assert!(gone(pid), "process {pid} is still there");
}

#[test]
fn the_banner_says_a_signed_copy_runs_and_the_copy_goes_with_the_session() {
    // its own program: copies of other tests' sessions are beside it too, so this one looks for its own name
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("own_target");
    std::fs::copy(target(), &exe).unwrap();
    let (s, first) = open(&Kind::Run { exe: exe.clone(), args: vec!["wait".into()] }).expect("start");
    let first = first.text_all();
    assert!(first.contains("signed ad hoc with get-task-allow"), "{first}");
    assert!(first.contains(&exe.display().to_string()), "{first}");
    assert!(first.contains("your own files only"), "{first}");
    assert_eq!(copies(&exe).len(), 1, "{first}");
    let pid = s.t.pid();
    drop(s);
    assert!(gone(pid), "process {pid} is still there");
    assert!(copies(&exe).is_empty(), "the copy is still there");
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
    // stopped on the first instruction, before its frame is made: the caller is still main
    let bt = run(&mut s, "bt");
    assert!(bt.lines().any(|l| l.trim_start().starts_with("#1") && l.contains("main")), "{bt}");
    let st = run(&mut s, "step");
    assert!(st.contains("step"), "{st}");
    let fin = run(&mut s, "finish");
    assert!(fin.contains("finish") && fin.contains("main"), "{fin}");
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
    assert!(stop.contains("EXC_BAD_ACCESS"), "{stop}");
    assert!(stop.contains("null pointer"), "{stop}");
    assert!(stop.contains("backtrace:") && stop.contains("dbg_crash"), "{stop}");
    // Linux also checks the source line of the fault here. On macOS the line table sits in the object files the
    // executable's debug map names (or a .dSYM), which nexo-bin does not read yet, so that part
    // is not checked on this system.
    // Rust's own SIGSEGV handler (its stack-overflow check) puts back the default action and returns, so the load
    // runs again and faults a second time, as on Linux; the cont after that ends the program.
    let again = run(&mut s, "cont");
    assert!(again.contains("SIGSEGV") && again.contains("dbg_crash"), "{again}");
    let end = run(&mut s, "cont");
    assert!(end.contains("killed by SIGSEGV"), "{end}");
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
    assert!(t.lines().filter(|l| l.contains("thread")).count() >= 2, "{t}");
    run(&mut s, "kill");
}

/// A copy of the test program in `dir`, signed (as `run` signs) to allow debugging, or left unsigned.
fn program_copy(dir: &Path, signed: bool) -> PathBuf {
    let exe = dir.join(if signed { "allows" } else { "plain" });
    let data = std::fs::read(target()).unwrap();
    let bytes = if signed { nexo_bin::adhoc_sign(&data, "allows", &[nexo_bin::GET_TASK_ALLOW]).unwrap() } else { data };
    std::fs::write(&exe, bytes).unwrap();
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    exe
}

#[test]
fn attach_stops_a_running_program_and_detach_lets_it_go() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(program_copy(dir.path(), true)).arg("attach").spawn().unwrap();
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
    assert!(alive(pid));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(gone(pid));
}

#[test]
fn attach_to_a_program_that_does_not_allow_it_says_what_macos_allows() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(program_copy(dir.path(), false)).arg("attach").spawn().unwrap();
    let pid = child.id();
    std::thread::sleep(Duration::from_millis(300));
    let e = open(&Kind::Attach(pid)).err().expect("macOS refuses it").to_string();
    assert!(e.contains("macOS lets nexo open only programs that allow debugging"), "{e}");
    assert!(e.contains("nexo dbg run") && e.contains("DevToolsSecurity -enable"), "{e}");
    assert!(!e.contains("sudo"), "{e}");
    assert!(alive(pid), "the refused attach left the program running");
    child.kill().unwrap();
    child.wait().unwrap();
    let e = open(&Kind::Attach(1)).err().expect("launchd is not ours").to_string();
    assert!(e.contains("belongs to another user"), "{e}");
}

#[test]
fn a_c_program_stops_in_main_steps_takes_a_memory_write_and_exits_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("seven.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\nint code = 3;\nint main(void) { printf(\"code %d\\n\", code); return code; }\n",
    )
    .unwrap();
    let exe = dir.path().join("seven");
    let built = std::process::Command::new("cc").args(["-O0", "-o"]).arg(&exe).arg(&src).status();
    if !built.is_ok_and(|s| s.success()) {
        eprintln!("skipped: no C compiler");
        return;
    }
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let b = run(&mut s, "break main");
    assert!(b.contains("breakpoint #1"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1") && stop.contains("main"), "{stop}");
    let rip = run(&mut s, "regs rip");
    assert!(rip.contains("rip = 0x"), "{rip}");
    let st = run(&mut s, "step");
    assert!(st.contains("step"), "{st}");
    assert_ne!(run(&mut s, "regs rip"), rip, "step moved rip");
    let w = run(&mut s, "write code 07 00 00 00");
    assert!(w.contains("wrote 4 bytes"), "{w}");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 7"), "{end}");
    assert!(run(&mut s, "out").contains("code 7"));
}
