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

    pub(crate) fn frames(&mut self, max: usize) -> Result<Vec<crate::modules::Frame>> {
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
        // `until *ADDR` is gdb's spelling of an exact place; an expression after the star is the place itself
        let addr = self.addr_of(t.strip_prefix('*').unwrap_or(t))?;
        let w = a.wait()?;
        self.go(Run { step: None, temp: Some(Temp { addr, min_sp: 0, kind: TempKind::Until }) }, w)
    }

    pub(super) fn cmd_kill(&mut self, a: &Args) -> Result<Output> {
        if a.pos.first().is_some_and(|w| w == "call") {
            return self.call_abort();
        }
        self.uninsert_all();
        self.t.kill()?;
        self.state = State::Exited;
        self.exit = Some("killed by the debugger".into());
        let mut l = vec!["program killed".to_string()];
        l.extend(self.call_gone());
        l.extend(self.unhit_lines());
        Ok(rows("kill", l, json!({})))
    }

    /// `run` typed inside a session that has a program: say what is live and what goes on or starts again.
    pub(super) fn cmd_run_live(&mut self) -> Result<Output> {
        let pid = self.t.pid();
        let again = "`end` as its own call closes this session, then `run <path> [args]` starts a new one";
        match self.state {
            State::Exited => {
                fail(format!("this session's program has ended ({}); {again}", self.exit.clone().unwrap_or_default()))
            }
            State::Running => fail(format!(
                "the program is already running (pid {pid}); `wait N` for the next stop or `stop` to stop it; {again}"
            )),
            State::Stopped => {
                let at = match self.t.regs(self.cur) {
                    Ok(r) => {
                        let name = self.mods.label(r.rip, false).unwrap_or_default();
                        format!("stopped at {:#x} {name}", r.rip)
                    }
                    Err(_) => "stopped".to_string(),
                };
                fail(format!(
                    "the program is already running (pid {pid}, {at}); `cont` goes on, `step`, `next`, `finish`, `until <addr>` move it; `kill` ends it; to start it again from the top: {again}"
                ))
            }
        }
    }

    pub(super) fn cmd_detach(&mut self) -> Result<Output> {
        // a call stopped half way would go on in the program without its return breakpoint
        let mut l = Vec::new();
        if self.state == State::Stopped && self.call_under_way().is_some() {
            l.extend(self.call_abort()?.rows.into_iter().map(|r| r.text));
        }
        if self.state == State::Stopped {
            self.uninsert_all();
        }
        self.t.detach()?;
        self.state = State::Exited;
        self.exit = Some("detached".into());
        l.push("the program goes on without the debugger".into());
        Ok(rows("detach", l, json!({})))
    }

    /// Several commands in one call: `do "break f; cont; args; bt"`, or a whole gdb script (one command per line,
    /// `commands ... end` blocks). It stops at the first command that fails, and when the program ends only the
    /// commands that read what is left (`out`, `log`, `history`, `status`, `info`, `echo`, `printf`) still run;
    /// `--keep-going` goes on after a failure.
    pub(super) fn cmd_do(&mut self, words: &[String]) -> Result<Output> {
        let a = Args::new(words)?;
        if a.pos.is_empty() {
            return usage("do \"<command>; <command>; ...\" [--keep-going]");
        }
        let script = a.pos.join(" ");
        let mut out: Vec<String> = Vec::new();
        let parts = crate::gdbdialect::units(&script);
        let mut skipped: Vec<&str> = Vec::new();
        for (n, part) in parts.iter().enumerate() {
            let left = parts[n + 1..].join("; ");
            let w = split_words(part);
            let Some(first) = w.first() else { continue };
            if first == "do" {
                return usage("`do` inside `do` is not allowed");
            }
            let name = crate::gdbdialect::base_name(first);
            // a command that needs the program is left out once it ended; a word nobody knows still answers with its error
            let known = crate::commands::canonical(name).is_some();
            if self.state == State::Exited && self.recording.is_none() && known && !AFTER_EXIT.contains(&name) {
                skipped.push(part);
                continue;
            }
            out.push(format!("> {part}"));
            // a closing `quit` ends nothing here: the session stays for the next call
            if matches!(name, "quit") && n + 1 == parts.len() {
                out.push("quit: the session stays open; `end` closes it".into());
                break;
            }
            match self.exec(&w) {
                Ok(o) => out.extend(o.rows.iter().map(|r| r.text.clone())),
                Err(e) => {
                    out.push(format!("error: {e}"));
                    if !a.has("keep-going") {
                        if !left.is_empty() {
                            out.push(format!("... script stopped at the error; not run: {left}"));
                        }
                        self.recording = None;
                        break;
                    }
                }
            }
            if name == "until" && self.state == State::Exited {
                out.push(format!(
                    "... `{part}` was never reached: the program ended first (a `break` there and `cont` shows whether it runs at all)"
                ));
            }
        }
        if !skipped.is_empty() {
            out.push(format!("... the program ended; not run: {}", skipped.join("; ")));
        }
        if self.recording.is_some() {
            self.recording = None;
            out.push("... the script ended inside `commands`: the block was dropped (close it with `end`)".into());
        }
        Ok(rows("do", out, json!({})))
    }
}

/// What a script may still run after the program ended: the commands that read what was kept.
const AFTER_EXIT: [&str; 10] = ["out", "log", "history", "status", "info", "echo", "printf", "breaks", "help", "show"];
