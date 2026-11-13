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
            l.push(format!(
                "stop {} thread {} {} at {:#x} {name} | rax {:#x} rdi {:#x} rsi {:#x} rsp {:#x}",
                r.n,
                r.tid,
                r.why,
                r.regs.rip,
                r.regs.gp[0],
                r.regs.gp[5],
                r.regs.gp[4],
                r.regs.rsp()
            ));
        }
        if l.is_empty() {
            l.push("no stops yet".into());
        }
        rows("history", l, json!({}))
    }

    pub(super) fn cmd_stdin(&mut self, a: &Args) -> Result<Output> {
        if a.has("close") {
            self.t.stdin(None)?;
            return Ok(rows("stdin", vec!["standard input closed".into()], json!({})));
        }
        if a.pos.is_empty() {
            return usage(
                "stdin <text> (a newline is added; --raw sends exactly the text, \\n \\t escapes work) | stdin --close",
            );
        }
        // --exact: the bytes of a file, no escapes read
        let mut text = a.pos.join(" ");
        if !a.has("exact") {
            text = text.replace("\\n", "\n").replace("\\t", "\t");
        }
        if !a.has("raw") {
            text.push('\n');
        }
        let n = self.t.stdin(Some(text.as_bytes()))?;
        Ok(rows("stdin", vec![format!("sent {n} bytes")], json!({})))
    }
}
