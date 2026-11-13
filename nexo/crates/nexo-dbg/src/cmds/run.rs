//! Moving the program: stepping, finishing, killing, detaching and scripts.

use super::*;

impl Session {
    pub(super) fn cmd_step(&mut self, a: &Args, over: bool) -> Result<Output> {
        let n = match a.pos.first() {
            Some(x) => parse_addr(x).map_err(|_| crate::Error::Usage("step needs a count".into()))?.clamp(1, 100_000),
            None => 1,
        };
        let w = a.wait()?;
        let mut last = None;
        for i in 0..n {
            self.need_stopped()?;
            let run = if over { self.next_run()? } else { Run { step: Some(self.cur), temp: None } };
            let out = self.go(run, w)?;
            let again = matches!(self.history.back(), Some(r) if r.why == "step") && self.state == State::Stopped;
            last = Some(out);
            if !again || i + 1 == n {
                break;
            }
        }
        Ok(last.unwrap_or_else(|| rows("step", vec![], json!({}))))
    }

    /// A step that does not go into calls: a temporary breakpoint after the call instruction.
    fn next_run(&mut self) -> Result<Run> {
        let r = self.t.regs(self.cur)?;
        let bytes = self.read_vec(r.rip, 16);
        let ins = decode_live(&bytes, r.rip, &|n| r.get(n));
        match ins {
            Some(i) if i.call => Ok(Run {
                step: None,
                temp: Some(Temp { addr: r.rip + i.len as u64, min_sp: r.rsp(), kind: TempKind::Next }),
            }),
            _ => Ok(Run { step: Some(self.cur), temp: None }),
        }
    }

    pub(super) fn frames(&mut self, max: usize) -> Result<Vec<crate::modules::Frame>> {
        let r = self.tid_regs()?;
        // a library may have come in since the last look: the chain goes through it
        self.mods.refresh(&mut *self.t);
        let Session { t, mods, .. } = self;
        let mut rd = |a: u64| {
            let mut b = [0u8; 8];
            (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
        };
        Ok(mods.backtrace(&r, &mut rd, max))
    }

    pub(super) fn cmd_finish(&mut self, a: &Args) -> Result<Output> {
        let f = self.frames(3)?;
        let Some(caller) = f.get(1) else {
            return fail("cannot find the caller of this frame (no unwind data): use `until <address>`");
        };
        let w = a.wait()?;
        self.go(Run { step: None, temp: Some(Temp { addr: caller.pc, min_sp: caller.sp, kind: TempKind::Finish }) }, w)
    }

    pub(super) fn cmd_until(&mut self, a: &Args) -> Result<Output> {
        let Some(t) = a.pos.first() else { return usage("until <address|name>") };
        let addr = self.addr_of(t)?;
        let w = a.wait()?;
        self.go(Run { step: None, temp: Some(Temp { addr, min_sp: 0, kind: TempKind::Until }) }, w)
    }

    pub(super) fn cmd_kill(&mut self) -> Result<Output> {
        self.uninsert_all();
        self.t.kill()?;
        self.state = State::Exited;
        self.exit = Some("killed by the debugger".into());
        Ok(rows("kill", vec!["program killed".into()], json!({})))
    }

    pub(super) fn cmd_detach(&mut self) -> Result<Output> {
        if self.state == State::Stopped {
            self.uninsert_all();
        }
        self.t.detach()?;
        self.state = State::Exited;
        self.exit = Some("detached".into());
        Ok(rows("detach", vec!["the program goes on without the debugger".into()], json!({})))
    }

    /// Several commands in one call: `do "break f; cont; args; bt"`. It stops at the first command that fails,
    /// and when the program ends; `--keep-going` goes on after a failure.
    pub(super) fn cmd_do(&mut self, words: &[String]) -> Result<Output> {
        let a = Args::new(words)?;
        if a.pos.is_empty() {
            return usage("do \"<command>; <command>; ...\" [--keep-going]");
        }
        let script = a.pos.join(" ");
        let mut out: Vec<String> = Vec::new();
        let parts: Vec<&str> = script.split(';').map(str::trim).filter(|p| !p.is_empty()).collect();
        for (n, &part) in parts.iter().enumerate() {
            let left = parts[n + 1..].join("; ");
            let w = split_words(part);
            if w.first().is_some_and(|c| c == "do") {
                return usage("`do` inside `do` is not allowed");
            }
            out.push(format!("> {part}"));
            match self.exec(&w) {
                Ok(o) => out.extend(o.rows.iter().map(|r| r.text.clone())),
                Err(e) => {
                    out.push(format!("error: {e}"));
                    if !a.has("keep-going") {
                        if !left.is_empty() {
                            out.push(format!("... script stopped at the error; not run: {left}"));
                        }
                        break;
                    }
                }
            }
            if self.state == State::Exited && !matches!(w[0].as_str(), "out" | "log" | "history" | "status") {
                if w[0] == "until" {
                    out.push(format!(
                        "... `{part}` was never reached: the program ended first (a `break` there and `cont` shows whether it runs at all)"
                    ));
                }
                if !left.is_empty() {
                    out.push(format!("... script stopped: the program ended; not run: {left}"));
                }
                break;
            }
        }
        Ok(rows("do", out, json!({})))
    }
}
