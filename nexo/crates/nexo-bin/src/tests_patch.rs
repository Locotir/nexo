use std::{fs, path::Path};

use crate::{assemble, fixtures, patch, patch_file, patch_with, Image, PatchData as D, PatchOpts, PatchTarget as T};

#[test]
fn asm_labels_use_rel32_at_the_requested_origin_in_both_modes() {
    for bits in [32, 64] {
        for at in [0, 0x401000] {
            let b = assemble("start: nop; call end; jmp start; end: ret", at, bits).unwrap();
            assert_eq!(b, [0x90, 0xe8, 5, 0, 0, 0, 0xe9, 0xf5, 0xff, 0xff, 0xff, 0xc3]);
            assert_eq!(assemble(&format!("call {:#x}", at + 0x20), at, bits).unwrap(), [0xe8, 0x1b, 0, 0, 0]);
        }
    }
    assert_eq!(assemble("xor eax, eax; mov eax, 42; ret", 0, 64).unwrap(), [0x31, 0xc0, 0xb8, 42, 0, 0, 0, 0xc3]);
    assert_eq!(assemble("mov rax, qword ptr [rbp-8]", 0, 64).unwrap(), [0x48, 0x8b, 0x45, 0xf8]);
    assert_eq!(assemble("mov eax, [rip+4]", 0x1000, 64).unwrap(), [0x8b, 5, 4, 0, 0, 0]);
    assert_eq!(assemble("mov eax, [ebx+ecx*4+8]", 0, 32).unwrap(), [0x8b, 0x44, 0x8b, 8]);
    for source in [
        "",
        "badopcode eax",
        "jmp missing",
        "x: nop; x: ret",
        "mov eax, 0x100000000",
        "mov eax, [rax-0x8000000000000001]",
    ] {
        assert!(assemble(source, 0, 64).is_err(), "{source}");
    }
    assert!(assemble("ret", 0, 16).is_err());
    assert!(assemble("ret", u64::MAX, 64).is_err());
    assert!(assemble("ret", 1 << 32, 32).is_err());
}

fn round_trip(img: &Image, dir: &Path, original: &[u8], at: u64) {
    for (name, data, expected) in [
        ("nop", D::Nop(3), vec![0x90; 3]),
        ("asm", D::Asm("mov eax, 7; ret"), vec![0xb8, 7, 0, 0, 0, 0xc3]),
        ("hex", D::Bytes("31c0 C3"), vec![0x31, 0xc0, 0xc3]),
    ] {
        let out = dir.join(name);
        let answer = patch(img, &out, T::Address(at), data).unwrap();
        let reread = Image::open(&out).unwrap();
        assert_eq!(reread.read_at(at, expected.len()).unwrap(), expected);
        assert_eq!(fs::read(&img.path).unwrap(), original);
        let off = img.addr_to_offset(at).unwrap() as usize;
        let mut full = original.to_vec();
        full[off..off + expected.len()].copy_from_slice(&expected);
        assert_eq!(fs::read(out).unwrap(), full);
        let text = answer.text_all();
        for word in ["file offset", "old:", "new:", "before:", "after:"] {
            assert!(text.contains(word), "{text}");
        }
    }
}

#[test]
fn elf_and_pe_patch_round_trips_and_refusals() {
    for f in [fixtures::elf64(false), fixtures::pe64(false)] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        fs::write(&input, &f.bytes).unwrap();
        let img = Image::open(&input).unwrap();
        round_trip(&img, dir.path(), &f.bytes, f.b);
        assert!(patch(&img, &input, T::Address(f.b), D::Nop(1)).unwrap_err().to_string().contains("OUT == IN"));
        let alias = dir.path().join(".").join("input");
        assert!(patch(&img, &alias, T::Address(f.b), D::Nop(1)).unwrap_err().to_string().contains("OUT == IN"));
        let hardlink = dir.path().join("hardlink");
        fs::hard_link(&input, &hardlink).unwrap();
        assert!(patch(&img, &hardlink, T::Address(f.b), D::Nop(1)).is_err());
        let existing = dir.path().join("existing");
        fs::write(&existing, b"keep").unwrap();
        assert!(patch(&img, &existing, T::Address(f.b), D::Nop(1)).unwrap_err().to_string().contains("already exists"));
        assert_eq!(fs::read(existing).unwrap(), b"keep");
        let out = dir.path().join("refused");
        let sec = img.section_at(f.b).unwrap();
        let at = sec.addr + img.section_bytes(sec).len() as u64 - 1;
        let e = patch(&img, &out, T::Address(at), D::Nop(2)).unwrap_err().to_string();
        assert!(e.contains(&sec.name) && e.contains("file-backed") && e.contains("limits"), "{e}");
        for (at, data) in [
            (0, D::Nop(1)),
            (f.b, D::Nop(0)),
            (f.b, D::Bytes("??")),
            (f.b, D::Bytes("a")),
            (f.b, D::Asm("no_such_instruction")),
        ] {
            assert!(patch(&img, &out, T::Address(at), data).is_err());
            assert!(!out.exists());
        }
        // A section may claim more virtual bytes than the file supplies.
        let mut truncated = Image::open(&input).unwrap();
        truncated.sections.iter_mut().find(|s| s.addr == sec.addr).unwrap().file_size = 1;
        let e = patch(&truncated, &out, T::Address(sec.addr + 1), D::Nop(1)).unwrap_err().to_string();
        assert!(e.contains("file-backed") && e.contains(&sec.name), "{e}");
        assert!(!out.exists());
        assert_eq!(fs::read(&input).unwrap(), f.bytes);
    }
}

#[cfg(unix)]
#[test]
fn output_symlink_to_input_or_missing_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    let f = fixtures::elf64(false);
    fs::write(&input, &f.bytes).unwrap();
    let img = Image::open(&input).unwrap();
    for (name, target) in [("alias", input), ("dangling", dir.path().join("missing"))] {
        let out = dir.path().join(name);
        std::os::unix::fs::symlink(target, &out).unwrap();
        assert!(patch(&img, &out, T::Address(f.b), D::Nop(1)).is_err());
        assert!(fs::symlink_metadata(out).unwrap().file_type().is_symlink());
    }
}

#[cfg(unix)]
#[test]
fn compiled_c_function_offset_patch_round_trip() {
    use std::process::Command;
    let cc = match Command::new("cc").arg("--version").output() {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: cc not present");
            return;
        }
        Err(e) => panic!("cannot check cc: {e}"),
    };
    assert!(cc.status.success());
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("tiny.c");
    let exe = dir.path().join("tiny");
    fs::write(&source, "int tiny(int x) { return x + 3; }\nint main(void) { return tiny(4); }\n").unwrap();
    let built = Command::new("cc")
        .args(crate::tests_map::x86_args())
        .args(["-O0", "-o"])
        .arg(&exe)
        .arg(&source)
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let img = Image::open(&exe).unwrap();
    let original = fs::read(&exe).unwrap();
    let start = crate::ops::resolve(&img, "tiny").unwrap().start;
    round_trip(&img, dir.path(), &original, start);
    let out = dir.path().join("offset");
    patch(&img, &out, T::Function("tiny+0x1"), D::Bytes("90")).unwrap();
    assert_eq!(Image::open(&out).unwrap().read_at(start + 1, 1).unwrap(), [0x90]);
    assert_eq!(fs::read(exe).unwrap(), original);
}

fn macho_patch(dir: &Path, name: &str, input: &[u8], bytes: &str) -> (Vec<u8>, String) {
    let path = dir.join(name);
    fs::write(&path, input).unwrap();
    let img = Image::open(&path).unwrap();
    let out = dir.join(format!("{name}.patched"));
    let answer = patch(&img, &out, T::Address(crate::tests_macho::SPARE), D::Bytes(bytes)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), input, "the input is not touched");
    (fs::read(out).unwrap(), answer.text_all())
}

#[test]
fn a_patched_signed_mach_o_carries_a_valid_ad_hoc_signature_that_the_reader_checks() {
    use crate::{read_signature, Signature};
    let dir = tempfile::tempdir().unwrap();
    for (arm64, code) in [(false, "c3"), (true, "c0035fd6")] {
        let signed = crate::adhoc_sign(&crate::tests_macho::thin(arm64, false).bytes, "prog", &[]).unwrap();
        assert!(matches!(read_signature(&signed), Signature::AdHoc { .. }), "the fixture is signed");
        // the same edit without a new signature breaks the hashes: this is what the kernel would reject
        let img = Image::from_bytes(signed.clone(), "/fx/prog".into()).unwrap();
        let off = img.addr_to_offset(crate::tests_macho::SPARE).unwrap() as usize;
        let mut stale = signed.clone();
        stale[off] ^= 0xff;
        assert!(matches!(read_signature(&stale), Signature::Broken(_)), "{:?}", read_signature(&stale));

        let (patched, text) = macho_patch(dir.path(), if arm64 { "arm" } else { "x86" }, &signed, code);
        assert!(matches!(read_signature(&patched), Signature::AdHoc { pages } if pages >= 1), "{text}");
        assert!(text.contains("signature: re-signed ad hoc"), "{text}");
        let reread = Image::from_bytes(patched, "/fx/prog".into()).unwrap();
        assert_eq!(reread.read_at(crate::tests_macho::SPARE, 1).unwrap()[0], if arm64 { 0xc0 } else { 0xc3 });
        let (_, sig) = reread.hardening.items.iter().find(|(k, _)| *k == "code_signature").unwrap();
        assert!(sig.contains("ad hoc") && sig.contains("page hashes match"), "{sig}");
    }
}

#[test]
fn a_patched_unsigned_x86_mach_o_stays_unsigned_and_an_unsigned_arm64_one_gets_signed() {
    use crate::{read_signature, Signature};
    let dir = tempfile::tempdir().unwrap();
    let (x86, text) = macho_patch(dir.path(), "x86", &crate::tests_macho::thin(false, false).bytes, "c3");
    assert_eq!(read_signature(&x86), Signature::Unsigned, "{text}");
    assert!(!text.contains("signature:"), "{text}");
    let (arm, text) = macho_patch(dir.path(), "arm", &crate::tests_macho::thin(true, false).bytes, "c0035fd6");
    assert!(matches!(read_signature(&arm), Signature::AdHoc { .. }), "{text}");
    assert!(text.contains("the old one was: no"), "{text}");
}

/// Replace the signature of a signed thin x86-64 fixture by a SuperBlob that has a primary CodeDirectory of the given
/// hash type (ad hoc flag set) at slot 0 and, when given, the file's own SHA-256 CodeDirectory at slot 0x1000.
fn with_primary_cd(signed: &[u8], primary_hash_type: u8, keep_sha256_alternate: bool) -> Vec<u8> {
    let le = |d: &[u8], o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
    let be = |d: &[u8], o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap()) as usize;
    let mut at = 32;
    while le(signed, at) != 0x1d {
        at += le(signed, at + 4);
    }
    let (off, len) = (le(signed, at + 8), le(signed, at + 12));
    let blob = &signed[off..off + len];
    let cd_off = (0..be(blob, 8)).find(|i| be(blob, 12 + 8 * i) == 0).map(|i| be(blob, 16 + 8 * i)).unwrap();
    let cd = &blob[cd_off..cd_off + be(blob, cd_off + 4)];
    // the load command changes size, so the pages that cover it are hashed again
    let total = 12 + 8 * (1 + usize::from(keep_sha256_alternate)) + cd.len() * (1 + usize::from(keep_sha256_alternate));
    let mut head = signed[..off].to_vec();
    head[at + 12..at + 16].copy_from_slice(&(total as u32).to_le_bytes());
    let mut cd = cd.to_vec();
    let hash_off = be(&cd, 16);
    for (i, page) in head.chunks(4096).enumerate() {
        use sha2::Digest;
        cd[hash_off + 32 * i..hash_off + 32 * (i + 1)].copy_from_slice(&sha2::Sha256::digest(page));
    }
    let cd = &cd[..];
    let mut fake = cd.to_vec();
    fake[37] = primary_hash_type;
    fake[36] = 20;
    let entries: Vec<(u32, &[u8])> =
        if keep_sha256_alternate { vec![(0, &fake), (0x1000, cd)] } else { vec![(0, &fake)] };
    let mut out = head;
    let mut sb = Vec::new();
    sb.extend_from_slice(&0xfade_0cc0u32.to_be_bytes());
    sb.extend_from_slice(&0u32.to_be_bytes());
    sb.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut pos = 12 + 8 * entries.len();
    for (slot, body) in &entries {
        sb.extend_from_slice(&slot.to_be_bytes());
        sb.extend_from_slice(&(pos as u32).to_be_bytes());
        pos += body.len();
    }
    for (_, body) in &entries {
        sb.extend_from_slice(body);
    }
    assert_eq!(sb.len(), total);
    sb[4..8].copy_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&sb);
    out
}

#[test]
fn an_ad_hoc_file_with_a_sha1_primary_and_a_sha256_alternate_is_checked_through_the_alternate() {
    use crate::{read_signature, Signature};
    let signed = crate::adhoc_sign(&crate::tests_macho::thin(false, false).bytes, "prog", &[]).unwrap();
    let both = with_primary_cd(&signed, 1, true);
    assert!(matches!(read_signature(&both), Signature::AdHoc { pages } if pages >= 1), "{:?}", read_signature(&both));
    // the alternate is the one that is checked: a changed byte of the code breaks it
    let img = Image::from_bytes(both.clone(), "/fx/prog".into()).unwrap();
    let mut stale = both.clone();
    stale[img.addr_to_offset(crate::tests_macho::SPARE).unwrap() as usize] ^= 0xff;
    assert!(matches!(read_signature(&stale), Signature::Broken(_)), "{:?}", read_signature(&stale));
    // with no SHA-256 CodeDirectory at all it is still ad hoc, not a certificate
    let sha1_only = with_primary_cd(&signed, 1, false);
    let seen = read_signature(&sha1_only);
    assert_eq!(seen, Signature::AdHocUnchecked);
    assert!(seen.describe().contains("ad hoc") && !seen.describe().contains("certificate"), "{}", seen.describe());
}

fn elf_input(dir: &Path) -> (fixtures::Fixture, std::path::PathBuf, Image) {
    let f = fixtures::elf64(false);
    let input = dir.join("input");
    fs::write(&input, &f.bytes).unwrap();
    let img = Image::open(&input).unwrap();
    (f, input, img)
}

#[test]
fn a_patch_file_with_a_bad_line_is_refused_with_its_line_number() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _, img) = elf_input(dir.path());
    let out = dir.path().join("out");
    let at = format!("{:#x}", f.b);
    let opts = PatchOpts::default();
    let cases: Vec<(String, &str)> = vec![
        ("# nothing\n\n".into(), "no patches"),
        (format!("# ok\n{at}\n"), "line 2"),
        (format!("{at} 9\n"), "line 1"),
        (format!("{at} 90 9090\n"), "OLD_HEX has 1 bytes"),
        (format!("{at} nop 0\n"), "nop length"),
        (format!("{at} 90\nzzz 90\n"), "line 2"),
        (format!("{at} 90 90 90\n"), "expected"),
    ];
    for (text, word) in &cases {
        let e = patch_file(&img, Some(&out), text, opts).unwrap_err().to_string();
        assert!(e.contains(word), "{text:?}: {e}");
        assert!(!out.exists());
    }
    let e = patch_file(&img, Some(&out), &format!("{at} 90\n"), PatchOpts { verify: true, ..opts }).unwrap_err();
    assert!(e.to_string().contains("--verify needs OLD_HEX"), "{e}");
}

#[test]
fn a_patch_file_is_all_or_none() {
    let dir = tempfile::tempdir().unwrap();
    let (f, _, img) = elf_input(dir.path());
    let out = dir.path().join("out");
    let (b0, b1, b4) = (f.b, f.b + 1, f.b + 4);
    // two lines touch the byte at b+1: nothing is written, though the first line alone is fine
    let text = format!("{b0:#x} 9090\n{b1:#x} 9090\n");
    let e = patch_file(&img, Some(&out), &text, PatchOpts::default()).unwrap_err().to_string();
    assert!(e.contains("overlap") && e.contains("nothing was written"), "{e}");
    assert!(!out.exists());
    // the second line is outside every section: the first is not applied either
    let text = format!("{b0:#x} 9090\n0x10 90\n");
    assert!(patch_file(&img, Some(&out), &text, PatchOpts::default()).is_err());
    assert!(!out.exists());
    // good lines, in any order, old bytes given on one
    let old = crate::ops::hex(img.read_at(b0, 2).unwrap()).replace(' ', "");
    let text = format!("{b4:#x} nop 2 # two nops\n{b0:#x} {old} cccc\n");
    let answer = patch_file(&img, Some(&out), &text, PatchOpts::default()).unwrap();
    assert!(answer.text_all().contains("2 patches"));
    let copy = Image::open(&out).unwrap();
    assert_eq!(copy.read_at(b0, 2).unwrap(), [0xcc, 0xcc]);
    assert_eq!(copy.read_at(b4, 2).unwrap(), [0x90, 0x90]);
}

#[test]
fn verify_refuses_bytes_that_differ_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (f, input, img) = elf_input(dir.path());
    let out = dir.path().join("out");
    let spaced = crate::ops::hex(img.read_at(f.b, 2).unwrap());
    let real = spaced.replace(' ', "");
    let wrong = if real == "ffff" { "0000" } else { "ffff" };
    let opts = PatchOpts { old: Some(wrong), verify: true, ..Default::default() };
    let e = patch_with(&img, Some(&out), T::Address(f.b), D::Bytes("9090"), opts).unwrap_err().to_string();
    assert!(e.contains("refused") && e.contains(&spaced), "{e}");
    assert!(!out.exists());
    // --old alone checks too; --verify without --old is a usage error
    let opts = PatchOpts { old: Some(wrong), ..Default::default() };
    assert!(patch_with(&img, Some(&out), T::Address(f.b), D::Bytes("9090"), opts).is_err());
    let opts = PatchOpts { verify: true, ..Default::default() };
    assert!(patch_with(&img, Some(&out), T::Address(f.b), D::Bytes("9090"), opts)
        .unwrap_err()
        .to_string()
        .contains("--old"));
    assert!(!out.exists());
    // the right bytes pass
    let opts = PatchOpts { old: Some(&real), verify: true, ..Default::default() };
    patch_with(&img, Some(&out), T::Address(f.b), D::Bytes("9090"), opts).unwrap();
    assert_eq!(Image::open(&out).unwrap().read_at(f.b, 2).unwrap(), [0x90, 0x90]);
    assert_eq!(fs::read(input).unwrap(), f.bytes);
}

#[test]
fn in_place_keeps_a_backup_and_never_overwrites_one() {
    let dir = tempfile::tempdir().unwrap();
    let (f, input, img) = elf_input(dir.path());
    let backup = dir.path().join("input.orig");
    let opts = PatchOpts { in_place: true, ..Default::default() };
    // --out and --in-place together, and neither
    assert!(patch_with(&img, Some(&dir.path().join("o")), T::Address(f.b), D::Nop(1), opts).is_err());
    assert!(patch_with(&img, None, T::Address(f.b), D::Nop(1), PatchOpts::default()).is_err());
    let answer = patch_with(&img, None, T::Address(f.b), D::Nop(2), opts).unwrap();
    assert!(answer.text_all().contains("input.orig"), "{}", answer.text_all());
    assert_eq!(fs::read(&backup).unwrap(), f.bytes);
    assert_eq!(Image::open(&input).unwrap().read_at(f.b, 2).unwrap(), [0x90, 0x90]);
    assert!(!dir.path().join("input.nexo-tmp").exists());
    // a second one is refused and leaves both files as they are
    let patched = fs::read(&input).unwrap();
    let img = Image::open(&input).unwrap();
    let e = patch_with(&img, None, T::Address(f.b), D::Nop(3), opts).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(fs::read(&input).unwrap(), patched);
    assert_eq!(fs::read(&backup).unwrap(), f.bytes);
    // a patch file works in place the same way
    let e = patch_file(&img, None, &format!("{:#x} 90\n", f.b), opts).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
}
