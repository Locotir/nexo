//! Checkpoints, as gdb has them: `checkpoint` makes the stopped program fork itself (a call of `fork` in it, by the
//! machinery of `call`), and the child stays stopped and attached, a copy of the program as it is. `restart N` goes
//! back to one: the copy becomes the program, a fresh fork of it takes its place in the list (so N can be used
//! again), and the process left is killed. Linux only, one thread, not inside a system call.
//!
//! The child's memory holds our `0xcc` at the breakpoints the parent had inserted during the call and its guarded
//! pages at their guard access; both are put back when the checkpoint is made, so a checkpoint is a clean copy and
//! the breakpoints in force are always the session's table at the next run.

use std::collections::BTreeSet;

use super::call::Called;
use super::watch::Guard;
use super::*;
use crate::cmds::Args;
use crate::expr::Arg;
use crate::usage;

/// What the answers say once about what a checkpoint is not.
pub const DIFFERS: &str = "a checkpoint is a fork of the program: its pid differs (getpid), a file opened before it is shared with the checkpoint (a write goes to the same file), and other processes and the network are not rewound";

/// What is known of a checkpoint without its process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub id: u32,
    pub pid: u32,
    pub pc: u64,
    /// Where it was made: a function, and file:line when there is one.
    pub place: String,
    /// The stop count of the session when it was made.
    pub nstop: u64,
}

/// A stored checkpoint: the child process, stopped and attached. Dropping it kills the process, which a plain drop
/// of a target that was attached would only let go.
pub struct Checkpoint {
    pub mark: Mark,
    t: Box<dyn Target>,
}

impl Drop for Checkpoint {
    fn drop(&mut self) {
        let _ = self.t.kill();
    }
}

/// The process a `restart` made the program: killed when the session goes, as a launched program is. It is the
/// first field of the session, so it goes before the target lets the process go; it kills only a process this
/// debugger still traces.
#[derive(Default)]
pub struct Reaper(pub u32);

impl Drop for Reaper {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if self.0 != 0 {
            let ours = std::fs::read_to_string(format!("/proc/{}/status", self.0)).is_ok_and(|s| {
                s.lines()
                    .any(|l| l.strip_prefix("TracerPid:").is_some_and(|v| v.trim() == std::process::id().to_string()))
            });
            if ours {
                // SAFETY: the process is one this debugger traces, a fork it made.
                unsafe { libc::kill(self.0 as i32, libc::SIGKILL) };
            }
        }
    }
}

/// `pc`, its name and its source line as one phrase.
pub fn where_text(pc: u64, name: &str, src: Option<&str>) -> String {
    let mut s = format!("{pc:#x}");
    if !name.is_empty() {
        s.push(' ');
        s.push_str(name);
    }
    if let Some(src) = src.filter(|x| !x.is_empty()) {
        s.push_str(&format!(" ({src})"));
    }
    s
}

/// The rows of `checkpoints`, `now` being the session's stop count.
pub fn list_lines(marks: &[Mark], now: u64) -> Vec<String> {
    if marks.is_empty() {
        return vec!["no checkpoints: `checkpoint` makes one at this stop".into()];
    }
    let mut l = vec![format!("{} checkpoint{}", marks.len(), if marks.len() == 1 { "" } else { "s" })];
    for m in marks {
        let ago = now.saturating_sub(m.nstop);
        let when = match ago {
            0 => "made at this stop".to_string(),
            1 => format!("made at stop {}, 1 stop ago", m.nstop),
            n => format!("made at stop {}, {n} stops ago", m.nstop),
        };
        l.push(format!("#{} pid {} at {} | {when}", m.id, m.pid, m.place));
    }
    l.push("`restart N` goes back to one (it stays), `delete checkpoint N|all` kills it".into());
    l
}

/// The answer to `checkpoint`.
pub fn made_lines(m: &Mark) -> Vec<String> {
    vec![
        format!("checkpoint #{} made: pid {} stopped at {}", m.id, m.pid, m.place),
        format!("{DIFFERS}; `restart {}` comes back to it, `checkpoints` lists them", m.id),
    ]
}

/// The breakpoint bytes of a checkpoint's memory against the breakpoints wanted there: the bytes to write back
/// (`held`: address and the program's own byte under a `0xcc`, those no longer wanted) and the addresses to insert
/// (wanted, not held). With nothing wanted, every held byte goes back.
pub fn reconcile(wanted: &BTreeSet<u64>, held: &[(u64, u8)]) -> (Vec<(u64, u8)>, Vec<u64>) {
    let restore: Vec<(u64, u8)> = held.iter().copied().filter(|(a, _)| !wanted.contains(a)).collect();
    let add: Vec<u64> = wanted.iter().copied().filter(|a| !held.iter().any(|(h, _)| h == a)).collect();
    (restore, add)
}

/// A child the backend kept at a checkpoint's fork, with what its memory holds that is ours.
struct Kept {
    kid: Box<dyn Target>,
    pid: u32,
}

impl Session {
    /// A stop of the process the checkpoint commands act on: Linux, a process of this machine, one thread, not in
    /// a system call, not recording.
    fn ckpt_base(&mut self) -> Result<()> {
        self.need_stopped()?;
        if self.t.os() != Os::Linux || !self.t.local_proc() {
            return fail("checkpoints need a Linux process of this machine: the program forks itself, and the copy stays attached");
        }
        if self.tape.on {
            return fail("`record` is on: a checkpoint is a fork the record cannot follow; `record stop` first");
        }
        if let Some(c) = self.call_under_way() {
            return fail(format!("`{c}` is under way: `cont` runs it to its end, `kill call` ends it"));
        }
        Ok(())
    }

    /// What the process being forked must be: one thread (a fork keeps only the thread that calls it) and not in a
    /// system call (the call would run again from its start).
    fn ckpt_thread_ok(&mut self) -> Result<()> {
        let n = self.t.threads().len();
        if n > 1 {
            return fail(format!(
                "the program has {n} threads: a fork keeps only the thread that calls it, so a checkpoint of it would not be the program"
            ));
        }
        let tid = self.t.threads().first().copied().unwrap_or(self.cur);
        if self.t.regs(tid)?.orig_rax != u64::MAX {
            return fail("the thread is stopped inside a system call: a checkpoint here would not come back from it; step out of it first (`finish`, or `cont` to the next stop)");
        }
        Ok(())
    }

    fn fork_addr(&mut self) -> Result<u64> {
        for n in ["libc!fork", "fork", "libc!__libc_fork", "__libc_fork"] {
            // an import slot is a table cell, not code: only the function itself can be called
            if let Ok(nexo_bin::Place::Code(a)) = self.resolve_place(n) {
                return Ok(a);
            }
        }
        fail("no `fork` function is loaded in the program (libc has it): there is nothing to make a copy with")
    }

    /// Make the stopped thread of `self.t` call `fork` and keep the child, cleaned: no breakpoint bytes, no guarded
    /// pages, registers as they were before the call.
    fn fork_here(&mut self) -> Result<Kept> {
        let tid = self.t.threads().first().copied().unwrap_or(self.cur);
        let addr = self.fork_addr()?;
        let saved = self.t.regs(tid)?;
        let vector = self.t.vector_state(tid);
        let (last, frame, prev, cur) = (self.last_stop.clone(), self.frame, self.prev.clone(), self.cur);
        let was = self.follow;
        if !self.t.set_follow(Follow::Both) {
            return fail("this session cannot keep a forked child");
        }
        self.ckpt_forks = Some(Vec::new());
        let r = self.call_now(tid, addr, &[] as &[Arg], "fork()".into(), Duration::from_secs(10));
        let got = self.ckpt_forks.take().unwrap_or_default();
        self.t.set_follow(was);
        self.last_stop = last;
        self.frame = frame;
        self.prev = prev;
        self.cur = cur;
        let pid = match r? {
            Called::Value(v) => v as u32 as i32,
            Called::Told(o) => {
                let said: Vec<String> = o.rows.iter().map(|r| r.text.clone()).collect();
                return fail(format!("fork() did not come back: {}", said.join(" | ")));
            }
        };
        if pid <= 0 {
            return fail(format!("fork() failed in the program (it returned {pid})"));
        }
        let pid = pid as u32;
        let mut mine = None;
        let mut others = Vec::new();
        for (f, g) in got {
            if f.pid == pid && mine.is_none() {
                mine = Some((f, g));
            } else {
                others.push(f);
            }
        }
        // any other child the call made is not ours to keep
        for mut o in others {
            if let Some(mut t) = o.target.take() {
                let _ = t.kill();
            }
        }
        let Some((f, guards)) = mine else {
            return fail(format!("fork() returned pid {pid}, but the backend did not keep the child"));
        };
        let Some(mut kid) = f.target else {
            return fail(format!("fork() returned pid {pid}, but the backend did not keep the child"));
        };
        let patches = f.patches;
        // clean it: our bytes out of its memory, then its registers as they were before the call
        let child_tid = kid.threads().first().copied().unwrap_or(pid as Tid);
        let cleaned = (|| -> Result<()> {
            let (restore, _) = reconcile(&BTreeSet::new(), &patches);
            for (a, b) in restore {
                kid.write(a, &[b])?;
            }
            // mprotect runs in the child, so before its registers are put back
            for g in guards.iter().filter(|g| g.armed) {
                kid.protect(g.start, g.len, g.orig)?;
            }
            kid.set_patches(&[]);
            kid.set_guards(&[]);
            kid.set_follow(Follow::Parent);
            kid.set_regs(child_tid, &saved)?;
            if let Some(v) = &vector {
                kid.set_vector_state(child_tid, v)?;
            }
            Ok(())
        })();
        if let Err(e) = cleaned {
            let _ = kid.kill();
            return fail(format!("the copy made by fork() could not be put back as it was ({e}); it was killed"));
        }
        Ok(Kept { kid, pid })
    }

    /// Where the thread is, for a checkpoint and its list.
    fn ckpt_place(&mut self, pc: u64) -> String {
        let name = self.mods.label(pc, false).unwrap_or_default();
        let src = self.mods.source(pc);
        where_text(pc, &name, src.as_deref())
    }

    /// `checkpoint` | `checkpoint list` | `checkpoint del N|all`.
    pub(crate) fn cmd_checkpoint(&mut self, a: &Args) -> Result<Output> {
        match a.pos.first().map(String::as_str) {
            Some("list") => return self.cmd_checkpoints(),
            Some("del" | "delete") => return self.ckpt_delete(&a.pos[1..]),
            Some(_) => return usage("checkpoint | checkpoint del N|all | checkpoints | restart N"),
            None => {}
        }
        self.ckpt_base()?;
        self.ckpt_thread_ok()?;
        let tid = self.t.threads().first().copied().unwrap_or(self.cur);
        let pc = self.t.regs(tid)?.rip;
        let place = self.ckpt_place(pc);
        let Kept { kid, pid } = self.fork_here()?;
        let id = self.ckpt_next;
        self.ckpt_next += 1;
        let mark = Mark { id, pid, pc, place, nstop: self.nstop };
        let l = made_lines(&mark);
        self.checkpoints.push(Checkpoint { mark, t: kid });
        Ok(rows("checkpoint", l, json!({"checkpoint": id, "pid": pid})))
    }

    pub(crate) fn cmd_checkpoints(&mut self) -> Result<Output> {
        let marks: Vec<Mark> = self.checkpoints.iter().map(|c| c.mark.clone()).collect();
        let l = list_lines(&marks, self.nstop);
        let data: Vec<_> =
            marks.iter().map(|m| json!({"id": m.id, "pid": m.pid, "pc": m.pc, "nstop": m.nstop})).collect();
        Ok(rows("checkpoints", l, json!({"checkpoints": data})))
    }

    /// `delete checkpoint N|all`: the stored process is killed.
    pub(crate) fn ckpt_delete(&mut self, words: &[String]) -> Result<Output> {
        let Some(w) = words.first() else { return usage("delete checkpoint N|all") };
        let n = self.checkpoints.len();
        if w == "all" {
            self.checkpoints.clear();
            return Ok(rows("checkpoint", vec![format!("{n} checkpoint(s) killed")], json!({})));
        }
        let id: u32 = w
            .trim_start_matches('#')
            .parse()
            .map_err(|_| crate::Error::Usage(format!("{w:?} is not a checkpoint number")))?;
        let Some(i) = self.checkpoints.iter().position(|c| c.mark.id == id) else {
            return fail(format!("no checkpoint #{id}: `checkpoints` lists them"));
        };
        let c = self.checkpoints.remove(i);
        let pid = c.mark.pid;
        drop(c);
        Ok(rows("checkpoint", vec![format!("checkpoint #{id} (pid {pid}) killed")], json!({"checkpoint": id})))
    }

    /// Every checkpoint process is killed: the session ends, or its program does.
    pub(crate) fn drop_checkpoints(&mut self) {
        self.checkpoints.clear();
    }

    /// `restart N`: the program comes back to checkpoint N.
    pub(crate) fn cmd_restart(&mut self, a: &Args) -> Result<Output> {
        let Some(w) = a.pos.first() else { return usage("restart N (`checkpoints` lists them)") };
        let id: u32 = w
            .trim_start_matches('#')
            .parse()
            .map_err(|_| crate::Error::Usage(format!("{w:?} is not a checkpoint number")))?;
        self.ckpt_base()?;
        let Some(i) = self.checkpoints.iter().position(|c| c.mark.id == id) else {
            return fail(format!("no checkpoint #{id}: `checkpoints` lists them"));
        };
        let old_pid = self.t.pid();
        let old_cur = self.cur;
        // the checkpoint's process stands in for the program while it is forked again
        std::mem::swap(&mut self.t, &mut self.checkpoints[i].t);
        let tid = self.t.threads().first().copied().unwrap_or(self.cur);
        self.cur = tid;
        self.prev.clear();
        self.mods.refresh(&mut *self.t);
        let made = match self.ckpt_thread_ok() {
            Ok(()) => self.fork_here(),
            Err(e) => Err(e),
        };
        let kept = match made {
            Ok(k) => k,
            Err(e) => {
                std::mem::swap(&mut self.t, &mut self.checkpoints[i].t);
                self.cur = old_cur;
                self.prev.clear();
                self.mods.refresh(&mut *self.t);
                return Err(e);
            }
        };
        // the copy takes N's place in the list; the process that was the program is the one left
        let mut gone = std::mem::replace(&mut self.checkpoints[i].t, kept.kid);
        let _ = gone.kill();
        drop(gone);
        self.checkpoints[i].mark.pid = kept.pid;
        let mark = self.checkpoints[i].mark.clone();
        self.reaper.0 = self.t.pid();
        self.t.set_follow(self.follow);
        // what belongs to the process, not the session
        self.cur = self.t.threads().first().copied().unwrap_or(self.cur);
        self.frame = 0;
        self.prev.clear();
        self.step_from = None;
        self.finish_from = None;
        self.inserted.clear();
        self.guards.clear();
        self.tr.pending.clear();
        self.mods.refresh(&mut *self.t);
        let mut l = vec![format!(
            "back at checkpoint #{id}: pid {} is the program now, pid {old_pid} was killed; the checkpoint stays as pid {}, `restart {id}` again comes back here",
            self.t.pid(),
            kept.pid
        )];
        l.push(DIFFERS.to_string());
        if let Err(e) = self.apply_watches() {
            l.push(format!("the hardware watches could not be set in the restored process ({e})"));
        }
        for i in 0..self.wps.len() {
            let (addr, len) = (self.wps[i].addr, self.wps[i].len as usize);
            self.wps[i].last = self.read_vec(addr, len);
        }
        if self.tr.wants_syscalls() {
            if let Err(e) = self.t.trace_syscalls(true) {
                l.push(format!("strace / catch syscall could not be switched on in the restored process ({e})"));
            }
        }
        if self.tr.heap.is_some() {
            l.push("heap on still holds the blocks of the process that was left".into());
        }
        let (breaks, watches) = (self.bps.iter().filter(|b| b.enabled).count(), self.wps.len());
        l.push(format!(
            "{breaks} breakpoint(s) and {watches} watch(es) in force as the session has them now; hooks and displays too"
        ));
        let tid = self.cur;
        let mut o = self.answer(tid, Why::Restart(mark_text(&mark, &l)));
        o.meta["restart"] = json!({"checkpoint": id, "pid": self.t.pid()});
        Ok(o)
    }
}

/// The lines of a restart's headline text.
fn mark_text(m: &Mark, l: &[String]) -> String {
    format!("restart #{} at {} | {}", m.id, m.place, l.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[u64]) -> BTreeSet<u64> {
        v.iter().copied().collect()
    }

    #[test]
    fn nothing_wanted_puts_every_held_byte_back() {
        let (restore, add) = reconcile(&set(&[]), &[(0x10, 0x55), (0x20, 0x48)]);
        assert_eq!(restore, [(0x10, 0x55), (0x20, 0x48)]);
        assert!(add.is_empty());
    }

    #[test]
    fn a_breakpoint_deleted_after_is_restored_and_one_added_after_is_inserted() {
        let (restore, add) = reconcile(&set(&[0x20, 0x30]), &[(0x10, 0x55), (0x20, 0x48)]);
        assert_eq!(restore, [(0x10, 0x55)], "0x10 is no longer wanted");
        assert_eq!(add, [0x30], "0x30 is new, 0x20 is held already");
    }

    #[test]
    fn the_same_table_changes_nothing() {
        let (restore, add) = reconcile(&set(&[0x10]), &[(0x10, 0x55)]);
        assert!(restore.is_empty() && add.is_empty());
    }

    #[test]
    fn where_text_names_the_place_with_what_is_known() {
        assert_eq!(where_text(0x401000, "main+4", Some("a.c:7")), "0x401000 main+4 (a.c:7)");
        assert_eq!(where_text(0x401000, "", None), "0x401000");
        assert_eq!(where_text(0x401000, "f", Some("")), "0x401000 f");
    }

    #[test]
    fn the_list_has_id_pid_place_and_when() {
        let m = |id, pid, n| Mark { id, pid, pc: 0x10, place: "0x10 f".into(), nstop: n };
        let l = list_lines(&[m(1, 100, 2), m(2, 101, 5)], 5);
        assert!(l[0].starts_with("2 checkpoints"), "{l:?}");
        assert!(l[1].contains("#1 pid 100 at 0x10 f") && l[1].contains("3 stops ago"), "{l:?}");
        assert!(l[2].contains("#2 pid 101") && l[2].contains("made at this stop"), "{l:?}");
        assert!(list_lines(&[], 3)[0].contains("no checkpoints"));
    }

    #[test]
    fn the_answer_says_once_what_differs() {
        let m = Mark { id: 1, pid: 77, pc: 0x10, place: "0x10 f".into(), nstop: 1 };
        let t = made_lines(&m).join("\n");
        for w in ["checkpoint #1", "pid 77", "pid differs", "shared with the checkpoint", "not rewound"] {
            assert!(t.contains(w), "lacks {w:?}: {t}");
        }
        assert_eq!(t.matches("not rewound").count(), 1);
    }

    #[test]
    fn the_classes_of_the_commands() {
        use crate::client::{dbg_class, DbgClass};
        let c = |w: &str| dbg_class(&[w.to_string()]);
        assert_eq!(c("checkpoint"), DbgClass::Change);
        assert_eq!(c("restart"), DbgClass::Control);
        assert_eq!(c("checkpoints"), DbgClass::Control);
    }
}
