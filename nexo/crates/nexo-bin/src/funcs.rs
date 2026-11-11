//! Function starts: symbols, the entry, exports, call targets, then PE exception data or ELF
//! `.eh_frame`. A function ends where the next one starts or its section ends.

use std::collections::{BTreeSet, HashMap};

use iced_x86::{FlowControl, Mnemonic, OpKind};

use crate::disasm::{target, walk, walk_code};
use crate::image::Section;
use crate::refs::Collector;
use crate::{Format, Image};

#[derive(Debug, Clone)]
pub struct Func {
    pub start: u64,
    /// One past the last byte.
    pub end: u64,
    pub name: String,
}

impl Func {
    pub fn size(&self) -> u64 {
        self.end - self.start
    }
}

pub(crate) fn find(img: &Image) -> Vec<Func> {
    find_with(img, None)
}

/// Like [`find`]; when a collector is given, the same decode pass hands it every instruction, so the
/// references need no second pass.
pub(crate) fn find_with(img: &Image, mut collector: Option<&mut Collector>) -> Vec<Func> {
    let in_code = |a: u64| img.section_at(a).is_some_and(|s| s.exec && a - s.addr < s.backed_len());
    let mut starts: BTreeSet<u64> = BTreeSet::new();
    for s in img.symbols.iter().filter(|s| s.func) {
        if in_code(s.addr) {
            starts.insert(s.addr);
        }
    }
    for e in &img.exports {
        if in_code(e.addr) {
            starts.insert(e.addr);
        }
    }
    if in_code(img.entry) {
        starts.insert(img.entry);
    }
    for sec in img.code_sections() {
        walk_code(img, sec, |ins, _| {
            if let Some(c) = collector.as_deref_mut() {
                c.instruction(img, ins);
            }
            if ins.flow_control() == FlowControl::Call {
                if let Some((t, false)) = target(ins) {
                    if in_code(t) {
                        starts.insert(t);
                    }
                }
            }
        });
    }
    match img.format {
        Format::Pe => pdata(img, &mut starts),
        Format::Elf => {
            for sec in img.sections.iter().filter(|s| s.alloc && s.name == ".eh_frame" && s.file_size > 0) {
                eh_frame(img, sec, &mut starts);
            }
        }
    }
    starts.retain(|&a| in_code(a));
    let all: Vec<u64> = starts.into_iter().collect();
    let mut out = Vec::with_capacity(all.len());
    for (i, &start) in all.iter().enumerate() {
        let Some(sec) = img.section_at(start) else { continue };
        let sec_end = sec.addr + sec.backed_len();
        let end = all.get(i + 1).copied().filter(|&n| n < sec_end).unwrap_or(sec_end);
        let name = img
            .names
            .get(&start)
            .cloned()
            .or_else(|| stub_name(img, start, end))
            .unwrap_or_else(|| format!("sub_{start:x}"));
        out.push(Func { start, end, name });
    }
    out
}

/// A jump through an import slot in the first instructions: the stub the loader's import goes through.
fn stub_name(img: &Image, start: u64, end: u64) -> Option<String> {
    let bytes = img.read_at(start, ((end - start) as usize).min(24))?;
    let mut seen = 0;
    let mut found = None;
    walk(bytes, start, |ins, _| {
        seen += 1;
        if found.is_some() || seen > 3 {
            return;
        }
        if ins.mnemonic() == Mnemonic::Jmp && ins.op0_kind() == OpKind::Memory && ins.is_ip_rel_memory_operand() {
            found = img.slots.get(&ins.ip_rel_memory_address()).map(|&i| img.imports[i].name.clone());
        }
    });
    found.map(|n| if img.format == Format::Elf { format!("{n}@plt") } else { n })
}

/// PE: RUNTIME_FUNCTION entries (begin, end, unwind info), each 12 bytes. Chained entries are pieces
/// of a function that already has an entry, so they are left out.
fn pdata(img: &Image, starts: &mut BTreeSet<u64>) {
    let Some((rva, size)) = img.exception_dir else { return };
    let Some(table) = img.read_at(img.image_base.wrapping_add(rva), size.min(1 << 28) as usize) else { return };
    for e in table.as_chunks::<12>().0.iter() {
        let begin = u64::from(u32::from_le_bytes([e[0], e[1], e[2], e[3]]));
        let end = u64::from(u32::from_le_bytes([e[4], e[5], e[6], e[7]]));
        let unwind = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
        if begin == 0 || end <= begin {
            continue;
        }
        if unwind & 1 == 0 {
            let flags = img
                .read_at(img.image_base.wrapping_add(u64::from(unwind)), 1)
                .and_then(|b| b.first().copied())
                .map_or(0, |b| b >> 3);
            if flags & 4 != 0 {
                continue;
            }
        }
        starts.insert(img.image_base.wrapping_add(begin));
    }
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at.checked_add(4)?).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn uleb(d: &[u8], p: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let b = *d.get(*p)?;
        *p += 1;
        if shift < 64 {
            v |= u64::from(b & 0x7f) << shift;
        }
        shift += 7;
        if b & 0x80 == 0 {
            return Some(v);
        }
        if shift > 70 {
            return None;
        }
    }
}

fn sleb(d: &[u8], p: &mut usize) -> Option<i64> {
    let (mut v, mut shift) = (0i64, 0u32);
    loop {
        let b = *d.get(*p)?;
        *p += 1;
        if shift < 64 {
            v |= i64::from(b & 0x7f) << shift;
        }
        shift += 7;
        if b & 0x80 == 0 {
            if shift < 64 && b & 0x40 != 0 {
                v |= -1i64 << shift;
            }
            return Some(v);
        }
        if shift > 70 {
            return None;
        }
    }
}

/// A DW_EH_PE value: the format in the low nibble, how to apply it in the high one.
/// `field_addr` is the address of the first byte read (for pc-relative values).
fn encoded(d: &[u8], p: &mut usize, enc: u8, field_addr: u64) -> Option<u64> {
    let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = d.get(*p..p.checked_add(n)?)?;
        *p += n;
        Some(s)
    };
    let v: u64 = match enc & 0x0f {
        0x00 | 0x04 | 0x0c => u64::from_le_bytes(take(p, 8)?.try_into().ok()?),
        0x01 => uleb(d, p)?,
        0x02 => u64::from(u16::from_le_bytes(take(p, 2)?.try_into().ok()?)),
        0x03 => u64::from(u32::from_le_bytes(take(p, 4)?.try_into().ok()?)),
        0x09 => sleb(d, p)? as u64,
        0x0a => i64::from(i16::from_le_bytes(take(p, 2)?.try_into().ok()?)) as u64,
        0x0b => i64::from(i32::from_le_bytes(take(p, 4)?.try_into().ok()?)) as u64,
        _ => return None,
    };
    match enc & 0x70 {
        0x00 => Some(v),
        0x10 => Some(field_addr.wrapping_add(v)),
        _ => None,
    }
}

/// The pointer encoding FDEs use, from a CIE body (after its id).
fn cie_encoding(c: &[u8]) -> Option<u8> {
    let mut p = 0usize;
    let version = *c.get(p)?;
    p += 1;
    let aug_start = p;
    while *c.get(p)? != 0 {
        p += 1;
    }
    let aug = &c[aug_start..p];
    p += 1;
    if aug.starts_with(b"eh") {
        p += 8;
    }
    uleb(c, &mut p)?;
    sleb(c, &mut p)?;
    if version == 1 {
        p += 1;
    } else {
        uleb(c, &mut p)?;
    }
    let mut enc = 0u8;
    if aug.first() == Some(&b'z') {
        let _len = uleb(c, &mut p)?;
        for &ch in &aug[1..] {
            match ch {
                b'R' => {
                    enc = *c.get(p)?;
                    p += 1;
                }
                b'L' => p += 1,
                b'P' => {
                    let penc = *c.get(p)?;
                    p += 1;
                    encoded(c, &mut p, penc & 0x0f, 0)?;
                }
                b'S' | b'B' => {}
                _ => break,
            }
        }
    }
    Some(enc)
}

fn eh_frame(img: &Image, sec: &Section, starts: &mut BTreeSet<u64>) {
    let d = img.section_bytes(sec);
    let mut cies: HashMap<usize, Option<u8>> = HashMap::new();
    let mut pos = 0usize;
    while pos + 8 <= d.len() {
        let Some(len32) = u32le(d, pos) else { break };
        if len32 == 0 {
            break;
        }
        let (hdr, len) = if len32 == 0xffff_ffff {
            match d.get(pos + 4..pos + 12).map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            {
                Some(l) => (12usize, l),
                None => break,
            }
        } else {
            (4usize, u64::from(len32))
        };
        let body = pos + hdr;
        let Some(end) = usize::try_from(len).ok().and_then(|l| body.checked_add(l)).filter(|&e| e <= d.len()) else {
            break;
        };
        let Some(id) = u32le(d, body) else { break };
        if id == 0 {
            cies.insert(pos, d.get(body + 4..end).and_then(cie_encoding));
        } else if let Some(cie_pos) = body.checked_sub(id as usize) {
            let enc = *cies.entry(cie_pos).or_insert_with(|| {
                // a CIE that comes later in the section: parse it where it is
                let l = u32le(d, cie_pos)? as usize;
                let b = cie_pos.checked_add(4)?;
                d.get(b + 4..b.checked_add(l)?).and_then(cie_encoding)
            });
            if let Some(enc) = enc {
                let mut p = body + 4;
                let field = sec.addr + p as u64;
                if let Some(a) = encoded(d, &mut p, enc, field) {
                    starts.insert(a);
                }
            }
        }
        pos = end;
    }
}

/// The references of every executable section, when the functions were found without a collector.
pub(crate) fn decode_refs(img: &Image, c: &mut Collector) {
    for sec in img.code_sections() {
        walk_code(img, sec, |ins, _| c.instruction(img, ins));
    }
}
