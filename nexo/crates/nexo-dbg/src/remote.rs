//! A program behind a GDB remote stub (gdbserver, `qemu -g`, an embedded probe, nexo's own `dbg serve --gdb`)
//! seen as a [`Target`], so every command of the debugger works on it as on a local program.
//!
//! x86-64 stubs only: the registers, the instruction text and the stack walk are x86-64. Breakpoints are
//! written into memory as `int3`, like locally, so the stub needs only memory access and `vCont`.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::platform::{Access, Event, Fault, HwSlot, Map, Module, Os, Resume, Stop, Target, Tid};
use crate::regs::Regs;
use crate::rsp::{hex, legacy_layout, parse_target_xml, regs_from_g, regs_into_g, unhex, Conn, Frame, Layout};
use crate::{fail, Error, Result};

const AT_ENTRY: u64 = 9;
const REPLY_WAIT: Duration = Duration::from_secs(10);

pub struct Remote {
    c: Conn,
    addr: String,
    layout: Layout,
    pid: u32,
    packet_max: usize,
    can_vcont: bool,
    can_siginfo: bool,
    can_auxv: bool,
    can_exec_file: bool,
    attached: bool,
    /// The thread `g` and `G` are about now.
    g_tid: Option<Tid>,
    running: bool,
    gone: bool,
    stepping: bool,
    interrupting: bool,
    /// A signal the program got and has not been given back yet: `(thread, number)`.
    sig: Option<(Tid, u8)>,
    patches: Vec<(u64, u8)>,
    tids: Vec<Tid>,
    watch: [Option<HwSlot>; 4],
    out: Vec<u8>,
    /// Where the program's file sits in memory minus where the file says: set once the caller has the file.
    bias: Option<u64>,
    exe: String,
}

fn io(e: std::io::Error) -> Error {
    Error::Fail(format!("the remote stub: {e}"))
}

fn num(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// A thread id as a stub writes it: `1a2`, or `p1.1a2` (the part after the dot).
fn tid_of(s: &str) -> Option<Tid> {
    let t = s.rsplit('.').next()?;
    u32::from_str_radix(t, 16).ok()
}

impl Remote {
    /// Connect to `host:port` and learn what the stub is: its registers, its threads, whether it is stopped.
    pub fn connect(addr: &str) -> Result<Remote> {
        let sa = addr
            .to_socket_addrs()
            .map_err(|e| Error::Usage(format!("{addr:?} is not host:port ({e})")))?
            .next()
            .ok_or_else(|| Error::Usage(format!("{addr:?} is not host:port")))?;
        let s = TcpStream::connect_timeout(&sa, Duration::from_secs(5))
            .map_err(|e| Error::Fail(format!("cannot connect to {addr}: {e}")))?;
        let mut r = Remote {
            c: Conn::new(s),
            addr: addr.to_string(),
            layout: legacy_layout(),
            pid: 0,
            packet_max: 1000,
            can_vcont: false,
            can_siginfo: false,
            can_auxv: false,
            can_exec_file: false,
            attached: false,
            g_tid: None,
            running: false,
            gone: false,
            stepping: false,
            interrupting: false,
            sig: None,
            patches: Vec::new(),
            tids: Vec::new(),
            watch: [None; 4],
            out: Vec::new(),
            bias: None,
            exe: String::new(),
        };
        r.handshake(sa.ip().is_loopback())?;
        Ok(r)
    }

    fn handshake(&mut self, loopback: bool) -> Result<()> {
        let sup = self.text("qSupported:swbreak+;hwbreak+;vContSupported+;xmlRegisters=i386")?;
        for f in sup.split(';') {
            match f.split_once('=') {
                Some(("PacketSize", v)) => self.packet_max = num(v).unwrap_or(1000) as usize,
                _ => match f {
                    "qXfer:siginfo:read+" => self.can_siginfo = true,
                    "qXfer:auxv:read+" => self.can_auxv = true,
                    "qXfer:exec-file:read+" => self.can_exec_file = true,
                    _ => {}
                },
            }
        }
        self.can_vcont = self.text("vCont?").is_ok_and(|s| s.starts_with("vCont;"));
        self.attached = self.text("qAttached").is_ok_and(|s| s == "1");
        // the first stop: where the stub left the program
        let first = self.text("?")?;
        if first.starts_with('W') || first.starts_with('X') {
            return fail(format!("the program behind {} has already ended ({first})", self.addr));
        }
        self.layout_from_stub()?;
        let tids = self.thread_list();
        self.tids = tids.clone();
        let first_tid = tids.first().copied().or_else(|| {
            let qc = self.text("qC").ok()?;
            tid_of(qc.strip_prefix("QC")?)
        });
        // a program on another machine has a process number that means nothing here
        self.pid = if loopback { first_tid.unwrap_or(0) } else { 0 };
        if first_tid.is_none() {
            return fail("the stub does not list any thread: it is not stopped on a program");
        }
        self.g_tid = None;
        self.exe = self.fetch_xfer("exec-file", &format!("{:x}", first_tid.unwrap_or(0))).unwrap_or_default();
        Ok(())
    }

    fn layout_from_stub(&mut self) -> Result<()> {
        let Some(xml) = self.fetch_xfer("features", "target.xml") else { return Ok(()) };
        let mut fetch = |name: &str| -> Option<String> { self.fetch_xfer("features", name) };
        let full = crate::rsp::expand_includes(&xml, &mut fetch, 0);
        let (arch, layout) = parse_target_xml(&full);
        if let Some(a) = arch.filter(|a| !a.contains("x86-64") && !a.contains("x86_64")) {
            return fail(format!(
                "the remote program is {a}: nexo dbg reads x86-64 remotes only (its registers and instruction text are x86-64)"
            ));
        }
        if layout.slots.iter().any(|s| s.name == "rax") && layout.slots.iter().any(|s| s.name == "rip") {
            self.layout = layout;
        }
        Ok(())
    }

    /// `qXfer:<object>:read:<annex>:off,len` read to the end.
    fn fetch_xfer(&mut self, object: &str, annex: &str) -> Option<String> {
        let mut all = Vec::new();
        let step = (self.packet_max.saturating_sub(8) / 2).clamp(64, 0x3000);
        loop {
            let r = self.call(format!("qXfer:{object}:read:{annex}:{:x},{:x}", all.len(), step).as_bytes()).ok()?;
            match r.split_first() {
                Some((b'm', rest)) => all.extend_from_slice(rest),
                Some((b'l', rest)) => {
                    all.extend_from_slice(rest);
                    return String::from_utf8(all).ok();
                }
                _ => return None,
            }
            if all.len() > 1 << 20 {
                return None;
            }
        }
    }

    fn text(&mut self, cmd: &str) -> Result<String> {
        let r = self.call(cmd.as_bytes())?;
        Ok(String::from_utf8_lossy(&r).into_owned())
    }

    /// Send a packet and wait for the answer (`O` console lines on the way go to the output).
    fn call(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        self.c.send(payload).map_err(io)?;
        let deadline = Instant::now() + REPLY_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.c.recv(left).map_err(io)? {
                Some(Frame::Pkt(p)) => {
                    if p.first() == Some(&b'O') && p.len() > 2 && p[1] != b'K' {
                        if let Some(b) = unhex(&p[1..]) {
                            self.out.extend(b);
                            continue;
                        }
                    }
                    return Ok(p);
                }
                Some(Frame::Interrupt) => continue,
                None => {
                    return fail(format!("the stub at {} did not answer {:?} in 10 seconds", self.addr, trim(payload)))
                }
            }
        }
    }

    fn ok(&mut self, payload: &[u8]) -> Result<()> {
        let r = self.call(payload)?;
        if r == b"OK" {
            Ok(())
        } else {
            fail(format!("the stub refused {:?}: {}", trim(payload), reply_text(&r)))
        }
    }

    fn thread_list(&mut self) -> Vec<Tid> {
        let mut out = Vec::new();
        let mut q = "qfThreadInfo";
        for _ in 0..64 {
            let Ok(r) = self.text(q) else { break };
            let Some(list) = r.strip_prefix('m') else { break };
            out.extend(list.split(',').filter_map(tid_of));
            q = "qsThreadInfo";
        }
        out
    }

    fn select(&mut self, tid: Tid) -> Result<()> {
        if self.g_tid != Some(tid) {
            self.ok(format!("Hg{tid:x}").as_bytes())?;
            self.g_tid = Some(tid);
        }
        Ok(())
    }

    fn read_g(&mut self, tid: Tid) -> Result<Vec<u8>> {
        self.select(tid)?;
        let r = self.call(b"g")?;
        if r.first() == Some(&b'E') {
            return fail(format!("registers of thread {tid}: {}", reply_text(&r)));
        }
        // gdbserver writes `x` for a register it cannot read; those are zeros here
        let r: Vec<u8> = r.iter().map(|c| if *c == b'x' { b'0' } else { *c }).collect();
        let g = unhex(&r).ok_or_else(|| Error::Fail("the stub's register block is not hex".into()))?;
        if g.len() < self.layout.len {
            return fail(format!(
                "the stub gave {} bytes of registers, its description needs {}",
                g.len(),
                self.layout.len
            ));
        }
        Ok(g)
    }

    /// Tell the remote where the program's file sits: needs the file's own entry address and base. Returns the load
    /// bias when the stub gave the entry point in memory.
    pub fn locate(&mut self, file_entry: u64, image_base: u64) -> Option<u64> {
        let mem = self.entry()?;
        let b = mem.wrapping_sub(file_entry);
        self.bias = Some(b.wrapping_add(image_base));
        Some(b)
    }

    /// The program's path as the stub's machine knows it (empty when it did not say).
    pub fn exe_path(&self) -> &str {
        &self.exe
    }

    fn mem_packet(&mut self, addr: u64, len: usize) -> Option<Vec<u8>> {
        let r = self.call(format!("m{addr:x},{len:x}").as_bytes()).ok()?;
        if r.first() == Some(&b'E') || r.is_empty() {
            return None;
        }
        let b = unhex(&r)?;
        (!b.is_empty()).then_some(b)
    }

    fn read_some(&mut self, addr: u64, out: &mut [u8]) -> usize {
        if out.is_empty() {
            return 0;
        }
        if let Some(b) = self.mem_packet(addr, out.len()) {
            let n = b.len().min(out.len());
            out[..n].copy_from_slice(&b[..n]);
            return n;
        }
        if out.len() == 1 {
            return 0;
        }
        let h = out.len() / 2;
        let a = self.read_some(addr, &mut out[..h]);
        if a < h {
            return a;
        }
        a + self.read_some(addr.wrapping_add(h as u64), &mut out[h..])
    }

    /// The text of a file on the stub's machine (`vFile`), at most 1 MiB.
    fn host_file(&mut self, path: &str) -> Option<Vec<u8>> {
        let r = self.call(format!("vFile:open:{},0,0", hex(path.as_bytes())).as_bytes()).ok()?;
        let fd = std::str::from_utf8(r.strip_prefix(b"F")?).ok()?.to_string();
        if fd.starts_with('-') {
            return None;
        }
        let mut all = Vec::new();
        let step = (self.packet_max.saturating_sub(16) / 3).clamp(64, 0x2000);
        let res = loop {
            let Ok(r) = self.call(format!("vFile:pread:{fd},{step:x},{:x}", all.len()).as_bytes()) else { break None };
            let Some(rest) = r.strip_prefix(b"F") else { break None };
            let semi = rest.iter().position(|&b| b == b';').unwrap_or(rest.len());
            let n = std::str::from_utf8(&rest[..semi]).ok().and_then(num).unwrap_or(0) as usize;
            if rest.starts_with(b"-") || n == 0 {
                break Some(all);
            }
            all.extend_from_slice(rest.get(semi + 1..).unwrap_or(&[]));
            if all.len() > 1 << 20 {
                break Some(all);
            }
        };
        let _ = self.call(format!("vFile:close:{fd}").as_bytes());
        res
    }

    fn fault_info(&mut self, sig: u8) -> (i32, Option<u64>) {
        if !self.can_siginfo || !matches!(sig, 4 | 7 | 8 | 11) {
            return (0, None);
        }
        let Some(txt) = self.fetch_xfer("siginfo", "") else { return (0, None) };
        let b = txt.as_bytes();
        if b.len() < 24 {
            return (0, None);
        }
        let code = i32::from_le_bytes(b[8..12].try_into().unwrap_or_default());
        let addr = u64::from_le_bytes(b[16..24].try_into().unwrap_or_default());
        (code, Some(addr))
    }

    /// One stop reply (`T`, `S`, `W`, `X`) as events.
    fn stop_event(&mut self, p: &[u8]) -> Option<Event> {
        let s = String::from_utf8_lossy(p).into_owned();
        let kind = *p.first()?;
        let hex2 = |i: usize| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
        match kind {
            b'W' => {
                self.gone = true;
                Some(Event::Exited { code: hex2(1).unwrap_or(0) as i32 })
            }
            b'X' => {
                self.gone = true;
                Some(Event::Killed { signal: hex2(1).unwrap_or(9) as i32 })
            }
            b'S' | b'T' => {
                let sig = hex2(1)?;
                let mut tid = self.g_tid.or_else(|| self.thread_list().first().copied()).unwrap_or(self.pid);
                let (mut sw, mut watch, mut exec) = (false, None, false);
                if kind == b'T' {
                    for kv in s.get(3..).unwrap_or("").split(';') {
                        let (k, v) = kv.split_once(':').unwrap_or((kv, ""));
                        match k {
                            "thread" => tid = tid_of(v).unwrap_or(tid),
                            "swbreak" | "hwbreak" => sw = true,
                            "watch" | "rwatch" | "awatch" => watch = Some(num(v).unwrap_or(0)),
                            "exec" => exec = true,
                            _ => {}
                        }
                    }
                }
                self.g_tid = None;
                let stop = if sig == 5 {
                    if let Some(a) = watch {
                        let bit = self
                            .watch
                            .iter()
                            .position(|w| w.is_some_and(|w| a >= w.addr && a < w.addr + u64::from(w.len)))
                            .unwrap_or(0);
                        Stop::Watch(1 << bit)
                    } else if exec {
                        Stop::Exec
                    } else if sw || !self.stepping {
                        self.fix_pc(tid);
                        Stop::Breakpoint
                    } else {
                        Stop::Step
                    }
                } else if sig == 2 && self.interrupting {
                    Stop::Interrupt
                } else {
                    self.sig = Some((tid, sig));
                    let (code, addr) = self.fault_info(sig);
                    Stop::Fault(Fault { number: u32::from(sig), code, addr, touch: None, first_chance: true })
                };
                self.interrupting = false;
                Some(Event::Stopped { tid, stop })
            }
            _ => None,
        }
    }

    /// `Stop::Breakpoint` means the counter is one past the `int3`. A stub that backs the counter up itself
    /// (gdbserver does when it knows the breakpoint) left it on our `int3`: move it past.
    fn fix_pc(&mut self, tid: Tid) {
        let Ok(mut r) = Target::regs(self, tid) else { return };
        let here = self.patches.iter().any(|p| p.0 == r.rip);
        let before = self.patches.iter().any(|p| p.0 == r.rip.wrapping_sub(1));
        if here && !before {
            r.rip += 1;
            let _ = Target::set_regs(self, tid, &r);
        }
    }

    fn go(&mut self, how: Resume) -> Result<()> {
        let sig = self.sig.take();
        let cmd = if self.can_vcont {
            match how {
                Resume::All => match sig {
                    Some((t, s)) => format!("vCont;C{s:02x}:{t:x};c"),
                    None => "vCont;c".to_string(),
                },
                Resume::Step(t) => match sig.filter(|x| x.0 == t) {
                    Some((_, s)) => format!("vCont;S{s:02x}:{t:x}"),
                    None => format!("vCont;s:{t:x}"),
                },
            }
        } else {
            match how {
                Resume::All => {
                    self.ok(b"Hc-1")?;
                    match sig {
                        Some((_, s)) => format!("C{s:02x}"),
                        None => "c".into(),
                    }
                }
                Resume::Step(t) => {
                    self.ok(format!("Hc{t:x}").as_bytes())?;
                    match sig.filter(|x| x.0 == t) {
                        Some((_, s)) => format!("S{s:02x}"),
                        None => "s".into(),
                    }
                }
            }
        };
        self.stepping = matches!(how, Resume::Step(_));
        self.interrupting = false;
        self.c.send(cmd.as_bytes()).map_err(io)?;
        self.running = true;
        Ok(())
    }
}

fn trim(p: &[u8]) -> String {
    String::from_utf8_lossy(&p[..p.len().min(40)]).into_owned()
}

fn reply_text(r: &[u8]) -> String {
    if r.is_empty() {
        "not supported".into()
    } else {
        String::from_utf8_lossy(r).into_owned()
    }
}

impl Target for Remote {
    fn os(&self) -> Os {
        Os::Linux
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn launched(&self) -> bool {
        false
    }

    fn threads(&self) -> Vec<Tid> {
        self.tids.clone()
    }

    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        let g = self.read_g(tid)?;
        Ok(regs_from_g(&self.layout, &g))
    }

    fn set_regs(&mut self, tid: Tid, regs: &Regs) -> Result<()> {
        let mut g = self.read_g(tid)?;
        g.truncate(self.layout.len);
        regs_into_g(&self.layout, regs, &mut g);
        self.ok(format!("G{}", hex(&g)).as_bytes())
    }

    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let step = (self.packet_max.saturating_sub(8) / 2).clamp(16, 0x2000);
        let mut done = 0;
        while done < buf.len() {
            let end = (done + step).min(buf.len());
            let want = end - done;
            let got = self.read_some(addr.wrapping_add(done as u64), &mut buf[done..end]);
            done += got;
            if got < want {
                break;
            }
        }
        done
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        let step = (self.packet_max.saturating_sub(24) / 2).clamp(16, 0x2000);
        for (i, ch) in bytes.chunks(step).enumerate() {
            let a = addr.wrapping_add((i * step) as u64);
            let r = self.call(format!("M{a:x},{:x}:{}", ch.len(), hex(ch)).as_bytes())?;
            if r != b"OK" {
                return fail(format!("write at {a:#x}: the stub says {}", reply_text(&r)));
            }
        }
        Ok(())
    }

    fn maps(&mut self) -> Result<Vec<Map>> {
        let Some(b) = self.host_file(&format!("/proc/{}/maps", self.pid)) else {
            return fail("this stub gives no memory map (no vFile access to /proc/<pid>/maps)");
        };
        Ok(String::from_utf8_lossy(&b).lines().filter_map(crate::rsp::parse_map_line).collect())
    }

    fn modules(&mut self) -> Result<Vec<Module>> {
        if let Ok(maps) = self.maps() {
            let mut out: Vec<Module> = Vec::new();
            for m in maps {
                if m.path.starts_with('/') && m.offset == 0 && !out.iter().any(|o| o.path == m.path) {
                    out.push(Module { path: m.path, base: m.start });
                }
            }
            if let Some(i) = out.iter().position(|m| m.path == self.exe) {
                out[..=i].rotate_right(1);
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        // no map: the program alone, where its entry point says it is
        match self.bias {
            Some(b) => Ok(vec![Module { path: self.exe.clone(), base: b }]),
            None => fail("the stub does not say where the program is loaded"),
        }
    }

    fn entry(&mut self) -> Option<u64> {
        if !self.can_auxv {
            return None;
        }
        let mut all = Vec::new();
        loop {
            let r = self.call(format!("qXfer:auxv:read::{:x},400", all.len()).as_bytes()).ok()?;
            match r.split_first() {
                Some((b'm', rest)) => all.extend_from_slice(rest),
                Some((b'l', rest)) => {
                    all.extend_from_slice(rest);
                    break;
                }
                _ => return None,
            }
        }
        all.as_chunks::<16>().0.iter().find_map(|c| {
            let key = u64::from_le_bytes(c[..8].try_into().ok()?);
            (key == AT_ENTRY).then(|| u64::from_le_bytes(c[8..].try_into().unwrap_or_default()))
        })
    }

    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        for w in self.watch.into_iter().flatten() {
            let z = match w.access {
                Access::Exec => 1,
                Access::Write => 2,
                Access::ReadWrite => 4,
            };
            let _ = self.call(format!("z{z},{:x},{:x}", w.addr, w.len).as_bytes());
        }
        self.watch = [None; 4];
        for (i, w) in slots.iter().enumerate() {
            let Some(w) = w else { continue };
            let z = match w.access {
                Access::Exec => 1,
                Access::Write => 2,
                Access::ReadWrite => 4,
            };
            let r = self.call(format!("Z{z},{:x},{:x}", w.addr, w.len).as_bytes())?;
            if r != b"OK" {
                return fail(format!("this stub cannot set a hardware watch there: {}", reply_text(&r)));
            }
            self.watch[i] = Some(*w);
        }
        Ok(())
    }

    fn set_patches(&mut self, patches: &[(u64, u8)]) {
        self.patches = patches.to_vec();
    }

    fn resume(&mut self, how: Resume) -> Result<()> {
        if self.gone {
            return fail("the program has ended");
        }
        self.go(how)
    }

    fn wait(&mut self, timeout: Duration) -> Result<Vec<Event>> {
        if self.gone {
            return fail("the program has ended");
        }
        if !self.running {
            return fail("the program is stopped: nothing to wait for");
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.c.recv(left).map_err(io) {
                Ok(Some(Frame::Pkt(p))) => {
                    if p.first() == Some(&b'O') && p.len() > 1 && p != b"OK" {
                        if let Some(b) = unhex(&p[1..]) {
                            self.out.extend(b);
                            continue;
                        }
                    }
                    self.running = false;
                    let ev = self.stop_event(&p);
                    if !self.gone {
                        self.tids = self.thread_list();
                    }
                    return Ok(ev.into_iter().collect());
                }
                Ok(Some(Frame::Interrupt)) => continue,
                Ok(None) => return Ok(Vec::new()),
                Err(_) => {
                    self.gone = true;
                    self.running = false;
                    return Ok(vec![Event::Exited { code: -1 }]);
                }
            }
        }
    }

    fn interrupt(&mut self) -> Result<()> {
        if !self.running {
            return fail("the program is already stopped");
        }
        self.interrupting = true;
        self.c.send_interrupt().map_err(io)
    }

    fn stdin(&mut self, _bytes: Option<&[u8]>) -> Result<usize> {
        fail("this program runs behind a remote stub: its input belongs to whoever started the stub")
    }

    fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn detach(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        if self.running {
            let _ = self.c.send_interrupt();
            let _ = self.wait(Duration::from_secs(2));
        }
        self.gone = true;
        let _ = self.call(b"D");
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        if self.gone {
            return Ok(());
        }
        self.gone = true;
        // the stub closes the connection without an answer
        let _ = self.c.send(b"k");
        Ok(())
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = Target::detach(self);
    }
}
