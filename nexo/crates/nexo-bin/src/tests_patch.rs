use std::{fs, path::Path};

use crate::{assemble, fixtures, patch, Image, PatchData as D, PatchTarget as T};

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
    let built = Command::new("cc").args(["-O0", "-o"]).arg(&exe).arg(&source).output().unwrap();
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
