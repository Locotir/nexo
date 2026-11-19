//! macOS x86-64 through Mach. The program's exceptions (faults, `int3`, the trap flag, and through `PT_ATTACHEXC`
//! every Unix signal) come to a port of ours as `mach_exception_raise` messages; the faulting thread waits in the
//! kernel until it gets our reply. A stop is the whole task suspended with `task_suspend`; threads are named by
//! their port names in this process.
//!
//! macOS lets a process that is not signed as a debugger open only programs that allow it: their signature holds
//! `com.apple.security.get-task-allow`. So `run` debugs a copy of the program signed that way (only a file the user
//! owns is copied), and `attach` says what macOS allows when the program does not allow it.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use libc::{c_char, c_int};

use crate::mach::*;
use crate::platform::{Access, Event, Fault, HwSlot, Launch, Map, Module, Os, Resume, Stop, Target, Tid};
use crate::regs::Regs;
use crate::{fail, Error, Result};

mod exc;
mod sign;
mod target;

use sign::*;

/// `_POSIX_SPAWN_DISABLE_ASLR` and `POSIX_SPAWN_CLOEXEC_DEFAULT` (the child gets only the descriptors named here).
const DISABLE_ASLR: i16 = 0x100;
const CLOEXEC_DEFAULT: i16 = 0x4000;
/// The trap flag in rflags: one instruction, then `EXC_BREAKPOINT`.
const TF: u64 = 0x100;
const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const MH_DYLINKER: u32 = 7;
const LC_MAIN: u32 = 0x8000_0028;
/// `VM_MEMORY_STACK`, and the `VM_MEMORY_MALLOC*` tags that make a region heap.
const TAG_STACK: u32 = 30;
const TAG_HEAP: [u32; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 12];

/// What macOS allows, said where an attach is refused.
const ATTACH_RULE: &str = "macOS lets nexo open only programs that allow debugging: start it with `nexo dbg run` (nexo signs a copy with get-task-allow), or turn on Developer Mode once with `DevToolsSecurity -enable` (admin password) and attach from a desktop Terminal";

/// Said at a start with randomisation off. macOS puts its random start values (`stack_guard=0x...`,
/// `malloc_entropy=0x...` among the strings after the environment) above the stack written without leading zeros,
/// so their length, and with it where the stack starts, can change from run to run; the debugger cannot fix them.
const STACK_SHIFT: &str = "; address randomisation is off: code and global addresses repeat from run to run; stack addresses can still differ by 16 bytes between some runs, because macOS writes random values of varying length (stack_guard, malloc_entropy) above the stack at each start";

/// A thread's exception message, waiting for our reply.
#[derive(Debug, Clone, Copy)]
struct Pend {
    reply: Port,
    bits: u32,
    id: i32,
    /// A Unix signal (`EXC_SOFT_SIGNAL`): the reply goes with `PT_THUPDATE`, which says which signal it gets.
    soft: bool,
}

#[derive(Debug, Default)]
struct Th {
    pend: Option<Pend>,
    /// The signal it gets when it runs again (0: none).
    sig: i32,
    /// It stopped on a fault: the reply says "not handled", and the kernel makes the signal of it.
    fault: bool,
    /// The signal the kernel is about to make of a fault answered so: it goes on without a stop.
    pass: i32,
}

/// One `mach_exception_raise` request.
struct Exc {
    thread: Port,
    task: Port,
    exc: i32,
    code: [i64; 2],
    pend: Pend,
}

pub struct Mac {
    pid: i32,
    task: Port,
    port: Port,
    /// The exception ports the program had: given back on detach.
    old: Vec<(u32, Port, c_int, c_int)>,
    th: BTreeMap<Port, Th>,
    launched: bool,
    stdin: Option<File>,
    out: Option<File>,
    watch: [Option<HwSlot>; 4],
    gone: bool,
    /// The task runs: one `task_suspend` stops it.
    running: bool,
    want_stop: bool,
    /// Threads kept still with `thread_suspend` while one steps.
    held: Vec<Port>,
    /// The program as the user named it, and the signed copy that runs in its place.
    exe: String,
    copy: Option<(PathBuf, PathBuf)>,
    note: Option<String>,
    exe_base: Option<u64>,
    /// The program's byte under every breakpoint the engine has put in, those taken out since included: a child the
    /// program forks copies the patched code, and is caught only at its first exception, which may come after the
    /// breakpoint left the parent.
    under: BTreeMap<u64, u8>,
}

fn os_err(what: &str, e: io::Error) -> Error {
    Error::Fail(format!("{what}: {e}"))
}

fn me() -> Port {
    // SAFETY: reads the process's own task port name, set once at start.
    unsafe { mach_task_self_ }
}

fn pipe() -> Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` holds two descriptors; both are ours to mark.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(os_err("pipe", io::Error::last_os_error()));
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
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

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process is there.
    unsafe { libc::kill(pid, 0) == 0 || *libc::__error() == libc::EPERM }
}

impl Mac {
    fn new(pid: i32, task: Port, launched: bool) -> Mac {
        Mac {
            pid,
            task,
            port: MACH_PORT_NULL,
            old: Vec::new(),
            th: BTreeMap::new(),
            launched,
            stdin: None,
            out: None,
            watch: [None; 4],
            gone: false,
            running: true,
            want_stop: false,
            held: Vec::new(),
            exe: String::new(),
            copy: None,
            note: None,
            exe_base: None,
            under: BTreeMap::new(),
        }
    }

    pub fn launch(l: &Launch) -> Result<Mac> {
        let prog = prepare(&l.exe)?;
        let drop_copy = |p: &Prepared| {
            if let Some(c) = &p.copy {
                let _ = std::fs::remove_file(c);
            }
        };
        let r = Self::spawn(l, &prog);
        if r.is_err() {
            drop_copy(&prog);
        }
        let mut me = r?;
        me.exe = l.exe.display().to_string();
        let mut note = prog.note;
        if !l.aslr {
            note.push_str(STACK_SHIFT);
        }
        me.note = Some(note);
        if let Some(c) = prog.copy {
            let real = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
            me.copy = Some((c, real));
        }
        Ok(me)
    }

    fn spawn(l: &Launch, prog: &Prepared) -> Result<Mac> {
        let c = |b: &[u8]| CString::new(b).map_err(|_| Error::Usage("an argument holds a zero byte".into()));
        let path = c(prog.run.as_os_str().as_bytes())?;
        // the program sees the name it was given, not the copy's
        let mut argv = vec![match &l.argv0 {
            Some(a) => c(a.as_bytes())?,
            None => c(l.exe.as_os_str().as_bytes())?,
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
        let mut argv_p: Vec<*mut c_char> = argv.iter().map(|a| a.as_ptr().cast_mut()).collect();
        argv_p.push(std::ptr::null_mut());
        let mut envp_p: Vec<*mut c_char> = envp.iter().map(|a| a.as_ptr().cast_mut()).collect();
        envp_p.push(std::ptr::null_mut());
        let (in_r, in_w) = pipe()?;
        let (out_r, out_w) = pipe()?;
        let mut pid = 0;
        // SAFETY: the attribute and action objects are made, used and destroyed here; every pointer handed to
        // posix_spawn lives until it returns.
        let rc = unsafe {
            let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
            let mut acts: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
            libc::posix_spawnattr_init(&mut attr);
            libc::posix_spawn_file_actions_init(&mut acts);
            let mut flags = libc::POSIX_SPAWN_START_SUSPENDED as i16
                | CLOEXEC_DEFAULT
                | libc::POSIX_SPAWN_SETSIGDEF as i16
                | libc::POSIX_SPAWN_SETSIGMASK as i16;
            if !l.aslr {
                flags |= DISABLE_ASLR;
            }
            libc::posix_spawnattr_setflags(&mut attr, flags);
            let mut none: libc::sigset_t = 0;
            libc::sigemptyset(&mut none);
            libc::posix_spawnattr_setsigmask(&mut attr, &none);
            // this process ignores SIGPIPE; the program gets the default, as from a shell
            let mut dfl: libc::sigset_t = 0;
            libc::sigemptyset(&mut dfl);
            libc::sigaddset(&mut dfl, libc::SIGPIPE);
            libc::posix_spawnattr_setsigdefault(&mut attr, &dfl);
            libc::posix_spawn_file_actions_adddup2(&mut acts, in_r, 0);
            libc::posix_spawn_file_actions_adddup2(&mut acts, out_w, 1);
            libc::posix_spawn_file_actions_adddup2(&mut acts, out_w, 2);
            if let Some(d) = &cwd {
                libc::posix_spawn_file_actions_addchdir_np(&mut acts, d.as_ptr());
            }
            let rc = libc::posix_spawn(&mut pid, path.as_ptr(), &acts, &attr, argv_p.as_ptr(), envp_p.as_ptr());
            libc::posix_spawn_file_actions_destroy(&mut acts);
            libc::posix_spawnattr_destroy(&mut attr);
            libc::close(in_r);
            libc::close(out_w);
            rc
        };
        let name = l.exe.display().to_string();
        if rc != 0 {
            // SAFETY: both ends are ours.
            unsafe {
                libc::close(in_w);
                libc::close(out_r);
            }
            return fail(format!("cannot run {name}: {}", io::Error::from_raw_os_error(rc)));
        }
        let stdin = nonblocking(in_w);
        let out = nonblocking(out_r);
        let mut task = MACH_PORT_NULL;
        // SAFETY: `task` is a valid place for the port name.
        let kr = unsafe { libc::task_for_pid(me(), pid, &mut task) };
        if kr != KERN_SUCCESS {
            // SAFETY: the child is ours and still held at its start; SIGKILL ends a held process too.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let deadline = Instant::now() + Duration::from_secs(3);
            // SAFETY: a plain wait on our own child.
            while unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) } == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let why = if prog.copy.is_some() {
                "although the copy it runs is signed with get-task-allow; Developer Mode (`DevToolsSecurity -enable`, admin password) lets a desktop Terminal debug it"
            } else {
                "it belongs to another user, so nexo did not sign a copy, and its own signature does not allow debugging; copy it into a folder of yours and run the copy, or turn on Developer Mode once with `DevToolsSecurity -enable` (admin password) and run it from a desktop Terminal"
            };
            return fail(format!("macOS does not let nexo open {name} ({}): {why}", kr_text(kr)));
        }
        let mut m = Mac::new(pid, task, true);
        m.stdin = Some(stdin);
        m.out = Some(out);
        // From here a failure drops `m`, which kills the child.
        m.running = false;
        m.catch_exceptions()?;
        m.trace()?;
        // SAFETY: undo the suspension posix_spawn started the program with. On macOS 15 PT_ATTACHEXC has already
        // lifted it (seen on an x86_64 Mac), and this call then fails with nothing left to undo.
        unsafe { task_resume(m.task) };
        m.running = true;
        m.first_stop().map_err(|e| Error::Fail(format!("cannot start {name}: {e}")))?;
        Ok(m)
    }

    pub fn attach(pid: u32) -> Result<Mac> {
        let pid = i32::try_from(pid).ok().filter(|p| *p > 0).ok_or_else(|| Error::Usage("not a process id".into()))?;
        // SAFETY: signal 0 only checks that the process is there and that this user may signal it.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return match io::Error::last_os_error().raw_os_error() {
                Some(libc::EPERM) => {
                    fail(format!("process {pid} belongs to another user; nexo dbg only attaches to your own processes"))
                }
                _ => fail(format!("no process {pid}")),
            };
        }
        let mut task = MACH_PORT_NULL;
        // SAFETY: `task` is a valid place for the port name.
        let kr = unsafe { libc::task_for_pid(me(), pid, &mut task) };
        if kr != KERN_SUCCESS {
            return fail(format!("cannot attach to {pid} ({}): {ATTACH_RULE}", kr_text(kr)));
        }
        let mut m = Mac::new(pid, task, false);
        m.catch_exceptions()?;
        m.trace().map_err(|e| Error::Fail(format!("cannot attach to {pid}: {e}")))?;
        m.first_stop().map_err(|e| Error::Fail(format!("cannot attach to {pid}: {e}")))?;
        Ok(m)
    }

    fn state(&self, tid: Port) -> Result<[u64; 21]> {
        let mut s = [0u64; 21];
        let mut n = X86_THREAD_STATE64_COUNT;
        // SAFETY: 21 words hold the 42 halves `n` says.
        let kr = unsafe { thread_get_state(tid, X86_THREAD_STATE64, s.as_mut_ptr().cast(), &mut n) };
        if kr != KERN_SUCCESS {
            return fail(format!("registers of thread {tid}: {}", kr_text(kr)));
        }
        Ok(s)
    }

    fn set_state(&self, tid: Port, s: &[u64; 21]) -> Result<()> {
        // SAFETY: as above.
        let kr = unsafe { thread_set_state(tid, X86_THREAD_STATE64, s.as_ptr().cast(), X86_THREAD_STATE64_COUNT) };
        if kr != KERN_SUCCESS {
            return fail(format!("set registers of thread {tid}: {}", kr_text(kr)));
        }
        Ok(())
    }

    fn debug_state(&self, tid: Port, d: &mut [u64; 8]) -> bool {
        let mut n = X86_DEBUG_STATE64_COUNT;
        // SAFETY: dr0 to dr7 are 8 words, the 16 halves `n` says.
        unsafe { thread_get_state(tid, X86_DEBUG_STATE64, d.as_mut_ptr().cast(), &mut n) == KERN_SUCCESS }
    }

    fn set_debug_state(&self, tid: Port, d: &[u64; 8]) -> bool {
        // SAFETY: as above.
        unsafe { thread_set_state(tid, X86_DEBUG_STATE64, d.as_ptr().cast(), X86_DEBUG_STATE64_COUNT) == KERN_SUCCESS }
    }

    /// The debug registers for the watch slots: dr0 to dr3 and dr7, as on Linux.
    fn debug_regs(&self) -> [u64; 8] {
        let mut d = [0u64; 8];
        for (i, s) in self.watch.iter().enumerate() {
            let Some(s) = s else { continue };
            d[i] = s.addr;
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
            d[7] |= 1 << (2 * i) | rw << (16 + 4 * i) | len << (18 + 4 * i);
        }
        d
    }

    /// The region at or after `addr`: start, end, protection, file offset and tag. Submaps (the shared cache) are
    /// looked into.
    fn region(&self, addr: u64, depth: &mut u32) -> Option<(u64, u64, c_int, u64, u32, bool)> {
        region_in(self.task, addr, depth)
    }

    fn file_at(&self, addr: u64) -> String {
        self.named(&region_file(self.pid, addr))
    }

    /// The copy's path as the program the user named.
    fn named(&self, p: &str) -> String {
        match &self.copy {
            Some((c, real)) if Path::new(p) == c || Path::new(p) == real => self.exe.clone(),
            _ => p.to_string(),
        }
    }

    fn read_word(&mut self, addr: u64) -> Option<u64> {
        let mut b = [0u8; 8];
        (self.read(addr, &mut b) == 8).then(|| u64::from_le_bytes(b))
    }

    fn read_cstr(&mut self, addr: u64) -> String {
        let mut out = Vec::new();
        let mut b = [0u8; 256];
        while out.len() < 4096 {
            let n = self.read(addr.wrapping_add(out.len() as u64), &mut b);
            if n == 0 {
                break;
            }
            match b[..n].iter().position(|&c| c == 0) {
                Some(z) => {
                    out.extend_from_slice(&b[..z]);
                    break;
                }
                None => out.extend_from_slice(&b[..n]),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// The images dyld lists (`dyld_all_image_infos`), each by its file's path.
    fn dyld_images(&mut self) -> Vec<Module> {
        let mut info = [0u32; TASK_DYLD_INFO_COUNT as usize];
        let mut n = TASK_DYLD_INFO_COUNT;
        // SAFETY: `info` holds the words `n` says.
        let kr = unsafe { libc::task_info(self.task, TASK_DYLD_INFO, info.as_mut_ptr().cast(), &mut n) };
        let all = u64::from(info[0]) | u64::from(info[1]) << 32;
        if kr != KERN_SUCCESS || all == 0 {
            return Vec::new();
        }
        let mut head = [0u8; 40];
        if self.read(all, &mut head) < 40 {
            return Vec::new();
        }
        let count = u32::from_le_bytes([head[4], head[5], head[6], head[7]]).min(4096);
        let array = u64::from_le_bytes(head[8..16].try_into().unwrap_or_default());
        let dyld = u64::from_le_bytes(head[32..40].try_into().unwrap_or_default());
        let mut out = Vec::new();
        for i in 0..u64::from(count) {
            let (Some(base), Some(name)) =
                (self.read_word(array.wrapping_add(24 * i)), self.read_word(array.wrapping_add(24 * i + 8)))
            else {
                break;
            };
            let path = self.read_cstr(name);
            let path = std::fs::canonicalize(&path).map(|p| p.display().to_string()).unwrap_or(path);
            out.push(Module { path: self.named(&path), base });
        }
        if dyld != 0 && !out.is_empty() && !out.iter().any(|m| m.base == dyld) {
            let path = self.file_at(dyld);
            out.push(Module { path, base: dyld });
        }
        out
    }

    /// Before dyld has run its list is empty: the program and dyld are found by their headers in memory.
    fn headers(&mut self) -> Vec<Module> {
        let mut out = Vec::new();
        for m in self.maps().unwrap_or_default() {
            if m.offset != 0 || m.path.is_empty() || !m.read {
                continue;
            }
            let mut h = [0u8; 16];
            if self.read(m.start, &mut h) < 16 || u32::from_le_bytes([h[0], h[1], h[2], h[3]]) != MH_MAGIC_64 {
                continue;
            }
            let kind = u32::from_le_bytes([h[12], h[13], h[14], h[15]]);
            if matches!(kind, MH_EXECUTE | MH_DYLINKER) && !out.iter().any(|o: &Module| o.path == m.path) {
                out.push(Module { path: m.path, base: m.start });
            }
        }
        out
    }
}

/// The file mapped at `addr` in process `pid`; empty for anonymous memory.
fn region_file(pid: c_int, addr: u64) -> String {
    let mut buf = [0u8; 1024];
    // SAFETY: the buffer holds the size given.
    let n = unsafe { libc::proc_regionfilename(pid, addr, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..(n as usize).min(buf.len())]).into_owned()
}

/// The region of `task` at or after `addr`: start, end, protection, file offset and tag. Submaps (the shared cache)
/// are looked into.
fn region_in(task: Port, addr: u64, depth: &mut u32) -> Option<(u64, u64, c_int, u64, u32, bool)> {
    let mut a = addr;
    let mut size = 0u64;
    let mut info = [0u32; 19];
    let mut n = VM_REGION_SUBMAP_INFO_COUNT_64;
    // SAFETY: `info` holds the 19 words `n` says.
    let kr = unsafe { mach_vm_region_recurse(task, &mut a, &mut size, depth, info.as_mut_ptr(), &mut n) };
    if kr != KERN_SUCCESS {
        return None;
    }
    let offset = u64::from(info[3]) | u64::from(info[4]) << 32;
    Some((a, a.wrapping_add(size), info[0] as c_int, offset, info[5], info[12] != 0))
}

fn write_mem(task: Port, addr: u64, bytes: &[u8]) -> Kr {
    // SAFETY: the bytes live through the call; the kernel copies them.
    unsafe { mach_vm_write(task, addr, bytes.as_ptr() as usize, bytes.len() as u32) }
}

/// Write into the memory of `task`, code included. Code is mapped read and execute: its pages open for writing as
/// a private copy, then the protection goes back. PT_ATTACHEXC told the kernel that the program's code may change,
/// and a child it forks inherits that.
fn patch(task: Port, addr: u64, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || write_mem(task, addr, bytes) == KERN_SUCCESS {
        return Ok(());
    }
    let mut depth = 64;
    let prot = match region_in(task, addr, &mut depth) {
        Some((start, _, prot, ..)) if start <= addr => prot,
        _ => return fail(format!("write at {addr:#x}: no memory there")),
    };
    let page = addr & !0xfff;
    let len = (addr + bytes.len() as u64 + 0xfff) & !0xfff;
    let len = len - page;
    // SAFETY: protection changes on pages of the task's own.
    let kr = unsafe { mach_vm_protect(task, page, len, 0, VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY) };
    if kr != KERN_SUCCESS {
        return fail(format!("write at {addr:#x}: {}", kr_text(kr)));
    }
    let kr = write_mem(task, addr, bytes);
    // SAFETY: as above.
    unsafe { mach_vm_protect(task, page, len, 0, prot) };
    if kr != KERN_SUCCESS {
        return fail(format!("write at {addr:#x}: {}", kr_text(kr)));
    }
    Ok(())
}

impl Drop for Mac {
    fn drop(&mut self) {
        let _ = if self.launched { self.kill() } else { self.detach() };
        // SAFETY: rights of ours: the task, and the send and receive rights of the exception port.
        unsafe {
            mach_port_deallocate(me(), self.task);
            if self.port != MACH_PORT_NULL {
                mach_port_deallocate(me(), self.port);
                mach_port_mod_refs(me(), self.port, MACH_PORT_RIGHT_RECEIVE, -1);
            }
        }
        if let Some((c, _)) = &self.copy {
            let _ = std::fs::remove_file(c);
        }
    }
}
