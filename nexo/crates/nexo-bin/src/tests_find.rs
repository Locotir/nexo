use std::{fs, path::PathBuf};

use crate::{find_asm_in, find_seq_in, find_str_in, fixtures, Image};

/// mov eax, 0x539; call rax; nop; mov eax, 7; call rax; ret
const CODE: [u8; 16] = [0xb8, 0x39, 5, 0, 0, 0xff, 0xd0, 0x90, 0xb8, 7, 0, 0, 0, 0xff, 0xd0, 0xc3];

fn code_image() -> Image {
    Image::from_raw(CODE.to_vec(), PathBuf::from("code.bin"), 0x1000, None).unwrap()
}

#[test]
fn find_str_reports_address_section_and_the_longer_string() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixtures::elf64(false);
    let path = dir.path().join("prog");
    fs::write(&path, &f.bytes).unwrap();
    let img = Image::open(&path).unwrap();
    let text = find_str_in(&img, "nexo", false, None, 200).unwrap().text_all();
    assert!(text.contains(&format!("{:#010x}", f.string + 6)), "{text}");
    assert!(text.contains(".data") && text.contains("hello nexo bin"), "{text}");
    let none = find_str_in(&img, "no such text", false, None, 200).unwrap().text_all();
    assert!(none.contains("no match"), "{none}");
    assert!(find_str_in(&img, "", false, None, 200).is_err());
    assert!(find_str_in(&img, "nexo", false, None, 0).is_err());
}

#[test]
fn find_str_wide_also_finds_utf16le() {
    let mut data = vec![0u8; 8];
    data.extend_from_slice(b"hello\0");
    data.extend_from_slice(&[0; 2]);
    data.extend("hello".encode_utf16().flat_map(u16::to_le_bytes));
    data.extend_from_slice(&[0; 4]);
    let img = Image::from_raw(data, PathBuf::from("d.bin"), 0x2000, None).unwrap();
    let plain = find_str_in(&img, "hello", false, None, 200).unwrap();
    assert_eq!(plain.meta["hits"], 1, "{}", plain.text_all());
    assert!(plain.text_all().contains(&format!("{:#010x}", 0x2008)));
    let both = find_str_in(&img, "hello", true, None, 200).unwrap();
    assert_eq!(both.meta["hits"], 2, "{}", both.text_all());
    assert!(both.text_all().contains("wide"), "{}", both.text_all());
    assert!(both.text_all().contains(&format!("{:#010x}", 0x2010)));
}

#[test]
fn find_asm_matches_the_printed_disassembly_one_row_per_instruction() {
    let img = code_image();
    let out = find_asm_in(&img, "^mov eax, ", None, 200).unwrap();
    assert_eq!(out.meta["hits"], 2, "{}", out.text_all());
    let text = out.text_all();
    assert!(text.contains("00001000") && text.contains("00001008"), "{text}");
    assert!(text.contains("b8 39 05 00 00") && text.contains("mov eax,"), "{text}");
    assert_eq!(out.rows[0].json.as_ref().unwrap()["bytes"], "b8 39 05 00 00");
    let none = find_asm_in(&img, "^xor ", None, 200).unwrap().text_all();
    assert!(none.contains("no match"), "{none}");
    assert!(find_asm_in(&img, "(", None, 200).is_err());
}

#[test]
fn find_seq_wants_consecutive_instructions() {
    let img = code_image();
    let out = find_seq_in(&img, "mov eax, *; call *", None, 200).unwrap();
    assert_eq!(out.meta["hits"], 2, "{}", out.text_all());
    assert!(out.text_all().contains("00001000") && out.text_all().contains("00001008"));
    assert!(out.text_all().contains("b8 39 05 00 00 ff d0"), "{}", out.text_all());
    // the same pieces in the wrong order, and with an instruction between them
    for seq in ["call *; mov eax, *", "mov eax, *; nop", "mov eax, 7; call rax; ret"] {
        let n = find_seq_in(&img, seq, None, 200).unwrap();
        assert_eq!(n.meta["hits"], if seq.ends_with("ret") { 1 } else { 0 }, "{seq}: {}", n.text_all());
    }
    assert!(find_seq_in(&img, " ; ", None, 200).is_err());
}

#[test]
fn find_limit_says_how_many_matches_it_left_out() {
    let img = code_image();
    let out = find_asm_in(&img, ".", None, 2).unwrap();
    let text = out.text_all();
    assert_eq!(out.meta["hits"], 2);
    assert_eq!(out.meta["total"], 6);
    assert!(text.contains("4 more matches beyond --limit 2"), "{text}");
    assert!(out.command.contains("--limit 2"), "{}", out.command);
    let all = find_asm_in(&img, ".", None, 200).unwrap().text_all();
    assert!(!all.contains("more matches"), "{all}");
}

/// The kind (`imm`, `disp`, `data`) of every match, so a test can tell an operand match from the same bytes found as data.
fn raw_hits(code: &[u8], v: i128) -> Vec<String> {
    let img = Image::from_raw(code.to_vec(), PathBuf::from("c.bin"), 0x1000, None).unwrap();
    let out = crate::find_imm(&img, v).unwrap();
    out.rows.iter().filter_map(|r| r.json.as_ref()).map(|j| j["kind"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn find_imm_sees_a_32_bit_polynomial_in_every_form_a_compiler_emits() {
    // and eax, imm32 | xor eax, imm32 | and edx, imm32 | mov ecx, imm32 | mov r8d, imm32
    let forms: [(&str, &[u8]); 5] = [
        ("and eax", &[0x25, 0x20, 0x83, 0xb8, 0xed]),
        ("xor eax", &[0x35, 0x20, 0x83, 0xb8, 0xed]),
        ("and edx", &[0x81, 0xe2, 0x20, 0x83, 0xb8, 0xed]),
        ("mov ecx", &[0xb9, 0x20, 0x83, 0xb8, 0xed]),
        ("mov r8d", &[0x41, 0xb8, 0x20, 0x83, 0xb8, 0xed]),
    ];
    for (name, bytes) in forms {
        let mut code = bytes.to_vec();
        code.push(0xc3);
        for v in [0xEDB8_8320i128, -0x1247_7CE0] {
            let t = raw_hits(&code, v);
            assert!(t.iter().any(|k| k == "imm"), "{name} {v:#x}: {t:?}");
        }
        assert!(raw_hits(&code, 0xEDB8_8321).is_empty(), "{name}");
    }
}

#[test]
fn find_imm_matches_a_sign_extended_64_bit_operand_by_its_low_32_bits() {
    // and rdx, imm32 (sign-extended) | xor rax, imm32 | mov rcx, imm32 (c7 /0)
    let forms: [(&str, &[u8]); 3] = [
        ("and rdx", &[0x48, 0x81, 0xe2, 0x20, 0x83, 0xb8, 0xed]),
        ("xor rax", &[0x48, 0x35, 0x20, 0x83, 0xb8, 0xed]),
        ("mov rcx", &[0x48, 0xc7, 0xc1, 0x20, 0x83, 0xb8, 0xed]),
    ];
    for (name, bytes) in forms {
        let mut code = bytes.to_vec();
        code.push(0xc3);
        let t = raw_hits(&code, 0xEDB8_8320);
        assert!(t.iter().any(|k| k == "imm"), "{name}: {t:?}");
        // the full sign-extended value and the negative number are the same operand too
        for v in [0xFFFF_FFFF_EDB8_8320i128, -0x1247_7CE0] {
            assert!(raw_hits(&code, v).iter().any(|k| k == "imm"), "{name} {v:#x}");
        }
        assert!(raw_hits(&code, 0xEDB8_8321).is_empty(), "{name}");
    }
    // a positive 32-bit operand still matches its own value
    let t = raw_hits(&[0x48, 0xc7, 0xc1, 0x78, 0x56, 0x34, 0x12, 0xc3], 0x1234_5678);
    assert!(t.iter().any(|k| k == "imm"), "{t:?}");
}
