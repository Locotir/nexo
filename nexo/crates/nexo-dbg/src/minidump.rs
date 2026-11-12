//! A Windows minidump (`.dmp`) read as a program that stopped for good: the threads' registers, the
//! memory the dump kept (usually the stacks and little else; the code of a module is read from the
//! module's file when that file is there), the modules and the exception that ended it. Nothing in it
//! can run, change or be stopped. Written by hand against the documented format: no new dependency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nexo_bin::Image;

use crate::corefile::Death;
use crate::platform::{Event, HwSlot, Map, Module, Os, Resume, Target, Tid, Touch};
use crate::regs::Regs;
use crate::{fail, Result};

const SIGNATURE: &[u8; 4] = b"MDMP";
const THREAD_LIST: u32 = 3;
const MODULE_LIST: u32 = 4;
const MEMORY_LIST: u32 = 5;
const EXCEPTION: u32 = 6;
const SYSTEM_INFO: u32 = 7;
const MEMORY64_LIST: u32 = 9;
const MISC_INFO: u32 = 15;
const MEMORY_INFO_LIST: u32 = 16;
const ARCH_AMD64: u16 = 9;
const AV: u32 = 0xC000_0005;
/// A dump this big is not read whole: the parts are read from the file as asked.
const MAX_STREAM: usize = 64 << 20;

struct Thread {
    tid: Tid,
    regs: Regs,
}

struct Range {
    start: u64,
    len: u64,
    /// Where the bytes sit in the file.
    at: u64,
}

struct Region {
    start: u64,
    end: u64,
    protect: u32,
}

pub struct Minidump {
    bytes: Vec<u8>,
    threads: Vec<Thread>,
    modules: Vec<(Module, u64)>,
    ranges: Vec<Range>,
    regions: Vec<Region>,
    death: Option<Death>,
    pid: u32,
    disk: HashMap<String, Option<Image>>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// True when the file starts like a minidump.
pub fn is_minidump(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == SIGNATURE
}

/// The registers from an x86-64 `CONTEXT`.
fn regs_of(c: &[u8]) -> Option<Regs> {
    let q = |at: usize| u64_at(c, at);
    let seg = |at: usize| u16_at(c, at).map(u64::from);
    Some(Regs {
        // rax rbx rcx rdx rsi rdi rbp rsp r8..r15
        gp: [
            q(0x78)?,
            q(0x90)?,
            q(0x80)?,
            q(0x88)?,
            q(0xA8)?,
            q(0xB0)?,
            q(0xA0)?,
            q(0x98)?,
            q(0xB8)?,
            q(0xC0)?,
            q(0xC8)?,
            q(0xD0)?,
            q(0xD8)?,
            q(0xE0)?,
            q(0xE8)?,
            q(0xF0)?,
        ],
        rip: q(0xF8)?,
        eflags: u32_at(c, 0x44)? as u64,
        seg: [seg(0x38)?, seg(0x42)?, seg(0x3A)?, seg(0x3C)?, seg(0x3E)?, seg(0x40)?],
        fs_base: 0,
        gs_base: 0,
        orig_rax: 0,
        has_bases: false,
    })
}

impl Minidump {
    pub fn open(path: &Path) -> Result<Minidump> {
        let bytes =
            std::fs::read(path).map_err(|e| crate::Error::Fail(format!("cannot open {}: {e}", path.display())))?;
        Minidump::parse(bytes)
    }

    pub fn parse(bytes: Vec<u8>) -> Result<Minidump> {
        if bytes.get(..4) != Some(&SIGNATURE[..]) {
            return fail("not a minidump (it does not start with MDMP)");
        }
        let count = u32_at(&bytes, 8).unwrap_or(0) as usize;
        let dir = u32_at(&bytes, 12).unwrap_or(0) as usize;
        if count == 0 || count > 4096 || dir.saturating_add(count * 12) > bytes.len() {
            return fail("the minidump is cut short: its list of streams is not all there");
        }
        let mut streams: Vec<(u32, usize, usize)> = Vec::new();
        for i in 0..count {
            let e = dir + i * 12;
            let (kind, size, rva) = (
                u32_at(&bytes, e).unwrap_or(0),
                u32_at(&bytes, e + 4).unwrap_or(0) as usize,
                u32_at(&bytes, e + 8).unwrap_or(0) as usize,
            );
            if size <= MAX_STREAM && rva.saturating_add(size) <= bytes.len() {
                streams.push((kind, rva, size));
            }
        }
        let find = |k: u32| streams.iter().find(|s| s.0 == k).map(|s| &bytes[s.1..s.1 + s.2]);
        if let Some(sys) = find(SYSTEM_INFO) {
            if u16_at(sys, 0) != Some(ARCH_AMD64) {
                return fail("only x86-64 minidumps can be read");
            }
        }
        let mut d = Minidump {
            bytes: Vec::new(),
            threads: Vec::new(),
            modules: Vec::new(),
            ranges: Vec::new(),
            regions: Vec::new(),
            death: None,
            pid: 0,
            disk: HashMap::new(),
        };
        let ctx = |loc: usize| -> Option<Regs> {
            let (size, rva) = (u32_at(&bytes, loc)? as usize, u32_at(&bytes, loc + 4)? as usize);
            regs_of(bytes.get(rva..rva.checked_add(size)?)?)
        };
        if let Some(s) = find(THREAD_LIST) {
            let n = u32_at(s, 0).unwrap_or(0) as usize;
            for i in 0..n {
                let at = 4 + i * 48;
                let Some(tid) = u32_at(s, at) else { break };
                // the context location sits at the end of the entry, inside the stream
                let (size, rva) = (u32_at(s, at + 40).unwrap_or(0) as usize, u32_at(s, at + 44).unwrap_or(0) as usize);
                let regs = bytes.get(rva..rva.saturating_add(size)).and_then(regs_of);
                if let Some(regs) = regs {
                    d.threads.push(Thread { tid, regs });
                }
                // the thread's stack is memory too
                if let (Some(start), Some(len), Some(at)) = (u64_at(s, at + 24), u32_at(s, at + 32), u32_at(s, at + 36))
                {
                    d.ranges.push(Range { start, len: len as u64, at: at as u64 });
                }
            }
        }
        if let Some(s) = find(MODULE_LIST) {
            let n = u32_at(s, 0).unwrap_or(0) as usize;
            for i in 0..n {
                let at = 4 + i * 108;
                let (Some(base), Some(size), Some(name)) = (u64_at(s, at), u32_at(s, at + 8), u32_at(s, at + 20))
                else {
                    break;
                };
                let name = string_at(&bytes, name as usize).unwrap_or_default();
                d.modules.push((Module { path: name, base }, size as u64));
            }
        }
        if let Some(s) = find(MEMORY_LIST) {
            let n = u32_at(s, 0).unwrap_or(0) as usize;
            for i in 0..n {
                let at = 4 + i * 16;
                if let (Some(start), Some(len), Some(rva)) = (u64_at(s, at), u32_at(s, at + 8), u32_at(s, at + 12)) {
                    d.ranges.push(Range { start, len: len as u64, at: rva as u64 });
                }
            }
        }
        if let Some(s) = find(MEMORY64_LIST) {
            let n = u64_at(s, 0).unwrap_or(0) as usize;
            let mut at = u64_at(s, 8).unwrap_or(0);
            for i in 0..n.min(1 << 20) {
                let (Some(start), Some(len)) = (u64_at(s, 16 + i * 16), u64_at(s, 24 + i * 16)) else { break };
                d.ranges.push(Range { start, len, at });
                at = at.saturating_add(len);
            }
        }
        if let Some(s) = find(MEMORY_INFO_LIST) {
            let (head, each) = (u32_at(s, 0).unwrap_or(16) as usize, u32_at(s, 4).unwrap_or(48) as usize);
            let n = u64_at(s, 8).unwrap_or(0) as usize;
            for i in 0..n.min(1 << 20) {
                let at = head + i * each;
                let (Some(start), Some(len), Some(state), Some(protect)) =
                    (u64_at(s, at), u64_at(s, at + 24), u32_at(s, at + 32), u32_at(s, at + 36))
                else {
                    break;
                };
                // committed memory only
                if state == 0x1000 {
                    d.regions.push(Region { start, end: start.saturating_add(len), protect });
                }
            }
        }
        if let Some(s) = find(MISC_INFO) {
            if u32_at(s, 4).unwrap_or(0) & 1 != 0 {
                d.pid = u32_at(s, 8).unwrap_or(0);
            }
        }
        if let Some((_, rva, _)) = streams.iter().find(|s| s.0 == EXCEPTION).copied() {
            let s = &bytes[rva..rva + streams.iter().find(|s| s.0 == EXCEPTION).map_or(0, |s| s.2)];
            let (tid, code) = (u32_at(s, 0).unwrap_or(0), u32_at(s, 8).unwrap_or(0));
            let pc = u64_at(s, 24);
            let params = u32_at(s, 32).unwrap_or(0);
            let (info0, info1) = (u64_at(s, 40), u64_at(s, 48));
            let av = code == AV && params >= 2;
            let touch = info0.filter(|_| av).map(|k| match k {
                0 => Touch::Read,
                1 => Touch::Write,
                _ => Touch::Exec,
            });
            d.death = Some(Death { signal: code, code: 0, addr: if av { info1 } else { pc }, touch });
            // the registers at the moment of the exception replace the thread's own context, which
            // is where the system stood when it wrote the dump
            if let Some(r) = ctx(rva + 160) {
                match d.threads.iter_mut().find(|t| t.tid == tid) {
                    Some(t) => t.regs = r,
                    None => d.threads.insert(0, Thread { tid, regs: r }),
                }
            }
            // the crashing thread first
            if let Some(i) = d.threads.iter().position(|t| t.tid == tid) {
                let t = d.threads.remove(i);
                d.threads.insert(0, t);
            }
        }
        if d.threads.is_empty() {
            return fail("the minidump holds no thread registers");
        }
        if d.pid == 0 {
            d.pid = d.threads[0].tid;
        }
        d.ranges.sort_by_key(|r| r.start);
        d.bytes = bytes;
        Ok(d)
    }

    /// How the program died, when the dump says.
    pub fn death(&self) -> Option<Death> {
        self.death
    }

    /// The program's own file as the dump names it: the first module that is an `.exe`, else the first.
    pub fn exe(&self) -> Option<PathBuf> {
        let m =
            self.modules.iter().find(|m| m.0.path.to_ascii_lowercase().ends_with(".exe")).or(self.modules.first())?;
        Some(PathBuf::from(&m.0.path))
    }

    /// Put the program's module first, as `Target::modules` promises.
    pub fn exe_first(&mut self) {
        if let Some(i) =
            self.modules.iter().position(|m| m.0.path.to_ascii_lowercase().ends_with(".exe")).filter(|&i| i > 0)
        {
            let m = self.modules.remove(i);
            self.modules.insert(0, m);
        }
    }

    fn disk_image(&mut self, path: &str) -> Option<&Image> {
        self.disk.entry(path.to_string()).or_insert_with(|| Image::open(Path::new(path)).ok()).as_ref()
    }

    /// Bytes at `addr` from the dump, else from the file of the module that holds them.
    fn chunk(&mut self, addr: u64, out: &mut [u8]) -> usize {
        if let Some(r) = self.ranges.iter().find(|r| addr >= r.start && addr - r.start < r.len) {
            let rel = addr - r.start;
            let n = (out.len() as u64).min(r.len - rel) as usize;
            let from = (r.at + rel) as usize;
            return match self.bytes.get(from..from.saturating_add(n)) {
                Some(b) => {
                    out[..n].copy_from_slice(b);
                    n
                }
                None => 0,
            };
        }
        let Some((path, base)) = self
            .modules
            .iter()
            .find(|m| addr >= m.0.base && addr - m.0.base < m.1)
            .map(|m| (m.0.path.clone(), m.0.base))
        else {
            return 0;
        };
        let Some(img) = self.disk_image(&path) else { return 0 };
        let va = addr - base + img.image_base;
        let Some(s) = img.section_at(va) else { return 0 };
        let bytes = img.section_bytes(s);
        let off = (va - s.addr) as usize;
        match bytes.get(off..) {
            Some(b) if !b.is_empty() => {
                let n = b.len().min(out.len()).min((s.addr + s.size - va) as usize);
                out[..n].copy_from_slice(&b[..n]);
                n
            }
            _ => 0,
        }
    }
}

/// A MINIDUMP_STRING: a byte count, then UTF-16.
fn string_at(b: &[u8], at: usize) -> Option<String> {
    let len = u32_at(b, at)? as usize;
    let raw = b.get(at + 4..(at + 4).checked_add(len)?)?;
    let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    Some(String::from_utf16_lossy(&units))
}

const ENDED: &str = "this is a minidump: the program is not running and cannot be changed";

impl Target for Minidump {
    fn os(&self) -> Os {
        Os::Windows
    }
    fn pid(&self) -> u32 {
        self.pid
    }
    fn launched(&self) -> bool {
        false
    }
    fn threads(&self) -> Vec<Tid> {
        self.threads.iter().map(|t| t.tid).collect()
    }
    fn regs(&mut self, tid: Tid) -> Result<Regs> {
        match self.threads.iter().find(|t| t.tid == tid) {
            Some(t) => Ok(t.regs),
            None => fail(format!("no thread {tid} in the minidump")),
        }
    }
    fn set_regs(&mut self, _: Tid, _: &Regs) -> Result<()> {
        fail(ENDED)
    }
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let n = self.chunk(addr.wrapping_add(done as u64), &mut buf[done..]);
            if n == 0 {
                break;
            }
            done += n;
        }
        done
    }
    fn write(&mut self, _: u64, _: &[u8]) -> Result<()> {
        fail(ENDED)
    }
    fn maps(&mut self) -> Result<Vec<Map>> {
        let in_module = |m: &[(Module, u64)], a: u64| {
            m.iter().find(|x| a >= x.0.base && a - x.0.base < x.1).map(|x| x.0.path.clone()).unwrap_or_default()
        };
        let mut out: Vec<Map> = Vec::new();
        if self.regions.is_empty() {
            // no protections in the dump: the modules, then the other memory it kept, as readable
            for (m, size) in &self.modules {
                out.push(Map {
                    start: m.base,
                    end: m.base + size,
                    read: true,
                    write: false,
                    exec: true,
                    offset: 0,
                    path: m.path.clone(),
                });
            }
            for r in &self.ranges {
                if self.modules.iter().all(|m| r.start < m.0.base || r.start >= m.0.base + m.1) {
                    out.push(Map {
                        start: r.start,
                        end: r.start + r.len,
                        read: true,
                        write: true,
                        exec: false,
                        offset: 0,
                        path: String::new(),
                    });
                }
            }
        } else {
            for r in &self.regions {
                let p = r.protect & 0xff;
                out.push(Map {
                    start: r.start,
                    end: r.end,
                    read: p != 0x01 && p != 0,
                    write: matches!(p, 0x04 | 0x08 | 0x40 | 0x80),
                    exec: matches!(p, 0x10 | 0x20 | 0x40 | 0x80),
                    offset: 0,
                    path: in_module(&self.modules, r.start),
                });
            }
        }
        out.sort_by_key(|m| m.start);
        Ok(out)
    }
    fn modules(&mut self) -> Result<Vec<Module>> {
        Ok(self.modules.iter().map(|m| m.0.clone()).collect())
    }
    fn entry(&mut self) -> Option<u64> {
        None
    }
    fn set_watch(&mut self, slots: &[Option<HwSlot>; 4]) -> Result<()> {
        if slots.iter().all(Option::is_none) {
            Ok(())
        } else {
            fail(ENDED)
        }
    }
    fn resume(&mut self, _: Resume) -> Result<()> {
        fail(ENDED)
    }
    fn wait(&mut self, _: Duration) -> Result<Vec<Event>> {
        Ok(Vec::new())
    }
    fn interrupt(&mut self) -> Result<()> {
        fail(ENDED)
    }
    fn stdin(&mut self, _: Option<&[u8]>) -> Result<usize> {
        fail(ENDED)
    }
    fn output(&mut self) -> Vec<u8> {
        Vec::new()
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
    fn kill(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn put64(b: &mut Vec<u8>, v: u64) {
        b.extend_from_slice(&v.to_le_bytes());
    }

    /// An x86-64 CONTEXT with these values.
    fn context(rip: u64, rsp: u64, rcx: u64) -> Vec<u8> {
        let mut c = vec![0u8; 1232];
        c[0x30..0x34].copy_from_slice(&0x10_001fu32.to_le_bytes());
        c[0x80..0x88].copy_from_slice(&rcx.to_le_bytes());
        c[0x98..0xa0].copy_from_slice(&rsp.to_le_bytes());
        c[0xf8..0x100].copy_from_slice(&rip.to_le_bytes());
        c[0x44..0x48].copy_from_slice(&0x246u32.to_le_bytes());
        c
    }

    /// A dump with one thread (id 77), one module and an access violation writing address 0x10.
    fn sample() -> Vec<u8> {
        let name: Vec<u8> = "C:\\app\\prog.exe".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        // layout: header 32, directory 5*12, then the pieces, each at a known place
        let (dir_at, n) = (32usize, 5usize);
        let sys_at = dir_at + n * 12;
        let threads_at = sys_at + 56;
        let modules_at = threads_at + 4 + 48;
        let name_at = modules_at + 4 + 108;
        let exc_at = name_at + 4 + name.len();
        let mem_at = exc_at + 168;
        let ctx_a = mem_at + 4 + 16;
        let ctx_b = ctx_a + 1232;
        let stack_at = ctx_b + 1232;
        let mut b = Vec::new();
        b.extend_from_slice(SIGNATURE);
        put32(&mut b, 0xa793);
        put32(&mut b, n as u32);
        put32(&mut b, dir_at as u32);
        b.resize(dir_at, 0);
        for (kind, size, rva) in [
            (SYSTEM_INFO, 56, sys_at),
            (THREAD_LIST, 52, threads_at),
            (MODULE_LIST, 112, modules_at),
            (EXCEPTION, 168, exc_at),
            (MEMORY_LIST, 20, mem_at),
        ] {
            put32(&mut b, kind);
            put32(&mut b, size as u32);
            put32(&mut b, rva as u32);
        }
        // system info: architecture 9 = AMD64
        b.extend_from_slice(&ARCH_AMD64.to_le_bytes());
        b.resize(threads_at, 0);
        // thread list: one thread, its stack at 0x7000, the context a little later
        put32(&mut b, 1);
        put32(&mut b, 77);
        b.extend_from_slice(&[0; 12]);
        put64(&mut b, 0);
        put64(&mut b, 0x7000);
        put32(&mut b, 16);
        put32(&mut b, stack_at as u32);
        put32(&mut b, 1232);
        put32(&mut b, ctx_a as u32);
        assert_eq!(b.len(), modules_at);
        // module list: one module at 0x140000000
        put32(&mut b, 1);
        put64(&mut b, 0x1_4000_0000);
        put32(&mut b, 0x5000);
        put32(&mut b, 0);
        put32(&mut b, 0);
        put32(&mut b, name_at as u32);
        b.resize(name_at, 0);
        put32(&mut b, name.len() as u32);
        b.extend_from_slice(&name);
        assert_eq!(b.len(), exc_at);
        // exception: thread 77, access violation at 0x140001010, a write to 0x10
        put32(&mut b, 77);
        put32(&mut b, 0);
        put32(&mut b, AV);
        put32(&mut b, 0);
        put64(&mut b, 0);
        put64(&mut b, 0x1_4000_1010);
        put32(&mut b, 2);
        put32(&mut b, 0);
        put64(&mut b, 1);
        put64(&mut b, 0x10);
        b.resize(exc_at + 160, 0);
        put32(&mut b, 1232);
        put32(&mut b, ctx_b as u32);
        assert_eq!(b.len(), mem_at);
        // memory list: the stack
        put32(&mut b, 1);
        put64(&mut b, 0x7000);
        put32(&mut b, 16);
        put32(&mut b, stack_at as u32);
        // the thread's own context says rip 1; the exception's says where it really was
        b.extend_from_slice(&context(1, 0x7000, 0));
        b.extend_from_slice(&context(0x1_4000_1010, 0x7000, 0x10));
        b.extend_from_slice(&(0..16u8).collect::<Vec<_>>());
        b
    }

    #[test]
    fn reads_the_crash_thread_exception_modules_and_stack() {
        let mut d = Minidump::parse(sample()).expect("parse");
        assert_eq!(d.threads(), vec![77]);
        // the exception's registers win over the thread's own context
        let r = d.regs(77).unwrap();
        assert_eq!((r.rip, r.rsp(), r.get("rcx")), (0x1_4000_1010, 0x7000, Some(0x10)));
        let death = d.death().unwrap();
        assert_eq!((death.signal, death.addr, death.touch), (AV, Some(0x10), Some(Touch::Write)));
        let m = d.modules().unwrap();
        assert_eq!((m[0].path.as_str(), m[0].base), ("C:\\app\\prog.exe", 0x1_4000_0000));
        assert_eq!(d.exe().unwrap(), PathBuf::from("C:\\app\\prog.exe"));
        let mut buf = [0u8; 4];
        assert_eq!(d.read(0x7004, &mut buf), 4);
        assert_eq!(buf, [4, 5, 6, 7]);
        // a read runs out where the dump's memory does
        let mut buf = [0u8; 8];
        assert_eq!(d.read(0x700c, &mut buf), 4);
        assert_eq!(d.read(0x9000, &mut buf), 0);
        assert!(d.resume(Resume::All).is_err());
    }

    #[test]
    fn refuses_what_is_not_a_minidump_or_is_cut_short() {
        assert!(Minidump::parse(b"\x7fELF....".to_vec()).is_err());
        let whole = sample();
        let e = Minidump::parse(whole[..40].to_vec()).err().unwrap().to_string();
        assert!(e.contains("cut short"), "{e}");
        // streams past the end of the file are left out, and a dump with no thread says so
        let e = Minidump::parse(whole[..100].to_vec()).err().unwrap().to_string();
        assert!(e.contains("no thread"), "{e}");
    }
}
