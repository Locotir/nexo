//! An ELF core file of a live Linux x86-64 process, written from plain data: the threads' registers, the process's
//! facts, the memory map and a way to read memory. Nothing here touches a process: the caller (`gcore`) gathers the
//! data, and [`crate::corefile`] (or gdb) reads the result back.
//!
//! Layout: the ELF header, the program headers (one `PT_NOTE`, one `PT_LOAD` per mapping), the notes, then each
//! mapping's bytes at a page-aligned offset in the order of the headers. A mapping whose bytes could not be read has
//! `filesz` 0: it stays in the headers (and in `NT_FILE` when a file backs it), so the reader finds it empty.

use std::io::{self, Write};

const PT_LOAD: u32 = 1;
const PT_NOTE: u32 = 4;
const NT_PRSTATUS: u32 = 1;
const NT_PRFPREG: u32 = 2;
const NT_PRPSINFO: u32 = 3;
const NT_AUXV: u32 = 6;
const NT_FILE: u32 = 0x4649_4c45;
const PAGE: u64 = 4096;
const EHDR: u64 = 64;
const PHDR: u64 = 56;
/// The size of `struct elf_prstatus` on x86-64.
const PRSTATUS: usize = 336;
/// The size of `struct elf_prpsinfo` on x86-64.
const PRPSINFO: usize = 136;
/// The `fxsave` area `NT_PRFPREG` holds (`struct user_fpregs_struct`).
pub const FPREGS: usize = 512;

/// One thread: its id, and its registers in the order of Linux's `user_regs_struct`.
pub struct ThreadIn {
    pub tid: u32,
    pub regs: [u64; 27],
    /// The first [`FPREGS`] bytes of the x87 and SSE state, when known.
    pub fpregs: Option<Vec<u8>>,
}

/// One mapping of the process. `flags` are the ELF ones: 4 read, 2 write, 1 execute.
pub struct MapIn {
    pub start: u64,
    pub end: u64,
    pub flags: u32,
    /// The bytes that go to the file: 0, or `end - start` when the memory could be read.
    pub filesz: u64,
    /// The offset into the mapped file, when a file backs it.
    pub file_off: u64,
    /// The file that backs it (an absolute path); anything else is not listed in `NT_FILE`.
    pub path: String,
}

/// Everything a core file says about the process.
pub struct CoreIn {
    pub pid: u32,
    pub ppid: u32,
    pub pgrp: u32,
    pub sid: u32,
    pub uid: u32,
    pub gid: u32,
    /// The program's name (15 bytes at most are kept) and its command line (79).
    pub name: String,
    pub args: String,
    /// `/proc/PID/auxv` as it is.
    pub auxv: Vec<u8>,
    /// The first thread is the one the core file is about.
    pub threads: Vec<ThreadIn>,
    pub maps: Vec<MapIn>,
}

fn align(n: u64, to: u64) -> u64 {
    n.div_ceil(to) * to
}

/// One note: name `CORE`, the descriptor, both padded to 4 bytes.
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

fn put(b: &mut [u8], at: usize, v: &[u8]) {
    b[at..at + v.len()].copy_from_slice(v);
}

fn prstatus(c: &CoreIn, t: &ThreadIn) -> Vec<u8> {
    let mut b = vec![0u8; PRSTATUS];
    put(&mut b, 32, &t.tid.to_le_bytes());
    put(&mut b, 36, &c.ppid.to_le_bytes());
    put(&mut b, 40, &c.pgrp.to_le_bytes());
    put(&mut b, 44, &c.sid.to_le_bytes());
    for (i, w) in t.regs.iter().enumerate() {
        put(&mut b, 112 + i * 8, &w.to_le_bytes());
    }
    // pr_fpvalid
    put(&mut b, 328, &u32::from(t.fpregs.is_some()).to_le_bytes());
    b
}

fn prpsinfo(c: &CoreIn) -> Vec<u8> {
    let mut b = vec![0u8; PRPSINFO];
    b[0] = b't';
    b[1] = b'T';
    put(&mut b, 16, &c.uid.to_le_bytes());
    put(&mut b, 20, &c.gid.to_le_bytes());
    put(&mut b, 24, &c.pid.to_le_bytes());
    put(&mut b, 28, &c.ppid.to_le_bytes());
    put(&mut b, 32, &c.pgrp.to_le_bytes());
    put(&mut b, 36, &c.sid.to_le_bytes());
    let name = c.name.as_bytes();
    put(&mut b, 40, &name[..name.len().min(15)]);
    let args = c.args.as_bytes();
    put(&mut b, 56, &args[..args.len().min(79)]);
    b
}

fn nt_file(maps: &[MapIn]) -> Vec<u8> {
    let files: Vec<&MapIn> = maps.iter().filter(|m| m.path.starts_with('/')).collect();
    let mut b = Vec::new();
    b.extend((files.len() as u64).to_le_bytes());
    b.extend(PAGE.to_le_bytes());
    for m in &files {
        b.extend(m.start.to_le_bytes());
        b.extend(m.end.to_le_bytes());
        b.extend((m.file_off / PAGE).to_le_bytes());
    }
    for m in &files {
        b.extend(m.path.as_bytes());
        b.push(0);
    }
    b
}

/// The notes: one `NT_PRSTATUS` (and `NT_PRFPREG`) per thread, the process, its auxiliary vector and its files.
fn notes(c: &CoreIn) -> Vec<u8> {
    let mut n = Vec::new();
    for t in &c.threads {
        note(&mut n, NT_PRSTATUS, &prstatus(c, t));
        if let Some(f) = t.fpregs.as_ref().filter(|f| f.len() >= FPREGS) {
            note(&mut n, NT_PRFPREG, &f[..FPREGS]);
        }
    }
    note(&mut n, NT_PRPSINFO, &prpsinfo(c));
    note(&mut n, NT_AUXV, &c.auxv);
    note(&mut n, NT_FILE, &nt_file(&c.maps));
    n
}

fn phdr(kind: u32, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64, al: u64) -> [u8; PHDR as usize] {
    let mut h = [0u8; PHDR as usize];
    put(&mut h, 0, &kind.to_le_bytes());
    put(&mut h, 4, &flags.to_le_bytes());
    put(&mut h, 8, &off.to_le_bytes());
    put(&mut h, 16, &vaddr.to_le_bytes());
    put(&mut h, 24, &0u64.to_le_bytes());
    put(&mut h, 32, &filesz.to_le_bytes());
    put(&mut h, 40, &memsz.to_le_bytes());
    put(&mut h, 48, &al.to_le_bytes());
    h
}

/// Write the core file; `read(addr, buf)` fills `buf` from the process's memory and says how many bytes it gave
/// (the rest of a mapping it cannot give is written as zeros). Returns the size of the file.
pub fn write_core(out: &mut dyn Write, c: &CoreIn, read: &mut dyn FnMut(u64, &mut [u8]) -> usize) -> io::Result<u64> {
    if c.threads.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a core file needs a thread"));
    }
    let notes = notes(c);
    let nph = 1 + c.maps.len() as u64;
    if nph > u64::from(u16::MAX) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "too many mappings for an ELF core file"));
    }
    let note_at = EHDR + PHDR * nph;
    let mut at = align(note_at + notes.len() as u64, PAGE);
    let mut heads: Vec<[u8; PHDR as usize]> = vec![phdr(PT_NOTE, 0, note_at, 0, notes.len() as u64, 0, 1)];
    let mut offsets = Vec::new();
    for m in &c.maps {
        let size = m.end.saturating_sub(m.start);
        let filesz = if m.filesz == 0 { 0 } else { size };
        heads.push(phdr(PT_LOAD, m.flags, if filesz == 0 { 0 } else { at }, m.start, filesz, size, PAGE));
        offsets.push(at);
        at += align(filesz, PAGE);
    }
    let mut eh = [0u8; EHDR as usize];
    eh[..16].copy_from_slice(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
    put(&mut eh, 16, &4u16.to_le_bytes());
    put(&mut eh, 18, &62u16.to_le_bytes());
    put(&mut eh, 20, &1u32.to_le_bytes());
    put(&mut eh, 32, &EHDR.to_le_bytes());
    put(&mut eh, 52, &(EHDR as u16).to_le_bytes());
    put(&mut eh, 54, &(PHDR as u16).to_le_bytes());
    put(&mut eh, 56, &(nph as u16).to_le_bytes());
    out.write_all(&eh)?;
    for h in &heads {
        out.write_all(h)?;
    }
    out.write_all(&notes)?;
    let mut pos = note_at + notes.len() as u64;
    let mut buf = vec![0u8; 1 << 20];
    for (m, off) in c.maps.iter().zip(&offsets) {
        let size = m.end.saturating_sub(m.start);
        if m.filesz == 0 || size == 0 {
            continue;
        }
        out.write_all(&vec![0u8; (off - pos) as usize])?;
        pos = *off;
        let mut done = 0u64;
        while done < size {
            let want = (size - done).min(buf.len() as u64) as usize;
            let got = read(m.start + done, &mut buf[..want]).min(want);
            buf[got..want].fill(0);
            out.write_all(&buf[..want])?;
            done += want as u64;
        }
        pos += size;
    }
    out.flush()?;
    // the last mapping's padding is not written: a reader never goes past the data a header names
    Ok(pos)
}

/// The facts `/proc/PID/stat` holds about a process: its name, parent, process group and session.
pub fn parse_stat(s: &str) -> Option<(String, u32, u32, u32)> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let name = s.get(open + 1..close)?.to_string();
    let mut f = s.get(close + 1..)?.split_whitespace();
    let _state = f.next()?;
    let ppid = f.next()?.parse().ok()?;
    let pgrp = f.next()?.parse().ok()?;
    let sid = f.next()?.parse().ok()?;
    Some((name, ppid, pgrp, sid))
}

/// The real user and group ids from `/proc/PID/status`.
pub fn parse_ids(status: &str) -> (u32, u32) {
    let id = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (id("Uid:"), id("Gid:"))
}

/// The ELF segment flags of a mapping's permissions.
pub fn elf_flags(read: bool, write: bool, exec: bool) -> u32 {
    u32::from(read) * 4 + u32::from(write) * 2 + u32::from(exec)
}

/// Whether a mapping goes into a core file: readable, and not the kernel's `[vvar]` pages, which cannot be read.
pub fn kept(read: bool, path: &str) -> bool {
    read && !path.starts_with("[vvar")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corefile::Core;
    use crate::platform::Target;

    fn sample() -> CoreIn {
        let mut r1 = [0u64; 27];
        r1[16] = 0x40_1000; // rip
        r1[19] = 0x7ffc_0000_1000; // rsp
        let mut r2 = r1;
        r2[16] = 0x40_2000;
        CoreIn {
            pid: 100,
            ppid: 1,
            pgrp: 100,
            sid: 100,
            uid: 1000,
            gid: 1000,
            name: "prog".into(),
            args: "prog arg".into(),
            auxv: vec![0u8; 32],
            threads: vec![
                ThreadIn { tid: 100, regs: r1, fpregs: Some(vec![0u8; FPREGS]) },
                ThreadIn { tid: 101, regs: r2, fpregs: None },
            ],
            maps: vec![
                MapIn {
                    start: 0x40_0000,
                    end: 0x40_2000,
                    flags: 5,
                    filesz: 0x2000,
                    file_off: 0,
                    path: "/bin/prog".into(),
                },
                MapIn {
                    start: 0x7ffc_0000_0000,
                    end: 0x7ffc_0000_2000,
                    flags: 6,
                    filesz: 0x2000,
                    file_off: 0,
                    path: "[stack]".into(),
                },
                MapIn { start: 0x90_0000, end: 0x90_1000, flags: 4, filesz: 0, file_off: 0, path: String::new() },
            ],
        }
    }

    #[test]
    fn a_core_written_from_threads_and_mappings_reads_back_with_the_crates_own_reader() {
        let c = sample();
        let mut file = Vec::new();
        let mut read = |a: u64, b: &mut [u8]| {
            // the code reads as 0x90, the stack as its address low byte
            for (i, x) in b.iter_mut().enumerate() {
                *x = if a < 0x1000_0000 { 0x90 } else { (a + i as u64) as u8 };
            }
            b.len()
        };
        let n = write_core(&mut file, &c, &mut read).unwrap();
        assert_eq!(n as usize, file.len());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.core");
        std::fs::write(&path, &file).unwrap();
        let mut core = Core::open(&path).unwrap();
        assert_eq!(core.threads(), vec![100, 101]);
        assert_eq!(core.regs(100).unwrap().rip, 0x40_1000);
        assert_eq!(core.regs(101).unwrap().rip, 0x40_2000);
        assert_eq!(core.regs(100).unwrap().rsp(), 0x7ffc_0000_1000);
        let mut b = [0u8; 4];
        assert_eq!(core.read(0x40_1000, &mut b), 4);
        assert_eq!(b, [0x90; 4]);
        assert_eq!(core.read(0x7ffc_0000_0010, &mut b), 4);
        assert_eq!(b[0], 0x10);
        // the mapping with no data is known (its bytes are zero), not missing
        assert_eq!(core.read(0x90_0000, &mut b), 4);
        let maps = core.maps().unwrap();
        assert!(maps.iter().any(|m| m.start == 0x40_0000 && m.path == "/bin/prog"), "{maps:?}");
        assert!(maps.iter().any(|m| m.start == 0x90_0000), "{maps:?}");
        assert_eq!(core.exe().map(|p| p.display().to_string()).as_deref(), Some("/bin/prog"));
    }

    #[test]
    fn a_short_read_is_padded_with_zeros_and_the_layout_does_not_move() {
        let mut c = sample();
        c.maps.truncate(1);
        let mut file = Vec::new();
        let mut read = |_: u64, b: &mut [u8]| {
            b[..4].fill(7);
            4
        };
        write_core(&mut file, &c, &mut read).unwrap();
        let ph = EHDR as usize + PHDR as usize;
        let off = u64::from_le_bytes(file[ph + 8..ph + 16].try_into().unwrap()) as usize;
        assert_eq!(off % 4096, 0);
        assert_eq!(&file[off..off + 5], &[7, 7, 7, 7, 0]);
        assert_eq!(file.len(), off + 0x2000);
    }

    #[test]
    fn stat_and_status_lines_give_the_process_facts() {
        let s = "123 (my (odd) prog) S 45 67 89 0 -1 4194304";
        assert_eq!(parse_stat(s), Some(("my (odd) prog".to_string(), 45, 67, 89)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_ids("Name:\tx\nUid:\t1000\t1000\t1000\t1000\nGid:\t100\t100\t100\t100\n"), (1000, 100));
        assert!(!kept(true, "[vvar]") && !kept(true, "[vvar_vclock]") && !kept(false, "") && kept(true, "[stack]"));
        assert_eq!(elf_flags(true, false, true), 5);
    }
}
