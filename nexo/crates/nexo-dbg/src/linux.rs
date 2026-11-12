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

use crate::platform::{Access, Event, Fault, HwSlot, Launch, Map, Module, Os, Resume, Stop, Target, Tid};
use crate::regs::Regs;
use crate::{fail, Error, Result};

const PEEKDATA: c_long = 2;
const PEEKUSER: c_long = 3;
const POKEDATA: c_long = 5;
const POKEUSER: c_long = 6;
const CONT: c_long = 7;
const SINGLESTEP: c_long = 9;
const GETREGS: c_long = 12;
const SETREGS: c_long = 13;
const DETACH: c_long = 17;
const GETEVENTMSG: c_long = 0x4201;
const GETSIGINFO: c_long = 0x4202;
const SEIZE: c_long = 0x4206;
const INTERRUPT: c_long = 0x4207;

const O_TRACEFORK: u64 = 0x2;
const O_TRACECLONE: u64 = 0x8;
const O_TRACEEXEC: u64 = 0x10;
const O_EXITKILL: u64 = 0x10_0000;

const EVENT_FORK: i32 = 1;
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
    /// The program ended or was let go.
    gone: bool,
    want_stop: bool,
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
        let mut argv = vec![exe.clone()];
        for a in &l.args {
            argv.push(c(a.as_bytes())?);
        }
        let mut envp = Vec::new();
        for (k, v) in std::env::vars_os() {
            if !l.env.iter().any(|(n, _)| n.as_bytes() == k.as_bytes()) {
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
        // SAFETY: a plain wait on our child.
        let mut st = 0;
        if unsafe { libc::waitpid(pid, &mut st, libc::WUNTRACED) } != pid || !libc::WIFSTOPPED(st) {
            return Err(why(pid));
        }
        if let Err(e) = pt(SEIZE, pid, 0, O_EXITKILL | O_TRACECLONE | O_TRACEFORK | O_TRACEEXEC) {
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
            gone: false,
            want_stop: false,
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
            gone: false,
            want_stop: false,
        };
        loop {
            let mut new = 0;
            let dir =
                std::fs::read_dir(format!("/proc/{pid}/task")).map_err(|_| Error::Fail(format!("no process {pid}")))?;
            for tid in dir.flatten().filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok()) {
                if me.th.contains_key(&tid) {
                    continue;
                }
                if let Err(e) = pt(SEIZE, tid, 0, O_TRACECLONE | O_TRACEFORK | O_TRACEEXEC) {
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
        match self.th.iter().find(|(_, t)| !t.running) {
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
        if pt(if t.step { SINGLESTEP } else { CONT }, tid, 0, sig as u64).is_ok() {
            t.running = true;
        }
    }

    /// A child the program forked: put the original bytes back over our breakpoints and let it go.
    fn release_child(&self, child: i32) {
        loop {
            match waitpid(child, 0) {
                Ok(Some(st)) if libc::WIFSTOPPED(st) => break,
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => {}
            }
        }
        for &(addr, byte) in &self.patches {
            let base = addr & !7;
            let mut w = 0u64;
            if pt(PEEKDATA, child, base, (&raw mut w) as u64).is_ok() {
                let mut b = w.to_le_bytes();
                b[(addr - base) as usize] = byte;
                let _ = pt(POKEDATA, child, base, u64::from_le_bytes(b));
            }
        }
        let _ = pt(DETACH, child, 0, 0);
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
            EVENT_FORK => {
                let mut child = 0u64;
                if pt(GETEVENTMSG, tid, 0, (&raw mut child) as u64).is_ok() && child > 0 {
                    self.release_child(child as i32);
                }
                if !stopping {
                    self.again(tid);
                }
            }
            EVENT_EXEC => {
                // The thread that called exec now carries the process id; the others are gone.
                self.th.retain(|k, _| *k == self.pid);
                self.th.entry(self.pid).or_default().running = false;
                self.watch = [None; 4];
                self.patches.clear();
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

impl Target for Linux {
    fn os(&self) -> Os {
        Os::Linux
    }

    fn pid(&self) -> u32 {
        self.pid as u32
    }

    fn launched(&self) -> bool {
        self.launched
    }

    fn threads(&self) -> Vec<Tid> {
        self.th.keys().map(|t| *t as Tid).collect()
    }

    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        let mut u = [0u64; 27];
        pt(GETREGS, tid as i32, 0, u.as_mut_ptr() as u64)
            .map_err(|e| os_err(&format!("registers of thread {tid}"), e))?;
        Ok(Regs::from_user(&u))
    }

    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()> {
        let u = regs.to_user();
        pt(SETREGS, tid as i32, 0, u.as_ptr() as u64)
            .map_err(|e| os_err(&format!("set registers of thread {tid}"), e))?;
        Ok(())
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        if let Some(f) = &self.mem {
            while done < buf.len() {
                match f.read_at(&mut buf[done..], addr.wrapping_add(done as u64)) {
                    Ok(n) if n > 0 => done += n,
                    _ => break,
                }
            }
            if done > 0 {
                return done;
            }
        }
        let Ok(tid) = self.stopped_tid() else { return 0 };
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let base = a & !7;
            let mut w = 0u64;
            if pt(PEEKDATA, tid, base, (&raw mut w) as u64).is_err() {
                break;
            }
            let off = (a - base) as usize;
            let n = (8 - off).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&w.to_le_bytes()[off..off + n]);
            done += n;
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        // Word by word through ptrace: it also works where the sandbox denies opening the memory file for writing.
        let tid = self.stopped_tid()?;
        let mut done = 0;
        while done < bytes.len() {
            let a = addr.wrapping_add(done as u64);
            let base = a & !7;
            let off = (a - base) as usize;
            let n = (8 - off).min(bytes.len() - done);
            let mut w = 0u64;
            let bad = |e| os_err(&format!("write at {a:#x}"), e);
            if n < 8 {
                pt(PEEKDATA, tid, base, (&raw mut w) as u64).map_err(bad)?;
            }
            let mut b = w.to_le_bytes();
            b[off..off + n].copy_from_slice(&bytes[done..done + n]);
            pt(POKEDATA, tid, base, u64::from_le_bytes(b)).map_err(bad)?;
            done += n;
        }
        Ok(())
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let text = std::fs::read_to_string(format!("/proc/{}/maps", self.pid))?;
        Ok(text.lines().filter_map(parse_map).collect())
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        let exe =
            std::fs::read_link(format!("/proc/{}/exe", self.pid)).map(|p| p.display().to_string()).unwrap_or_default();
        let mut out: Vec<Module> = Vec::new();
        for m in self.maps()? {
            if m.path.starts_with('/') && m.offset == 0 && !out.iter().any(|o| o.path == m.path) {
                out.push(Module { path: m.path, base: m.start });
            }
        }
        if let Some(i) = out.iter().position(|m| m.path == exe) {
            out[..=i].rotate_right(1);
        }
        Ok(out)
    }

    fn entry(&mut self) -> Option<u64> {
        let b = std::fs::read(format!("/proc/{}/auxv", self.pid)).ok()?;
        b.as_chunks::<16>().0.iter().find_map(|c| {
            let key = u64::from_le_bytes(c[..8].try_into().ok()?);
            (key == AT_ENTRY).then(|| u64::from_le_bytes(c[8..].try_into().unwrap_or_default()))
        })
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        self.watch = *slots;
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            self.apply_watch(tid).map_err(|e| os_err("hardware watch", e))?;
        }
        Ok(())
    }

    fn set_patches(&mut self, patches: &[(u64, u8)]) {
        self.patches = patches.to_vec();
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        if self.gone {
            return fail("the program has ended");
        }
        let one = match how {
            Resume::All => None,
            Resume::Step(t) => Some(t as i32),
        };
        for (tid, t) in self.th.iter_mut() {
            if t.running || one.is_some_and(|o| o != *tid) {
                continue;
            }
            t.step = one.is_some();
            let sig = std::mem::take(&mut t.sig);
            match pt(if t.step { SINGLESTEP } else { CONT }, *tid, 0, sig as u64) {
                Ok(_) => t.running = true,
                // A thread that died while stopped is found by the next wait.
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => t.running = true,
                Err(e) => return Err(os_err("resume", e)),
            }
        }
        if let Some(o) = one {
            if !self.th.contains_key(&o) {
                return fail(format!("no thread {o}"));
            }
        }
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if self.gone {
            return fail("the program has ended");
        }
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        let mut round = 0u32;
        loop {
            let running: Vec<i32> = self.th.iter().filter(|(_, t)| t.running).map(|(k, _)| *k).collect();
            if running.is_empty() {
                return fail("the program is stopped: nothing to wait for");
            }
            for tid in running {
                match waitpid(tid, libc::WNOHANG) {
                    Ok(Some(st)) => self.handle(tid, st, false, &mut events),
                    Ok(None) => {}
                    Err(_) => {
                        self.th.remove(&tid);
                        if tid == self.pid {
                            self.gone = true;
                            events.push(Event::Exited { code: -1 });
                        }
                    }
                }
                if self.gone {
                    break;
                }
            }
            if !events.is_empty() {
                self.stop_all(&mut events);
                self.want_stop = false;
                return Ok(events);
            }
            if Instant::now() >= deadline {
                return Ok(events);
            }
            round += 1;
            if round < 200 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros((u64::from(round) * 10).min(2000)));
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        let Some(tid) = self.th.iter().find(|(_, t)| t.running).map(|(k, _)| *k) else {
            return fail("the program is already stopped");
        };
        self.want_stop = true;
        pt(INTERRUPT, tid, 0, 0).map_err(|e| os_err("stop", e))?;
        Ok(())
    }

    fn stdin(&mut self, bytes: Option<&[u8]>) -> Result<usize> {
        let Some(bytes) = bytes else {
            self.stdin = None;
            return Ok(0);
        };
        let Some(f) = self.stdin.as_mut() else {
            return fail("this program's input is not connected to the session (attached, or already closed)");
        };
        let mut done = 0;
        while done < bytes.len() {
            match f.write(&bytes[done..]) {
                Ok(n) if n > 0 => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(os_err("write to the program's input", e)),
                Ok(_) => break,
            }
        }
        Ok(done)
    }

    fn output(&mut self) -> Vec<u8> {
        let mut all = Vec::new();
        if let Some(f) = self.out.as_mut() {
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => all.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
        all
    }

    fn detach(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        let mut events = Vec::new();
        self.stop_all(&mut events);
        for (tid, t) in &self.th {
            let _ = pt(DETACH, *tid, 0, t.sig as u64);
        }
        self.th.clear();
        self.gone = true;
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        // SAFETY: the process is the one we trace.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut events = Vec::new();
        while !self.gone && Instant::now() < deadline {
            for tid in self.th.keys().copied().collect::<Vec<_>>() {
                match waitpid(tid, libc::WNOHANG) {
                    Ok(Some(st)) => self.handle(tid, st, true, &mut events),
                    Ok(None) => {}
                    Err(_) => {
                        self.th.remove(&tid);
                        self.gone |= tid == self.pid;
                    }
                }
            }
            if !self.gone {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.gone = true;
        self.th.clear();
        Ok(())
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
