//! The output of the program, the command history and its input.

use super::*;

impl Session {
    pub(super) fn cmd_out(&mut self, a: &Args) -> Output {
        self.drain_output();
        let from = if a.has("all") { 0 } else { self.out_seen.min(self.out.len()) };
        let text = String::from_utf8_lossy(&self.out[from..]).to_string();
        self.out_seen = self.out.len();
        let mut l: Vec<String> = text.lines().map(str::to_string).collect();
        if self.out_dropped > 0 && a.has("all") {
            l.insert(0, format!("... {} older bytes were dropped (the last 1 MiB is kept)", self.out_dropped));
        }
        if l.is_empty() {
            l.push("no new output".into());
        }
        rows("out", l, json!({}))
    }

    pub(super) fn cmd_history(&mut self, a: &Args) -> Output {
        let n = a.pos.first().and_then(|x| x.parse::<usize>().ok()).unwrap_or(10);
        let skip = self.history.len().saturating_sub(n);
        let mut l = Vec::new();
        for r in self.history.iter().skip(skip) {
            let name = self.mods.label(r.regs.rip, false).unwrap_or_default();
            let g = &r.regs.gp;
            // a 32-bit thread: its arguments are on the stack, not in rdi and rsi
            let regs = if r.regs.x32 {
                format!("eax {:#x} ecx {:#x} edx {:#x} esp {:#x}", g[0], g[2], g[3], r.regs.rsp())
            } else {
                format!("rax {:#x} rdi {:#x} rsi {:#x} rsp {:#x}", g[0], g[5], g[4], r.regs.rsp())
            };
            l.push(format!("stop {} thread {} {} at {:#x} {name} | {regs}", r.n, r.tid, r.why, r.regs.rip));
        }
        if l.is_empty() {
            l.push("no stops yet".into());
        }
        rows("history", l, json!({}))
    }
}
