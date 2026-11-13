//! Registers: the `status` line, `regs` and `setreg`.

use super::*;

impl Session {
    pub(super) fn status(&mut self) -> Result<Output> {
        let mut l = vec![format!(
            "pid {} {} | {} | {} breakpoints, {} watches | {} stops so far",
            self.t.pid(),
            if self.t.launched() { "started by the debugger" } else { "attached" },
            match self.state {
                State::Stopped => "stopped".to_string(),
                State::Running => "running".to_string(),
                State::Exited => format!("ended ({})", self.exit.clone().unwrap_or_default()),
            },
            self.bps.len(),
            self.wps.len(),
            self.nstop
        )];
        if self.state == State::Stopped {
            let r = self.t.regs(self.cur)?;
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            l.push(format!("thread {} at {:#x} {name}", self.cur, r.rip));
        }
        if let Some(o) = self.new_output_line() {
            l.push(o);
        }
        Ok(rows(
            "status",
            l,
            json!({"state": match self.state { State::Stopped => "stopped", State::Running => "running", State::Exited => "exited" }, "pid": self.t.pid()}),
        ))
    }

    pub(super) fn cmd_regs(&mut self, a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        if let Some(n) = a.pos.first() {
            let Some(v) = r.get(n) else { return usage(format!("no register named {n:?}")) };
            return Ok(rows("regs", vec![format!("{n} = {v:#x} ({})", v as i64)], json!({})));
        }
        let mut l = Vec::new();
        let named = r.named();
        for chunk in named.chunks(4) {
            l.push(chunk.iter().map(|(n, v)| format!("{n:<7}{v:#018x}")).collect::<Vec<_>>().join("  "));
        }
        l.push(format!("flags {}", r.flags_text()));
        let sp = r.rsp();
        let mut stack = Vec::new();
        for i in 0..4u64 {
            let mut b = [0u8; 8];
            if self.t.read(sp + i * 8, &mut b) == 8 {
                stack.push(format!("{:#x}", u64::from_le_bytes(b)));
            }
        }
        if !stack.is_empty() {
            l.push(format!("[rsp] {}", stack.join(" ")));
        }
        Ok(rows("regs", l, json!({"thread": self.cur})))
    }

    pub(super) fn cmd_setreg(&mut self, a: &Args) -> Result<Output> {
        let (Some(n), Some(v)) = (a.pos.first(), a.pos.get(1..)) else {
            return usage("setreg <register> <value or expression>");
        };
        let val = self.addr_of(&v.join(" "))?;
        let mut r = self.tid_regs()?;
        if !r.set(n, val) {
            return usage(format!("{n:?} cannot be set: use a 64-bit register, rip or eflags"));
        }
        self.t.set_regs(self.cur, &r)?;
        Ok(rows("setreg", vec![format!("{n} = {val:#x}")], json!({})))
    }
}
