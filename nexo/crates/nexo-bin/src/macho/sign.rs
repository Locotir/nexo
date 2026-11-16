//! An ad-hoc code signature, the one `codesign -s - --entitlements FILE` writes, made without Apple's tools.
//!
//! macOS lets one unprivileged process open another (`task_for_pid`) only when the other one is signed with
//! `com.apple.security.get-task-allow`. The debugger signs a copy of the user's own program that way, so this
//! writes a CodeDirectory with SHA-256 page hashes, an empty requirement set, the entitlements as XML and DER, and
//! an empty CMS slot, wrapped in one SuperBlob at the end of `__LINKEDIT`. The load commands are fixed first
//! (`LC_CODE_SIGNATURE` added when missing, `__LINKEDIT` grown to cover the signature), because the page hashes
//! cover them. A universal file gets each slice signed and is laid out again.

use sha2::{Digest, Sha256};

use crate::{Error, Result};
use nexo_binmagic::{be32, u32_at, CPU_ARM64, FAT_MAGIC, FAT_MAGIC_64, MAX_SLICES, MH_MAGIC_64};

const SUPERBLOB: u32 = 0xfade_0cc0;
const CODE_DIRECTORY: u32 = 0xfade_0c02;
const REQUIREMENTS: u32 = 0xfade_0c01;
const ENTITLEMENTS: u32 = 0xfade_7171;
const DER_ENTITLEMENTS: u32 = 0xfade_7172;
const BLOB_WRAPPER: u32 = 0xfade_0b01;
const CS_ADHOC: u32 = 0x2;
/// The CodeDirectory version with the executable-segment fields.
const CD_VERSION: u32 = 0x0002_0400;
const CD_HEADER: usize = 88;
const HASH_LEN: usize = 32;
/// The pages hashed are 4 KiB on every architecture (the `pageSize` field holds log2 of this).
const PAGE: usize = 4096;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const MH_EXECUTE: u32 = 2;
const EXECSEG_MAIN_BINARY: u64 = 0x1;
const EXECSEG_ALLOW_UNSIGNED: u64 = 0x10;
const EXECSEG_DEBUGGER: u64 = 0x20;
/// The entitlement that lets a debugger of the same user open the program.
pub const GET_TASK_ALLOW: &str = "com.apple.security.get-task-allow";

fn cannot(why: &str) -> Error {
    Error::NotRead(format!("cannot sign this Mach-O file: {why}"))
}

fn le64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

/// Sign `data` (a 64-bit Mach-O file or a universal file of them) ad-hoc. `ident` names the code (the file name,
/// as codesign does) and each of `entitlements` is set to true. The signature it had is replaced.
pub fn adhoc_sign(data: &[u8], ident: &str, entitlements: &[&str]) -> Result<Vec<u8>> {
    if ident.is_empty() || ident.contains('\0') {
        return Err(cannot("the identifier is empty or holds a zero byte"));
    }
    match be32(data, 0) {
        Some(FAT_MAGIC) => sign_fat(data, ident, entitlements),
        Some(FAT_MAGIC_64) => Err(cannot("universal files with 64-bit offsets are not signed here")),
        _ => sign_thin(data, ident, entitlements),
    }
}

fn sign_fat(data: &[u8], ident: &str, ents: &[&str]) -> Result<Vec<u8>> {
    let n = be32(data, 4).ok_or_else(|| cannot("the universal header is cut"))? as usize;
    if n == 0 || n > MAX_SLICES {
        return Err(cannot("the universal header lists no slices or too many"));
    }
    let head = 8 + 20 * n;
    let mut slices = Vec::new();
    for i in 0..n {
        let at = (8 + 20 * i) as u64;
        let word = |k: u64| be32(data, at + k).ok_or_else(|| cannot("the universal header is cut"));
        let (cpu, sub, off, size, align) = (word(0)?, word(4)?, word(8)? as usize, word(12)? as usize, word(16)?);
        let body = off
            .checked_add(size)
            .and_then(|end| data.get(off..end))
            .ok_or_else(|| cannot("a slice of the universal file lies past its end"))?;
        if align > 15 {
            return Err(cannot("a slice asks for an alignment over 32 KiB"));
        }
        slices.push((cpu, sub, align, sign_thin(body, ident, ents)?));
    }
    let mut out = data[..head].to_vec();
    for (i, (cpu, sub, align, body)) in slices.iter().enumerate() {
        let at = out.len().next_multiple_of(1usize << align);
        out.resize(at, 0);
        let entry = 8 + 20 * i;
        let fields = [*cpu, *sub, u32::try_from(at).map_err(|_| cannot("the signed file passes 4 GiB"))?];
        for (k, v) in fields.iter().enumerate() {
            out[entry + 4 * k..entry + 4 * k + 4].copy_from_slice(&v.to_be_bytes());
        }
        let len = u32::try_from(body.len()).map_err(|_| cannot("a signed slice passes 4 GiB"))?;
        out[entry + 12..entry + 16].copy_from_slice(&len.to_be_bytes());
        out[entry + 16..entry + 20].copy_from_slice(&align.to_be_bytes());
        out.extend_from_slice(body);
    }
    Ok(out)
}

/// The parts of a thin file the signer changes.
struct Layout {
    cpu: u32,
    filetype: u32,
    ncmds: u32,
    sizeofcmds: usize,
    /// (file offset, size) of `__TEXT`.
    text: (u64, u64),
    /// Where the `__LINKEDIT` load command is.
    linkedit: usize,
    /// Where `LC_CODE_SIGNATURE` is, when the file has one.
    sig: Option<usize>,
    /// The first file byte a section uses: the load commands must end before it.
    first_section: usize,
}

fn layout(data: &[u8]) -> Result<Layout> {
    if u32_at(data, 0) != Some(MH_MAGIC_64) {
        return Err(cannot("it is not a 64-bit little-endian Mach-O file"));
    }
    let word = |o: usize| u32_at(data, o as u64).ok_or_else(|| cannot("the load commands are cut"));
    let (cpu, filetype, ncmds, sizeofcmds) = (word(4)?, word(12)?, word(16)?, word(20)? as usize);
    let end = 32usize
        .checked_add(sizeofcmds)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| cannot("the load commands are cut"))?;
    let (mut text, mut linkedit, mut sig, mut first_section) = (None, None, None, data.len());
    let mut at = 32usize;
    for _ in 0..ncmds {
        let (cmd, size) = (word(at)?, word(at + 4)? as usize);
        if size < 8 || at + size > end {
            return Err(cannot("a load command runs past the others"));
        }
        match cmd {
            LC_SEGMENT_64 if size >= 72 => {
                let name = &data[at + 8..at + 24];
                let field = |k: usize| le64(data, at + k).ok_or_else(|| cannot("a segment command is cut"));
                if name.starts_with(b"__TEXT\0") {
                    text = Some((field(40)?, field(48)?));
                } else if name.starts_with(b"__LINKEDIT\0") {
                    linkedit = Some(at);
                }
                let nsects = word(at + 64)? as usize;
                if 72 + nsects * 80 > size {
                    return Err(cannot("a segment lists more sections than its command holds"));
                }
                for s in 0..nsects {
                    let sect = at + 72 + s * 80;
                    let (off, kind) = (word(sect + 48)? as usize, word(sect + 64)? & 0xff);
                    // zero-fill sections take no file bytes
                    if off != 0 && !matches!(kind, 0x1 | 0xc | 0x12) {
                        first_section = first_section.min(off);
                    }
                }
            }
            LC_CODE_SIGNATURE if size == 16 => sig = Some(at),
            _ => {}
        }
        at += size;
    }
    Ok(Layout {
        cpu,
        filetype,
        ncmds,
        sizeofcmds,
        text: text.ok_or_else(|| cannot("it has no __TEXT segment"))?,
        linkedit: linkedit.ok_or_else(|| cannot("it has no __LINKEDIT segment"))?,
        sig,
        first_section,
    })
}

fn blob(magic: u32, body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + body.len());
    b.extend(magic.to_be_bytes());
    b.extend(((8 + body.len()) as u32).to_be_bytes());
    b.extend(body);
    b
}

/// The entitlements as the property list codesign embeds, each key set to true, sorted.
fn entitlements_xml(keys: &[&str]) -> String {
    let mut s = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
        "<plist version=\"1.0\">\n<dict>\n"
    ));
    for k in keys {
        s.push_str(&format!("\t<key>{k}</key>\n\t<true/>\n"));
    }
    s.push_str("</dict>\n</plist>\n");
    s
}

fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = body.len();
    if n < 0x80 {
        v.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
        v.push(0x80 | bytes.len() as u8);
        v.extend(bytes);
    }
    v.extend(body);
    v
}

/// The entitlements in the DER form the kernel reads: `[APPLICATION 16] { INTEGER 1, [16] { SEQUENCE { UTF8String
/// key, BOOLEAN true } ... } }`.
fn entitlements_der(keys: &[&str]) -> Vec<u8> {
    let mut pairs = Vec::new();
    for k in keys {
        let mut pair = der(0x0c, k.as_bytes());
        pair.extend([0x01, 0x01, 0xff]);
        pairs.extend(der(0x30, &pair));
    }
    let mut body = vec![0x02, 0x01, 0x01];
    body.extend(der(0xb0, &pairs));
    der(0x70, &body)
}

fn sign_thin(data: &[u8], ident: &str, ents: &[&str]) -> Result<Vec<u8>> {
    let lay = layout(data)?;
    let mut keys: Vec<&str> = ents.to_vec();
    keys.sort_unstable();
    keys.dedup();

    let le = lay.linkedit;
    let le_off = le64(data, le + 40).ok_or_else(|| cannot("the __LINKEDIT command is cut"))?;
    let le_vmsize = le64(data, le + 32).ok_or_else(|| cannot("the __LINKEDIT command is cut"))?;
    let mut out;
    let mut sig_at = lay.sig;
    match lay.sig {
        Some(s) => {
            let dataoff = u32_at(data, s as u64 + 8).unwrap_or(0) as usize;
            if dataoff == 0 || dataoff > data.len() || (dataoff as u64) < le_off {
                return Err(cannot("its signature command points outside __LINKEDIT"));
            }
            out = data[..dataoff].to_vec();
        }
        None => {
            let le_size = le64(data, le + 48).unwrap_or(0);
            if le_off.checked_add(le_size) != Some(data.len() as u64) {
                return Err(cannot("__LINKEDIT does not end the file, so the signature has no place"));
            }
            let end = 32 + lay.sizeofcmds;
            if end + 16 > lay.first_section || data[end..end + 16].iter().any(|b| *b != 0) {
                return Err(cannot("its header has no free room for a signature load command"));
            }
            out = data.to_vec();
            put32(&mut out, end, LC_CODE_SIGNATURE);
            put32(&mut out, end + 4, 16);
            put32(&mut out, 16, lay.ncmds + 1);
            put32(&mut out, 20, (lay.sizeofcmds + 16) as u32);
            sig_at = Some(end);
        }
    }
    let sig_at = sig_at.unwrap_or_default();
    let code_limit = out.len().next_multiple_of(16);
    out.resize(code_limit, 0);
    let code_limit32 = u32::try_from(code_limit).map_err(|_| cannot("it passes 4 GiB"))?;

    // every blob but the CodeDirectory, which hashes them
    let requirements = blob(REQUIREMENTS, &0u32.to_be_bytes());
    let xml = (!keys.is_empty()).then(|| blob(ENTITLEMENTS, entitlements_xml(&keys).as_bytes()));
    let der_blob = (!keys.is_empty()).then(|| blob(DER_ENTITLEMENTS, &entitlements_der(&keys)));
    let cms = blob(BLOB_WRAPPER, &[]);
    let n_special: usize = if keys.is_empty() { 2 } else { 7 };
    let n_code = code_limit.div_ceil(PAGE);
    let ident_len = ident.len() + 1;
    let cd_len = CD_HEADER + ident_len + (n_special + n_code) * HASH_LEN;
    let mut blobs: Vec<(u32, usize)> = vec![(0, cd_len), (2, requirements.len())];
    if let (Some(x), Some(d)) = (&xml, &der_blob) {
        blobs.push((5, x.len()));
        blobs.push((7, d.len()));
    }
    blobs.push((0x1_0000, cms.len()));
    let super_len = 12 + 8 * blobs.len() + blobs.iter().map(|b| b.1).sum::<usize>();
    let data_size = super_len.next_multiple_of(16);

    // the load commands as they will be, before any page is hashed
    put32(&mut out, sig_at + 8, code_limit32);
    put32(&mut out, sig_at + 12, data_size as u32);
    let le_filesize = (code_limit + data_size) as u64 - le_off;
    let seg_page: u64 = if lay.cpu == CPU_ARM64 { 0x4000 } else { 0x1000 };
    put64(&mut out, le + 48, le_filesize);
    put64(&mut out, le + 32, le_vmsize.max(le_filesize.next_multiple_of(seg_page)));

    let mut exec_flags = 0u64;
    if lay.filetype == MH_EXECUTE {
        exec_flags |= EXECSEG_MAIN_BINARY;
    }
    if keys.contains(&GET_TASK_ALLOW) {
        exec_flags |= EXECSEG_ALLOW_UNSIGNED;
    }
    if keys.contains(&"com.apple.security.cs.debugger") {
        exec_flags |= EXECSEG_DEBUGGER;
    }
    let hash_offset = CD_HEADER + ident_len + n_special * HASH_LEN;
    let mut cd = Vec::with_capacity(cd_len);
    for w in [
        CODE_DIRECTORY,
        cd_len as u32,
        CD_VERSION,
        CS_ADHOC,
        hash_offset as u32,
        CD_HEADER as u32,
        n_special as u32,
        n_code as u32,
        code_limit32,
    ] {
        cd.extend(w.to_be_bytes());
    }
    cd.extend([HASH_LEN as u8, 2, 0, 12]);
    // spare2, scatterOffset, teamOffset, spare3, codeLimit64, then the executable segment
    cd.extend([0u8; 16]);
    cd.extend(0u64.to_be_bytes());
    for v in [lay.text.0, lay.text.1, exec_flags] {
        cd.extend(v.to_be_bytes());
    }
    debug_assert_eq!(cd.len(), CD_HEADER);
    cd.extend(ident.as_bytes());
    cd.push(0);
    // special slots, the highest number first: slot -n sits n hashes before the code hashes
    let mut special = vec![[0u8; HASH_LEN]; n_special];
    let sha = |b: &[u8]| -> [u8; HASH_LEN] { Sha256::digest(b).into() };
    special[n_special - 2] = sha(&requirements);
    if let (Some(x), Some(d)) = (&xml, &der_blob) {
        special[n_special - 5] = sha(x);
        special[n_special - 7] = sha(d);
    }
    for h in &special {
        cd.extend(h);
    }
    for page in out.chunks(PAGE) {
        cd.extend(sha(page));
    }
    debug_assert_eq!(cd.len(), cd_len);

    let mut sb = Vec::with_capacity(data_size);
    sb.extend(SUPERBLOB.to_be_bytes());
    sb.extend((super_len as u32).to_be_bytes());
    sb.extend((blobs.len() as u32).to_be_bytes());
    let mut at = 12 + 8 * blobs.len();
    for (slot, len) in &blobs {
        sb.extend(slot.to_be_bytes());
        sb.extend((at as u32).to_be_bytes());
        at += len;
    }
    sb.extend(cd);
    sb.extend(requirements);
    if let (Some(x), Some(d)) = (xml, der_blob) {
        sb.extend(x);
        sb.extend(d);
    }
    sb.extend(cms);
    sb.resize(data_size, 0);
    out.extend(sb);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_macho::{thin, universal};

    fn be(b: &[u8], o: usize) -> u32 {
        u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
    }

    /// Check one signed thin file the way the kernel does: the signature sits where its load command says, the
    /// page hashes match the bytes, the special slots match the blobs. Returns the entitlements XML.
    fn verify(b: &[u8]) -> String {
        let lay = layout(b).expect("the signed file still parses");
        let s = lay.sig.expect("it has LC_CODE_SIGNATURE");
        let (off, size) = (u32_at(b, s as u64 + 8).unwrap() as usize, u32_at(b, s as u64 + 12).unwrap() as usize);
        assert_eq!(off + size, b.len(), "the signature ends the file");
        let le_end = le64(b, lay.linkedit + 40).unwrap() + le64(b, lay.linkedit + 48).unwrap();
        assert_eq!(le_end, b.len() as u64, "__LINKEDIT covers the signature");
        let sig = &b[off..];
        assert_eq!(be(sig, 0), SUPERBLOB);
        let count = be(sig, 8) as usize;
        let mut found = std::collections::HashMap::new();
        for i in 0..count {
            let (slot, at) = (be(sig, 12 + 8 * i), be(sig, 16 + 8 * i) as usize);
            let len = be(sig, at + 4) as usize;
            found.insert(slot, &sig[at..at + len]);
        }
        let cd = found[&0];
        assert_eq!(be(cd, 0), CODE_DIRECTORY);
        assert_eq!(be(cd, 12) & CS_ADHOC, CS_ADHOC);
        let (hash_off, n_special, n_code, limit) =
            (be(cd, 16) as usize, be(cd, 24) as usize, be(cd, 28) as usize, be(cd, 32) as usize);
        assert_eq!(limit, off);
        for (i, page) in b[..limit].chunks(PAGE).enumerate() {
            let want: [u8; 32] = Sha256::digest(page).into();
            assert_eq!(&cd[hash_off + 32 * i..hash_off + 32 * (i + 1)], &want, "page {i}");
        }
        assert_eq!(n_code, limit.div_ceil(PAGE));
        for (slot, n) in [(2u32, 2usize), (5, 5), (7, 7)] {
            if n > n_special {
                continue;
            }
            let want: [u8; 32] = Sha256::digest(found[&slot]).into();
            assert_eq!(&cd[hash_off - 32 * n..hash_off - 32 * (n - 1)], &want, "special slot -{n}");
        }
        found.get(&5).map(|x| String::from_utf8_lossy(&x[8..]).into_owned()).unwrap_or_default()
    }

    #[test]
    fn a_program_without_a_signature_gets_one_with_its_entitlements_and_still_opens() {
        let fx = thin(false, false);
        let signed = adhoc_sign(&fx.bytes, "prog", &[GET_TASK_ALLOW]).unwrap();
        let xml = verify(&signed);
        assert!(xml.contains("<key>com.apple.security.get-task-allow</key>\n\t<true/>"), "{xml}");
        assert_eq!(u32_at(&signed, 16), u32_at(&fx.bytes, 16).map(|n| n + 1), "one more load command");
        let der = entitlements_der(&[GET_TASK_ALLOW]);
        assert_eq!(&der[..7], &[0x70, 0x2d, 0x02, 0x01, 0x01, 0xb0, 0x28]);
        assert_eq!(&der[der.len() - 3..], &[0x01, 0x01, 0xff]);
        let img = crate::Image::from_bytes(signed, "/fx/prog".into()).expect("the signed file opens");
        assert!(img.warnings.is_empty(), "{:?}", img.warnings);
    }

    #[test]
    fn signing_a_signed_file_again_replaces_the_signature_and_gives_the_same_bytes() {
        let fx = thin(true, false);
        let once = adhoc_sign(&fx.bytes, "prog", &[GET_TASK_ALLOW]).unwrap();
        let twice = adhoc_sign(&once, "prog", &[GET_TASK_ALLOW]).unwrap();
        assert_eq!(once, twice);
        let plain = adhoc_sign(&once, "prog", &[]).unwrap();
        assert_eq!(verify(&plain), "", "no entitlements, no entitlement blob");
    }

    #[test]
    fn each_slice_of_a_universal_file_is_signed_and_placed_on_its_alignment() {
        let signed = adhoc_sign(&universal(), "prog", &[GET_TASK_ALLOW]).unwrap();
        assert_eq!(be(&signed, 4), 2);
        for i in 0..2 {
            let (off, size, align) = (be(&signed, 16 + 20 * i), be(&signed, 20 + 20 * i), be(&signed, 24 + 20 * i));
            assert_eq!(off % (1 << align), 0);
            verify(&signed[off as usize..(off + size) as usize]);
        }
        let img = crate::Image::from_bytes_arch(signed, "/fx/prog".into(), Some("arm64")).unwrap();
        assert!(img.warnings.is_empty(), "{:?}", img.warnings);
    }

    #[test]
    fn a_file_that_is_not_a_64_bit_mach_o_is_refused_with_the_reason() {
        let e = adhoc_sign(b"\x7fELF\x02\x01\x01\0", "prog", &[]).unwrap_err().to_string();
        assert!(e.contains("not a 64-bit little-endian Mach-O"), "{e}");
    }

    /// The test the kernel's own checker gives: Apple's `codesign -v` accepts what this signer wrote, on a program
    /// the linker signed, on one with no signature, and on a universal system program.
    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_accepts_what_the_signer_wrote() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("p.c");
        std::fs::write(&src, "int main(void) { return 7; }\n").unwrap();
        let prog = dir.path().join("p");
        let cc = Command::new("cc").arg("-o").arg(&prog).arg(&src).status().unwrap();
        assert!(cc.success());
        let bare = dir.path().join("bare");
        std::fs::copy(&prog, &bare).unwrap();
        let rm = Command::new("/usr/bin/codesign").arg("--remove-signature").arg(&bare).status().unwrap();
        assert!(rm.success());
        let ls = dir.path().join("ls");
        std::fs::copy("/bin/ls", &ls).unwrap();
        for (file, runs) in [(&prog, Some(7)), (&bare, Some(7)), (&ls, None)] {
            let signed = adhoc_sign(&std::fs::read(file).unwrap(), "p", &[GET_TASK_ALLOW]).unwrap();
            std::fs::write(file, signed).unwrap();
            let v = Command::new("/usr/bin/codesign").args(["-v", "-v"]).arg(file).output().unwrap();
            assert!(v.status.success(), "{}: {}", file.display(), String::from_utf8_lossy(&v.stderr));
            let d = Command::new("/usr/bin/codesign").args(["-d", "--entitlements", "-"]).arg(file).output().unwrap();
            let said = format!("{}{}", String::from_utf8_lossy(&d.stdout), String::from_utf8_lossy(&d.stderr));
            assert!(said.contains(GET_TASK_ALLOW), "{said}");
            if let Some(code) = runs {
                assert_eq!(Command::new(file).status().unwrap().code(), Some(code));
            }
        }
    }
}
