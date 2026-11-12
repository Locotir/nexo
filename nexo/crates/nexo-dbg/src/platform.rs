//! What the engine asks of an operating system: one program under control, all of its threads
//! stopped together or running together. One implementation per system; the engine above it holds
//! breakpoints, names and everything a person sees.

use std::path::PathBuf;
use std::time::Duration;

use crate::regs::Regs;
use crate::Result;

/// A thread, by the number the system gives it.
pub type Tid = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
}

/// One piece of the program's address space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    /// Offset into the file, when the piece comes from one.
    pub offset: u64,
    /// The file, or a word like `[stack]`; empty when the system gives none.
    pub path: String,
}

/// A file loaded into the program. `base` is where the start of the file sits in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub path: String,
    pub base: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Exec,
    Write,
    /// x86 has no read-only watch: this one fires on a read or a write.
    ReadWrite,
}

/// What a faulting instruction was doing with the address it could not use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touch {
    Read,
    Write,
    Exec,
}

/// One of the four hardware debug registers: `len` is 1, 2, 4 or 8 and `addr` is aligned to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwSlot {
    pub addr: u64,
    pub len: u8,
    pub access: Access,
}

/// A signal (Linux) or an exception (Windows) the program is about to get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    /// The signal number or the exception code.
    pub number: u32,
    /// Linux `si_code`; 0 on Windows.
    pub code: i32,
    /// The address the fault is about, when the system says.
    pub addr: Option<u64>,
    /// What was being done at `addr`, when the system says (Windows does, Linux does not).
    pub touch: Option<Touch>,
    /// False on Windows when no handler took the exception and the program is about to die.
    pub first_chance: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// An `int3` ran: the program counter is one byte past it.
    Breakpoint,
    /// One instruction ran after `Resume::Step`.
    Step,
    /// Hardware slots that fired, one bit each (bit 0 is slot 0).
    Watch(u8),
    Fault(Fault),
    /// The stop `Target::interrupt` asked for.
    Interrupt,
    /// The program replaced itself with another one: every address is new.
    Exec,
    /// A system call starts or ends (only while `Target::trace_syscalls` is on).
    Syscall(Sys),
}

/// One side of a system call, as the kernel reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sys {
    /// True: the call is about to run (`args` are read). False: it returned (`ret` is read).
    pub entry: bool,
    /// The call's number; on a return the kernel does not give it again, so it is the number in `orig_rax`.
    pub nr: u64,
    pub args: [u64; 6],
    pub ret: i64,
    /// The kernel says `ret` is an error (`-errno`).
    pub error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Stopped {
        tid: Tid,
        stop: Stop,
    },
    Exited {
        code: i32,
    },
    /// Ended by a signal (Linux).
    Killed {
        signal: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// Every thread runs.
    All,
    /// One thread runs one instruction; the others stay stopped.
    Step(Tid),
}

/// A program to start.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    pub exe: PathBuf,
    pub args: Vec<String>,
    /// Added to (or replacing) the environment of this process.
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// Leave address randomisation on (it is turned off by default so addresses repeat between runs).
    pub aslr: bool,
}

/// A program under control. Every call but `wait`, `interrupt`, `stdin` and `output` needs the
/// program stopped. All calls must come from the thread that made the value.
pub trait Target {
    fn os(&self) -> Os;
    fn pid(&self) -> u32;
    /// True when this debugger started the program (it is then killed, not let go, at teardown).
    fn launched(&self) -> bool;
    fn threads(&self) -> Vec<Tid>;
    fn regs(&mut self, tid: Tid) -> Result<Regs>;
    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()>;
    /// Read memory as it is (breakpoint bytes included); the count read stops at the first byte that cannot be read.
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize;
    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()>;
    fn maps(&mut self) -> Result<Vec<Map>>;
    /// The files loaded, the program itself first.
    fn modules(&mut self) -> Result<Vec<Module>>;
    /// Where the program's own first instruction is in memory.
    fn entry(&mut self) -> Option<u64>;
    /// Put these four slots on every thread, the ones not yet born included.
    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()>;
    /// The bytes under the breakpoints now in memory: a child the program forks is let go with them put back.
    fn set_patches(&mut self, _patches: &[(u64, u8)]) {}
    fn resume(&mut self, how: Resume) -> Result<()>;
    /// Stop at every system call's start and end from the next resume on (`Stop::Syscall`).
    fn trace_syscalls(&mut self, _on: bool) -> Result<()> {
        crate::fail("system call tracing runs on Linux only")
    }
    /// What happened, waiting at most `timeout`. Empty: still running. Not empty: every thread is
    /// stopped (or the program is gone); the first event is the one that stopped it, the others
    /// happened in the same moment on other threads.
    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>>;
    /// Ask a running program to stop; `wait` then reports `Stop::Interrupt`.
    fn interrupt(&mut self) -> Result<()>;
    /// Bytes for the program's standard input (`None` closes it). The count taken is returned.
    fn stdin(&mut self, bytes: Option<&[u8]>) -> Result<usize>;
    /// What the program wrote to standard output and error since the last call.
    fn output(&mut self) -> Vec<u8>;
    /// Let the program go on without the debugger.
    fn detach(&mut self) -> Result<()>;
    fn kill(&mut self) -> Result<()>;
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn launch(l: &Launch) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::linux::Linux::launch(l)?))
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn attach(pid: u32) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::linux::Linux::attach(pid)?))
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub fn launch(l: &Launch) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::windows::Windows::launch(l)?))
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub fn attach(pid: u32) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::windows::Windows::attach(pid)?))
}

#[cfg(not(any(all(target_os = "linux", target_arch = "x86_64"), all(windows, target_arch = "x86_64"))))]
const ONLY: &str =
    "nexo dbg runs programs on Linux x86-64 and Windows x86-64 only (`nexo dbg core` reads a core file anywhere)";

#[cfg(not(any(all(target_os = "linux", target_arch = "x86_64"), all(windows, target_arch = "x86_64"))))]
pub fn launch(_: &Launch) -> Result<Box<dyn Target>> {
    crate::fail(ONLY)
}

#[cfg(not(any(all(target_os = "linux", target_arch = "x86_64"), all(windows, target_arch = "x86_64"))))]
pub fn attach(_: u32) -> Result<Box<dyn Target>> {
    crate::fail(ONLY)
}
