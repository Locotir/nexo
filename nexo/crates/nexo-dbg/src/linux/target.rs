//! `Target` for a Linux process.

use super::*;

impl Target for Linux {
    fn os(&self) -> Os {
        Os::Linux
    }

    fn pid(&self) -> u32 {
        self.pid as u32
    }

    fn launched(&self) -> bool {
        self.launched
    }

    fn local_proc(&self) -> bool {
        true
    }

    fn threads(&self) -> Vec<Tid> {
        self.th.keys().map(|t| *t as Tid).collect()
    }

    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        let mut u = [0u64; 27];
        pt(GETREGS, tid as i32, 0, u.as_mut_ptr() as u64)
            .map_err(|e| os_err(&format!("registers of thread {tid}"), e))?;
        Ok(Regs::from_user(&u))
    }

    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()> {
        let u = regs.to_user();
        pt(SETREGS, tid as i32, 0, u.as_ptr() as u64)
            .map_err(|e| os_err(&format!("set registers of thread {tid}"), e))?;
        Ok(())
    }

    fn vector_state(&mut self, tid: Tid) -> Option<Vec<u8>> {
        // The XSAVE area (first byte 0), or the 512-byte FXSAVE one (first byte 1) on a kernel without it.
        let mut b = vec![0u8; XSTATE_ROOM];
        let mut iov = [b.as_mut_ptr() as u64, b.len() as u64];
        if pt(GETREGSET, tid as i32, NT_X86_XSTATE, iov.as_mut_ptr() as u64).is_ok() {
            b.truncate(iov[1] as usize);
            b.insert(0, 0);
            return Some(b);
        }
        let mut f = vec![0u8; 512];
        pt(GETFPREGS, tid as i32, 0, f.as_mut_ptr() as u64).ok()?;
        f.insert(0, 1);
        Some(f)
    }

    fn set_vector_state(&mut self, tid: Tid, state: &[u8]) -> Result<()> {
        let Some((kind, body)) = state.split_first() else { return Ok(()) };
        let mut b = body.to_vec();
        let r = if *kind == 0 {
            let mut iov = [b.as_mut_ptr() as u64, b.len() as u64];
            pt(SETREGSET, tid as i32, NT_X86_XSTATE, iov.as_mut_ptr() as u64)
        } else {
            pt(SETFPREGS, tid as i32, 0, b.as_mut_ptr() as u64)
        };
        r.map_err(|e| os_err(&format!("set the vector registers of thread {tid}"), e))?;
        Ok(())
    }

    fn take_signal(&mut self, tid: Tid) -> u64 {
        self.th.get_mut(&(tid as i32)).map_or(0, |t| std::mem::take(&mut t.sig) as u64)
    }

    fn give_signal(&mut self, tid: Tid, sig: u64) {
        if let Some(t) = self.th.get_mut(&(tid as i32)) {
            t.sig = sig as i32;
        }
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        if let Some(f) = &self.mem {
            while done < buf.len() {
                match f.read_at(&mut buf[done..], addr.wrapping_add(done as u64)) {
                    Ok(n) if n > 0 => done += n,
                    _ => break,
                }
            }
            if done > 0 {
                return done;
            }
        }
        let Ok(tid) = self.stopped_tid() else { return 0 };
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let base = a & !7;
            let mut w = 0u64;
            if pt(PEEKDATA, tid, base, (&raw mut w) as u64).is_err() {
                break;
            }
            let off = (a - base) as usize;
            let n = (8 - off).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&w.to_le_bytes()[off..off + n]);
            done += n;
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        // Word by word through ptrace: it also works where the sandbox denies opening the memory file for writing.
        let tid = self.stopped_tid()?;
        let mut done = 0;
        while done < bytes.len() {
            let a = addr.wrapping_add(done as u64);
            let base = a & !7;
            let off = (a - base) as usize;
            let n = (8 - off).min(bytes.len() - done);
            let mut w = 0u64;
            let bad = |e| os_err(&format!("write at {a:#x}"), e);
            if n < 8 {
                pt(PEEKDATA, tid, base, (&raw mut w) as u64).map_err(bad)?;
            }
            let mut b = w.to_le_bytes();
            b[off..off + n].copy_from_slice(&bytes[done..done + n]);
            pt(POKEDATA, tid, base, u64::from_le_bytes(b)).map_err(bad)?;
            done += n;
        }
        Ok(())
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let text = std::fs::read_to_string(format!("/proc/{}/maps", self.pid))?;
        Ok(text.lines().filter_map(parse_map).collect())
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        let exe =
            std::fs::read_link(format!("/proc/{}/exe", self.pid)).map(|p| p.display().to_string()).unwrap_or_default();
        let mut out: Vec<Module> = crate::modules::loads(&self.maps()?);
        if let Some(i) = out.iter().position(|m| m.path == exe) {
            out[..=i].rotate_right(1);
        }
        Ok(out)
    }

    fn entry(&mut self) -> Option<u64> {
        let b = std::fs::read(format!("/proc/{}/auxv", self.pid)).ok()?;
        // a 32-bit program's vector is of 4-byte words; its file's class says which one it is
        let mut head = [0u8; 5];
        let elf32 = File::open(format!("/proc/{}/exe", self.pid)).and_then(|mut f| f.read_exact(&mut head)).is_ok()
            && head == *b"\x7fELF\x01";
        auxv_entry(&b, if elf32 { 4 } else { 8 })
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        self.watch = *slots;
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            self.apply_watch(tid).map_err(|e| os_err("hardware watch", e))?;
        }
        Ok(())
    }

    fn protect(&mut self, addr: u64, len: u64, prot: u8) -> Result<()> {
        // ptrace cannot change another process's pages: the program itself runs one mprotect from a stopped thread
        // (`mprotect_in`), one on the way back to its code if there is one, else one a step takes out of a fork,
        // clone or exec first
        let mut finish = None;
        let mut why = Vec::new();
        let mut free = None;
        for (&tid, t) in self.th.iter().filter(|(_, t)| !t.running && !self.gone) {
            if t.held {
                why.push(format!("thread {tid} waits in vfork for its child to exec or exit"));
                continue;
            }
            match inside_call(tid) {
                Ok(false) => {
                    free = Some(tid);
                    break;
                }
                Ok(true) => {
                    finish.get_or_insert(tid);
                }
                Err(e) => why.push(e),
            }
        }
        let Some((tid, ends)) = free.map(|t| (t, false)).or(finish.map(|t| (t, true))) else {
            return if why.is_empty() {
                fail("the program is not stopped")
            } else {
                fail(format!("mprotect cannot run in the program now: {}", why.join("; ")))
            };
        };
        let mut sigs = Vec::new();
        let r = mprotect_in(tid, addr, len, prot, ends, &mut sigs);
        self.keep_signals(tid, sigs);
        r
    }

    fn set_guards(&mut self, guards: &[(u64, u64, u8, u8)]) {
        self.guards = guards.to_vec();
    }

    fn set_patches(&mut self, patches: &[(u64, u8)]) {
        self.patches = patches.to_vec();
    }

    fn set_follow(&mut self, follow: Follow) -> bool {
        self.follow = follow;
        true
    }

    fn forks(&mut self) -> Vec<Forked> {
        std::mem::take(&mut self.forks)
    }

    fn trace_syscalls(&mut self, on: bool) -> Result<()> {
        if on && !self.sys {
            // ask once, on a stopped thread, whether this kernel can say what a stop is about
            let Some(tid) = self.th.keys().next().copied() else { return fail("the program has ended") };
            let mut b = [0u64; 12];
            if let Err(e) = pt(GET_SYSCALL_INFO, tid, std::mem::size_of_val(&b) as u64, b.as_mut_ptr() as u64) {
                return Err(os_err(
                    "this kernel cannot describe system calls (PTRACE_GET_SYSCALL_INFO, Linux 5.3+)",
                    e,
                ));
            }
        }
        self.sys = on;
        Ok(())
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        if self.gone {
            return fail("the program has ended");
        }
        let one = match how {
            Resume::All => None,
            Resume::Step(t) => Some(t as i32),
        };
        let sys = self.sys;
        for (tid, t) in self.th.iter_mut() {
            if t.running || one.is_some_and(|o| o != *tid) {
                continue;
            }
            t.step = one.is_some();
            if t.held {
                t.go = true;
                continue;
            }
            let sig = std::mem::take(&mut t.sig);
            match pt(order(t.step, sys), *tid, 0, sig as u64) {
                Ok(_) => t.running = true,
                // A thread that died while stopped is found by the next wait.
                Err(e) if e.raw_os_error() == Some(libc::ESRCH) => t.running = true,
                Err(e) => return Err(os_err("resume", e)),
            }
        }
        if let Some(o) = one {
            if !self.th.contains_key(&o) {
                return fail(format!("no thread {o}"));
            }
        }
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if self.gone {
            return fail("the program has ended");
        }
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        let mut round = 0u32;
        loop {
            self.tend_vkids(&mut events);
            let held = self.th.iter().find(|(_, t)| t.held && t.go).map(|(k, _)| *k);
            // a held thread is looked at too: it ends with the process
            let running: Vec<i32> = self.th.iter().filter(|(_, t)| t.running || t.held).map(|(k, _)| *k).collect();
            if !self.th.values().any(|t| t.running) && events.is_empty() {
                match held {
                    None => return fail("the program is stopped: nothing to wait for"),
                    Some(tid) if self.want_stop => {
                        self.want_stop = false;
                        events.push(Event::Stopped { tid: tid as Tid, stop: Stop::Interrupt });
                    }
                    Some(_) => {}
                }
            }
            for tid in running {
                match waitpid(tid, libc::WNOHANG) {
                    Ok(Some(st)) => self.handle(tid, st, false, &mut events),
                    Ok(None) => {}
                    Err(_) => {
                        self.th.remove(&tid);
                        if tid == self.pid {
                            self.gone = true;
                            events.push(Event::Exited { code: -1 });
                        }
                    }
                }
                if self.gone {
                    break;
                }
            }
            if !events.is_empty() {
                self.stop_all(&mut events);
                self.want_stop = false;
                return Ok(events);
            }
            if Instant::now() >= deadline {
                return Ok(events);
            }
            round += 1;
            if round < 200 {
                std::thread::yield_now();
            } else {
                std::thread::sleep(Duration::from_micros((u64::from(round) * 10).min(2000)));
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        let Some(tid) = self.th.iter().find(|(_, t)| t.running).map(|(k, _)| *k) else {
            // only threads held at a vfork that would go on: the next wait reports one of them stopped
            if self.th.values().any(|t| t.held && t.go) {
                self.want_stop = true;
                return Ok(());
            }
            return fail("the program is already stopped");
        };
        self.want_stop = true;
        pt(INTERRUPT, tid, 0, 0).map_err(|e| os_err("stop", e))?;
        Ok(())
    }

    fn stdin(&mut self, bytes: Option<&[u8]>) -> Result<usize> {
        let Some(bytes) = bytes else {
            self.stdin = None;
            return Ok(0);
        };
        let Some(f) = self.stdin.as_mut() else {
            return fail("this program's input is not connected to the session (attached, or already closed)");
        };
        let mut done = 0;
        while done < bytes.len() {
            match f.write(&bytes[done..]) {
                Ok(n) if n > 0 => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(os_err("write to the program's input", e)),
                Ok(_) => break,
            }
        }
        Ok(done)
    }

    fn output(&mut self) -> Vec<u8> {
        let mut all = Vec::new();
        if let Some(f) = self.out.as_mut() {
            let mut buf = [0u8; 8192];
            loop {
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => all.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    _ => break,
                }
            }
        }
        all
    }

    fn detach(&mut self) -> Result<()> {
        self.drop_vkids(false);
        if self.gone {
            return Ok(());
        }
        let mut events = Vec::new();
        self.stop_all(&mut events);
        // a watch left in a thread's debug registers would end it with SIGTRAP once nobody traces it
        let watched = self.watch.iter().any(Option::is_some);
        self.watch = [None; 4];
        for (tid, t) in &self.th {
            if watched {
                let _ = pt(POKEUSER, *tid, DEBUGREG + 7 * 8, 0);
            }
            let _ = pt(DETACH, *tid, 0, t.sig as u64);
        }
        self.th.clear();
        self.gone = true;
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        self.drop_vkids(true);
        if self.gone {
            return Ok(());
        }
        // SAFETY: the process is the one we trace.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut events = Vec::new();
        while !self.gone && Instant::now() < deadline {
            for tid in self.th.keys().copied().collect::<Vec<_>>() {
                match waitpid(tid, libc::WNOHANG) {
                    Ok(Some(st)) => self.handle(tid, st, true, &mut events),
                    Ok(None) => {}
                    Err(_) => {
                        self.th.remove(&tid);
                        self.gone |= tid == self.pid;
                    }
                }
            }
            if !self.gone {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        self.gone = true;
        self.th.clear();
        Ok(())
    }
}
