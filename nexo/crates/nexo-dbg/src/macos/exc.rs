//! Mach exception messages: receiving them, replying, and turning them into events.

use super::*;

impl Mac {
    /// Take the program's exceptions on a port of ours, keeping the ports it had for a detach.
    pub(super) fn catch_exceptions(&mut self) -> Result<()> {
        let mut port = MACH_PORT_NULL;
        // SAFETY: plain Mach calls on ports of this process.
        let kr = unsafe {
            let kr = mach_port_allocate(me(), MACH_PORT_RIGHT_RECEIVE, &mut port);
            if kr == KERN_SUCCESS {
                mach_port_insert_right(me(), port, port, MACH_MSG_TYPE_MAKE_SEND)
            } else {
                kr
            }
        };
        if kr != KERN_SUCCESS {
            return fail(format!("cannot make an exception port: {}", kr_text(kr)));
        }
        self.port = port;
        let mut masks = [0u32; EXC_TYPES_COUNT];
        let mut ports = [0 as Port; EXC_TYPES_COUNT];
        let mut behaviors = [0 as c_int; EXC_TYPES_COUNT];
        let mut flavors = [0 as c_int; EXC_TYPES_COUNT];
        let mut n = EXC_TYPES_COUNT as u32;
        // SAFETY: every array holds EXC_TYPES_COUNT entries, as `n` says.
        let kr = unsafe {
            task_swap_exception_ports(
                self.task,
                EXC_MASK,
                port,
                EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES,
                THREAD_STATE_NONE,
                masks.as_mut_ptr(),
                &mut n,
                ports.as_mut_ptr(),
                behaviors.as_mut_ptr(),
                flavors.as_mut_ptr(),
            )
        };
        if kr != KERN_SUCCESS {
            return fail(format!("cannot take the program's exceptions: {}", kr_text(kr)));
        }
        self.old =
            (0..(n as usize).min(EXC_TYPES_COUNT)).map(|i| (masks[i], ports[i], behaviors[i], flavors[i])).collect();
        Ok(())
    }

    /// `PT_ATTACHEXC`: signals come as exceptions, and the kernel lets breakpoints change the program's code. It
    /// sends the program a SIGSTOP, which `first_stop` takes.
    pub(super) fn trace(&mut self) -> Result<()> {
        // SAFETY: a plain system call on a process of this user.
        if unsafe { libc::ptrace(libc::PT_ATTACHEXC, self.pid, std::ptr::null_mut(), 0) } != 0 {
            let e = io::Error::last_os_error();
            return fail(format!("ptrace(PT_ATTACHEXC): {e} (another debugger may hold it)"));
        }
        Ok(())
    }

    /// Wait for the SIGSTOP that `PT_ATTACHEXC` sent and keep the program stopped on it; it is dropped when the
    /// program goes on.
    pub(super) fn first_stop(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let Some(e) = self.receive(50) else {
                if let Some(ev) = self.reap() {
                    return fail(format!("it ended before it started ({ev:?})"));
                }
                continue;
            };
            let stop = e.exc == EXC_SOFTWARE && e.code[0] == EXC_SOFT_SIGNAL && e.code[1] == i64::from(libc::SIGSTOP);
            let thread = e.thread;
            let mut events = Vec::new();
            self.take(e, &mut events);
            if stop {
                if let Some(t) = self.th.get_mut(&thread) {
                    t.sig = 0;
                }
                self.refresh();
                return Ok(());
            }
            // anything before it (a trap in the program's own start) goes on as it would without a debugger
            if let Some(t) = self.th.get_mut(&thread) {
                t.fault = false;
            }
            self.answer(thread);
            self.go_on();
        }
        fail("it did not stop for the debugger within 10 seconds")
    }

    /// The next exception message, waiting at most `ms` milliseconds.
    pub(super) fn receive(&mut self, ms: u32) -> Option<Exc> {
        loop {
            let mut buf = [0u64; 128];
            let head = buf.as_mut_ptr().cast::<MsgHeader>();
            // SAFETY: the buffer is 1 KiB, as the receive size says; the kernel writes the message into it.
            let kr = unsafe { mach_msg(head, MACH_RCV_MSG | MACH_RCV_TIMEOUT, 0, 1024, self.port, ms, MACH_PORT_NULL) };
            if kr != KERN_SUCCESS {
                return None;
            }
            let b: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
            let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
            let i64_at = |o: usize| i64::from_le_bytes(b[o..o + 8].try_into().unwrap_or_default());
            let pend = Pend { reply: u32_at(8), bits: u32_at(0), id: u32_at(20) as i32, soft: false };
            if pend.id != MACH_EXCEPTION_RAISE || u32_at(4) < 84 {
                // not an exception request: nothing waits on it
                continue;
            }
            let exc = u32_at(60) as i32;
            let n = u32_at(64).min(2) as usize;
            let mut code = [0i64; 2];
            for (i, c) in code.iter_mut().enumerate().take(n) {
                *c = i64_at(68 + 8 * i);
            }
            let soft = exc == EXC_SOFTWARE && code[0] == EXC_SOFT_SIGNAL;
            return Some(Exc { thread: u32_at(28), task: u32_at(40), exc, code, pend: Pend { soft, ..pend } });
        }
    }

    /// Let a child the program forked go on without the debugger, as Linux lets one go: the program's bytes go back
    /// under the breakpoints it copied, its exception ports become the ones the program had before nexo took them,
    /// and the trap that brought it here is undone. Returns the reply: success resumes the child, failure lets the
    /// kernel make of the exception the signal the child would get without a debugger.
    fn release_child(&self, e: &Exc) -> Kr {
        let child = e.task;
        let byte_at = |addr: u64| {
            let (mut b, mut got) = ([0u8; 1], 0u64);
            // SAFETY: the kernel writes at most one byte into `b`.
            let kr = unsafe { mach_vm_read_overwrite(child, addr, 1, b.as_mut_ptr() as u64, &mut got) };
            (kr == KERN_SUCCESS && got == 1).then_some(b[0])
        };
        let mut pid: c_int = 0;
        // SAFETY: one int out.
        let known = unsafe { pid_for_task(child, &mut pid) } == KERN_SUCCESS;
        for (&addr, &byte) in &self.under {
            // only where the child still maps the program's own file: a child that ran exec has other code there
            let same = known && {
                let file = self.file_at(addr);
                !file.is_empty() && file == self.named(&region_file(pid, addr))
            };
            if same && byte != 0xcc && byte_at(addr) == Some(0xcc) {
                let _ = patch(child, addr, &[byte]);
            }
        }
        // SAFETY: the child's task port came with the message; the call copies the send rights in `old`, which
        // stay ours for the program's own detach.
        unsafe {
            task_set_exception_ports(child, EXC_MASK, MACH_PORT_NULL, EXCEPTION_DEFAULT, THREAD_STATE_NONE);
            for &(mask, port, behavior, flavor) in &self.old {
                task_set_exception_ports(child, mask, port, behavior, flavor);
            }
        }
        // the copied thread may carry the watch registers: nothing watches the child, and a hit would kill it
        let mut d = [0u64; 8];
        let watched = self.debug_state(e.thread, &mut d) && d[7] != 0;
        if watched {
            self.set_debug_state(e.thread, &[0; 8]);
        }
        let Ok(mut s) = self.state(e.thread) else { return KERN_FAILURE };
        match e.exc {
            EXC_BREAKPOINT if e.code[0] == EXC_I386_BPT => {
                // the int3 has run: go back to its address, where the program's instruction is again
                let at = s[16].wrapping_sub(1);
                if !self.under.contains_key(&at) || byte_at(at).is_none_or(|b| b == 0xcc) {
                    return KERN_FAILURE;
                }
                s[16] = at;
            }
            // a hit of the watch registers, or the trap flag of a step over the fork the thread was copied in
            EXC_BREAKPOINT if e.code[0] == EXC_I386_SGL && (watched || s[17] & TF != 0) => s[17] &= !TF,
            _ => return KERN_FAILURE,
        }
        if self.set_state(e.thread, &s).is_err() {
            return KERN_FAILURE;
        }
        KERN_SUCCESS
    }

    pub(super) fn reply(&self, p: Pend, ret: Kr) {
        let mut r = Reply {
            head: MsgHeader {
                bits: p.bits & 0x1f,
                size: std::mem::size_of::<Reply>() as u32,
                remote: p.reply,
                local: MACH_PORT_NULL,
                voucher: MACH_PORT_NULL,
                id: p.id + 100,
            },
            ndr: NDR,
            ret,
        };
        // SAFETY: a complete reply message; the send-once right in `remote` is used up by the send.
        unsafe {
            mach_msg(
                &mut r.head,
                MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                std::mem::size_of::<Reply>() as u32,
                0,
                MACH_PORT_NULL,
                100,
                MACH_PORT_NULL,
            );
        }
    }

    /// Take in one exception: the task is stopped, and what it means joins `events` (nothing for a signal the
    /// kernel made of a fault already shown).
    pub(super) fn take(&mut self, e: Exc, events: &mut Vec<Event>) {
        if e.task != self.task {
            // a child the program forked keeps our exception port and the breakpoints of the code it copied
            let ret = self.release_child(&e);
            self.reply(e.pend, ret);
            // SAFETY: both ports came with the message and are not ones we keep.
            unsafe {
                mach_port_deallocate(me(), e.task);
                mach_port_deallocate(me(), e.thread);
            }
            return;
        }
        // SAFETY: the message gave us one more reference on the task port; the one we keep holds the name.
        unsafe {
            mach_port_deallocate(me(), e.task);
        }
        if self.th.contains_key(&e.thread) {
            // SAFETY: as above; the name stays valid through the reference we already hold.
            unsafe { mach_port_deallocate(me(), e.thread) };
        }
        if self.running {
            // SAFETY: the task port is ours.
            unsafe { task_suspend(self.task) };
            self.running = false;
        }
        let tid = e.thread;
        let t = self.th.entry(tid).or_default();
        if let Some(old) = t.pend.replace(e.pend) {
            // one thread waits on one exception at a time; an old right is stale
            // SAFETY: a send-once right of ours.
            unsafe { mach_port_deallocate(me(), old.reply) };
        }
        let stopped = |stop| Event::Stopped { tid: tid as Tid, stop };
        match e.exc {
            EXC_SOFTWARE if e.pend.soft => {
                let sig = e.code[1] as i32;
                if t.pass != 0 && t.pass == sig {
                    t.pass = 0;
                    t.sig = sig;
                    return;
                }
                t.sig = sig;
                events.push(stopped(Stop::Fault(Fault {
                    number: sig as u32,
                    code: 0,
                    addr: None,
                    touch: None,
                    first_chance: true,
                })));
            }
            EXC_BREAKPOINT if e.code[0] == EXC_I386_BPT => events.push(stopped(Stop::Breakpoint)),
            EXC_BREAKPOINT if e.code[0] == EXC_I386_SGL => {
                let mut d = [0u64; 8];
                let hit = if self.debug_state(tid, &mut d) {
                    let hit = (d[6] & 0xf) as u8;
                    d[6] = 0;
                    self.set_debug_state(tid, &d);
                    hit
                } else {
                    0
                };
                // the trap flag stays set in the saved rflags: clear it, or the next run is one instruction too
                if let Ok(mut s) = self.state(tid) {
                    if s[17] & TF != 0 {
                        s[17] &= !TF;
                        let _ = self.set_state(tid, &s);
                    }
                }
                events.push(stopped(if hit != 0 { Stop::Watch(hit) } else { Stop::Step }));
            }
            EXC_BAD_ACCESS | EXC_BAD_INSTRUCTION | EXC_ARITHMETIC => {
                // the signal the kernel makes of it (bsd/uxkern/ux_exception.c and its i386 part)
                let sig = match e.exc {
                    EXC_BAD_ACCESS if matches!(e.code[0], KERN_INVALID_ADDRESS | EXC_I386_GPFLT) => libc::SIGSEGV,
                    EXC_BAD_ACCESS => libc::SIGBUS,
                    EXC_BAD_INSTRUCTION => libc::SIGILL,
                    _ => libc::SIGFPE,
                };
                t.sig = sig;
                t.fault = true;
                let addr = (e.exc == EXC_BAD_ACCESS && e.code[0] != EXC_I386_GPFLT).then_some(e.code[1] as u64);
                events.push(stopped(Stop::Fault(Fault {
                    number: sig as u32,
                    code: e.code[0] as i32,
                    addr,
                    touch: None,
                    first_chance: true,
                })));
            }
            _ => {
                // an EXC_BREAKPOINT of another kind, or EXC_SOFTWARE that is not a signal: SIGTRAP is what it becomes
                t.sig = libc::SIGTRAP;
                t.fault = true;
                events.push(stopped(Stop::Fault(Fault {
                    number: libc::SIGTRAP as u32,
                    code: 0,
                    addr: None,
                    touch: None,
                    first_chance: true,
                })));
            }
        }
    }

    /// Reply to the exception `tid` waits on, with the signal it carries going on.
    pub(super) fn answer(&mut self, tid: Port) {
        let pid = self.pid;
        let Some(t) = self.th.get_mut(&tid) else { return };
        let Some(p) = t.pend.take() else { return };
        let sig = std::mem::take(&mut t.sig);
        if std::mem::take(&mut t.fault) {
            // "not handled": the kernel makes the fault the signal it would be without a debugger
            t.pass = sig;
            self.reply(p, KERN_FAILURE);
            return;
        }
        if p.soft {
            // SAFETY: PT_THUPDATE names the thread by its port in this process and sets the signal it gets (0: none).
            unsafe { libc::ptrace(libc::PT_THUPDATE, pid, tid as usize as *mut c_char, sig) };
        }
        self.reply(p, KERN_SUCCESS);
    }

    /// Let the task run again after a stop nobody needs to see.
    pub(super) fn go_on(&mut self) {
        if !self.running {
            // SAFETY: undoes our own task_suspend.
            unsafe { task_resume(self.task) };
            self.running = true;
        }
    }

    /// The thread that waits on a signal (`EXC_SOFT_SIGNAL`), with the signal it is to get.
    pub(super) fn waits_on_signal(&self) -> Option<(Port, c_int)> {
        self.th.iter().find(|(_, t)| t.pend.is_some_and(|p| p.soft)).map(|(k, t)| (*k, t.sig))
    }

    /// Reply to every exception but the signal stops: breakpoints and steps go on, faults become their signals.
    pub(super) fn answer_all_but_signals(&mut self) {
        let wait: Vec<Port> =
            self.th.iter().filter(|(_, t)| t.pend.is_some_and(|p| !p.soft)).map(|(k, _)| *k).collect();
        for tid in wait {
            self.answer(tid);
        }
    }

    pub(super) fn release_held(&mut self) {
        for t in std::mem::take(&mut self.held) {
            // SAFETY: undoes our own thread_suspend; the task stays suspended.
            unsafe { thread_resume(t) };
        }
    }

    /// Whether the program ended; the event that says how.
    pub(super) fn reap(&mut self) -> Option<Event> {
        let mut st = 0;
        // SAFETY: a plain wait on the program, which is our child (PT_ATTACHEXC makes the debugger its parent).
        let r = unsafe { libc::waitpid(self.pid, &mut st, libc::WNOHANG) };
        let ev = if r == self.pid && libc::WIFEXITED(st) {
            Event::Exited { code: libc::WEXITSTATUS(st) }
        } else if r == self.pid && libc::WIFSIGNALED(st) {
            Event::Killed { signal: libc::WTERMSIG(st) }
        } else if r < 0 && !alive(self.pid) {
            Event::Exited { code: -1 }
        } else {
            return None;
        };
        // A traced child reaped once goes back to the parent it had before the attach, which for a program nexo
        // started is this process again, still a zombie: the second wait frees it.
        // SAFETY: as above.
        unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), libc::WNOHANG) };
        self.ended();
        Some(ev)
    }

    pub(super) fn ended(&mut self) {
        self.gone = true;
        self.running = false;
        self.held.clear();
        for (tid, t) in std::mem::take(&mut self.th) {
            // SAFETY: rights of ours on ports of a task that is gone.
            unsafe {
                if let Some(p) = t.pend {
                    mach_port_deallocate(me(), p.reply);
                }
                mach_port_deallocate(me(), tid);
            }
        }
    }

    /// Read the thread list again: names new to us are kept, the ones of threads that ended are let go.
    pub(super) fn refresh(&mut self) {
        let mut list: libc::thread_act_array_t = std::ptr::null_mut();
        let mut n = 0u32;
        // SAFETY: the kernel hands back an array of `n` send rights in memory of ours, which is freed below.
        let now: Vec<Port> = unsafe {
            if libc::task_threads(self.task, &mut list, &mut n) != KERN_SUCCESS || list.is_null() {
                return;
            }
            let v = std::slice::from_raw_parts(list, n as usize).to_vec();
            libc::vm_deallocate(me(), list as libc::vm_address_t, n as usize * 4);
            v
        };
        for &t in &now {
            match self.th.entry(t) {
                // SAFETY: one more reference on a name we already hold.
                Entry::Occupied(_) => unsafe {
                    mach_port_deallocate(me(), t);
                },
                Entry::Vacant(v) => {
                    v.insert(Th::default());
                }
            }
        }
        let dead: Vec<Port> = self.th.keys().filter(|k| !now.contains(k)).copied().collect();
        for d in dead {
            if let Some(t) = self.th.remove(&d) {
                // SAFETY: rights of ours on a thread that ended.
                unsafe {
                    if let Some(p) = t.pend {
                        mach_port_deallocate(me(), p.reply);
                    }
                    mach_port_deallocate(me(), d);
                }
            }
        }
    }
}
