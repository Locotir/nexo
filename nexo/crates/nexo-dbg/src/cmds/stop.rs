//! How a stop is told: the report, `name`/`note` and the triage of a fault.

use super::*;

/// Registers listed in a stop report; the rest are one `regs` away.
/// Frames a stop report shows; one more asked for tells whether there are others.
const BT_SHOWN: usize = 8;
const CHANGED_SHOWN: usize = 6;
/// Lines of logpoint and `commands` output a stop report carries; the rest stay in `log`.
const FRESH_LOG: usize = 40;

impl Session {
    /// For a stop in the program's own code: its path and the file address of the pc, so the answer can name the
    /// `bin` call that decompiles the function. `None` at the entry, outside the program, or while stepping in a
    /// function that already got the line.
    pub fn bridge(&mut self) -> Option<(String, u64)> {
        let s =
            self.last_stop.as_ref().filter(|s| !matches!(s.event.as_str(), "entry" | "exec" | "exit" | "killed"))?;
        let file_addr = self.mods.in_main(s.addr)?;
        let key = match s.place.split('+').next().unwrap_or("") {
            "?" | "" => format!("?{:x}", file_addr >> 12),
            f => f.to_string(),
        };
        if self.bridged == key {
            return None;
        }
        self.bridged = key;
        Some((self.mods.exe_path().display().to_string(), file_addr))
    }

    /// Tell the stop: why, where, the instruction, what changed.
    pub fn stopped(&mut self, tid: Tid, why: Why) -> Output {
        self.nstop += 1;
        self.drain_output();
        let mut l: Vec<String> = Vec::new();
        // macOS: the entry stop sits on dyld's call of main (engine.rs back_to_the_call_of)
        let in_dyld =
            self.t.os() == Os::MacOS && self.t.entry().is_some_and(|e| self.t.regs(tid).is_ok_and(|r| r.rip != e));
        let headline = match &why {
            Why::Entry if in_dyld => {
                "stopped at the program's entry: dyld's call of main, after it loaded the libraries".to_string()
            }
            Why::Entry => "stopped at the program's entry".to_string(),
            Why::Step => "step".to_string(),
            Why::Next => "next".to_string(),
            Why::Finish => "finish: returned".to_string(),
            Why::Until => "reached the address".to_string(),
            Why::Break(ids) => {
                format!("breakpoint {}", ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" "))
            }
            Why::Watch(ids) => format!("watch {}", ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" ")),
            Why::Signal(f) => {
                let mut t = crate::engine::fault_line(self.t.os(), f);
                if self.t.os() == Os::Windows && !f.first_chance {
                    t.push_str(", not handled by the program");
                }
                t
            }
            Why::Int3 => "int3 in the program (not ours)".to_string(),
            Why::Interrupt => "stopped on request".to_string(),
            Why::Exec => "the program started another program (exec): addresses are new".to_string(),
            Why::Syscall(t) => format!("system call {t}"),
            Why::Load(t) => t.clone(),
            Why::Hook(_, t) => format!("hook {t}"),
            Why::Heap(t) => t.clone(),
            Why::Returned => "a call returned".to_string(),
            Why::Fork(t) | Why::LetGo(t) | Why::Restart(t) => t.clone(),
            Why::Exited(c) => format!("the program exited with code {c}"),
            Why::Killed(s) => {
                format!("the program was killed by {}", crate::engine::signal_name_on(self.t.os(), *s as u32))
            }
        };
        let event = match &why {
            Why::Entry => "entry",
            Why::Step => "step",
            Why::Next => "next",
            Why::Finish => "finish",
            Why::Until => "until",
            Why::Break(_) => "breakpoint",
            Why::Watch(_) => "watch",
            Why::Signal(_) => "signal",
            Why::Int3 => "int3",
            Why::Interrupt => "interrupt",
            Why::Exec => "exec",
            Why::Syscall(_) => "syscall",
            Why::Load(_) => "load",
            Why::Hook(..) => "hook",
            Why::Heap(_) => "heap",
            Why::Returned => "call",
            Why::Fork(_) | Why::LetGo(_) => "fork",
            Why::Restart(_) => "restart",
            Why::Exited(_) => "exit",
            Why::Killed(_) => "killed",
        };
        let short = match &why {
            Why::Step | Why::Next => "step".to_string(),
            other => format!("{other:?}").split(['(', ' ']).next().unwrap_or("").to_lowercase(),
        };
        if matches!(why, Why::Exited(_) | Why::Killed(_) | Why::LetGo(_)) {
            l.push(headline);
            l.extend(self.call_gone());
            if !matches!(why, Why::LetGo(_)) {
                l.extend(self.unhit_lines());
            }
            if let Some(o) = self.new_output_line() {
                l.push(o);
            }
            l.extend(self.fresh_log());
            l.extend(self.tape.take_notes());
            l.extend(self.heap_exit_note());
            l.extend(self.trace_note());
            self.last_stop = Some(StopInfo { n: self.nstop, event: event.into(), place: "-".into(), addr: 0 });
            self.history.push_back(Rec { n: self.nstop, tid, why: short, regs: crate::Regs::default() });
            return rows("exit", l, json!({"state": "exited"}));
        }
        // a stop always selects the frame it stopped in: `up` and `frame N` read its callers
        self.frame = 0;
        let compact = super::quiet::compact_applies(self.compact_stops, &why);
        let mut cplace = String::new();
        let mut cret: Option<String> = None;
        if !compact {
            l.push(format!("{headline} (thread {tid}, frame #0)"));
        }
        if why == Why::Exec && !compact {
            let mods: Vec<String> = self.mods.list.iter().map(|m| format!("{} at {:#x}", m.path, m.base)).collect();
            l.push(format!("modules now: {}", mods.join(", ")));
        }
        l.extend(self.call_note());
        if let Ok(r) = self.t.regs(tid) {
            self.rec_observe(tid, &r, matches!(why, Why::Step), None);
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            let src = self.mods.source(r.rip);
            let at = src.as_ref().map(|s| format!("  ({s})")).unwrap_or_default();
            if compact {
                cplace = super::quiet::compact_place(r.rip, &name, src.as_deref());
            } else {
                l.push(format!("pc {:#x} {name}{at}", r.rip));
                if let Some(t) = self.mods.source_text(r.rip) {
                    l.push(format!("   {t}"));
                }
            }
            if why == Why::Finish {
                if let Some(from) = self.finish_from.take() {
                    let got = self.returned_line(from, &r, tid);
                    if compact {
                        cret = got;
                    } else {
                        l.extend(got);
                    }
                }
            }
            self.last_stop = Some(StopInfo {
                n: self.nstop,
                event: event.into(),
                place: if name.is_empty() { "?".into() } else { name.clone() },
                addr: r.rip,
            });
            let bytes = if compact { Vec::new() } else { self.read_vec(r.rip, 16) };
            let ins: Option<LiveIns> =
                if compact { None } else { decode_live_in(self.bits(), &bytes, r.rip, &|n| r.get(n)) };
            if let Some(i) = &ins {
                l.push(format!("=> {}", i.text));
                // the next two as well: what the program is about to do, without a `dis` call
                let (mut at, mut len) = (r.rip, i.len as u64);
                for _ in 0..2 {
                    at += len;
                    let b = self.read_vec(at, 16);
                    let Some(n) = decode_live_in(self.bits(), &b, at, &|_| None) else { break };
                    l.push(format!("   {:#x} {}", at, n.text));
                    len = n.len as u64;
                }
            }
            if matches!(why, Why::Watch(_)) && !compact {
                // written at the hit, when the bytes before and after and the instruction were known
                l.append(&mut self.watch_lines);
            }
            if let Why::Signal(f) = &why {
                l.extend(self.triage(f, &r, ins.as_ref()));
            }
            if !compact && matches!(why, Why::Signal(_) | Why::Break(_) | Why::Until | Why::Finish) {
                l.extend(self.locals_line());
            }
            let changed = match self.prev.get(&tid) {
                Some(p) if !compact => r.changed(p),
                _ => Vec::new(),
            };
            if !changed.is_empty() {
                let more = changed.len().saturating_sub(CHANGED_SHOWN);
                let mut line = format!(
                    "changed: {}",
                    changed
                        .iter()
                        .take(CHANGED_SHOWN)
                        .map(|(n, o, v)| format!("{n} {o:#x} -> {v:#x}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if more > 0 {
                    line.push_str(&format!(" (+{more} more: `regs`)"));
                }
                l.push(line);
            }
            self.prev.insert(tid, r);
            if let Why::Signal(_) = &why {
                if let Ok(f) = self.frames(BT_SHOWN + 1) {
                    l.push("backtrace:".into());
                    let fl = self.frame_lines(&f[..f.len().min(BT_SHOWN)]);
                    l.extend(fl.into_iter().map(|s| format!("  {s}")));
                    if f.len() > BT_SHOWN {
                        l.push("  ... more frames (`bt 50`)".into());
                    }
                }
            }
            self.history.push_back(Rec { n: self.nstop, tid, why: short, regs: r });
            while self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        if let Some(o) = self.new_output_line() {
            l.push(o);
        }
        l.extend(self.fresh_log());
        if compact {
            let mut why_text = super::quiet::compact_why(&why);
            if matches!(why, Why::Watch(_)) && !self.watch_lines.is_empty() {
                why_text = self.watch_lines.join("; ");
            }
            let mut rest: Vec<String> = cret.into_iter().collect();
            rest.extend(self.display_pieces());
            l.insert(0, super::quiet::compact_line(&why_text, &cplace, &rest));
            if !self.compact_told {
                self.compact_told = true;
                l.insert(1, "(compact stops: `stops full` brings the detail back)".to_string());
            }
        } else {
            l.extend(self.display_lines());
        }
        l.extend(self.tape.take_notes());
        l.extend(self.trace_note());
        rows("stop", l, json!({"state": "stopped", "thread": tid}))
    }

    /// The logpoint and `commands` output written since the last report, so the answer carries it without a `log`
    /// call; past `FRESH_LOG` lines the oldest stay in `log`.
    pub(crate) fn fresh_log(&mut self) -> Vec<String> {
        let new = (self.log_total - self.log_shown) as usize;
        self.log_shown = self.log_total;
        if new == 0 {
            return Vec::new();
        }
        let have = self.log.len();
        let take = new.min(have);
        let shown = take.min(FRESH_LOG);
        // the newest lines: what the program did last, and why it stopped, are at the end
        let mut l: Vec<String> = Vec::new();
        if new > shown {
            l.push(format!("log: {} earlier lines (`log` lists the last {have})", new - shown));
        }
        l.extend(self.log.iter().skip(have - shown).map(|s| format!("log: {s}")));
        l
    }

    /// `name <addr> <name>` and `note [<0xaddr>] <text>`: keep a finding for this binary. A note without an
    /// address is for the place the program stopped at. Answers with one `saved:` line.
    pub(super) fn cmd_keep(&mut self, is_name: bool, w: &[String]) -> Result<Output> {
        let Some(dir) = crate::knowledge::data_dir() else {
            return fail("nothing can be kept here: this host has no data folder for names and notes");
        };
        let mine = |s: &mut Session, a: u64| -> Result<u64> {
            let (base, bias) = (s.mods.main_base(), s.mods.main_bias());
            if base == 0 || s.mods.locate(a) != s.mods.locate(base) {
                return fail(format!(
                    "{a:#x} is not inside the program itself; only its own code and data can be named"
                ));
            }
            Ok(a.wrapping_sub(bias))
        };
        let (addr, text) = if is_name {
            if w.len() != 2 {
                return usage("usage: name <addr> <name>   (a name for a function or data; letters, digits and _ . : $ @ < > ~ -)");
            }
            let a = self.addr_of(&w[0])?;
            (mine(self, a)?, w[1].clone())
        } else {
            let explicit = w.len() > 1 && w[0].starts_with("0x");
            if explicit {
                let a = self.addr_of(&w[0])?;
                (mine(self, a)?, w[1..].join(" "))
            } else {
                if w.is_empty() {
                    return usage(
                        "usage: note [0xaddr] <text>   (without an address the note is for the current place)",
                    );
                }
                let pc = self.tid_regs()?.rip;
                // macOS stops at the entry on dyld's call of main, outside the program: there the place is main
                let on_dyld_call = self.t.os() == Os::MacOS
                    && self.last_stop.as_ref().is_some_and(|s| s.event == "entry" && s.addr == pc);
                let at = match self.t.entry() {
                    Some(main) if on_dyld_call => main,
                    _ => pc,
                };
                (mine(self, at)?, w.join(" "))
            }
        };
        let target = format!("{addr:#x}");
        let img = self.mods.main_image();
        let r = if is_name {
            nexo_bin::name_set(img, &dir, &target, &text)
        } else {
            nexo_bin::note_set(img, &dir, &target, &text)
        };
        let o = r.map_err(|e| crate::Error::Fail(e.to_string()))?;
        if let Some(why) = self.mods.reload_names(&dir) {
            return fail(why);
        }
        let line = o.rows.first().map(|r| r.text.clone()).unwrap_or_default();
        let line = format!("saved: {line} (kept for this binary)");
        Ok(rows(if is_name { "name" } else { "note" }, vec![line], json!({"addr": addr})))
    }

    /// The likely cause of a fault, from the signal, the address and the instruction that did it.
    fn triage(&mut self, f: &Fault, r: &crate::Regs, ins: Option<&LiveIns>) -> Vec<String> {
        let mut l = Vec::new();
        let sp = r.rsp();
        let win = self.t.os() == Os::Windows;
        // Linux signals and Windows exceptions folded into one number: 11 is a bad memory access,
        // 1000 + the low byte of the code is any other Windows exception
        let number = if win && f.number == 0xC000_0005 {
            11
        } else if win {
            1000 + (f.number & 0xff)
        } else if self.t.os() == Os::MacOS {
            // macOS SIGBUS (10) is Linux 7; its own 7 is SIGEMT, which says nothing about memory
            match f.number {
                10 => 7,
                7 => 0,
                n => n,
            }
        } else {
            f.number
        };
        match number {
            11 | 7 => {
                let addr = f.addr.unwrap_or(0);
                let what = if win || f.code == 0 {
                    "bad access"
                } else if f.code == 1 {
                    "address not mapped"
                } else if f.code == 2 {
                    "mapped but not allowed"
                } else {
                    "bad access"
                };
                let verb = match f.touch {
                    Some(crate::platform::Touch::Read) => " while reading",
                    Some(crate::platform::Touch::Write) => " while writing",
                    Some(crate::platform::Touch::Exec) => " while running code there",
                    None => "",
                };
                let kind = if addr < 0x1000 {
                    format!("null pointer (offset {addr:#x} into a null object)")
                } else if addr.abs_diff(sp) < 0x10000 {
                    "next to the stack pointer: stack overflow or a corrupted stack".to_string()
                } else if addr == r.rip {
                    "jumped to an address that holds no code (a bad function pointer or a smashed return address)".to_string()
                } else if addr > 0x0000_8000_0000_0000 {
                    "not a valid user address (an uninitialised or corrupted pointer)".to_string()
                } else {
                    self.mods.label(addr, true).map_or(what.to_string(), |n| format!("{what}; near {n}"))
                };
                l.push(format!("fault address {addr:#x}{verb}: {kind}"));
                if let Some(i) = ins {
                    for m in &i.mem {
                        if let Some(a) = m.addr {
                            l.push(format!(
                                "the instruction {} {} byte(s) at {a:#x}",
                                if m.write { "writes" } else { "reads" },
                                m.size
                            ));
                        }
                    }
                }
            }
            // Windows exceptions, by the low byte of the code
            1253 => l.push("stack overflow: the stack is used up (endless recursion or a very large local); the frames below show the repeating call".into()),
            1148 | 1149 | 1142 => l.push("arithmetic fault: an integer division by zero or an overflow in the instruction above".into()),
            1029 => l.push("illegal instruction: the code is damaged or the jump went into data".into()),
            1009 => l.push(format!(
                "fast-fail: the program stopped itself on purpose (abort, a failed security check or a detected stack smash); reason code {:#x} (rcx)",
                r.get("rcx").unwrap_or(0)
            )),
            1116 => l.push("heap corruption: a block was overrun or freed twice; the frames below show who noticed".into()),
            6 => l.push("abort():the program stopped itself (an assert, a panic, a failed check, or a corrupted heap): read the frames below".into()),
            8 => l.push("arithmetic fault: an integer division by zero or an overflow in the instruction above".into()),
            4 => l.push("illegal instruction: the code is damaged or the jump went into data".into()),
            _ => {}
        }
        l
    }
}
