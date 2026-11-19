//! What the target reports, turned into stops: breakpoint hits, forks, exec, the log lines.

use super::*;

impl Session {
    pub(super) fn process(&mut self, events: Vec<Event>) -> Result<Option<(Tid, Why)>> {
        let mut verdict: Option<(Tid, Why)> = None;
        // a thread that ran into one of our breakpoints is put back before the instruction
        let mut bp_hits: Vec<(Tid, u64)> = Vec::new();
        // debug register hits, faults in guarded pages and the steps of a step watch
        let mut watch_evs: Vec<(Tid, Stop)> = Vec::new();
        for ev in &events {
            if let Event::Stopped { tid, stop } = *ev {
                let stepped = self.step_from.is_some_and(|(t, _)| t == tid) && matches!(stop, Stop::Step);
                if stepped && self.tape.on {
                    let from = self.step_from.map(|(_, pc)| pc);
                    if let Ok(r) = self.t.regs(tid) {
                        self.rec_observe(tid, &r, true, from);
                    }
                }
                if stepped || matches!(stop, Stop::Watch(_)) || matches!(stop, Stop::Fault(f) if self.guard_fault(&f)) {
                    watch_evs.push((tid, stop));
                }
            }
            match *ev {
                Event::Exited { code } => {
                    self.ended(format!("exited with code {code}"));
                    return Ok(Some((self.cur, Why::Exited(code))));
                }
                Event::Killed { signal } => {
                    self.ended(format!("killed by {}", signal_name_on(self.t.os(), signal as u32)));
                    return Ok(Some((self.cur, Why::Killed(signal))));
                }
                Event::Stopped { tid, stop: Stop::Breakpoint } => {
                    let mut r = self.t.regs(tid)?;
                    let a = r.rip.wrapping_sub(1);
                    if self.inserted.contains_key(&a) {
                        r.rip = a;
                        self.t.set_regs(tid, &r)?;
                        bp_hits.push((tid, a));
                    } else if verdict.is_none() {
                        verdict = Some((tid, Why::Int3));
                    }
                }
                Event::Stopped { tid, stop: Stop::Syscall(sys) } => {
                    let v = self.on_syscall(tid, sys);
                    if verdict.is_none() {
                        verdict = v;
                    }
                }
                Event::Stopped { stop: Stop::Exec, .. } => self.on_exec(),
                Event::Stopped { tid, stop: Stop::Load } => {
                    let v = self.on_load(tid);
                    if verdict.is_none() {
                        verdict = v;
                    }
                }
                _ => {}
            }
        }
        for (tid, a) in bp_hits {
            let v = self.hit(tid, a)?;
            // a breakpoint that served once (a call's return) goes: the thread runs the program's byte
            if !self.wanted().contains(&a) {
                if let Some(b) = self.inserted.remove(&a) {
                    self.t.write(a, &[b])?;
                    let patches: Vec<(u64, u8)> = self.inserted.iter().map(|(a, b)| (*a, *b)).collect();
                    self.t.set_patches(&patches);
                }
            }
            if verdict.is_none() {
                verdict = v;
            }
        }
        let taken: Vec<(Tid, Stop)> = watch_evs.clone();
        if !watch_evs.is_empty() {
            let v = self.watch_events(watch_evs)?;
            if verdict.is_none() {
                verdict = v;
            }
        }
        for ev in &events {
            if verdict.is_some() {
                break;
            }
            if let Event::Stopped { tid, stop } = *ev {
                if matches!(stop, Stop::Fault(_)) && taken.contains(&(tid, stop)) {
                    continue;
                }
                match stop {
                    Stop::Step | Stop::Watch(_) if self.run.step.is_some() => {
                        // a single step that lands on the return address of a watched frame: no breakpoint was hit
                        // there, so the watch ends here, and the stop stays the step's
                        if self.wps.iter().any(|w| w.scope.is_some()) {
                            let at = self.t.regs(tid)?.rip;
                            if self.scope_ended(tid, at)?.is_some() {
                                self.watch_lines_to_log();
                            }
                        }
                        verdict = Some((tid, Why::Step));
                    }
                    Stop::Fault(f) => {
                        if self.catch.contains(&f.number) || (self.t.os() == Os::Windows && !f.first_chance) {
                            verdict = Some((tid, Why::Signal(f)));
                        }
                    }
                    Stop::Interrupt => verdict = Some((tid, Why::Interrupt)),
                    Stop::Exec => verdict = Some((tid, Why::Exec)),
                    Stop::Fork(pid) => verdict = Some((tid, Why::LetGo(self.let_go_text(pid)))),
                    _ => {}
                }
            }
        }
        Ok(verdict)
    }

    pub(super) fn ended(&mut self, how: String) {
        self.state = State::Exited;
        self.exit = Some(how);
        self.inserted.clear();
        self.drain_output();
        self.hooks_end();
        self.trace_end();
        self.drop_checkpoints();
    }

    /// A thread is at one of our breakpoints: does it stop?
    fn hit(&mut self, tid: Tid, addr: u64) -> Result<Option<(Tid, Why)>> {
        if self.call_returned(tid, addr)? {
            return Ok(Some((tid, Why::Returned)));
        }
        // a watch on a local ends here; a user breakpoint or a `finish` at the same address still gets its own stop,
        // and the line about the watch goes to the log then
        let scoped = self.scope_ended(tid, addr)?;
        // the loader's hook: libraries came or went, which may set breakpoints before the ones at this address
        let loaded = if self.libs.armed == Some(addr) { self.on_load(tid) } else { None };
        // the trace and the hooks see every pass, whatever a breakpoint there decides
        let traced = self.hook_hit(tid, addr)?.or(loaded);
        let mut stop_ids: Vec<u32> = Vec::new();
        let mut spent: Vec<u32> = Vec::new();
        let ids: Vec<u32> = self.bps.iter().filter(|b| b.enabled && b.addr == addr).map(|b| b.id).collect();
        for id in ids {
            let Some(i) = self.bps.iter().position(|b| b.id == id) else { continue };
            if let Some(c) = self.bps[i].cond.clone() {
                let tid_regs = self.ev(tid);
                let truth = match tid_regs {
                    Ok(mut ev) => expr::value(&c, &mut ev),
                    Err(e) => Err(e),
                };
                match truth {
                    Ok(0) => continue,
                    Ok(_) => {}
                    // a condition that cannot be worked out stops, and says so
                    Err(e) => {
                        self.log_line(format!("breakpoint #{id}: condition {c:?} failed: {e}"));
                    }
                }
            }
            self.bps[i].hits += 1;
            if self.bps[i].hits <= self.bps[i].after {
                continue;
            }
            if self.bps[i].call {
                let line = self.call_line(tid, addr);
                self.log_line(line);
                continue;
            }
            if let Some(text) = self.bps[i].log.clone() {
                let line = self.format_log(tid, &text);
                self.log_line(format!("#{id} {line}"));
                continue;
            }
            if !self.bps[i].actions.is_empty() {
                let go_on = self.run_actions(tid, id);
                let hits = self.bps[i].hits;
                if go_on && hits < ACTION_HITS {
                    if self.bps[i].once {
                        spent.push(id);
                    }
                    continue;
                }
                if go_on {
                    self.log_line(format!(
                        "#{id} stops after {ACTION_HITS} hits so the answer comes back (`cont` goes on, the actions keep running)"
                    ));
                    self.bps[i].hits = 0;
                }
            }
            stop_ids.push(id);
        }
        self.bps.retain(|b| !spent.contains(&b.id));
        if !stop_ids.is_empty() {
            self.bps.retain(|b| !(b.once && stop_ids.contains(&b.id)));
            if scoped.is_some() {
                self.watch_lines_to_log();
            }
            return Ok(Some((tid, Why::Break(stop_ids))));
        }
        if let Some(t) = self.run.temp {
            if t.addr == addr {
                let sp = self.t.regs(tid)?.rsp();
                if sp >= t.min_sp {
                    if scoped.is_some() {
                        self.watch_lines_to_log();
                    }
                    let why = match t.kind {
                        TempKind::Entry if self.t.os() == Os::MacOS => {
                            self.back_to_the_call_of(tid, addr);
                            Why::Entry
                        }
                        TempKind::Entry => Why::Entry,
                        TempKind::Next => Why::Next,
                        TempKind::Finish => Why::Finish,
                        TempKind::Until => Why::Until,
                    };
                    return Ok(Some((tid, why)));
                }
            }
        }
        Ok(scoped.map(|why| (tid, why)).or(traced))
    }

    /// The lines a watch stop would show go to the log instead: the stop is another one.
    pub(crate) fn watch_lines_to_log(&mut self) {
        for l in std::mem::take(&mut self.watch_lines) {
            self.log_line(l);
        }
    }

    /// macOS: a program's entry (LC_MAIN) is its `main`, which dyld calls once the libraries are loaded and their
    /// initializers ran. The entry stop goes back to the instruction in dyld that calls it (rip on the call, the
    /// return address off the stack), so a breakpoint set there on `main` is hit by the next `cont`, as after the
    /// entry stop on Linux. Left at `main` when no call there goes to it.
    fn back_to_the_call_of(&mut self, tid: Tid, main: u64) {
        let Ok(mut r) = self.t.regs(tid) else { return };
        let Ok(back) = <[u8; 8]>::try_from(self.read_vec(r.rsp(), 8)) else { return };
        let back = u64::from_le_bytes(back);
        let word = |s: &mut Self, a: u64| <[u8; 8]>::try_from(s.read_vec(a, 8)).ok().map(u64::from_le_bytes);
        for len in 2..=7u64 {
            let at = back.wrapping_sub(len);
            let mut bytes = self.read_vec(at, len as usize);
            for (i, b) in bytes.iter_mut().enumerate() {
                if let Some(o) = self.inserted.get(&(at + i as u64)) {
                    *b = *o;
                }
            }
            let Some(ins) = nexo_bin::decode_live(&bytes, at, &|n| r.get(n)) else { continue };
            if !ins.call || ins.len as u64 != len {
                continue;
            }
            let goes = match ins.target {
                Some((t, false)) => Some(t),
                Some((slot, true)) => word(self, slot),
                None => nexo_bin::indirect_call_target(&bytes, at, &|n| r.get(n), &|_| None).or_else(|| {
                    let slot = ins.mem.iter().find(|m| m.read && !m.write)?.addr?;
                    word(self, slot)
                }),
            };
            if goes == Some(main) {
                r.rip = at;
                r.set("rsp", r.rsp() + 8);
                let _ = self.t.set_regs(tid, &r);
                return;
            }
        }
    }

    /// The byte our own int3 replaced at `addr`, if one sits there.
    pub(crate) fn inserted_byte(&self, addr: u64) -> Option<u8> {
        self.inserted.get(&addr).copied()
    }

    /// `call 0x401136 main+0x20 -> 0x401220 helper`: the real target of the indirect call at `addr` now.
    fn call_line(&mut self, tid: Tid, addr: u64) -> String {
        let from = self.mods.label(addr, false).unwrap_or_default();
        let mut bytes = self.read_vec(addr, 16);
        // our own int3 sits on the first byte while the program runs
        if let (Some(b), Some(o)) = (bytes.first_mut(), self.inserted.get(&addr)) {
            *b = *o;
        }
        let regs = self.t.regs(tid).ok();
        let (bits, n) = (self.bits(), self.ptr_size());
        let to = regs.and_then(|r| {
            let t = std::cell::RefCell::new(&mut self.t);
            nexo_bin::indirect_call_target_in(bits, &bytes, addr, &|n| r.get(n), &|a| {
                let mut b = [0u8; 8];
                (t.borrow_mut().read(a, &mut b[..n]) == n).then(|| u64::from_le_bytes(b))
            })
        });
        match to {
            Some(t) => {
                if self.mods.locate(t).is_none() {
                    // a library that came in after the last look
                    self.mods.refresh(&mut *self.t);
                }
                let name = self.mods.label(t, false).unwrap_or_else(|| "?".into());
                self.keep_target(addr, t, &name);
                format!("call {addr:#x} {from} -> {t:#x} {name}")
            }
            None => format!("call {addr:#x} {from} -> ? (target could not be read)"),
        }
    }

    /// Save what an indirect call went to, for `nexo bin decomp` to show. Only the program's own sites, once per pair.
    fn keep_target(&mut self, site: u64, target: u64, name: &str) {
        let Some(dir) = crate::knowledge::data_dir() else { return };
        let main = self.mods.main_base();
        if main == 0 || self.mods.locate(site) != self.mods.locate(main) {
            return;
        }
        let bias = self.mods.main_bias();
        let entry = if self.mods.locate(target) == self.mods.locate(main) {
            format!("{:#x} {name}", target.wrapping_sub(bias))
        } else {
            name.to_string()
        };
        let site = site.wrapping_sub(bias);
        if self.seen_targets.insert((site, entry.clone())) {
            let saved = nexo_bin::add_call_target(self.mods.main_image(), &dir, site, &entry);
            if saved.is_ok() && !self.said_saved {
                self.said_saved = true;
                self.log_line(format!(
                    "saved indirect-call targets for `nexo bin decomp` in {} (more are added as they are seen)",
                    nexo_bin::names_path(&dir, &self.mods.main_image().store_key()).display()
                ));
            }
        }
    }

    pub fn log_line(&mut self, s: String) {
        self.log_total += 1;
        self.log.push_back(s);
        while self.log.len() > LOG_KEEP {
            self.log.pop_front();
        }
    }

    /// Write down the children the program forked since the last look. Each one the backend kept becomes a session
    /// of its own in `kids`, with the breakpoints copied.
    pub(super) fn take_forks(&mut self) {
        for f in self.t.forks() {
            // a checkpoint's fork: the child is kept as it is, with the guards of the moment, not adopted
            if let Some(kept) = &mut self.ckpt_forks {
                let guards = self.guards.iter().filter(|g| g.armed).copied().collect();
                kept.push((f, guards));
                continue;
            }
            let Forked { pid, at, vfork, patches, target, .. } = f;
            // fork returns in libc, which the list read at the start may not hold yet
            if self.mods.locate(at).is_none() {
                self.mods.refresh(&mut *self.t);
            }
            let name = self.mods.label(at, false).unwrap_or_default();
            let place = if name.is_empty() { format!("{at:#x}") } else { format!("{at:#x} {name}") };
            let session = target.and_then(|t| self.adopt(t, pid, &place, vfork, &patches));
            if let Some(id) = &session {
                if self.follow == Follow::Both {
                    let (kind, when) = if vfork { ("vfork", "at its exec") } else { ("fork", "where it was made") };
                    self.log_line(format!(
                        "pid {pid} ({kind} at {place}) is session {id}, stopped {when}: `-s {id} cont` runs it"
                    ));
                }
            }
            self.forks.push(ForkNote { pid, place, vfork, session });
        }
    }

    /// A forked child the backend kept: a session of its own, stopped where fork returned 0 in it (a vfork child: at
    /// its exec), with the breakpoints copied. `None` when its program cannot be read; the child then goes when `t` is dropped.
    fn adopt(
        &mut self,
        t: Box<dyn Target>,
        pid: u32,
        place: &str,
        vfork: bool,
        patches: &[(u64, u8)],
    ) -> Option<String> {
        let img = match Image::open_arch(self.mods.exe_path(), crate::platform::NATIVE_SLICE) {
            Ok(i) => i,
            Err(e) => {
                self.log_line(format!("pid {pid} forked at {place} was not kept: its program cannot be read ({e})"));
                return None;
            }
        };
        let mut kid = Session::new(t, img);
        kid.follow = self.follow;
        kid.t.set_follow(self.follow);
        kid.bps = self.bps.iter().filter(|b| !b.once).cloned().map(|b| Bp { hits: 0, ..b }).collect();
        kid.next_id = self.next_id;
        kid.catch = self.catch.clone();
        let ids: Vec<String> = kid.bps.iter().filter(|b| !b.call).map(|b| format!("#{}", b.id)).collect();
        let parent = self.t.pid();
        let text = if vfork {
            // a vfork child comes at its exec: the memory it shared is the parent's alone again, and the copied
            // breakpoints are addresses in the parent's program, so they are off; names come from the new program
            kid.on_exec();
            let mods: Vec<String> = kid.mods.list.iter().map(|m| format!("{} at {:#x}", m.path, m.base)).collect();
            let copied = if ids.is_empty() {
                "no breakpoints to copy".to_string()
            } else {
                format!("breakpoints {} copied and off (addresses in pid {parent}'s program)", ids.join(" "))
            };
            format!(
                "pid {pid}, made by vfork in pid {parent} at {place}: stopped at its exec of {}; modules now: {}; {copied} (watches, hooks and traces stay with pid {parent})",
                kid.mods.exe_path().display(),
                mods.join(", ")
            )
        } else {
            // the child's memory holds 0xcc where the parent's did at the fork; a stopped session holds the
            // program's own bytes, and the child's next run puts its breakpoints back
            kid.inserted = patches.iter().copied().collect();
            kid.t.set_patches(patches);
            // the child's copy of a guarded page is guarded too, and its watch stays with the parent
            kid.guards = self.guards.iter().filter(|g| g.armed).copied().collect();
            kid.uninsert_all();
            kid.guards.clear();
            let copied = if ids.is_empty() {
                "no breakpoints to copy".to_string()
            } else {
                format!("breakpoints {} copied", ids.join(" "))
            };
            format!(
                "pid {pid}, made by fork in pid {parent} at {place}: stopped where fork returned 0 in it; {copied} (watches, hooks and traces stay with pid {parent})"
            )
        };
        let first = kid.answer(pid as Tid, Why::Fork(text));
        let id = crate::server::new_id();
        self.kids.push(Kid { id: id.clone(), sess: kid, first });
        Some(id)
    }

    /// The answer of a parent let go at a fork under `follow child`.
    fn let_go_text(&self, pid: u32) -> String {
        match self.forks.iter().rev().find(|f| f.pid == pid) {
            Some(ForkNote { place, session: Some(id), .. }) => format!(
                "the program forked pid {pid} at {place}: under `follow child` the child is session {id} and this process goes on without the debugger; `-s {id} cont` runs the child"
            ),
            _ => format!("the program forked pid {pid}: under `follow child` this process goes on without the debugger"),
        }
    }

    /// The program replaced itself (exec). The bytes under our breakpoints went with the old program, and what was
    /// set by address in it does not point into the new one, so it is switched off; names and lines come from the new
    /// program from here on.
    fn on_exec(&mut self) {
        self.inserted.clear();
        self.t.set_patches(&[]);
        self.libs.forget();
        self.tr.pending.clear();
        let mut off = Vec::new();
        for b in self.bps.iter_mut().filter(|b| b.enabled) {
            b.enabled = false;
            off.push(format!("#{}", b.id));
        }
        let mut gone = Vec::new();
        if self.tr.lib.take().is_some() {
            gone.push("ltrace");
        }
        if self.tr.heap.take().is_some() {
            gone.push("heap");
        }
        if !self.tr.hooks.is_empty() {
            self.tr.hooks.clear();
            gone.push("hooks");
        }
        if self.tr.cover.take().is_some() {
            gone.push("cover");
        }
        // the guarded pages went with the old program
        self.guards.clear();
        self.step_from = None;
        let n = self.wps.len();
        self.wps.retain(|w| w.how == WatchHow::Hardware && w.scope.is_none());
        if self.wps.len() < n {
            gone.push("page, step and frame watches");
        }
        if self.t.os() == Os::Linux {
            if let Ok(path) = std::fs::read_link(format!("/proc/{}/exe", self.t.pid())) {
                if let Ok(mut img) = Image::open_arch(&path, crate::platform::NATIVE_SLICE) {
                    if let Some(d) = crate::knowledge::data_dir() {
                        let _ = nexo_bin::attach_names(&mut img, &d);
                    }
                    self.mods = Mods::new(img);
                }
            }
        }
        self.mods.refresh(&mut *self.t);
        if !off.is_empty() {
            self.log_line(format!(
                "breakpoints {} were addresses in the old program and are disabled now (`enable <id>` arms one at the same address)",
                off.join(" ")
            ));
        }
        if !gone.is_empty() {
            self.log_line(format!("{} were set on the old program and are off now", gone.join(", ")));
        }
    }

    /// `x is {rax}` with each `{expression}` replaced by its value.
    pub(super) fn format_log(&mut self, tid: Tid, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            let Some(j) = rest[i..].find('}') else {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            };
            let e = &rest[i + 1..i + j];
            // `{mem:EXPR:N}`: N bytes at EXPR as hex
            if let Some(m) = e.strip_prefix("mem:") {
                let shown = self.log_bytes(tid, m);
                out.push_str(&shown);
                rest = &rest[i + j + 1..];
                continue;
            }
            let (src, fmt) = e.rsplit_once(':').filter(|(_, f)| matches!(*f, "x" | "d" | "s")).unwrap_or((e, "x"));
            let shown = match self.ev(tid).and_then(|mut ev| expr::eval(src, &mut ev)) {
                Ok(v) if fmt == "d" => format!("{}", v.v as i64),
                Ok(v) if fmt == "s" => self.cstr(v.v),
                Ok(v) => format!("{:#x}", v.v),
                Err(e) => format!("<{e}>"),
            };
            out.push_str(&shown);
            rest = &rest[i + j + 1..];
        }
        out.push_str(rest);
        out
    }

    /// `{mem:EXPR:N}` of a logpoint: N (1 to 256, default 16) bytes at the address EXPR works out to, as hex.
    fn log_bytes(&mut self, tid: Tid, spec: &str) -> String {
        let (src, n) = match spec.rsplit_once(':').and_then(|(a, n)| Some((a, nexo_bin::parse_addr(n).ok()?))) {
            Some((a, n)) => (a, n as usize),
            None => (spec, 16),
        };
        if n == 0 || n > LOG_BYTES {
            return format!("<mem takes 1 to {LOG_BYTES} bytes>");
        }
        let at = match self.ev(tid).and_then(|mut ev| expr::value(src, &mut ev)) {
            Ok(a) => a,
            Err(e) => return format!("<{e}>"),
        };
        let b = self.read_vec(at, n);
        if b.is_empty() {
            return format!("<cannot read {at:#x}>");
        }
        b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
    }
}
