//! Linux x86-64 through ptrace. Every thread of the program is a tracee of the thread that made
//! the [`Linux`] value, so every call must come from that thread.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::time::{Duration, Instant};

use libc::{c_long, c_void};

use crate::platform::{
    Access, Event, Fault, Follow, Forked, HwSlot, Launch, Map, Module, Os, Resume, Stop, Sys, Target, Tid,
};
use crate::regs::Regs;
use crate::{fail, Error, Result};

mod forks;
mod target;

const PEEKDATA: c_long = 2;
const PEEKUSER: c_long = 3;
const POKEDATA: c_long = 5;
const POKEUSER: c_long = 6;
const CONT: c_long = 7;
const SINGLESTEP: c_long = 9;
const SYSCALL: c_long = 24;
const GETREGS: c_long = 12;
const SETREGS: c_long = 13;
const GETFPREGS: c_long = 14;
const SETFPREGS: c_long = 15;
const GETREGSET: c_long = 0x4204;
const SETREGSET: c_long = 0x4205;
const NT_X86_XSTATE: u64 = 0x202;
/// Room for the whole XSAVE area: x87, SSE, AVX, AVX-512 and AMX tiles fit in 16 KiB.
const XSTATE_ROOM: usize = 16 << 10;
const DETACH: c_long = 17;
const GETEVENTMSG: c_long = 0x4201;
const GETSIGINFO: c_long = 0x4202;
const SEIZE: c_long = 0x4206;
const INTERRUPT: c_long = 0x4207;
const GET_SYSCALL_INFO: c_long = 0x420e;

const O_TRACESYSGOOD: u64 = 0x1;
const O_TRACEFORK: u64 = 0x2;
const O_TRACEVFORK: u64 = 0x4;
const O_TRACECLONE: u64 = 0x8;
const O_TRACEEXEC: u64 = 0x10;
const O_EXITKILL: u64 = 0x10_0000;

const EVENT_FORK: i32 = 1;
const EVENT_VFORK: i32 = 2;
const EVENT_CLONE: i32 = 3;
const EVENT_EXEC: i32 = 4;
const EVENT_STOP: i32 = 128;

/// Where `u_debugreg` sits in the kernel's `struct user`.
const DEBUGREG: u64 = 848;
const AT_ENTRY: u64 = 9;
const ADDR_NO_RANDOMIZE: libc::c_ulong = 0x004_0000;

fn pt(req: c_long, tid: i32, addr: u64, data: u64) -> io::Result<i64> {
    // SAFETY: a plain system call; where `data` is a pointer the caller keeps the buffer alive.
    let r = unsafe { libc::syscall(libc::SYS_ptrace, req, tid as c_long, addr as c_long, data as c_long) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// The status of `tid` if it changed; `None` with `WNOHANG` when it did not.
fn waitpid(tid: i32, flags: i32) -> io::Result<Option<i32>> {
    loop {
        let mut st = 0;
        // SAFETY: `st` is a valid place for the status.
        let r = unsafe { libc::waitpid(tid, &mut st, flags | libc::__WALL) };
        if r == tid {
            return Ok(Some(st));
        }
        if r == 0 {
            return Ok(None);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

#[derive(Debug, Default)]
struct Th {
    running: bool,
    /// The signal it stopped on, handed back when it runs again.
    sig: i32,
    /// The last order was one instruction, not "run".
    step: bool,
    /// Born while we were not looking: the watch slots are not on it yet.
    fresh: bool,
    /// It made a vfork child that has not exec'd or exited yet: it stays at that stop until then (see `Vkid`).
    held: bool,
    /// Held, and the last resume would have let it go: it goes when its child execs or exits.
    go: bool,
}

/// A vfork child, run by the parent's target until it execs or exits. It shares the parent's memory, breakpoint
/// bytes included, and the parent thread that made it waits in the kernel where `PTRACE_INTERRUPT` does not reach
/// it; so that thread is held at its vfork stop (it never waits in the kernel), and the child is stepped past our
/// breakpoints here. At its exec it is kept as a target of its own (`follow child|both`) or let go.
struct Vkid {
    pid: i32,
    /// The parent thread, held at its vfork stop.
    parent: i32,
    at: u64,
    /// The breakpoint address the child is stepping past with the program's byte in place: 0xcc goes back after.
    over: Option<u64>,
    /// The guard (see `Linux::guards`) opened for one instruction of the child: it closes again after the step.
    open: Option<(u64, u64, u8, u8)>,
}

pub struct Linux {
    pid: i32,
    th: BTreeMap<i32, Th>,
    mem: Option<File>,
    launched: bool,
    stdin: Option<File>,
    out: Option<File>,
    watch: [Option<HwSlot>; 4],
    patches: Vec<(u64, u8)>,
    /// The guarded pages of the page watches (start, length, own access, guard access): a child forked while they
    /// are guarded gets its copy's own access back.
    guards: Vec<(u64, u64, u8, u8)>,
    /// The program ended or was let go.
    gone: bool,
    want_stop: bool,
    /// Stop at system calls (`PTRACE_SYSCALL` instead of `PTRACE_CONT`).
    sys: bool,
    /// Which side of a fork is kept (`follow`).
    follow: Follow,
    /// The children forked since `Target::forks` last took them.
    forks: Vec<Forked>,
    /// The vfork children that have not exec'd or exited yet.
    vkids: Vec<Vkid>,
}

/// The ptrace order that lets a thread go: one instruction, up to the next system call, or on.
fn order(step: bool, sys: bool) -> c_long {
    if step {
        SINGLESTEP
    } else if sys {
        SYSCALL
    } else {
        CONT
    }
}

/// What the kernel says about the system call `tid` is stopped in (`PTRACE_GET_SYSCALL_INFO`).
fn syscall_info(tid: i32) -> Option<Sys> {
    let mut b = [0u64; 12];
    let n = pt(GET_SYSCALL_INFO, tid, std::mem::size_of_val(&b) as u64, b.as_mut_ptr() as u64).ok()?;
    if n < 24 {
        return None;
    }
    match b[0] & 0xff {
        1 => Some(Sys { entry: true, nr: b[3], args: [b[4], b[5], b[6], b[7], b[8], b[9]], ret: 0, error: false }),
        2 => {
            // the kernel does not give the number again on the way out: it is still in orig_rax
            let mut nr = u64::MAX;
            let _ = pt(PEEKUSER, tid, 15 * 8, (&raw mut nr) as u64);
            Some(Sys { entry: false, nr, args: [0; 6], ret: b[3] as i64, error: b[4] & 0xff != 0 })
        }
        _ => None,
    }
}

/// Where stopped thread `tid` stands, for an instruction of ours to run there: `Ok(false)` on the way back to its
/// code (a signal, a step, a breakpoint, a job-control stop, the end of a system call); `Ok(true)` inside a fork,
/// clone or exec, which one step ends first; `Err` where one step would run the thread's own system call (it may
/// block) or, with the call number taken out, skip it, or where it waits for a vfork child.
fn inside_call(tid: i32) -> std::result::Result<bool, String> {
    let mut info = [0u64; 16];
    if let Err(e) = pt(GETSIGINFO, tid, 0, info.as_mut_ptr() as u64) {
        return Err(format!("the stop of thread {tid} cannot be read ({e})"));
    }
    let (signo, code) = (info[0] as u32 as i32, (info[1] & 0xffff_ffff) as u32 as i32);
    if signo != libc::SIGTRAP {
        return Ok(false);
    }
    if code == libc::SIGTRAP | 0x80 {
        return match syscall_info(tid) {
            Some(s) if s.entry => Err(format!("thread {tid} stands at the start of system call {}, not run yet", s.nr)),
            _ => Ok(false),
        };
    }
    match code >> 8 {
        EVENT_FORK | EVENT_CLONE | EVENT_EXEC => Ok(true),
        EVENT_VFORK => Err(format!("thread {tid} waits in vfork for its child to exec or exit")),
        _ => Ok(false),
    }
}

/// Put `bytes` at `addr` through stopped thread `tid` and give back the bytes that were there, exactly as they
/// were (a breakpoint's 0xcc included).
fn swap_bytes(tid: i32, addr: u64, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut old = Vec::with_capacity(bytes.len());
    for (a, &b) in (addr..).zip(bytes) {
        let base = a & !7;
        let mut w = 0u64;
        pt(PEEKDATA, tid, base, (&raw mut w) as u64)?;
        let mut word = w.to_le_bytes();
        old.push(word[(a - base) as usize]);
        word[(a - base) as usize] = b;
        pt(POKEDATA, tid, base, u64::from_le_bytes(word))?;
    }
    Ok(old)
}

/// One step of thread `tid`. True when it ended in the step's trap. An interrupt or job-control stop that came
/// first is passed over; another signal goes to `sigs`, for the thread's next run.
fn step_once(tid: i32, sigs: &mut Vec<i32>) -> io::Result<bool> {
    pt(SINGLESTEP, tid, 0, 0)?;
    let st = waitpid(tid, 0)?.unwrap_or(0);
    if !libc::WIFSTOPPED(st) {
        return Err(io::Error::other("the thread ended during mprotect"));
    }
    let sig = libc::WSTOPSIG(st);
    if st >> 16 != 0 {
        return Ok(false);
    }
    if sig == libc::SIGTRAP {
        return Ok(true);
    }
    sigs.push(sig);
    Ok(false)
}

/// Step thread `tid` with registers `u` until it has run the two-byte system call instruction at its pc: the
/// result in rax. A trap that left the pc where it was (the step ended a system call the thread stood in) is
/// stepped again.
fn run_injected(tid: i32, u: &[u64; 27], sigs: &mut Vec<i32>) -> io::Result<i64> {
    let (from, to) = (u[16], u[16] + 2);
    for _ in 0..8 {
        pt(SETREGS, tid, 0, u.as_ptr() as u64)?;
        if !step_once(tid, sigs)? {
            continue;
        }
        let mut after = [0u64; 27];
        pt(GETREGS, tid, 0, after.as_mut_ptr() as u64)?;
        if after[16] == to {
            return Ok(after[10] as i64);
        }
        if after[16] != from {
            return Err(io::Error::other(format!("the step ended at {:#x}, not after the call", after[16])));
        }
    }
    Err(io::Error::other("signals kept coming before the mprotect step"))
}

/// Run `mprotect(addr, len, prot)` in the program from stopped thread `tid`: a `syscall` (`int 0x80` in a 32-bit
/// program) put for one step where the thread stands, then every byte and register goes back as it was. `finish`:
/// the thread stands inside a fork, clone or exec, and one step ends that call first, so the program sees it return
/// as it would have. Signals that come meanwhile go to `sigs`.
fn mprotect_in(tid: i32, addr: u64, len: u64, prot: u8, finish: bool, sigs: &mut Vec<i32>) -> Result<()> {
    if finish {
        let mut ended = false;
        for _ in 0..8 {
            if step_once(tid, sigs).map_err(|e| os_err("the end of the system call before mprotect", e))? {
                ended = true;
                break;
            }
        }
        if !ended {
            return fail(format!("thread {tid} did not leave its system call for mprotect"));
        }
    }
    let mut saved = [0u64; 27];
    pt(GETREGS, tid, 0, saved.as_mut_ptr() as u64).map_err(|e| os_err("registers for mprotect", e))?;
    let rip = saved[16];
    // the bits are the same as Linux's PROT_READ, PROT_WRITE, PROT_EXEC
    let p = u64::from(prot & 7);
    let mut u = saved;
    // not inside a system call, so the kernel restarts none on the way out
    u[15] = u64::MAX;
    let insn = if saved[17] == 0x23 {
        (u[10], u[5], u[11], u[12]) = (125, addr, len, p);
        [0xcd, 0x80]
    } else {
        (u[10], u[14], u[13], u[12]) = (libc::SYS_mprotect as u64, addr, len, p);
        [0x0f, 0x05]
    };
    let code = swap_bytes(tid, rip, &insn).map_err(|e| os_err(&format!("the code at {rip:#x} for mprotect"), e))?;
    let ran = run_injected(tid, &u, sigs);
    let back = swap_bytes(tid, rip, &code);
    pt(SETREGS, tid, 0, saved.as_ptr() as u64).map_err(|e| os_err("registers after mprotect", e))?;
    back.map_err(|e| os_err(&format!("the code at {rip:#x} after mprotect"), e))?;
    match ran {
        Ok(r) if r < 0 => fail(format!(
            "mprotect({addr:#x}, {len:#x}) in the program failed: {}",
            io::Error::from_raw_os_error(-r as i32)
        )),
        Ok(_) => Ok(()),
        Err(e) => Err(os_err("mprotect", e)),
    }
}

/// Send thread `tid` the signals it stopped on while it ran an instruction of ours, so it gets them when it runs on.
fn send_again(tid: i32, sigs: Vec<i32>) {
    for s in sigs {
        // SAFETY: a thread of a process we trace.
        unsafe { libc::syscall(libc::SYS_tkill, tid, s) };
    }
}

/// AT_ENTRY in an auxiliary vector of (key, value) pairs of `word`-byte words (8, or 4 in a 32-bit process).
fn auxv_entry(b: &[u8], word: usize) -> Option<u64> {
    let at = |s: &[u8]| {
        let mut w = [0u8; 8];
        w[..word].copy_from_slice(s);
        u64::from_le_bytes(w)
    };
    b.chunks_exact(2 * word).find_map(|c| (at(&c[..word]) == AT_ENTRY).then(|| at(&c[word..])))
}

fn os_err(what: &str, e: io::Error) -> Error {
    Error::Fail(format!("{what}: {e}"))
}

fn pipe() -> Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` holds two descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(os_err("pipe", io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

fn nonblocking(fd: i32) -> File {
    // SAFETY: the descriptor is ours and open; the File owns it from here.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        File::from_raw_fd(fd)
    }
}

impl Linux {
    pub fn launch(l: &Launch) -> Result<Linux> {
        let c = |b: &[u8]| CString::new(b).map_err(|_| Error::Usage("an argument holds a zero byte".into()));
        let exe = c(l.exe.as_os_str().as_bytes())?;
        let mut argv = vec![match &l.argv0 {
            Some(a) => c(a.as_bytes())?,
            None => exe.clone(),
        }];
        for a in &l.args {
            argv.push(c(a.as_bytes())?);
        }
        let mut envp = Vec::new();
        for (k, v) in std::env::vars_os() {
            if !l.env.iter().any(|(n, _)| n.as_bytes() == k.as_bytes())
                && !l.unset.iter().any(|n| n.as_bytes() == k.as_bytes())
            {
                envp.push(c(&[k.as_bytes(), b"=", v.as_bytes()].concat())?);
            }
        }
        for (k, v) in &l.env {
            envp.push(c(format!("{k}={v}").as_bytes())?);
        }
        let cwd = match &l.cwd {
            Some(d) => Some(c(d.as_os_str().as_bytes())?),
            None => None,
        };
        let mut argv_p: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        argv_p.push(std::ptr::null());
        let mut envp_p: Vec<*const libc::c_char> = envp.iter().map(|a| a.as_ptr()).collect();
        envp_p.push(std::ptr::null());
        let (in_r, in_w) = pipe()?;
        let (out_r, out_w) = pipe()?;
        let (err_r, err_w) = pipe()?;
        let aslr = l.aslr;
        // SAFETY: after fork the child only makes async-signal-safe calls with memory prepared above.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(os_err("fork", io::Error::last_os_error()));
        }
        if pid == 0 {
            // SAFETY: see above; this branch never returns.
            unsafe {
                if !aslr {
                    let now = libc::personality(0xffff_ffff);
                    if now >= 0 {
                        libc::personality(now as libc::c_ulong | ADDR_NO_RANDOMIZE);
                    }
                }
                libc::dup2(in_r, 0);
                libc::dup2(out_w, 1);
                libc::dup2(out_w, 2);
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                let ok = cwd.as_ref().is_none_or(|d| libc::chdir(d.as_ptr()) == 0);
                if ok {
                    libc::kill(libc::getpid(), libc::SIGSTOP);
                    libc::execve(exe.as_ptr(), argv_p.as_ptr(), envp_p.as_ptr());
                }
                let e: i32 = *libc::__errno_location();
                libc::write(err_w, (&raw const e).cast::<c_void>(), 4);
                libc::_exit(127);
            }
        }
        // SAFETY: these ends belong to the child now.
        unsafe {
            libc::close(in_r);
            libc::close(out_w);
            libc::close(err_w);
        }
        let stdin = nonblocking(in_w);
        let out = nonblocking(out_r);
        // SAFETY: the descriptor is ours.
        let mut err = unsafe { File::from_raw_fd(err_r) };
        let name = l.exe.display().to_string();
        let mut why = move |pid: i32| -> Error {
            // SAFETY: the child is ours; killing and reaping it is the clean way out.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), libc::__WALL);
            }
            let mut b = [0u8; 4];
            match err.read(&mut b) {
                Ok(4) => {
                    Error::Fail(format!("cannot run {name}: {}", io::Error::from_raw_os_error(i32::from_ne_bytes(b))))
                }
                _ => Error::Fail(format!("cannot run {name}: it ended before it started")),
            }
        };
        // The child stops itself; from then on it is ours.
        let mut st = 0;
        // SAFETY: a plain wait on our child.
        if unsafe { libc::waitpid(pid, &mut st, libc::WUNTRACED) } != pid || !libc::WIFSTOPPED(st) {
            return Err(why(pid));
        }
        if let Err(e) =
            pt(SEIZE, pid, 0, O_EXITKILL | O_TRACESYSGOOD | O_TRACECLONE | O_TRACEFORK | O_TRACEVFORK | O_TRACEEXEC)
        {
            let _ = why(pid);
            return Err(Error::Fail(format!(
                "the system does not let nexo trace a program here ({e}); inside nexo's sandbox run the dbg command unsandboxed"
            )));
        }
        // SAFETY: wake our stopped child.
        unsafe { libc::kill(pid, libc::SIGCONT) };
        loop {
            match waitpid(pid, 0) {
                Ok(Some(st)) if libc::WIFSTOPPED(st) => {
                    if st >> 16 == EVENT_EXEC {
                        break;
                    }
                    if pt(CONT, pid, 0, 0).is_err() {
                        return Err(why(pid));
                    }
                }
                _ => return Err(why(pid)),
            }
        }
        let mut th = BTreeMap::new();
        th.insert(pid, Th::default());
        let mut me = Linux {
            pid,
            th,
            mem: None,
            launched: true,
            stdin: Some(stdin),
            out: Some(out),
            watch: [None; 4],
            patches: Vec::new(),
            guards: Vec::new(),
            gone: false,
            want_stop: false,
            sys: false,
            follow: Follow::Parent,
            forks: Vec::new(),
            vkids: Vec::new(),
        };
        me.open_mem();
        Ok(me)
    }

    pub fn attach(pid: u32) -> Result<Linux> {
        let pid = i32::try_from(pid).ok().filter(|p| *p > 0).ok_or_else(|| Error::Usage("not a process id".into()))?;
        let owner =
            std::fs::metadata(format!("/proc/{pid}")).map_err(|_| Error::Fail(format!("no process {pid}")))?.uid();
        // SAFETY: no arguments, no failure.
        if owner != unsafe { libc::geteuid() } {
            return fail(format!(
                "process {pid} belongs to another user; nexo dbg only attaches to your own processes"
            ));
        }
        let mut me = Linux {
            pid,
            th: BTreeMap::new(),
            mem: None,
            launched: false,
            stdin: None,
            out: None,
            watch: [None; 4],
            patches: Vec::new(),
            guards: Vec::new(),
            gone: false,
            want_stop: false,
            sys: false,
            follow: Follow::Parent,
            forks: Vec::new(),
            vkids: Vec::new(),
        };
        loop {
            let mut new = 0;
            let dir =
                std::fs::read_dir(format!("/proc/{pid}/task")).map_err(|_| Error::Fail(format!("no process {pid}")))?;
            for tid in dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok()) {
                if me.th.contains_key(&tid) {
                    continue;
                }
                if let Err(e) =
                    pt(SEIZE, tid, 0, O_TRACESYSGOOD | O_TRACECLONE | O_TRACEFORK | O_TRACEVFORK | O_TRACEEXEC)
                {
                    if e.raw_os_error() == Some(libc::ESRCH) {
                        continue;
                    }
                    let _ = me.detach();
                    return fail(format!(
                        "cannot attach to {pid}: {e}. With kernel.yama.ptrace_scope=1 only a parent may attach (or the program allows it with PR_SET_PTRACER); inside nexo's sandbox attach needs an unsandboxed command"
                    ));
                }
                let _ = pt(INTERRUPT, tid, 0, 0);
                let mut t = Th::default();
                if let Ok(Some(st)) = waitpid(tid, 0) {
                    if !libc::WIFSTOPPED(st) {
                        continue;
                    }
                    if st >> 16 == 0 && libc::WSTOPSIG(st) != libc::SIGTRAP {
                        t.sig = libc::WSTOPSIG(st);
                    }
                }
                me.th.insert(tid, t);
                new += 1;
            }
            if new == 0 {
                break;
            }
        }
        if me.th.is_empty() {
            return fail(format!("no process {pid}"));
        }
        me.open_mem();
        Ok(me)
    }

    fn open_mem(&mut self) {
        self.mem = File::open(format!("/proc/{}/mem", self.pid)).ok();
    }

    fn stopped_tid(&self) -> Result<i32> {
        // a thread held at its vfork runs no code of its own before the child execs or exits
        let free = self.th.iter().find(|(_, t)| !t.running && !t.held);
        match free.or_else(|| self.th.iter().find(|(_, t)| !t.running)) {
            Some((tid, _)) if !self.gone => Ok(*tid),
            _ => fail("the program is not stopped"),
        }
    }

    fn apply_watch(&self, tid: i32) -> io::Result<()> {
        pt(POKEUSER, tid, DEBUGREG + 7 * 8, 0)?;
        let mut dr7 = 0u64;
        for (i, s) in self.watch.iter().enumerate() {
            let Some(s) = s else { continue };
            pt(POKEUSER, tid, DEBUGREG + i as u64 * 8, s.addr)?;
            let rw: u64 = match s.access {
                Access::Exec => 0,
                Access::Write => 1,
                Access::ReadWrite => 3,
            };
            let len: u64 = match s.len {
                2 => 1,
                4 => 3,
                8 => 2,
                _ => 0,
            };
            dr7 |= 1 << (2 * i) | rw << (16 + 4 * i) | len << (18 + 4 * i);
        }
        if dr7 != 0 {
            pt(POKEUSER, tid, DEBUGREG + 7 * 8, dr7)?;
        }
        Ok(())
    }

    /// Give `tid` its last order again (it stopped for something nobody asked about).
    fn again(&mut self, tid: i32) {
        let Some(t) = self.th.get_mut(&tid) else { return };
        let sig = std::mem::take(&mut t.sig);
        if pt(order(t.step, self.sys), tid, 0, sig as u64).is_ok() {
            t.running = true;
        }
    }

    /// Wait for the first stop of a child the kernel attached to us at a fork. False: it is already gone.
    fn first_stop(child: i32) -> bool {
        loop {
            match waitpid(child, 0) {
                Ok(Some(st)) if libc::WIFSTOPPED(st) => return true,
                Ok(Some(_)) | Err(_) => return false,
                Ok(None) => {}
            }
        }
    }

    /// Write one byte through stopped thread `tid` (ptrace writes whole words).
    fn poke_byte(tid: i32, addr: u64, byte: u8) -> bool {
        let base = addr & !7;
        let mut w = 0u64;
        if pt(PEEKDATA, tid, base, (&raw mut w) as u64).is_err() {
            return false;
        }
        let mut b = w.to_le_bytes();
        b[(addr - base) as usize] = byte;
        pt(POKEDATA, tid, base, u64::from_le_bytes(b)).is_ok()
    }

    /// Take in one status of `tid`. `stopping` is true while every thread is being stopped: nothing
    /// is set running again then.
    fn handle(&mut self, tid: i32, st: i32, stopping: bool, events: &mut Vec<Event>) {
        if libc::WIFEXITED(st) || libc::WIFSIGNALED(st) {
            self.th.remove(&tid);
            if tid == self.pid {
                self.gone = true;
                self.th.clear();
                events.push(if libc::WIFEXITED(st) {
                    Event::Exited { code: libc::WEXITSTATUS(st) }
                } else {
                    Event::Killed { signal: libc::WTERMSIG(st) }
                });
            }
            return;
        }
        if !libc::WIFSTOPPED(st) {
            return;
        }
        let sig = libc::WSTOPSIG(st);
        let fresh = {
            let t = self.th.entry(tid).or_default();
            t.running = false;
            std::mem::take(&mut t.fresh)
        };
        if fresh {
            let _ = self.apply_watch(tid);
        }
        let stopped = |stop| Event::Stopped { tid: tid as Tid, stop };
        match st >> 16 {
            EVENT_CLONE => {
                let mut new = 0u64;
                if pt(GETEVENTMSG, tid, 0, (&raw mut new) as u64).is_ok() && new > 0 {
                    self.th.entry(new as i32).or_insert(Th { running: true, fresh: true, ..Th::default() });
                }
                if !stopping {
                    self.again(tid);
                }
            }
            EVENT_FORK => self.on_fork(tid, false, stopping, events),
            EVENT_VFORK => self.on_fork(tid, true, stopping, events),
            EVENT_EXEC => {
                // The thread that called exec now carries the process id; the others are gone.
                self.th.retain(|k, _| *k == self.pid);
                self.th.entry(self.pid).or_default().running = false;
                self.watch = [None; 4];
                self.patches.clear();
                self.guards.clear();
                self.open_mem();
                events.push(Event::Stopped { tid: self.pid as Tid, stop: Stop::Exec });
            }
            EVENT_STOP => {
                if stopping {
                } else if self.want_stop {
                    self.want_stop = false;
                    events.push(stopped(Stop::Interrupt));
                } else {
                    self.again(tid);
                }
            }
            0 if sig == libc::SIGTRAP | 0x80 => match syscall_info(tid) {
                Some(sys) => events.push(stopped(Stop::Syscall(sys))),
                None if !stopping => self.again(tid),
                None => {}
            },
            0 => {
                let mut info = [0u64; 16];
                let have = pt(GETSIGINFO, tid, 0, info.as_mut_ptr() as u64).is_ok();
                let code = if have { (info[1] & 0xffff_ffff) as u32 as i32 } else { 0 };
                if sig == libc::SIGTRAP && matches!(code, 0x80 | 1) {
                    events.push(stopped(Stop::Breakpoint));
                } else if sig == libc::SIGTRAP && matches!(code, 2 | 4) {
                    let mut dr6 = 0u64;
                    let _ = pt(PEEKUSER, tid, DEBUGREG + 6 * 8, (&raw mut dr6) as u64);
                    let _ = pt(POKEUSER, tid, DEBUGREG + 6 * 8, 0);
                    let hit = (dr6 & 0xf) as u8;
                    events.push(stopped(if hit != 0 { Stop::Watch(hit) } else { Stop::Step }));
                } else {
                    if let Some(t) = self.th.get_mut(&tid) {
                        t.sig = sig;
                    }
                    let about = matches!(sig, libc::SIGSEGV | libc::SIGBUS | libc::SIGILL | libc::SIGFPE) && code > 0;
                    events.push(stopped(Stop::Fault(Fault {
                        number: sig as u32,
                        code,
                        addr: about.then_some(info[2]),
                        touch: None,
                        first_chance: true,
                    })));
                }
            }
            _ => {
                if !stopping {
                    self.again(tid);
                }
            }
        }
    }

    /// Stop every thread that still runs and take in what each one was doing.
    fn stop_all(&mut self, events: &mut Vec<Event>) {
        // a held thread is stopped already; the program is reported stopped, so it stays when its child execs
        for t in self.th.values_mut() {
            t.go = false;
        }
        while !self.gone {
            let running: Vec<i32> = self.th.iter().filter(|(_, t)| t.running).map(|(k, _)| *k).collect();
            if running.is_empty() {
                break;
            }
            for tid in &running {
                let _ = pt(INTERRUPT, *tid, 0, 0);
            }
            for tid in running {
                match waitpid(tid, 0) {
                    Ok(Some(st)) => self.handle(tid, st, true, events),
                    _ => {
                        self.th.remove(&tid);
                    }
                }
                if self.gone {
                    break;
                }
            }
        }
    }
}

impl Drop for Linux {
    fn drop(&mut self) {
        let _ = if self.launched { self.kill() } else { self.detach() };
    }
}

fn parse_map(line: &str) -> Option<Map> {
    let mut it = line.splitn(6, ' ');
    let (a, b) = it.next()?.split_once('-')?;
    let perms = it.next()?.as_bytes();
    let offset = u64::from_str_radix(it.next()?, 16).ok()?;
    let (_dev, _inode) = (it.next()?, it.next()?);
    Some(Map {
        start: u64::from_str_radix(a, 16).ok()?,
        end: u64::from_str_radix(b, 16).ok()?,
        read: perms.first() == Some(&b'r'),
        write: perms.get(1) == Some(&b'w'),
        exec: perms.get(2) == Some(&b'x'),
        offset,
        path: it.next().unwrap_or("").trim().to_string(),
    })
}
