//! Calls into the live program: `call f(1, "abc")` and `print f(x) + 1`. The thread's registers are saved, the
//! arguments go where the ABI puts them (System V: rdi, rsi, rdx, rcx, r8, r9, then the stack; Windows x64: rcx,
//! rdx, r8, r9, 32 bytes of shadow space, then the stack), text goes on the stack below the red zone, and the
//! return address is the program's entry, where a breakpoint waits. The whole program runs until that thread
//! comes back to the entry with its stack where the call left it, or until something else stops it. Then rax is
//! the value and every register (the vector ones too, where the system gives them) is put back.

use super::*;
use crate::expr::Arg;

/// Bytes kept free below the stopped code's stack pointer: the System V red zone (128) and some more.
const BELOW_SP: u64 = 256;

/// A call under way.
pub struct Pending {
    pub tid: Tid,
    /// The call as the answers name it: `add(2, 3)`.
    pub text: String,
    saved: Regs,
    vector: Option<Vec<u8>>,
    /// The signal the thread was to get when it ran again, held back while the call runs.
    signal: u64,
    /// Where the call returns to: the program's entry.
    pub ret: u64,
    /// The stack pointer once the call has returned.
    sp: u64,
    /// The function called, and whether the thread has not run yet: a breakpoint there stops the call at once.
    target: u64,
    fresh: bool,
}

/// How a call ended for the expression that made it.
pub enum Called {
    Value(u64),
    /// It did not come back with a value (a fault, a breakpoint inside it, still running): the answer that says so.
    Told(Output),
}

/// The expression of `call` and `print` with a call: [`Ev`] plus calls into the program.
struct CallEv<'a> {
    s: &'a mut Session,
    tid: Tid,
    regs: Regs,
    wait: Duration,
    told: Option<Output>,
}

impl CallEv<'_> {
    fn ev(&mut self) -> Ev<'_> {
        Ev { t: &mut *self.s.t, mods: &mut self.s.mods, regs: self.regs, conv: &mut self.s.conv, lost: false }
    }
}

impl Env for CallEv<'_> {
    fn reg(&mut self, name: &str) -> Option<u64> {
        self.ev().reg(name)
    }

    fn sym(&mut self, name: &str) -> Option<u64> {
        self.ev().sym(name)
    }

    fn load(&mut self, addr: u64, size: usize) -> Option<u64> {
        self.ev().load(addr, size)
    }

    fn var(&mut self, name: &str) -> Option<expr::Val> {
        self.ev().var(name)
    }

    fn set_var(&mut self, name: &str, v: expr::Val) -> bool {
        self.ev().set_var(name, v)
    }

    fn missing(&mut self, name: &str) -> Option<String> {
        self.ev().missing(name)
    }

    fn ptr_bytes(&mut self) -> u8 {
        self.ev().ptr_bytes()
    }

    fn call(&mut self, target: u64, args: &[Arg]) -> Result<u64> {
        let text = self.s.call_text(target, args);
        match self.s.call_now(self.tid, target, args, text, self.wait)? {
            Called::Value(v) => Ok(v),
            Called::Told(o) => {
                self.told = Some(o);
                fail("the call did not come back with a value")
            }
        }
    }
}

impl Session {
    /// An expression that may call functions of the program, in the selected frame. `Err` inside `Ok`: a call did
    /// not come back with a value, and this is the answer that says what happened instead.
    pub(crate) fn eval_calling(&mut self, src: &str, wait: Duration) -> Result<std::result::Result<expr::Val, Output>> {
        self.need_stopped()?;
        let (tid, regs) = (self.cur, self.sel_regs()?);
        let mut ev = CallEv { s: self, tid, regs, wait, told: None };
        let r = expr::eval(src, &mut ev);
        match ev.told.take() {
            Some(o) => Ok(Err(o)),
            None => r.map(Ok),
        }
    }

    /// `add(2, "abc")`: the function by name where it has one, the arguments as typed.
    fn call_text(&mut self, target: u64, args: &[Arg]) -> String {
        let f = self.mods.label(target, false).unwrap_or_else(|| format!("{target:#x}"));
        let a: Vec<String> = args
            .iter()
            .map(|a| match a {
                Arg::Num(n) if *n < 0x10000 => n.to_string(),
                Arg::Num(n) if (*n as i64) < 0 && (*n as i64) > -0x10000 => (*n as i64).to_string(),
                Arg::Num(n) => format!("{n:#x}"),
                Arg::Text(t) => format!("{:?}", String::from_utf8_lossy(t)),
            })
            .collect();
        format!("{f}({})", a.join(", "))
    }

    /// Run `target` in thread `tid` with `args` and wait up to `wait` for it to return.
    pub(crate) fn call_now(
        &mut self,
        tid: Tid,
        target: u64,
        args: &[Arg],
        text: String,
        wait: Duration,
    ) -> Result<Called> {
        self.need_stopped()?;
        if let Some(p) = &self.calling {
            return fail(format!(
                "`{}` is under way in thread {}: `cont` runs it to its end, `kill call` ends it",
                p.text, p.tid
            ));
        }
        if self.bits() == 32 {
            return fail(format!("`{text}` was not run: this debugger calls functions in 64-bit programs only, and this program is 32-bit"));
        }
        let maps = self.t.maps()?;
        let exec = |a: u64| maps.iter().any(|m| m.exec && m.start <= a && a < m.end);
        if !exec(target) {
            return fail(format!(
                "{target:#x} is not in executable memory, so `{text}` has nothing to run (`maps` lists what is)"
            ));
        }
        let Some(ret) = self.t.entry().filter(|a| exec(*a)) else {
            return fail("the program's entry is not known, and a call returns to it");
        };
        let saved = self.t.regs(tid)?;
        // the stack the call is built on is written outside the record
        self.tape.cut();
        let (in_regs, shadow): (&[&str], u64) = if self.t.os() == Os::Windows {
            (&["rcx", "rdx", "r8", "r9"], 32)
        } else {
            (&["rdi", "rsi", "rdx", "rcx", "r8", "r9"], 0)
        };
        let mut sp = saved.rsp().wrapping_sub(BELOW_SP) & !0xf;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Arg::Num(n) => vals.push(*n),
                Arg::Text(t) => {
                    sp = sp.wrapping_sub(t.len() as u64 + 1) & !0xf;
                    let mut b = t.clone();
                    b.push(0);
                    self.t.write(sp, &b)?;
                    vals.push(sp);
                }
            }
        }
        let on_stack: Vec<u64> = vals.iter().skip(in_regs.len()).copied().collect();
        // at the function's first instruction rsp + 8 is a multiple of 16, in both ABIs
        let base = sp.wrapping_sub(shadow + 8 * on_stack.len() as u64) & !0xf;
        for (i, v) in on_stack.iter().enumerate() {
            self.t.write(base + shadow + 8 * i as u64, &v.to_le_bytes())?;
        }
        let rsp = base - 8;
        self.t.write(rsp, &ret.to_le_bytes())?;
        let mut r = saved;
        for (name, v) in in_regs.iter().zip(&vals) {
            r.set(name, *v);
        }
        // System V: al is how many vector registers a variadic function gets, none here
        r.set("rax", 0);
        r.set("rsp", rsp);
        r.rip = target;
        // both ABIs want the direction flag clear at a call; no trap flag either
        r.eflags &= !0x500;
        // Linux: a system call the thread stopped in is not restarted at the function's first instruction
        r.orig_rax = u64::MAX;
        let vector = self.t.vector_state(tid);
        let signal = self.t.take_signal(tid);
        if let Err(e) = self.t.set_regs(tid, &r) {
            self.t.give_signal(tid, signal);
            return Err(e);
        }
        self.calling = Some(Pending { tid, text, saved, vector, signal, ret, sp: rsp + 8, target, fresh: true });
        let started = self.begin(Run::default());
        if let Some(p) = &mut self.calling {
            p.fresh = false;
        }
        if let Err(e) = started {
            let _ = self.call_restore();
            return Err(e);
        }
        let deadline = Instant::now() + wait;
        loop {
            let slice = deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(200));
            if let Some((tid, why)) = self.poll(slice)? {
                return Ok(self.call_stop(tid, why));
            }
            if Instant::now() >= deadline {
                return Ok(Called::Told(self.call_running(wait)));
            }
        }
    }

    /// The call under way, as its answers name it.
    pub(crate) fn call_under_way(&self) -> Option<String> {
        self.calling.as_ref().map(|p| p.text.clone())
    }

    /// Thread `tid` is about to run the first instruction of the call just made, at `pc`.
    pub(super) fn call_starts_at(&self, tid: Tid, pc: u64) -> bool {
        self.calling.as_ref().is_some_and(|p| p.fresh && p.tid == tid && p.target == pc)
    }

    /// The thread of the call is back at its return address with the stack where the call left it.
    pub(super) fn call_returned(&mut self, tid: Tid, addr: u64) -> Result<bool> {
        let Some(p) = &self.calling else { return Ok(false) };
        let (ret, me, sp) = (p.ret, p.tid, p.sp);
        Ok(ret == addr && me == tid && self.t.regs(tid)?.rsp() >= sp)
    }

    /// A stop while a call is under way: its value when it returned (the registers are back), or the answer for any
    /// other stop; a fault in the call's thread ends the call and puts the registers back.
    pub(crate) fn call_stop(&mut self, tid: Tid, why: Why) -> Called {
        let Some(p) = &self.calling else { return Called::Told(self.stopped(tid, why)) };
        let (me, text) = (p.tid, p.text.clone());
        match why {
            Why::Returned => {
                let v = self.t.regs(tid).map(|r| r.gp[0]);
                let back = self.call_restore();
                match (v, back) {
                    (Ok(v), Ok(())) => Called::Value(v),
                    (Err(e), _) | (_, Err(e)) => Called::Told(rows(
                        "call",
                        vec![format!(
                            "`{text}` returned, but its thread's registers could not be read or put back: {e}"
                        )],
                        json!({"state": "stopped", "thread": tid}),
                    )),
                }
            }
            Why::Signal(f) if tid == me => {
                self.nstop += 1;
                let mut l = vec![format!("`{text}` faulted in thread {tid}: {}", fault_line(self.t.os(), &f))];
                if let Ok(r) = self.t.regs(tid) {
                    let name = self.mods.label(r.rip, false).unwrap_or_default();
                    let at = self.mods.source(r.rip).map(|s| format!("  ({s})")).unwrap_or_default();
                    l.push(format!("at pc {:#x} {name}{at}", r.rip));
                }
                if let Ok(f) = self.frames(6) {
                    l.push("backtrace in the call:".into());
                    let fl = self.frame_lines(&f);
                    l.extend(fl.into_iter().map(|s| format!("  {s}")));
                }
                // the fault is the call's, not the program's: the thread does not get it
                let _ = self.t.take_signal(tid);
                match self.call_restore() {
                    Ok(()) => l.push(format!("the fault was not handed to the program; {}", self.back_line(tid))),
                    Err(e) => l.push(format!("the registers of thread {tid} could not be put back: {e}")),
                }
                if let Some(o) = self.new_output_line() {
                    l.push(o);
                }
                Called::Told(rows("call", l, json!({"state": "stopped", "thread": tid, "call": "faulted"})))
            }
            other => Called::Told(self.stopped(tid, other)),
        }
    }

    /// Put the thread of the call back as it was before it: registers, vector registers, its pending signal.
    fn call_restore(&mut self) -> Result<()> {
        let Some(p) = self.calling.take() else { return Ok(()) };
        self.t.set_regs(p.tid, &p.saved)?;
        if let Some(v) = &p.vector {
            self.t.set_vector_state(p.tid, v)?;
        }
        self.t.give_signal(p.tid, p.signal);
        // the answer's header names where the thread is now, not a stop inside the call
        let place = self.mods.label(p.saved.rip, false).unwrap_or_else(|| "?".into());
        self.last_stop = Some(StopInfo { n: self.nstop, event: "call".into(), place, addr: p.saved.rip });
        self.prev.insert(p.tid, p.saved);
        self.cur = p.tid;
        self.frame = 0;
        Ok(())
    }

    /// Where thread `tid` is now that the call is over.
    pub(crate) fn back_line(&mut self, tid: Tid) -> String {
        let pc = self.t.regs(tid).map(|r| r.rip).unwrap_or_default();
        let name = self.mods.label(pc, false).unwrap_or_default();
        format!("thread {tid} is back as before the call: pc {pc:#x} {name}, every register as it was")
    }

    /// The answer when the call still runs after `wait`.
    fn call_running(&mut self, wait: Duration) -> Output {
        let text = self.calling.as_ref().map(|p| p.text.clone()).unwrap_or_default();
        let mut l = vec![format!(
            "`{text}` still runs after {:.0}s: `wait N` waits for it to return (its value comes with that stop), `stop` makes it stop, and then `kill call` ends it with the registers put back",
            wait.as_secs_f64()
        )];
        l.extend(self.new_output_line());
        rows("running", l, json!({"state": "running", "call": text}))
    }

    /// The line a stop answer carries while a call is under way.
    pub(crate) fn call_note(&self) -> Option<String> {
        let p = self.calling.as_ref()?;
        Some(format!(
            "stopped inside `{}` (thread {}): `cont`, or `finish` in the function it called, runs it to its end and gives its value; `kill call` ends it and puts the registers of thread {} back as before the call",
            p.text, p.tid, p.tid
        ))
    }

    /// Where a backtrace reaches the return address of the call under way: the index of that frame and the line
    /// that stands for it and for what lies below.
    pub(crate) fn call_cut(&mut self, f: &[crate::modules::Frame]) -> Option<(usize, String)> {
        let p = self.calling.as_ref()?;
        let i = f.iter().skip(1).position(|fr| fr.pc == p.ret)? + 1;
        let (text, ret, pc) = (p.text.clone(), p.ret, p.saved.rip);
        let name = self.mods.label(pc, false).unwrap_or_default();
        Some((
            i,
            format!("#{i}  {ret:#x}  return address of `{text}`, called by the debugger; before it the thread was at pc {pc:#x} {name}"),
        ))
    }

    /// The process goes on without the debugger in the middle of the call (it forked inside it under `follow
    /// child`): its thread is put back as before the call first. The line that says so.
    pub(crate) fn call_let_go(&mut self) -> Option<String> {
        let (tid, text) = self.calling.as_ref().map(|p| (p.tid, p.text.clone()))?;
        Some(match self.call_restore() {
            Ok(()) => format!("`{text}` did not return: the fork was inside it; {}", self.back_line(tid)),
            Err(e) => format!(
                "`{text}` did not return: the fork was inside it, and the registers of thread {tid} could not be put back: {e}"
            ),
        })
    }

    /// The program ended while a call was under way: the call is gone with it.
    pub(crate) fn call_gone(&mut self) -> Option<String> {
        let p = self.calling.take()?;
        Some(format!("`{}` did not return: the program ended during it", p.text))
    }

    /// The answer when a call that stopped inside returns at a later `cont`, `finish` or `wait`.
    pub(crate) fn call_value(&mut self, text: &str, tid: Tid, v: u64) -> Output {
        let mut l = vec![format!("`{text}` returned {}", self.value_text(v)), self.back_line(tid)];
        l.extend(self.new_output_line());
        rows("call", l, json!({"state": "stopped", "thread": tid, "value": v}))
    }

    /// A value a call returned: hex, signed, the 32-bit int it may be, and the name of the place it points at.
    pub(crate) fn value_text(&mut self, v: u64) -> String {
        let mut s = format!("{v:#x} ({})", v as i64);
        if v >> 32 == 0 && v & 0x8000_0000 != 0 {
            s.push_str(&format!(", {} as a 32-bit int", v as u32 as i32));
        }
        if let Some(l) = self.mods.label(v, true) {
            s.push_str(&format!(" -> {l}"));
        }
        s
    }

    /// `kill call`: end the call under way without its value.
    pub(crate) fn call_abort(&mut self) -> Result<Output> {
        let Some(p) = &self.calling else { return fail("no call is under way: `kill` alone ends the program") };
        let (text, tid) = (p.text.clone(), p.tid);
        if self.state == State::Running {
            return fail(format!("`{text}` is running: `stop` makes it stop, then `kill call` ends it"));
        }
        self.call_restore()?;
        Ok(rows(
            "kill",
            vec![format!("`{text}` ended without a value; {}", self.back_line(tid))],
            json!({"state": "stopped", "thread": tid}),
        ))
    }
}
