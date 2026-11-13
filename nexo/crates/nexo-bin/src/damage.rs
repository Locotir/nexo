//! Telling a cut or damaged file from a good one, in words an agent can act on: what the headers promise,
//! where the file ends, and the offset of the first promise it does not keep.

fn u16_at(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?) as u64)
}
fn u32_at(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?) as u64)
}
fn u64_at(b: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// What a file that starts like an ELF or a PE but is too short to be one says about itself.
pub(crate) fn short_magic(data: &[u8]) -> Option<String> {
    let len = data.len();
    if data.starts_with(b"\x7fELF") && len < 64 {
        return Some(format!("starts like an ELF file but is only {len} bytes; an ELF64 header alone is 64 bytes"));
    }
    if data.starts_with(b"MZ") && len < 64 {
        return Some(format!("starts like a Windows (MZ) file but is only {len} bytes; its header alone is 64 bytes"));
    }
    if data.starts_with(b"MZ") {
        if let Some(nt) = u32_at(data, 0x3c) {
            if nt.saturating_add(24) > len as u64 {
                return Some(format!(
                    "is a Windows (MZ) file cut at {len} bytes: the PE header promised at offset 0x3c (at {nt:#x}) reaches byte {:#x}",
                    nt.saturating_add(24)
                ));
            }
        }
    }
    if let Some(w) = crate::damage_macho::short(data) {
        return Some(w);
    }
    if data.starts_with(b"\x7fEL") || (data.starts_with(b"\x7fE") && len < 4) || (data.starts_with(b"M") && len < 2) {
        return Some(format!("is {len} bytes and may be the start of an ELF or PE file that was cut"));
    }
    None
}

/// The first place where a header promises bytes the file does not have: "the file is cut short or damaged:
/// it is N bytes, but the section header table (offset 0x.., 12 entries) ends at byte 0x..".
pub(crate) fn extent_problem(data: &[u8]) -> Option<String> {
    let len = data.len() as u64;
    let what = if data.starts_with(b"\x7fELF") && data.get(4) == Some(&2) {
        elf_reach(data)
    } else if crate::damage_macho::is_macho(data) {
        crate::damage_macho::reach(data)
    } else {
        pe_reach(data)
    };
    let (need, at, why) = what?;
    (need > len).then(|| {
        format!("the file is cut short or damaged: it is {len} bytes, but {why} (promised at offset {at:#x}) reaches byte {need:#x}")
    })
}

/// (the furthest byte a header promises, offset of the promise, what it is about)
fn elf_reach(b: &[u8]) -> Option<(u64, u64, String)> {
    let mut worst: Option<(u64, u64, String)> = None;
    let mut see = |need: Option<u64>, at: u64, why: String| {
        if let Some(n) = need {
            if worst.as_ref().is_none_or(|w| n > w.0) {
                worst = Some((n, at, why));
            }
        }
    };
    let (phoff, phentsize, phnum) = (u64_at(b, 0x20)?, u16_at(b, 0x36)?, u16_at(b, 0x38)?);
    let (shoff, shentsize, shnum) = (u64_at(b, 0x28)?, u16_at(b, 0x3a)?, u16_at(b, 0x3c)?);
    if phnum > 0 {
        see(
            phoff.checked_add(phentsize.saturating_mul(phnum)),
            0x20,
            format!("the program header table ({phnum} entries of {phentsize} bytes at {phoff:#x})"),
        );
    }
    if shnum > 0 {
        see(
            shoff.checked_add(shentsize.saturating_mul(shnum)),
            0x28,
            format!("the section header table ({shnum} entries of {shentsize} bytes at {shoff:#x})"),
        );
    }
    for i in 0..phnum.min(4096) {
        let h = phoff.saturating_add(i.saturating_mul(phentsize));
        if let (Some(kind), Some(off), Some(sz)) =
            (u32_at(b, h), u64_at(b, h.saturating_add(8)), u64_at(b, h.saturating_add(32)))
        {
            // PT_LOAD and PT_NOTE carry file bytes
            if (kind == 1 || kind == 4) && sz > 0 {
                see(off.checked_add(sz), h, format!("program header #{i} ({sz:#x} bytes at {off:#x})"));
            }
        }
    }
    for i in 0..shnum.min(4096) {
        let h = shoff.saturating_add(i.saturating_mul(shentsize));
        if let (Some(kind), Some(off), Some(sz)) =
            (u32_at(b, h.saturating_add(4)), u64_at(b, h.saturating_add(0x18)), u64_at(b, h.saturating_add(0x20)))
        {
            // NOBITS (8) and the null section take no file bytes
            if kind != 0 && kind != 8 && sz > 0 {
                see(off.checked_add(sz), h, format!("section #{i} ({sz:#x} bytes at {off:#x})"));
            }
        }
    }
    worst
}

fn pe_reach(b: &[u8]) -> Option<(u64, u64, String)> {
    if !b.starts_with(b"MZ") {
        return None;
    }
    let nt = u32_at(b, 0x3c)?;
    if b.get(nt as usize..(nt as usize).checked_add(4)?) != Some(b"PE\0\0") {
        return None;
    }
    let (count, opt) = (u16_at(b, nt + 6)?, u16_at(b, nt + 20)?);
    let table = nt.checked_add(24)?.checked_add(opt)?;
    let mut worst: Option<(u64, u64, String)> = None;
    let mut see = |need: Option<u64>, at: u64, why: String| {
        if let Some(n) = need {
            if worst.as_ref().is_none_or(|w| n > w.0) {
                worst = Some((n, at, why));
            }
        }
    };
    see(
        table.checked_add(count * 40),
        nt + 6,
        format!("the section table ({count} entries after the optional header)"),
    );
    for i in 0..count.min(4096) {
        let h = table + i * 40;
        if let (Some(raw), Some(ptr)) = (u32_at(b, h + 16), u32_at(b, h + 20)) {
            if raw > 0 && ptr > 0 {
                see(ptr.checked_add(raw), h, format!("section #{} ({raw:#x} bytes at {ptr:#x})", i + 1));
            }
        }
    }
    worst
}
