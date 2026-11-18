//! `record` and `back`: a bounded record of the instructions that ran, and a way to read it. The record itself
//! (the ring and its filters) is in `record.rs`; this file is the commands and the hooks the run calls.
//! `back` looks back, it does not undo. `rstepi` and `rcont` do: they write the recorded "before" bytes back and
//! set the registers to what they were (the plan is in `undo.rs`), and let the entries go, so a step forward
//! records them again.

use super::*;
use crate::record::{Entry, DEFAULT_KEEP, MAX_KEEP, MAX_WRITE};
use crate::regs::Regs;
use crate::undo::{self, Goal};

/// Registers one row of `back` writes before it says how many more there were.
const BACK_REGS: usize = 6;
/// Bytes of one memory write a row of `back` shows.
const BACK_BYTES: usize = 16;
/// The most rows one `back` gives.
const BACK_MAX: usize = 1000;

fn hex(b: &[u8]) -> String {
    let mut s: String = b.iter().take(BACK_BYTES).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ");
    if b.len() > BACK_BYTES {
        s.push_str(" ...");
    }
    s
}

impl Session {
    /// The thread is at `r` after a stop or a single step. While recording this makes the entry for the
    /// instruction that just ran (when it was one step: `stepped`, from `from` when known) and reads what the
    /// next one is about to write, so its "before" bytes are in hand.
    pub(crate) fn rec_observe(&mut self, tid: Tid, r: &Regs, stepped: bool, from: Option<u64>) {
        if !self.tape.on {
            return;
        }
        {
            let Session { t, tape, .. } = self;
            tape.observe(tid, r, stepped, from, &mut |a, n| {
                let mut b = vec![0u8; n];
                let k = t.read(a, &mut b);
                b.truncate(k);
                b
            });
        }
        if self.tape.over_budget() {
            self.tape.give_up();
            return;
        }
        if !self.tape.has_pending(tid) {
            let bytes = self.read_vec(r.rip, 16);
            let Some(ins) = decode_live_in(self.bits(), &bytes, r.rip, &|n| r.get(n)) else {
                self.tape.set_lossy(tid, true);
                return;
            };
            let mut w = Vec::new();
            let mut lossy = false;
            for m in ins.mem.iter().filter(|m| m.write) {
                let Some(a) = m.addr else {
                    lossy = true;
                    continue;
                };
                if m.size == 0 || m.size > MAX_WRITE {
                    lossy = true;
                    continue;
                }
                let before = self.read_vec(a, m.size);
                if before.len() == m.size {
                    w.push((a, before));
                } else {
                    lossy = true;
                }
            }
            self.tape.set_lossy(tid, lossy);
            self.tape.set_pending(tid, w);
        }
    }

    fn rec_linux(&self, what: &str) -> Result<()> {
        if matches!(self.t.os(), Os::Linux) {
            return Ok(());
        }
        fail(format!(
            "{what}: recording is Linux-only for now (it single-steps with ptrace); `step` and `history` work here"
        ))
    }

    /// `record [N]` | `record stop`.
    pub(super) fn cmd_record(&mut self, a: &Args) -> Result<Output> {
        self.rec_linux("record")?;
        // gdb's `record full` is the same thing here
        let words: Vec<&String> = a.pos.iter().filter(|w| w.as_str() != "full").collect();
        match words.first().map(|s| s.as_str()) {
            Some("stop") => {
                let kept = self.tape.kept();
                let was = self.tape.on;
                self.tape.stop();
                let l = if was {
                    format!("recording ended; {kept} entries stay for `back`")
                } else {
                    "recording was not on".to_string()
                };
                return Ok(rows("record", vec![l], json!({"on": false, "kept": kept})));
            }
            Some(n) if parse_addr(n).is_err() => return usage("record [N] | record stop"),
            _ => {}
        }
        let keep = match words.first() {
            Some(n) => match parse_addr(n) {
                Ok(v) if (1..=MAX_KEEP as u64).contains(&v) => v as usize,
                _ => return usage(format!("record N keeps the last N instructions: 1 to {MAX_KEEP}")),
            },
            None => DEFAULT_KEEP,
        };
        self.need_stopped()?;
        self.tape.start(keep);
        // the place the record starts from, so the first step has an entry
        if let Ok(r) = self.t.regs(self.cur) {
            let cur = self.cur;
            self.rec_observe(cur, &r, false, None);
        }
        let l = vec![format!(
            "recording the last {keep} instructions from here: each step and stop keeps the pc, the registers that changed and memory writes of up to {MAX_WRITE} bytes"
        ), format!(
            "`cont` (and `finish`, `until`) now go one instruction at a time: slow, still bound by --wait, and the record ends by itself after {} fills of the ring in one run; `back N` reads it, `record stop` ends it",
            crate::record::WRAPS_PER_RUN
        )];
        Ok(rows("record", l, json!({"on": true, "keep": keep})))
    }

    /// `back [N]` | `back --reg REG [N]` | `back --mem ADDR [N]`: reads the record, never moves the program.
    pub(super) fn cmd_back(&mut self, a: &Args) -> Result<Output> {
        let n = match a.pos.first() {
            Some(x) => {
                parse_addr(x).map_err(|_| crate::Error::Usage("back needs a count".into()))?.clamp(1, BACK_MAX as u64)
            }
            None => 1,
        } as usize;
        if a.has("reg") && a.has("mem") {
            return usage("back takes --reg or --mem, not both");
        }
        if self.tape.kept() == 0 {
            let how =
                if matches!(self.t.os(), Os::Linux) { "`record` starts it" } else { "recording is Linux-only for now" };
            return Ok(rows("back", vec![format!("nothing recorded: {how}")], json!({"kept": 0})));
        }
        let want_mem = match a.flag("mem") {
            Some(at) => Some(self.addr_of(at)?),
            None => None,
        };
        let (title, picked): (String, Vec<Entry>) = if let Some(reg) = a.flag("reg") {
            let reg = reg.trim_start_matches('$').to_lowercase();
            if Regs::default().get(&reg).is_none() {
                return usage(format!("no register named {reg:?}"));
            }
            (
                format!("the last {n} instructions that changed {reg} (newest first)"),
                self.tape.by_reg(&reg, n).into_iter().cloned().collect(),
            )
        } else if let Some(addr) = want_mem {
            (
                format!("the last {n} writes that cover {addr:#x} (newest first)"),
                self.tape.by_mem(addr, n).into_iter().cloned().collect(),
            )
        } else {
            (format!("the last {n} instructions (newest first)"), self.tape.back(n).into_iter().cloned().collect())
        };
        let mut l = vec![format!(
            "{title}; {} kept, {} dropped; this looks back, it does not undo (`rstepi N` and `rcont` do)",
            self.tape.kept(),
            self.tape.dropped
        )];
        if picked.is_empty() {
            l.push("none in the record".into());
        }
        let bits = self.bits();
        for e in &picked {
            let label = self.mods.label(e.pc, false).unwrap_or_default();
            let bytes = self.read_vec(e.pc, 16);
            let text = decode_live_in(bits, &bytes, e.pc, &|_| None).filter(|i| !i.invalid).map(|i| i.text);
            let mut row = format!("#{} {:#x} {label}", e.n, e.pc);
            if let Some(t) = text {
                row.push_str(&format!(" | {t}"));
            }
            if !e.regs.is_empty() {
                let more = e.regs.len().saturating_sub(BACK_REGS);
                let shown: Vec<String> =
                    e.regs.iter().take(BACK_REGS).map(|(r, o, v)| format!("{r} {o:#x} -> {v:#x}")).collect();
                row.push_str(&format!(" | {}", shown.join(", ")));
                if more > 0 {
                    row.push_str(&format!(" (+{more} more)"));
                }
            }
            for m in e.mem.iter().filter(|m| want_mem.is_none_or(|a| m.covers(a))) {
                row.push_str(&format!(" | mem {:#x}: {} -> {}", m.addr, hex(&m.before), hex(&m.after)));
            }
            l.push(row);
        }
        Ok(rows("back", l, json!({"kept": self.tape.kept(), "dropped": self.tape.dropped, "shown": picked.len()})))
    }
    /// `rstepi [N]`: undo the last N recorded instructions of this thread.
    pub(super) fn cmd_rstepi(&mut self, a: &Args) -> Result<Output> {
        let n = match a.pos.first() {
            Some(x) => parse_addr(x).ok().filter(|v| (1..=MAX_KEEP as u64).contains(v)).ok_or_else(|| {
                crate::Error::Usage(format!("rstepi [N] undoes the last N instructions: 1 to {MAX_KEEP}"))
            })?,
            None => 1,
        } as usize;
        self.rec_undo("rstepi", Some(n))
    }

    /// `rcont`: undo until an enabled breakpoint's address is the pc, a watched address changes, or the record ends.
    pub(super) fn cmd_rcont(&mut self, _a: &Args) -> Result<Output> {
        self.rec_undo("rcont", None)
    }

    /// The two commands: plan, check, then change the program (memory first, then registers).
    fn rec_undo(&mut self, name: &str, count: Option<usize>) -> Result<Output> {
        self.rec_linux(name)?;
        self.need_stopped()?;
        if !self.tape.on {
            return fail(format!(
                "{name}: recording is off, so nothing can be undone (`record` starts it; entries kept from an earlier record are for `back` only, the program has moved since)"
            ));
        }
        let cur = self.cur;
        let now = self.t.regs(cur)?;
        let bps: Vec<(u32, u64)> =
            self.bps.iter().filter(|b| b.enabled && b.addr != 0).map(|b| (b.id, b.addr)).collect();
        let wps: Vec<(u32, u64, u64)> = self.wps.iter().map(|w| (w.id, w.addr, w.addr + w.len)).collect();
        let stop = |e: &Entry| {
            bps.iter().any(|(_, a)| *a == e.pc)
                || e.mem.iter().any(|m| wps.iter().any(|(_, lo, hi)| undo::changes_bytes(m, *lo, *hi)))
        };
        let (plan, last) = {
            let Session { t, tape, .. } = self;
            let ring = tape.newest_first();
            let goal = match count {
                Some(n) => Goal::Count(n),
                None => Goal::Until(&stop),
            };
            let plan = undo::plan(&ring, cur, &now, goal, &mut |pc| {
                let mut b = [0u8; 2];
                t.read(pc, &mut b) == 2 && undo::is_syscall_bytes(&b)
            });
            match plan {
                Ok(p) => {
                    let last = ring[p.undone - 1].clone();
                    (p, last)
                }
                Err(m) => return fail(format!("{name}: {m}")),
            }
        };
        // every place must be readable before anything is written
        for (addr, bytes) in &plan.writes {
            if self.read_vec(*addr, bytes.len()).len() != bytes.len() {
                return fail(format!(
                    "{name}: {addr:#x} cannot be read any more ({} bytes to put back), so nothing is undone",
                    bytes.len()
                ));
            }
        }
        for (addr, bytes) in &plan.writes {
            self.t.write(*addr, bytes)?;
        }
        self.t.set_regs(cur, &plan.regs)?;
        self.tape.rewind(cur, plan.undone, &plan.regs);
        self.frame = 0;
        let spans: Vec<(u64, usize)> = self.wps.iter().map(|w| (w.addr, w.len as usize)).collect();
        for (i, (addr, len)) in spans.into_iter().enumerate() {
            let bytes = self.read_vec(addr, len);
            self.wps[i].last = bytes;
        }
        self.rec_observe(cur, &plan.regs, false, None);
        let label = self.mods.label(plan.pc_to, false).unwrap_or_default();
        let mut l = vec![format!(
            "undid {} instruction{}: pc {:#x} -> {:#x} {label}; {} memory write{} put back ({} bytes), {} register{} restored",
            plan.undone,
            if plan.undone == 1 { "" } else { "s" },
            plan.pc_from,
            plan.pc_to,
            plan.writes.len(),
            if plan.writes.len() == 1 { "" } else { "s" },
            plan.bytes,
            plan.regs_changed,
            if plan.regs_changed == 1 { "" } else { "s" }
        )];
        if count.is_none() {
            if plan.ran_out {
                l.push(format!(
                    "the record ran out: no enabled breakpoint or watched change in it; this is the oldest state it holds ({} older instructions were dropped)",
                    self.tape.dropped
                ));
            } else if let Some((id, _)) = bps.iter().find(|(_, a)| *a == plan.pc_to) {
                l.push(format!("stopped at breakpoint #{id}"));
            } else if let Some((id, ..)) =
                wps.iter().find(|(_, lo, hi)| last.mem.iter().any(|m| undo::changes_bytes(m, *lo, *hi)))
            {
                l.push(format!("stopped before the write that changed watch #{id}"));
            }
        }
        l.push("never rewound: files, other processes and the network, what the kernel holds; vector and x87 registers are not in the record".into());
        l.push(format!(
            "{} entries stay in the record; `stepi` goes forward and records again, `back` reads what is left",
            self.tape.kept()
        ));
        Ok(rows(
            name,
            l,
            json!({"undone": plan.undone, "from": plan.pc_from, "to": plan.pc_to, "bytes": plan.bytes, "ran_out": plan.ran_out, "kept": self.tape.kept()}),
        ))
    }
}
