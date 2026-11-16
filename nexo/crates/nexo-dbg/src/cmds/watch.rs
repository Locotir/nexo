//! Watches of any length: debug registers while the range fits the four of them, else guarded pages, else single
//! steps that compare the bytes.

use super::*;
use crate::engine::watch::hw_pieces;
use crate::engine::WatchHow;
use crate::platform::PAGE;

/// Most bytes one watch covers: it keeps a copy of them to compare.
const WATCH_MAX: u64 = 1 << 24;

const WATCH_USE: &str = "watch <place|A..B> [length] [w|r|x] [hw|page|step] [if <expr>] [after N] [log [text]]";

impl Session {
    fn slots(&self) -> [Option<HwSlot>; 4] {
        let mut s = [None; 4];
        for w in &self.wps {
            for (i, h) in &w.slots {
                s[*i] = Some(*h);
            }
        }
        s
    }

    /// Put the debug registers as the watches say, and have the guarded pages worked out again at the next run.
    pub(super) fn apply_watches(&mut self) -> Result<()> {
        if self.state == State::Exited {
            return Ok(());
        }
        let s = self.slots();
        self.t.set_watch(&s)?;
        self.drop_guards();
        Ok(())
    }

    /// The size the program's symbol `name` gives a variable (`int counter` is 4 bytes).
    fn data_size(&self, name: &str) -> Option<u64> {
        self.mods.main_image().symbols.iter().find(|s| s.name == name && !s.func && s.size > 0).map(|s| s.size)
    }

    pub(super) fn cmd_watch(&mut self, words: &[String]) -> Result<Output> {
        let Some(spec) = words.first() else { return usage(WATCH_USE) };
        self.need_stopped()?;
        let (mut len, mut acc, mut how) = (None, Access::Write, None);
        let (mut cond, mut after, mut log) = (None, 0u64, None);
        let keyword = |w: &String| matches!(w.as_str(), "if" | "after" | "log");
        let mut i = 1;
        while i < words.len() {
            let w = words[i].as_str();
            if keyword(&words[i]) {
                let end = words[i + 1..].iter().position(keyword).map_or(words.len(), |p| i + 1 + p);
                let text = words[i + 1..end].join(" ");
                match w {
                    "if" => cond = Some(text),
                    "log" => log = Some(text),
                    _ => after = parse_addr(&text).map_err(|_| crate::Error::Usage("after needs a number".into()))?,
                }
                i = end;
                continue;
            }
            match w {
                "w" => acc = Access::Write,
                "r" | "rw" => acc = Access::ReadWrite,
                "x" => acc = Access::Exec,
                "hw" => how = Some(WatchHow::Hardware),
                "page" => how = Some(WatchHow::Pages),
                "step" => how = Some(WatchHow::Steps),
                f if f.starts_with("--") => {}
                n => {
                    let v = parse_addr(n).ok().filter(|v| *v > 0);
                    len = Some(v.ok_or_else(|| crate::Error::Usage(format!("unexpected {n:?}: {WATCH_USE}")))?);
                }
            }
            i += 1;
        }
        let (addr, len) = match self.span_of(spec)? {
            Some((lo, hi)) => (lo, hi - lo),
            None => {
                let a = self.addr_of(spec)?;
                (a, len.or_else(|| self.data_size(spec)).unwrap_or(8))
            }
        };
        if len > WATCH_MAX {
            return usage(format!("{len} bytes: one watch covers at most {WATCH_MAX} bytes"));
        }
        let used: Vec<usize> = self.wps.iter().flat_map(|w| w.slots.iter().map(|(s, _)| *s)).collect();
        let free: Vec<usize> = (0..4).filter(|s| !used.contains(s)).collect();
        let len = if acc == Access::Exec { 1 } else { len };
        let pieces = hw_pieces(addr, len, free.len());
        let forced = how.is_some();
        let how =
            how.unwrap_or(if pieces.is_some() || acc == Access::Exec { WatchHow::Hardware } else { WatchHow::Pages });
        if acc == Access::Exec && (how != WatchHow::Hardware || cond.is_some() || log.is_some() || after > 0) {
            return usage("an `x` watch is one debug register on one byte and stops at every pass; `break` has `if`, `after` and `log` for code");
        }
        if how == WatchHow::Steps && acc == Access::ReadWrite {
            return usage("a step watch compares the bytes after each instruction, so it sees writes; `hw` and `page` see reads too");
        }
        let id = self.new_id();
        let last = self.read_vec(addr, len as usize);
        if last.len() as u64 != len {
            return fail(format!("{addr:#x}+{len} cannot be read in the program: there is nothing there to watch"));
        }
        let mut wp = Wp { id, addr, len, access: acc, how, slots: Vec::new(), last, hits: 0, cond, after, log };
        let mut lines = Vec::new();
        match how {
            WatchHow::Hardware => {
                let Some(pieces) = pieces else {
                    return fail(format!(
                        "{len} bytes at {addr:#x} take more aligned pieces of 8, 4, 2 or 1 bytes than the {} free debug registers; `page` or `step` watch any length",
                        free.len()
                    ));
                };
                if pieces.is_empty() || free.is_empty() {
                    return fail("all 4 debug registers are in use: `delete <id>` frees a hardware watch, or `page` and `step` watch without them");
                }
                wp.slots = free
                    .iter()
                    .zip(&pieces)
                    .map(|(s, (a, n))| (*s, HwSlot { addr: *a, len: *n, access: acc }))
                    .collect();
                let regs: Vec<String> =
                    wp.slots.iter().map(|(s, h)| format!("dr{s} {:#x}/{}", h.addr, h.len)).collect();
                lines.push(format!(
                    "{}: the processor stops the thread right after an access, at full speed; {} of 4 debug registers left",
                    regs.join(" "),
                    free.len() - pieces.len()
                ));
                self.wps.push(wp);
                if let Err(e) = self.apply_watches() {
                    self.wps.pop();
                    return Err(e);
                }
            }
            WatchHow::Pages | WatchHow::Steps => {
                self.wps.push(wp);
                let mut how = how;
                if how == WatchHow::Pages {
                    if let Err(e) = self.page_check(acc) {
                        if forced || acc == Access::ReadWrite {
                            self.wps.pop();
                            return Err(e);
                        }
                        lines.push(format!(
                            "pages cannot be guarded here ({e}): it runs one instruction at a time instead"
                        ));
                        how = WatchHow::Steps;
                        if let Some(w) = self.wps.last_mut() {
                            w.how = how;
                        }
                    }
                }
                self.drop_guards();
                lines.push(if how == WatchHow::Pages {
                    let (lo, hi) = (addr & !(PAGE - 1), (addr + len).div_ceil(PAGE) * PAGE);
                    format!(
                        "{} page{} of 4 KiB at {lo:#x} lose {} access while the program runs; each access to them, other data there included, costs a fault, one step and two protection changes; a system call that writes into them (read, recv) fails with EFAULT meanwhile; a child forked meanwhile gets its copy's own access back, and the accesses of a vfork child (it shares the memory until its exec) go through without a hit; `step` instead compares after each instruction",
                        (hi - lo) / PAGE,
                        if hi - lo > PAGE { "s" } else { "" },
                        if acc == Access::ReadWrite { "all" } else { "write" }
                    )
                } else {
                    format!(
                        "thread {} runs one instruction per stop and the bytes are compared after each: two system calls and a read per instruction instead of full speed; the other threads wait while this watch is set",
                        self.step_watch_thread().unwrap_or(self.cur)
                    )
                });
            }
        }
        let w = self.wps.last().map_or(how, |w| w.how);
        let kind = match acc {
            Access::Exec => "x",
            Access::Write => "w",
            Access::ReadWrite => "r/w",
        };
        let name = self.mods.label(addr, true).filter(|n| !n.is_empty()).map(|n| format!(" {n}")).unwrap_or_default();
        lines.insert(0, format!("watch #{id} {addr:#x}{name} {len} bytes {kind} ({})", w.word()));
        if acc != Access::Exec {
            lines.push(
                "at a hit `$old`, `$new` and `$at` (offset of the change in the range) hold the change for `if` and `log`; a `log` watch writes one log line per hit and does not stop".into(),
            );
        }
        Ok(rows("watch", lines, json!({"id": id, "addr": addr, "len": len, "how": w.word()})))
    }

    /// Whether the pages of the newest watch can be guarded: they are mapped, a read guard holds no code, and the
    /// system lets their access change (it is set to what it already is).
    fn page_check(&mut self, acc: Access) -> Result<()> {
        let plan = self.guard_plan()?;
        if acc == Access::ReadWrite && plan.iter().any(|g| g.orig & crate::platform::PROT_X != 0) {
            return fail("the range shares a page with code, which a read guard would stop at every instruction; `hw` watches up to 4 aligned pieces of 8 bytes");
        }
        for g in plan {
            self.t.protect(g.start, g.len, g.orig)?;
        }
        Ok(())
    }
}
