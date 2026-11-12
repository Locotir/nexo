//! The emulator's memory: 4 KiB pages made on demand, each with write and exec flags, and a record of
//! every byte range the run wrote. Nothing here touches the real process: it is plain bytes in maps.

use std::collections::{BTreeMap, HashMap};

pub(crate) const PAGE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    Write,
    Exec,
}

impl Access {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Exec => "run",
        }
    }
}

/// An access the memory refused: nothing is mapped there, or the page does not allow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fault {
    pub addr: u64,
    pub access: Access,
    /// The page exists but does not allow the access (a write to code or read-only data).
    pub mapped: bool,
}

struct Page {
    bytes: Box<[u8; PAGE as usize]>,
    write: bool,
    exec: bool,
}

#[derive(Default)]
pub(crate) struct Mem {
    pages: HashMap<u64, Page>,
    /// Ranges the run wrote: start to end (exclusive), merged, never touching.
    written: BTreeMap<u64, u64>,
    /// Set when a write lands on an executable page (decoded instructions there are stale).
    pub(crate) code_written: bool,
}

impl Mem {
    /// Make the pages that cover `[addr, addr+len)`; pages already there keep their bytes and gain the flags.
    pub(crate) fn map(&mut self, addr: u64, len: u64, write: bool, exec: bool) {
        if len == 0 {
            return;
        }
        let first = addr / PAGE;
        let last = (addr.saturating_add(len - 1)) / PAGE;
        for p in first..=last {
            let page = self.pages.entry(p).or_insert_with(|| Page {
                bytes: Box::new([0; PAGE as usize]),
                write: false,
                exec: false,
            });
            page.write |= write;
            page.exec |= exec;
        }
    }

    pub(crate) fn is_mapped(&self, addr: u64) -> bool {
        self.pages.contains_key(&(addr / PAGE))
    }

    /// Put bytes in mapped pages without flags or the written record (loading the file, presets).
    pub(crate) fn load(&mut self, addr: u64, data: &[u8]) -> Result<(), Fault> {
        self.copy_in(addr, data, None)
    }

    /// A write by the program: it needs writable pages and is recorded.
    pub(crate) fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), Fault> {
        if data.is_empty() {
            return Ok(());
        }
        self.copy_in(addr, data, Some(Access::Write))?;
        self.record(addr, addr.wrapping_add(data.len() as u64));
        Ok(())
    }

    fn copy_in(&mut self, addr: u64, data: &[u8], check: Option<Access>) -> Result<(), Fault> {
        // check every page first, so a refused write changes nothing
        if data.is_empty() {
            return Ok(());
        }
        let last = addr.wrapping_add(data.len() as u64 - 1);
        if last < addr {
            return Err(Fault { addr: 0, access: Access::Write, mapped: false });
        }
        for p in addr / PAGE..=last / PAGE {
            let a = (p * PAGE).max(addr);
            let page = self.pages.get(&p).ok_or(Fault { addr: a, access: Access::Write, mapped: false })?;
            if check.is_some() && !page.write {
                return Err(Fault { addr: a, access: Access::Write, mapped: true });
            }
            if check.is_some() && page.exec {
                self.code_written = true;
            }
        }
        let mut done = 0;
        while done < data.len() {
            let a = addr.wrapping_add(done as u64);
            let off = (a % PAGE) as usize;
            let n = (PAGE as usize - off).min(data.len() - done);
            let page = self.pages.get_mut(&(a / PAGE)).expect("checked above");
            page.bytes[off..off + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        Ok(())
    }

    pub(crate) fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), Fault> {
        let mut done = 0;
        while done < buf.len() {
            let a = addr.wrapping_add(done as u64);
            let page = self.pages.get(&(a / PAGE)).ok_or(Fault { addr: a, access: Access::Read, mapped: false })?;
            let off = (a % PAGE) as usize;
            let n = (PAGE as usize - off).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&page.bytes[off..off + n]);
            done += n;
        }
        Ok(())
    }

    pub(crate) fn read_u(&self, addr: u64, size: usize) -> Result<u64, Fault> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b[..size])?;
        Ok(u64::from_le_bytes(b))
    }

    pub(crate) fn write_u(&mut self, addr: u64, size: usize, v: u64) -> Result<(), Fault> {
        self.write(addr, &v.to_le_bytes()[..size])
    }

    /// Up to 15 instruction bytes at `addr`: the first must be on an executable page; the rest stop at
    /// the first byte that is not.
    pub(crate) fn fetch(&self, addr: u64, buf: &mut [u8; 15]) -> Result<usize, Fault> {
        let mut n = 0;
        while n < 15 {
            let a = addr.wrapping_add(n as u64);
            match self.pages.get(&(a / PAGE)) {
                Some(p) if p.exec => {
                    buf[n] = p.bytes[(a % PAGE) as usize];
                    n += 1;
                }
                Some(_) if n == 0 => return Err(Fault { addr: a, access: Access::Exec, mapped: true }),
                None if n == 0 => return Err(Fault { addr: a, access: Access::Exec, mapped: false }),
                _ => break,
            }
        }
        Ok(n)
    }

    /// The bytes of a C string at `addr`, without the NUL, at most `max`; the flag is false when no NUL was
    /// found within `max` or the string runs into unmapped memory (then the fault is the error).
    pub(crate) fn cstr(&self, addr: u64, max: usize) -> Result<(Vec<u8>, bool), Fault> {
        let mut out = Vec::new();
        let mut a = addr;
        while out.len() < max {
            let page = self.pages.get(&(a / PAGE)).ok_or(Fault { addr: a, access: Access::Read, mapped: false })?;
            let off = (a % PAGE) as usize;
            let rest = &page.bytes[off..];
            let room = max - out.len();
            if let Some(z) = rest.iter().take(room).position(|&b| b == 0) {
                out.extend_from_slice(&rest[..z]);
                return Ok((out, true));
            }
            let n = rest.len().min(room);
            out.extend_from_slice(&rest[..n]);
            a = a.wrapping_add(n as u64);
        }
        Ok((out, false))
    }

    fn record(&mut self, start: u64, end: u64) {
        if end <= start {
            return;
        }
        let (mut s, mut e) = (start, end);
        let touching: Vec<u64> =
            self.written.range(..=e).rev().take_while(|(_, &end)| end >= s).map(|(&k, _)| k).collect();
        for k in touching {
            let ke = self.written.remove(&k).unwrap_or(k);
            s = s.min(k);
            e = e.max(ke);
        }
        self.written.insert(s, e);
    }

    /// Every range the run wrote, in address order.
    pub(crate) fn written(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.written.iter().map(|(&s, &e)| (s, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_merged_into_ranges_and_refused_on_read_only_pages() {
        let mut m = Mem::default();
        m.map(0x1000, 0x2000, true, false);
        m.map(0x5000, 0x10, false, false);
        m.write(0x1ffe, &[1, 2, 3, 4]).unwrap();
        m.write(0x2002, &[5]).unwrap();
        m.write(0x1100, &[9]).unwrap();
        assert_eq!(m.written().collect::<Vec<_>>(), vec![(0x1100, 0x1101), (0x1ffe, 0x2003)]);
        assert_eq!(m.read_u(0x1ffe, 4).unwrap(), 0x0403_0201);
        let f = m.write(0x5000, &[1]).unwrap_err();
        assert!(f.mapped && f.access == Access::Write);
        let f = m.read_u(0x9000, 1).unwrap_err();
        assert!(!f.mapped && f.addr == 0x9000);
        m.load(0x1200, b"hi\0").unwrap();
        assert_eq!(m.cstr(0x1200, 64).unwrap(), (b"hi".to_vec(), true));
    }
}
