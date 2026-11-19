//! `Target` for a macOS process.

use super::*;

impl Target for Mac {
    fn os(&self) -> Os {
        Os::MacOS
    }

    fn pid(&self) -> u32 {
        self.pid as u32
    }

    fn launched(&self) -> bool {
        self.launched
    }

    fn threads(&self) -> Vec<Tid> {
        self.th.keys().map(|t| *t as Tid).collect()
    }

    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        Ok(Regs::from_mach(&self.state(tid as Port)?))
    }

    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()> {
        self.set_state(tid as Port, &regs.to_mach())
    }

    fn vector_state(&mut self, tid: Tid) -> Option<Vec<u8>> {
        // The AVX state (the float state, then the upper ymm halves) where the CPU has it, else the float state.
        let flavors = [(X86_AVX_STATE64, X86_AVX_STATE64_COUNT), (X86_FLOAT_STATE64, X86_FLOAT_STATE64_COUNT)];
        for (flavor, count) in flavors {
            let mut s = vec![0u32; count as usize];
            let mut n = count;
            // SAFETY: `s` holds the `n` words the call may write.
            let kr = unsafe { thread_get_state(tid as Port, flavor, s.as_mut_ptr(), &mut n) };
            if kr == KERN_SUCCESS {
                return Some(s[..n as usize].iter().flat_map(|w| w.to_le_bytes()).collect());
            }
        }
        None
    }

    fn set_vector_state(&mut self, tid: Tid, state: &[u8]) -> Result<()> {
        let s: Vec<u32> = state.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        // the flavor `vector_state` read, told by its length
        let flavor = if s.len() >= X86_AVX_STATE64_COUNT as usize { X86_AVX_STATE64 } else { X86_FLOAT_STATE64 };
        // SAFETY: `s` holds the words `thread_get_state` gave.
        let kr = unsafe { thread_set_state(tid as Port, flavor, s.as_ptr(), s.len() as u32) };
        if kr != KERN_SUCCESS {
            return fail(format!("set the vector registers of thread {tid}: {}", kr_text(kr)));
        }
        Ok(())
    }

    fn take_signal(&mut self, tid: Tid) -> u64 {
        // the fault or signal the thread stopped on is not handed to the program when it runs again
        let Some(t) = self.th.get_mut(&(tid as Port)) else { return 0 };
        t.fault = false;
        std::mem::take(&mut t.sig) as u32 as u64
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let n = (buf.len() - done).min((0x1000 - (a & 0xfff)) as usize);
            let mut got = 0u64;
            // SAFETY: the kernel writes at most `n` bytes into the buffer from `done` on.
            let kr =
                unsafe { mach_vm_read_overwrite(self.task, a, n as u64, buf[done..].as_mut_ptr() as u64, &mut got) };
            if kr != KERN_SUCCESS || got == 0 {
                break;
            }
            done += got as usize;
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        patch(self.task, addr, bytes)
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let mut out: Vec<Map> = Vec::new();
        let mut addr = 0u64;
        // depth 0: the shared library cache is one submap region; looking into it would list thousands of pieces
        while let Some((start, end, prot, offset, tag, _)) = self.region(addr, &mut 0) {
            if end <= start {
                break;
            }
            addr = end;
            let mut path = self.file_at(start);
            if path.is_empty() {
                if tag == TAG_STACK {
                    path = "[stack]".into();
                } else if TAG_HEAP.contains(&tag) {
                    path = "[heap]".into();
                }
            }
            let m = Map {
                start,
                end,
                read: prot & VM_PROT_READ != 0,
                write: prot & VM_PROT_WRITE != 0,
                exec: prot & VM_PROT_EXECUTE != 0,
                offset,
                path,
            };
            match out.last_mut() {
                Some(l)
                    if l.end == m.start
                        && (l.read, l.write, l.exec) == (m.read, m.write, m.exec)
                        && l.path == m.path =>
                {
                    l.end = m.end;
                }
                _ => out.push(m),
            }
        }
        Ok(out)
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        let mut out = self.dyld_images();
        if out.is_empty() {
            out = self.headers();
        }
        if let Some(i) = out.iter().position(|m| m.path == self.exe) {
            out[..=i].rotate_right(1);
        } else if let Some(i) = out.iter().position(|m| {
            let mut h = [0u8; 16];
            // the program is the one MH_EXECUTE image; for an attach it has no name given
            self.read(m.base, &mut h) == 16 && u32::from_le_bytes([h[12], h[13], h[14], h[15]]) == MH_EXECUTE
        }) {
            out[..=i].rotate_right(1);
        }
        Ok(out)
    }

    fn entry(&mut self) -> Option<u64> {
        let base = match self.exe_base {
            Some(b) => b,
            None => {
                let b = self.modules().ok()?.first()?.base;
                self.exe_base = Some(b);
                b
            }
        };
        let mut h = [0u8; 32];
        if self.read(base, &mut h) < 32 || u32::from_le_bytes([h[0], h[1], h[2], h[3]]) != MH_MAGIC_64 {
            return None;
        }
        let size = u32::from_le_bytes([h[20], h[21], h[22], h[23]]) as usize;
        let mut cmds = vec![0u8; size.min(1 << 20)];
        let n = self.read(base + 32, &mut cmds);
        let cmds = &cmds[..n];
        let mut at = 0;
        while at + 16 <= cmds.len() {
            let cmd = u32::from_le_bytes(cmds[at..at + 4].try_into().ok()?);
            let len = u32::from_le_bytes(cmds[at + 4..at + 8].try_into().ok()?) as usize;
            if cmd == LC_MAIN {
                // entryoff counts from the start of __TEXT, which is where the header sits
                return Some(base.wrapping_add(u64::from_le_bytes(cmds[at + 8..at + 16].try_into().ok()?)));
            }
            if len < 8 {
                break;
            }
            at += len;
        }
        None
    }

    fn set_patches(&mut self, patches: &[(u64, u8)]) {
        // kept after a breakpoint leaves: a child forked while it was in still has it (see `under`)
        self.under.extend(patches.iter().copied());
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        self.watch = *slots;
        let d = self.debug_regs();
        for &tid in self.th.keys() {
            if !self.set_debug_state(tid, &d) {
                return fail(format!("hardware watch: the debug registers of thread {tid} cannot be set"));
            }
        }
        // threads born later start from the task's debug registers
        // SAFETY: 8 words, the 16 halves the count says.
        unsafe { task_set_state(self.task, X86_DEBUG_STATE64, d.as_ptr().cast(), X86_DEBUG_STATE64_COUNT) };
        Ok(())
    }

    fn protect(&mut self, addr: u64, len: u64, prot: u8) -> Result<()> {
        // the bits are the same as VM_PROT_READ, VM_PROT_WRITE, VM_PROT_EXECUTE
        // SAFETY: protection changes on pages of the task's own.
        let kr = unsafe { mach_vm_protect(self.task, addr, len, 0, c_int::from(prot & 7)) };
        if kr != KERN_SUCCESS {
            return fail(format!("mach_vm_protect({addr:#x}, {len:#x}): {}", kr_text(kr)));
        }
        Ok(())
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        if self.gone {
            return fail("the program has ended");
        }
        if self.running {
            return Ok(());
        }
        match how {
            Resume::All => {
                for tid in self.th.keys().copied().collect::<Vec<_>>() {
                    self.answer(tid);
                }
            }
            Resume::Step(one) => {
                let one = one as Port;
                if !self.th.contains_key(&one) {
                    return fail(format!("no thread {one}"));
                }
                let mut s = self.state(one)?;
                s[17] |= TF;
                self.set_state(one, &s)?;
                for &t in self.th.keys() {
                    // SAFETY: a thread of the program; released at the next stop.
                    if t != one && unsafe { thread_suspend(t) } == KERN_SUCCESS {
                        self.held.push(t);
                    }
                }
                self.answer(one);
            }
        }
        self.go_on();
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if self.gone {
            return fail("the program has ended");
        }
        let mut events = Vec::new();
        if !self.running {
            if !self.want_stop {
                return fail("the program is stopped: nothing to wait for");
            }
            // what came before the stop counts first
            while let Some(e) = self.receive(0) {
                self.take(e, &mut events);
            }
            self.want_stop = false;
            if events.is_empty() {
                let tid = self.th.keys().next().copied().unwrap_or_default();
                events.push(Event::Stopped { tid: tid as Tid, stop: Stop::Interrupt });
            }
            self.release_held();
            self.refresh();
            return Ok(events);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = left.as_millis().clamp(0, 10) as u32;
            if let Some(e) = self.receive(ms) {
                self.take(e, &mut events);
                while let Some(e) = self.receive(0) {
                    self.take(e, &mut events);
                }
            }
            if let Some(ev) = self.reap() {
                events.push(ev);
                return Ok(events);
            }
            if !self.running {
                if events.is_empty() {
                    // only a signal the kernel made of a fault already shown: it goes on
                    for tid in self.th.keys().copied().collect::<Vec<_>>() {
                        if self.th.get(&tid).is_some_and(|t| t.pend.is_some_and(|p| p.soft)) {
                            self.answer(tid);
                        }
                    }
                    self.go_on();
                } else {
                    self.release_held();
                    self.refresh();
                    return Ok(events);
                }
            }
            if Instant::now() >= deadline {
                return Ok(events);
            }
        }
    }

    fn load_hook(&mut self) -> Option<u64> {
        // dyld_all_image_infos: after the image count and the image array comes the function dyld calls each time
        // it adds or removes images; the module list is read again from that array at the hit
        let mut info = [0u32; TASK_DYLD_INFO_COUNT as usize];
        let mut n = TASK_DYLD_INFO_COUNT;
        // SAFETY: `info` holds the words `n` says.
        let kr = unsafe { libc::task_info(self.task, TASK_DYLD_INFO, info.as_mut_ptr().cast(), &mut n) };
        let all = u64::from(info[0]) | u64::from(info[1]) << 32;
        if kr != KERN_SUCCESS || all == 0 {
            return None;
        }
        self.read_word(all + 16).filter(|&f| f != 0)
    }

    fn interrupt(&mut self) -> Result<()> {
        if self.gone || !self.running {
            return fail("the program is already stopped");
        }
        // SAFETY: the task port is ours.
        let kr = unsafe { task_suspend(self.task) };
        if kr != KERN_SUCCESS {
            return fail(format!("stop: {}", kr_text(kr)));
        }
        self.running = false;
        self.want_stop = true;
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
        if self.gone {
            return Ok(());
        }
        if self.running {
            // SAFETY: the task port is ours.
            unsafe { task_suspend(self.task) };
            self.running = false;
        }
        let mut events = Vec::new();
        while let Some(e) = self.receive(0) {
            self.take(e, &mut events);
        }
        self.release_held();
        self.watch = [None; 4];
        let d = [0u64; 8];
        for &tid in self.th.keys() {
            self.set_debug_state(tid, &d);
        }
        // SAFETY: the task port is ours; threads started later get no watchpoint either.
        unsafe { task_set_state(self.task, X86_DEBUG_STATE64, d.as_ptr().cast(), X86_DEBUG_STATE64_COUNT) };
        // macOS takes PT_DETACH only while a thread waits on a signal (the process is in its stopped state then).
        // A stop at a breakpoint, a step or an interrupt is not one: nexo sends SIGSTOP and drops it on the way
        // out. A fault becomes its signal and goes to the program with the detach, as it would without a debugger.
        let ours = self.waits_on_signal().is_none() && !self.th.values().any(|t| t.fault && t.pend.is_some());
        self.answer_all_but_signals();
        if ours {
            // SAFETY: a signal to the process we trace.
            unsafe { libc::kill(self.pid, libc::SIGSTOP) };
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let sig = loop {
            match self.waits_on_signal() {
                Some((_, sig)) if !ours => break Some(sig),
                Some((_, libc::SIGSTOP)) => break Some(0),
                // a signal of the program's own came first: it gets it
                Some((tid, _)) => self.answer(tid),
                None => {}
            }
            if Instant::now() > deadline {
                break None;
            }
            self.go_on();
            match self.receive(50) {
                Some(e) => {
                    self.take(e, &mut events);
                    self.answer_all_but_signals();
                }
                None => {
                    if self.reap().is_some() {
                        return Ok(());
                    }
                }
            }
        };
        // SAFETY: give the program back the exception ports it had; the rights handed to us go with them.
        unsafe {
            task_set_exception_ports(self.task, EXC_MASK, MACH_PORT_NULL, EXCEPTION_DEFAULT, THREAD_STATE_NONE);
            for (mask, port, behavior, flavor) in std::mem::take(&mut self.old) {
                task_set_exception_ports(self.task, mask, port, behavior, flavor);
                if port != MACH_PORT_NULL {
                    mach_port_deallocate(me(), port);
                }
            }
        }
        let done = match sig {
            // SAFETY: data is the signal the program gets as it goes on (0: none).
            Some(sig) => unsafe {
                libc::ptrace(libc::PT_DETACH, self.pid, std::ptr::without_provenance_mut::<c_char>(1), sig) == 0
            },
            None => false,
        };
        let why = io::Error::last_os_error();
        // the waiting threads go on: the signal stop with what PT_DETACH said, anything else as it was
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            let p = self.th.get_mut(&tid).and_then(|t| if t.fault { None } else { t.pend.take() });
            match p {
                Some(p) => self.reply(p, KERN_SUCCESS),
                None => self.answer(tid),
            }
        }
        if !self.running {
            // SAFETY: undoes our own suspension (PT_DETACH may already have).
            unsafe { task_resume(self.task) };
        }
        self.ended();
        if !done {
            let why = if sig.is_none() {
                "it did not stop for the SIGSTOP within 3 seconds".to_string()
            } else {
                why.to_string()
            };
            return fail(format!(
                "ptrace(PT_DETACH): {why}; the program runs on, but stays traced by this nexo process"
            ));
        }
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        // SAFETY: the process is the one we trace.
        unsafe { libc::kill(self.pid, libc::SIGKILL) };
        // every waiting thread is let go and the task runs, so the kernel can end it
        for tid in self.th.keys().copied().collect::<Vec<_>>() {
            if let Some(t) = self.th.get_mut(&tid) {
                (t.fault, t.sig) = (false, 0);
            }
            self.answer(tid);
        }
        self.release_held();
        self.go_on();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            while let Some(e) = self.receive(0) {
                // a late exception of a dying thread
                self.reply(e.pend, KERN_SUCCESS);
                // SAFETY: rights that came with the message.
                unsafe {
                    mach_port_deallocate(me(), e.task);
                    mach_port_deallocate(me(), e.thread);
                }
            }
            if self.reap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.ended();
        Ok(())
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}
