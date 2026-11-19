//! Children the program forks or vforks: following them, releasing them, their breakpoints.

use super::*;

impl Linux {
    /// A child the program forked: put the original bytes back over our breakpoints and let it go. `shared`: it
    /// shares the memory it was made in (a thread or a vfork child of a vfork child), so the bytes stay.
    fn release_child(&self, child: i32, shared: bool) {
        if !Self::first_stop(child) {
            return;
        }
        let mut sigs = Vec::new();
        if !shared {
            for &(addr, byte) in &self.patches {
                Self::poke_byte(child, addr, byte);
            }
            // its copy of a guarded page would end it with SIGSEGV at its first write there
            for &(start, len, own, _) in &self.guards {
                let _ = mprotect_in(child, start, len, own, false, &mut sigs);
            }
        }
        let first = if sigs.is_empty() { 0 } else { sigs.remove(0) };
        send_again(child, sigs);
        let _ = pt(DETACH, child, 0, first as u64);
    }

    /// Signals thread `tid` stopped on while it ran an instruction of ours: the first is handed over when it runs
    /// again, as if it had stopped on it (when it holds none yet), the others are sent again.
    pub(super) fn keep_signals(&mut self, tid: i32, mut sigs: Vec<i32>) {
        if let Some(t) = self.th.get_mut(&tid).filter(|t| t.sig == 0 && !sigs.is_empty()) {
            t.sig = sigs.remove(0);
        }
        send_again(tid, sigs);
    }

    /// Vfork child `i` stopped on SIGSEGV or SIGBUS. In a page a watch guards (the memory is the parent's), the page
    /// gets its own access back for the child's one instruction, which is no hit of the parent's watch; true then.
    fn vkid_fault(&mut self, i: usize) -> bool {
        let pid = self.vkids[i].pid;
        let mut info = [0u64; 16];
        if pt(GETSIGINFO, pid, 0, info.as_mut_ptr() as u64).is_err() {
            return false;
        }
        let a = info[2];
        let Some(&g) = self.guards.iter().find(|g| a >= g.0 && a - g.0 < g.1) else { return false };
        // the two bytes of the mprotect call stand for one step where the child is, in code the parent's other
        // threads (they run) could reach meanwhile; a vfork child runs code of its own before its exec
        let mut sigs = Vec::new();
        let opened = mprotect_in(pid, g.0, g.1, g.2, false, &mut sigs).is_ok();
        send_again(pid, sigs);
        if !opened {
            return false;
        }
        // a breakpoint on that instruction would end the step: the program's byte goes in for it
        let pc = self.regs(pid as Tid).map_or(0, |r| r.rip);
        if let Some(byte) = self.patches.iter().find(|p| p.0 == pc).map(|p| p.1) {
            if Self::poke_byte(pid, pc, byte) {
                self.vkids[i].over = Some(pc);
            }
        }
        self.vkids[i].open = Some(g);
        pt(SINGLESTEP, pid, 0, 0).is_ok()
    }

    /// A child the program forked, kept as a target of its own: one thread, stopped where fork returned in it (with
    /// the parent's breakpoint bytes, `patches`, in its memory) or at its exec. It is killed at the end when the
    /// parent was started here, as `PTRACE_O_EXITKILL` passes to it.
    fn kid(&self, child: i32, patches: Vec<(u64, u8)>) -> Linux {
        let mut th = BTreeMap::new();
        th.insert(child, Th::default());
        let mut kid = Linux {
            pid: child,
            th,
            mem: None,
            launched: self.launched,
            stdin: None,
            // the child writes to the same pipe as the parent; its own copy of the read end keeps the pipe open (no
            // SIGPIPE in the child) when the parent's session ends first
            out: self.out.as_ref().and_then(|f| f.try_clone().ok()),
            watch: [None; 4],
            patches,
            guards: Vec::new(),
            gone: false,
            want_stop: false,
            sys: false,
            follow: self.follow,
            forks: Vec::new(),
            vkids: Vec::new(),
        };
        kid.open_mem();
        kid
    }

    /// The program forked: keep the child or let it go as `follow` says, and write the fork down.
    pub(super) fn on_fork(&mut self, tid: i32, vfork: bool, stopping: bool, events: &mut Vec<Event>) {
        let mut child = 0u64;
        if pt(GETEVENTMSG, tid, 0, (&raw mut child) as u64).is_err() || child == 0 {
            if !stopping {
                self.again(tid);
            }
            return;
        }
        let child = child as i32;
        let at = self.regs(tid as Tid).map_or(0, |r| r.rip);
        if vfork {
            if Self::first_stop(child) && pt(CONT, child, 0, 0).is_ok() {
                self.vkids.push(Vkid { pid: child, parent: tid, at, over: None, open: None });
                if let Some(t) = self.th.get_mut(&tid) {
                    t.held = true;
                    t.go = !stopping;
                }
                return;
            }
            self.forks.push(Forked {
                pid: child as u32,
                tid: tid as Tid,
                at,
                vfork,
                patches: Vec::new(),
                target: None,
            });
            if !stopping {
                self.again(tid);
            }
            return;
        }
        let target: Option<Box<dyn Target>> = match self.follow {
            Follow::Parent => {
                self.release_child(child, false);
                None
            }
            Follow::Child | Follow::Both => {
                Self::first_stop(child).then(|| Box::new(self.kid(child, self.patches.clone())) as Box<dyn Target>)
            }
        };
        let kept = target.is_some();
        self.forks.push(Forked {
            pid: child as u32,
            tid: tid as Tid,
            at,
            vfork,
            patches: self.patches.clone(),
            target,
        });
        if kept && self.follow == Follow::Child {
            events.push(Event::Stopped { tid: tid as Tid, stop: Stop::Fork(child as u32) });
        } else if !stopping {
            self.again(tid);
        }
    }

    /// Look at the vfork children: step one past a breakpoint, hand it its signals, see it exec or end.
    pub(super) fn tend_vkids(&mut self, events: &mut Vec<Event>) {
        let mut i = 0;
        while i < self.vkids.len() {
            let pid = self.vkids[i].pid;
            let st = match waitpid(pid, libc::WNOHANG) {
                Ok(None) => {
                    i += 1;
                    continue;
                }
                Ok(Some(st)) => Some(st),
                Err(_) => None,
            };
            let stepped = match self.vkids[i].over.take() {
                Some(a) => {
                    self.rearm(a, self.vkids[i].parent);
                    true
                }
                None => false,
            };
            let opened = self.vkids[i].open.take();
            let Some(st) = st.filter(|&st| libc::WIFSTOPPED(st)) else {
                // a child gone in the middle of its step leaves an opened guard open: no stopped thread can close
                // it until the parent's program stops
                self.vfork_done(i, false, events);
                continue;
            };
            // the guard opened for the child's step closes, unless the watch was dropped meanwhile
            if let Some(g) = opened.filter(|g| self.guards.contains(g)) {
                let mut sigs = Vec::new();
                let _ = mprotect_in(pid, g.0, g.1, g.3, false, &mut sigs);
                send_again(pid, sigs);
            }
            let sig = libc::WSTOPSIG(st);
            match st >> 16 {
                EVENT_EXEC => {
                    self.vfork_done(i, true, events);
                    continue;
                }
                EVENT_FORK | EVENT_VFORK | EVENT_CLONE => {
                    // what the vfork child starts before its exec is not followed
                    let mut new = 0u64;
                    if pt(GETEVENTMSG, pid, 0, (&raw mut new) as u64).is_ok() && new > 0 {
                        self.release_child(new as i32, st >> 16 != EVENT_FORK);
                    }
                    let _ = pt(CONT, pid, 0, 0);
                }
                // the end of the step past a guarded page (the mprotect run since stands in for its trap)
                0 if sig == libc::SIGTRAP && opened.is_some() => {
                    let _ = pt(CONT, pid, 0, 0);
                }
                0 if sig == libc::SIGTRAP => self.vkid_trap(i, stepped),
                0 if matches!(sig, libc::SIGSEGV | libc::SIGBUS) && self.vkid_fault(i) => {}
                0 => {
                    let _ = pt(CONT, pid, 0, sig as u64);
                }
                _ => {
                    let _ = pt(CONT, pid, 0, 0);
                }
            }
            i += 1;
        }
    }

    /// `Some(a)` when the SIGTRAP vfork child `pid` stopped on came from one of our int3 bytes at `a`, there now or
    /// taken out since (its pc is set back to `a` then); `None` when the trap is its own.
    fn our_trap(&mut self, pid: i32) -> Option<u64> {
        let mut info = [0u64; 16];
        pt(GETSIGINFO, pid, 0, info.as_mut_ptr() as u64).ok()?;
        if !matches!((info[1] & 0xffff_ffff) as u32, 0x80 | 1) {
            return None;
        }
        let mut r = self.regs(pid as Tid).ok()?;
        let a = r.rip.wrapping_sub(1);
        let mut b = [0u8; 1];
        if !self.patches.iter().any(|p| p.0 == a) && !(self.read(a, &mut b) == 1 && b[0] != 0xcc) {
            return None;
        }
        r.rip = a;
        self.set_regs(pid as Tid, &r).ok()?;
        Some(a)
    }

    /// Vfork child `i` stopped on SIGTRAP. At one of our breakpoints it runs that one instruction with the program's
    /// byte in place (0xcc goes back when the step ends); at one taken out since, it runs the program's byte; the end
    /// of our step (`stepped`) goes on; any other trap is its own and it gets the signal.
    fn vkid_trap(&mut self, i: usize, stepped: bool) {
        let pid = self.vkids[i].pid;
        let Some(a) = self.our_trap(pid) else {
            let sig = if stepped { 0 } else { libc::SIGTRAP as u64 };
            let _ = pt(CONT, pid, 0, sig);
            return;
        };
        match self.patches.iter().find(|p| p.0 == a).map(|p| p.1) {
            Some(byte) if Self::poke_byte(pid, a, byte) => {
                self.vkids[i].over = Some(a);
                let _ = pt(SINGLESTEP, pid, 0, 0);
            }
            _ => {
                let _ = pt(CONT, pid, 0, 0);
            }
        }
    }

    /// 0xcc back at `a` after a vfork child stepped past it, through the parent thread `parent` (the step may have
    /// been the exec); not when the breakpoint was taken out meanwhile.
    fn rearm(&self, a: u64, parent: i32) {
        if self.patches.iter().any(|p| p.0 == a) {
            Self::poke_byte(parent, a, 0xcc);
        }
    }

    /// Vfork child `i` exec'd (`exec`) or ended: the parent's memory is its own again, so the thread that made it
    /// goes on, and the child is kept at its exec as a target of its own (`follow child|both`) or let go.
    fn vfork_done(&mut self, i: usize, exec: bool, events: &mut Vec<Event>) {
        let v = self.vkids.remove(i);
        let target: Option<Box<dyn Target>> = match self.follow {
            _ if !exec => None,
            Follow::Parent => {
                let _ = pt(DETACH, v.pid, 0, 0);
                None
            }
            // none of our bytes are in the new program's memory
            Follow::Child | Follow::Both => Some(Box::new(self.kid(v.pid, Vec::new()))),
        };
        let kept = target.is_some();
        self.forks.push(Forked {
            pid: v.pid as u32,
            tid: v.parent as Tid,
            at: v.at,
            vfork: true,
            patches: Vec::new(),
            target,
        });
        let Some(t) = self.th.get_mut(&v.parent) else { return };
        t.held = false;
        let go = std::mem::take(&mut t.go);
        if kept && self.follow == Follow::Child {
            events.push(Event::Stopped { tid: v.parent as Tid, stop: Stop::Fork(v.pid as u32) });
        } else if go {
            self.again(v.parent);
        }
    }

    /// The vfork children go with the program: killed (`kill`), or let go (`detach`); one stopped at a trap of ours
    /// has its pc set back before the int3 first.
    pub(super) fn drop_vkids(&mut self, kill: bool) {
        for v in std::mem::take(&mut self.vkids) {
            if let Some(t) = self.th.get_mut(&v.parent) {
                t.held = false;
                t.go = false;
            }
            if kill {
                // SAFETY: a process we trace.
                unsafe { libc::kill(v.pid, libc::SIGKILL) };
            } else {
                let _ = pt(INTERRUPT, v.pid, 0, 0);
            }
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match waitpid(v.pid, libc::WNOHANG) {
                    Ok(Some(st)) if libc::WIFSTOPPED(st) && kill => {
                        let _ = pt(CONT, v.pid, 0, 0);
                    }
                    Ok(Some(st)) if libc::WIFSTOPPED(st) => {
                        let sig = libc::WSTOPSIG(st);
                        let hand = match st >> 16 {
                            0 if sig == libc::SIGTRAP => {
                                self.our_trap(v.pid).is_none() && v.over.is_none() && v.open.is_none()
                            }
                            0 => true,
                            EVENT_FORK | EVENT_VFORK | EVENT_CLONE => {
                                let mut new = 0u64;
                                if pt(GETEVENTMSG, v.pid, 0, (&raw mut new) as u64).is_ok() && new > 0 {
                                    self.release_child(new as i32, st >> 16 != EVENT_FORK);
                                }
                                false
                            }
                            _ => false,
                        };
                        let _ = pt(DETACH, v.pid, 0, if hand { sig as u64 } else { 0 });
                        break;
                    }
                    Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(1)),
                    _ => break,
                }
            }
        }
    }
}
