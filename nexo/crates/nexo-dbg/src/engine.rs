//! One debugging session: the program under control, the breakpoints, and the rules for running it
//! until something worth a person's attention happens. The commands people type are in `cmds`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use nexo_bin::{Image, Output, Row};
use serde_json::{json, Value};

use crate::expr::{self, Env};
use crate::modules::Mods;
use crate::platform::{Event, Fault, HwSlot, Os, Resume, Stop, Target, Tid};
use crate::regs::Regs;
use crate::{fail, Result};

/// Stops remembered for `history`.
pub const HISTORY: usize = 64;
/// Bytes of the program's output kept.
pub const OUT_KEEP: usize = 1 << 20;
/// Lines of logpoint output kept.
pub const LOG_KEEP: usize = 1000;

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
}

pub struct Wp {
    pub id: u32,
    pub slot: usize,
    pub hw: HwSlot,
    pub last: Vec<u8>,
    pub hits: u64,
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
    pub bps: Vec<Bp>,
    pub wps: Vec<Wp>,
    pub next_id: u32,
    inserted: BTreeMap<u64, u8>,
    pub run: Run,
    backlog: Vec<Event>,
    pub prev: HashMap<Tid, Regs>,
    pub out: Vec<u8>,
    pub out_seen: usize,
    pub out_dropped: u64,
    pub log: VecDeque<String>,
    pub history: VecDeque<Rec>,
    pub nstop: u64,
    /// Where the last stop was, for the header of the answer.
    pub last_stop: Option<StopInfo>,
    pub catch: BTreeSet<u32>,
    pub exit: Option<String>,
    /// `strace`, `ltrace`, hooks and coverage.
    pub tr: crate::trace::Tracing,
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
        Os::Windows => match exception_name(number) {
            Some(n) => format!("{n} (exception {number:#010x})"),
            None => format!("exception {number:#010x}"),
        },
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
            bps: Vec::new(),
            wps: Vec::new(),
            next_id: 1,
            inserted: BTreeMap::new(),
            run: Run::default(),
            backlog: Vec::new(),
            prev: HashMap::new(),
            out: Vec::new(),
            out_seen: 0,
            out_dropped: 0,
            log: VecDeque::new(),
            history: VecDeque::new(),
            nstop: 0,
            last_stop: None,
            catch: default_catch(os),
            exit: None,
            tr: Default::default(),
        };
        s.cur = s.t.threads().first().copied().unwrap_or(s.t.pid());
        s.mods.refresh(&mut *s.t);
        s
    }

    pub fn ev(&mut self, tid: Tid) -> Result<Ev<'_>> {
        let regs = self.t.regs(tid)?;
        Ok(Ev { t: &mut *self.t, mods: &mut self.mods, regs })
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

    pub fn eval(&mut self, src: &str) -> Result<u64> {
        let tid = self.cur;
        let mut ev = self.ev(tid)?;
        expr::value(src, &mut ev)
    }

    pub fn new_id(&mut self) -> u32 {
        let n = self.next_id;
        self.next_id += 1;
        n
    }

    fn wanted(&self) -> BTreeSet<u64> {
        let mut w: BTreeSet<u64> = self.bps.iter().filter(|b| b.enabled).map(|b| b.addr).collect();
        if let Some(t) = &self.run.temp {
            w.insert(t.addr);
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

    /// Put the program's own bytes back everywhere.
    pub fn uninsert_all(&mut self) {
        for (a, b) in std::mem::take(&mut self.inserted) {
            let _ = self.t.write(a, &[b]);
        }
        self.t.set_patches(&[]);
    }

    /// Threads that sit on an address where a breakpoint is (or will be) step past it alone first.
    fn step_over_wanted(&mut self) -> Result<()> {
        let wanted = self.wanted();
        if wanted.is_empty() {
            return Ok(());
        }
        for tid in self.t.threads() {
            let Ok(r) = self.t.regs(tid) else { continue };
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
        self.run = run;
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
        self.step_over_wanted()?;
        self.state = State::Running;
        if !self.backlog.is_empty() {
            return Ok(());
        }
        self.insert_wanted()?;
        self.t.resume(Resume::All)
    }

    /// Wait up to `timeout` for the program to stop for a reason worth reporting. `Ok(None)`: still running.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<(Tid, Why)>> {
        loop {
            if self.state != State::Running {
                return fail("the program is not running");
            }
            let events =
                if self.backlog.is_empty() { self.t.wait(timeout)? } else { std::mem::take(&mut self.backlog) };
            self.drain_output();
            if events.is_empty() {
                return Ok(None);
            }
            if let Some(stop) = self.process(events)? {
                if self.state == State::Running {
                    self.state = State::Stopped;
                    self.cur = stop.0;
                    self.uninsert_all();
                }
                self.run = Run::default();
                return Ok(Some(stop));
            }
            // an event nobody asked about: go on
            self.resume_now()?;
            if timeout.is_zero() {
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
        for ev in &events {
            match *ev {
                Event::Exited { code } => {
                    self.ended(format!("exited with code {code}"));
                    return Ok(Some((self.cur, Why::Exited(code))));
                }
                Event::Killed { signal } => {
                    self.ended(format!("killed by {}", signal_name(signal as u32)));
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
                _ => {}
            }
        }
        for (tid, a) in bp_hits {
            let v = self.hit(tid, a)?;
            if verdict.is_none() {
                verdict = v;
            }
        }
        for ev in &events {
            if verdict.is_some() {
                break;
            }
            if let Event::Stopped { tid, stop } = *ev {
                match stop {
                    Stop::Step if self.run.step.is_some() => verdict = Some((tid, Why::Step)),
                    Stop::Watch(bits) => {
                        let ids: Vec<u32> = self.wps.iter().filter(|w| bits >> w.slot & 1 == 1).map(|w| w.id).collect();
                        verdict = Some((tid, Why::Watch(ids)));
                    }
                    Stop::Fault(f) => {
                        if self.catch.contains(&f.number) || (self.t.os() == Os::Windows && !f.first_chance) {
                            verdict = Some((tid, Why::Signal(f)));
                        }
                    }
                    Stop::Interrupt => verdict = Some((tid, Why::Interrupt)),
                    Stop::Exec => verdict = Some((tid, Why::Exec)),
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
        self.trace_end();
    }

    /// A thread is at one of our breakpoints: does it stop?
    fn hit(&mut self, tid: Tid, addr: u64) -> Result<Option<(Tid, Why)>> {
        let mut stop_ids: Vec<u32> = Vec::new();
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
            stop_ids.push(id);
        }
        if !stop_ids.is_empty() {
            self.bps.retain(|b| !(b.once && stop_ids.contains(&b.id)));
            return Ok(Some((tid, Why::Break(stop_ids))));
        }
        if let Some(t) = self.run.temp {
            if t.addr == addr {
                let sp = self.t.regs(tid)?.rsp();
                if sp >= t.min_sp {
                    let why = match t.kind {
                        TempKind::Entry => Why::Entry,
                        TempKind::Next => Why::Next,
                        TempKind::Finish => Why::Finish,
                        TempKind::Until => Why::Until,
                    };
                    return Ok(Some((tid, why)));
                }
            }
        }
        Ok(None)
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
        let to = regs.and_then(|r| {
            let t = std::cell::RefCell::new(&mut self.t);
            nexo_bin::indirect_call_target(&bytes, addr, &|n| r.get(n), &|a| {
                let mut b = [0u8; 8];
                (t.borrow_mut().read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
            })
        });
        match to {
            Some(t) => {
                if self.mods.locate(t).is_none() {
                    // a library that came in after the last look
                    self.mods.refresh(&mut *self.t);
                }
                let name = self.mods.label(t, false).unwrap_or_else(|| "?".into());
                format!("call {addr:#x} {from} -> {t:#x} {name}")
            }
            None => format!("call {addr:#x} {from} -> ? (target could not be read)"),
        }
    }

    pub fn log_line(&mut self, s: String) {
        self.log.push_back(s);
        while self.log.len() > LOG_KEEP {
            self.log.pop_front();
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

    /// Block until the program stops or `wait` passes, then describe the stop (or say it still runs).
    pub fn settle(&mut self, wait: Duration) -> Result<Output> {
        let deadline = Instant::now() + wait;
        loop {
            let slice = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(200));
            if let Some((tid, why)) = self.poll(slice)? {
                return Ok(self.stopped(tid, why));
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
