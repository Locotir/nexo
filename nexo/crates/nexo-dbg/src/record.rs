//! A bounded record of what ran, so "how did I get here" can be asked without a restart. One entry per single
//! step: the pc it started at, the registers it changed and the memory it wrote. It only looks back; it never
//! undoes anything. The ring is bounded (the last N instructions) and counts what it let go.
//!
//! This file holds the record and its questions, with no process in it, so the filters can be tested on a
//! hand-built tape. The commands and the hooks into the run are in `cmds/record.rs`.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::platform::Tid;
use crate::regs::Regs;

/// Instructions kept when `record` has no number.
pub const DEFAULT_KEEP: usize = 10_000;
/// The most `record N` takes: a bigger ring is memory the program's own run would need.
pub const MAX_KEEP: usize = 1_000_000;
/// The longest memory write an entry keeps, in bytes (a `rep movs` or a vector store past it is left out).
pub const MAX_WRITE: usize = 64;
/// A `cont` that fills the ring this many times stops recording: single-stepping has no end otherwise.
pub const WRAPS_PER_RUN: u64 = 10;

/// Bytes at one place, before and after the instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemWrite {
    pub addr: u64,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
}

impl MemWrite {
    /// The write touched `a`.
    pub fn covers(&self, a: u64) -> bool {
        a >= self.addr && a - self.addr < self.before.len() as u64
    }
}

/// One instruction that ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its number since `record` started (1 is the first), so a gap in the numbers shows what was dropped.
    pub n: u64,
    pub tid: Tid,
    /// Where it was.
    pub pc: u64,
    /// The registers it changed (name, old, new); the pc is not among them.
    pub regs: Vec<(&'static str, u64, u64)>,
    pub mem: Vec<MemWrite>,
    /// Where the thread was after it (the pc of the next instruction), so a chain of entries can be checked.
    pub next: u64,
    /// It may have written memory the entry does not hold (a write over `MAX_WRITE` bytes, an address that could not
    /// be worked out, code that could not be decoded): it cannot be undone.
    pub lossy: bool,
}

impl Entry {
    /// It changed `name` (the pc counts for every instruction: each one moves it).
    pub fn changed(&self, name: &str) -> bool {
        matches!(name, "rip" | "eip") || self.regs.iter().any(|(n, _, _)| *n == name)
    }
}

#[derive(Default)]
pub struct Tape {
    pub on: bool,
    cap: usize,
    ring: VecDeque<Entry>,
    /// Entries let go because the ring was full.
    pub dropped: u64,
    total: u64,
    /// Entries made since the last `cont` began.
    run_kept: u64,
    /// The run under way single-steps because of the record (a `cont`, `finish`, `until`).
    pub slow: bool,
    /// What the next answer must say: the record ended by itself.
    pub note: Option<String>,
    /// Calls that arrived somewhere other than one step from the last place seen.
    pub gaps: u64,
    last: HashMap<Tid, Regs>,
    pending: HashMap<Tid, Vec<(u64, Vec<u8>)>>,
    /// Threads whose next instruction may write more than `pending` holds.
    lossy: HashSet<Tid>,
}

impl Tape {
    /// Begin (or begin again): an empty ring of `keep` entries.
    pub fn start(&mut self, keep: usize) {
        *self = Tape { on: true, cap: keep.clamp(1, MAX_KEEP), ..Tape::default() };
    }

    /// End the record. The entries stay: `back` still reads them.
    pub fn stop(&mut self) {
        self.on = false;
        self.slow = false;
        self.last.clear();
        self.pending.clear();
        self.lossy.clear();
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn kept(&self) -> usize {
        self.ring.len()
    }

    /// A new run begins: the wrap count starts again.
    pub fn begin_run(&mut self, slow: bool) {
        self.run_kept = 0;
        self.slow = slow;
    }

    /// This run filled the ring [`WRAPS_PER_RUN`] times.
    pub fn over_budget(&self) -> bool {
        self.on && self.run_kept >= self.cap as u64 * WRAPS_PER_RUN
    }

    /// End the record because the run went on too long.
    pub fn give_up(&mut self) {
        self.note = Some(format!(
            "recording stopped: this run filled the ring ({} instructions) {WRAPS_PER_RUN} times; the last {} are kept for `back`, `record` starts again",
            self.cap,
            self.ring.len()
        ));
        self.stop();
    }

    /// What an answer after a run says about the record, once.
    pub fn take_notes(&mut self) -> Vec<String> {
        let mut l = Vec::new();
        if std::mem::take(&mut self.slow) {
            l.push("recording: this run went one instruction at a time (slow); `back N` reads what it did".to_string());
        }
        l.extend(self.note.take());
        l
    }

    /// The line for `status`, when there is anything to say.
    pub fn status_line(&self) -> Option<String> {
        if !self.on && self.ring.is_empty() {
            return None;
        }
        Some(format!(
            "recording {}: {} entries kept, {} dropped (the ring holds {})",
            if self.on { "on" } else { "off" },
            self.ring.len(),
            self.dropped,
            self.cap
        ))
    }

    pub fn push(&mut self, mut e: Entry) {
        self.total += 1;
        self.run_kept += 1;
        e.n = self.total;
        self.ring.push_back(e);
        while self.ring.len() > self.cap {
            self.ring.pop_front();
            self.dropped += 1;
        }
    }

    pub fn has_pending(&self, tid: Tid) -> bool {
        self.pending.contains_key(&tid)
    }

    /// The memory the instruction at the thread's pc is about to write, as it is now. An empty list is not kept.
    pub fn set_pending(&mut self, tid: Tid, w: Vec<(u64, Vec<u8>)>) {
        if w.is_empty() {
            self.pending.remove(&tid);
        } else {
            self.pending.insert(tid, w);
        }
    }

    /// Whether the instruction at the thread's pc may write more than the pending list holds.
    pub fn set_lossy(&mut self, tid: Tid, lossy: bool) {
        if lossy {
            self.lossy.insert(tid);
        } else {
            self.lossy.remove(&tid);
        }
    }

    /// The whole record, newest first.
    pub fn newest_first(&self) -> Vec<&Entry> {
        self.ring.iter().rev().collect()
    }

    /// The newest `k` entries were undone and the thread is at `to`: they are let go (so a step forward records
    /// them again), and the place the next step starts from is `to`.
    pub fn rewind(&mut self, tid: Tid, k: usize, to: &Regs) {
        for _ in 0..k.min(self.ring.len()) {
            self.ring.pop_back();
            self.total = self.total.saturating_sub(1);
        }
        self.run_kept = self.run_kept.saturating_sub(k as u64);
        self.pending.remove(&tid);
        self.lossy.remove(&tid);
        self.last.insert(tid, *to);
    }

    /// The thread is at `now`. `stepped`: it got here by one single step (from `from` when that is known), so
    /// one entry is made; otherwise it is only noted as the new place (and counted as a gap when it moved).
    /// `read` gives the bytes at an address, for the "after" of the memory the step wrote.
    pub fn observe(
        &mut self,
        tid: Tid,
        now: &Regs,
        stepped: bool,
        from: Option<u64>,
        read: &mut dyn FnMut(u64, usize) -> Vec<u8>,
    ) {
        let pend = self.pending.remove(&tid).unwrap_or_default();
        let lossy = self.lossy.remove(&tid);
        match self.last.get(&tid).copied() {
            Some(p) if stepped && from.is_none_or(|f| f == p.rip) => {
                let mem = pend
                    .into_iter()
                    .filter_map(|(addr, before)| {
                        let after = read(addr, before.len());
                        (after.len() == before.len()).then_some(MemWrite { addr, before, after })
                    })
                    .collect();
                let regs = now.changed(&p);
                self.push(Entry { n: 0, tid, pc: p.rip, regs, mem, next: now.rip, lossy });
            }
            // the same place seen again (a stop after the steps that got there): nothing ran in between
            Some(p) if !stepped && p.rip == now.rip => {
                if !pend.is_empty() {
                    self.pending.insert(tid, pend);
                }
                if lossy {
                    self.lossy.insert(tid);
                }
            }
            Some(_) => self.gaps += 1,
            None => {}
        }
        self.last.insert(tid, *now);
    }

    /// The last `n` entries, newest first.
    pub fn back(&self, n: usize) -> Vec<&Entry> {
        self.ring.iter().rev().take(n).collect()
    }

    /// The last `n` entries that changed `reg`, newest first.
    pub fn by_reg(&self, reg: &str, n: usize) -> Vec<&Entry> {
        self.ring.iter().rev().filter(|e| e.changed(reg)).take(n).collect()
    }

    /// The last `n` entries that wrote a byte of `addr`, newest first.
    pub fn by_mem(&self, addr: u64, n: usize) -> Vec<&Entry> {
        self.ring.iter().rev().filter(|e| e.mem.iter().any(|m| m.covers(addr))).take(n).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pc: u64, regs: &[(&'static str, u64, u64)], mem: &[(u64, u8, u8)]) -> Entry {
        Entry {
            n: 0,
            tid: 1,
            pc,
            regs: regs.to_vec(),
            next: pc + 4,
            lossy: false,
            mem: mem.iter().map(|(a, b, n)| MemWrite { addr: *a, before: vec![*b], after: vec![*n] }).collect(),
        }
    }

    fn tape(keep: usize, count: u64) -> Tape {
        let mut t = Tape::default();
        t.start(keep);
        for i in 0..count {
            t.push(entry(0x1000 + i, &[], &[]));
        }
        t
    }

    #[test]
    fn the_ring_keeps_the_last_n_and_counts_what_it_let_go() {
        let t = tape(5, 8);
        assert_eq!((t.kept(), t.dropped), (5, 3));
        // numbers go on across the drop: the oldest kept is 4
        let all = t.back(10);
        assert_eq!(all.len(), 5);
        assert_eq!((all[0].n, all[4].n), (8, 4));
        assert_eq!(all[0].pc, 0x1007);
    }

    #[test]
    fn the_size_is_bounded_at_both_ends() {
        let mut t = Tape::default();
        t.start(0);
        assert_eq!(t.cap(), 1);
        t.start(usize::MAX);
        assert_eq!(t.cap(), MAX_KEEP);
        t.start(DEFAULT_KEEP);
        assert_eq!(t.cap(), 10_000);
    }

    #[test]
    fn a_run_that_fills_the_ring_ten_times_gives_up_and_keeps_the_ring() {
        let mut t = Tape::default();
        t.start(4);
        t.begin_run(true);
        for i in 0..39 {
            t.push(entry(i, &[], &[]));
        }
        assert!(!t.over_budget());
        t.push(entry(39, &[], &[]));
        assert!(t.over_budget());
        t.give_up();
        assert!(!t.on);
        assert_eq!(t.kept(), 4);
        let notes = t.take_notes();
        assert!(notes.iter().any(|n| n.contains("recording stopped")), "{notes:?}");
        // the note is said once
        assert!(t.take_notes().is_empty());
    }

    #[test]
    fn back_reg_gives_the_instructions_that_changed_it_newest_first() {
        let mut t = Tape::default();
        t.start(100);
        t.push(entry(0x10, &[("rax", 0, 1)], &[]));
        t.push(entry(0x14, &[("rbx", 0, 2)], &[]));
        t.push(entry(0x18, &[("rax", 1, 5), ("rcx", 0, 1)], &[]));
        let rax: Vec<u64> = t.by_reg("rax", 10).iter().map(|e| e.pc).collect();
        assert_eq!(rax, [0x18, 0x10]);
        assert_eq!(t.by_reg("rax", 1).len(), 1);
        assert_eq!(t.by_reg("rcx", 10).len(), 1);
        assert!(t.by_reg("rdx", 10).is_empty());
        // every instruction moves the pc
        assert_eq!(t.by_reg("rip", 10).len(), 3);
    }

    #[test]
    fn back_mem_gives_the_writes_that_cover_the_address() {
        let mut t = Tape::default();
        t.start(100);
        t.push(entry(0x10, &[], &[(0x7000, 0, 1)]));
        t.push(entry(0x14, &[], &[(0x7001, 0, 2)]));
        let mut wide = entry(0x18, &[], &[]);
        wide.mem.push(MemWrite { addr: 0x6ffe, before: vec![0; 4], after: vec![9; 4] });
        t.push(wide);
        let at = |a| t.by_mem(a, 10).iter().map(|e| e.pc).collect::<Vec<_>>();
        assert_eq!(at(0x7000), [0x10]);
        assert_eq!(at(0x7001), [0x18, 0x14]);
        assert_eq!(at(0x6ffe), [0x18]);
        assert!(at(0x7002).is_empty());
        assert!(at(0x6ffd).is_empty());
    }

    #[test]
    fn a_step_makes_one_entry_with_the_changed_registers_and_the_bytes_written() {
        let mut t = Tape::default();
        t.start(10);
        let a = Regs { rip: 0x400000, ..Regs::default() };
        let mut b = a;
        b.rip = 0x400004;
        b.gp[0] = 7;
        let mut none = |_: u64, _: usize| Vec::new();
        t.observe(3, &a, false, None, &mut none);
        t.set_pending(3, vec![(0x9000, vec![0, 0])]);
        t.observe(3, &b, true, Some(0x400000), &mut |_, n| vec![1; n]);
        let e = t.back(1)[0].clone();
        assert_eq!(e.pc, 0x400000);
        assert_eq!(e.regs.len(), 1);
        assert_eq!(e.mem, [MemWrite { addr: 0x9000, before: vec![0, 0], after: vec![1, 1] }]);
        // a jump the record did not see is a gap, not an entry
        let mut c = b;
        c.rip = 0x500000;
        t.observe(3, &c, false, None, &mut none);
        assert_eq!((t.kept(), t.gaps), (1, 1));
        // a step from a place other than the last one seen is a gap too
        let mut d = c;
        d.rip = 0x500004;
        t.observe(3, &d, true, Some(0x123), &mut none);
        assert_eq!((t.kept(), t.gaps), (1, 2));
    }

    #[test]
    fn stop_keeps_the_entries_and_the_status_line_says_so() {
        let mut t = tape(5, 3);
        assert!(t.status_line().unwrap().starts_with("recording on: 3 entries kept, 0 dropped"));
        t.stop();
        assert!(t.status_line().unwrap().starts_with("recording off: 3 entries kept"));
        assert!(Tape::default().status_line().is_none());
    }
}
