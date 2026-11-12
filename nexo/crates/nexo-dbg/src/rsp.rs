//! The GDB remote serial protocol, the part both sides share: packet framing over a TCP stream, hex,
//! the register layout a stub describes in its target description, and the layout nexo's own stub gives.
//! `remote` is the client (nexo debugs a program behind gdbserver, `qemu -g` or a probe), `stub` the server.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::regs::{Regs, GP};

/// One thing that arrived: a checked packet, or the 0x03 byte that asks a running program to stop.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Pkt(Vec<u8>),
    Interrupt,
}

pub struct Conn {
    s: TcpStream,
    buf: Vec<u8>,
    queue: VecDeque<Frame>,
    acked: Option<bool>,
    /// False after `QStartNoAckMode`: no `+`/`-` either way.
    pub ack: bool,
}

pub fn checksum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, x| a.wrapping_add(*x))
}

/// `$payload#xx`, with the four bytes the protocol reserves escaped.
pub fn encode(payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(payload.len() + 8);
    for &b in payload {
        if matches!(b, b'#' | b'$' | b'}' | b'*') {
            body.push(b'}');
            body.push(b ^ 0x20);
        } else {
            body.push(b);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 4);
    out.push(b'$');
    out.extend_from_slice(&body);
    out.extend_from_slice(format!("#{:02x}", checksum(&body)).as_bytes());
    out
}

/// The bytes a packet body stands for: `}x` is `x ^ 0x20`, `c*n` repeats `c` (n - 29) more times.
pub fn decode(body: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'}' if i + 1 < body.len() => {
                out.push(body[i + 1] ^ 0x20);
                i += 2;
            }
            b'*' if i + 1 < body.len() && !out.is_empty() => {
                let n = body[i + 1].saturating_sub(29) as usize;
                let last = *out.last().unwrap_or(&0);
                out.extend(std::iter::repeat_n(last, n));
                i += 2;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

impl Conn {
    pub fn new(s: TcpStream) -> Conn {
        let _ = s.set_nodelay(true);
        Conn { s, buf: Vec::new(), queue: VecDeque::new(), acked: None, ack: true }
    }

    /// Read what has come, waiting at most `timeout` for the first byte. `Err` when the other side closed.
    fn fill(&mut self, timeout: Duration) -> io::Result<()> {
        self.s.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut tmp = [0u8; 8192];
        match self.s.read(&mut tmp) {
            Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection closed")),
            Ok(n) => {
                self.buf.extend_from_slice(&tmp[..n]);
                self.parse();
                Ok(())
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn parse(&mut self) {
        loop {
            let Some(&first) = self.buf.first() else { return };
            match first {
                b'+' | b'-' => {
                    self.acked = Some(first == b'+');
                    self.buf.remove(0);
                }
                3 => {
                    self.queue.push_back(Frame::Interrupt);
                    self.buf.remove(0);
                }
                b'$' => {
                    let Some(h) = self.buf.iter().position(|&b| b == b'#') else { return };
                    if self.buf.len() < h + 3 {
                        return;
                    }
                    let body = self.buf[1..h].to_vec();
                    let sum =
                        std::str::from_utf8(&self.buf[h + 1..h + 3]).ok().and_then(|s| u8::from_str_radix(s, 16).ok());
                    self.buf.drain(..h + 3);
                    if sum == Some(checksum(&body)) {
                        if self.ack {
                            let _ = self.s.write_all(b"+");
                        }
                        self.queue.push_back(Frame::Pkt(decode(&body)));
                    } else if self.ack {
                        let _ = self.s.write_all(b"-");
                    }
                }
                _ => {
                    self.buf.remove(0);
                }
            }
        }
    }

    /// Send one packet and, unless acks are off, wait for the other side's `+` (a `-` sends it again, three times).
    pub fn send(&mut self, payload: &[u8]) -> io::Result<()> {
        let frame = encode(payload);
        for _ in 0..3 {
            self.acked = None;
            self.s.write_all(&frame)?;
            if !self.ack {
                return Ok(());
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.acked.is_none() {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the other side never acknowledged a packet"));
                }
                self.fill(Duration::from_millis(50))?;
            }
            if self.acked == Some(true) {
                return Ok(());
            }
        }
        Err(io::Error::new(io::ErrorKind::InvalidData, "the other side refused a packet three times"))
    }

    /// The next frame, waiting at most `timeout`; `None` when nothing came.
    pub fn recv(&mut self, timeout: Duration) -> io::Result<Option<Frame>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.queue.pop_front() {
                return Ok(Some(f));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            self.fill(left)?;
            if self.queue.is_empty() && Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    /// The raw interrupt byte.
    pub fn send_interrupt(&mut self) -> io::Result<()> {
        self.s.write_all(&[3])
    }
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn unhex(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() & 1 == 1 {
        return None;
    }
    s.chunks(2).map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok()).collect()
}

/// One register of the `g` packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub bits: u32,
    pub off: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub slots: Vec<Slot>,
    pub len: usize,
}

fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let at = tag.find(&pat)?;
    // the key must start a word, so `bitsize` is not found inside `xbitsize`
    if at > 0 && !tag.as_bytes()[at - 1].is_ascii_whitespace() {
        return None;
    }
    let rest = &tag[at + pat.len()..];
    Some(rest[..rest.find('"')?].to_string())
}

/// What a target description says: the architecture and the registers in `g` packet order.
pub fn parse_target_xml(xml: &str) -> (Option<String>, Layout) {
    let arch = xml.find("<architecture>").and_then(|i| {
        let rest = &xml[i + 14..];
        Some(rest[..rest.find('<')?].trim().to_string())
    });
    let mut regs: Vec<(usize, String, u32)> = Vec::new();
    let mut next = 0usize;
    let mut rest = xml;
    while let Some(i) = rest.find("<reg ") {
        rest = &rest[i + 1..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        let name = attr(tag, "name").unwrap_or_default();
        let bits = attr(tag, "bitsize").and_then(|b| b.parse().ok()).unwrap_or(0);
        let num = attr(tag, "regnum").and_then(|b| b.parse().ok()).unwrap_or(next);
        next = num + 1;
        regs.push((num, name, bits));
        rest = &rest[end..];
    }
    regs.sort_by_key(|r| r.0);
    let mut off = 0;
    let slots = regs
        .into_iter()
        .map(|(_, name, bits)| {
            let s = Slot { name, bits, off };
            off += (bits as usize).div_ceil(8);
            s
        })
        .collect();
    (arch, Layout { slots, len: off })
}

/// Replace each `<xi:include href="x"/>` with the text `fetch` gives for `x`.
pub fn expand_includes(xml: &str, fetch: &mut dyn FnMut(&str) -> Option<String>, depth: u32) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<xi:include") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('>').map_or(tail.len(), |e| e + 1);
        let tag = &tail[..end];
        if depth < 4 {
            if let Some(text) = attr(tag, "href").and_then(|h| fetch(&h)) {
                out.push_str(&expand_includes(&text, fetch, depth + 1));
            }
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn gp_name(i: usize) -> &'static str {
    GP[i]
}

/// What gdb used before target descriptions: the layout every x86-64 stub then had.
pub fn legacy_layout() -> Layout {
    let mut v: Vec<(String, u32)> = (0..16).map(|i| (gp_name(i).to_string(), 64)).collect();
    v.push(("rip".into(), 64));
    for n in ["eflags", "cs", "ss", "ds", "es", "fs", "gs"] {
        v.push((n.into(), 32));
    }
    for i in 0..8 {
        v.push((format!("st{i}"), 80));
    }
    for n in ["fctrl", "fstat", "ftag", "fiseg", "fioff", "foseg", "fooff", "fop"] {
        v.push((n.into(), 32));
    }
    for i in 0..16 {
        v.push((format!("xmm{i}"), 128));
    }
    v.push(("mxcsr".into(), 32));
    let mut off = 0;
    let slots = v
        .into_iter()
        .map(|(name, bits)| {
            let s = Slot { name, bits, off };
            off += (bits as usize).div_ceil(8);
            s
        })
        .collect();
    Layout { slots, len: off }
}

/// The target description nexo's own stub serves: the x86-64 core registers (the x87 ones read as zero, the
/// debugger does not keep them), the segment bases and `orig_rax`.
pub fn stub_xml() -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\"?><!DOCTYPE target SYSTEM \"gdb-target.dtd\"><target version=\"1.0\"><architecture>i386:x86-64</architecture><osabi>GNU/Linux</osabi><feature name=\"org.gnu.gdb.i386.core\">",
    );
    for i in 0..16 {
        let ty = if i == 6 || i == 7 { "data_ptr" } else { "int64" };
        x.push_str(&format!("<reg name=\"{}\" bitsize=\"64\" type=\"{ty}\"/>", gp_name(i)));
    }
    x.push_str(
        "<reg name=\"rip\" bitsize=\"64\" type=\"code_ptr\"/><reg name=\"eflags\" bitsize=\"32\" type=\"int32\"/>",
    );
    for n in ["cs", "ss", "ds", "es", "fs", "gs"] {
        x.push_str(&format!("<reg name=\"{n}\" bitsize=\"32\" type=\"int32\"/>"));
    }
    for i in 0..8 {
        x.push_str(&format!("<reg name=\"st{i}\" bitsize=\"80\" type=\"i387_ext\"/>"));
    }
    for n in ["fctrl", "fstat", "ftag", "fiseg", "fioff", "foseg", "fooff", "fop"] {
        x.push_str(&format!("<reg name=\"{n}\" bitsize=\"32\" type=\"int\" group=\"float\"/>"));
    }
    x.push_str("</feature><feature name=\"org.gnu.gdb.i386.segments\"><reg name=\"fs_base\" bitsize=\"64\" type=\"int\"/><reg name=\"gs_base\" bitsize=\"64\" type=\"int\"/></feature>");
    x.push_str("<feature name=\"org.gnu.gdb.i386.linux\"><reg name=\"orig_rax\" bitsize=\"64\" type=\"int\" group=\"system\"/></feature></target>");
    x
}

fn le(g: &[u8], off: usize, bits: u32) -> u64 {
    let n = (bits as usize / 8).min(8);
    let mut b = [0u8; 8];
    if let Some(src) = g.get(off..off + n) {
        b[..n].copy_from_slice(src);
    }
    u64::from_le_bytes(b)
}

/// The registers in a `g` packet, by name; the ones nexo does not keep (x87, vector) are skipped.
pub fn regs_from_g(l: &Layout, g: &[u8]) -> Regs {
    let mut r = Regs::default();
    for s in &l.slots {
        let v = le(g, s.off, s.bits);
        if let Some(i) = GP.iter().position(|n| *n == s.name) {
            r.gp[i] = v;
            continue;
        }
        match s.name.as_str() {
            "rip" => r.rip = v,
            "eflags" => r.eflags = v,
            "cs" => r.seg[0] = v,
            "ss" => r.seg[1] = v,
            "ds" => r.seg[2] = v,
            "es" => r.seg[3] = v,
            "fs" => r.seg[4] = v,
            "gs" => r.seg[5] = v,
            "fs_base" => {
                r.fs_base = v;
                r.has_bases = true;
            }
            "gs_base" => r.gs_base = v,
            "orig_rax" => r.orig_rax = v,
            _ => {}
        }
    }
    r
}

/// Put `r` into a `g` packet buffer (the registers it does not carry are left as they are in `g`).
pub fn regs_into_g(l: &Layout, r: &Regs, g: &mut [u8]) {
    for s in &l.slots {
        let v = if let Some(i) = GP.iter().position(|n| *n == s.name) {
            r.gp[i]
        } else {
            match s.name.as_str() {
                "rip" => r.rip,
                "eflags" => r.eflags,
                "cs" => r.seg[0],
                "ss" => r.seg[1],
                "ds" => r.seg[2],
                "es" => r.seg[3],
                "fs" => r.seg[4],
                "gs" => r.seg[5],
                "fs_base" => r.fs_base,
                "gs_base" => r.gs_base,
                "orig_rax" => r.orig_rax,
                _ => continue,
            }
        };
        let n = (s.bits as usize / 8).min(8);
        if let Some(dst) = g.get_mut(s.off..s.off + n) {
            dst.copy_from_slice(&v.to_le_bytes()[..n]);
        }
    }
}

/// One line of `/proc/<pid>/maps`.
pub fn parse_map_line(line: &str) -> Option<crate::platform::Map> {
    let mut it = line.splitn(6, ' ');
    let (a, b) = it.next()?.split_once('-')?;
    let perms = it.next()?.as_bytes();
    let offset = u64::from_str_radix(it.next()?, 16).ok()?;
    let (_dev, _inode) = (it.next()?, it.next()?);
    Some(crate::platform::Map {
        start: u64::from_str_radix(a, 16).ok()?,
        end: u64::from_str_radix(b, 16).ok()?,
        read: perms.first() == Some(&b'r'),
        write: perms.get(1) == Some(&b'w'),
        exec: perms.get(2) == Some(&b'x'),
        offset,
        path: it.next().unwrap_or("").trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_escape_the_reserved_bytes_and_read_back() {
        let body = b"X10,4:a#b$c}d*e";
        let f = encode(body);
        assert_eq!(f[0], b'$');
        let h = f.iter().position(|&b| b == b'#').unwrap();
        assert!(f[1..h].iter().all(|&b| b != b'$' && b != b'*'));
        assert_eq!(decode(&f[1..h]), body);
    }

    #[test]
    fn run_length_runs_are_expanded() {
        // "0* " is a zero repeated (0x20 - 29 = 3) more times
        assert_eq!(decode(b"0* x"), b"0000x");
    }

    #[test]
    fn the_checksum_is_the_sum_of_the_body() {
        assert_eq!(encode(b"OK"), b"$OK#9a");
    }

    #[test]
    fn the_stub_description_reads_back_in_order() {
        let (arch, l) = parse_target_xml(&stub_xml());
        assert_eq!(arch.as_deref(), Some("i386:x86-64"));
        assert_eq!(l.slots[0].name, "rax");
        assert_eq!(l.slots[16].name, "rip");
        assert_eq!(l.slots[16].off, 128);
        assert_eq!(l.slots[17].off, 136);
        let fs = l.slots.iter().find(|s| s.name == "fs_base").unwrap();
        assert_eq!(fs.bits, 64);
        let mut g = vec![0u8; l.len];
        let mut r = Regs { has_bases: true, ..Regs::default() };
        r.gp[3] = 0x1122;
        r.rip = 0x401136;
        r.eflags = 0x246;
        r.seg[4] = 7;
        r.fs_base = 0x7fff0000;
        r.orig_rax = u64::MAX;
        regs_into_g(&l, &r, &mut g);
        assert_eq!(regs_from_g(&l, &g), r);
    }

    #[test]
    fn includes_are_expanded_where_they_stand() {
        let xml = "<target><xi:include href=\"a.xml\"/><x/></target>";
        let out =
            expand_includes(xml, &mut |h| (h == "a.xml").then(|| "<reg name=\"rax\" bitsize=\"64\"/>".to_string()), 0);
        assert_eq!(out, "<target><reg name=\"rax\" bitsize=\"64\"/><x/></target>");
    }

    #[test]
    fn the_legacy_layout_puts_rip_after_the_sixteen_general_registers() {
        let l = legacy_layout();
        assert_eq!(l.slots.iter().find(|s| s.name == "rip").unwrap().off, 128);
        assert_eq!(l.slots.iter().find(|s| s.name == "gs").unwrap().off, 128 + 8 + 4 + 5 * 4);
    }
}
