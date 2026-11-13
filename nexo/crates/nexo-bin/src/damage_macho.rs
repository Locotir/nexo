//! Mach-O for `damage`: what the headers of a thin 64-bit file or a universal file promise, so a cut
//! file says where it was cut instead of reading as empty.

const THIN: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
const FAT: [u8; 4] = [0xca, 0xfe, 0xba, 0xbe];
const FAT64: [u8; 4] = [0xca, 0xfe, 0xba, 0xbf];

fn le32(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?).into())
}
fn le64(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}
fn be32(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_be_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?).into())
}
fn be64(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_be_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// A 16-byte segment or section name.
fn name16(b: &[u8], o: u64) -> String {
    let s = usize::try_from(o).ok().and_then(|o| b.get(o..o.checked_add(16)?)).unwrap_or(&[]);
    String::from_utf8_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]).into_owned()
}

/// The slice count of a universal file; `None` for anything else, a Java class too (its version is
/// where the count would be, and is 45 or more).
fn fat_count(d: &[u8]) -> Option<u64> {
    let n = be32(d, 4)?;
    (d.starts_with(&FAT64) || (d.starts_with(&FAT) && n < 45)).then_some(n)
}

pub(crate) fn is_macho(d: &[u8]) -> bool {
    d.starts_with(&THIN) || fat_count(d).is_some()
}

/// What a file that starts like a Mach-O file but is too short to be read says about itself.
pub(crate) fn short(d: &[u8]) -> Option<String> {
    let len = d.len();
    if (1..4).contains(&len) && (THIN.starts_with(d) || FAT.starts_with(d) || FAT64.starts_with(d)) {
        return Some(format!("is {len} bytes and may be the start of a Mach-O file that was cut"));
    }
    if d.starts_with(&THIN) && len < 32 {
        return Some(format!("starts like a Mach-O file but is only {len} bytes; its header alone is 32 bytes"));
    }
    if (d.starts_with(&FAT) || d.starts_with(&FAT64)) && len < 8 {
        return Some(format!(
            "starts like a Mach-O universal file but is only {len} bytes; its header alone is 8 bytes"
        ));
    }
    let n = fat_count(d)?;
    let need = 8 + n * if d.starts_with(&FAT64) { 32 } else { 20 };
    (need > len as u64).then(|| {
        format!(
            "is a Mach-O universal file cut at {len} bytes: its slice table ({n} entries, promised at offset 0x4) reaches byte {need:#x}"
        )
    })
}

/// The furthest byte a promise reaches: (byte, offset of the promise, what it is about).
#[derive(Default)]
struct Worst(Option<(u64, u64, String)>);

impl Worst {
    fn see(&mut self, need: Option<u64>, at: u64, why: impl FnOnce() -> String) {
        if let Some(n) = need {
            if self.0.as_ref().is_none_or(|w| n > w.0) {
                self.0 = Some((n, at, why()));
            }
        }
    }
}

/// (the furthest byte the headers promise, offset of the promise, what it is about)
pub(crate) fn reach(d: &[u8]) -> Option<(u64, u64, String)> {
    let mut w = Worst::default();
    if d.starts_with(&THIN) {
        thin(d, 0, &mut w);
    } else if let Some(n) = fat_count(d) {
        let wide = d.starts_with(&FAT64);
        let entry = if wide { 32 } else { 20 };
        w.see(Some(8 + n * entry), 4, || format!("the slice table ({n} entries of {entry} bytes)"));
        for i in 0..n.min(64) {
            let h = 8 + i * entry;
            let (off, size) = if wide { (be64(d, h + 8), be64(d, h + 16)) } else { (be32(d, h + 8), be32(d, h + 12)) };
            let (Some(off), Some(size)) = (off, size) else { break };
            w.see(off.checked_add(size), h, || format!("slice #{} ({size:#x} bytes at {off:#x})", i + 1));
            if usize::try_from(off).ok().and_then(|o| d.get(o..)).is_some_and(|s| s.starts_with(&THIN)) {
                thin(d, off, &mut w);
            }
        }
    }
    w.0
}

/// The promises of one thin file that starts at `base`: its load commands and the file ranges they name.
fn thin(d: &[u8], base: u64, w: &mut Worst) {
    let (Some(ncmds), Some(size)) = (le32(d, base + 16), le32(d, base + 20)) else { return };
    let cmds_end = base.saturating_add(32 + size);
    w.see(Some(cmds_end), base + 20, || format!("the load commands ({size} bytes after the 32-byte header)"));
    let end = cmds_end.min(d.len() as u64);
    // a range of the file: offsets in a slice count from the slice start
    let range = |w: &mut Worst, off: u64, n: u64, at: u64, what: &str| {
        if n > 0 && off > 0 {
            w.see(base.checked_add(off).and_then(|o| o.checked_add(n)), at, || {
                format!("{what} ({n:#x} bytes at {off:#x})")
            });
        }
    };
    let mut off = base + 32;
    for _ in 0..ncmds.min(16_384) {
        let (Some(cmd), Some(cs)) = (le32(d, off), le32(d, off + 4)) else { break };
        if cs < 8 || off + cs > end {
            break;
        }
        let f32 = |o: u64| le32(d, off + o).unwrap_or(0);
        match cmd {
            0x19 => {
                let name = name16(d, off + 8);
                range(
                    w,
                    le64(d, off + 40).unwrap_or(0),
                    le64(d, off + 48).unwrap_or(0),
                    off,
                    &format!("segment {name}"),
                );
                let n = f32(64).min(cs.saturating_sub(72) / 80);
                for k in 0..n {
                    let s = off + 72 + k * 80;
                    let ty = le32(d, s + 64).unwrap_or(0) & 0xff;
                    if !matches!(ty, 0x1 | 0xc | 0x12) {
                        let what = format!("section {}", name16(d, s));
                        range(w, le32(d, s + 48).unwrap_or(0), le64(d, s + 40).unwrap_or(0), s, &what);
                    }
                }
            }
            0x2 => {
                range(w, f32(8), f32(12).saturating_mul(16), off, "the symbol table");
                range(w, f32(16), f32(20), off, "the string table");
            }
            0xb => {
                range(w, f32(56), f32(60).saturating_mul(4), off, "the indirect symbol table");
                range(w, f32(64), f32(68).saturating_mul(8), off, "the external relocations");
                range(w, f32(72), f32(76).saturating_mul(8), off, "the local relocations");
            }
            // linkedit data: code signature, split info, function starts, data in code, exports trie, fixups
            0x1d | 0x1e | 0x26 | 0x29 | 0x2b | 0x2e | 0x8000_0033 | 0x8000_0034 => {
                range(w, f32(8), f32(12), off, "linkedit data");
            }
            0x22 | 0x8000_0022 => {
                for (o, what) in [
                    (8, "rebase info"),
                    (16, "bind info"),
                    (24, "weak bind info"),
                    (32, "lazy bind info"),
                    (40, "export info"),
                ] {
                    range(w, f32(o), f32(o + 4), off, what);
                }
            }
            _ => {}
        }
        off += cs;
    }
}
