#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! The GDB remote protocol, both sides: nexo's client against nexo's own stub (examples/dbg_target.rs), and
//! the client against gdbserver and the stub against gdb when those are installed.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;

use nexo_dbg::{open, serve_gdb, split_words, Kind, Session, State};

fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join("examples").join("dbg_target")
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// nexo's stub on a thread, running the fixture in `mode`; the address it listens on.
fn stub(mode: &str) -> (String, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let kind = Kind::Run { exe: target(), args: vec![mode.to_string()] };
    let h = std::thread::spawn(move || {
        let r = serve_gdb(&kind, 0, &mut |a| {
            let _ = tx.send(a.to_string());
        });
        assert!(r.is_ok(), "{r:?}");
    });
    let addr = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("the stub did not come up");
    (addr, h)
}

fn connect(addr: &str) -> (Session, String) {
    let (s, first) = open(&Kind::Remote { addr: addr.to_string(), exe: Some(target()) }).expect("connect");
    let text = first.text_all();
    (s, text)
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

#[test]
fn the_client_drives_nexos_own_stub_from_break_to_exit() {
    let (addr, stub) = stub("loop");
    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    let (mut s, first) = connect(&addr);
    assert!(first.contains("connected to the stub at"), "{first}");
    let regs = run(&mut s, "regs");
    assert!(regs.contains("rip") && regs.contains("rsp"), "{regs}");
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
    let g = run(&mut s, "mem DBG_COUNTER 8 --as u64");
    assert!(g.contains("0x0") || g.contains(" 0"), "{g}");
    run(&mut s, "delete 1");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(&mut s, "out");
    assert!(out.contains("done 15"), "{out}");
    assert_eq!(s.state, State::Exited);
    drop(s);
    stub.join().unwrap();
}

#[test]
fn a_condition_stops_where_the_register_says_over_the_wire() {
    let (addr, stub) = stub("loop");
    let (mut s, _) = connect(&addr);
    run(&mut s, "break dbg_add if rdi == 3");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1"), "{stop}");
    let p = run(&mut s, "print rdi");
    assert!(p.contains("0x3"), "{p}");
    run(&mut s, "kill");
    drop(s);
    stub.join().unwrap();
}

#[test]
fn a_write_and_a_register_change_go_through_the_stub() {
    let (addr, stub) = stub("loop");
    let (mut s, _) = connect(&addr);
    run(&mut s, "break dbg_add");
    run(&mut s, "cont");
    run(&mut s, "setreg rdi 0x40");
    let p = run(&mut s, "print rdi");
    assert!(p.contains("0x40"), "{p}");
    run(&mut s, "write DBG_COUNTER 07 00 00 00 00 00 00 00");
    let g = run(&mut s, "mem DBG_COUNTER 8 --as u64");
    assert!(g.contains("0000000000000007"), "{g}");
    run(&mut s, "kill");
    drop(s);
    stub.join().unwrap();
}

#[test]
fn a_hardware_watch_stops_on_the_write() {
    let (addr, stub) = stub("loop");
    let (mut s, _) = connect(&addr);
    let w = run(&mut s, "watch DBG_COUNTER 8 w");
    assert!(w.contains("watch #1"), "{w}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("watch #1") && stop.contains("0000000000000000 -> 0000000000000001"), "{stop}");
    run(&mut s, "kill");
    drop(s);
    stub.join().unwrap();
}

#[test]
fn a_crash_behind_the_stub_is_reported_with_its_signal_and_stack() {
    let (addr, stub) = stub("crash");
    let (mut s, _) = connect(&addr);
    let stop = run(&mut s, "cont");
    assert!(stop.contains("SIGSEGV"), "{stop}");
    assert!(stop.contains("backtrace:") && stop.contains("dbg_crash"), "{stop}");
    run(&mut s, "kill");
    drop(s);
    stub.join().unwrap();
}

#[test]
fn stop_interrupts_a_running_program_behind_the_stub() {
    let (addr, stub) = stub("wait");
    let (mut s, _) = connect(&addr);
    s.exec(&split_words("cont --wait 1")).unwrap();
    assert_eq!(s.state, State::Running);
    let st = s.stop_now().unwrap().text_all();
    assert!(st.contains("interrupt") || st.contains("stopped"), "{st}");
    assert_eq!(s.state, State::Stopped);
    run(&mut s, "kill");
    drop(s);
    stub.join().unwrap();
}

#[test]
fn a_remote_that_is_not_there_says_so() {
    let r = open(&Kind::Remote { addr: "127.0.0.1:1".into(), exe: Some(target()) });
    let e = r.err().expect("no stub on port 1");
    assert!(e.to_string().contains("cannot connect"), "{e}");
}

/// gdbserver, when installed, running the fixture; its address.
fn gdbserver(mode: &str) -> Option<(String, std::process::Child)> {
    if !have("gdbserver") {
        eprintln!("skipped: gdbserver is not installed");
        return None;
    }
    let mut child = Command::new("gdbserver")
        .args(["--once", "127.0.0.1:0"])
        .arg(target())
        .arg(mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut rd = BufReader::new(child.stderr.take()?);
    let mut line = String::new();
    for _ in 0..20 {
        line.clear();
        if rd.read_line(&mut line).ok()? == 0 {
            break;
        }
        if let Some(p) = line.trim().strip_prefix("Listening on port ") {
            // keep draining so gdbserver never blocks on a full pipe
            std::thread::spawn(move || {
                let mut sink = String::new();
                while rd.read_line(&mut sink).unwrap_or(0) > 0 {
                    sink.clear();
                }
            });
            return Some((format!("127.0.0.1:{}", p.trim()), child));
        }
    }
    let _ = child.kill();
    eprintln!("skipped: gdbserver could not start the fixture here");
    None
}

#[test]
fn the_client_drives_gdbserver_when_it_is_installed() {
    let Some((addr, mut child)) = gdbserver("loop") else { return };
    let (mut s, first) = connect(&addr);
    assert!(first.contains("connected to the stub at"), "{first}");
    let b = run(&mut s, "break dbg_add");
    assert!(b.contains("breakpoint #1"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1") && stop.contains("dbg_add"), "{stop}");
    let args = run(&mut s, "args");
    assert!(args.contains("arg1 rdi = 0x0") && args.contains("arg2 rsi = 0x1"), "{args}");
    let bt = run(&mut s, "bt");
    assert!(bt.contains("#1"), "{bt}");
    run(&mut s, "step");
    run(&mut s, "delete 1");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    drop(s);
    let _ = child.wait();
}

#[test]
fn gdb_can_drive_nexos_stub_when_it_is_installed() {
    if !have("gdb") {
        eprintln!("skipped: gdb is not installed");
        return;
    }
    let (addr, stub) = stub("loop");
    let out = Command::new("gdb")
        .args(["-nx", "-batch", "-ex", &format!("target remote {addr}"), "-ex", "break dbg_add", "-ex", "continue"])
        .args(["-ex", "info registers rip", "-ex", "bt 2", "-ex", "kill"])
        .arg(target())
        .output()
        .expect("gdb");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("dbg_add"), "{text}");
    assert!(text.contains("Breakpoint 1,"), "{text}");
    assert!(text.contains("rip"), "{text}");
    stub.join().unwrap();
}
