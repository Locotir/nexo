//! How a stop is told: the report, `name`/`note` and the triage of a fault.

use super::*;

/// Registers listed in a stop report; the rest are one `regs` away.
/// Frames a stop report shows; one more asked for tells whether there are others.
const BT_SHOWN: usize = 8;
const CHANGED_SHOWN: usize = 6;

impl Session {
    /// Tell the stop: why, where, the instruction, what changed.
    pub fn stopped(&mut self, tid: Tid, why: Why) -> Output {
        self.nstop += 1;
        self.drain_output();
        let mut l: Vec<String> = Vec::new();
        let headline = match &why {
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
                let mut t = crate::engine::fault_text(self.t.os(), f.number);
                if self.t.os() == Os::Windows && !f.first_chance {
                    t.push_str(", not handled by the program");
                }
                t
            }
            Why::Int3 => "int3 in the program (not ours)".to_string(),
            Why::Interrupt => "stopped on request".to_string(),
            Why::Exec => "the program started another program (exec): addresses are new".to_string(),
            Why::Syscall(t) => format!("system call {t}"),
            Why::Hook(_, t) => format!("hook {t}"),
            Why::Exited(c) => format!("the program exited with code {c}"),
            Why::Killed(s) => format!("the program was killed by {}", signal_name(*s as u32)),
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
            Why::Hook(..) => "hook",
            Why::Exited(_) => "exit",
            Why::Killed(_) => "killed",
        };
        let short = match &why {
            Why::Step | Why::Next => "step".to_string(),
            other => format!("{other:?}").split(['(', ' ']).next().unwrap_or("").to_lowercase(),
        };
        if matches!(why, Why::Exited(_) | Why::Killed(_)) {
            l.push(headline);
            if let Some(o) = self.new_output_line() {
                l.push(o);
            }
            l.extend(self.trace_note());
            self.last_stop = Some(StopInfo { n: self.nstop, event: event.into(), place: "-".into(), addr: 0 });
            self.history.push_back(Rec { n: self.nstop, tid, why: short, regs: crate::Regs::default() });
            return rows("exit", l, json!({"state": "exited"}));
        }
        l.push(format!("{headline} (thread {tid})"));
        if let Ok(r) = self.t.regs(tid) {
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            let at = self.mods.source(r.rip).map(|s| format!("  ({s})")).unwrap_or_default();
            l.push(format!("pc {:#x} {name}{at}", r.rip));
            if let Some(t) = self.mods.source_text(r.rip) {
                l.push(format!("   {t}"));
            }
            self.last_stop = Some(StopInfo {
                n: self.nstop,
                event: event.into(),
                place: if name.is_empty() { "?".into() } else { name.clone() },
                addr: r.rip,
            });
            let bytes = self.read_vec(r.rip, 16);
            let ins: Option<LiveIns> = decode_live(&bytes, r.rip, &|n| r.get(n));
            if let Some(i) = &ins {
                l.push(format!("=> {}", i.text));
                // the next two as well: what the program is about to do, without a `dis` call
                let (mut at, mut len) = (r.rip, i.len as u64);
                for _ in 0..2 {
                    at += len;
                    let b = self.read_vec(at, 16);
                    let Some(n) = decode_live(&b, at, &|_| None) else { break };
                    l.push(format!("   {:#x} {}", at, n.text));
                    len = n.len as u64;
                }
            }
            if let Why::Watch(ids) = &why {
                for id in ids {
                    let Some(i) = self.wps.iter().position(|w| w.id == *id) else { continue };
                    let (addr, len) = (self.wps[i].hw.addr, self.wps[i].hw.len as usize);
                    let now = self.read_vec(addr, len);
                    let old = std::mem::replace(&mut self.wps[i].last, now.clone());
                    self.wps[i].hits += 1;
                    l.push(format!("watch #{id} {addr:#x}: {} -> {}", hex(&old), hex(&now)));
                }
            }
            if let Why::Signal(f) = &why {
                l.extend(self.triage(f, &r, ins.as_ref()));
            }
            if matches!(why, Why::Signal(_) | Why::Break(_) | Why::Until | Why::Finish) {
                l.extend(self.locals_line());
            }
            let changed = match self.prev.get(&tid) {
                Some(p) => r.changed(p),
                None => Vec::new(),
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
        if !self.log.is_empty() {
            l.push(format!("log: {} lines (`log` shows them)", self.log.len()));
        }
        l.extend(self.trace_note());
        rows("stop", l, json!({"state": "stopped", "thread": tid}))
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
                (mine(self, pc)?, w.join(" "))
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

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "?".into();
    }
    b.iter().rev().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}
