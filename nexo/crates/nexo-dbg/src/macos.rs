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

/// The program that runs under the debugger: a signed copy of a file the user owns, or the file itself.
struct Prepared {
    run: PathBuf,
    copy: Option<PathBuf>,
    note: String,
}

const ENTITLEMENTS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>com.apple.security.get-task-allow</key><true/></dict></plist>\n";

/// Copy `exe` beside itself (so `@executable_path` libraries still load), or into the session folder when its folder
/// is not writable, and sign the copy ad hoc with get-task-allow alone. A file of another user is not copied.
fn prepare(exe: &Path) -> Result<Prepared> {
    let meta = std::fs::metadata(exe).map_err(|e| os_err(&format!("cannot read {}", exe.display()), e))?;
    // SAFETY: no arguments, no failure.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Ok(Prepared {
            run: exe.to_path_buf(),
            copy: None,
            note: format!(
                "{} belongs to another user, so nexo runs it as it is, unsigned: macOS lets nexo debug it only if its own signature allows debugging (get-task-allow)",
                exe.display()
            ),
        });
    }
    let data = std::fs::read(exe).map_err(|e| os_err(&format!("cannot read {}", exe.display()), e))?;
    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "program".into());
    // one name per session: several sessions in one process may run the same program at once
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let copy_name = format!(".{name}.nexo-dbg-{}-{n}", std::process::id());
    let beside = exe.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let signed = nexo_bin::adhoc_sign(&data, &name, &[nexo_bin::GET_TASK_ALLOW]);
    let bytes = signed.as_deref().unwrap_or(&data);
    let mut copy = None;
    for dir in [beside.to_path_buf(), crate::state_dir()] {
        let _ = std::fs::create_dir_all(&dir);
        sweep(&dir, &name);
        let at = dir.join(&copy_name);
        let _ = std::fs::remove_file(&at);
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&at)
            .and_then(|mut f| f.write_all(bytes));
        match made {
            Ok(()) => {
                copy = Some(at);
                break;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&at);
            }
        }
    }
    let Some(copy) = copy else {
        return fail(format!(
            "cannot write a signed copy of {} beside it or in {}: macOS lets nexo debug only programs signed to allow it",
            exe.display(),
            crate::state_dir().display()
        ));
    };
    if let Err(e) = signed {
        // the system's own signer is the fallback: it ships with every macOS
        let plist = copy.with_file_name(format!("{copy_name}.plist"));
        let ran = std::fs::write(&plist, ENTITLEMENTS).and_then(|()| {
            std::process::Command::new("/usr/bin/codesign")
                .args(["-s", "-", "-f", "--entitlements"])
                .arg(&plist)
                .arg(&copy)
                .output()
        });
        let _ = std::fs::remove_file(&plist);
        let ok = ran.as_ref().is_ok_and(|o| o.status.success());
        if !ok {
            let _ = std::fs::remove_file(&copy);
            let why = match ran {
                Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                Err(e) => e.to_string(),
            };
            return fail(format!(
                "cannot sign a copy of {} to allow debugging ({e}; /usr/bin/codesign: {why}); macOS lets nexo debug only programs signed to allow it",
                exe.display()
            ));
        }
    }
    Ok(Prepared {
        run: copy.clone(),
        note: format!(
            "macOS: this runs {}, a copy of {} signed ad hoc with get-task-allow so macOS lets nexo debug it; nexo signs copies of your own files only, leaves the original as it is, and removes the copy when the session ends",
            copy.display(),
            exe.display()
        ),
        copy: Some(copy),
    })
}

/// Remove copies of `name` that sessions which no longer run left in `dir` (a debugger that was killed).
fn sweep(dir: &Path, name: &str) {
    let lead = format!(".{name}.nexo-dbg-");
    let Ok(list) = std::fs::read_dir(dir) else { return };
    for e in list.flatten() {
        let file = e.file_name();
        let pid = file.to_str().and_then(|f| f.strip_prefix(&lead)).and_then(|p| p.split('-').next());
        let Some(pid) = pid.and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        if !alive(pid) {
            let _ = std::fs::remove_file(e.path());
        }
    }
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
        me.note = Some(prog.note);
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

    /// Take the program's exceptions on a port of ours, keeping the ports it had for a detach.
    fn catch_exceptions(&mut self) -> Result<()> {
        let mut port = MACH_PORT_NULL;
        // SAFETY: plain Mach calls on ports of this process.
        let kr = unsafe {
            let kr = mach_port_allocate(me(), MACH_PORT_RIGHT_RECEIVE, &mut port);
            if kr == KERN_SUCCESS {
                mach_port_insert_right(me(), port, port, MACH_MSG_TYPE_MAKE_SEND)
            } else {
                kr
            }
        };
        if kr != KERN_SUCCESS {
            return fail(format!("cannot make an exception port: {}", kr_text(kr)));
        }
        self.port = port;
        let mut masks = [0u32; EXC_TYPES_COUNT];
        let mut ports = [0 as Port; EXC_TYPES_COUNT];
        let mut behaviors = [0 as c_int; EXC_TYPES_COUNT];
        let mut flavors = [0 as c_int; EXC_TYPES_COUNT];
        let mut n = EXC_TYPES_COUNT as u32;
        // SAFETY: every array holds EXC_TYPES_COUNT entries, as `n` says.
        let kr = unsafe {
            task_swap_exception_ports(
                self.task,
                EXC_MASK,
                port,
                EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
                THREAD_STATE_NONE,
                masks.as_mut_ptr(),
                &mut n,
                ports.as_mut_ptr(),
                behaviors.as_mut_ptr(),
                flavors.as_mut_ptr(),
            )
        };
        if kr != KERN_SUCCESS {
            return fail(format!("cannot take the program's exceptions: {}", kr_text(kr)));
        }
        self.old =
            (0..(n as usize).min(EXC_TYPES_COUNT)).map(|i| (masks[i], ports[i], behaviors[i], flavors[i])).collect();
        Ok(())
    }

    /// `PT_ATTACHEXC`: signals come as exceptions, and the kernel lets breakpoints change the program's code. It
    /// sends the program a SIGSTOP, which `first_stop` takes.
    fn trace(&mut self) -> Result<()> {
        // SAFETY: a plain system call on a process of this user.
        if unsafe { libc::ptrace(libc::PT_ATTACHEXC, self.pid, std::ptr::null_mut(), 0) } != 0 {
            let e = io::Error::last_os_error();
            return fail(format!("ptrace(PT_ATTACHEXC): {e} (another debugger may hold it)"));
        }
        Ok(())
    }

    /// Wait for the SIGSTOP that `PT_ATTACHEXC` sent and keep the program stopped on it; it is dropped when the
    /// program goes on.
    fn first_stop(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let Some(e) = self.receive(50) else {
                if let Some(ev) = self.reap() {
                    return fail(format!("it ended before it started ({ev:?})"));
                }
                continue;
            };
            let stop = e.exc == EXC_SOFTWARE && e.code[0] == EXC_SOFT_SIGNAL && e.code[1] == i64::from(libc::SIGSTOP);
            let thread = e.thread;
            let mut events = Vec::new();
            self.take(e, &mut events);
            if stop {
                if let Some(t) = self.th.get_mut(&thread) {
                    t.sig = 0;
                }
                self.refresh();
                return Ok(());
            }
            // anything before it (a trap in the program's own start) goes on as it would without a debugger
            if let Some(t) = self.th.get_mut(&thread) {
                t.fault = false;
            }
            self.answer(thread);
            self.go_on();
        }
        fail("it did not stop for the debugger within 10 seconds")
    }

    /// The next exception message, waiting at most `ms` milliseconds.
    fn receive(&mut self, ms: u32) -> Option<Exc> {
        loop {
            let mut buf = [0u64; 128];
            let head = buf.as_mut_ptr().cast::<MsgHeader>();
            // SAFETY: the buffer is 1 KiB, as the receive size says; the kernel writes the message into it.
            let kr = unsafe { mach_msg(head, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, 1024, self.port, ms, MACH_PORT_NULL) };
            if kr != KERN_SUCCESS {
                return None;
            }
            let b: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
            let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
            let i64_at = |o: usize| i64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
            let pend = Pend { reply: u32_at(8), bits: u32_at(0), id: u32_at(20) as i32, soft: false };
            if pend.id != MACH_EXCEPTION_RAISE || u32_at(4) < 84 {
                // not an exception request: nothing waits on it
                continue;
            }
            let exc = u32_at(60) as i32;
            let n = u32_at(64).min(2) as usize;
            let mut code = [0i64; 2];
            for (i, c) in code.iter_mut().enumerate().take(n) {
                *c = i64_at(68 + 8 * i);
            }
            let soft = exc == EXC_SOFTWARE && code[0] == EXC_SOFT_SIGNAL;
            return Some(Exc { thread: u32_at(28), task: u32_at(40), exc, code, pend: Pend { soft, ..pend } });
        }
    }

    /// Let a child the program forked go on without the debugger, as Linux lets one go: the program's bytes go back
    /// under the breakpoints it copied, its exception ports become the ones the program had before nexo took them,
    /// and the trap that brought it here is undone. Returns the reply: success resumes the child, failure lets the
    /// kernel make of the exception the signal the child would get without a debugger.
    fn release_child(&self, e: &Exc) -> Kr {
        let child = e.task;
        let byte_at = |addr: u64| {
            let (mut b, mut got) = ([0u8; 1], 0u64);
            // SAFETY: the kernel writes at most one byte into `b`.
            let kr = unsafe { mach_vm_read_overwrite(child, addr, 1, b.as_mut_ptr() as u64, &mut got) };
            (kr == KERN_SUCCESS && got == 1).then_some(b[0])
        };
        let mut pid: c_int = 0;
        // SAFETY: one int out.
        let known = unsafe { pid_for_task(child, &mut pid) } == KERN_SUCCESS;
        for (&addr, &byte) in &self.under {
            // only where the child still maps the program's own file: a child that ran exec has other code there
            let same = known && {
                let file = self.file_at(addr);
                !file.is_empty() && file == self.named(&region_file(pid, addr))
            };
            if same && byte != 0xcc && byte_at(addr) == Some(0xcc) {
                let _ = patch(child, addr, &[byte]);
            }
        }
        // SAFETY: the child's task port came with the message; the call copies the send rights in `old`, which
        // stay ours for the program's own detach.
        unsafe {
            task_set_exception_ports(child, EXC_MASK, MACH_PORT_NULL, EXCEPTION_DEFAULT, THREAD_STATE_NONE);
            for &(mask, port, behavior, flavor) in &self.old {
                task_set_exception_ports(child, mask, port, behavior, flavor);
            }
        }
        // the copied thread may carry the watch registers: nothing watches the child, and a hit would kill it
        let mut d = [0u64; 8];
        let watched = self.debug_state(e.thread, &mut d) && d[7] != 0;
        if watched {
            self.set_debug_state(e.thread, &[0; 8]);
        }
        let Ok(mut s) = self.state(e.thread) else { return KERN_FAILURE };
        match e.exc {
            EXC_BREAKPOINT if e.code[0] == EXC_I386_BPT => {
                // the int3 has run: go back to its address, where the program's instruction is again
                let at = s[16].wrapping_sub(1);
                if !self.under.contains_key(&at) || byte_at(at).is_none_or(|b| b == 0xcc) {
                    return KERN_FAILURE;
                }
                s[16] = at;
            }
            // a hit of the watch registers, or the trap flag of a step over the fork the thread was copied in
            EXC_BREAKPOINT if e.code[0] == EXC_I386_SGL && (watched || s[17] & TF != 0) => s[17] &= !TF,
            _ => return KERN_FAILURE,
        }
        if self.set_state(e.thread, &s).is_err() {
            return KERN_FAILURE;
        }
        KERN_SUCCESS
    }

    fn reply(&self, p: Pend, ret: Kr) {
        let mut r = Reply {
            head: MsgHeader {
                bits: p.bits & 0x1f,
                size: std::mem::size_of::<Reply>() as u32,
                remote: p.reply,
                local: MACH_PORT_NULL,
                voucher: MACH_PORT_NULL,
                id: p.id + 100,
            },
            ndr: NDR,
            ret,
        };
        // SAFETY: a complete reply message; the send-once right in `remote` is used up by the send.
        unsafe {
            mach_msg(
                &mut r.head,
                MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                std::mem::size_of::<Reply>() as u32,
                0,
                MACH_PORT_NULL,
                100,
                MACH_PORT_NULL,
            );
        }
    }

    /// Take in one exception: the task is stopped, and what it means joins `events` (nothing for a signal the
    /// kernel made of a fault already shown).
    fn take(&mut self, e: Exc, events: &mut Vec<Event>) {
        if e.task != self.task {
            // a child the program forked keeps our exception port and the breakpoints of the code it copied
            let ret = self.release_child(&e);
            self.reply(e.pend, ret);
            // SAFETY: both ports came with the message and are not ones we keep.
            unsafe {
                mach_port_deallocate(me(), e.task);
                mach_port_deallocate(me(), e.thread);
            }
            return;
        }
        // SAFETY: the message gave us one more reference on the task port; the one we keep holds the name.
        unsafe {
            mach_port_deallocate(me(), e.task);
        }
        if self.th.contains_key(&e.thread) {
            // SAFETY: as above; the name stays valid through the reference we already hold.
            unsafe { mach_port_deallocate(me(), e.thread) };
        }
        if self.running {
            // SAFETY: the task port is ours.
            unsafe { task_suspend(self.task) };
            self.running = false;
        }
        let tid = e.thread;
        let t = self.th.entry(tid).or_default();
        if let Some(old) = t.pend.replace(e.pend) {
            // one thread waits on one exception at a time; an old right is stale
            // SAFETY: a send-once right of ours.
            unsafe { mach_port_deallocate(me(), old.reply) };
        }
        let stopped = |stop| Event::Stopped { tid: tid as Tid, stop };
        match e.exc {
            EXC_SOFTWARE if e.pend.soft => {
                let sig = e.code[1] as i32;
                if t.pass != 0 && t.pass == sig {
                    t.pass = 0;
                    t.sig = sig;
                    return;
                }
                t.sig = sig;
                events.push(stopped(Stop::Fault(Fault {
                    number: sig as u32,
                    code: 0,
                    addr: None,
                    touch: None,
                    first_chance: true,
                })));
            }
            EXC_BREAKPOINT if e.code[0] == EXC_I386_BPT => events.push(stopped(Stop::Breakpoint)),
            EXC_BREAKPOINT if e.code[0] == EXC_I386_SGL => {
                let mut d = [0u64; 8];
                let hit = if self.debug_state(tid, &mut d) {
                    let hit = (d[6] & 0xf) as u8;
                    d[6] = 0;
                    self.set_debug_state(tid, &d);
                    hit
                } else {
                    0
                };
                // the trap flag stays set in the saved rflags: clear it, or the next run is one instruction too
                if let Ok(mut s) = self.state(tid) {
                    if s[17] & TF != 0 {
                        s[17] &= !TF;
                        let _ = self.set_state(tid, &s);
                    }
                }
                events.push(stopped(if hit != 0 { Stop::Watch(hit) } else { Stop::Step }));
            }
            EXC_BAD_ACCESS | EXC_BAD_INSTRUCTION | EXC_ARITHMETIC => {
                // the signal the kernel makes of it (bsd/uxkern/ux_exception.c and its i386 part)
                let sig = match e.exc {
                    EXC_BAD_ACCESS if matches!(e.code[0], KERN_INVALID_ADDRESS | EXC_I386_GPFLT) => libc::SIGSEGV,
                    EXC_BAD_ACCESS => libc::SIGBUS,
                    EXC_BAD_INSTRUCTION => libc::SIGILL,
                    _ => libc::SIGFPE,
                };
                t.sig = sig;
                t.fault = true;
                let addr = (e.exc == EXC_BAD_ACCESS && e.code[0] != EXC_I386_GPFLT).then_some(e.code[1] as u64);
                events.push(stopped(Stop::Fault(Fault {
                    number: sig as u32,
                    code: e.code[0] as i32,
                    addr,
                    touch: None,
                    first_chance: true,
                })));
            }
            _ => {
                // an EXC_BREAKPOINT of another kind, or EXC_SOFTWARE that is not a signal: SIGTRAP is what it becomes
                t.sig = libc::SIGTRAP;
                t.fault = true;
                events.push(stopped(Stop::Fault(Fault {
                    number: libc::SIGTRAP as u32,
                    code: 0,
                    addr: None,
                    touch: None,
                    first_chance: true,
                })));
            }
        }
    }

    /// Reply to the exception `tid` waits on, with the signal it carries going on.
    fn answer(&mut self, tid: Port) {
        let pid = self.pid;
        let Some(t) = self.th.get_mut(&tid) else { return };
        let Some(p) = t.pend.take() else { return };
        let sig = std::mem::take(&mut t.sig);
        if std::mem::take(&mut t.fault) {
            // "not handled": the kernel makes the fault the signal it would be without a debugger
            t.pass = sig;
            self.reply(p, KERN_FAILURE);
            return;
        }
        if p.soft {
            // SAFETY: PT_THUPDATE names the thread by its port in this process and sets the signal it gets (0: none).
            unsafe { libc::ptrace(libc::PT_THUPDATE, pid, tid as usize as *mut c_char, sig) };
        }
        self.reply(p, KERN_SUCCESS);
    }

    /// Let the task run again after a stop nobody needs to see.
    fn go_on(&mut self) {
        if !self.running {
            // SAFETY: undoes our own task_suspend.
            unsafe { task_resume(self.task) };
            self.running = true;
        }
    }

    /// The thread that waits on a signal (`EXC_SOFT_SIGNAL`), with the signal it is to get.
    fn waits_on_signal(&self) -> Option<(Port, c_int)> {
        self.th.iter().find(|(_, t)| t.pend.is_some_and(|p| p.soft)).map(|(k, t)| (*k, t.sig))
    }

    /// Reply to every exception but the signal stops: breakpoints and steps go on, faults become their signals.
    fn answer_all_but_signals(&mut self) {
        let wait: Vec<Port> =
            self.th.iter().filter(|(_, t)| t.pend.is_some_and(|p| !p.soft)).map(|(k, _)| *k).collect();
        for tid in wait {
            self.answer(tid);
        }
    }

    fn release_held(&mut self) {
        for t in std::mem::take(&mut self.held) {
            // SAFETY: undoes our own thread_suspend; the task stays suspended.
            unsafe { thread_resume(t) };
        }
    }

    /// Whether the program ended; the event that says how.
    fn reap(&mut self) -> Option<Event> {
        let mut st = 0;
        // SAFETY: a plain wait on the program, which is our child (PT_ATTACHEXC makes the debugger its parent).
        let r = unsafe { libc::waitpid(self.pid, &mut st, libc::WNOHANG) };
        let ev = if r == self.pid && libc::WIFEXITED(st) {
            Event::Exited { code: libc::WEXITSTATUS(st) }
        } else if r == self.pid && libc::WIFSIGNALED(st) {
            Event::Killed { signal: libc::WTERMSIG(st) }
        } else if r < 0 && !alive(self.pid) {
            Event::Exited { code: -1 }
        } else {
            return None;
        };
        // A traced child reaped once goes back to the parent it had before the attach, which for a program nexo
        // started is this process again, still a zombie: the second wait frees it.
        // SAFETY: as above.
        unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), libc::WNOHANG) };
        self.ended();
        Some(ev)
    }

    fn ended(&mut self) {
        self.gone = true;
        self.running = false;
        self.held.clear();
        for (tid, t) in std::mem::take(&mut self.th) {
            // SAFETY: rights of ours on ports of a task that is gone.
            unsafe {
                if let Some(p) = t.pend {
                    mach_port_deallocate(me(), p.reply);
                }
                mach_port_deallocate(me(), tid);
            }
        }
    }

    /// Read the thread list again: names new to us are kept, the ones of threads that ended are let go.
    fn refresh(&mut self) {
        let mut list: libc::thread_act_array_t = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: the kernel hands back an array of `n` send rights in memory of ours, which is freed below.
        let now: Vec<Port> = unsafe {
            if libc::task_threads(self.task, &mut list, &mut n) != KERN_SUCCESS || list.is_null() {
                return;
            }
            let v = std::slice::from_raw_parts(list, n as usize).to_vec();
            libc::vm_deallocate(me(), list as libc::vm_address_t, n as usize * 4);
            v
        };
        for &t in &now {
            match self.th.entry(t) {
                // SAFETY: one more reference on a name we already hold.
                Entry::Occupied(_) => unsafe {
                    mach_port_deallocate(me(), t);
                },
                Entry::Vacant(v) => {
                    v.insert(Th::default());
                }
            }
        }
        let dead: Vec<Port> = self.th.keys().filter(|k| !now.contains(k)).copied().collect();
        for d in dead {
            if let Some(t) = self.th.remove(&d) {
                // SAFETY: rights of ours on a thread that ended.
                unsafe {
                    if let Some(p) = t.pend {
                        mach_port_deallocate(me(), p.reply);
                    }
                    mach_port_deallocate(me(), d);
                }
            }
        }
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
            let n = self.read(addr + out.len() as u64, &mut b);
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
            let (Some(base), Some(name)) = (self.read_word(array + 24 * i), self.read_word(array + 24 * i + 8)) else {
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

impl Target for Mac {
    fn os(&self) -> Os {
        Os::MacOS
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
        Ok(Regs::from_mach(&self.state(tid as Port)?))
    }

    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()> {
        self.set_state(tid as Port, &regs.to_mach())
    }

    fn vector_state(&mut self, tid: Tid) -> Option<Vec<u8>> {
        // The AVX state (the float state, then the upper ymm halves) where the CPU has it, else the float state.
        let flavors = [(X86_AVX_STATE64, X86_AVX_STATE64_COUNT), (X86_FLOAT_STATE64, X86_FLOAT_STATE64_COUNT)];
        for (flavor, count) in flavors {
            let mut s = vec![0u32; count as usize];
            let mut n = count;
            // SAFETY: `s` holds the `n` words the call may write.
            let kr = unsafe { thread_get_state(tid as Port, flavor, s.as_mut_ptr(), &mut n) };
            if kr == KERN_SUCCESS {
                return Some(s[..n as usize].iter().flat_map(|w| w.to_le_bytes()).collect());
            }
        }
        None
    }

    fn set_vector_state(&mut self, tid: Tid, state: &[u8]) -> Result<()> {
        let s: Vec<u32> = state.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        // the flavor `vector_state` read, told by its length
        let flavor = if s.len() >= X86_AVX_STATE64_COUNT as usize { X86_AVX_STATE64 } else { X86_FLOAT_STATE64 };
        // SAFETY: `s` holds the words `thread_get_state` gave.
        let kr = unsafe { thread_set_state(tid as Port, flavor, s.as_ptr(), s.len() as u32) };
        if kr != KERN_SUCCESS {
            return fail(format!("set the vector registers of thread {tid}: {}", kr_text(kr)));
        }
        Ok(())
    }

    fn take_signal(&mut self, tid: Tid) -> u64 {
        // the fault or signal the thread stopped on is not handed to the program when it runs again
        let Some(t) = self.th.get_mut(&(tid as Port)) else { return 0 };
        t.fault = false;
        std::mem::take(&mut t.sig) as u32 as u64
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let n = (buf.len() - done).min((0x1000 - (a & 0xfff)) as usize);
            let mut got = 0u64;
            // SAFETY: the kernel writes at most `n` bytes into the buffer from `done` on.
            let kr =
                unsafe { mach_vm_read_overwrite(self.task, a, n as u64, buf[done..].as_mut_ptr() as u64, &mut got) };
            if kr != KERN_SUCCESS || got == 0 {
                break;
            }
            done += got as usize;
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        patch(self.task, addr, bytes)
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let mut out: Vec<Map> = Vec::new();
        let mut addr = 0u64;
        // depth 0: the shared library cache is one submap region; looking into it would list thousands of pieces
        while let Some((start, end, prot, offset, tag, _)) = self.region(addr, &mut 0) {
            if end <= start {
                break;
            }
            addr = end;
            let mut path = self.file_at(start);
            if path.is_empty() {
                if tag == TAG_STACK {
                    path = "[stack]".into();
                } else if TAG_HEAP.contains(&tag) {
                    path = "[heap]".into();
                }
            }
            let m = Map {
                start,
                end,
                read: prot & VM_PROT_READ != 0,
                write: prot & VM_PROT_WRITE != 0,
                exec: prot & VM_PROT_EXECUTE != 0,
                offset,
                path,
            };
            match out.last_mut() {
                Some(l)
                    if l.end == m.start
                        && (l.read, l.write, l.exec) == (m.read, m.write, m.exec)
                        && l.path == m.path =>
                {
                    l.end = m.end;
                }
                _ => out.push(m),
            }
        }
        Ok(out)
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        let mut out = self.dyld_images();
        if out.is_empty() {
            out = self.headers();
        }
        if let Some(i) = out.iter().position(|m| m.path == self.exe) {
            out[..=i].rotate_right(1);
        } else if let Some(i) = out.iter().position(|m| {
            let mut h = [0u8; 16];
            // the program is the one MH_EXECUTE image; for an attach it has no name given
            self.read(m.base, &mut h) == 16 && u32::from_le_bytes([h[12], h[13], h[14], h[15]]) == MH_EXECUTE
        }) {
            out[..=i].rotate_right(1);
        }
        Ok(out)
    }

    fn entry(&mut self) -> Option<u64> {
        let base = match self.exe_base {
            Some(b) => b,
            None => {
                let b = self.modules().ok()?.first()?.base;
                self.exe_base = Some(b);
                b
            }
        };
        let mut h = [0u8; 32];
        if self.read(base, &mut h) < 32 || u32::from_le_bytes([h[0], h[1], h[2], h[3]]) != MH_MAGIC_64 {
            return None;
        }
        let size = u32::from_le_bytes([h[20], h[21], h[22], h[23]]) as usize;
        let mut cmds = vec![0u8; size.min(1 << 20)];
        let n = self.read(base + 32, &mut cmds);
        let cmds = &cmds[..n];
        let mut at = 0;
        while at + 16 <= cmds.len() {
            let cmd = u32::from_le_bytes(cmds[at..at + 4].try_into().ok()?);
            let len = u32::from_le_bytes(cmds[at + 4..at + 8].try_into().ok()?) as usize;
            if cmd == LC_MAIN {
                // entryoff counts from the start of __TEXT, which is where the header sits
                return Some(base + u64::from_le_bytes(cmds[at + 8..at + 16].try_into().ok()?));
            }
            if len < 8 {
                break;
            }
            at += len;
        }
        None
    }

    fn set_patches(&mut self, patches: &[(u64, u8)]) {
        // kept after a breakpoint leaves: a child forked while it was in still has it (see `under`)
        self.under.extend(patches.iter().copied());
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        self.watch = *slots;
        let d = self.debug_regs();
        for &tid in self.th.keys() {
            if !self.set_debug_state(tid, &d) {
                return fail(format!("hardware watch: the debug registers of thread {tid} cannot be set"));
            }
        }
        // threads born later start from the task's debug registers
        // SAFETY: 8 words, the 16 halves the count says.
        unsafe { task_set_state(self.task, X86_DEBUG_STATE64, d.as_ptr().cast(), X86_DEBUG_STATE64_COUNT) };
        Ok(())
    }

    fn protect(&mut self, addr: u64, len: u64, prot: u8) -> Result<()> {
        // the bits are the same as VM_PROT_READ, VM_PROT_WRITE, VM_PROT_EXECUTE
        // SAFETY: protection changes on pages of the task's own.
        let kr = unsafe { mach_vm_protect(self.task, addr, len, 0, c_int::from(prot & 7)) };
        if kr != KERN_SUCCESS {
            return fail(format!("mach_vm_protect({addr:#x}, {len:#x}): {}", kr_text(kr)));
        }
        Ok(())
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        if self.gone {
            return fail("the program has ended");
        }
        if self.running {
            return Ok(());
        }
        match how {
            Resume::All => {
                for tid in self.th.keys().copied().collect::<Vec<_>>() {
                    self.answer(tid);
                }
            }
            Resume::Step(one) => {
                let one = one as Port;
                if !self.th.contains_key(&one) {
                    return fail(format!("no thread {one}"));
                }
                let mut s = self.state(one)?;
                s[17] |= TF;
                self.set_state(one, &s)?;
                for &t in self.th.keys() {
                    // SAFETY: a thread of the program; released at the next stop.
                    if t != one && unsafe { thread_suspend(t) } == KERN_SUCCESS {
                        self.held.push(t);
                    }
                }
                self.answer(one);
            }
        }
        self.go_on();
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if self.gone {
            return fail("the program has ended");
        }
        let mut events = Vec::new();
        if !self.running {
            if !self.want_stop {
                return fail("the program is stopped: nothing to wait for");
            }
            // what came before the stop counts first
            while let Some(e) = self.receive(0) {
                self.take(e, &mut events);
            }
            self.want_stop = false;
            if events.is_empty() {
                let tid = self.th.keys().next().copied().unwrap_or_default();
                events.push(Event::Stopped { tid: tid as Tid, stop: Stop::Interrupt });
            }
            self.release_held();
            self.refresh();
            return Ok(events);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = left.as_millis().clamp(0, 10) as u32;
            if let Some(e) = self.receive(ms) {
                self.take(e, &mut events);
                while let Some(e) = self.receive(0) {
                    self.take(e, &mut events);
                }
            }
            if let Some(ev) = self.reap() {
                events.push(ev);
                return Ok(events);
            }
            if !self.running {
                if events.is_empty() {
                    // only a signal the kernel made of a fault already shown: it goes on
                    for tid in self.th.keys().copied().collect::<Vec<_>>() {
                        if self.th.get(&tid).is_some_and(|t| t.pend.is_some_and(|p| p.soft)) {
                            self.answer(tid);
                        }
                    }
                    self.go_on();
                } else {
                    self.release_held();
                    self.refresh();
                    return Ok(events);
                }
            }
            if Instant::now() >= deadline {
                return Ok(events);
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        if self.gone || !self.running {
            return fail("the program is already stopped");
        }
        // SAFETY: the task port is ours.
        let kr = unsafe { task_suspend(self.task) };
        if kr != KERN_SUCCESS {
            return fail(format!("stop: {}", kr_text(kr)));
        }
        self.running = false;
        self.want_stop = true;
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
        if self.running {
            // SAFETY: the task port is ours.
            unsafe { task_suspend(self.task) };
            self.running = false;
        }
        let mut events = Vec::new();
        while let Some(e) = self.receive(0) {
            self.take(e, &mut events);
        }
        self.release_held();
        self.watch = [None; 4];
        let d = [0u64; 8];
        for &tid in self.th.keys() {
            self.set_debug_state(tid, &d);
        }
        // SAFETY: the task port is ours; threads started later get no watchpoint either.
        unsafe { task_set_state(self.task, X86_DEBUG_STATE64, d.as_ptr().cast(), X86_DEBUG_STATE64_COUNT) };
        // macOS takes PT_DETACH only while a thread waits on a signal (the process is in its stopped state then).
        // A stop at a breakpoint, a step or an interrupt is not one: nexo sends SIGSTOP and drops it on the way
        // out. A fault becomes its signal and goes to the program with the detach, as it would without a debugger.
        let ours = self.waits_on_signal().is_none() && !self.th.values().any(|t| t.fault && t.pend.is_some());
        self.answer_all_but_signals();
        if ours {
            // SAFETY: a signal to the process we trace.
            unsafe { libc::kill(self.pid, libc::SIGSTOP) };
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let sig = loop {
            match self.waits_on_signal() {
                Some((_, sig)) if !ours => break Some(sig),
                Some((_, libc::SIGSTOP)) => break Some(0),
                // a signal of the program's own came first: it gets it
                Some((tid, _)) => self.answer(tid),
                None => {}
            }
            if Instant::now() > deadline {
                break None;
            }
            self.go_on();
            match self.receive(50) {
                Some(e) => {
                    self.take(e, &mut events);
                    self.answer_all_but_signals();
                }
                None => {
                    if self.reap().is_some() {
                        return Ok(());
                    }
                }
            }
        };
        // SAFETY: give the program back the exception ports it had; the rights handed to us go with them.
        unsafe {
            task_set_exception_ports(self.task, EXC_MASK, MACH_PORT_NULL, EXCEPTION_DEFAULT, THREAD_STATE_NONE);
            for (mask, port, behavior, flavor) in std::mem::take(&mut self.old) {
                task_set_exception_ports(self.task, mask, port, behavior, flavor);
                if port != MACH_PORT_NULL {
                    mach_port_deallocate(me(), port);
                }
            }
        }
        let done = match sig {
            // SAFETY: data is the signal the program gets as it goes on (0: none).
            Some(sig) => unsafe {
                libc::ptrace(libc::PT_DETACH, self.pid, std::ptr::without_provenance_mut::<c_char>(1), sig) == 0
            },
            None => false,
        };
        let why = io::Error::last_os_error();
        // the waiting threads go on: the signal stop with what PT_DETACH said, anything else as it was
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            let p = self.th.get_mut(&tid).and_then(|t| if t.fault { None } else { t.pend.take() });
            match p {
                Some(p) => self.reply(p, KERN_SUCCESS),
                None => self.answer(tid),
            }
        }
        if !self.running {
            // SAFETY: undoes our own suspension (PT_DETACH may already have).
            unsafe { task_resume(self.task) };
        }
        self.ended();
        if !done {
            let why = if sig.is_none() {
                "it did not stop for the SIGSTOP within 3 seconds".to_string()
            } else {
                why.to_string()
            };
            return fail(format!(
                "ptrace(PT_DETACH): {why}; the program runs on, but stays traced by this nexo process"
            ));
        }
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        // SAFETY: the process is the one we trace.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        // every waiting thread is let go and the task runs, so the kernel can end it
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            if let Some(t) = self.th.get_mut(&tid) {
                (t.fault, t.sig) = (false, 0);
            }
            self.answer(tid);
        }
        self.release_held();
        self.go_on();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            while let Some(e) = self.receive(0) {
                // a late exception of a dying thread
                self.reply(e.pend, KERN_SUCCESS);
                // SAFETY: rights that came with the message.
                unsafe {
                    mach_port_deallocate(me(), e.task);
                    mach_port_deallocate(me(), e.thread);
                }
            }
            if self.reap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.ended();
        Ok(())
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }
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
