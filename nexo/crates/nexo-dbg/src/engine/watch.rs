//! What a watch sees while the program runs. A range that fits the four debug registers is watched by the
//! processor; a bigger one, or one past the four, by taking write access (or all access) from its pages and
//! looking at each fault in them; when pages cannot be guarded, by running one thread an instruction at a time
//! and comparing the bytes after each.

use super::*;
use crate::platform::{Access, PAGE, PROT_R, PROT_W, PROT_X};
use nexo_bin::{decode_live_in, LiveIns};

/// How a watch learns of an access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchHow {
    /// Debug registers: the processor stops the thread right after the access, and the program runs at full
    /// speed.
    Hardware,
    /// The pages that hold the range lose write access (all access for a read watch); a fault in them is let
    /// through one instruction at a time and checked.
    Pages,
    /// One thread runs one instruction at a time and the bytes are compared after each.
    Steps,
}

impl WatchHow {
    pub fn word(self) -> &'static str {
        match self {
            WatchHow::Hardware => "hw",
            WatchHow::Pages => "page",
            WatchHow::Steps => "step",
        }
    }
}

/// A run of pages a page watch guards: where it is, the access it has and the access it gets while the program
/// runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guard {
    pub start: u64,
    pub len: u64,
    pub orig: u8,
    pub guard: u8,
    pub armed: bool,
}

/// `addr..addr+len` as aligned pieces of 8, 4, 2 or 1 bytes, the sizes a debug register watches; `None` when it
/// takes more than `max` of them.
pub fn hw_pieces(addr: u64, len: u64, max: usize) -> Option<Vec<(u64, u8)>> {
    let mut v = Vec::new();
    let end = addr.checked_add(len)?;
    let mut a = addr;
    while a < end {
        if v.len() == max {
            return None;
        }
        let n = [8u64, 4, 2, 1].into_iter().find(|n| a.is_multiple_of(*n) && a + n <= end).unwrap_or(1);
        v.push((a, n as u8));
        a += n;
    }
    Some(v)
}

/// The value of up to 8 little-endian bytes.
fn le(b: &[u8]) -> u64 {
    let mut x = [0u8; 8];
    let n = b.len().min(8);
    x[..n].copy_from_slice(&b[..n]);
    u64::from_le_bytes(x)
}

/// Bytes as the hex of the little-endian number they make, so `00000007` reads as the 7 it is.
fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "?".into();
    }
    b.iter().rev().map(|x| format!("{x:02x}")).collect()
}

fn val(v: u64) -> expr::Val {
    expr::Val { v, ty: expr::Ty { bytes: 8, signed: false, stars: 0, ch: false } }
}

/// The offsets of a watch's bytes an access is about: the bytes the instruction touched when that is known,
/// else the bytes that changed (at most 8, from the first); a watch of 8 bytes or fewer is about all of them.
fn piece(old: &[u8], now: &[u8], touched: Option<(usize, usize)>) -> Option<(usize, usize)> {
    let len = now.len();
    if len <= 8 {
        return Some((0, len));
    }
    if let Some((a, b)) = touched {
        return Some((a, b.min(a + 8)));
    }
    let first = (0..len).find(|&i| old.get(i) != now.get(i))?;
    let last = (first..len).rev().find(|&i| old.get(i) != now.get(i)).unwrap_or(first);
    Some((first, (last + 1).min(first + 8)))
}

impl Session {
    /// The pages the page watches guard, in runs of pages with the same access, each with the access it gets.
    pub(crate) fn guard_plan(&mut self) -> Result<Vec<Guard>> {
        // page -> a read watch covers it (the page then loses all access, not only writes)
        let mut want: BTreeMap<u64, bool> = BTreeMap::new();
        for w in self.wps.iter().filter(|w| w.how == WatchHow::Pages) {
            let mut p = w.addr & !(PAGE - 1);
            while p < w.addr.saturating_add(w.len) {
                *want.entry(p).or_default() |= w.access == Access::ReadWrite;
                p += PAGE;
            }
        }
        if want.is_empty() {
            return Ok(Vec::new());
        }
        let maps = self.t.maps()?;
        let mut out: Vec<Guard> = Vec::new();
        for (p, rw) in want {
            let Some(m) = maps.iter().find(|m| m.start <= p && p < m.end) else {
                return fail(format!("{p:#x} is not mapped in the program: there is no page to guard"));
            };
            let orig = (u8::from(m.read) * PROT_R) | (u8::from(m.write) * PROT_W) | (u8::from(m.exec) * PROT_X);
            let guard = if rw { 0 } else { orig & !PROT_W };
            // a page that cannot be written already faults on a write by itself
            if guard == orig {
                continue;
            }
            match out.last_mut() {
                Some(g) if g.start + g.len == p && g.orig == orig && g.guard == guard => g.len += PAGE,
                _ => out.push(Guard { start: p, len: PAGE, orig, guard, armed: false }),
            }
        }
        Ok(out)
    }

    /// Give every guarded page its access back and forget the plan, so the next run works it out from the watches
    /// as they are then. A plan with pages still guarded (the program runs) is kept until it stops.
    pub(crate) fn drop_guards(&mut self) {
        self.unguard(None);
        if self.guards.iter().all(|g| !g.armed) {
            self.guards.clear();
        }
    }

    /// Guard the pages of the page watches that are open, before the program runs on.
    pub(crate) fn guard_pages(&mut self) -> Result<()> {
        if self.guards.is_empty() {
            self.guards = self.guard_plan()?;
        }
        for i in 0..self.guards.len() {
            let g = self.guards[i];
            if !g.armed {
                self.t.protect(g.start, g.len, g.guard)?;
                self.guards[i].armed = true;
            }
        }
        Ok(())
    }

    /// Give guarded pages their own access back: all of them, or the run that holds `only`.
    pub(crate) fn unguard(&mut self, only: Option<u64>) {
        for i in 0..self.guards.len() {
            let g = self.guards[i];
            if !g.armed || only.is_some_and(|a| a < g.start || a >= g.start + g.len) {
                continue;
            }
            if self.t.protect(g.start, g.len, g.orig).is_ok() {
                self.guards[i].armed = false;
            }
        }
    }

    /// A fault the guard of a page watch caused (and not the program's own).
    pub(crate) fn guard_fault(&self, f: &Fault) -> bool {
        let Some(a) = f.addr else { return false };
        // SIGBUS is 7 on Linux and 10 on macOS, SIGSEGV 11 on both, ACCESS_VIOLATION on Windows
        matches!(f.number, 7 | 10 | 11 | 0xC000_0005)
            && self.guards.iter().any(|g| g.armed && a >= g.start && a < g.start + g.len)
    }

    /// The thread a step watch runs one instruction at a time: the current one while it lives.
    pub(crate) fn step_watch_thread(&self) -> Option<Tid> {
        if !self.wps.iter().any(|w| w.how == WatchHow::Steps) {
            return None;
        }
        let threads = self.t.threads();
        if threads.contains(&self.cur) {
            Some(self.cur)
        } else {
            threads.first().copied()
        }
    }

    /// The instruction at `pc` with its address, name and text, for the line of a hit.
    fn ins_at(&mut self, pc: u64, ins: &LiveIns) -> String {
        match self.mods.label(pc, false) {
            Some(n) if !n.is_empty() => format!("{pc:#x} {n}: {}", ins.text),
            _ => format!("{pc:#x}: {}", ins.text),
        }
    }

    /// The instruction that ends where the thread stands: the processor stops a data watch right after the access,
    /// so that one made it. Found by decoding the function from its start; `None` when there is no start.
    fn writer_before(&mut self, rip: u64) -> Option<String> {
        let bits = self.bits();
        let start = self.mods.symbol_start(rip).filter(|s| *s < rip && rip - s <= 0x10000)?;
        let code = self.read_vec(start, (rip - start) as usize);
        let mut at = 0usize;
        while at < code.len() {
            let ins = decode_live_in(bits, &code[at..], start + at as u64, &|_| None)?;
            if at + ins.len == code.len() {
                return Some(self.ins_at(start + at as u64, &ins));
            }
            at += ins.len.max(1);
        }
        None
    }

    /// Watches of `how` saw thread `tid` stop after an instruction: decide for each one, as `break` does for a
    /// breakpoint, whether it stops. `hit` tells a watch the access was its own whether or not the bytes changed
    /// (a debug register, an instruction that touched its range); the rest are hit only by a change.
    fn watch_round(&mut self, tid: Tid, ids: &[u32], writer: Option<String>, touched: &[(u64, u64, bool)]) -> Vec<u32> {
        let mut stops = Vec::new();
        for &id in ids {
            let Some(i) = self.wps.iter().position(|w| w.id == id) else { continue };
            let (addr, len, access, how) = (self.wps[i].addr, self.wps[i].len, self.wps[i].access, self.wps[i].how);
            let now = self.read_vec(addr, len as usize);
            let old = std::mem::replace(&mut self.wps[i].last, now.clone());
            let changed = old != now;
            // the part of the range the instruction touched, as offsets into it
            let mine = touched.iter().find_map(|&(a, n, write)| {
                let (lo, hi) = (a.max(addr), a.saturating_add(n.max(1)).min(addr + len));
                (lo < hi && (write || access == Access::ReadWrite))
                    .then(|| ((lo - addr) as usize, (hi - addr) as usize))
            });
            let hit = changed || mine.is_some() || how == WatchHow::Hardware;
            if !hit {
                continue;
            }
            let line = if access == Access::Exec {
                format!("reached {addr:#x}")
            } else {
                let part = piece(&old, &now, mine.filter(|_| len > 8));
                let (a, b) = part.unwrap_or((0, (len as usize).min(8)));
                let (o, n) = (old.get(a..b).unwrap_or(&[]), now.get(a..b).unwrap_or(&[]));
                self.conv.insert("old".into(), val(le(o)));
                self.conv.insert("new".into(), val(le(n)));
                self.conv.insert("at".into(), val(a as u64));
                let place = if len <= 8 { format!("{addr:#x}") } else { format!("{addr:#x}+{len} bytes {a}..{b}") };
                let mut l = if o == n {
                    format!(
                        "{place}: {} (same value), {}",
                        hex(n),
                        if changed { "changed elsewhere" } else { "touched" }
                    )
                } else {
                    format!("{place}: {} -> {} ({} -> {})", hex(o), hex(n), le(o), le(n))
                };
                let all = old.iter().zip(&now).filter(|(x, y)| x != y).count();
                if len > 8 && all > b - a {
                    l.push_str(&format!(", {all} bytes changed in all"));
                }
                l
            };
            let line = match &writer {
                Some(w) => format!("{line}; by {w}"),
                None => line,
            };
            if let Some(c) = self.wps[i].cond.clone() {
                let truth = match self.ev(tid) {
                    Ok(mut ev) => expr::value(&c, &mut ev),
                    Err(e) => Err(e),
                };
                match truth {
                    Ok(0) => continue,
                    Ok(_) => {}
                    // a condition that cannot be worked out stops, and says so
                    Err(e) => self.log_line(format!("watch #{id}: condition {c:?} failed: {e}")),
                }
            }
            self.wps[i].hits += 1;
            if self.wps[i].hits <= self.wps[i].after {
                continue;
            }
            if let Some(text) = self.wps[i].log.clone() {
                let l = if text.is_empty() { line } else { self.format_log(tid, &text) };
                self.log_line(format!("#{id} {l}"));
                continue;
            }
            self.watch_lines.push(format!("watch #{id} {} {line}", how.word()));
            stops.push(id);
        }
        stops
    }

    /// The watch side of a batch of events: debug register hits, faults in guarded pages and the steps of a step
    /// watch. Returns the stop they make, if any, and the threads whose fault they took.
    pub(crate) fn watch_events(&mut self, evs: Vec<(Tid, Stop)>) -> Result<Option<(Tid, Why)>> {
        let mut verdict = None;
        for (tid, stop) in evs {
            let mut stops = Vec::new();
            if self.step_from.is_some_and(|(t, _)| t == tid) && matches!(stop, Stop::Step | Stop::Watch(_)) {
                let (_, pc) = self.step_from.take().unwrap_or_default();
                let ids: Vec<u32> = self
                    .wps
                    .iter()
                    .filter(|w| w.how == WatchHow::Steps)
                    .map(|w| (w.id, w.addr, w.len, w.last.clone()))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .filter(|(_, a, n, last)| self.read_vec(*a, *n as usize) != *last)
                    .map(|(id, ..)| id)
                    .collect();
                if !ids.is_empty() {
                    let bytes = self.read_vec(pc, 16);
                    let writer = decode_live_in(self.bits(), &bytes, pc, &|_| None).map(|i| self.ins_at(pc, &i));
                    stops.extend(self.watch_round(tid, &ids, writer, &[]));
                }
            }
            match stop {
                Stop::Watch(bits) => {
                    let ids: Vec<u32> = self
                        .wps
                        .iter()
                        .filter(|w| w.how == WatchHow::Hardware && w.slots.iter().any(|(s, _)| bits >> s & 1 == 1))
                        .map(|w| w.id)
                        .collect();
                    if !ids.is_empty() {
                        let rip = self.t.regs(tid)?.rip;
                        let exec =
                            ids.iter().all(|id| self.wps.iter().any(|w| w.id == *id && w.access == Access::Exec));
                        let writer = if exec { None } else { self.writer_before(rip) };
                        stops.extend(self.watch_round(tid, &ids, writer, &[]));
                    }
                }
                Stop::Fault(f) => {
                    if let Some(v) = self.page_hit(tid, &f, &mut stops)? {
                        verdict = verdict.or(Some(v));
                    }
                }
                _ => {}
            }
            if !stops.is_empty() && verdict.is_none() {
                verdict = Some((tid, Why::Watch(stops)));
            }
        }
        Ok(verdict)
    }

    /// Thread `tid` touched a guarded page: open that run of pages, let the one instruction through, look at what
    /// it did. Another guarded run it touches is opened the same way; a fault where the program has no access is
    /// its own, and is the answer when the fault is one that stops.
    fn page_hit(&mut self, tid: Tid, f: &Fault, stops: &mut Vec<u32>) -> Result<Option<(Tid, Why)>> {
        let r = self.t.regs(tid)?;
        let pc = r.rip;
        let bytes = self.read_vec(pc, 16);
        let ins = decode_live_in(self.bits(), &bytes, pc, &|n| r.get(n));
        let mut fault = *f;
        let mut done = false;
        for _ in 0..4 {
            self.t.take_signal(tid);
            self.unguard(fault.addr);
            // a breakpoint on this very instruction would stop the step: the program's byte goes in for it
            let bp = self.inserted.get(&pc).copied();
            if let Some(b) = bp {
                self.t.write(pc, &[b])?;
            }
            self.t.resume(Resume::Step(tid))?;
            let evs = self.t.wait(Duration::from_secs(5))?;
            if evs.is_empty() {
                return fail(format!(
                    "thread {tid} did not finish the instruction at {pc:#x} that touched a watched page"
                ));
            }
            let ended = evs.iter().any(|e| matches!(e, Event::Exited { .. } | Event::Killed { .. }));
            if bp.is_some() && !ended {
                let _ = self.t.write(pc, &[0xcc]);
            }
            let mut again = None;
            for e in evs {
                match e {
                    Event::Stopped { tid: t, stop: Stop::Step } if t == tid => done = true,
                    Event::Stopped { tid: t, stop: Stop::Watch(b) } if t == tid => {
                        done = true;
                        self.backlog.push(Event::Stopped { tid, stop: Stop::Watch(b) });
                    }
                    Event::Stopped { tid: t, stop: Stop::Fault(g) } if t == tid && self.guard_fault(&g) => {
                        again = Some(g)
                    }
                    Event::Stopped { tid: t, stop: Stop::Fault(g) } if t == tid && g.addr.is_some() => {
                        if self.catch.contains(&g.number) || (self.t.os() == Os::Windows && !g.first_chance) {
                            return Ok(Some((tid, Why::Signal(g))));
                        }
                        // the program's own handler gets it when the thread runs on
                    }
                    other => self.backlog.push(other),
                }
            }
            match again {
                Some(g) => fault = g,
                None => break,
            }
        }
        if !done {
            return Ok(None);
        }
        let touched: Vec<(u64, u64, bool)> = match &ins {
            Some(i) if i.mem.iter().any(|m| m.addr.is_some()) => {
                i.mem.iter().filter_map(|m| Some((m.addr?, m.size as u64, m.write))).collect()
            }
            // a write guard lets reads through, so its fault was a write
            _ => vec![(f.addr.unwrap_or(0), 1, true)],
        };
        let ids: Vec<u32> = self.wps.iter().filter(|w| w.how == WatchHow::Pages).map(|w| w.id).collect();
        let writer = ins.as_ref().map(|i| self.ins_at(pc, i));
        stops.extend(self.watch_round(tid, &ids, writer, &touched));
        Ok(None)
    }
}
