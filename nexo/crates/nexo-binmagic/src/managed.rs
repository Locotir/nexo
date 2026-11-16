//! Whether the first bytes are managed code (.NET, JVM) that `nexo bin`'s `managed` op reads whole.

/// How many leading bytes `is_managed` and `is_mixed_mode` need: the CLI header sits at the start of the
/// first section, usually well inside this.
pub const HEAD: usize = 64 * 1024;

/// A class file and a Mach-O universal binary share the CAFEBABE magic: the next four bytes are the
/// class version (45 or more) or the architecture count (a handful).
pub fn is_class(d: &[u8]) -> bool {
    d.starts_with(&[0xCA, 0xFE, 0xBA, 0xBE])
        && d.get(4..8).is_some_and(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]) >= 45)
}

pub fn looks_like_zip(d: &[u8]) -> bool {
    d.starts_with(b"PK\x03\x04") || d.starts_with(b"PK\x05\x06")
}

/// The part of a PE file's headers that says where its CLI header (the .NET one) is.
pub struct PeCli {
    /// `(virtual address, virtual size, raw offset, raw size)`.
    pub sections: Vec<(u32, u32, u32, u32)>,
    /// The CLI header directory: `(rva, size)`, zero when there is none.
    pub clr: (u32, u32),
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// Reads the PE headers in `d` for the section table and the CLI header directory.
pub fn pe_cli(d: &[u8]) -> Result<PeCli, String> {
    if d.get(..2) != Some(b"MZ") {
        return Err("no MZ header".into());
    }
    let pe = rd32(d, 0x3c).ok_or("cut short")? as usize;
    if d.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("no PE signature".into());
    }
    let nsec = rd16(d, pe + 6).ok_or("cut short")? as usize;
    let optsz = rd16(d, pe + 20).ok_or("cut short")? as usize;
    let opt = pe + 24;
    let base = match rd16(d, opt) {
        Some(0x10b) => 96,
        Some(0x20b) => 112,
        _ => return Err("unknown optional header".into()),
    };
    let ndirs = rd32(d, opt + base - 4).unwrap_or(0) as usize;
    let clr = if ndirs > 14 {
        (rd32(d, opt + base + 14 * 8).ok_or("cut short")?, rd32(d, opt + base + 14 * 8 + 4).ok_or("cut short")?)
    } else {
        (0, 0)
    };
    let mut sections = Vec::new();
    for i in 0..nsec {
        let s = opt + optsz + i * 40;
        // Header-only prefixes (detection reads 4 KB) may not hold every section: take what is there.
        let (Some(vs), Some(va), Some(rs), Some(ro)) =
            (rd32(d, s + 8), rd32(d, s + 12), rd32(d, s + 16), rd32(d, s + 20))
        else {
            break;
        };
        sections.push((va, vs, ro, rs));
    }
    Ok(PeCli { sections, clr })
}

/// The file offset of `rva`: only bytes the file really holds, since past the raw size an RVA is zero fill,
/// not the next section.
pub fn section_offset(sections: &[(u32, u32, u32, u32)], rva: u32) -> Option<usize> {
    let s = sections.iter().find(|(va, _, _, rs)| rva.checked_sub(*va).is_some_and(|x| x < *rs))?;
    Some((rva - s.0).checked_add(s.2)? as usize)
}

/// `Some(true)` for a CLI header with COMIMAGE_FLAGS_ILONLY, `Some(false)` without it (mixed-mode C++/CLI
/// or native code inside), `None` when there is no CLI header or it is not inside `head`.
fn il_only(head: &[u8]) -> Option<bool> {
    let pe = pe_cli(head).ok()?;
    if pe.clr.0 == 0 {
        return None;
    }
    let cli = section_offset(&pe.sections, pe.clr.0)?;
    Some(rd32(head, cli.checked_add(16)?)? & 1 != 0)
}

/// True when the PE headers in `head` say this is an IL-only .NET assembly: what `managed` reads whole.
pub fn is_dotnet(head: &[u8]) -> bool {
    il_only(head) == Some(true)
}

/// True for a .NET assembly that also holds native code (C++/CLI): the native ops read it, and `managed`
/// reads its metadata and IL.
pub fn is_mixed_mode(head: &[u8]) -> bool {
    il_only(head) == Some(false)
}

/// True when the first bytes say the file is managed bytecode `managed` reads whole. A .NET assembly
/// counts only when its CLI header (inside `head`) has the IL-only flag.
pub fn is_managed(head: &[u8]) -> bool {
    is_class(head) || looks_like_zip(head) || is_dotnet(head)
}
