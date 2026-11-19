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
mod checkpoint;
mod events;
mod signals;
pub mod watch;

pub use checkpoint::Checkpoint;
pub use signals::*;

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
    /// A watch on a place in a function's frame ends when that frame returns.
    pub scope: Option<WatchScope>,
}

/// The frame a watch on a local belongs to: the watch is deleted, with one stop, when the function returns to
/// `ret` with the stack pointer back at `cfa` or above it (a deeper call of the same function returns there too,
/// but lower).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchScope {
    /// The frame's CFA: the stack pointer after the function returns.
    pub cfa: u64,
    /// Where the function returns to: an internal breakpoint sits there while the watch lives.
    pub ret: u64,
    /// The thread the frame belongs to.
    pub tid: Tid,
    /// The expression as typed, and the function whose frame it is.
    pub what: String,
    pub func: String,
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
    /// The heap check found a free or realloc of a pointer that is not a live block: what it says.
    Heap(String),
    /// A call made by `call` or `print` came back to its return address.
    Returned,
    /// `restart N`: the program is back at a checkpoint; what it says.
    Restart(String),
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
    /// Kills the process a `restart` made the program when the session goes; first so it drops before `t`.
    reaper: checkpoint::Reaper,
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
    /// `stops compact`: a stop is one line (why, where, the displays) except a crash; `compact_told` once the
    /// way back (`stops full`) was said.
    pub compact_stops: bool,
    pub compact_told: bool,
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
    /// The checkpoints (`checkpoint`): processes forked from the program, stopped and attached.
    pub checkpoints: Vec<Checkpoint>,
    ckpt_next: u32,
    /// While a checkpoint's `fork()` runs: the children the backend kept, with the pages guarded at that moment.
    ckpt_forks: Option<Vec<(Forked, Vec<Guard>)>>,
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
            compact_stops: false,
            compact_told: false,
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
            reaper: Default::default(),
            checkpoints: Vec::new(),
            ckpt_next: 1,
            ckpt_forks: None,
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

    /// Where an expression that names memory lives (`*(u32*)(rdi+8)`, `p[i]`), in the selected frame; `None` when it
    /// is not one of those forms.
    pub fn eval_place(&mut self, src: &str) -> Result<Option<expr::Place>> {
        let regs = self.sel_regs()?;
        let mut ev = Ev { t: &mut *self.t, mods: &mut self.mods, regs, conv: &mut self.conv, lost: false };
        expr::place(src, &mut ev)
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
        w.extend(self.wps.iter().filter_map(|w| w.scope.as_ref().map(|s| s.ret)));
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
