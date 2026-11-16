//! Reading a Mach-O code signature back: where the signature is, whether it is ad hoc, and whether the page
//! hashes of its CodeDirectory match the bytes they cover (what the kernel checks before it runs a page).
//! The CMS of a real certificate is not checked: only the hashes are, so a file signed by a developer
//! certificate is reported as signed by someone else.

use sha2::{Digest, Sha256};

use nexo_binmagic::{be32, FAT_MAGIC};

/// What a file's code signature is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    /// No `LC_CODE_SIGNATURE`.
    Unsigned,
    /// An ad hoc signature whose page hashes all match, with how many pages it covers (summed over the slices of a
    /// universal file).
    AdHoc { pages: usize },
    /// An ad hoc signature none of whose CodeDirectories uses SHA-256 with 4 KiB pages (for example SHA-1 only), so
    /// its hashes were not checked.
    AdHocUnchecked,
    /// A signature that is not ad hoc (a certificate); its hashes were not checked.
    Certificate,
    /// A signature that does not hold, and why.
    Broken(String),
}

impl Signature {
    /// The words `info` shows for the file.
    pub fn describe(&self) -> String {
        match self {
            Signature::Unsigned => "no".into(),
            Signature::AdHoc { pages } => format!("yes (ad hoc, the {pages} page hashes match)"),
            Signature::AdHocUnchecked => "yes (ad hoc, its hash type is not checked)".into(),
            Signature::Certificate => "yes (by a certificate, not checked)".into(),
            Signature::Broken(why) => format!("yes (does not hold: {why})"),
        }
    }
}

/// Read the signature of a Mach-O file, thin or universal. Not a Mach-O file reads as unsigned.
pub fn read_signature(data: &[u8]) -> Signature {
    if be32(data, 0) != Some(FAT_MAGIC) {
        return thin(data);
    }
    let n = be32(data, 4).unwrap_or(0).min(nexo_binmagic::MAX_SLICES as u32) as usize;
    let mut pages = 0;
    let mut seen = Signature::Unsigned;
    for i in 0..n {
        let at = (8 + 20 * i) as u64;
        let (Some(off), Some(size)) = (be32(data, at + 8), be32(data, at + 12)) else {
            return Signature::Broken("the universal header is cut".into());
        };
        let body = (off as usize).checked_add(size as usize).and_then(|end| data.get(off as usize..end));
        match body.map(thin) {
            Some(Signature::AdHoc { pages: p }) => {
                pages += p;
                seen = Signature::AdHoc { pages };
            }
            Some(other) => return other,
            None => return Signature::Broken("a slice lies past the end of the file".into()),
        }
    }
    seen
}

fn thin(d: &[u8]) -> Signature {
    // load commands of a 64-bit little-endian file: ncmds at 16, the commands from 32
    let le = |o: usize| nexo_binmagic::u32_at(d, o as u64);
    if le(0) != Some(nexo_binmagic::MH_MAGIC_64) {
        return Signature::Unsigned;
    }
    let (Some(ncmds), Some(size)) = (le(16), le(20)) else { return Signature::Unsigned };
    let end = 32usize.saturating_add(size as usize).min(d.len());
    let mut at = 32usize;
    let mut sig = None;
    for _ in 0..ncmds {
        let (Some(cmd), Some(len)) = (le(at), le(at + 4)) else { break };
        if len < 8 || at + len as usize > end {
            break;
        }
        if cmd == 0x1d && len == 16 {
            sig = Some((le(at + 8), le(at + 12)));
        }
        at += len as usize;
    }
    let Some((Some(off), Some(len))) = sig else { return Signature::Unsigned };
    let broken = |why: &str| Signature::Broken(why.into());
    let (off, len) = (off as usize, len as usize);
    let Some(blob) = off.checked_add(len).and_then(|e| d.get(off..e)) else {
        return broken("the signature lies past the end of the file");
    };
    let at = |o: usize| be32(blob, o as u64).map(|v| v as usize);
    if at(0) != Some(0xfade_0cc0) {
        return broken("the signature is not a SuperBlob");
    }
    let count = at(8).unwrap_or(0).min(16);
    // slot 0 is the primary CodeDirectory; 0x1000.. are alternates (codesign writes a SHA-1 primary plus a SHA-256
    // alternate for older deployment targets)
    let cd_at = |slot: usize| {
        (0..count).find(|i| at(12 + 8 * i) == Some(slot)).and_then(|i| at(16 + 8 * i)).and_then(|o| blob.get(o..))
    };
    let Some(cd) = cd_at(0) else { return broken("it has no CodeDirectory") };
    let f = |cd: &[u8], o: usize| be32(cd, o as u64).map(|v| v as usize);
    if f(cd, 0) != Some(0xfade_0c02) {
        return broken("its CodeDirectory is cut");
    }
    if f(cd, 12).unwrap_or(0) & 0x2 == 0 {
        return Signature::Certificate;
    }
    let checkable = |cd: &[u8]| {
        f(cd, 0) == Some(0xfade_0c02)
            && (cd.get(36).copied(), cd.get(37).copied(), cd.get(39).copied()) == (Some(32), Some(2), Some(12))
    };
    let Some(cd) = std::iter::once(0).chain(0x1000..0x1005).filter_map(cd_at).find(|cd| checkable(cd)) else {
        return Signature::AdHocUnchecked;
    };
    let (Some(hash_off), Some(n_code), Some(limit)) = (f(cd, 16), f(cd, 28), f(cd, 32)) else {
        return broken("its CodeDirectory is cut");
    };
    let Some(code) = d.get(..limit) else { return broken("it covers bytes past the end of the file") };
    let want = limit.div_ceil(4096);
    if n_code != want {
        return broken("its page count does not fit the bytes it covers");
    }
    for (i, page) in code.chunks(4096).enumerate() {
        let start = hash_off + 32 * i;
        let stored = cd.get(start..start + 32);
        if stored != Some(Sha256::digest(page).as_slice()) {
            return Signature::Broken(format!("page {i} of the file differs from its hash"));
        }
    }
    Signature::AdHoc { pages: n_code }
}
