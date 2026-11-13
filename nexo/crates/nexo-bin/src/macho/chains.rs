//! Symbol sizes, imports from bind opcodes, chained fixups, classic indirect symbols and the export trie.

use super::*;

/// Symbol sizes: to the next symbol or listed function start in the same section, else the section end.
pub(super) fn sizes(img: &mut Image, starts: &[u64]) {
    let mut marks: Vec<u64> = img.symbols.iter().map(|s| s.addr).chain(starts.iter().copied()).collect();
    marks.sort_unstable();
    marks.dedup();
    let secs: Vec<(u64, u64)> = img.sections.iter().map(|s| (s.addr, s.addr + s.size)).collect();
    for s in img.symbols.iter_mut().filter(|s| s.size == 0) {
        let Some(&(_, end)) = secs.iter().find(|&&(a, e)| s.addr >= a && s.addr < e) else { continue };
        let next = marks[marks.partition_point(|&a| a <= s.addr)..].first().copied().filter(|&n| n < end);
        s.size = next.unwrap_or(end) - s.addr;
    }
}

/// The import named `raw`, added when the symbol table did not list it.
fn import_index(
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    raw: &[u8],
    lib: Option<String>,
) -> Option<usize> {
    let raw = String::from_utf8_lossy(&raw[..raw.len().min(MAX_NAME_IN)]).into_owned();
    if raw.is_empty() {
        return None;
    }
    if let Some(&k) = by_raw.get(&raw) {
        return Some(k);
    }
    if img.imports.len() >= MAX_IMPORTS {
        return None;
    }
    let name = shown(&raw);
    img.imports.push(Import { lib, raw: raw_of(&raw, &name), name, slot: None });
    by_raw.insert(raw, img.imports.len() - 1);
    Some(img.imports.len() - 1)
}

pub(super) fn chained(
    d: &[u8],
    (o, n): Range,
    segs: &[Seg],
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    lib_of: &dyn Fn(Option<u32>) -> Option<String>,
) {
    let fixups = match DyldChainedFixups::parse(LE, bytes_at(d, o, n)) {
        Ok(f) => f,
        Err(e) => {
            img.warnings.push(format!("the chained fixups at {o:#x} are not readable ({e}); imports have no slots"));
            return;
        }
    };
    let mut names: Vec<(&[u8], Option<u32>)> = Vec::new();
    if let Ok(mut it) = fixups.imports(LE) {
        while names.len() < MAX_IMPORTS {
            match it.next() {
                Ok(Some(i)) => names.push((i.name, i.dylib.index())),
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the chained fixups import table stops early: {e}"));
                    break;
                }
            }
        }
    }
    let Ok(mut it) = fixups.segments(LE) else {
        img.warnings.push("the chained fixups list no readable segment starts; imports have no slots".into());
        return;
    };
    let mut count = 0usize;
    for _ in 0..segs.len().max(1) {
        let seg = match it.next() {
            Ok(Some(s)) => s,
            Ok(None) => break,
            Err(e) => {
                img.warnings.push(format!("the chained fixups stop early: {e}"));
                break;
            }
        };
        let Some(g) = segs.get(seg.index() as usize) else { continue };
        let mut fx = seg.fixups(LE, img.image_base, bytes_at(d, g.fileoff, g.filesize));
        while count < MAX_FIXUPS {
            count += 1;
            match fx.next() {
                Ok(Some((at, Fixup::Bind(b)))) => {
                    let Some(&(raw, lib)) = names.get(b.ordinal as usize) else { continue };
                    if let Some(k) = import_index(img, by_raw, raw, lib_of(lib)) {
                        img.imports[k].slot.get_or_insert(g.vmaddr.wrapping_add(at));
                    }
                }
                Ok(Some((at, Fixup::Rebase(r)))) => {
                    img.rel_ptrs.push((g.vmaddr.wrapping_add(at), img.image_base.wrapping_add(r.target_offset)));
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the chained fixups of segment {} stop early: {e}", g.name));
                    break;
                }
            }
        }
    }
}

pub(super) fn classic(
    d: &[u8],
    at: usize,
    segs: &[Seg],
    img: &mut Image,
    by_raw: &mut HashMap<String, usize>,
    lib_of: &dyn Fn(Option<u32>) -> Option<String>,
) {
    let Ok((cmd, _)) = object::pod::from_bytes::<object::macho::DyldInfoCommand<LE>>(&d[at..]) else {
        img.warnings.push(format!("the dyld info command at {at:#x} is not readable; imports may lack slots"));
        return;
    };
    let mut count = 0usize;
    for which in 0..3 {
        let it = match which {
            0 => cmd.binds(LE, d, 8),
            1 => cmd.lazy_binds(LE, d, 8),
            _ => cmd.weak_binds(LE, d, 8),
        };
        let Ok(mut it) = it else { continue };
        while count < MAX_FIXUPS {
            count += 1;
            match it.next() {
                Ok(Some(b)) => {
                    let Some(g) = segs.get(usize::from(b.segment_index)) else { continue };
                    if let Some(k) = import_index(img, by_raw, b.symbol, lib_of(b.dylib.index())) {
                        img.imports[k].slot.get_or_insert(g.vmaddr.wrapping_add(b.segment_offset));
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    img.warnings.push(format!("the bind info stops early: {e}"));
                    break;
                }
            }
        }
    }
}

/// The exports trie: (name as written, address). Each node is read once, so a damaged trie whose
/// edges loop or share nodes ends; the work is bounded by the node count.
pub(super) fn trie(d: &[u8], base: u64) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut stack: Vec<(usize, Vec<u8>)> = vec![(0, Vec::new())];
    while let Some((node, prefix)) = stack.pop() {
        if out.len() >= MAX_SYMBOLS || seen.len() >= MAX_TRIE_NODES || !seen.insert(node) {
            continue;
        }
        let mut p = node;
        let Some(info) = uleb(d, &mut p).and_then(|n| usize::try_from(n).ok()) else { continue };
        let children = p.saturating_add(info);
        if info > 0 && !prefix.is_empty() {
            if let Some(flags) = uleb(d, &mut p) {
                // re-exports (0x08) point into another library; thread-local ones (kind 1) are no address
                let kind = flags & 0x3;
                if flags & 0x08 == 0 && kind != 1 {
                    if let Some(v) = uleb(d, &mut p) {
                        let addr = if kind == 2 { v } else { base.wrapping_add(v) };
                        out.push((String::from_utf8_lossy(&prefix).into_owned(), addr));
                    }
                }
            }
        }
        let mut p = children;
        let Some(&n) = d.get(p) else { continue };
        p += 1;
        for _ in 0..n {
            let len = d.get(p..).map_or(0, |r| r.iter().position(|&c| c == 0).unwrap_or(r.len()));
            let edge = d.get(p..p + len).unwrap_or(&[]);
            p = p.saturating_add(len + 1);
            let Some(child) = uleb(d, &mut p).and_then(|c| usize::try_from(c).ok()) else { break };
            if prefix.len() + edge.len() <= MAX_NAME_IN {
                let mut name = prefix.clone();
                name.extend_from_slice(edge);
                stack.push((child, name));
            }
        }
    }
    out
}
