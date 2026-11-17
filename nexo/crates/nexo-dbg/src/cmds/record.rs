//! `record` and `back`: a bounded record of the instructions that ran, and a way to read it. The record itself
//! (the ring and its filters) is in `record.rs`; this file is the commands and the hooks the run calls.
//! It looks back, it does not undo: the program is never moved.

use super::*;
use crate::record::{Entry, DEFAULT_KEEP, MAX_KEEP, MAX_WRITE};
use crate::regs::Regs;

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
            let Some(ins) = decode_live_in(self.bits(), &bytes, r.rip, &|n| r.get(n)) else { return };
            let mut w = Vec::new();
            for m in ins.mem.iter().filter(|m| m.write) {
                let Some(a) = m.addr else { continue };
                if m.size == 0 || m.size > MAX_WRITE {
                    continue;
                }
                let before = self.read_vec(a, m.size);
                if before.len() == m.size {
                    w.push((a, before));
                }
            }
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
                Ok(v) if v >= 1 && v <= MAX_KEEP as u64 => v as usize,
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
            "{title}; {} kept, {} dropped; this looks back, it does not undo",
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
            for m in e.mem.iter().filter(|m| want_mem.map_or(true, |a| m.covers(a))) {
                row.push_str(&format!(" | mem {:#x}: {} -> {}", m.addr, hex(&m.before), hex(&m.after)));
            }
            l.push(row);
        }
        Ok(rows("back", l, json!({"kept": self.tape.kept(), "dropped": self.tape.dropped, "shown": picked.len()})))
    }
}
