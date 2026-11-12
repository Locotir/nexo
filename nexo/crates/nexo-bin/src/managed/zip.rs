//! A small zip reader for `.jar` files: the central directory, stored and deflated entries.
//! Zip64 and encrypted entries are reported by name, never skipped quietly.

use crate::{Error, Result};

/// Largest file this reads (a jar bigger than this is not a program's own classes).
pub const MAX_ARCHIVE: usize = 256 << 20;
/// Largest single entry after inflating.
pub const MAX_ENTRY: usize = 32 << 20;

pub struct Entry {
    pub name: String,
    method: u16,
    csize: u32,
    usize: u32,
    local: u32,
    flags: u16,
}

pub struct Zip<'a> {
    data: &'a [u8],
    pub entries: Vec<Entry>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

pub fn looks_like_zip(d: &[u8]) -> bool {
    d.starts_with(b"PK\x03\x04") || d.starts_with(b"PK\x05\x06")
}

impl<'a> Zip<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self> {
        let bad = |m: &str| Error::NotRead(format!("not a readable zip/jar: {m}"));
        if data.len() > MAX_ARCHIVE {
            return Err(Error::NotRead(format!(
                "the archive is over {} MB, which this does not read",
                MAX_ARCHIVE >> 20
            )));
        }
        let lo = data.len().saturating_sub(22 + 65535);
        let eocd = (lo..data.len().saturating_sub(21))
            .rev()
            .find(|&i| data[i..].starts_with(b"PK\x05\x06"))
            .ok_or_else(|| bad("no end-of-directory record"))?;
        let count = u16_at(data, eocd + 10).ok_or_else(|| bad("cut short"))? as usize;
        let mut at = u32_at(data, eocd + 16).ok_or_else(|| bad("cut short"))? as usize;
        if count == 0xFFFF || at == 0xFFFF_FFFF {
            return Err(Error::NotRead("zip64 archives are not read yet".into()));
        }
        let mut entries = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            if data.get(at..at + 4) != Some(b"PK\x01\x02") {
                return Err(bad("central directory is damaged"));
            }
            let (flags, method) = (u16_at(data, at + 8), u16_at(data, at + 10));
            let (csize, usize_, nlen) = (u32_at(data, at + 20), u32_at(data, at + 24), u16_at(data, at + 28));
            let (xlen, clen, local) = (u16_at(data, at + 30), u16_at(data, at + 32), u32_at(data, at + 42));
            let (Some(flags), Some(method), Some(csize), Some(usize_), Some(nlen), Some(xlen), Some(clen), Some(local)) =
                (flags, method, csize, usize_, nlen, xlen, clen, local)
            else {
                return Err(bad("central directory is cut short"));
            };
            let name = data.get(at + 46..at + 46 + nlen as usize).ok_or_else(|| bad("name cut short"))?;
            entries.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method,
                csize,
                usize: usize_,
                local,
                flags,
            });
            at += 46 + nlen as usize + xlen as usize + clen as usize;
        }
        Ok(Zip { data, entries })
    }

    /// The bytes of one entry, or why they cannot be had.
    pub fn read(&self, e: &Entry) -> std::result::Result<Vec<u8>, String> {
        if e.flags & 1 != 0 {
            return Err("encrypted".into());
        }
        if e.csize == 0xFFFF_FFFF || e.usize == 0xFFFF_FFFF {
            return Err("zip64 entry".into());
        }
        if e.usize as usize > MAX_ENTRY {
            return Err(format!("{} MB after inflating, over the {} MB limit", e.usize >> 20, MAX_ENTRY >> 20));
        }
        let l = e.local as usize;
        if self.data.get(l..l + 4) != Some(b"PK\x03\x04") {
            return Err("local header is damaged".into());
        }
        let (n, x) = (u16_at(self.data, l + 26).unwrap_or(0) as usize, u16_at(self.data, l + 28).unwrap_or(0) as usize);
        let start = l + 30 + n + x;
        let raw = self.data.get(start..start + e.csize as usize).ok_or("data cut short")?;
        match e.method {
            0 => Ok(raw.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, MAX_ENTRY)
                .map_err(|e| format!("bad deflate data ({:?})", e.status)),
            m => Err(format!("compression method {m} is not supported")),
        }
    }
}
