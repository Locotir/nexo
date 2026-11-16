//! A module as it lies in memory, turned back into a file `nexo bin` opens: the loader put every segment at its
//! address, so in the rebuilt file each segment's bytes start at the offset its address has from the image start.
//! Whatever the program changed at run time (decrypted code, unpacked sections, filled tables) is in the file.

/// A rebuilt image and what was done to it, in words for the answer.
pub struct Rebuilt {
    pub bytes: Vec<u8>,
    pub notes: Vec<String>,
}

/// The format of the image whose first bytes are `head`, by its magic number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Elf,
    Pe,
    Other,
}

pub fn kind(head: &[u8]) -> Kind {
    if head.starts_with(b"\x7fELF") {
        Kind::Elf
    } else if head.starts_with(b"MZ") {
        Kind::Pe
    } else {
        Kind::Other
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?) as u64)
}

fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?) as u64)
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// An address-sized field: 8 bytes in a 64-bit file, 4 in a 32-bit one.
fn word_at(b: &[u8], at: usize, wide: bool) -> Option<u64> {
    if wide {
        u64_at(b, at)
    } else {
        u32_at(b, at)
    }
}

fn put(b: &mut [u8], at: usize, size: usize, v: u64) {
    if let Some(d) = b.get_mut(at..at + size) {
        d.copy_from_slice(&v.to_le_bytes()[..size]);
    }
}

/// Field offsets of the ELF header, a program header and a section header, for one class.
struct ElfLayout {
    wide: bool,
    phoff: usize,
    shoff: usize,
    phentsize: usize,
    shnum: usize,
    p_offset: usize,
    p_vaddr: usize,
    p_filesz: usize,
    p_memsz: usize,
    sh_flags: usize,
    sh_addr: usize,
    sh_offset: usize,
    sh_size: usize,
}

const ELF64: ElfLayout = ElfLayout {
    wide: true,
    phoff: 0x20,
    shoff: 0x28,
    phentsize: 0x36,
    shnum: 0x3c,
    p_offset: 0x08,
    p_vaddr: 0x10,
    p_filesz: 0x20,
    p_memsz: 0x28,
    sh_flags: 0x08,
    sh_addr: 0x10,
    sh_offset: 0x18,
    sh_size: 0x20,
};

const ELF32: ElfLayout = ElfLayout {
    wide: false,
    phoff: 0x1c,
    shoff: 0x20,
    phentsize: 0x2a,
    shnum: 0x30,
    p_offset: 0x04,
    p_vaddr: 0x08,
    p_filesz: 0x10,
    p_memsz: 0x14,
    sh_flags: 0x08,
    sh_addr: 0x0c,
    sh_offset: 0x10,
    sh_size: 0x14,
};

const PT_LOAD: u64 = 1;
const PT_DYNAMIC: u64 = 2;
const SHT_NOBITS: u64 = 8;
const SHT_PROGBITS: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_TLS: u64 = 0x400;

/// One program header: (type, offset, vaddr, filesz, memsz).
type Phdr = (u64, u64, u64, u64, u64);

/// The class layout and program headers of a little-endian ELF whose first bytes are `b`.
fn elf_phdrs(b: &[u8]) -> Option<(&'static ElfLayout, usize, Vec<Phdr>)> {
    if !b.starts_with(b"\x7fELF") || b.get(5) != Some(&1) {
        return None;
    }
    let l = match b.get(4) {
        Some(2) => &ELF64,
        Some(1) => &ELF32,
        _ => return None,
    };
    let phoff = word_at(b, l.phoff, l.wide)? as usize;
    let size = u16_at(b, l.phentsize)? as usize;
    let num = u16_at(b, l.phentsize + 2)? as usize;
    let mut out = Vec::with_capacity(num);
    for i in 0..num {
        let at = phoff.checked_add(i.checked_mul(size)?)?;
        out.push((
            u32_at(b, at)?,
            word_at(b, at + l.p_offset, l.wide)?,
            word_at(b, at + l.p_vaddr, l.wide)?,
            word_at(b, at + l.p_filesz, l.wide)?,
            word_at(b, at + l.p_memsz, l.wide)?,
        ));
    }
    Some((l, phoff, out))
}

/// The address the start of an ELF file is loaded at in its own numbering (its file addresses), and how many bytes
/// from there its segments reach in memory. `head` is the start of the image (its headers).
pub fn elf_span(head: &[u8]) -> Option<(u64, u64)> {
    let (_, _, ph) = elf_phdrs(head)?;
    let loads: Vec<&Phdr> = ph.iter().filter(|p| p.0 == PT_LOAD).collect();
    let start = loads.iter().map(|p| p.2.wrapping_sub(p.1)).min()? & !0xfff;
    let end = loads.iter().map(|p| p.2.saturating_add(p.4)).max()?;
    Some((start, end.checked_sub(start)?))
}

/// The ELF image `mem` (its bytes from the address its file starts at) as a file. `disk` is the file the module was
/// loaded from, used only when its program headers are the ones in memory: its section headers, symbol table and
/// debug sections are carried over, and its dynamic table replaces the one the loader moved to memory addresses.
pub fn elf(mem: &[u8], disk: Option<&[u8]>, from: &str) -> Result<Rebuilt, String> {
    let (l, phoff, ph) = elf_phdrs(mem).ok_or("the program headers in memory cannot be read")?;
    let (start, _) = elf_span(mem).ok_or("the image has no loaded segment")?;
    let size = u16_at(mem, l.phentsize).unwrap_or(0) as usize;
    let table = mem.get(phoff..phoff + size * ph.len()).ok_or("the program headers lie outside the image")?.to_vec();
    let mut out = mem.to_vec();
    let mut notes = Vec::new();
    let len = mem.len() as u64;
    let (wsize, wide) = if l.wide { (8, true) } else { (4, false) };
    for (i, p) in ph.iter().enumerate() {
        let at = phoff + i * size;
        let (kind, _, vaddr, filesz, memsz) = *p;
        let Some(off) = vaddr.checked_sub(start).filter(|o| *o < len) else { continue };
        if kind == PT_LOAD {
            // the whole segment is in the file now, its zero-filled tail (.bss) with what the program put there
            put(&mut out, at + l.p_offset, wsize, off);
            put(&mut out, at + l.p_filesz, wsize, memsz.min(len - off));
        } else if memsz > 0 {
            put(&mut out, at + l.p_offset, wsize, off);
            put(&mut out, at + l.p_filesz, wsize, filesz.min(len - off));
        }
    }
    let same = disk.filter(|d| {
        elf_phdrs(d).is_some_and(|(dl, dphoff, dph)| {
            dl.wide == wide && dph.len() == ph.len() && d.get(dphoff..dphoff + table.len()) == Some(&table[..])
        })
    });
    let Some(d) = same else {
        put(&mut out, l.shoff, wsize, 0);
        put(&mut out, l.shnum, 2, 0);
        put(&mut out, l.shnum + 2, 2, 0);
        notes.push(match disk {
            Some(_) => format!("{from} is not the file this module was loaded from (its program headers differ): the file has no section headers, its segments stand in for sections"),
            None => format!("{from} cannot be read: the file has no section headers, its segments stand in for sections"),
        });
        return Ok(Rebuilt { bytes: out, notes });
    };
    // the loader adds the load shift to the dynamic table's addresses in place; the file's own table has the
    // addresses every other part of the file uses
    // (a table that lies before the image or past its end is left as the loader wrote it: the kernel never reads it)
    if let Some(&(_, doff, vaddr, filesz, _)) = ph.iter().find(|p| p.0 == PT_DYNAMIC) {
        let span = |a: u64| Some(usize::try_from(a).ok()?..usize::try_from(a.checked_add(filesz)?).ok()?);
        let dst = vaddr.checked_sub(start).and_then(span).filter(|r| r.end <= out.len());
        if let (Some(src), Some(dst)) = (span(doff).and_then(|r| d.get(r)), dst) {
            out[dst].copy_from_slice(src);
        }
    }
    let shoff = word_at(d, l.shoff, wide).and_then(|o| usize::try_from(o).ok()).unwrap_or(0);
    // a section header is 0x40 bytes in a 64-bit file and 0x28 in a 32-bit one; a smaller entry size (0 in a
    // stripped or packed file whose loader never reads the table) holds no section
    let shsize = u16_at(d, l.phentsize + 4).unwrap_or(0) as usize;
    let mut shnum = u16_at(d, l.shnum).unwrap_or(0) as usize;
    if shnum == 0 && shoff != 0 {
        // more sections than the header field holds: the count is the first section header's size
        shnum = shoff.checked_add(l.sh_size).and_then(|a| word_at(d, a, wide)).map_or(0, |n| n as usize);
    }
    let table = shsize.checked_mul(shnum).and_then(|n| shoff.checked_add(n)).and_then(|end| d.get(shoff..end));
    let whole = shsize >= if wide { 0x40 } else { 0x28 };
    let Some(mut shdrs) = table.filter(|_| shoff != 0 && shnum > 0 && whole).map(<[u8]>::to_vec) else {
        put(&mut out, l.shoff, wsize, 0);
        put(&mut out, l.shnum, 2, 0);
        put(&mut out, l.shnum + 2, 2, 0);
        notes.push(format!("{from} has no section headers either: its segments stand in for sections"));
        return Ok(Rebuilt { bytes: out, notes });
    };
    let mut filled = 0;
    // the sections of a file share its bytes, so together they hold no more than it does; headers that point many
    // times at the same bytes would otherwise grow the rebuilt file past any size
    let (mut carried, mut left_out) = (0usize, 0usize);
    for i in 1..shnum {
        let at = i * shsize;
        let typ = u32_at(&shdrs, at + 4).unwrap_or(0);
        let flags = word_at(&shdrs, at + l.sh_flags, wide).unwrap_or(0);
        let addr = word_at(&shdrs, at + l.sh_addr, wide).unwrap_or(0);
        let doff = word_at(&shdrs, at + l.sh_offset, wide).unwrap_or(0);
        let size = word_at(&shdrs, at + l.sh_size, wide).unwrap_or(0);
        if flags & SHF_ALLOC != 0 {
            let Some(off) = addr.checked_sub(start).filter(|o| o.saturating_add(size) <= len) else { continue };
            put(&mut shdrs, at + l.sh_offset, wsize, off);
            // .bss holds what the program wrote there, so it is read as data like the rest
            if typ == SHT_NOBITS && flags & SHF_TLS == 0 && size > 0 {
                put(&mut shdrs, at + 4, 4, SHT_PROGBITS);
                filled += 1;
            }
        } else if typ != SHT_NOBITS && size > 0 {
            // symbols, names and debug sections are not loaded: they come from the file on disk
            let Some(src) = d.get(doff as usize..doff.saturating_add(size) as usize) else { continue };
            if carried + src.len() > d.len() {
                // its header stays, as a section with no bytes in the file
                put(&mut shdrs, at + 4, 4, SHT_NOBITS);
                left_out += 1;
                continue;
            }
            carried += src.len();
            out.resize(out.len().next_multiple_of(8), 0);
            let to = out.len() as u64;
            put(&mut shdrs, at + l.sh_offset, wsize, to);
            out.extend_from_slice(src);
        }
    }
    out.resize(out.len().next_multiple_of(8), 0);
    let new_shoff = out.len() as u64;
    out.extend_from_slice(&shdrs);
    put(&mut out, l.shoff, wsize, new_shoff);
    for (field, size) in [(l.phentsize + 4, 2), (l.shnum, 2), (l.shnum + 2, 2)] {
        let v = u16_at(d, field).unwrap_or(0);
        put(&mut out, field, size, v);
    }
    let mut note = format!(
        "section headers, symbols and the dynamic table are from {from} (its program headers are the ones in memory)"
    );
    if filled > 0 {
        note.push_str(&format!("; {filled} zero-filled section(s) such as .bss hold their run-time bytes"));
    }
    if left_out > 0 {
        note.push_str(&format!(
            "; {left_out} section(s) have no bytes in it: with them the sections would hold more bytes than {from} has"
        ));
    }
    notes.push(note);
    Ok(Rebuilt { bytes: out, notes })
}

/// Offsets of a PE file's headers: (COFF header, optional header, whether it is PE32+).
fn pe_heads(b: &[u8]) -> Option<(usize, usize, bool)> {
    let pe = u32_at(b, 0x3c)? as usize;
    if b.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let opt = pe + 24;
    let plus = match u16_at(b, opt)? {
        0x20b => true,
        0x10b => false,
        _ => return None,
    };
    Some((pe + 4, opt, plus))
}

/// The size of a PE image in memory (`SizeOfImage`), from its headers.
pub fn pe_span(head: &[u8]) -> Option<u64> {
    let (_, opt, _) = pe_heads(head)?;
    u32_at(head, opt + 56)
}

/// The PE image `mem` (its bytes from its load address `base`) as a file whose sections lie at their own addresses
/// and whose image base is `base`, so `nexo bin` shows the addresses the program uses. `disk` is the file it was
/// loaded from: its COFF symbol table is carried over when its time stamp and image size are the ones in memory.
pub fn pe(mem: &[u8], base: u64, disk: Option<&[u8]>, from: &str) -> Result<Rebuilt, String> {
    let (coff, opt, plus) = pe_heads(mem).ok_or("the PE headers in memory cannot be read")?;
    let mut out = mem.to_vec();
    let mut notes = Vec::new();
    let len = mem.len() as u64;
    let count = u16_at(mem, coff + 2).unwrap_or(0) as usize;
    let opt_size = u16_at(mem, coff + 16).unwrap_or(0) as usize;
    let align = u32_at(mem, opt + 32).filter(|a| *a > 0).unwrap_or(0x1000);
    for i in 0..count {
        let at = opt + opt_size + i * 40;
        let (Some(vsize), Some(va), Some(raw)) = (u32_at(mem, at + 8), u32_at(mem, at + 12), u32_at(mem, at + 16))
        else {
            return Err(format!("section header {i} lies outside the image"));
        };
        let size = if vsize == 0 { raw } else { vsize };
        let (off, size) = if va < len { (va, size.next_multiple_of(align).min(len - va)) } else { (0, 0) };
        put(&mut out, at + 20, 4, off);
        put(&mut out, at + 16, 4, size);
    }
    put(&mut out, opt + 36, 4, align);
    if plus {
        put(&mut out, opt + 24, 8, base);
    } else if let Ok(b) = u32::try_from(base) {
        put(&mut out, opt + 28, 4, b as u64);
    }
    // the certificate table is the one directory given as a file offset, and the signature is not in memory
    let (dirs, ndirs) = if plus { (opt + 112, opt + 108) } else { (opt + 96, opt + 92) };
    if u32_at(mem, ndirs).unwrap_or(0) > 4 {
        put(&mut out, dirs + 4 * 8, 8, 0);
    }
    let stamp = |b: &[u8], c: usize, o: usize| (u32_at(b, c + 4), u32_at(b, o + 56));
    let syms = disk.and_then(|d| {
        let (dcoff, dopt, _) = pe_heads(d)?;
        if stamp(d, dcoff, dopt) != stamp(mem, coff, opt) {
            return None;
        }
        let ptr = u32_at(d, dcoff + 8).filter(|p| *p != 0)? as usize;
        let n = u32_at(d, dcoff + 12)? as usize;
        let strings = ptr.checked_add(n.checked_mul(18)?)?;
        let total = strings.checked_add(u32_at(d, strings)? as usize)?;
        Some((d.get(ptr..total)?, n))
    });
    match syms {
        Some((table, n)) => {
            out.resize(out.len().next_multiple_of(8), 0);
            let at = out.len() as u64;
            put(&mut out, coff + 8, 4, at);
            put(&mut out, coff + 12, 4, n as u64);
            out.extend_from_slice(table);
            notes.push(format!("the COFF symbol table ({n} entries) is from {from} (same time stamp and image size)"));
        }
        None => {
            put(&mut out, coff + 8, 4, 0);
            put(&mut out, coff + 12, 4, 0);
        }
    }
    Ok(Rebuilt { bytes: out, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file loaded the way the Windows loader does it: headers at the start, each section at its address.
    fn load_pe(file: &[u8]) -> Vec<u8> {
        let (coff, opt, _) = pe_heads(file).expect("a PE file");
        let mut mem = vec![0u8; pe_span(file).unwrap() as usize];
        let heads = u32_at(file, opt + 60).unwrap() as usize;
        mem[..heads].copy_from_slice(&file[..heads]);
        let opt_size = u16_at(file, coff + 16).unwrap() as usize;
        for i in 0..u16_at(file, coff + 2).unwrap() as usize {
            let at = opt + opt_size + i * 40;
            let (va, raw, ptr) =
                (u32_at(file, at + 12).unwrap(), u32_at(file, at + 16).unwrap(), u32_at(file, at + 20).unwrap());
            let n = (raw as usize).min(mem.len() - va as usize);
            mem[va as usize..va as usize + n].copy_from_slice(&file[ptr as usize..ptr as usize + n]);
        }
        mem
    }

    fn mingw_exe(dir: &std::path::Path) -> Option<Vec<u8>> {
        let src = dir.join("w.c");
        std::fs::write(
            &src,
            "static int counter;\nint work(int x) { return x * 3 + counter++; }\nint main(void) { return work(4); }\n",
        )
        .ok()?;
        let exe = dir.join("w.exe");
        let ok = std::process::Command::new("x86_64-w64-mingw32-gcc")
            .args(["-O0", "-o"])
            .arg(&exe)
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("SKIPPED: no x86_64-w64-mingw32-gcc to build a PE file");
            return None;
        }
        std::fs::read(exe).ok()
    }

    #[test]
    fn a_pe_image_from_memory_opens_at_its_load_address_with_its_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let Some(file) = mingw_exe(dir.path()) else { return };
        let mut mem = load_pe(&file);
        // what the program changed in memory is what the rebuilt file holds
        let base = 0x7ff6_1234_0000u64;
        let disk = nexo_bin::Image::open(&dir.path().join("w.exe")).unwrap();
        let work = disk.symbols.iter().find(|s| s.name == "work").expect("work in the COFF symbols").addr;
        let at = (work - disk.image_base) as usize;
        mem[at..at + 6].copy_from_slice(&[0xb8, 0x2a, 0, 0, 0, 0xc3]); // mov eax, 42; ret

        let r = pe(&mem, base, Some(&file), "w.exe").unwrap();
        assert!(r.notes.iter().any(|n| n.contains("COFF symbol table")), "{:?}", r.notes);
        let out = dir.path().join("w.dump");
        std::fs::write(&out, &r.bytes).unwrap();
        let img = nexo_bin::Image::open(&out).unwrap();
        assert_eq!(img.image_base, base);
        let w = img.symbols.iter().find(|s| s.name == "work").expect("work kept its name");
        assert_eq!(w.addr, base + (work - disk.image_base));
        let text = nexo_bin::func_view(&img, &nexo_bin::FnQuery::new("work")).unwrap().text_all();
        assert!(text.contains("mov eax, 0x2a"), "{text}");

        // a file that is not the one loaded gives no symbols, and the sections still open
        let r = pe(&mem, base, Some(b"MZ"), "other.exe").unwrap();
        std::fs::write(&out, &r.bytes).unwrap();
        let img = nexo_bin::Image::open(&out).unwrap();
        assert!(img.sections.iter().any(|s| s.name == ".text"));
    }

    /// A 64-bit ELF image of 0x1000 bytes loaded at 0x1000: one PT_LOAD for all of it, then `extra` program headers.
    fn elf64(extra: &[Phdr]) -> Vec<u8> {
        let mut b = vec![0u8; 0x1000];
        b[..6].copy_from_slice(b"\x7fELF\x02\x01");
        put(&mut b, 0x20, 8, 0x40);
        put(&mut b, 0x36, 2, 0x38);
        put(&mut b, 0x38, 2, 1 + extra.len() as u64);
        for (i, p) in [(PT_LOAD, 0, 0x1000, 0x1000, 0x1000)].iter().chain(extra).enumerate() {
            let at = 0x40 + i * 0x38;
            put(&mut b, at, 4, p.0);
            for (field, v) in [(8, p.1), (0x10, p.2), (0x20, p.3), (0x28, p.4)] {
                put(&mut b, at + field, 8, v);
            }
        }
        b
    }

    #[test]
    fn a_section_count_from_a_header_with_no_entry_size_is_no_section_and_ends_at_once() {
        let mut d = elf64(&[]);
        // e_shnum 0 says the count is the first section header's size; e_shentsize 0 makes the table empty
        put(&mut d, 0x28, 8, 0x200);
        put(&mut d, 0x200 + 0x20, 8, 1 << 40);
        let r = elf(&d, Some(&d), "packed").unwrap();
        assert!(r.notes.iter().any(|n| n.contains("no section headers")), "{:?}", r.notes);
        // a count whose table size wraps past the end of the numbers
        put(&mut d, 0x3a, 2, 0x40);
        put(&mut d, 0x200 + 0x20, 8, u64::MAX / 0x20);
        let r = elf(&d, Some(&d), "packed").unwrap();
        assert!(r.notes.iter().any(|n| n.contains("no section headers")), "{:?}", r.notes);
    }

    #[test]
    fn a_dynamic_table_before_the_image_or_past_its_end_is_left_as_it_is() {
        for at in [0x800, u64::MAX - 0x10] {
            let d = elf64(&[(PT_DYNAMIC, 0, at, 0x900, 0x900)]);
            let r = elf(&d, Some(&d), "odd").unwrap();
            assert_eq!(r.bytes[..0x1000], d[..], "the image is unchanged where the table would go");
        }
    }

    #[test]
    fn sections_that_point_many_times_at_the_same_bytes_are_carried_once_at_most() {
        let mut d = elf64(&[]);
        d.resize(0x8000, 0);
        // 0x100 headers of non-loaded sections, each 0x1000 bytes at file offset 0
        let (shoff, n) = (0x2000usize, 0x100usize);
        put(&mut d, 0x28, 8, shoff as u64);
        put(&mut d, 0x3a, 2, 0x40);
        put(&mut d, 0x3c, 2, n as u64);
        for i in 1..n {
            let at = shoff + i * 0x40;
            put(&mut d, at + 4, 4, SHT_PROGBITS);
            put(&mut d, at + 0x20, 8, 0x1000);
        }
        let mem = d[..0x1000].to_vec();
        let r = elf(&mem, Some(&d), "many").unwrap();
        assert!(r.bytes.len() <= mem.len() + d.len() + n * 0x40 + 16, "{} bytes", r.bytes.len());
        assert!(r.notes.iter().any(|x| x.contains("have no bytes in it")), "{:?}", r.notes);
    }

    #[test]
    fn the_formats_are_told_by_their_first_bytes() {
        assert_eq!(kind(b"\x7fELF\x02\x01"), Kind::Elf);
        assert_eq!(kind(b"MZ\x90\0"), Kind::Pe);
        assert_eq!(kind(b"\xcf\xfa\xed\xfe"), Kind::Other);
        assert!(elf_span(b"\x7fELF").is_none() && pe_span(b"MZ").is_none());
    }
}
