//! `nexo dbg serve --gdb <port>`: nexo's own debugger as a GDB remote stub, so gdb, lldb or another nexo can
//! drive a program that nexo started or attached to. Bound to 127.0.0.1 only; one client; the program ends
//! (killed if nexo started it, let go if nexo attached) when the client detaches, kills or goes away.
//!
//! The x87 and vector registers read as zero (the debugger does not keep them) and a program that stops on
//! a signal gets it back on the next resume whatever the client asks. One thread steps at a time.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use crate::platform::{self, Access, Event, HwSlot, Launch, Resume, Stop, Target, Tid};
use crate::rsp::{hex, parse_target_xml, regs_from_g, regs_into_g, stub_xml, unhex, Conn, Frame, Layout};
use crate::server::Kind;
use crate::{fail, Error, Result};

struct Stub {
    t: Box<dyn Target>,
    layout: Layout,
    /// Breakpoints in memory: address to the byte the program has there.
    bps: BTreeMap<u64, u8>,
    watch: Vec<(u8, u64, u8)>,
    g: Tid,
    c: Tid,
    last: String,
    noack: bool,
    files: Vec<Option<Vec<u8>>>,
    end: End,
}

#[derive(PartialEq, Eq)]
enum End {
    No,
    /// Detach, then stop serving.
    Detach,
    /// Kill, then stop serving.
    Kill,
}

fn ok() -> Option<Vec<u8>> {
    Some(b"OK".to_vec())
}

fn err(n: u8) -> Option<Vec<u8>> {
    Some(format!("E{n:02x}").into_bytes())
}

fn num(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

/// `addr,len` as two hex numbers.
fn pair(s: &str) -> Option<(u64, usize)> {
    let (a, l) = s.split_once(',')?;
    Some((num(a)?, num(l)? as usize))
}

fn tid_arg(s: &str) -> Option<i64> {
    if s == "-1" {
        return Some(-1);
    }
    let t = s.rsplit('.').next()?;
    num(t).map(|v| v as i64)
}

/// A slice of `data` for `qXfer` (`off,len`): `m` when more is left, `l` at the end.
fn chunk(data: &[u8], arg: &str) -> Option<Vec<u8>> {
    let (off, len) = pair(arg)?;
    let off = usize::try_from(off).ok()?;
    let end = off.saturating_add(len).min(data.len());
    let part = if off >= data.len() { &[][..] } else { &data[off..end] };
    let mut out = vec![if end < data.len() { b'm' } else { b'l' }];
    out.extend_from_slice(part);
    Some(out)
}

impl Stub {
    fn tids(&self) -> Vec<Tid> {
        self.t.threads()
    }

    fn first_tid(&self) -> Tid {
        self.tids().first().copied().unwrap_or(self.t.pid())
    }

    fn pick(&self, want: Tid) -> Tid {
        if want == 0 || !self.tids().contains(&want) {
            self.first_tid()
        } else {
            want
        }
    }

    fn read_mem(&mut self, addr: u64, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        let n = self.t.read(addr, &mut v);
        v.truncate(n);
        for (a, orig) in &self.bps {
            if *a >= addr && *a < addr + n as u64 {
                v[(*a - addr) as usize] = *orig;
            }
        }
        v
    }

    fn write_mem(&mut self, addr: u64, data: &[u8]) -> Result<()> {
        let mut d = data.to_vec();
        for (a, orig) in self.bps.iter_mut() {
            if *a >= addr && *a < addr + d.len() as u64 {
                let i = (*a - addr) as usize;
                *orig = d[i];
                d[i] = 0xcc;
            }
        }
        self.t.write(addr, &d)
    }

    fn insert_bp(&mut self, addr: u64) -> bool {
        if self.bps.contains_key(&addr) {
            return true;
        }
        let mut b = [0u8; 1];
        if self.t.read(addr, &mut b) != 1 || self.t.write(addr, &[0xcc]).is_err() {
            return false;
        }
        self.bps.insert(addr, b[0]);
        self.sync_patches();
        true
    }

    fn remove_bp(&mut self, addr: u64) -> bool {
        let Some(orig) = self.bps.remove(&addr) else { return true };
        self.sync_patches();
        self.t.write(addr, &[orig]).is_ok()
    }

    fn sync_patches(&mut self) {
        let p: Vec<(u64, u8)> = self.bps.iter().map(|(a, b)| (*a, *b)).collect();
        self.t.set_patches(&p);
    }

    fn put_back_all(&mut self) {
        for (a, b) in std::mem::take(&mut self.bps) {
            let _ = self.t.write(a, &[b]);
        }
        self.t.set_patches(&[]);
    }

    fn set_hw(&mut self) -> Result<()> {
        let mut slots: [Option<HwSlot>; 4] = [None; 4];
        if self.watch.len() > 4 {
            return fail("only four hardware watchpoints");
        }
        for (i, (kind, addr, len)) in self.watch.iter().enumerate() {
            let access = match kind {
                2 => Access::Write,
                1 => Access::Exec,
                _ => Access::ReadWrite,
            };
            if !matches!(len, 1 | 2 | 4 | 8) || addr % u64::from(*len) != 0 {
                return fail("hardware watches are 1, 2, 4 or 8 bytes, aligned");
            }
            slots[i] = Some(HwSlot { addr: *addr, len: *len, access });
        }
        self.t.set_watch(&slots)
    }

    fn registers(&mut self) -> Option<Vec<u8>> {
        let tid = self.pick(self.g);
        let r = self.t.regs(tid).ok()?;
        let mut g = vec![0u8; self.layout.len];
        regs_into_g(&self.layout, &r, &mut g);
        Some(hex(&g).into_bytes())
    }

    /// The reply for a stop: `T05thread:1a2;swbreak:;`.
    fn stop_reply(&mut self, events: Vec<Event>) -> Vec<u8> {
        // every thread that ran into one of our breakpoints is put back on it, whichever stop is reported
        let mut swbreak = Vec::new();
        for ev in &events {
            if let Event::Stopped { tid, stop: Stop::Breakpoint } = ev {
                if let Ok(mut r) = self.t.regs(*tid) {
                    let a = r.rip.wrapping_sub(1);
                    if self.bps.contains_key(&a) {
                        r.rip = a;
                        let _ = self.t.set_regs(*tid, &r);
                        swbreak.push(*tid);
                    }
                }
            }
        }
        let Some(first) = events.first().copied() else { return b"S00".to_vec() };
        let reply = match first {
            Event::Exited { code } => {
                self.end = End::No;
                format!("W{:02x}", code & 0xff)
            }
            Event::Killed { signal } => format!("X{:02x}", signal & 0xff),
            Event::Stopped { tid, stop } => {
                self.g = tid;
                self.c = tid;
                let (sig, extra) = match stop {
                    Stop::Breakpoint if swbreak.contains(&tid) => (5, "swbreak:;".to_string()),
                    Stop::Watch(bits) => {
                        let i = (0..4).find(|i| bits >> i & 1 == 1).unwrap_or(0);
                        let a = self.watch.get(i).map_or(0, |w| w.1);
                        let kind = match self.watch.get(i).map(|w| w.0) {
                            Some(2) => "watch",
                            Some(3) => "rwatch",
                            _ => "awatch",
                        };
                        (5, format!("{kind}:{a:x};"))
                    }
                    Stop::Fault(f) => (f.number as u8, String::new()),
                    Stop::Interrupt => (2, String::new()),
                    Stop::Exec => (5, "exec:;".to_string()),
                    _ => (5, String::new()),
                };
                format!("T{sig:02x}thread:{tid:x};{extra}")
            }
        };
        self.last = reply.clone();
        reply.into_bytes()
    }

    /// Start the program as asked. `Err` carries a stop to report at once (a thread that stepping off a
    /// breakpoint ran into something).
    fn start(&mut self, how: Resume) -> std::result::Result<(), Vec<Event>> {
        let tids: Vec<Tid> = match how {
            Resume::All => self.tids(),
            Resume::Step(t) => vec![t],
        };
        let stepping = matches!(how, Resume::Step(_));
        for tid in tids {
            let Ok(r) = self.t.regs(tid) else { continue };
            let Some(orig) = self.bps.get(&r.rip).copied() else { continue };
            let _ = self.t.write(r.rip, &[orig]);
            if self.t.resume(Resume::Step(tid)).is_err() {
                let _ = self.t.write(r.rip, &[0xcc]);
                continue;
            }
            let ev = self.t.wait(Duration::from_secs(5)).unwrap_or_default();
            let gone = ev.iter().any(|e| matches!(e, Event::Exited { .. } | Event::Killed { .. }));
            if !gone {
                let _ = self.t.write(r.rip, &[0xcc]);
            }
            if ev.is_empty() {
                continue;
            }
            // stepping off the breakpoint was the whole of a step; for a resume it only clears the way
            let plain = ev.iter().all(|e| matches!(e, Event::Stopped { stop: Stop::Step, .. }));
            if stepping || !plain {
                return Err(ev);
            }
        }
        match self.t.resume(how) {
            Ok(()) => Ok(()),
            Err(_) => Err(vec![Event::Exited { code: -1 }]),
        }
    }

    fn handle(&mut self, p: &[u8], conn: &mut Conn) -> Option<Vec<u8>> {
        let s = String::from_utf8_lossy(p).into_owned();
        let Some(&c0) = p.first() else { return Some(Vec::new()) };
        match c0 {
            b'?' => Some(self.last.clone().into_bytes()),
            b'g' => self.registers().or_else(|| err(1)),
            b'G' => {
                let tid = self.pick(self.g);
                let Some(g) = unhex(&p[1..]).filter(|g| g.len() >= self.layout.len) else { return err(1) };
                let r = regs_from_g(&self.layout, &g);
                if self.t.set_regs(tid, &r).is_ok() {
                    ok()
                } else {
                    err(2)
                }
            }
            b'p' => {
                let n = num(&s[1..])? as usize;
                let g = unhex(&self.registers()?)?;
                let sl = self.layout.slots.get(n)?;
                Some(hex(&g[sl.off..sl.off + (sl.bits as usize).div_ceil(8)]).into_bytes())
            }
            b'P' => {
                let (n, v) = s[1..].split_once('=')?;
                let n = num(n)? as usize;
                let v = unhex(v.as_bytes())?;
                let tid = self.pick(self.g);
                let r = self.t.regs(tid).ok()?;
                let mut g = vec![0u8; self.layout.len];
                regs_into_g(&self.layout, &r, &mut g);
                let sl = self.layout.slots.get(n)?.clone();
                let w = (sl.bits as usize).div_ceil(8);
                if v.len() != w {
                    return err(1);
                }
                g[sl.off..sl.off + w].copy_from_slice(&v);
                let new = regs_from_g(&self.layout, &g);
                if self.t.set_regs(tid, &new).is_ok() {
                    ok()
                } else {
                    err(2)
                }
            }
            b'm' => {
                let (a, l) = pair(&s[1..])?;
                let l = l.min(0x10000);
                let v = self.read_mem(a, l);
                if v.is_empty() && l > 0 {
                    err(1)
                } else {
                    Some(hex(&v).into_bytes())
                }
            }
            b'M' => {
                let (head, data) = s[1..].split_once(':')?;
                let (a, l) = pair(head)?;
                let d = unhex(data.as_bytes())?;
                if d.len() != l {
                    return err(1);
                }
                if self.write_mem(a, &d).is_ok() {
                    ok()
                } else {
                    err(2)
                }
            }
            b'X' => {
                let colon = p.iter().position(|&b| b == b':')?;
                let (a, l) = pair(std::str::from_utf8(&p[1..colon]).ok()?)?;
                // `Conn` already undid the escapes of the whole packet
                let d = &p[colon + 1..];
                if l == 0 {
                    return ok();
                }
                if d.len() != l {
                    return err(1);
                }
                if self.write_mem(a, d).is_ok() {
                    ok()
                } else {
                    err(2)
                }
            }
            b'H' => {
                let which = *p.get(1)?;
                let t = tid_arg(&s[2..])?;
                let tid = if t <= 0 { 0 } else { t as Tid };
                if which == b'g' {
                    self.g = tid;
                } else {
                    self.c = tid;
                }
                ok()
            }
            b'T' => {
                let t = tid_arg(&s[1..])?;
                if self.tids().contains(&(t as Tid)) {
                    ok()
                } else {
                    err(1)
                }
            }
            b'Z' | b'z' => {
                let mut it = s[1..].split(',');
                let kind: u8 = it.next()?.parse().ok()?;
                let addr = num(it.next()?)?;
                let len = num(it.next()?)? as u8;
                let insert = c0 == b'Z';
                match kind {
                    0 | 1 => {
                        // a hardware breakpoint is served as an `int3` too
                        let done = if insert { self.insert_bp(addr) } else { self.remove_bp(addr) };
                        if done {
                            ok()
                        } else {
                            err(1)
                        }
                    }
                    2 | 4 => {
                        let want = (kind, addr, len);
                        if insert {
                            if !self.watch.contains(&want) {
                                self.watch.push(want);
                            }
                        } else {
                            self.watch.retain(|w| *w != want);
                        }
                        if self.set_hw().is_ok() {
                            ok()
                        } else {
                            self.watch.retain(|w| *w != want);
                            err(1)
                        }
                    }
                    _ => Some(Vec::new()),
                }
            }
            b'D' => {
                self.end = End::Detach;
                ok()
            }
            b'k' => {
                self.end = End::Kill;
                None
            }
            b'c' | b's' | b'C' | b'S' => {
                let mut t = self.pick(self.c);
                if c0 == b's' || c0 == b'S' {
                    t = self.pick(if self.c == 0 { self.g } else { self.c });
                }
                if c0 == b'c' && s.len() > 1 {
                    // resume at an address
                    let a = num(&s[1..])?;
                    let tid = self.pick(self.g);
                    let mut r = self.t.regs(tid).ok()?;
                    r.rip = a;
                    let _ = self.t.set_regs(tid, &r);
                }
                let how = if c0 == b's' || c0 == b'S' { Resume::Step(t) } else { Resume::All };
                self.run(how, conn)
            }
            b'v' => self.v_packet(&s, conn),
            b'q' => self.q_packet(&s),
            b'Q' => {
                if s == "QStartNoAckMode" {
                    self.noack = true;
                    return ok();
                }
                Some(Vec::new())
            }
            _ => Some(Vec::new()),
        }
    }

    fn v_packet(&mut self, s: &str, conn: &mut Conn) -> Option<Vec<u8>> {
        if s == "vCont?" {
            return Some(b"vCont;c;C;s;S".to_vec());
        }
        if let Some(rest) = s.strip_prefix("vCont;") {
            let mut how = None;
            for act in rest.split(';') {
                let (a, tid) = act.split_once(':').map_or((act, None), |(a, t)| (a, tid_arg(t)));
                let tid = tid.filter(|t| *t > 0).map(|t| t as Tid);
                match a.chars().next()? {
                    's' | 'S' => {
                        how = Some(Resume::Step(self.pick(tid.unwrap_or(self.c))));
                        break;
                    }
                    'c' | 'C' if how.is_none() => how = Some(Resume::All),
                    _ => {}
                }
            }
            return self.run(how?, conn);
        }
        if s == "vKill" || s.starts_with("vKill;") {
            self.end = End::Kill;
            return ok();
        }
        if s == "vMustReplyEmpty" {
            return Some(Vec::new());
        }
        if let Some(rest) = s.strip_prefix("vFile:") {
            return self.v_file(rest);
        }
        Some(Vec::new())
    }

    /// Read-only access to `/proc/<pid>/maps`, which is what a client needs to find the program's libraries.
    fn v_file(&mut self, rest: &str) -> Option<Vec<u8>> {
        let (op, args) = rest.split_once(':').unwrap_or((rest, ""));
        match op {
            "setfs" => Some(b"F0".to_vec()),
            "open" => {
                let path = String::from_utf8(unhex(args.split(',').next()?.as_bytes())?).ok()?;
                if path != format!("/proc/{}/maps", self.t.pid()) {
                    return Some(b"F-1,2".to_vec());
                }
                match std::fs::read(&path) {
                    Ok(b) => {
                        self.files.push(Some(b));
                        Some(format!("F{}", self.files.len()).into_bytes())
                    }
                    Err(_) => Some(b"F-1,2".to_vec()),
                }
            }
            "pread" => {
                let mut it = args.split(',');
                let fd = num(it.next()?)? as usize;
                let (count, off) = (num(it.next()?)? as usize, num(it.next()?)? as usize);
                let data = self.files.get(fd.checked_sub(1)?)?.as_ref()?;
                let part = if off >= data.len() { &[][..] } else { &data[off..(off + count).min(data.len())] };
                let mut r = format!("F{:x};", part.len()).into_bytes();
                r.extend_from_slice(part);
                Some(r)
            }
            "close" => {
                let fd = num(args)? as usize;
                if let Some(f) = self.files.get_mut(fd.checked_sub(1)?) {
                    *f = None;
                }
                Some(b"F0".to_vec())
            }
            _ => Some(Vec::new()),
        }
    }

    fn q_packet(&mut self, s: &str) -> Option<Vec<u8>> {
        if s.starts_with("qSupported") {
            return Some(
                b"PacketSize=4000;qXfer:features:read+;qXfer:auxv:read+;qXfer:exec-file:read+;swbreak+;hwbreak+;vContSupported+;QStartNoAckMode+"
                    .to_vec(),
            );
        }
        match s {
            "qAttached" => return Some(if self.t.launched() { b"0".to_vec() } else { b"1".to_vec() }),
            "qC" => return Some(format!("QC{:x}", self.pick(self.g)).into_bytes()),
            "qfThreadInfo" => {
                let t: Vec<String> = self.tids().iter().map(|t| format!("{t:x}")).collect();
                return Some(format!("m{}", t.join(",")).into_bytes());
            }
            "qsThreadInfo" => return Some(b"l".to_vec()),
            "qSymbol::" => return ok(),
            _ => {}
        }
        if let Some(rest) = s.strip_prefix("qXfer:features:read:") {
            let (annex, arg) = rest.split_once(':')?;
            if annex != "target.xml" {
                return err(1);
            }
            return chunk(stub_xml().as_bytes(), arg);
        }
        if let Some(rest) = s.strip_prefix("qXfer:auxv:read:") {
            let arg = rest.split_once(':')?.1;
            let b = std::fs::read(format!("/proc/{}/auxv", self.t.pid())).ok()?;
            return chunk(&b, arg);
        }
        if let Some(rest) = s.strip_prefix("qXfer:exec-file:read:") {
            let arg = rest.split_once(':')?.1;
            let p = std::fs::read_link(format!("/proc/{}/exe", self.t.pid())).ok()?;
            return chunk(p.display().to_string().as_bytes(), arg);
        }
        Some(Vec::new())
    }

    /// Run the program and answer when it stops. The client may send `0x03` meanwhile.
    fn run(&mut self, how: Resume, conn: &mut Conn) -> Option<Vec<u8>> {
        let events = match self.start(how) {
            Err(ev) => ev,
            Ok(()) => loop {
                match conn.recv(Duration::from_millis(1)) {
                    Ok(Some(Frame::Interrupt)) => {
                        let _ = self.t.interrupt();
                    }
                    Ok(_) => {}
                    Err(_) => {
                        self.end = End::Kill;
                        return None;
                    }
                }
                let out = self.t.output();
                for part in out.chunks(1000) {
                    let mut o = b"O".to_vec();
                    o.extend_from_slice(hex(part).as_bytes());
                    if conn.send(&o).is_err() {
                        self.end = End::Kill;
                        return None;
                    }
                }
                match self.t.wait(Duration::from_millis(20)) {
                    Ok(ev) if !ev.is_empty() => break ev,
                    Ok(_) => {}
                    Err(_) => break vec![Event::Exited { code: -1 }],
                }
            },
        };
        let out = self.t.output();
        for part in out.chunks(1000) {
            let mut o = b"O".to_vec();
            o.extend_from_slice(hex(part).as_bytes());
            let _ = conn.send(&o);
        }
        Some(self.stop_reply(events))
    }
}

/// Start (or attach to) a program and serve it on `127.0.0.1:<port>` (0: any free port). `ready` is told the
/// address once the program is loaded and the port is open. Blocks until the client leaves.
pub fn serve_gdb(kind: &Kind, port: u16, ready: &mut dyn FnMut(SocketAddr)) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| Error::Fail(format!("cannot listen on 127.0.0.1:{port}: {e}")))?;
    let addr = listener.local_addr().map_err(|e| Error::Fail(e.to_string()))?;
    let t = match kind {
        Kind::Run { exe, args } => {
            if !exe.is_file() {
                return fail(format!("{} is not a file", exe.display()));
            }
            platform::launch(&Launch { exe: exe.clone(), args: args.clone(), ..Launch::default() })?
        }
        Kind::Attach(pid) => platform::attach(*pid)?,
        _ => return fail("`serve --gdb` takes a program to run or a pid to attach to"),
    };
    let (_, layout) = parse_target_xml(&stub_xml());
    let mut st = Stub {
        t,
        layout,
        bps: BTreeMap::new(),
        watch: Vec::new(),
        g: 0,
        c: 0,
        last: String::new(),
        noack: false,
        files: Vec::new(),
        end: End::No,
    };
    let first = st.first_tid();
    st.last = format!("T05thread:{first:x};");
    ready(addr);
    let (stream, _) = listener.accept().map_err(|e| Error::Fail(e.to_string()))?;
    drop(listener);
    let mut conn = Conn::new(stream);
    loop {
        match conn.recv(Duration::from_secs(1)) {
            Ok(Some(Frame::Pkt(p))) => {
                let was_noack = st.noack;
                let reply = st.handle(&p, &mut conn);
                if let Some(r) = reply {
                    if conn.send(&r).is_err() {
                        break;
                    }
                }
                if st.noack && !was_noack {
                    conn.ack = false;
                }
                if st.end != End::No {
                    break;
                }
            }
            Ok(Some(Frame::Interrupt)) => {
                let r = st.last.clone().into_bytes();
                let _ = conn.send(&r);
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    st.put_back_all();
    if st.end != End::Detach && st.t.launched() {
        let _ = st.t.kill();
    } else {
        let _ = st.t.detach();
    }
    Ok(())
}
