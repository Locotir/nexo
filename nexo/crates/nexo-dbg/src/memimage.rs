//! A Mach-O library read from the program's memory. Since macOS 11 the system libraries live only in the dyld shared
//! cache: `/usr/lib/system/libsystem_c.dylib` has no file on disk, so its exports (`atoi`, `strlen`) are found only in
//! the copy the loader mapped. The header and load commands are read where the library is loaded, and its exports
//! trie from its `__LINKEDIT` segment, which the cache maps apart from the code: that segment is at its own `vmaddr`
//! moved by the library's slide, and the trie at its file offset counted from the segment's.

use std::path::PathBuf;

use nexo_bin::Image;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const LC_SEGMENT_64: u32 = 0x19;
const LC_DYLD_INFO: u32 = 0x22;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
/// The most bytes read for the load commands and for the trie; a system library's trie takes tens of KiB.
const MAX_CMDS: usize = 1 << 16;
const MAX_TRIE: usize = 16 << 20;

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The library loaded at `base`, and how many bytes from `base` its `__TEXT` segment (its code) reaches; `None` when
/// no 64-bit Mach-O header with a `__TEXT` segment is there. `rd` reads the program's memory and returns the count
/// read, as `Target::read` does. `path` is the name the image carries.
pub(crate) fn read(rd: &mut dyn FnMut(u64, &mut [u8]) -> usize, base: u64, path: &str) -> Option<(Image, u64)> {
    let mut head = [0u8; 32];
    if rd(base, &mut head) < head.len() || u32_at(&head, 0)? != MH_MAGIC_64 {
        return None;
    }
    let mut cmds = vec![0u8; (u32_at(&head, 20)? as usize).min(MAX_CMDS)];
    let got = rd(base.wrapping_add(32), &mut cmds);
    cmds.truncate(got);
    // The segments are kept with their file offsets taken out: in the cache those count from the start of the
    // cache file, and in the bytes built below they would point at the trie.
    let (mut kept, mut count) = (Vec::new(), 0u32);
    let (mut text, mut linkedit, mut trie) = (None, None, None);
    let mut off = 0usize;
    for _ in 0..u32_at(&head, 16)? {
        let (Some(cmd), Some(size)) = (u32_at(&cmds, off), u32_at(&cmds, off + 4)) else { break };
        let size = size as usize;
        let Some(b) = cmds.get(off..off.saturating_add(size)).filter(|_| size >= 8) else { break };
        match cmd {
            LC_SEGMENT_64 if size >= 72 => {
                let name = &b[8..24];
                let (vmaddr, vmsize, fileoff) = (u64_at(b, 24)?, u64_at(b, 32)?, u64_at(b, 40)?);
                if name.starts_with(b"__TEXT\0") {
                    text = Some((vmaddr, vmsize));
                } else if name.starts_with(b"__LINKEDIT\0") {
                    linkedit = Some((vmaddr, fileoff));
                }
                let mut seg = b.to_vec();
                seg[40..56].fill(0);
                for s in (72..size.saturating_sub(79)).step_by(80) {
                    seg[s + 48..s + 52].fill(0);
                }
                kept.extend(seg);
                count += 1;
            }
            LC_DYLD_EXPORTS_TRIE => trie = u32_at(b, 8).zip(u32_at(b, 12)),
            LC_DYLD_INFO | LC_DYLD_INFO_ONLY if trie.is_none() => trie = u32_at(b, 40).zip(u32_at(b, 44)),
            _ => {}
        }
        off += size;
    }
    let (text_at, text_size) = text?;
    let slide = base.wrapping_sub(text_at);
    let mut exports = Vec::new();
    if let (Some((seg_at, seg_off)), Some((at, n))) = (linkedit, trie) {
        exports = vec![0u8; (n as usize).min(MAX_TRIE)];
        let at = seg_at.wrapping_add(slide).wrapping_add(u64::from(at).wrapping_sub(seg_off));
        let got = rd(at, &mut exports);
        exports.truncate(got);
    }
    // A file of those segments and the trie alone, which nexo-bin reads as it reads any library.
    let sizeofcmds = kept.len() + 16;
    let mut file = Vec::with_capacity(32 + sizeofcmds + exports.len());
    file.extend_from_slice(&head[..16]);
    file.extend((count + 1).to_le_bytes());
    file.extend((sizeofcmds as u32).to_le_bytes());
    file.extend_from_slice(&head[24..32]);
    file.extend(kept);
    for w in [LC_DYLD_EXPORTS_TRIE, 16, (32 + sizeofcmds) as u32, exports.len() as u32] {
        file.extend(w.to_le_bytes());
    }
    file.extend(exports);
    let img = Image::from_bytes(file, PathBuf::from(path)).ok()?;
    Some((img, text_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: u64 = 0x7ff8_1000_0000;
    const LINKEDIT: u64 = 0x7ff8_4000_0000;
    const SLIDE: u64 = 0x0123_4000;

    fn segment(c: &mut Vec<u8>, name: &str, vm: (u64, u64), file: (u64, u64), sect: Option<(&str, u32)>) {
        let mut n = [0u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        c.extend(LC_SEGMENT_64.to_le_bytes());
        c.extend((72 + 80 * u32::from(sect.is_some())).to_le_bytes());
        c.extend(n);
        for v in [vm.0, vm.1, file.0, file.1] {
            c.extend(v.to_le_bytes());
        }
        for v in [5u32, 5, u32::from(sect.is_some()), 0] {
            c.extend(v.to_le_bytes());
        }
        if let Some((s, offset)) = sect {
            let mut sn = [0u8; 16];
            sn[..s.len()].copy_from_slice(s.as_bytes());
            c.extend(sn);
            c.extend(n);
            c.extend((vm.0 + 0x40).to_le_bytes());
            c.extend(0x100u64.to_le_bytes());
            for v in [offset, 4, 0, 0, 0x8000_0400, 0, 0, 0] {
                c.extend(v.to_le_bytes());
            }
        }
    }

    /// A library as the shared cache maps it: header and code at `TEXT + SLIDE`, the trie in `__LINKEDIT` at
    /// `LINKEDIT + SLIDE + 0x100` (file offsets count from the cache's start). It exports `_atoi` (+0x40),
    /// `_strlen` (+0x80) and `_puts` as a re-export.
    fn cached_library() -> Vec<(u64, Vec<u8>)> {
        let mut trie = vec![0u8, 3];
        trie.extend(b"_atoi\0");
        trie.push(25);
        trie.extend(b"_strlen\0");
        trie.push(29);
        trie.extend(b"_puts\0");
        trie.push(34);
        trie.extend([2, 0, 0x40, 0]); // 25: flags 0, offset 0x40, no children
        trie.extend([3, 0, 0x80, 1, 0]); // 29: flags 0, offset 0x80
        trie.extend([3, 8, 1, 0, 0]); // 34: a re-export from library 1, no address here
        let mut c = Vec::new();
        segment(&mut c, "__TEXT", (TEXT, 0x2000), (0x5000_0000, 0x2000), Some(("__text", 0x10)));
        segment(&mut c, "__LINKEDIT", (LINKEDIT, 0x1000), (0x9000_0000, 0x1000), None);
        c.extend(LC_DYLD_EXPORTS_TRIE.to_le_bytes());
        c.extend(16u32.to_le_bytes());
        c.extend(0x9000_0100u32.to_le_bytes());
        c.extend((trie.len() as u32).to_le_bytes());
        let mut head = Vec::new();
        for w in [MH_MAGIC_64, 0x0100_0007, 3, 6, 3, c.len() as u32, 0x8000_0000, 0] {
            head.extend(w.to_le_bytes());
        }
        head.extend(c);
        vec![(TEXT + SLIDE, head), (LINKEDIT + SLIDE + 0x100, trie)]
    }

    fn reader(mem: &[(u64, Vec<u8>)]) -> impl FnMut(u64, &mut [u8]) -> usize + '_ {
        move |at, buf| {
            let Some((start, b)) = mem.iter().find(|(s, b)| at >= *s && at - s < b.len() as u64) else { return 0 };
            let from = &b[(at - start) as usize..];
            let n = from.len().min(buf.len());
            buf[..n].copy_from_slice(&from[..n]);
            n
        }
    }

    #[test]
    fn a_cached_library_is_read_from_memory_with_its_exports_at_their_unslid_addresses() {
        let mem = cached_library();
        let (img, span) = read(&mut reader(&mem), TEXT + SLIDE, "/usr/lib/system/libsystem_c.dylib").unwrap();
        assert_eq!(span, 0x2000);
        assert_eq!(img.image_base, TEXT);
        let at = |n: &str| img.exports.iter().find(|e| e.name == n).map(|e| e.addr);
        assert_eq!((at("atoi"), at("strlen"), at("puts")), (Some(TEXT + 0x40), Some(TEXT + 0x80), None));
        assert_eq!(nexo_bin::nearest_symbol(&img, TEXT + 0x80), Some(("strlen".into(), 0)));
        // the section's offset counted in the cache file, so it names no bytes of the image built here
        assert!(img.sections.iter().all(|s| s.file_size == 0), "{:?}", img.sections);
    }

    #[test]
    fn a_library_whose_trie_cannot_be_read_still_gives_its_code_span() {
        let mut mem = cached_library();
        mem.pop();
        let (img, span) = read(&mut reader(&mem), TEXT + SLIDE, "/usr/lib/libz.1.dylib").unwrap();
        assert_eq!((span, img.exports.len()), (0x2000, 0));
    }

    #[test]
    fn memory_without_a_mach_o_header_gives_nothing() {
        let mem = vec![(0x1000u64, b"\x7fELF\x02\x01\x01\0".repeat(8))];
        assert!(read(&mut reader(&mem), 0x1000, "/lib/libc.so.6").is_none());
        assert!(read(&mut reader(&mem), 0x9000, "/lib/libc.so.6").is_none());
    }
}
