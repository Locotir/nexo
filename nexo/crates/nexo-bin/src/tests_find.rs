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
