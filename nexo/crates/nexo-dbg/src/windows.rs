//! The Windows x86-64 backend: the Win32 Debug API (`CreateProcessW` with `DEBUG_ONLY_THIS_PROCESS`,
//! `WaitForDebugEvent`, `ContinueDebugEvent`, thread contexts, `ReadProcessMemory`). It never hides itself
//! from the program: `IsDebuggerPresent` answers true and the program's own checks are left alone.
//!
//! Not run on Windows yet: it builds and passes clippy for `x86_64-pc-windows-gnu` only.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetHandleInformation, DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, NTSTATUS, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, ReadFile, WriteFile};
use windows_sys::Win32::System::Diagnostics::Debug::*;
use windows_sys::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::*;

use crate::platform::{Access, Event, Fault, HwSlot, Launch, Map, Module, Os, Resume, Stop, Target, Tid, Touch};
use crate::regs::Regs;
use crate::{fail, Result};

const EXC_BREAKPOINT: u32 = 0x8000_0003;
const EXC_SINGLE_STEP: u32 = 0x8000_0004;
const EXC_ACCESS_VIOLATION: u32 = 0xC000_0005;
const TF: u64 = 0x100;
const KEEP_OUTPUT: usize = 1 << 20;

#[repr(C, align(16))]
struct Ctx(CONTEXT);

fn ctx(flags: u32) -> Ctx {
    // SAFETY: CONTEXT is plain data; all zero is a valid value.
    let mut c: Ctx = unsafe { std::mem::zeroed() };
    c.0.ContextFlags = flags;
    c
}

const CTX_REGS: u32 = CONTEXT_CONTROL_AMD64 | CONTEXT_INTEGER_AMD64 | CONTEXT_SEGMENTS_AMD64;
const CTX_DEBUG: u32 = CONTEXT_DEBUG_REGISTERS_AMD64;

/// A handle that may be moved to the thread reading a pipe.
struct Sendable(usize);

pub struct Windows {
    pid: u32,
    process: HANDLE,
    launched: bool,
    threads: BTreeMap<Tid, HANDLE>,
    modules: Vec<Module>,
    entry: Option<u64>,
    watch: [Option<HwSlot>; 4],
    /// The event the program is stopped at: it is answered when the program goes on.
    pending: Option<(u32, u32, NTSTATUS)>,
    /// Threads held back while one steps.
    held: Vec<HANDLE>,
    stepping: Option<Tid>,
    seen_first_break: bool,
    interrupting: bool,
    exited: bool,
    stdin: Option<HANDLE>,
    out: Arc<Mutex<Vec<u8>>>,
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

fn last_error(what: &str) -> crate::Error {
    // SAFETY: reads the calling thread's last error.
    crate::Error::Fail(format!("{what} failed (Windows error {})", unsafe { GetLastError() }))
}

impl Windows {
    pub fn launch(l: &Launch) -> Result<Windows> {
        // standard input and output as pipes
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let (mut in_r, mut in_w, mut out_r, mut out_w): (HANDLE, HANDLE, HANDLE, HANDLE) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: plain Win32 calls with valid out-pointers; handles are closed below.
        unsafe {
            if CreatePipe(&mut in_r, &mut in_w, &sa, 0) == 0 || CreatePipe(&mut out_r, &mut out_w, &sa, 0) == 0 {
                return Err(last_error("CreatePipe"));
            }
            SetHandleInformation(in_w, HANDLE_FLAG_INHERIT, 0);
            SetHandleInformation(out_r, HANDLE_FLAG_INHERIT, 0);
        }
        let mut cmd = quote(&l.exe.display().to_string());
        for a in &l.args {
            cmd.push(' ');
            cmd.push_str(&quote(a));
        }
        let mut cmdw: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let mut env: Vec<u16> = Vec::new();
        for (k, v) in l.env.iter().cloned().chain(std::env::vars().filter(|(k, _)| !l.env.iter().any(|(n, _)| n == k)))
        {
            env.extend(format!("{k}={v}").encode_utf16());
            env.push(0);
        }
        env.push(0);
        let cwd = l.cwd.as_ref().map(|d| wide(d.as_os_str()));
        // SAFETY: zeroed STARTUPINFOW / PROCESS_INFORMATION are valid before the call fills them.
        let (mut si, mut pi): (STARTUPINFOW, PROCESS_INFORMATION) = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = in_r;
        si.hStdOutput = out_w;
        si.hStdError = out_w;
        // SAFETY: every pointer is valid for the call; the buffers outlive it.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmdw.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                DEBUG_ONLY_THIS_PROCESS | CREATE_UNICODE_ENVIRONMENT,
                env.as_ptr() as *const c_void,
                cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                &si,
                &mut pi,
            )
        };
        // SAFETY: the child holds its own copies of these ends.
        unsafe {
            CloseHandle(in_r);
            CloseHandle(out_w);
        }
        if ok == 0 {
            let e = last_error(&format!("starting {}", l.exe.display()));
            // SAFETY: closing handles made above.
            unsafe {
                CloseHandle(in_w);
                CloseHandle(out_r);
            }
            return Err(e);
        }
        // SAFETY: the process is ours; it dies with this debugger.
        unsafe {
            DebugSetProcessKillOnExit(1);
            CloseHandle(pi.hThread);
        }
        let out = Arc::new(Mutex::new(Vec::new()));
        let (sink, h) = (out.clone(), Sendable(out_r as usize));
        std::thread::spawn(move || {
            let h = h.0 as HANDLE;
            let mut buf = [0u8; 4096];
            loop {
                let mut n = 0u32;
                // SAFETY: reading into a local buffer from a pipe handle this thread owns.
                let ok = unsafe { ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
                if ok == 0 || n == 0 {
                    break;
                }
                let mut o = sink.lock().unwrap_or_else(|e| e.into_inner());
                o.extend_from_slice(&buf[..n as usize]);
                if o.len() > KEEP_OUTPUT {
                    let cut = o.len() - KEEP_OUTPUT;
                    o.drain(..cut);
                }
            }
            // SAFETY: the pipe's read end is closed once.
            unsafe { CloseHandle(h) };
        });
        let mut w = Windows::blank(pi.dwProcessId, pi.hProcess, true, out);
        w.stdin = Some(in_w);
        w.until_created()?;
        Ok(w)
    }

    pub fn attach(pid: u32) -> Result<Windows> {
        // SAFETY: opening a process by id with the rights the debugger needs.
        let process = unsafe {
            OpenProcess(
                PROCESS_VM_READ
                    | PROCESS_VM_WRITE
                    | PROCESS_VM_OPERATION
                    | PROCESS_QUERY_INFORMATION
                    | PROCESS_SUSPEND_RESUME
                    | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        if process.is_null() {
            return Err(last_error(&format!("opening process {pid} (is it yours?)")));
        }
        // SAFETY: attaches as a debugger; the program is let go, not killed, at the end.
        unsafe {
            if DebugActiveProcess(pid) == 0 {
                let e = last_error(&format!("attaching to {pid}"));
                CloseHandle(process);
                return Err(e);
            }
            DebugSetProcessKillOnExit(0);
        }
        let mut w = Windows::blank(pid, process, false, Arc::new(Mutex::new(Vec::new())));
        w.until_created()?;
        Ok(w)
    }

    fn blank(pid: u32, process: HANDLE, launched: bool, out: Arc<Mutex<Vec<u8>>>) -> Windows {
        Windows {
            pid,
            process,
            launched,
            threads: BTreeMap::new(),
            modules: Vec::new(),
            entry: None,
            watch: [None; 4],
            pending: None,
            held: Vec::new(),
            stepping: None,
            seen_first_break: false,
            interrupting: false,
            exited: false,
            stdin: None,
            out,
        }
    }

    /// Wait for the program's creation event (and, when attached, the break the system makes), leaving it stopped.
    fn until_created(&mut self) -> Result<()> {
        let mut ev = DEBUG_EVENT::default();
        let until = Instant::now() + Duration::from_secs(10);
        let mut created = false;
        while Instant::now() < until {
            // SAFETY: ev is a valid out-structure.
            if unsafe { WaitForDebugEvent(&mut ev, 200) } == 0 {
                continue;
            }
            self.book(&ev);
            if ev.dwDebugEventCode == CREATE_PROCESS_DEBUG_EVENT {
                created = true;
                if self.launched {
                    self.pending = Some((ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE));
                    return Ok(());
                }
            }
            if created && ev.dwDebugEventCode == EXCEPTION_DEBUG_EVENT {
                // attached: the system's break is the stop; it is answered when the program goes on
                self.seen_first_break = true;
                self.pending = Some((ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE));
                return Ok(());
            }
            // SAFETY: answering the event just received.
            unsafe { ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE) };
        }
        fail("the program did not report in 10 seconds")
    }

    /// Keep the lists of threads and files up to date for an event.
    fn book(&mut self, ev: &DEBUG_EVENT) {
        // SAFETY: the union member read matches the event code.
        unsafe {
            match ev.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let i = ev.u.CreateProcessInfo;
                    self.threads.insert(ev.dwThreadId, i.hThread);
                    self.modules.insert(0, Module { path: handle_path(i.hFile), base: i.lpBaseOfImage as u64 });
                    self.entry = i.lpStartAddress.map(|f| f as usize as u64);
                    if !i.hFile.is_null() {
                        CloseHandle(i.hFile);
                    }
                }
                CREATE_THREAD_DEBUG_EVENT => {
                    let h = ev.u.CreateThread.hThread;
                    self.threads.insert(ev.dwThreadId, h);
                    let w = self.watch;
                    let _ = self.put_watch(h, &w);
                }
                EXIT_THREAD_DEBUG_EVENT => {
                    self.threads.remove(&ev.dwThreadId);
                }
                LOAD_DLL_DEBUG_EVENT => {
                    let i = ev.u.LoadDll;
                    self.modules.push(Module { path: handle_path(i.hFile), base: i.lpBaseOfDll as u64 });
                    if !i.hFile.is_null() {
                        CloseHandle(i.hFile);
                    }
                }
                UNLOAD_DLL_DEBUG_EVENT => {
                    let b = ev.u.UnloadDll.lpBaseOfDll as u64;
                    self.modules.retain(|m| m.base != b);
                }
                _ => {}
            }
        }
    }

    fn thread(&self, tid: Tid) -> Result<HANDLE> {
        match self.threads.get(&tid) {
            Some(h) => Ok(*h),
            None => fail(format!("no thread {tid}")),
        }
    }

    fn put_watch(&self, h: HANDLE, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        let mut c = ctx(CTX_DEBUG);
        // SAFETY: c is aligned and sized for CONTEXT; h is a thread of the program, stopped.
        unsafe {
            if GetThreadContext(h, &mut c.0) == 0 {
                return Err(last_error("GetThreadContext"));
            }
        }
        let mut dr7 = 0u64;
        let mut addrs = [0u64; 4];
        for (i, s) in slots.iter().enumerate() {
            let Some(s) = s else { continue };
            addrs[i] = s.addr;
            let rw = match s.access {
                Access::Exec => 0,
                Access::Write => 1,
                Access::ReadWrite => 3,
            };
            let len = match s.len {
                1 => 0,
                2 => 1,
                8 => 2,
                _ => 3,
            };
            dr7 |= 1 << (2 * i);
            dr7 |= (rw | (len << 2)) << (16 + 4 * i);
        }
        c.0.Dr0 = addrs[0];
        c.0.Dr1 = addrs[1];
        c.0.Dr2 = addrs[2];
        c.0.Dr3 = addrs[3];
        c.0.Dr6 = 0;
        c.0.Dr7 = dr7;
        // SAFETY: as above.
        if unsafe { SetThreadContext(h, &c.0) } == 0 {
            return Err(last_error("SetThreadContext"));
        }
        Ok(())
    }

    fn release_held(&mut self) {
        for h in self.held.drain(..) {
            // SAFETY: threads suspended by `resume` for a single step.
            unsafe { ResumeThread(h) };
        }
        self.stepping = None;
    }

    fn stop_of(&mut self, ev: &DEBUG_EVENT) -> Option<Event> {
        // SAFETY: the member read matches the event code.
        let (code, rec, first) = unsafe {
            let e = ev.u.Exception;
            (e.ExceptionRecord.ExceptionCode as u32, e.ExceptionRecord, e.dwFirstChance != 0)
        };
        let tid = ev.dwThreadId;
        self.pending = Some((ev.dwProcessId, tid, DBG_CONTINUE));
        match code {
            EXC_BREAKPOINT => {
                if !self.seen_first_break {
                    // the loader's own break, before the program's first instruction
                    self.seen_first_break = true;
                    return None;
                }
                if std::mem::take(&mut self.interrupting) {
                    return Some(Event::Stopped { tid, stop: Stop::Interrupt });
                }
                Some(Event::Stopped { tid, stop: Stop::Breakpoint })
            }
            EXC_SINGLE_STEP => {
                if self.stepping == Some(tid) {
                    return Some(Event::Stopped { tid, stop: Stop::Step });
                }
                let h = self.threads.get(&tid).copied()?;
                let mut c = ctx(CTX_DEBUG);
                // SAFETY: as in put_watch.
                let bits = if unsafe { GetThreadContext(h, &mut c.0) } != 0 { (c.0.Dr6 & 0xf) as u8 } else { 0 };
                Some(Event::Stopped { tid, stop: if bits != 0 { Stop::Watch(bits) } else { Stop::Step } })
            }
            _ => {
                self.pending = Some((ev.dwProcessId, tid, DBG_EXCEPTION_NOT_HANDLED));
                let is_av = code == EXC_ACCESS_VIOLATION && rec.NumberParameters >= 2;
                let touch = is_av.then(|| match rec.ExceptionInformation[0] {
                    0 => Touch::Read,
                    1 => Touch::Write,
                    _ => Touch::Exec,
                });
                let addr = if is_av { Some(rec.ExceptionInformation[1] as u64) } else { None };
                Some(Event::Stopped {
                    tid,
                    stop: Stop::Fault(Fault { number: code, code: 0, addr, touch, first_chance: first }),
                })
            }
        }
    }
}

fn handle_path(h: HANDLE) -> String {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return String::new();
    }
    let mut buf = [0u16; 1024];
    // SAFETY: buf is valid for its length; h is a file handle from a debug event.
    let n = unsafe { GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, 0) } as usize;
    let s = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

impl Target for Windows {
    fn os(&self) -> Os {
        Os::Windows
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn launched(&self) -> bool {
        self.launched
    }

    fn threads(&self) -> Vec<Tid> {
        self.threads.keys().copied().collect()
    }

    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        let h = self.thread(tid)?;
        let mut c = ctx(CTX_REGS);
        // SAFETY: c is aligned and sized for CONTEXT; the thread is stopped.
        if unsafe { GetThreadContext(h, &mut c.0) } == 0 {
            return Err(last_error("GetThreadContext"));
        }
        let x = &c.0;
        Ok(Regs {
            gp: [
                x.Rax, x.Rbx, x.Rcx, x.Rdx, x.Rsi, x.Rdi, x.Rbp, x.Rsp, x.R8, x.R9, x.R10, x.R11, x.R12, x.R13, x.R14,
                x.R15,
            ],
            rip: x.Rip,
            eflags: x.EFlags as u64,
            seg: [x.SegCs as u64, x.SegSs as u64, x.SegDs as u64, x.SegEs as u64, x.SegFs as u64, x.SegGs as u64],
            fs_base: 0,
            gs_base: 0,
            orig_rax: 0,
            has_bases: false,
        })
    }

    fn set_regs(&mut self, tid: Tid, r: &Regs) -> Result<()> {
        let h = self.thread(tid)?;
        let mut c = ctx(CTX_REGS);
        // SAFETY: as in `regs`.
        if unsafe { GetThreadContext(h, &mut c.0) } == 0 {
            return Err(last_error("GetThreadContext"));
        }
        let x = &mut c.0;
        [
            x.Rax, x.Rbx, x.Rcx, x.Rdx, x.Rsi, x.Rdi, x.Rbp, x.Rsp, x.R8, x.R9, x.R10, x.R11, x.R12, x.R13, x.R14,
            x.R15,
        ] = r.gp;
        x.Rip = r.rip;
        x.EFlags = r.eflags as u32;
        // SAFETY: as above.
        if unsafe { SetThreadContext(h, &c.0) } == 0 {
            return Err(last_error("SetThreadContext"));
        }
        Ok(())
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0usize;
        while done < buf.len() {
            // never across a page edge in one call: a failed call still says how far it got
            let a = addr.wrapping_add(done as u64);
            let to_edge = (0x1000 - (a & 0xfff)) as usize;
            let want = to_edge.min(buf.len() - done);
            let mut got = 0usize;
            // SAFETY: the destination is a live slice of at least `want` bytes.
            let ok = unsafe {
                ReadProcessMemory(self.process, a as *const c_void, buf[done..].as_mut_ptr().cast(), want, &mut got)
            };
            done += got;
            if ok == 0 || got < want {
                break;
            }
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        let mut put = 0usize;
        // SAFETY: the source is a live slice; the target process is stopped.
        let ok = unsafe {
            WriteProcessMemory(self.process, addr as *const c_void, bytes.as_ptr().cast(), bytes.len(), &mut put)
        };
        if ok == 0 || put != bytes.len() {
            return Err(last_error(&format!("writing {} bytes at {addr:#x}", bytes.len())));
        }
        // SAFETY: tells the processor that code may have changed.
        unsafe { FlushInstructionCache(self.process, addr as *const c_void, bytes.len()) };
        Ok(())
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let mut out: Vec<Map> = Vec::new();
        let mut at = 0u64;
        loop {
            // SAFETY: a zeroed MEMORY_BASIC_INFORMATION is valid before the call fills it.
            let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: m is a valid out-structure of the size given.
            let n = unsafe {
                VirtualQueryEx(
                    self.process,
                    at as *const c_void,
                    &mut m,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if n == 0 {
                break;
            }
            let (start, size) = (m.BaseAddress as u64, m.RegionSize as u64);
            if m.State == MEM_COMMIT {
                let p = m.Protect & 0xff;
                let exec = matches!(p, 0x10 | 0x20 | 0x40 | 0x80);
                let write = matches!(p, 0x04 | 0x08 | 0x40 | 0x80);
                let read = p != 0x01 && p != 0;
                let path = if m.Type == MEM_IMAGE {
                    self.modules
                        .iter()
                        .find(|x| x.base == m.AllocationBase as u64)
                        .map(|x| x.path.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                out.push(Map { start, end: start + size, read, write, exec, offset: 0, path });
            }
            let next = start.saturating_add(size);
            if next <= at || next >= 0x8000_0000_0000 {
                break;
            }
            at = next;
        }
        Ok(out)
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        Ok(self.modules.clone())
    }

    fn entry(&mut self) -> Option<u64> {
        self.entry
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        self.watch = *slots;
        for h in self.threads.values() {
            self.put_watch(*h, slots)?;
        }
        Ok(())
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        let Some((pid, tid, status)) = self.pending.take() else { return fail("the program is not stopped") };
        if let Resume::Step(t) = how {
            let h = self.thread(t)?;
            let mut c = ctx(CTX_REGS);
            // SAFETY: as in `regs`: set the trap flag so one instruction runs.
            unsafe {
                if GetThreadContext(h, &mut c.0) == 0 {
                    return Err(last_error("GetThreadContext"));
                }
                c.0.EFlags |= TF as u32;
                if SetThreadContext(h, &c.0) == 0 {
                    return Err(last_error("SetThreadContext"));
                }
            }
            for (&other, &oh) in &self.threads {
                if other != t {
                    // SAFETY: held for the one step, released when it ends.
                    unsafe { SuspendThread(oh) };
                    self.held.push(oh);
                }
            }
            self.stepping = Some(t);
        }
        // SAFETY: answers the event the program is stopped at.
        if unsafe { ContinueDebugEvent(pid, tid, status) } == 0 {
            return Err(last_error("ContinueDebugEvent"));
        }
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        let until = Instant::now() + timeout;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let ms = left.as_millis().min(u32::MAX as u128 - 1) as u32;
            let mut ev = DEBUG_EVENT::default();
            // SAFETY: ev is a valid out-structure.
            if unsafe { WaitForDebugEvent(&mut ev, ms) } == 0 {
                // SAFETY: reads the last error of this thread.
                if unsafe { GetLastError() } == WAIT_TIMEOUT || Instant::now() >= until {
                    return Ok(Vec::new());
                }
                return Err(last_error("WaitForDebugEvent"));
            }
            self.book(&ev);
            match ev.dwDebugEventCode {
                EXIT_PROCESS_DEBUG_EVENT => {
                    // SAFETY: the member read matches the event code.
                    let code = unsafe { ev.u.ExitProcess.dwExitCode } as i32;
                    self.exited = true;
                    self.held.clear();
                    // SAFETY: the last event of the program is answered.
                    unsafe { ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE) };
                    return Ok(vec![Event::Exited { code }]);
                }
                EXCEPTION_DEBUG_EVENT => {
                    self.release_held();
                    if let Some(e) = self.stop_of(&ev) {
                        return Ok(vec![e]);
                    }
                    self.pending = None;
                    // SAFETY: the loader's break is answered and the program goes on.
                    unsafe { ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE) };
                }
                _ => {
                    // SAFETY: bookkeeping events are answered at once.
                    unsafe { ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE) };
                }
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        self.interrupting = true;
        // SAFETY: makes the system start a thread in the program that breaks into the debugger.
        if unsafe { DebugBreakProcess(self.process) } == 0 {
            self.interrupting = false;
            return Err(last_error("DebugBreakProcess"));
        }
        Ok(())
    }

    fn stdin(&mut self, bytes: Option<&[u8]>) -> Result<usize> {
        let Some(b) = bytes else {
            if let Some(h) = self.stdin.take() {
                // SAFETY: closing the write end once tells the program its input is over.
                unsafe { CloseHandle(h) };
            }
            return Ok(0);
        };
        let Some(h) = self.stdin else { return fail("standard input is closed") };
        let mut n = 0u32;
        // SAFETY: the source is a live slice.
        if unsafe { WriteFile(h, b.as_ptr(), b.len() as u32, &mut n, std::ptr::null_mut()) } == 0 {
            return Err(last_error("writing to standard input"));
        }
        Ok(n as usize)
    }

    fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut *self.out.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn detach(&mut self) -> Result<()> {
        self.release_held();
        if let Some((pid, tid, status)) = self.pending.take() {
            // SAFETY: the stopped event is answered before the program is let go.
            unsafe { ContinueDebugEvent(pid, tid, status) };
        }
        // SAFETY: stops debugging; the program lives on.
        unsafe {
            DebugSetProcessKillOnExit(0);
            if DebugActiveProcessStop(self.pid) == 0 {
                return Err(last_error("DebugActiveProcessStop"));
            }
        }
        self.exited = true;
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        if self.exited {
            return Ok(());
        }
        self.release_held();
        // SAFETY: ends the program, then its last events are drained.
        unsafe { TerminateProcess(self.process, 1) };
        if let Some((pid, tid, status)) = self.pending.take() {
            // SAFETY: as above.
            unsafe { ContinueDebugEvent(pid, tid, status) };
        }
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            let mut ev = DEBUG_EVENT::default();
            // SAFETY: ev is a valid out-structure.
            if unsafe { WaitForDebugEvent(&mut ev, 100) } == 0 {
                continue;
            }
            let last = ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT;
            // SAFETY: draining events of a dying program.
            unsafe { ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE) };
            if last {
                break;
            }
        }
        self.exited = true;
        Ok(())
    }
}

impl Drop for Windows {
    fn drop(&mut self) {
        if !self.exited {
            let _ = if self.launched { self.kill() } else { self.detach() };
        }
        if let Some(h) = self.stdin.take() {
            // SAFETY: closing our write end once.
            unsafe { CloseHandle(h) };
        }
        // SAFETY: the process handle was opened or given to us and is closed once.
        unsafe { CloseHandle(self.process) };
    }
}
