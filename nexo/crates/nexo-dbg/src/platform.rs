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
    MacOS,
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

/// Page access bits of [`Target::protect`]: read, write, execute.
pub const PROT_R: u8 = 1;
pub const PROT_W: u8 = 2;
pub const PROT_X: u8 = 4;
/// The page size the watches guard by: 4 KiB on x86, the only live debugging there is.
pub const PAGE: u64 = 0x1000;

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
    /// Linux `si_code`; on macOS the Mach exception code of a fault (0 for a signal); 0 on Windows.
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
    /// The program forked this child, kept as its own target (`Target::forks`); the parent waits here only under
    /// `Follow::Child`, so the debugger can let it go.
    Fork(u32),
}

/// Which side of a fork the debugger keeps (`follow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Follow {
    /// The child is let go; the program goes on as it did.
    #[default]
    Parent,
    /// The child is kept as its own target and the parent waits at the fork, to be let go.
    Child,
    /// The child is kept as its own target and the parent goes on.
    Both,
}

/// A child the program forked, as the backend saw it.
pub struct Forked {
    pub pid: u32,
    /// The thread that forked and its program counter just after the fork call.
    pub tid: Tid,
    pub at: u64,
    /// A vfork child shares the parent's memory until it execs or exits; the parent waits for that.
    pub vfork: bool,
    /// The bytes under the breakpoints at that moment: the child's memory has `0xcc` at each address.
    pub patches: Vec<(u64, u8)>,
    /// The child, stopped where fork returned in it, when the backend keeps children (`Follow::Child` or `Both`).
    pub target: Option<Box<dyn Target>>,
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
    /// Names taken out of the environment this process passes on.
    pub unset: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Leave address randomisation on (it is turned off by default so addresses repeat between runs).
    pub aslr: bool,
    /// What the program sees as `argv[0]` (the path it was started from when `None`).
    pub argv0: Option<String>,
}

/// What a `run` may change about how the program starts: its environment, working folder, address randomisation
/// and `argv[0]`. The default is the debugger's own start: this process's environment and folder, randomisation off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOpts {
    pub env: Vec<(String, String)>,
    pub unset: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub aslr: bool,
    pub argv0: Option<String>,
}

impl LaunchOpts {
    pub fn is_default(&self) -> bool {
        *self == LaunchOpts::default()
    }

    /// The program to start with these options. A relative path is made absolute first when the folder changes,
    /// because it is resolved after the change; the program still sees the name it was given as `argv[0]`.
    pub fn launch(&self, exe: &std::path::Path, args: &[String]) -> Launch {
        let (mut exe, mut argv0) = (exe.to_path_buf(), self.argv0.clone());
        if self.cwd.is_some() && exe.is_relative() {
            if let Ok(here) = std::env::current_dir() {
                argv0.get_or_insert_with(|| exe.display().to_string());
                exe = here.join(&exe);
            }
        }
        Launch {
            exe,
            args: args.to_vec(),
            env: self.env.clone(),
            unset: self.unset.clone(),
            cwd: self.cwd.clone(),
            aslr: self.aslr,
            argv0,
        }
    }

    /// One line for the start answer: what was changed, with variable names only (a value may be a secret).
    pub fn fact(&self) -> Option<String> {
        let mut v = Vec::new();
        if !self.env.is_empty() {
            let names: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
            v.push(format!("environment set: {}", names.join(", ")));
        }
        if !self.unset.is_empty() {
            v.push(format!("environment unset: {}", self.unset.join(", ")));
        }
        if let Some(c) = &self.cwd {
            v.push(format!("working folder {}", c.display()));
        }
        if self.aslr {
            v.push("address randomisation on".to_string());
        }
        if let Some(a) = &self.argv0 {
            v.push(format!("argv[0] {a}"));
        }
        if v.is_empty() {
            None
        } else {
            Some(format!("launch: {}", v.join("; ")))
        }
    }
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
    /// The x87, SSE and AVX registers of `tid` as the system saves them, for [`Target::set_vector_state`] to put
    /// back; `None` where they are not read.
    fn vector_state(&mut self, _tid: Tid) -> Option<Vec<u8>> {
        None
    }
    fn set_vector_state(&mut self, _tid: Tid, _state: &[u8]) -> Result<()> {
        Ok(())
    }
    /// The signal (Linux, macOS) or exception status (Windows) `tid` gets when it runs again, taken away so it runs
    /// without it: 0 when there is none. A fault comes back by itself when its instruction runs again.
    fn take_signal(&mut self, _tid: Tid) -> u64 {
        0
    }
    /// Hand back what [`Target::take_signal`] took. Only Linux keeps a signal across other stops; elsewhere this
    /// does nothing.
    fn give_signal(&mut self, _tid: Tid, _sig: u64) {}
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
    /// Set the access of the pages that hold `addr..addr+len` to `prot` (bits [`PROT_R`], [`PROT_W`], [`PROT_X`]). A
    /// watch that does not fit the four hardware slots guards its pages this way and is told by the fault.
    fn protect(&mut self, _addr: u64, _len: u64, _prot: u8) -> Result<()> {
        crate::fail("this target cannot change the access of a page")
    }
    /// The bytes under the breakpoints now in memory: a child the program forks is let go with them put back.
    fn set_patches(&mut self, _patches: &[(u64, u8)]) {}
    /// Which side of a fork to keep. `false`: this backend lets every child go, whatever is asked.
    fn set_follow(&mut self, _follow: Follow) -> bool {
        false
    }
    /// The children forked since the last call, the let-go ones too.
    fn forks(&mut self) -> Vec<Forked> {
        Vec::new()
    }
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
    /// A fact about how the program was started that the start banner says (macOS: the signed copy that runs).
    fn note(&self) -> Option<String> {
        None
    }
}

/// The system nexo runs on, as the messages name it.
const HOST: &str = if cfg!(windows) {
    "Windows"
} else if cfg!(target_os = "macos") {
    "macOS"
} else {
    "Linux"
};

/// The Mach-O CPU type of this host (`CPU_TYPE_ARM64` or `CPU_TYPE_X86_64`).
const HOST_CPU: u32 = if cfg!(target_arch = "aarch64") { 0x0100_000c } else { 0x0100_0007 };

/// The slice of a universal (fat) Mach-O file the debugger reads: the one this Mac runs. `None` off macOS, where
/// the file is read as it always was.
pub const NATIVE_SLICE: Option<&str> = if !cfg!(target_os = "macos") {
    None
} else if cfg!(target_arch = "aarch64") {
    Some("arm64")
} else {
    Some("x86-64")
};

/// Whether `run` and `attach` start or open programs on this host: x86-64 Linux, Windows and macOS. Elsewhere they
/// fail with the reason; `nexo dbg core` reads a core file anywhere.
pub const fn runs_here() -> bool {
    cfg!(all(target_arch = "x86_64", any(target_os = "linux", windows, target_os = "macos")))
}

/// What kind of program `exe` is when this host cannot run it: a Windows program on Linux, a Linux or macOS one on
/// Windows; or when the debugger cannot run it here: a 32-bit program on Windows. `None` when it can run here (or
/// the file is not one of those formats: the launch says what is wrong).
pub fn foreign_program(exe: &std::path::Path) -> Option<String> {
    use std::io::Read;
    // the PE header is found past the DOS stub, at most a few KiB in
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(exe).and_then(|f| f.take(4096).read_to_end(&mut head)).ok()?;
    if let Some(why) = wow64_on(&head, HOST) {
        return Some(format!("{} is {why}", exe.display()));
    }
    let (kind, host) = foreign_on(&head, HOST, HOST_CPU)?;
    Some(format!(
        "{} is {kind}; this host runs {host} programs only, so `run` cannot start it here (`bin` reads it without running it)",
        exe.display()
    ))
}

/// The machine a PE file starting with `head` is built for (0x14c: 32-bit x86, 0x8664: x86-64).
fn pe_machine(head: &[u8]) -> Option<u16> {
    let at = |o: usize, n: usize| head.get(o..o + n);
    at(0, 2).filter(|m| *m == b"MZ")?;
    let pe = u32::from_le_bytes(at(0x3c, 4)?.try_into().ok()?) as usize;
    at(pe, 4).filter(|s| *s == b"PE\0\0")?;
    Some(u16::from_le_bytes(at(pe + 4, 2)?.try_into().ok()?))
}

/// Why the debugger does not start a 32-bit program (PE32) on Windows, whose WOW64 threads it does not read.
fn wow64_on(head: &[u8], host: &str) -> Option<&'static str> {
    (host == "Windows" && pe_machine(head) == Some(0x14c)).then_some(
        "a 32-bit Windows program (PE32), which Windows runs under WOW64; this debugger reads and sets the registers \
         of 64-bit Windows threads only, so `run` does not start it (`bin` reads it without running it)",
    )
}

/// The kind of program a file starting with `head` is, and what a `host` with the Mach-O CPU type `cpu` runs, when
/// that host cannot run it. A Mach-O file runs on a Mac when it holds code for the Mac's processor: thin, or one
/// slice of a universal file. A universal header shares its magic with a Java class file, whose version number
/// (45 or more) sits where the universal file counts its few slices.
fn foreign_on(head: &[u8], host: &str, cpu: u32) -> Option<(&'static str, String)> {
    let pe = head.starts_with(b"MZ");
    let elf = head.starts_with(b"\x7fELF");
    let thin = [[0xfe, 0xed, 0xfa, 0xce], [0xfe, 0xed, 0xfa, 0xcf], [0xce, 0xfa, 0xed, 0xfe], [0xcf, 0xfa, 0xed, 0xfe]]
        .iter()
        .any(|m| head.starts_with(m));
    let word = |o: usize| head.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let fat = matches!(word(0), Some(0xcafe_babe | 0xcafe_babf)) && word(4).is_some_and(|n| (1..32).contains(&n));
    let (kind, here) = match (pe, elf, thin || fat) {
        (true, ..) => ("a Windows (PE) program", host == "Windows"),
        (_, true, _) => ("a Linux (ELF) program", host == "Linux"),
        (.., true) if host == "macOS" => {
            if nexo_bin::macho_runs_on(head, cpu) {
                return None;
            }
            let arch = if cpu == 0x0100_000c { "arm64" } else { "x86-64" };
            return Some(("a macOS (Mach-O) program with no code for this processor", format!("macOS {arch}")));
        }
        (.., true) => ("a macOS (Mach-O) program", false),
        _ => return None,
    };
    (!here).then(|| (kind, host.to_string()))
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

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub fn launch(l: &Launch) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::macos::Mac::launch(l)?))
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub fn attach(pid: u32) -> Result<Box<dyn Target>> {
    Ok(Box::new(crate::macos::Mac::attach(pid)?))
}

// An Apple-silicon Mac builds and runs everything else; only a live program is refused, with the reason.
#[cfg(all(target_os = "macos", not(target_arch = "x86_64")))]
const ONLY: &str = "nexo dbg on macOS runs x86-64 programs only for now: arm64 (Apple silicon) registers, breakpoints and stepping are not written yet (`nexo dbg core` reads a core file anywhere)";

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64")
)))]
const ONLY: &str =
    "nexo dbg runs programs on Linux, Windows and macOS x86-64 only (`nexo dbg core` reads a core file anywhere)";

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub fn launch(_: &Launch) -> Result<Box<dyn Target>> {
    crate::fail(ONLY)
}

#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(windows, target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub fn attach(_: u32) -> Result<Box<dyn Target>> {
    crate::fail(ONLY)
}

/// The DWARF of a macOS program built with `-g` stays in the object files its debug map names, unless a dSYM
/// beside the program holds it (`cc -g` makes one when it compiles and links in one step; a separate link or a cargo
/// build leaves only the map). Gather it the way the system debugger does, with `dsymutil`, into a folder of our
/// own that is read and removed at once. Returns a fact for the first answer when the program has a debug map and
/// still no debug info could be read; `None` otherwise, and always off macOS (no other format has a debug map).
pub fn gather_dwarf(img: &mut nexo_bin::Image) -> Option<String> {
    let objects = img.debug_map_objects();
    if objects.is_empty() || !img.debug_section(".debug_info").is_empty() {
        return None;
    }
    let n = objects.len();
    // the linker writes an object file as it saw it, and an archive member as `lib.a(member.o)`
    let here = objects.iter().any(|o| std::path::Path::new(o.split('(').next().unwrap_or(o)).exists());
    let lost = |why: String| {
        Some(format!("no debug info: the program's DWARF is in the object files its debug map names ({n}), and {why}"))
    };
    if !here {
        return lost("none of them is on disk here".into());
    }
    match dsymutil(img) {
        Ok(()) if !img.debug_section(".debug_info").is_empty() => None,
        Ok(()) => lost(format!("{DSYMUTIL} gathered no DWARF from them")),
        Err(why) => lost(why),
    }
}

const DSYMUTIL: &str = "/usr/bin/dsymutil";

/// How long `dsymutil` may take: the session must be up within the 40 seconds its client waits.
#[cfg(target_os = "macos")]
const GATHER_WAIT: Duration = Duration::from_secs(20);

#[cfg(not(target_os = "macos"))]
fn dsymutil(_: &mut nexo_bin::Image) -> std::result::Result<(), String> {
    Err(format!("{DSYMUTIL} runs on macOS only"))
}

/// Run `dsymutil` on the program into a fresh folder, take the dSYM it writes, and remove the folder.
#[cfg(target_os = "macos")]
fn dsymutil(img: &mut nexo_bin::Image) -> std::result::Result<(), String> {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = img.path.file_name().ok_or("the program path has no file name")?.to_owned();
    let dir = crate::server::state_dir().join(format!(
        "dsym-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{} could not be made: {e}", dir.display()))?;
    let out = dir.join(&name);
    let mut bundle = out.clone().into_os_string();
    bundle.push(".dSYM");
    let said = dir.join("stderr");
    let mut cmd = Command::new(DSYMUTIL);
    cmd.arg("-o").arg(&bundle).arg(&img.path).stdin(Stdio::null()).stdout(Stdio::null());
    // to a file, not a pipe: thousands of "unable to open object file" lines would fill a pipe nobody reads yet
    cmd.stderr(std::fs::File::create(&said).map_or_else(|_| Stdio::null(), Stdio::from));
    let ran = match cmd.spawn() {
        Err(e) => Err(format!("{DSYMUTIL} did not start: {e}")),
        Ok(mut child) => {
            let deadline = std::time::Instant::now() + GATHER_WAIT;
            loop {
                match child.try_wait() {
                    Ok(Some(st)) if st.success() => break Ok(()),
                    Ok(Some(st)) => break Err(format!("{DSYMUTIL} failed ({st})")),
                    Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(format!("{DSYMUTIL} did not finish in {} s", GATHER_WAIT.as_secs()));
                    }
                }
            }
        }
    };
    let first = std::fs::read_to_string(&said).ok().and_then(|t| {
        t.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(200).collect::<String>())
    });
    let took = ran.is_ok() && img.attach_dsym(&nexo_bin::Image::dsym_path(&out));
    let _ = std::fs::remove_dir_all(&dir);
    match (ran, took, first) {
        (Ok(()), true, _) => Ok(()),
        (Ok(()), false, Some(l)) => Err(format!("{DSYMUTIL} wrote no dSYM this program's build matches: {l}")),
        (Ok(()), false, None) => Err(format!("{DSYMUTIL} wrote no dSYM this program's build matches")),
        (Err(why), _, Some(l)) => Err(format!("{why}: {l}")),
        (Err(why), _, None) => Err(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prog");
        std::fs::write(&p, bytes).unwrap();
        (dir, p)
    }

    #[test]
    fn a_32_bit_windows_program_is_not_started_on_windows() {
        let pe = |machine: u16| {
            let mut b = vec![0u8; 0x100];
            b[..2].copy_from_slice(b"MZ");
            b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
            b[0x80..0x84].copy_from_slice(b"PE\0\0");
            b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
            b
        };
        assert_eq!(pe_machine(&pe(0x14c)), Some(0x14c));
        let why = wow64_on(&pe(0x14c), "Windows").unwrap();
        assert!(why.contains("32-bit") && why.contains("WOW64"), "{why}");
        assert_eq!(wow64_on(&pe(0x8664), "Windows"), None);
        assert_eq!(wow64_on(&pe(0x14c), "Linux"), None);
        // a header cut short, or one past what was read: nothing is claimed
        assert_eq!(pe_machine(&pe(0x14c)[..0x84]), None);
        assert_eq!(pe_machine(b"MZ"), None);
    }

    #[test]
    fn a_program_of_another_system_is_named_and_one_of_this_system_is_not() {
        let (_d, pe) = file(b"MZ\x90\0\x03\0\0\0");
        let (_e, elf) = file(b"\x7fELF\x02\x01\x01\0");
        let (_m, mach) = file(&[0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1]);
        let (_t, text) = file(b"#!/bin/sh\n");
        let (pe, elf, mach) = (foreign_program(&pe), foreign_program(&elf), foreign_program(&mach));
        assert_eq!(foreign_program(&text), None);
        if cfg!(windows) {
            assert_eq!(pe, None);
            assert!(elf.unwrap().contains("a Linux (ELF) program; this host runs Windows programs only"));
            assert!(mach.unwrap().contains("macOS (Mach-O) program"));
        } else if cfg!(target_os = "macos") {
            assert!(pe.unwrap().contains("a Windows (PE) program; this host runs macOS programs only"));
            assert!(elf.unwrap().contains("a Linux (ELF) program; this host runs macOS programs only"));
            assert_eq!(mach.is_none(), cfg!(target_arch = "x86_64"));
        } else {
            assert_eq!(elf, None);
            assert!(mach.unwrap().contains("macOS (Mach-O) program"));
            let e = pe.unwrap();
            assert!(e.contains("a Windows (PE) program; this host runs Linux programs only"), "{e}");
            assert!(e.contains("`bin` reads it without running it"), "{e}");
        }
    }

    #[test]
    fn on_a_mac_a_mach_o_file_with_code_for_its_processor_runs_and_elf_and_pe_do_not() {
        let (x86, arm) = (0x0100_0007u32, 0x0100_000cu32);
        let thin = |cpu: u32| [0xfeed_facfu32.to_le_bytes(), cpu.to_le_bytes()].concat();
        let mut fat = [0xcafe_babeu32.to_be_bytes(), 2u32.to_be_bytes()].concat();
        for cpu in [x86, arm] {
            fat.extend(cpu.to_be_bytes());
            fat.extend([0u8; 16]);
        }
        assert_eq!(foreign_on(&thin(x86), "macOS", x86), None);
        assert_eq!(foreign_on(&fat, "macOS", x86), None);
        assert_eq!(foreign_on(&fat, "macOS", arm), None);
        let (kind, host) = foreign_on(&thin(arm), "macOS", x86).unwrap();
        assert_eq!((kind, host.as_str()), ("a macOS (Mach-O) program with no code for this processor", "macOS x86-64"));
        assert_eq!(foreign_on(b"\x7fELF\x02\x01\x01\0", "macOS", x86).unwrap().0, "a Linux (ELF) program");
        assert_eq!(foreign_on(b"MZ\x90\0", "macOS", x86).unwrap().0, "a Windows (PE) program");
        // a Java class file of version 52 is not a universal file
        assert_eq!(foreign_on(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52], "macOS", x86), None);
        assert_eq!(foreign_on(&fat, "Linux", x86).unwrap().0, "a macOS (Mach-O) program");
    }
}
