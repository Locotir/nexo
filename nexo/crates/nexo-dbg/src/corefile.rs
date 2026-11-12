//! A Linux x86-64 core file read as a program that stopped for good: its threads' registers, its memory as the
//! dump kept it (the pages a dump leaves out because a file on disk holds them are read from that file), its
//! maps and the files it had loaded. Nothing in it can run, change or be stopped.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::platform::{Event, HwSlot, Map, Module, Os, Resume, Target, Tid};
use crate::regs::Regs;
use crate::{fail, Result};

const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const NT_PRSTATUS: u32 = 1;
const NT_SIGINFO: u32 = 0x5349_4749;
const NT_FILE: u32 = 0x4649_4c45;
const ET_CORE: u16 = 4;
const EM_X86_64: u16 = 62;

struct Seg {
    vaddr: u64,
    memsz: u64,
    filesz: u64,
    offset: u64,
    flags: u32,
}

/// One file-backed mapping from the dump's `NT_FILE` note.
struct Mapped {
    start: u64,
    end: u64,
    offset: u64,
    path: String,
}

struct Thread {
    tid: Tid,
    regs: Regs,
    signal: u32,
}

/// What a core file says about how the program died.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Death {
    pub signal: u32,
    pub code: i32,
    pub addr: Option<u64>,
    /// What the faulting instruction was doing with `addr`, when the dump says (a Windows minidump does).
    pub touch: Option<crate::platform::Touch>,
}

pub struct Core {
    file: File,
    segs: Vec<Seg>,
    files: Vec<Mapped>,
    threads: Vec<Thread>,
    death: Option<Death>,
    disk: Option<(String, File)>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

impl Core {
    pub fn open(path: &Path) -> Result<Core> {
        let mut file =
            File::open(path).map_err(|e| crate::Error::Fail(format!("cannot open {}: {e}", path.display())))?;
        let mut head = [0u8; 64];
        file.read_exact(&mut head).map_err(|_| crate::Error::Fail(format!("{} is not an ELF file", path.display())))?;
        if &head[..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
            return fail(format!("{} is not a 64-bit little-endian ELF file", path.display()));
        }
        if u16_at(&head, 16) != Some(ET_CORE) {
            return fail(format!("{} is an ELF file but not a core file", path.display()));
        }
        if u16_at(&head, 18) != Some(EM_X86_64) {
            return fail("only x86-64 core files can be read");
        }
        let phoff = u64_at(&head, 32).unwrap_or(0);
        let phentsize = u16_at(&head, 54).unwrap_or(0) as usize;
        let phnum = u16_at(&head, 56).unwrap_or(0) as usize;
        if phentsize < 56 || phnum == 0 || phnum > 65535 {
            return fail("the core file has no program headers");
        }
        if phentsize * phnum > 16 << 20 {
            return fail(format!(
                "the core file's program header table claims {phnum} entries of {phentsize} bytes ({} MiB); a real core is far smaller, so the header is damaged",
                (phentsize * phnum) >> 20
            ));
        }
        let mut table = vec![0u8; phentsize * phnum];
        file.seek(SeekFrom::Start(phoff))?;
        file.read_exact(&mut table).map_err(|_| {
            crate::Error::Fail(format!("the core file is cut short: its program header table (at offset {phoff:#x}, {} bytes) runs past the end", table.len()))
        })?;
        let mut core = Core { file, segs: Vec::new(), files: Vec::new(), threads: Vec::new(), death: None, disk: None };
        let mut notes: Vec<(u64, u64)> = Vec::new();
        for i in 0..phnum {
            let h = &table[i * phentsize..];
            let kind = u32_at(h, 0).unwrap_or(0);
            let (flags, offset, vaddr) =
                (u32_at(h, 4).unwrap_or(0), u64_at(h, 8).unwrap_or(0), u64_at(h, 16).unwrap_or(0));
            let (filesz, memsz) = (u64_at(h, 32).unwrap_or(0), u64_at(h, 40).unwrap_or(0));
            match kind {
                PT_LOAD => core.segs.push(Seg { vaddr, memsz, filesz, offset, flags }),
                PT_NOTE => notes.push((offset, filesz)),
                _ => {}
            }
        }
        for (off, len) in notes {
            if len > 64 << 20 {
                continue;
            }
            let mut b = vec![0u8; len as usize];
            core.file.seek(SeekFrom::Start(off))?;
            if core.file.read_exact(&mut b).is_ok() {
                core.notes(&b);
            }
        }
        if core.threads.is_empty() {
            return fail("the core file holds no thread registers");
        }
        // the signal that killed it: the siginfo note when there is one, else the first thread's
        if core.death.is_none() {
            let signal = core.threads[0].signal;
            core.death = (signal != 0).then_some(Death { signal, code: 0, addr: None, touch: None });
        }
        Ok(core)
    }

    fn notes(&mut self, b: &[u8]) {
        let mut at = 0usize;
        while at + 12 <= b.len() {
            let (namesz, descsz, kind) = (
                u32_at(b, at).unwrap_or(0) as usize,
                u32_at(b, at + 4).unwrap_or(0) as usize,
                u32_at(b, at + 8).unwrap_or(0),
            );
            let name_at = at + 12;
            let desc_at = name_at + namesz.div_ceil(4) * 4;
            let Some(desc) = b.get(desc_at..desc_at + descsz) else { break };
            let name = b.get(name_at..name_at + namesz).unwrap_or(&[]);
            if name.starts_with(b"CORE") {
                match kind {
                    NT_PRSTATUS if descsz >= 112 + 27 * 8 => {
                        let mut u = [0u64; 27];
                        for (i, w) in u.iter_mut().enumerate() {
                            *w = u64_at(desc, 112 + i * 8).unwrap_or(0);
                        }
                        self.threads.push(Thread {
                            tid: u32_at(desc, 32).unwrap_or(0),
                            regs: Regs::from_user(&u),
                            signal: u16_at(desc, 12).unwrap_or(0) as u32,
                        });
                    }
                    NT_SIGINFO if descsz >= 24 && self.death.is_none() => {
                        let signal = u32_at(desc, 0).unwrap_or(0);
                        let code = u32_at(desc, 8).unwrap_or(0) as i32;
                        // a fault carries the address it is about; a signal sent by a process carries a pid there
                        let addr = u64_at(desc, 16).filter(|_| matches!(signal, 4 | 7 | 8 | 11));
                        self.death = (signal != 0).then_some(Death { signal, code, addr, touch: None });
                    }
                    NT_FILE => self.files(desc),
                    _ => {}
                }
            }
            at = desc_at + descsz.div_ceil(4) * 4;
        }
    }

    fn files(&mut self, d: &[u8]) {
        let count = u64_at(d, 0).unwrap_or(0) as usize;
        if count > d.len() / 24 {
            return;
        }
        let page = u64_at(d, 8).unwrap_or(4096);
        let names_at = 16 + count.saturating_mul(24);
        let Some(names) = d.get(names_at..) else { return };
        let mut names = names.split(|&c| c == 0);
        for i in 0..count {
            let (Some(start), Some(end), Some(pg)) =
                (u64_at(d, 16 + i * 24), u64_at(d, 24 + i * 24), u64_at(d, 32 + i * 24))
            else {
                break;
            };
            let path = names.next().map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
            self.files.push(Mapped { start, end, offset: pg.saturating_mul(page), path });
        }
    }

    /// How the program died, when the dump says.
    pub fn death(&self) -> Option<Death> {
        self.death
    }

    /// The program's own file as the dump names it (the first file mapped), when there is one.
    pub fn exe(&self) -> Option<PathBuf> {
        self.files.first().map(|m| PathBuf::from(&m.path))
    }

    /// A chunk of memory at `addr`, at most `want` bytes, from the dump or the mapped file on disk.
    fn chunk(&mut self, addr: u64, want: usize, out: &mut [u8]) -> usize {
        let Some(s) = self.segs.iter().find(|s| addr >= s.vaddr && addr < s.vaddr.saturating_add(s.memsz)) else {
            return 0;
        };
        let (vaddr, memsz, filesz, offset) = (s.vaddr, s.memsz, s.filesz, s.offset);
        let rel = addr - vaddr;
        if rel < filesz {
            let n = want.min((filesz - rel) as usize);
            let ok = self.file.seek(SeekFrom::Start(offset.saturating_add(rel))).is_ok()
                && self.file.read_exact(&mut out[..n]).is_ok();
            return if ok { n } else { 0 };
        }
        // not in the dump: a file on disk may hold it, else it was zero
        if let Some(m) = self.files.iter().find(|m| addr >= m.start && addr < m.end) {
            let n = want.min((m.end - addr).min(vaddr.saturating_add(memsz) - addr) as usize);
            let (path, at) = (m.path.clone(), m.offset.saturating_add(addr - m.start));
            if self.disk.as_ref().map(|d| d.0.as_str()) != Some(path.as_str()) {
                self.disk = File::open(&path).ok().map(|f| (path, f));
            }
            if let Some((_, f)) = self.disk.as_mut() {
                if f.seek(SeekFrom::Start(at)).is_ok() {
                    let mut got = 0;
                    while got < n {
                        match f.read(&mut out[got..n]) {
                            Ok(0) | Err(_) => break,
                            Ok(k) => got += k,
                        }
                    }
                    if got > 0 {
                        return got;
                    }
                }
            }
            return 0;
        }
        let n = want.min((vaddr.saturating_add(memsz) - addr) as usize);
        out[..n].fill(0);
        n
    }
}

const ENDED: &str = "this is a core file: the program is not running and cannot be changed";

impl Target for Core {
    fn os(&self) -> Os {
        Os::Linux
    }
    fn pid(&self) -> u32 {
        self.threads.first().map_or(0, |t| t.tid)
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
            None => fail(format!("no thread {tid} in the core file")),
        }
    }
    fn set_regs(&mut self, _: Tid, _: &Regs) -> Result<()> {
        fail(ENDED)
    }
    fn read(&mut self, addr: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let n = self.chunk(addr.wrapping_add(done as u64), buf.len() - done, &mut buf[done..]);
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
        let mut out = Vec::new();
        for s in &self.segs {
            let end = s.vaddr.saturating_add(s.memsz);
            // a segment may cover several files: cut it where the file mappings cut it
            let mut at = s.vaddr;
            let mut inside: Vec<&Mapped> = self.files.iter().filter(|m| m.start < end && m.end > s.vaddr).collect();
            inside.sort_by_key(|m| m.start);
            for m in inside {
                if m.start > at {
                    out.push(Map {
                        start: at,
                        end: m.start,
                        read: s.flags & 4 != 0,
                        write: s.flags & 2 != 0,
                        exec: s.flags & 1 != 0,
                        offset: 0,
                        path: String::new(),
                    });
                }
                let (a, b) = (m.start.max(at), m.end.min(end));
                if b > a {
                    out.push(Map {
                        start: a,
                        end: b,
                        read: s.flags & 4 != 0,
                        write: s.flags & 2 != 0,
                        exec: s.flags & 1 != 0,
                        offset: m.offset.saturating_add(a - m.start),
                        path: m.path.clone(),
                    });
                    at = b;
                }
            }
            if at < end {
                out.push(Map {
                    start: at,
                    end,
                    read: s.flags & 4 != 0,
                    write: s.flags & 2 != 0,
                    exec: s.flags & 1 != 0,
                    offset: 0,
                    path: String::new(),
                });
            }
        }
        out.sort_by_key(|m| m.start);
        Ok(out)
    }
    fn modules(&mut self) -> Result<Vec<Module>> {
        let mut out: Vec<Module> = Vec::new();
        for m in &self.files {
            if m.path.starts_with('/') && m.offset == 0 && !out.iter().any(|o| o.path == m.path) {
                out.push(Module { path: m.path.clone(), base: m.start });
            }
        }
        Ok(out)
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

    fn note(out: &mut Vec<u8>, kind: u32, desc: &[u8]) {
        out.extend(5u32.to_le_bytes());
        out.extend((desc.len() as u32).to_le_bytes());
        out.extend(kind.to_le_bytes());
        out.extend(b"CORE\0\0\0\0");
        out.extend(desc);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }

    /// A small valid x86-64 core: one thread, a siginfo, one mapped file, one dumped page.
    fn sample() -> Vec<u8> {
        let mut notes = Vec::new();
        let mut st = vec![0u8; 336];
        st[0..4].copy_from_slice(&11u32.to_le_bytes());
        st[12..14].copy_from_slice(&11u16.to_le_bytes());
        st[32..36].copy_from_slice(&1234u32.to_le_bytes());
        note(&mut notes, NT_PRSTATUS, &st);
        let mut si = vec![0u8; 128];
        si[0..4].copy_from_slice(&11u32.to_le_bytes());
        note(&mut notes, NT_SIGINFO, &si);
        let mut nf = Vec::new();
        nf.extend(1u64.to_le_bytes());
        nf.extend(4096u64.to_le_bytes());
        nf.extend(0x40_0000u64.to_le_bytes());
        nf.extend(0x40_2000u64.to_le_bytes());
        nf.extend(0u64.to_le_bytes());
        nf.extend(b"/bin/true\0");
        note(&mut notes, NT_FILE, &nf);
        let ph = |kind: u32, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64| {
            let mut h = Vec::new();
            h.extend(kind.to_le_bytes());
            h.extend(flags.to_le_bytes());
            h.extend(off.to_le_bytes());
            h.extend(vaddr.to_le_bytes());
            h.extend(0u64.to_le_bytes());
            h.extend(filesz.to_le_bytes());
            h.extend(memsz.to_le_bytes());
            h.extend(1u64.to_le_bytes());
            h
        };
        let data_at = 64 + 56 * 3 + notes.len();
        let mut e = Vec::new();
        e.extend(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
        e.extend(4u16.to_le_bytes());
        e.extend(62u16.to_le_bytes());
        e.extend(1u32.to_le_bytes());
        e.extend(0u64.to_le_bytes());
        e.extend(64u64.to_le_bytes());
        e.extend(0u64.to_le_bytes());
        e.extend(0u32.to_le_bytes());
        e.extend(64u16.to_le_bytes());
        e.extend(56u16.to_le_bytes());
        e.extend(3u16.to_le_bytes());
        e.extend([0u8; 6]);
        e.extend(ph(PT_NOTE, 0, (64 + 56 * 3) as u64, 0, notes.len() as u64, 0));
        e.extend(ph(PT_LOAD, 6, data_at as u64, 0x7ff0_0000, 4096, 4096));
        e.extend(ph(PT_LOAD, 5, 0, 0x40_0000, 0, 0x2000));
        e.extend(notes);
        e.extend(vec![7u8; 4096]);
        e
    }

    /// Open and use a core the way the debugger does; never panic, never hang.
    fn exercise(bytes: &[u8], tag: &str) {
        let path = std::env::temp_dir().join(format!("nexo-core-harden-{}-{tag}.core", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        if let Ok(mut c) = Core::open(&path) {
            let mut buf = [0u8; 300];
            for a in [0u64, 0x40_0000, 0x40_1fff, 0x7ff0_0000, 0x7ff0_0ff0, u64::MAX - 10, u64::MAX] {
                let _ = c.read(a, &mut buf);
            }
            let _ = c.maps();
            let _ = c.modules();
            let _ = c.death();
            let _ = c.exe();
            for t in c.threads() {
                let _ = c.regs(t);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_sample_core_reads() {
        let path = std::env::temp_dir().join(format!("nexo-core-sample-{}.core", std::process::id()));
        std::fs::write(&path, sample()).unwrap();
        let mut c = Core::open(&path).unwrap();
        let mut b = [0u8; 4];
        assert_eq!(c.read(0x7ff0_0000, &mut b), 4);
        assert_eq!(b, [7; 4]);
        assert_eq!(c.threads(), vec![1234]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn damaged_cores_never_panic() {
        let good = sample();
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let cases: usize = std::env::var("NEXO_DBG_HARDEN_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
        for i in 0..cases {
            let mut b = good.clone();
            match next() % 5 {
                0 => {
                    let n = (next() as usize) % b.len();
                    b.truncate(n);
                }
                1 => {
                    let at = (next() as usize) % b.len();
                    b[at] ^= 1 << (next() % 8);
                }
                2 => {
                    // smash a whole field with an extreme value, mostly inside the headers and notes
                    let at = (next() as usize) % (64 + 56 * 3 + 400).min(b.len() - 8);
                    let v = [u64::MAX, 1 << 63, 1 << 40, 0, 0xffff_ffff][(next() % 5) as usize];
                    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
                }
                3 => {
                    let at = (next() as usize) % (64 + 56 * 3 + 400).min(b.len() - 4);
                    b[at..at + 4].copy_from_slice(&(next() as u32).to_le_bytes());
                }
                _ => {
                    for _ in 0..4 {
                        let at = (next() as usize) % b.len();
                        b[at] = next() as u8;
                    }
                }
            }
            let t0 = std::time::Instant::now();
            exercise(&b, &i.to_string());
            assert!(t0.elapsed() < Duration::from_secs(5), "case {i} took {:?}", t0.elapsed());
        }
    }

    #[test]
    fn a_huge_program_header_table_is_refused_by_name() {
        let mut b = sample();
        b[54..56].copy_from_slice(&60000u16.to_le_bytes());
        b[56..58].copy_from_slice(&60000u16.to_le_bytes());
        let path = std::env::temp_dir().join(format!("nexo-core-huge-{}.core", std::process::id()));
        std::fs::write(&path, b).unwrap();
        let e = match Core::open(&path) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("opened"),
        };
        let _ = std::fs::remove_file(&path);
        assert!(e.contains("damaged") && e.contains("60000"), "{e}");
    }
}
