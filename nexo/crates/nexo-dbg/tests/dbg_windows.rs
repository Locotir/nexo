#![cfg(all(windows, target_arch = "x86_64"))]
//! The debugger against a real Windows program (examples/dbg_target.rs), through the Win32 Debug API.
//! The program is built by cargo as an example; a test skips with a message when it is not there.

use std::path::PathBuf;

use nexo_dbg::{open, split_words, Kind, Session, State};

fn target() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().unwrap().parent().unwrap().join("examples").join("dbg_target.exe")
}

/// Start the example in `mode`, or say why the test is skipped.
fn start(mode: &str) -> Option<(Session, String)> {
    if !target().exists() {
        eprintln!("skipped: {} was not built (cargo build --examples)", target().display());
        return None;
    }
    let (mut s, first) = open(&Kind::Run { exe: target(), args: vec![mode.to_string()] }).expect("start");
    let text = first.text_all();
    // a program built without names in its own file (a PDB-only build) cannot be broken by name yet
    if s.exec(&split_words("print dbg_add")).is_err() {
        eprintln!("skipped: the example carries no function names the debugger can read");
        let _ = s.exec(&split_words("kill"));
        return None;
    }
    Some((s, text))
}

/// The caller of `dbg_add`/`dbg_crash` is the Rust `main`, which only has a name when the file carries one. A
/// build whose names live in a .pdb (MSVC) shows it as `sub_<addr>+off`; nexo reads no .pdb yet, so then the
/// test checks the frame is a real address inside the program and says so, never a silent pass.
fn names_main(text: &str) -> bool {
    if text.contains("dbg_target::main") {
        return true;
    }
    eprintln!("note: main has no name in the file (PDB-only build); checking the frame by address");
    assert!(text.contains("sub_"), "no named caller and no sub_ frame either\n{text}");
    false
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

#[test]
fn starts_at_entry_and_kill_ends_it() {
    let Some((mut s, first)) = start("wait") else { return };
    assert!(first.contains("stopped at the program's entry"), "{first}");
    let st = run(&mut s, "status");
    assert!(st.contains("stopped"), "{st}");
    run(&mut s, "kill");
    assert_eq!(s.state, State::Exited);
}

#[test]
fn break_by_name_args_bt_step_finish_and_read_the_global() {
    let Some((mut s, _)) = start("loop") else { return };
    let b = run(&mut s, "break dbg_add");
    assert!(b.contains("breakpoint #1") && b.contains("dbg_add"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1") && stop.contains("dbg_add"), "{stop}");
    // the Windows calling convention: first two arguments in rcx and rdx
    let args = run(&mut s, "args");
    assert!(args.contains("arg1 rcx = 0x0") && args.contains("arg2 rdx = 0x1"), "{args}");
    let bt = run(&mut s, "bt");
    assert!(bt.lines().count() >= 2 && bt.contains("#1"), "{bt}");
    let st = run(&mut s, "step");
    assert!(st.contains("step"), "{st}");
    let fin = run(&mut s, "finish");
    assert!(fin.contains("finish"), "{fin}");
    let g = run(&mut s, "mem DBG_COUNTER 8 --as u64");
    assert!(g.contains("0x0") || g.contains(" 0"), "{g}");
    run(&mut s, "delete 1");
    let end = run(&mut s, "cont --wait 20");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(&mut s, "out");
    assert!(out.contains("done 15"), "{out}");
    assert_eq!(s.state, State::Exited);
}

#[test]
fn break_by_address_and_next_over_a_call() {
    let Some((mut s, _)) = start("loop") else { return };
    let addr = run(&mut s, "print dbg_add");
    let hex = addr.split_whitespace().find(|w| w.starts_with("0x")).unwrap_or_else(|| panic!("{addr}")).to_string();
    let b = run(&mut s, &format!("break {hex}"));
    assert!(b.contains("breakpoint #1"), "{b}");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1") && stop.contains("dbg_add"), "{stop}");
    let n = run(&mut s, "next");
    assert!(n.contains("next") || n.contains("step"), "{n}");
    run(&mut s, "kill");
}

#[test]
fn condition_and_hit_count() {
    let Some((mut s, _)) = start("loop") else { return };
    run(&mut s, "break dbg_add if rcx == 3");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("breakpoint #1"), "{stop}");
    let p = run(&mut s, "print rcx");
    assert!(p.contains("0x3"), "{p}");
    run(&mut s, "kill");
}

#[test]
fn registers_memory_expressions_and_writes() {
    let Some((mut s, _)) = start("loop") else { return };
    run(&mut s, "break dbg_add");
    run(&mut s, "cont");
    let r = run(&mut s, "regs");
    assert!(r.contains("rip") && r.contains("rsp") && r.contains("flags"), "{r}");
    let p = run(&mut s, "print rdx + 41");
    assert!(p.contains("0x2a"), "{p}");
    run(&mut s, "setreg rdx 100");
    let p = run(&mut s, "print rdx");
    assert!(p.contains("0x64"), "{p}");
    let d = run(&mut s, "dis");
    assert!(d.contains("=>"), "{d}");
    let m = run(&mut s, "maps");
    assert!(m.contains("dbg_target"), "{m}");
    run(&mut s, "delete all");
    let end = run(&mut s, "cont --wait 20");
    // rdx was 100 for the first call only: 0+100, then 1+1, 2+1, 3+1, 4+1
    assert!(end.contains("exited"), "{end}");
    assert!(run(&mut s, "out").contains("done 114"));
}

#[test]
fn logpoint_prints_and_the_program_goes_on() {
    let Some((mut s, _)) = start("loop") else { return };
    run(&mut s, "break dbg_add log a={rcx:d} b={rdx:d}");
    let end = run(&mut s, "cont --wait 20");
    assert!(end.contains("exited with code 0"), "{end}");
    let log = run(&mut s, "log");
    assert_eq!(log.lines().count(), 5, "{log}");
    assert!(log.contains("a=0 b=1") && log.contains("a=4 b=1"), "{log}");
}

#[test]
fn a_crash_is_explained_with_code_address_function_and_instruction() {
    let Some((mut s, _)) = start("crash") else { return };
    let stop = run(&mut s, "cont --wait 20");
    assert!(stop.contains("0xc0000005") || stop.contains("0xC0000005") || stop.contains("ACCESS_VIOLATION"), "{stop}");
    assert!(stop.contains("null pointer"), "{stop}");
    assert!(stop.contains("while reading"), "{stop}");
    assert!(stop.contains("dbg_crash"), "{stop}");
    assert!(stop.contains("backtrace:"), "{stop}");
    let pc = stop.lines().find(|l| l.starts_with("pc ")).unwrap_or_default();
    assert!(pc.contains("dbg_crash"), "{stop}");
    run(&mut s, "kill");
}

#[test]
fn stdin_goes_in_and_stdout_comes_back() {
    let Some((mut s, _)) = start("echo") else { return };
    run(&mut s, "stdin hello there");
    run(&mut s, "stdin --close");
    let end = run(&mut s, "cont --wait 20");
    assert!(end.contains("exited with code 0"), "{end}");
    let out = run(&mut s, "out --all");
    assert!(out.contains("echo: hello there"), "{out}");
}

#[test]
fn a_script_runs_several_commands_in_one_call() {
    let Some((mut s, _)) = start("loop") else { return };
    let o = run(&mut s, "do \"break dbg_add; cont; args; bt 2; kill\"");
    assert!(o.contains("> break dbg_add") && o.contains("arg1 rcx") && o.contains("program killed"), "{o}");
}

#[test]
fn threads_are_listed() {
    let Some((mut s, _)) = start("thread") else { return };
    run(&mut s, "break dbg_add");
    run(&mut s, "cont --wait 20");
    let t = run(&mut s, "threads");
    assert!(t.contains("thread"), "{t}");
    run(&mut s, "kill");
}

#[test]
fn watch_stops_on_the_write() {
    let Some((mut s, _)) = start("loop") else { return };
    let w = run(&mut s, "watch DBG_COUNTER 8 w");
    assert!(w.contains("watch #1"), "{w}");
    let stop = run(&mut s, "cont --wait 20");
    assert!(stop.contains("watch #1"), "{stop}");
    run(&mut s, "kill");
}

/// True when no process has this id any more (or it has ended).
fn gone(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION};
    for _ in 0..50 {
        // SAFETY: opens a handle for a read-only question and closes it.
        let alive = unsafe {
            let h = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if h.is_null() {
                false
            } else {
                let mut code = 0u32;
                let ok = GetExitCodeProcess(h, &mut code) != 0 && code == 259;
                CloseHandle(h);
                ok
            }
        };
        if !alive {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[test]
fn killing_and_dropping_leave_no_process() {
    let Some((mut s, _)) = start("wait") else { return };
    let pid = s.t.pid();
    run(&mut s, "kill");
    drop(s);
    assert!(gone(pid), "process {pid} is still there after kill");
    let Some((s, _)) = start("wait") else { return };
    let pid = s.t.pid();
    drop(s);
    assert!(gone(pid), "process {pid} is still there after the session was dropped");
}

#[test]
fn finish_returns_to_the_caller_through_the_unwind_tables() {
    let Some((mut s, _)) = start("loop") else { return };
    run(&mut s, "break dbg_add");
    run(&mut s, "cont --wait 20");
    let bt = run(&mut s, "bt");
    let named = names_main(&bt);
    let fin = run(&mut s, "finish");
    assert!(fin.contains("finish: returned"), "{fin}");
    assert!(!named || fin.contains("dbg_target::main"), "{fin}");
    run(&mut s, "kill");
}

#[test]
fn an_exception_nobody_handles_ends_the_program_with_its_code() {
    let Some((mut s, _)) = start("crash") else { return };
    let first = run(&mut s, "cont --wait 20");
    assert!(first.contains("ACCESS_VIOLATION (exception 0xc0000005)"), "{first}");
    // a second go hands the exception to the program, which has no handler: the system ends it
    let second = run(&mut s, "cont --wait 20");
    assert!(second.contains("not handled by the program"), "{second}");
    let end = run(&mut s, "cont --wait 20");
    assert!(end.contains("exited with code -1073741819"), "{end}");
}

#[test]
fn catch_takes_exception_names_and_codes() {
    let Some((mut s, _)) = start("wait") else { return };
    let c = run(&mut s, "catch add stack_overflow 0xc0000096 status_integer_divide_by_zero");
    assert!(c.contains("STACK_OVERFLOW") && c.contains("PRIVILEGED_INSTRUCTION"), "{c}");
    let c = run(&mut s, "catch del access_violation");
    assert!(!c.contains("ACCESS_VIOLATION"), "{c}");
    assert!(s.exec(&split_words("catch add nosuchthing")).is_err());
    run(&mut s, "kill");
}

#[test]
fn attach_stops_a_running_program_and_detach_lets_it_go() {
    if !target().exists() {
        eprintln!("skipped: {} was not built (cargo build --examples)", target().display());
        return;
    }
    let mut child = std::process::Command::new(target()).arg("attach").spawn().unwrap();
    let pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let (mut s, first) = open(&Kind::Attach(pid)).expect("attach");
    assert!(first.text_all().contains("attached to pid"), "{}", first.text_all());
    let r = run(&mut s, "regs rip");
    assert!(r.contains("rip ="), "{r}");
    run(&mut s, "detach");
    drop(s);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!gone(pid), "the program ended with the detach");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(gone(pid));
}

/// Write a minidump of the stopped program with the system's own writer, as the crash reporter does.
/// `None` (with a message) when the writer refuses, which some Windows-on-Linux layers do.
fn dump_of(s: &mut Session, file: &std::path::Path) -> Option<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL};
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, THREAD_ALL_ACCESS,
    };
    let (pid, tid) = (s.t.pid(), s.t.threads()[0]);
    // SAFETY: plain Win32 calls on handles opened here and closed below; the structures are local.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        let thread = OpenThread(THREAD_ALL_ACCESS, 0, tid);
        let wide: Vec<u16> = file.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let out = CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            std::ptr::null(),
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        let mut ctx: Box<CONTEXT> = Box::new(std::mem::zeroed());
        ctx.ContextFlags = CONTEXT_ALL_AMD64;
        let got = !thread.is_null() && GetThreadContext(thread, &mut *ctx) != 0;
        let mut rec: EXCEPTION_RECORD = std::mem::zeroed();
        rec.ExceptionCode = 0xC000_0005u32 as i32;
        rec.ExceptionAddress = ctx.Rip as *mut _;
        rec.NumberParameters = 2;
        let mut ptrs = EXCEPTION_POINTERS { ExceptionRecord: &mut rec, ContextRecord: &mut *ctx };
        let info = MINIDUMP_EXCEPTION_INFORMATION { ThreadId: tid, ExceptionPointers: &mut ptrs, ClientPointers: 0 };
        let ok = got
            && !process.is_null()
            && out as isize != -1
            && MiniDumpWriteDump(process, pid, out, MiniDumpNormal, &info, std::ptr::null(), std::ptr::null()) != 0;
        for h in [process, thread, out] {
            if !h.is_null() && h as isize != -1 {
                CloseHandle(h);
            }
        }
        if !ok {
            eprintln!("skipped: this system could not write a minidump");
        }
        ok.then_some(())
    }
}

#[test]
fn a_minidump_of_a_crash_is_read_with_cause_function_and_stack() {
    let Some((mut s, _)) = start("crash") else { return };
    let stop = run(&mut s, "cont --wait 20");
    assert!(stop.contains("ACCESS_VIOLATION"), "{stop}");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("crash.dmp");
    if dump_of(&mut s, &file).is_none() {
        return;
    }
    run(&mut s, "kill");
    let (mut d, first) = open(&Kind::Core { file: file.clone(), exe: Some(target()) }).expect("open the dump");
    let first = first.text_all();
    assert!(first.contains("ACCESS_VIOLATION"), "{first}");
    assert!(first.contains("null pointer") && first.contains("dbg_crash"), "{first}");
    assert!(first.contains("backtrace:"), "{first}");
    names_main(&first);
    // a dump is read, never run
    let err = d.exec(&split_words("cont")).map(|o| o.text_all()).unwrap_or_else(|e| e.to_string());
    assert!(err.contains("minidump") || err.contains("not running") || err.contains("cannot"), "{err}");
    let mods = run(&mut d, "modules");
    assert!(mods.contains("dbg_target.exe"), "{mods}");
    let regs = run(&mut d, "regs rip");
    assert!(regs.contains("rip ="), "{regs}");
}
