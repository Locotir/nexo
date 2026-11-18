//! One debugging session: the program under control, the breakpoints, and the rules for running it
//! until something worth a person's attention happens. The commands people type are in `cmds`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use nexo_bin::{Image, Output, Row};
use serde_json::{json, Value};

use crate::expr::{self, Env};
use crate::modules::Mods;
use crate::platform::{Access, Event, Fault, Follow, Forked, HwSlot, Os, Resume, Stop, Target, Tid};
use crate::regs::Regs;
use crate::{fail, Result};

mod call;
pub mod watch;

pub use watch::{Guard, WatchHow};

/// Stops remembered for `history`.
pub const HISTORY: usize = 64;
/// Bytes of the program's output kept.
pub const OUT_KEEP: usize = 1 << 20;
/// Lines of logpoint output kept.
pub const LOG_KEEP: usize = 1000;
/// Hits a breakpoint with `commands` serves on its own before it stops so the answer comes back.
pub const ACTION_HITS: u64 = 2000;
/// Most bytes one `{mem:EXPR:N}` of a logpoint dumps.
pub const LOG_BYTES: usize = 256;

#[derive(Clone)]
pub struct Bp {
    pub id: u32,
    pub addr: u64,
    pub enabled: bool,
    pub cond: Option<String>,
    /// Hits to let go by before the first stop.
    pub after: u64,
    pub hits: u64,
    /// A logpoint: this text (with `{expr}` holes) is written to the log and the program goes on.
    pub log: Option<String>,
    /// Removed after the first stop.
    pub once: bool,
    /// Set by `calls`: at an indirect call, the real target goes to the log and the program goes on.
    pub call: bool,
    /// gdb-style `commands`: read-only commands run at every hit, as words. A `continue` among them lets the
    /// program go on without a stop; without one the breakpoint stops after them.
    pub actions: Vec<Vec<String>>,
    /// The name it was set by when that name is in a library: the address is found again when the library loads
    /// again. `addr` is 0 while no loaded file has the name (a pending breakpoint).
    pub name: Option<String>,
}

pub struct Wp {
    pub id: u32,
    pub addr: u64,
    pub len: u64,
    pub access: Access,
    pub how: WatchHow,
    /// The debug registers it takes (number and what each one watches), for a hardware watch.
    pub slots: Vec<(usize, HwSlot)>,
    /// Its bytes as they were at the last look.
    pub last: Vec<u8>,
    pub hits: u64,
    /// As for a breakpoint: a condition (`$old`, `$new` and `$at` hold the change), hits let go by first, and a
    /// line for the log instead of a stop (empty: the hit line itself).
    pub cond: Option<String>,
    pub after: u64,
    pub log: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    Entry,
    Step,
    Break(Vec<u32>),
    Next,
    Finish,
    Until,
    Watch(Vec<u32>),
    Signal(Fault),
    Int3,
    Interrupt,
    Exec,
    /// `catch syscall`: the call, as the trace writes it.
    Syscall(String),
    /// `catch load` or `catch unload`: the libraries that came or went.
    Load(String),
    /// A hook with `stop`: its id and its line.
    Hook(u32, String),
    /// A call made by `call` or `print` came back to its return address.
    Returned,
    /// The first stop of a forked child kept as a session of its own: what it is and where it came from.
    Fork(String),
    /// The program forked under `follow child`: the child is kept and this process was let go.
    LetGo(String),
    Exited(i32),
    Killed(i32),
}

#[derive(Debug, Clone, Copy)]
pub struct Temp {
    pub addr: u64,
    /// The stack pointer must be at least this when the address is reached (a recursive call does not count).
    pub min_sp: u64,
    pub kind: TempKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempKind {
    Entry,
    Next,
    Finish,
    Until,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Run {
    pub step: Option<Tid>,
    pub temp: Option<Temp>,
}

/// What a stop looked like, kept for `history`.
pub struct Rec {
    pub n: u64,
    pub tid: Tid,
    pub why: String,
    pub regs: Regs,
}

pub struct Session {
    pub t: Box<dyn Target>,
    pub mods: Mods,
    pub state: State,
    pub cur: Tid,
    /// The frame `regs`, `locals`, `args`, `print`, `mem` and `dis` read: 0 is where the thread stopped, 1 its
    /// caller and so on (`frame N`, `up`, `down`). Every resume and thread switch sets it back to 0.
    pub frame: usize,
    pub bps: Vec<Bp>,
    pub wps: Vec<Wp>,
    pub next_id: u32,
    inserted: BTreeMap<u64, u8>,
    /// (site, target) pairs of indirect calls already saved to the names file.
    seen_targets: std::collections::BTreeSet<(u64, String)>,
    /// The "saved targets" line has been written to the log.
    said_saved: bool,
    pub run: Run,
    backlog: Vec<Event>,
    pub prev: HashMap<Tid, Regs>,
    pub out: Vec<u8>,
    pub out_seen: usize,
    pub out_dropped: u64,
    pub log: VecDeque<String>,
    pub history: VecDeque<Rec>,
    /// `record`: the instructions that ran, bounded.
    pub tape: crate::record::Tape,
    pub nstop: u64,
    /// Where the last stop was, for the header of the answer.
    pub last_stop: Option<StopInfo>,
    /// The function of the last stop that got a `bin decomp` line, so stepping inside it does not repeat the line.
    pub bridged: String,
    pub catch: BTreeSet<u32>,
    pub exit: Option<String>,
    /// `strace`, `ltrace`, hooks and coverage.
    pub tr: crate::trace::Tracing,
    /// Convenience variables (`set \$v = ...`), by name without the `\$`.
    pub conv: HashMap<String, expr::Val>,
    /// Breakpoint ids and actions being typed after `commands`, until `end`.
    pub recording: Option<(Vec<u32>, Vec<Vec<String>>)>,
    /// `display` expressions shown at every stop: id and the command that shows one.
    pub displays: Vec<(u32, Vec<String>)>,
    /// Lines ever written to the log, and how many of them an answer already showed.
    pub log_total: u64,
    pub log_shown: u64,
    /// The call into the program under way (`call`, `print f(x)`), until it returns or ends.
    calling: Option<call::Pending>,
    /// Which side of a fork is kept (`follow`).
    pub follow: Follow,
    /// The children the program forked, kept or let go.
    pub forks: Vec<ForkNote>,
    /// Children kept as sessions of their own, waiting for the server to give them a port.
    pub kids: Vec<Kid>,
    /// Pages page watches guard while the program runs.
    pub guards: Vec<Guard>,
    /// The lines of the watch hits the next stop reports.
    pub watch_lines: Vec<String>,
    /// The thread a step watch stepped and the instruction it ran.
    step_from: Option<(Tid, u64)>,
    /// Libraries loaded and unloaded while the program runs: pending names, `catch load`.
    pub libs: crate::libs::Loads,
    /// The pc inside the function a `finish` runs out of, until the finish stop reads its return value.
    pub(crate) finish_from: Option<u64>,
}

/// A child the program forked.
pub struct ForkNote {
    pub pid: u32,
    /// Where fork returned in the parent: the address and its name.
    pub place: String,
    pub vfork: bool,
    /// The child's session id, when it was kept.
    pub session: Option<String>,
}

/// A forked child kept as a session: its id, the session and the answer that tells its first stop.
pub struct Kid {
    pub id: String,
    pub sess: Session,
    pub first: Output,
}

/// A stop in the words of the header line of an answer: its number, the event, the place and the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopInfo {
    pub n: u64,
    pub event: String,
    pub place: String,
    pub addr: u64,
}

pub fn signal_name(n: u32) -> &'static str {
    match n {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        17 => "SIGCHLD",
        18 => "SIGCONT",
        19 => "SIGSTOP",
        20 => "SIGTSTP",
        28 => "SIGWINCH",
        31 => "SIGSYS",
        _ => "signal",
    }
}

/// The name of a macOS signal: past 6 the numbers are not the Linux ones (SIGBUS is 10, SIGUSR1 30).
pub fn mac_signal_name(n: u32) -> &'static str {
    match n {
        7 => "SIGEMT",
        10 => "SIGBUS",
        12 => "SIGSYS",
        16 => "SIGURG",
        17 => "SIGSTOP",
        18 => "SIGTSTP",
        19 => "SIGCONT",
        20 => "SIGCHLD",
        21 => "SIGTTIN",
        22 => "SIGTTOU",
        23 => "SIGIO",
        24 => "SIGXCPU",
        25 => "SIGXFSZ",
        26 => "SIGVTALRM",
        27 => "SIGPROF",
        28 => "SIGWINCH",
        29 => "SIGINFO",
        30 => "SIGUSR1",
        31 => "SIGUSR2",
        1..=6 | 8 | 9 | 11 | 13..=15 => signal_name(n),
        _ => "signal",
    }
}

/// The name of signal `n` on `os`.
pub fn signal_name_on(os: Os, n: u32) -> &'static str {
    match os {
        Os::MacOS => mac_signal_name(n),
        Os::Linux | Os::Windows => signal_name(n),
    }
}

/// The name of a Windows exception code (`STATUS_` left off), when it is one a program is likely to die of.
pub fn exception_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0x8000_0002 => "DATATYPE_MISALIGNMENT",
        0x8000_0003 => "BREAKPOINT",
        0x8000_0004 => "SINGLE_STEP",
        0xC000_0005 => "ACCESS_VIOLATION",
        0xC000_0006 => "IN_PAGE_ERROR",
        0xC000_0008 => "INVALID_HANDLE",
        0xC000_000D => "INVALID_PARAMETER",
        0xC000_0017 => "NO_MEMORY",
        0xC000_001D => "ILLEGAL_INSTRUCTION",
        0xC000_0025 => "NONCONTINUABLE_EXCEPTION",
        0xC000_0026 => "INVALID_DISPOSITION",
        0xC000_008C => "ARRAY_BOUNDS_EXCEEDED",
        0xC000_008D => "FLOAT_DENORMAL_OPERAND",
        0xC000_008E => "FLOAT_DIVIDE_BY_ZERO",
        0xC000_0090 => "FLOAT_INVALID_OPERATION",
        0xC000_0091 => "FLOAT_OVERFLOW",
        0xC000_0094 => "INTEGER_DIVIDE_BY_ZERO",
        0xC000_0095 => "INTEGER_OVERFLOW",
        0xC000_0096 => "PRIVILEGED_INSTRUCTION",
        0xC000_00FD => "STACK_OVERFLOW",
        0xC000_0135 => "DLL_NOT_FOUND",
        0xC000_0142 => "DLL_INIT_FAILED",
        0xC000_0374 => "HEAP_CORRUPTION",
        0xC000_0409 => "STACK_BUFFER_OVERRUN",
        0xC000_0417 => "INVALID_CRUNTIME_PARAMETER",
        0xE06D_7363 => "C++ exception (throw)",
        0xE24C_4A02 => "Rust panic (unwinding)",
        _ => return None,
    })
}

/// A fault in the words of the platform: `SIGSEGV (signal 11)` or `ACCESS_VIOLATION (exception 0xc0000005)`.
pub fn fault_text(os: Os, number: u32) -> String {
    match os {
        Os::Linux => format!("{} (signal {number})", signal_name(number)),
        Os::MacOS => format!("{} (signal {number})", mac_signal_name(number)),
        Os::Windows => match exception_name(number) {
            Some(n) => format!("{n} (exception {number:#010x})"),
            None => format!("exception {number:#010x}"),
        },
    }
}

/// The Mach exception behind a macOS hardware fault and the name of its code, as crash reports print them.
fn mach_exception(number: u32, code: i32) -> Option<(&'static str, &'static str)> {
    let exc = match number {
        10 | 11 => "EXC_BAD_ACCESS",
        4 => "EXC_BAD_INSTRUCTION",
        8 => "EXC_ARITHMETIC",
        _ => return None,
    };
    let name = match (exc, code) {
        ("EXC_BAD_ACCESS", 1) => "KERN_INVALID_ADDRESS",
        ("EXC_BAD_ACCESS", 2) => "KERN_PROTECTION_FAILURE",
        ("EXC_BAD_ACCESS", 13) => "EXC_I386_GPFLT",
        ("EXC_BAD_INSTRUCTION", 1) => "EXC_I386_INVOP",
        ("EXC_ARITHMETIC", 1) => "EXC_I386_DIV",
        ("EXC_ARITHMETIC", 2) => "EXC_I386_INTO",
        ("EXC_ARITHMETIC", 5) => "EXC_I386_EXTERR",
        ("EXC_ARITHMETIC", 8) => "EXC_I386_SSEEXTERR",
        _ => "",
    };
    Some((exc, name))
}

/// The headline of a fault stop. A macOS fault the processor raised (its `code` is the Mach code) is named by its
/// Mach exception as well: `EXC_BAD_ACCESS (SIGSEGV) KERN_INVALID_ADDRESS (signal 11)`.
pub fn fault_line(os: Os, f: &crate::platform::Fault) -> String {
    match (os, mach_exception(f.number, f.code)) {
        (Os::MacOS, Some((exc, name))) if f.code != 0 => {
            let sig = mac_signal_name(f.number);
            let code = if name.is_empty() { format!("code {}", f.code) } else { name.to_string() };
            format!("{exc} ({sig}) {code} (signal {})", f.number)
        }
        _ => fault_text(os, f.number),
    }
}

/// An exception code from a name (`access_violation`, `STATUS_ACCESS_VIOLATION`) or a number (`0xc0000005`).
pub fn exception_by(word: &str) -> Option<u32> {
    if let Some(h) = word.strip_prefix("0x").or_else(|| word.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok();
    }
    let w = word.strip_prefix("STATUS_").or_else(|| word.strip_prefix("status_")).unwrap_or(word);
    (0x8000_0000u32..=0x8000_0004)
        .chain(0xC000_0000..=0xC000_0500)
        .find(|&c| exception_name(c).is_some_and(|n| n.eq_ignore_ascii_case(w)))
}

pub fn default_catch(os: Os) -> BTreeSet<u32> {
    match os {
        Os::Linux => [4, 5, 6, 7, 8, 11, 31].into_iter().collect(),
        // the same faults in macOS numbers: SIGBUS is 10 and SIGSYS 12 there
        Os::MacOS => [4, 5, 6, 8, 10, 11, 12].into_iter().collect(),
        // the faults a program dies of; a first-chance one the program may still handle itself, so it
        // stops here only for these. Any other exception stops when no handler took it (second chance).
        Os::Windows => [
            0xC000_0005u32,
            0xC000_0006,
            0xC000_001D,
            0xC000_008C,
            0xC000_008E,
            0xC000_0094,
            0xC000_0095,
            0xC000_0096,
            0xC000_00FD,
            0xC000_0374,
            0xC000_0409,
        ]
        .into_iter()
        .collect(),
    }
}

/// What a condition or a `print` may ask: the registers of one thread, names, memory.
pub struct Ev<'a> {
    pub t: &'a mut dyn Target,
    pub mods: &'a mut Mods,
    pub regs: Regs,
    pub conv: &'a mut HashMap<String, expr::Val>,
    /// Set when the expression named a register these registers have no value for (a caller's frame, see
    /// [`Regs::lost`]): its error then says which, and no name in the files can stand in for that register.
    pub lost: bool,
}

impl Env for Ev<'_> {
    fn reg(&mut self, name: &str) -> Option<u64> {
        self.regs.get(name)
    }

    fn sym(&mut self, name: &str) -> Option<u64> {
        self.mods.resolve(name).ok().map(|p| p.addr())
    }

    fn load(&mut self, addr: u64, size: usize) -> Option<u64> {
        let mut b = [0u8; 8];
        (size <= 8 && self.t.read(addr, &mut b[..size]) == size).then(|| u64::from_le_bytes(b))
    }

    fn var(&mut self, name: &str) -> Option<expr::Val> {
        self.conv.get(name).copied()
    }

    fn set_var(&mut self, name: &str, v: expr::Val) -> bool {
        self.conv.insert(name.to_string(), v);
        true
    }

    fn missing(&mut self, name: &str) -> Option<String> {
        let why = self.regs.is_lost(name).then(|| {
            let n = name.trim_start_matches('$');
            format!("{n} is not saved in this frame: the call out of it may have changed {n} and no unwind rule gives it back")
        });
        self.lost |= why.is_some();
        why
    }

    fn ptr_bytes(&mut self) -> u8 {
        // the program's file class, as `Session::ptr_size` reads it
        if self.mods.main_image().bits == 32 {
            4
        } else {
            8
        }
    }
}

pub fn rows(op: &'static str, lines: Vec<String>, meta: Value) -> Output {
    Output {
        op,
        meta,
        rows: lines.into_iter().enumerate().map(|(i, text)| Row { key: i as u64, text, json: None }).collect(),
        tail: 0,
        command: String::new(),
    }
}

impl Session {
    pub fn new(t: Box<dyn Target>, img: Image) -> Session {
        let os = t.os();
        let mut img = img;
        if let Some(d) = crate::knowledge::data_dir() {
            let _ = nexo_bin::attach_names(&mut img, &d);
        }
        let mut s = Session {
            t,
            mods: Mods::new(img),
            state: State::Stopped,
            cur: 0,
            frame: 0,
            bps: Vec::new(),
            wps: Vec::new(),
            next_id: 1,
            inserted: BTreeMap::new(),
            seen_targets: std::collections::BTreeSet::new(),
            said_saved: false,
            run: Run::default(),
            backlog: Vec::new(),
            prev: HashMap::new(),
            out: Vec::new(),
            out_seen: 0,
            out_dropped: 0,
            log: VecDeque::new(),
            history: VecDeque::new(),
            tape: Default::default(),
            nstop: 0,
            last_stop: None,
            bridged: String::new(),
            catch: default_catch(os),
            exit: None,
            tr: Default::default(),
            conv: HashMap::new(),
            recording: None,
            displays: Vec::new(),
            log_total: 0,
            log_shown: 0,
            calling: None,
            follow: Follow::Parent,
            forks: Vec::new(),
            kids: Vec::new(),
            guards: Vec::new(),
            watch_lines: Vec::new(),
            step_from: None,
            libs: Default::default(),
            finish_from: None,
        };
        s.cur = s.t.threads().first().copied().unwrap_or(s.t.pid());
        s.mods.refresh(&mut *s.t);
        s
    }

    pub fn ev(&mut self, tid: Tid) -> Result<Ev<'_>> {
        let regs = self.t.regs(tid)?;
        Ok(Ev { t: &mut *self.t, mods: &mut self.mods, regs, conv: &mut self.conv, lost: false })
    }

    pub fn need_stopped(&self) -> Result<()> {
        match self.state {
            State::Stopped => Ok(()),
            State::Running => fail("the program is running: `wait N` for the next stop, or `stop` to make it stop"),
            State::Exited => fail(match &self.exit {
                Some(e) => format!("the program has ended ({e})"),
                None => "the program has ended".to_string(),
            }),
        }
    }

    /// Bytes of the program, as they are in memory.
    pub fn read_vec(&mut self, addr: u64, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        let n = self.t.read(addr, &mut v);
        v.truncate(n);
        v
    }

    /// The program's code width from its file's class: 32 for a 32-bit x86 program (ELF32, PE32), else 64.
    pub(crate) fn bits(&self) -> u32 {
        if self.mods.main_image().bits == 32 {
            32
        } else {
            64
        }
    }

    /// The size of a pointer and of a stack slot in the program: 4 or 8 bytes.
    pub(crate) fn ptr_size(&self) -> usize {
        self.bits() as usize / 8
    }

    /// A pointer read from the program's memory: 4 bytes in a 32-bit program, 8 otherwise.
    pub(crate) fn read_ptr(&mut self, addr: u64) -> Option<u64> {
        let n = self.ptr_size();
        let mut b = [0u8; 8];
        (self.t.read(addr, &mut b[..n]) == n).then(|| u64::from_le_bytes(b))
    }

    /// An expression in the selected frame (see `frame`).
    pub fn eval(&mut self, src: &str) -> Result<u64> {
        self.eval_frame(src).map_err(|(e, _)| e)
    }

    /// [`Session::eval`], with `true` beside an error whose reason is a register the selected frame has no value for.
    pub(crate) fn eval_frame(&mut self, src: &str) -> std::result::Result<u64, (crate::Error, bool)> {
        let regs = self.sel_regs().map_err(|e| (e, false))?;
        let mut ev = Ev { t: &mut *self.t, mods: &mut self.mods, regs, conv: &mut self.conv, lost: false };
        expr::value(src, &mut ev).map_err(|e| (e, ev.lost))
    }

    /// An expression with its type, in the selected frame. Without a stopped thread the registers read as zero, so
    /// names, numbers and convenience variables still work (`set \$v = main`).
    pub fn eval_any(&mut self, src: &str) -> Result<expr::Val> {
        let regs = if self.state == State::Stopped { self.sel_regs()? } else { Regs::default() };
        let mut ev = Ev { t: &mut *self.t, mods: &mut self.mods, regs, conv: &mut self.conv, lost: false };
        expr::eval(src, &mut ev)
    }

    pub fn new_id(&mut self) -> u32 {
        let n = self.next_id;
        self.next_id += 1;
        n
    }

    fn wanted(&self) -> BTreeSet<u64> {
        let mut w: BTreeSet<u64> = self.bps.iter().filter(|b| b.enabled && b.addr != 0).map(|b| b.addr).collect();
        if let Some(t) = &self.run.temp {
            w.insert(t.addr);
        }
        w.extend(self.hook_addrs());
        w.extend(self.libs.armed);
        if let Some(p) = &self.calling {
            w.insert(p.ret);
        }
        w
    }

    fn insert_wanted(&mut self) -> Result<()> {
        for a in self.wanted() {
            if self.inserted.contains_key(&a) {
                continue;
            }
            let mut b = [0u8; 1];
            if self.t.read(a, &mut b) != 1 {
                return fail(format!("cannot set a breakpoint at {a:#x}: that memory cannot be read"));
            }
            self.t.write(a, &[0xcc])?;
            self.inserted.insert(a, b[0]);
        }
        let patches: Vec<(u64, u8)> = self.inserted.iter().map(|(a, b)| (*a, *b)).collect();
        self.t.set_patches(&patches);
        Ok(())
    }

    /// A library went away with the bytes under our breakpoints in `lo..hi`: they are forgotten, not written back.
    pub(crate) fn forget_inserted(&mut self, lo: u64, hi: u64) {
        self.inserted.retain(|a, _| *a < lo || *a >= hi);
        let patches: Vec<(u64, u8)> = self.inserted.iter().map(|(a, b)| (*a, *b)).collect();
        self.t.set_patches(&patches);
    }

    /// Put the program's own bytes back everywhere.
    pub fn uninsert_all(&mut self) {
        for (a, b) in std::mem::take(&mut self.inserted) {
            let _ = self.t.write(a, &[b]);
        }
        self.t.set_patches(&[]);
        self.drop_guards();
    }

    /// Threads that sit on an address where a breakpoint is (or will be) step past it alone first.
    fn step_over_wanted(&mut self) -> Result<()> {
        let wanted = self.wanted();
        if wanted.is_empty() {
            return Ok(());
        }
        for tid in self.t.threads() {
            let Ok(r) = self.t.regs(tid) else { continue };
            // a call that just starts at a breakpoint stops there, as gdb does
            if self.call_starts_at(tid, r.rip) {
                continue;
            }
            if !wanted.contains(&r.rip) {
                continue;
            }
            let orig = self.inserted.get(&r.rip).copied();
            if let Some(b) = orig {
                self.t.write(r.rip, &[b])?;
            }
            self.t.resume(Resume::Step(tid))?;
            let ev = self.t.wait(Duration::from_secs(5))?;
            if ev.is_empty() {
                return fail(format!(
                    "thread {tid} did not come off the breakpoint at {:#x} (it blocks in that instruction)",
                    r.rip
                ));
            }
            if let Some(b) = orig {
                if !ev.iter().any(|e| matches!(e, Event::Exited { .. } | Event::Killed { .. })) {
                    let _ = self.t.write(r.rip, &[0xcc]);
                } else {
                    let _ = b;
                }
            }
            let odd: Vec<Event> =
                ev.into_iter().filter(|e| !matches!(e, Event::Stopped { stop: Stop::Step, .. })).collect();
            if !odd.is_empty() {
                self.backlog.extend(odd);
                return Ok(());
            }
        }
        Ok(())
    }

    /// Start running as `run` says. The program must be stopped.
    pub fn begin(&mut self, run: Run) -> Result<()> {
        self.need_stopped()?;
        self.frame = 0;
        self.run = run;
        self.watch_lines.clear();
        // a record single-steps whatever is not already one step: slow, and said in the answer
        self.tape.begin_run(self.tape.on && run.step.is_none());
        let r = self.resume_now();
        if r.is_err() {
            // the program did not start: it is where it was, with none of our breakpoints in it
            self.state = State::Stopped;
            self.uninsert_all();
            self.run = Run::default();
        }
        r
    }

    fn resume_now(&mut self) -> Result<()> {
        if let Some(tid) = self.run.step {
            self.t.resume(Resume::Step(tid))?;
            self.state = State::Running;
            return Ok(());
        }
        self.arm_loads();
        // guarded before a thread steps off a breakpoint, so a write by that one instruction is seen too
        self.guard_pages()?;
        self.step_over_wanted()?;
        self.state = State::Running;
        if !self.backlog.is_empty() {
            return Ok(());
        }
        self.insert_wanted()?;
        if let Some(tid) = self.step_watch_thread() {
            let pc = self.t.regs(tid)?.rip;
            self.step_from = Some((tid, pc));
            return self.t.resume(Resume::Step(tid));
        }
        self.t.resume(Resume::All)
    }

    /// Wait up to `timeout` for the program to stop for a reason worth reporting. `Ok(None)`: still running.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<(Tid, Why)>> {
        let until = Instant::now() + timeout;
        loop {
            if self.state != State::Running {
                return fail("the program is not running");
            }
            let left = until.saturating_duration_since(Instant::now());
            let events = if self.backlog.is_empty() { self.t.wait(left)? } else { std::mem::take(&mut self.backlog) };
            self.drain_output();
            self.take_forks();
            if events.is_empty() {
                return Ok(None);
            }
            if let Some(mut stop) = self.process(events)? {
                if self.state == State::Running {
                    self.state = State::Stopped;
                    self.cur = stop.0;
                    self.frame = 0;
                    self.uninsert_all();
                }
                self.run = Run::default();
                if let Why::LetGo(text) = &mut stop.1 {
                    // `follow child`: the parent goes on alone, its own bytes back in place, and a call it forked in
                    // undone, or the thread would go on inside the call with nowhere to return to
                    if let Some(l) = self.call_let_go() {
                        text.push_str("; ");
                        text.push_str(&l);
                    }
                    let _ = self.t.detach();
                    self.ended("let go at a fork (`follow child`)".into());
                }
                return Ok(Some(stop));
            }
            // an event nobody asked about: go on (a step watch makes one per instruction, so the time is kept)
            self.resume_now()?;
            if Instant::now() >= until {
                return Ok(None);
            }
        }
    }

    pub fn drain_output(&mut self) {
        let new = self.t.output();
        if new.is_empty() {
            return;
        }
        self.out.extend_from_slice(&new);
        if self.out.len() > OUT_KEEP {
            let cut = self.out.len() - OUT_KEEP;
            self.out.drain(..cut);
            self.out_seen = self.out_seen.saturating_sub(cut);
            self.out_dropped += cut as u64;
        }
    }

    fn process(&mut self, events: Vec<Event>) -> Result<Option<(Tid, Why)>> {
        let mut verdict: Option<(Tid, Why)> = None;
        // a thread that ran into one of our breakpoints is put back before the instruction
        let mut bp_hits: Vec<(Tid, u64)> = Vec::new();
        // debug register hits, faults in guarded pages and the steps of a step watch
        let mut watch_evs: Vec<(Tid, Stop)> = Vec::new();
        for ev in &events {
            if let Event::Stopped { tid, stop } = *ev {
                let stepped = self.step_from.is_some_and(|(t, _)| t == tid) && matches!(stop, Stop::Step);
                if stepped && self.tape.on {
                    let from = self.step_from.map(|(_, pc)| pc);
                    if let Ok(r) = self.t.regs(tid) {
                        self.rec_observe(tid, &r, true, from);
                    }
                }
                if stepped || matches!(stop, Stop::Watch(_)) || matches!(stop, Stop::Fault(f) if self.guard_fault(&f)) {
                    watch_evs.push((tid, stop));
                }
            }
            match *ev {
                Event::Exited { code } => {
                    self.ended(format!("exited with code {code}"));
                    return Ok(Some((self.cur, Why::Exited(code))));
                }
                Event::Killed { signal } => {
                    self.ended(format!("killed by {}", signal_name_on(self.t.os(), signal as u32)));
                    return Ok(Some((self.cur, Why::Killed(signal))));
                }
                Event::Stopped { tid, stop: Stop::Breakpoint } => {
                    let mut r = self.t.regs(tid)?;
                    let a = r.rip.wrapping_sub(1);
                    if self.inserted.contains_key(&a) {
                        r.rip = a;
                        self.t.set_regs(tid, &r)?;
                        bp_hits.push((tid, a));
                    } else if verdict.is_none() {
                        verdict = Some((tid, Why::Int3));
                    }
                }
                Event::Stopped { tid, stop: Stop::Syscall(sys) } => {
                    let v = self.on_syscall(tid, sys);
                    if verdict.is_none() {
                        verdict = v;
                    }
                }
                Event::Stopped { stop: Stop::Exec, .. } => self.on_exec(),
                Event::Stopped { tid, stop: Stop::Load } => {
                    let v = self.on_load(tid);
                    if verdict.is_none() {
                        verdict = v;
                    }
                }
                _ => {}
            }
        }
        for (tid, a) in bp_hits {
            let v = self.hit(tid, a)?;
            // a breakpoint that served once (a call's return) goes: the thread runs the program's byte
            if !self.wanted().contains(&a) {
                if let Some(b) = self.inserted.remove(&a) {
                    self.t.write(a, &[b])?;
                    let patches: Vec<(u64, u8)> = self.inserted.iter().map(|(a, b)| (*a, *b)).collect();
                    self.t.set_patches(&patches);
                }
            }
            if verdict.is_none() {
                verdict = v;
            }
        }
        let taken: Vec<(Tid, Stop)> = watch_evs.clone();
        if !watch_evs.is_empty() {
            let v = self.watch_events(watch_evs)?;
            if verdict.is_none() {
                verdict = v;
            }
        }
        for ev in &events {
            if verdict.is_some() {
                break;
            }
            if let Event::Stopped { tid, stop } = *ev {
                if matches!(stop, Stop::Fault(_)) && taken.contains(&(tid, stop)) {
                    continue;
                }
                match stop {
                    Stop::Step | Stop::Watch(_) if self.run.step.is_some() => verdict = Some((tid, Why::Step)),
                    Stop::Fault(f) => {
                        if self.catch.contains(&f.number) || (self.t.os() == Os::Windows && !f.first_chance) {
                            verdict = Some((tid, Why::Signal(f)));
                        }
                    }
                    Stop::Interrupt => verdict = Some((tid, Why::Interrupt)),
                    Stop::Exec => verdict = Some((tid, Why::Exec)),
                    Stop::Fork(pid) => verdict = Some((tid, Why::LetGo(self.let_go_text(pid)))),
                    _ => {}
                }
            }
        }
        Ok(verdict)
    }

    fn ended(&mut self, how: String) {
        self.state = State::Exited;
        self.exit = Some(how);
        self.inserted.clear();
        self.drain_output();
        self.hooks_end();
        self.trace_end();
    }

    /// A thread is at one of our breakpoints: does it stop?
    fn hit(&mut self, tid: Tid, addr: u64) -> Result<Option<(Tid, Why)>> {
        if self.call_returned(tid, addr)? {
            return Ok(Some((tid, Why::Returned)));
        }
        // the loader's hook: libraries came or went, which may set breakpoints before the ones at this address
        let loaded = if self.libs.armed == Some(addr) { self.on_load(tid) } else { None };
        // the trace and the hooks see every pass, whatever a breakpoint there decides
        let traced = self.hook_hit(tid, addr)?.or(loaded);
        let mut stop_ids: Vec<u32> = Vec::new();
        let mut spent: Vec<u32> = Vec::new();
        let ids: Vec<u32> = self.bps.iter().filter(|b| b.enabled && b.addr == addr).map(|b| b.id).collect();
        for id in ids {
            let Some(i) = self.bps.iter().position(|b| b.id == id) else { continue };
            if let Some(c) = self.bps[i].cond.clone() {
                let tid_regs = self.ev(tid);
                let truth = match tid_regs {
                    Ok(mut ev) => expr::value(&c, &mut ev),
                    Err(e) => Err(e),
                };
                match truth {
                    Ok(0) => continue,
                    Ok(_) => {}
                    // a condition that cannot be worked out stops, and says so
                    Err(e) => {
                        self.log_line(format!("breakpoint #{id}: condition {c:?} failed: {e}"));
                    }
                }
            }
            self.bps[i].hits += 1;
            if self.bps[i].hits <= self.bps[i].after {
                continue;
            }
            if self.bps[i].call {
                let line = self.call_line(tid, addr);
                self.log_line(line);
                continue;
            }
            if let Some(text) = self.bps[i].log.clone() {
                let line = self.format_log(tid, &text);
                self.log_line(format!("#{id} {line}"));
                continue;
            }
            if !self.bps[i].actions.is_empty() {
                let go_on = self.run_actions(tid, id);
                let hits = self.bps[i].hits;
                if go_on && hits < ACTION_HITS {
                    if self.bps[i].once {
                        spent.push(id);
                    }
                    continue;
                }
                if go_on {
                    self.log_line(format!(
                        "#{id} stops after {ACTION_HITS} hits so the answer comes back (`cont` goes on, the actions keep running)"
                    ));
                    self.bps[i].hits = 0;
                }
            }
            stop_ids.push(id);
        }
        self.bps.retain(|b| !spent.contains(&b.id));
        if !stop_ids.is_empty() {
            self.bps.retain(|b| !(b.once && stop_ids.contains(&b.id)));
            return Ok(Some((tid, Why::Break(stop_ids))));
        }
        if let Some(t) = self.run.temp {
            if t.addr == addr {
                let sp = self.t.regs(tid)?.rsp();
                if sp >= t.min_sp {
                    let why = match t.kind {
                        TempKind::Entry if self.t.os() == Os::MacOS => {
                            self.back_to_the_call_of(tid, addr);
                            Why::Entry
                        }
                        TempKind::Entry => Why::Entry,
                        TempKind::Next => Why::Next,
                        TempKind::Finish => Why::Finish,
                        TempKind::Until => Why::Until,
                    };
                    return Ok(Some((tid, why)));
                }
            }
        }
        Ok(traced)
    }

    /// macOS: a program's entry (LC_MAIN) is its `main`, which dyld calls once the libraries are loaded and their
    /// initializers ran. The entry stop goes back to the instruction in dyld that calls it (rip on the call, the
    /// return address off the stack), so a breakpoint set there on `main` is hit by the next `cont`, as after the
    /// entry stop on Linux. Left at `main` when no call there goes to it.
    fn back_to_the_call_of(&mut self, tid: Tid, main: u64) {
        let Ok(mut r) = self.t.regs(tid) else { return };
        let Ok(back) = <[u8; 8]>::try_from(self.read_vec(r.rsp(), 8)) else { return };
        let back = u64::from_le_bytes(back);
        let word = |s: &mut Self, a: u64| <[u8; 8]>::try_from(s.read_vec(a, 8)).ok().map(u64::from_le_bytes);
        for len in 2..=7u64 {
            let at = back.wrapping_sub(len);
            let mut bytes = self.read_vec(at, len as usize);
            for (i, b) in bytes.iter_mut().enumerate() {
                if let Some(o) = self.inserted.get(&(at + i as u64)) {
                    *b = *o;
                }
            }
            let Some(ins) = nexo_bin::decode_live(&bytes, at, &|n| r.get(n)) else { continue };
            if !ins.call || ins.len as u64 != len {
                continue;
            }
            let goes = match ins.target {
                Some((t, false)) => Some(t),
                Some((slot, true)) => word(self, slot),
                None => nexo_bin::indirect_call_target(&bytes, at, &|n| r.get(n), &|_| None).or_else(|| {
                    let slot = ins.mem.iter().find(|m| m.read && !m.write)?.addr?;
                    word(self, slot)
                }),
            };
            if goes == Some(main) {
                r.rip = at;
                r.set("rsp", r.rsp() + 8);
                let _ = self.t.set_regs(tid, &r);
                return;
            }
        }
    }

    /// The byte our own int3 replaced at `addr`, if one sits there.
    pub(crate) fn inserted_byte(&self, addr: u64) -> Option<u8> {
        self.inserted.get(&addr).copied()
    }

    /// `call 0x401136 main+0x20 -> 0x401220 helper`: the real target of the indirect call at `addr` now.
    fn call_line(&mut self, tid: Tid, addr: u64) -> String {
        let from = self.mods.label(addr, false).unwrap_or_default();
        let mut bytes = self.read_vec(addr, 16);
        // our own int3 sits on the first byte while the program runs
        if let (Some(b), Some(o)) = (bytes.first_mut(), self.inserted.get(&addr)) {
            *b = *o;
        }
        let regs = self.t.regs(tid).ok();
        let (bits, n) = (self.bits(), self.ptr_size());
        let to = regs.and_then(|r| {
            let t = std::cell::RefCell::new(&mut self.t);
            nexo_bin::indirect_call_target_in(bits, &bytes, addr, &|n| r.get(n), &|a| {
                let mut b = [0u8; 8];
                (t.borrow_mut().read(a, &mut b[..n]) == n).then(|| u64::from_le_bytes(b))
            })
        });
        match to {
            Some(t) => {
                if self.mods.locate(t).is_none() {
                    // a library that came in after the last look
                    self.mods.refresh(&mut *self.t);
                }
                let name = self.mods.label(t, false).unwrap_or_else(|| "?".into());
                self.keep_target(addr, t, &name);
                format!("call {addr:#x} {from} -> {t:#x} {name}")
            }
            None => format!("call {addr:#x} {from} -> ? (target could not be read)"),
        }
    }

    /// Save what an indirect call went to, for `nexo bin decomp` to show. Only the program's own sites, once per pair.
    fn keep_target(&mut self, site: u64, target: u64, name: &str) {
        let Some(dir) = crate::knowledge::data_dir() else { return };
        let main = self.mods.main_base();
        if main == 0 || self.mods.locate(site) != self.mods.locate(main) {
            return;
        }
        let bias = self.mods.main_bias();
        let entry = if self.mods.locate(target) == self.mods.locate(main) {
            format!("{:#x} {name}", target.wrapping_sub(bias))
        } else {
            name.to_string()
        };
        let site = site.wrapping_sub(bias);
        if self.seen_targets.insert((site, entry.clone())) {
            let saved = nexo_bin::add_call_target(self.mods.main_image(), &dir, site, &entry);
            if saved.is_ok() && !self.said_saved {
                self.said_saved = true;
                self.log_line(format!(
                    "saved indirect-call targets for `nexo bin decomp` in {} (more are added as they are seen)",
                    nexo_bin::names_path(&dir, &self.mods.main_image().store_key()).display()
                ));
            }
        }
    }

    pub fn log_line(&mut self, s: String) {
        self.log_total += 1;
        self.log.push_back(s);
        while self.log.len() > LOG_KEEP {
            self.log.pop_front();
        }
    }

    /// Write down the children the program forked since the last look. Each one the backend kept becomes a session
    /// of its own in `kids`, with the breakpoints copied.
    fn take_forks(&mut self) {
        for f in self.t.forks() {
            let Forked { pid, at, vfork, patches, target, .. } = f;
            // fork returns in libc, which the list read at the start may not hold yet
            if self.mods.locate(at).is_none() {
                self.mods.refresh(&mut *self.t);
            }
            let name = self.mods.label(at, false).unwrap_or_default();
            let place = if name.is_empty() { format!("{at:#x}") } else { format!("{at:#x} {name}") };
            let session = target.and_then(|t| self.adopt(t, pid, &place, vfork, &patches));
            if let Some(id) = &session {
                if self.follow == Follow::Both {
                    let (kind, when) = if vfork { ("vfork", "at its exec") } else { ("fork", "where it was made") };
                    self.log_line(format!(
                        "pid {pid} ({kind} at {place}) is session {id}, stopped {when}: `-s {id} cont` runs it"
                    ));
                }
            }
            self.forks.push(ForkNote { pid, place, vfork, session });
        }
    }

    /// A forked child the backend kept: a session of its own, stopped where fork returned 0 in it (a vfork child: at
    /// its exec), with the breakpoints copied. `None` when its program cannot be read; the child then goes when `t` is dropped.
    fn adopt(
        &mut self,
        t: Box<dyn Target>,
        pid: u32,
        place: &str,
        vfork: bool,
        patches: &[(u64, u8)],
    ) -> Option<String> {
        let img = match Image::open_arch(self.mods.exe_path(), crate::platform::NATIVE_SLICE) {
            Ok(i) => i,
            Err(e) => {
                self.log_line(format!("pid {pid} forked at {place} was not kept: its program cannot be read ({e})"));
                return None;
            }
        };
        let mut kid = Session::new(t, img);
        kid.follow = self.follow;
        kid.t.set_follow(self.follow);
        kid.bps = self.bps.iter().filter(|b| !b.once).cloned().map(|b| Bp { hits: 0, ..b }).collect();
        kid.next_id = self.next_id;
        kid.catch = self.catch.clone();
        let ids: Vec<String> = kid.bps.iter().filter(|b| !b.call).map(|b| format!("#{}", b.id)).collect();
        let parent = self.t.pid();
        let text = if vfork {
            // a vfork child comes at its exec: the memory it shared is the parent's alone again, and the copied
            // breakpoints are addresses in the parent's program, so they are off; names come from the new program
            kid.on_exec();
            let mods: Vec<String> = kid.mods.list.iter().map(|m| format!("{} at {:#x}", m.path, m.base)).collect();
            let copied = if ids.is_empty() {
                "no breakpoints to copy".to_string()
            } else {
                format!("breakpoints {} copied and off (addresses in pid {parent}'s program)", ids.join(" "))
            };
            format!(
                "pid {pid}, made by vfork in pid {parent} at {place}: stopped at its exec of {}; modules now: {}; {copied} (watches, hooks and traces stay with pid {parent})",
                kid.mods.exe_path().display(),
                mods.join(", ")
            )
        } else {
            // the child's memory holds 0xcc where the parent's did at the fork; a stopped session holds the
            // program's own bytes, and the child's next run puts its breakpoints back
            kid.inserted = patches.iter().copied().collect();
            kid.t.set_patches(patches);
            // the child's copy of a guarded page is guarded too, and its watch stays with the parent
            kid.guards = self.guards.iter().filter(|g| g.armed).copied().collect();
            kid.uninsert_all();
            kid.guards.clear();
            let copied = if ids.is_empty() {
                "no breakpoints to copy".to_string()
            } else {
                format!("breakpoints {} copied", ids.join(" "))
            };
            format!(
                "pid {pid}, made by fork in pid {parent} at {place}: stopped where fork returned 0 in it; {copied} (watches, hooks and traces stay with pid {parent})"
            )
        };
        let first = kid.answer(pid as Tid, Why::Fork(text));
        let id = crate::server::new_id();
        self.kids.push(Kid { id: id.clone(), sess: kid, first });
        Some(id)
    }

    /// The answer of a parent let go at a fork under `follow child`.
    fn let_go_text(&self, pid: u32) -> String {
        match self.forks.iter().rev().find(|f| f.pid == pid) {
            Some(ForkNote { place, session: Some(id), .. }) => format!(
                "the program forked pid {pid} at {place}: under `follow child` the child is session {id} and this process goes on without the debugger; `-s {id} cont` runs the child"
            ),
            _ => format!("the program forked pid {pid}: under `follow child` this process goes on without the debugger"),
        }
    }

    /// The program replaced itself (exec). The bytes under our breakpoints went with the old program, and what was
    /// set by address in it does not point into the new one, so it is switched off; names and lines come from the new
    /// program from here on.
    fn on_exec(&mut self) {
        self.inserted.clear();
        self.t.set_patches(&[]);
        self.libs.forget();
        self.tr.pending.clear();
        let mut off = Vec::new();
        for b in self.bps.iter_mut().filter(|b| b.enabled) {
            b.enabled = false;
            off.push(format!("#{}", b.id));
        }
        let mut gone = Vec::new();
        if self.tr.lib.take().is_some() {
            gone.push("ltrace");
        }
        if !self.tr.hooks.is_empty() {
            self.tr.hooks.clear();
            gone.push("hooks");
        }
        if self.tr.cover.take().is_some() {
            gone.push("cover");
        }
        // the guarded pages went with the old program
        self.guards.clear();
        self.step_from = None;
        let n = self.wps.len();
        self.wps.retain(|w| w.how == WatchHow::Hardware);
        if self.wps.len() < n {
            gone.push("page and step watches");
        }
        if self.t.os() == Os::Linux {
            if let Ok(path) = std::fs::read_link(format!("/proc/{}/exe", self.t.pid())) {
                if let Ok(mut img) = Image::open_arch(&path, crate::platform::NATIVE_SLICE) {
                    if let Some(d) = crate::knowledge::data_dir() {
                        let _ = nexo_bin::attach_names(&mut img, &d);
                    }
                    self.mods = Mods::new(img);
                }
            }
        }
        self.mods.refresh(&mut *self.t);
        if !off.is_empty() {
            self.log_line(format!(
                "breakpoints {} were addresses in the old program and are disabled now (`enable <id>` arms one at the same address)",
                off.join(" ")
            ));
        }
        if !gone.is_empty() {
            self.log_line(format!("{} were set on the old program and are off now", gone.join(", ")));
        }
    }

    /// `x is {rax}` with each `{expression}` replaced by its value.
    fn format_log(&mut self, tid: Tid, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            let Some(j) = rest[i..].find('}') else {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            };
            let e = &rest[i + 1..i + j];
            // `{mem:EXPR:N}`: N bytes at EXPR as hex
            if let Some(m) = e.strip_prefix("mem:") {
                let shown = self.log_bytes(tid, m);
                out.push_str(&shown);
                rest = &rest[i + j + 1..];
                continue;
            }
            let (src, fmt) = e.rsplit_once(':').filter(|(_, f)| matches!(*f, "x" | "d" | "s")).unwrap_or((e, "x"));
            let shown = match self.ev(tid).and_then(|mut ev| expr::eval(src, &mut ev)) {
                Ok(v) if fmt == "d" => format!("{}", v.v as i64),
                Ok(v) if fmt == "s" => self.cstr(v.v),
                Ok(v) => format!("{:#x}", v.v),
                Err(e) => format!("<{e}>"),
            };
            out.push_str(&shown);
            rest = &rest[i + j + 1..];
        }
        out.push_str(rest);
        out
    }

    /// `{mem:EXPR:N}` of a logpoint: N (1 to 256, default 16) bytes at the address EXPR works out to, as hex.
    fn log_bytes(&mut self, tid: Tid, spec: &str) -> String {
        let (src, n) = match spec.rsplit_once(':').and_then(|(a, n)| Some((a, nexo_bin::parse_addr(n).ok()?))) {
            Some((a, n)) => (a, n as usize),
            None => (spec, 16),
        };
        if n == 0 || n > LOG_BYTES {
            return format!("<mem takes 1 to {LOG_BYTES} bytes>");
        }
        let at = match self.ev(tid).and_then(|mut ev| expr::value(src, &mut ev)) {
            Ok(a) => a,
            Err(e) => return format!("<{e}>"),
        };
        let b = self.read_vec(at, n);
        if b.is_empty() {
            return format!("<cannot read {at:#x}>");
        }
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
    }

    /// The text at an address up to a zero byte, at most 200 bytes, as a quoted string.
    pub fn cstr(&mut self, addr: u64) -> String {
        let b = self.read_vec(addr, 200);
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        let s: String = b[..end]
            .iter()
            .map(|&c| match c {
                b'"' => "\\\"".to_string(),
                b'\\' => "\\\\".to_string(),
                b'\n' => "\\n".to_string(),
                0x20..=0x7e => (c as char).to_string(),
                _ => format!("\\x{c:02x}"),
            })
            .collect();
        format!("\"{s}\"")
    }

    /// The answer for a stop [`Session::poll`] caught. While a call into the program is under way the stop goes
    /// through the call first: its return gives the value and puts the registers back, a fault in its thread ends it.
    pub fn answer(&mut self, tid: Tid, why: Why) -> Output {
        let Some(text) = self.call_under_way() else { return self.stopped(tid, why) };
        match self.call_stop(tid, why) {
            call::Called::Value(v) => self.call_value(&text, tid, v),
            call::Called::Told(o) => o,
        }
    }

    /// One look at the running program between requests (the server does this while nobody asks): the answer for a
    /// stop that came within `timeout`. None while it still runs, when it is not running, or when it cannot be
    /// waited on.
    pub fn news(&mut self, timeout: Duration) -> Option<Output> {
        if self.state != State::Running {
            return None;
        }
        self.drain_output();
        let (tid, why) = self.poll(timeout).ok()??;
        Some(self.answer(tid, why))
    }

    /// Block until the program stops or `wait` passes, then describe the stop (or say it still runs).
    pub fn settle(&mut self, wait: Duration) -> Result<Output> {
        let deadline = Instant::now() + wait;
        loop {
            let slice = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(200));
            if let Some((tid, why)) = self.poll(slice)? {
                return Ok(self.answer(tid, why));
            }
            if Instant::now() >= deadline {
                let secs = wait.as_secs_f64();
                return Ok(rows(
                    "running",
                    vec![
                        format!(
                            "still running after {secs:.0}s; `wait N` waits for the next stop, `stop` makes it stop"
                        ),
                        self.new_output_line().unwrap_or_default(),
                    ]
                    .into_iter()
                    .filter(|l| !l.is_empty())
                    .collect(),
                    json!({"state": "running"}),
                ));
            }
        }
    }

    /// The program's output since the last time it was shown, the first part of it, in one line.
    pub fn new_output_line(&mut self) -> Option<String> {
        self.drain_output();
        if self.out_seen >= self.out.len() {
            return None;
        }
        let new = &self.out[self.out_seen..];
        let shown: String = String::from_utf8_lossy(&new[..new.len().min(300)]).replace('\n', "\\n");
        let more = if new.len() > 300 { format!("... ({} bytes; `out` shows them)", new.len()) } else { String::new() };
        Some(format!("output: {shown}{more}"))
    }

    /// Run `run` and wait for it. The common path of `cont`, `next`, `finish`.
    pub fn go(&mut self, run: Run, wait: Duration) -> Result<Output> {
        self.begin(run)?;
        self.settle(wait)
    }

    /// Stop everything: used when the program has to be looked at while it runs.
    pub fn stop_now(&mut self) -> Result<Output> {
        if self.state != State::Running {
            return fail("the program is not running");
        }
        self.t.interrupt()?;
        self.settle(Duration::from_secs(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Fault;

    #[test]
    fn a_macos_fault_is_named_by_its_mach_exception_and_signal_in_macos_numbers() {
        let f = |number, code| Fault { number, code, addr: Some(0), touch: None, first_chance: true };
        assert_eq!(fault_line(Os::MacOS, &f(11, 1)), "EXC_BAD_ACCESS (SIGSEGV) KERN_INVALID_ADDRESS (signal 11)");
        assert_eq!(fault_line(Os::MacOS, &f(10, 2)), "EXC_BAD_ACCESS (SIGBUS) KERN_PROTECTION_FAILURE (signal 10)");
        assert_eq!(fault_line(Os::MacOS, &f(8, 1)), "EXC_ARITHMETIC (SIGFPE) EXC_I386_DIV (signal 8)");
        // a signal another process sent carries no Mach code
        assert_eq!(fault_line(Os::MacOS, &f(30, 0)), "SIGUSR1 (signal 30)");
        assert_eq!(fault_line(Os::Linux, &f(10, 1)), "SIGUSR1 (signal 10)");
        assert_eq!(signal_name_on(Os::MacOS, 12), "SIGSYS");
        let catch = default_catch(Os::MacOS);
        assert!(catch.contains(&10) && catch.contains(&12) && !catch.contains(&31), "{catch:?}");
    }
}
