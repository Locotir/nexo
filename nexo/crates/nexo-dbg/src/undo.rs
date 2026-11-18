//! The plan for undoing recorded instructions, with no process in it: given the record (newest first) and the
//! registers now, which memory goes back, in which order, which registers the thread ends with, and when to refuse.
//! The commands that carry the plan out are in `cmds/record.rs`.
//!
//! The record holds, per instruction, the registers it changed (old and new), the pc it started at, the pc it
//! left, and the bytes it wrote (before and after). Undoing walks it from the newest entry back: the registers
//! take each entry's old values, memory takes each write's before bytes, newest write first so the oldest
//! "before" wins where writes overlap. Nothing is changed when any entry cannot be undone.

use crate::platform::Tid;
use crate::record::{Entry, MemWrite, MAX_WRITE};
use crate::regs::Regs;

/// What decides how far back to go.
pub enum Goal<'a> {
    /// Exactly this many instructions (at least 1).
    Count(usize),
    /// Until this says stop after an entry is undone (its `pc` is then the thread's pc), or the record runs out.
    Until(&'a dyn Fn(&Entry) -> bool),
}

/// What undoing does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Entries undone.
    pub undone: usize,
    /// Memory to write, in the order to write it: (address, bytes).
    pub writes: Vec<(u64, Vec<u8>)>,
    /// The registers to set, pc included.
    pub regs: Regs,
    /// The pc now and the pc after the undo.
    pub pc_from: u64,
    pub pc_to: u64,
    /// Registers that get a different value.
    pub regs_changed: usize,
    /// Total bytes in `writes`.
    pub bytes: usize,
    /// `Until` found no stop and went back as far as the record goes.
    pub ran_out: bool,
}

/// A system call instruction (`syscall`, `sysenter`, `int 0x80`) starts with these bytes.
pub fn is_syscall_bytes(b: &[u8]) -> bool {
    matches!(b, [0x0f, 0x05, ..] | [0x0f, 0x34, ..] | [0xcd, 0x80, ..])
}

/// Some byte of `m` inside `lo..hi` has a different value after the instruction than before.
pub fn changes_bytes(m: &MemWrite, lo: u64, hi: u64) -> bool {
    m.before.iter().zip(&m.after).enumerate().any(|(i, (b, a))| {
        let at = m.addr + i as u64;
        (lo..hi).contains(&at) && b != a
    })
}

/// The plan, or the plain reason it cannot be done. `ring` is the whole record, newest first; `now` the registers
/// of thread `tid`; `syscall(pc)` says whether the instruction at `pc` is a system call.
pub fn plan(
    ring: &[&Entry],
    tid: Tid,
    now: &Regs,
    goal: Goal,
    syscall: &mut dyn FnMut(u64) -> bool,
) -> Result<Plan, String> {
    if ring.is_empty() {
        return Err("the record is empty: nothing to undo".into());
    }
    if let Goal::Count(n) = goal {
        if ring.len() < n {
            return Err(format!(
                "the record holds {} instruction{}, not {n}",
                ring.len(),
                if ring.len() == 1 { "" } else { "s" }
            ));
        }
    }
    let newest = ring[0];
    if now.rip != newest.next {
        return Err(format!(
            "the thread is at {:#x} but the last recorded instruction (#{}) left it at {:#x}: it moved without the record (a jump, a run before `record`, a `restart`)",
            now.rip, newest.n, newest.next
        ));
    }
    let named = now.named();
    for (r, _, new) in &newest.regs {
        if named.iter().find(|(n, _)| n == r).is_some_and(|(_, v)| v != new) {
            return Err(format!("{r} is not what instruction #{} left in it: it was set since (setreg), so the record no longer matches", newest.n));
        }
    }
    let mut s = *now;
    let mut writes = Vec::new();
    let mut undone = 0;
    let mut ran_out = false;
    let mut prev_pc = now.rip;
    loop {
        if let Goal::Count(n) = goal {
            if undone == n {
                break;
            }
        }
        let Some(e) = ring.get(undone).copied() else {
            ran_out = true;
            break;
        };
        if e.tid != tid {
            return Err(format!(
                "thread {} ran in between (instruction #{} at {:#x}): memory may have changed under this thread, so nothing is undone",
                e.tid, e.n, e.pc
            ));
        }
        if e.next != prev_pc {
            return Err(format!(
                "instruction #{} at {:#x} left the thread at {:#x}, not at {prev_pc:#x} where the next one began: the record has a gap",
                e.n, e.pc, e.next
            ));
        }
        if e.signal != 0 {
            return Err(format!(
                "instruction #{} at {:#x} was stepped with signal {} ({}) waiting: the step went into the signal frame and handler, which the record did not keep, so it cannot be undone",
                e.n,
                e.pc,
                crate::engine::signal_name(e.signal as u32),
                e.signal
            ));
        }
        if e.lossy {
            return Err(format!(
                "instruction #{} at {:#x} may have written memory the record did not keep (more than {MAX_WRITE} bytes, an address it could not work out, or code it could not decode)",
                e.n, e.pc
            ));
        }
        for (r, old, _) in &e.regs {
            if !s.put(r, *old) {
                return Err(format!("instruction #{} changed {r}, which cannot be set back", e.n));
            }
        }
        if syscall(e.pc) {
            // rax before the call is the call number
            let what = match (s.x32, crate::syscalls::name(s.gp[0])) {
                (false, Some(n)) => format!("{n} ({})", s.gp[0]),
                _ => format!("number {}", s.gp[0]),
            };
            return Err(format!(
                "a system call ran inside the range: {what} at {:#x}, instruction #{}; the kernel's writes are not in the record, so it cannot be undone (`rstepi` a smaller number, or `restart` a checkpoint)",
                e.pc, e.n
            ));
        }
        for m in e.mem.iter().rev() {
            writes.push((m.addr, m.before.clone()));
        }
        s.rip = e.pc;
        prev_pc = e.pc;
        undone += 1;
        if let Goal::Until(stop) = goal {
            if stop(e) {
                break;
            }
        }
    }
    if undone == 0 {
        return Err("nothing to undo".into());
    }
    let regs_changed = now.named().into_iter().zip(s.named()).filter(|((_, a), (_, b))| a != b).count();
    let bytes = writes.iter().map(|(_, b)| b.len()).sum();
    Ok(Plan { undone, writes, regs: s, pc_from: now.rip, pc_to: s.rip, regs_changed, bytes, ran_out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: u64, pc: u64, next: u64, regs: &[(&'static str, u64, u64)], mem: &[(u64, u8, u8)]) -> Entry {
        Entry {
            n,
            tid: 1,
            pc,
            next,
            lossy: false,
            signal: 0,
            regs: regs.to_vec(),
            mem: mem.iter().map(|(a, b, x)| MemWrite { addr: *a, before: vec![*b], after: vec![*x] }).collect(),
        }
    }

    fn at(rip: u64, rax: u64) -> Regs {
        let mut r = Regs { rip, ..Regs::default() };
        r.gp[0] = rax;
        r
    }

    /// Three instructions: rax 0 -> 1 (writes [0x100]: 7 -> 8), rax 1 -> 2, rax 2 -> 3 (writes [0x100]: 8 -> 9).
    fn tape() -> Vec<Entry> {
        vec![
            e(3, 0x1008, 0x100c, &[("rax", 2, 3)], &[(0x100, 8, 9)]),
            e(2, 0x1004, 0x1008, &[("rax", 1, 2)], &[]),
            e(1, 0x1000, 0x1004, &[("rax", 0, 1)], &[(0x100, 7, 8)]),
        ]
    }

    fn run(es: &[Entry], rip: u64, rax: u64, goal: Goal) -> Result<Plan, String> {
        let ring: Vec<&Entry> = es.iter().collect();
        plan(&ring, 1, &at(rip, rax), goal, &mut |_| false)
    }

    #[test]
    fn two_back_gives_the_writes_newest_first_and_the_registers_before_the_oldest() {
        let t = tape();
        let p = run(&t, 0x100c, 3, Goal::Count(2)).unwrap();
        assert_eq!(p.undone, 2);
        assert_eq!(p.writes, [(0x100, vec![8])]);
        assert_eq!((p.pc_from, p.pc_to, p.regs.rip, p.regs.gp[0]), (0x100c, 0x1004, 0x1004, 1));
        assert_eq!((p.bytes, p.regs_changed, p.ran_out), (1, 2, false));
    }

    #[test]
    fn overlapping_writes_end_with_the_oldest_before_bytes() {
        let t = tape();
        let p = run(&t, 0x100c, 3, Goal::Count(3)).unwrap();
        assert_eq!(p.writes, [(0x100, vec![8]), (0x100, vec![7])]);
        assert_eq!((p.regs.gp[0], p.pc_to), (0, 0x1000));
    }

    #[test]
    fn it_refuses_before_changing_anything() {
        let t = tape();
        let m = run(&t, 0x100c, 3, Goal::Count(4)).unwrap_err();
        assert!(m.contains("holds 3 instructions, not 4"), "{m}");
        assert!(run(&[], 0x100c, 3, Goal::Count(1)).unwrap_err().contains("empty"));
        // the thread is not where the record left it
        assert!(run(&t, 0x2000, 3, Goal::Count(1)).unwrap_err().contains("moved without the record"));
        // a register was set since
        assert!(run(&t, 0x100c, 9, Goal::Count(1)).unwrap_err().contains("rax is not what"));
    }

    #[test]
    fn another_thread_in_the_range_a_gap_and_a_lossy_entry_refuse() {
        let mut t = tape();
        t[1].tid = 2;
        let m = run(&t, 0x100c, 3, Goal::Count(3)).unwrap_err();
        assert!(m.contains("thread 2 ran in between") && m.contains("#2"), "{m}");
        // only the newest is asked for: the other thread's entry is outside the range
        assert!(run(&t, 0x100c, 3, Goal::Count(1)).is_ok());
        let mut g = tape();
        g[1].next = 0x1009;
        assert!(run(&g, 0x100c, 3, Goal::Count(2)).unwrap_err().contains("gap"));
        let mut l = tape();
        l[1].lossy = true;
        let m = run(&l, 0x100c, 3, Goal::Count(2)).unwrap_err();
        assert!(m.contains("#2") && m.contains("did not keep"), "{m}");
        let mut s = tape();
        s[1].signal = 11;
        s[1].lossy = true;
        let m = run(&s, 0x100c, 3, Goal::Count(2)).unwrap_err();
        assert!(m.contains("signal") && m.contains("(11)") && m.contains("#2"), "{m}");
    }

    #[test]
    fn a_system_call_in_the_range_is_named_with_its_place() {
        let t = tape();
        let ring: Vec<&Entry> = t.iter().collect();
        // entry #2 at 0x1004 is a `syscall`; rax before it is 1 (write)
        let m = plan(&ring, 1, &at(0x100c, 3), Goal::Count(3), &mut |pc| pc == 0x1004).unwrap_err();
        assert!(m.contains("write (1)") && m.contains("0x1004") && m.contains("#2"), "{m}");
        // outside the range it does not matter
        assert!(plan(&ring, 1, &at(0x100c, 3), Goal::Count(1), &mut |pc| pc == 0x1004).is_ok());
        assert!(
            is_syscall_bytes(&[0x0f, 0x05]) && is_syscall_bytes(&[0xcd, 0x80, 0]) && !is_syscall_bytes(&[0x90, 0x90])
        );
    }

    #[test]
    fn until_stops_after_the_entry_it_names_or_runs_out() {
        let t = tape();
        let p = run(&t, 0x100c, 3, Goal::Until(&|e| e.pc == 0x1004)).unwrap();
        assert_eq!((p.undone, p.pc_to, p.ran_out), (2, 0x1004, false));
        let p = run(&t, 0x100c, 3, Goal::Until(&|_| false)).unwrap();
        assert_eq!((p.undone, p.pc_to, p.ran_out), (3, 0x1000, true));
        // a watch on 0x100 stops after undoing the writes that changed it
        let w = |e: &Entry| e.mem.iter().any(|m| changes_bytes(m, 0x100, 0x101));
        assert_eq!(run(&t, 0x100c, 3, Goal::Until(&w)).unwrap().undone, 1);
    }

    #[test]
    fn a_write_that_leaves_the_byte_alone_is_not_a_change() {
        let m = MemWrite { addr: 0x100, before: vec![1, 2, 3], after: vec![1, 9, 3] };
        assert!(changes_bytes(&m, 0x101, 0x102));
        assert!(!changes_bytes(&m, 0x100, 0x101));
        assert!(!changes_bytes(&m, 0x102, 0x110));
        assert!(changes_bytes(&m, 0x0, 0x1000));
    }
}
