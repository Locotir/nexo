use std::path::PathBuf;

use crate::fixtures::{self, Fixture, IMM, STRING, TABLE};
use crate::*;

fn open(f: &Fixture) -> Image {
    Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("the fixture opens")
}

fn spans(img: &Image) -> Vec<(u64, u64)> {
    img.funcs().iter().map(|f| (f.start, f.end)).collect()
}

fn text(o: &Output) -> String {
    o.page(None, 1000).text()
}

#[test]
fn elf_info_fields() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    assert_eq!(img.format, Format::Elf);
    assert_eq!(img.arch_name(), "x86-64");
    assert_eq!(img.entry, f.a);
    assert_eq!(img.sha256.len(), 64);
    let names: Vec<&str> = img.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [".text", ".eh_frame", ".data", ".symtab", ".strtab", ".shstrtab"]);
    let t = &img.sections[0];
    assert_eq!((t.addr, t.size, t.flags().as_str()), (f.a, 35, "r-x"));
    assert_eq!(t.index, 1);
    assert_eq!(img.sections[2].flags(), "rw-");
    let o = info(&img);
    assert_eq!(o.meta["format"], "ELF64");
    assert_eq!(o.meta["counts"]["functions"], 3);
    assert_eq!(o.meta["counts"]["strings"], 1);
    assert_eq!(o.meta["counts"]["symbols"], 2);
    assert_eq!(o.meta["hardening"]["pie"], "no (fixed base)");
    assert_eq!(o.meta["hardening"]["nx_stack"], "yes");
    assert_eq!(o.meta["hardening"]["relro"], "none");
    let t = text(&o);
    assert!(t.contains("ELF64 little-endian, x86-64"), "{t}");
    assert!(t.contains("entry       0x400100 (main)"), "{t}");
    assert!(t.contains("pie: no (fixed base) | nx stack: yes"), "{t}");
}

#[test]
fn pe_info_fields() {
    let f = fixtures::pe64(false);
    let img = open(&f);
    assert_eq!(img.format, Format::Pe);
    assert_eq!(img.image_base, 0x1_4000_0000);
    assert_eq!(img.entry, f.a);
    let names: Vec<&str> = img.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, [".text", ".rdata", ".data", ".idata", ".pdata"]);
    assert_eq!(img.sections[0].flags(), "r-x");
    assert_eq!(img.imports.len(), 1);
    assert_eq!(img.imports[0].name, "ExitProcess");
    assert_eq!(img.imports[0].lib.as_deref(), Some("KERNEL32.dll"));
    assert_eq!(img.imports[0].slot, Some(f.image_base + 0x4050));
    let o = info(&img);
    assert_eq!(o.meta["format"], "PE32+");
    assert_eq!(o.meta["counts"]["imports"], 1);
    assert_eq!(o.meta["hardening"]["aslr"], "yes");
    assert_eq!(o.meta["hardening"]["dep"], "yes");
    assert_eq!(o.meta["hardening"]["cfg"], "no");
}

#[test]
fn function_starts_and_ends() {
    for f in [fixtures::elf64(false), fixtures::pe64(false)] {
        let img = open(&f);
        assert_eq!(spans(&img), vec![(f.a, f.b), (f.b, f.c), (f.c, f.text_end)]);
    }
}

#[test]
fn exception_data_finds_a_function_nothing_calls() {
    for f in [fixtures::elf64(true), fixtures::pe64(true)] {
        let img = open(&f);
        let d = f.d.expect("a hidden function");
        assert_eq!(spans(&img), vec![(f.a, f.b), (f.b, f.c), (f.c, d), (d, f.text_end)]);
    }
    // without it in the exception data, the same bytes would not show D: the plain files have 3
    assert_eq!(open(&fixtures::elf64(false)).funcs().len(), 3);
}

#[test]
fn symbols_and_imports_name_things() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    assert_eq!(img.name_at(f.a), Some("main"));
    assert_eq!(img.funcs()[1].name, "helper");
    assert_eq!(img.funcs()[2].name, format!("sub_{:x}", f.c));
    let p = fixtures::pe64(false);
    let img = open(&p);
    assert_eq!(img.name_at(p.image_base + 0x4050), Some("ExitProcess"));
}

#[test]
fn address_and_offset_go_both_ways() {
    for f in [fixtures::elf64(false), fixtures::pe64(false)] {
        let img = open(&f);
        for a in [f.a, f.b + 3, f.string, f.table + 5] {
            let off = img.addr_to_offset(a).expect("mapped");
            assert_eq!(img.offset_to_addr(off), Some(a));
        }
        assert_eq!(img.addr_to_offset(f.image_base + 0x50_0000), None);
        let off = img.addr_to_offset(f.table).unwrap() as usize;
        assert_eq!(&img.data[off..off + 4], &TABLE[0].to_le_bytes());
    }
}

#[test]
fn fn_text_names_the_callee_and_the_string() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    let t = text(&func(&img, "main", false).unwrap());
    assert!(t.starts_with(&format!("fn main {:#x}..{:#x} (16 bytes, 6 instructions)", f.a, f.b)), "{t}");
    assert!(t.contains("call 0x400110  ; helper"), "{t}");
    assert!(t.contains(&format!("call {:#x}  ; sub_{:x}", f.c, f.c)), "{t}");
    // an address inside the function finds it too; bytes are off by default and on with the flag
    let t = text(&func(&img, &format!("{:#x}", f.b + 8), false).unwrap());
    assert!(t.contains("lea rdi, [0x") && t.contains(&format!("; \"{STRING}\"")), "{t}");
    assert!(!t.contains("48 8d 3d"));
    let t = text(&func(&img, "helper", true).unwrap());
    assert!(t.contains("48 8d 3d"), "{t}");
    assert!(t.contains(&format!("mov eax, {IMM:#x}")) || t.contains(&format!("mov eax, {IMM}")), "{t}");
    let p = fixtures::pe64(false);
    let img = open(&p);
    let t = text(&func(&img, &format!("{:#x}", p.c), false).unwrap());
    assert!(t.contains("; ExitProcess"), "{t}");
    let t = text(&func(&img, &format!("{:#x}", p.b), false).unwrap());
    assert!(t.contains(&format!("; \"{STRING}\"")), "{t}");
}

#[test]
fn fn_asks_fail_with_a_reason() {
    let img = open(&fixtures::elf64(false));
    let e = func(&img, "nothing_like_it", false).unwrap_err().to_string();
    assert!(e.contains("no function is named"), "{e}");
    let e = func(&img, "0x400200", false).unwrap_err().to_string();
    assert!(e.contains("data, not code"), "{e}");
    let e = func(&img, "0x10", false).unwrap_err().to_string();
    assert!(e.contains("not inside any section"), "{e}");
    let pe = open(&fixtures::pe64(false));
    let e = func(&pe, "sub_", false).unwrap_err().to_string();
    assert!(e.contains("3 functions match"), "{e}");
}

#[test]
fn bytes_as_u32_shows_the_table() {
    for f in [fixtures::elf64(false), fixtures::pe64(false)] {
        let img = open(&f);
        let o = bytes(&img, f.table, 16, ElemType::U32).unwrap();
        let t = text(&o);
        assert_eq!(t, format!("{:#010x}  0x0000000a 0x00000014 0x0000001e 0x12345678", f.table));
        assert_eq!(o.page(None, 50).value["items"][0]["values"], serde_json::json!([10, 20, 30, 0x1234_5678u32]));
    }
    let img = open(&fixtures::elf64(false));
    let f = fixtures::elf64(false);
    let t = text(&bytes(&img, f.table, 16, ElemType::I32).unwrap());
    assert!(t.contains("20") && t.contains("305419896"), "{t}");
    let t = text(&bytes(&img, f.table, 20, ElemType::U8).unwrap());
    assert_eq!(t.lines().count(), 2);
    assert!(t.lines().next().unwrap().ends_with('|'), "{t}");
    let t = text(&bytes(&img, f.string, 15, ElemType::Str).unwrap());
    assert_eq!(t, format!("{:#010x}  \"{STRING}\"", f.string));
    let t = text(&bytes(&img, f.table, 5, ElemType::U32).unwrap());
    assert!(t.contains("1 trailing bytes not shown"), "{t}");
    // a pointer shows what it points to
    let mut data = f.bytes.clone();
    let off = img.addr_to_offset(f.table).unwrap() as usize;
    data[off..off + 8].copy_from_slice(&f.b.to_le_bytes());
    data[off + 8..off + 16].copy_from_slice(&f.string.to_le_bytes());
    let img2 = Image::from_bytes(data, PathBuf::from("/fx/prog")).unwrap();
    let t = text(&bytes(&img2, f.table, 16, ElemType::Ptr).unwrap());
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].ends_with("helper"), "{t}");
    assert!(lines[1].ends_with(&format!("\"{STRING}\"")), "{t}");
}

#[test]
fn bytes_refuses_what_it_cannot_read() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    assert!(bytes(&img, 0x10, 4, ElemType::U8).unwrap_err().to_string().contains("not inside any section"));
    assert!(bytes(&img, f.table, 0, ElemType::U8).is_err());
    assert!(bytes(&img, f.table, u64::MAX, ElemType::U8).unwrap_err().to_string().contains("too many"));
    assert!(ElemType::parse("u128").is_err());
    // the data section ends 31 bytes in: asking for more says where the bytes stop
    let t = text(&bytes(&img, f.table, 64, ElemType::U8).unwrap());
    assert!(t.contains("the file holds no more bytes after"), "{t}");
}

#[test]
fn strings_grep() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    let o = strings(&img, Some("nexo")).unwrap();
    assert_eq!(text(&o), format!("{:#010x}  \"{STRING}\"", f.string));
    assert_eq!(o.meta["matched"], 1);
    let t = text(&strings(&img, Some("^zzz$")).unwrap());
    assert!(t.starts_with("no string matches"), "{t}");
    assert!(strings(&img, Some("(")).unwrap_err().to_string().contains("bad regular expression"));
    let p = fixtures::pe64(false);
    let img = open(&p);
    assert_eq!(text(&strings(&img, Some("hello")).unwrap()), format!("{:#010x}  \"{STRING}\"", p.string));
}

#[test]
fn strings_finds_utf16() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    let mut data = f.bytes.clone();
    let off = img.addr_to_offset(f.table).unwrap() as usize;
    for (i, c) in "Wide!".chars().enumerate() {
        data[off + 2 * i] = c as u8;
        data[off + 2 * i + 1] = 0;
    }
    let img = Image::from_bytes(data, PathBuf::from("/fx/prog")).unwrap();
    let t = text(&strings(&img, Some("Wide")).unwrap());
    assert_eq!(t, format!("{:#010x}  L\"Wide!\"", f.table));
}

#[test]
fn find_bytes_and_imm() {
    for f in [fixtures::elf64(false), fixtures::pe64(false)] {
        let img = open(&f);
        let t = text(&find_bytes(&img, "48 8d 3d").unwrap());
        assert!(t.starts_with(&format!("{:#010x}  .text", f.b)), "{t}");
        assert!(t.contains(" 48 8d 3d"), "{t}");
        // the holding function is named, wildcards work
        let t2 = text(&find_bytes(&img, "48 ?? 3d").unwrap());
        assert!(t2.starts_with(&format!("{:#010x}", f.b)), "{t2}");
        assert_eq!(text(&find_bytes(&img, "48 8d 3d").unwrap()).lines().count(), 1);
        let t = text(&find_imm(&img, i128::from(IMM)).unwrap());
        assert!(t.starts_with(&format!("{:#010x}  .text", f.b + 7)), "{t}");
        assert!(t.contains("mov eax"), "{t}");
        // the same number as a u32 in the data
        let t = text(&find_imm(&img, 0x1234_5678).unwrap());
        assert!(t.contains("data u32") && t.contains(&format!("{:#010x}", f.table + 12)), "{t}");
        assert!(text(&find_imm(&img, 0x7777_7777).unwrap()).starts_with("no match"));
    }
    let img = open(&fixtures::elf64(false));
    let t = text(&find_bytes(&img, "55").unwrap());
    assert!(t.contains("main"), "{t}");
    assert!(find_bytes(&img, "4").is_err());
    assert!(find_bytes(&img, "zz").is_err());
    assert!(find_bytes(&img, "?? ??").is_err());
    assert!(find_imm(&img, 1 << 70).is_err());
}

#[test]
fn find_imm_sees_negative_and_wide_numbers() {
    let f = fixtures::elf64(false);
    let img = open(&f);
    // `mov eax, 1337` holds 1337 as a 32-bit immediate, so -1337 as i32 is the same bits: 0xfffffac7 is not it
    assert!(text(&find_imm(&img, -1337).unwrap()).starts_with("no match"));
    assert!(text(&find_imm(&img, 0xdead_beef_1234).unwrap()).starts_with("no match"));
}

#[test]
fn a_cut_says_what_is_left_and_the_pieces_join_to_the_whole() {
    let rows: Vec<Row> = (0..300u64)
        .map(|i| Row { key: 0x1000 + i * 3, text: format!("line {i}"), json: Some(serde_json::json!(i)) })
        .collect();
    let o = Output { op: "fn", meta: serde_json::json!({}), rows, command: "nexo bin fn /x/prog 0x1000".into() };
    let whole = o.text_all();
    for budget in [10usize, 25, 120, 299, 300, 301] {
        let mut joined: Vec<String> = Vec::new();
        let mut from = None;
        let mut pieces = 0;
        loop {
            let p = o.page(from, budget);
            pieces += 1;
            assert!(p.lines.len() <= budget.max(MIN_LINES), "budget {budget}");
            assert!(pieces < 400);
            match &p.next {
                Some(cmd) => {
                    let (body, last) = p.lines.split_at(p.lines.len() - 1);
                    joined.extend(body.iter().cloned());
                    let expect = format!("... {} more lines: {cmd}", p.left);
                    assert_eq!(last[0], expect);
                    assert!(cmd.starts_with("nexo bin fn /x/prog 0x1000 --from 0x"), "{cmd}");
                    assert_eq!(cmd.contains("--lines"), budget != DEFAULT_LINES);
                    let key = cmd.split("--from ").nth(1).unwrap().split(' ').next().unwrap();
                    from = Some(parse_addr(key).unwrap());
                }
                None => {
                    joined.extend(p.lines.iter().cloned());
                    assert_eq!(p.left, 0);
                    break;
                }
            }
        }
        assert_eq!(joined.join("\n"), whole, "budget {budget}");
    }
    // JSON takes the same cut
    let p = o.page(None, 20);
    assert_eq!(p.value["items"].as_array().unwrap().len(), 19);
    assert_eq!(p.value["left"], 281);
    assert_eq!(p.value["next"], p.next.clone().unwrap());
    // past the end
    let p = o.page(Some(0xffff), 20);
    assert!(p.lines[0].starts_with("... nothing at or after 0xffff"), "{:?}", p.lines);
}

/// A function in the test binary, cut in pieces, equals the whole.
fn pieces_equal_whole(img: &Image, f: &crate::funcs::Func) {
    let target = format!("{:#x}", f.start);
    let o = func(img, &target, false).unwrap();
    let whole = o.text_all();
    let mut joined = Vec::new();
    let mut from = None;
    for _ in 0..10_000 {
        let p = o.page(from, 30);
        match p.next {
            Some(cmd) => {
                joined.extend(p.lines[..p.lines.len() - 1].iter().cloned());
                from = Some(parse_addr(cmd.split("--from ").nth(1).unwrap().split(' ').next().unwrap()).unwrap());
            }
            None => {
                joined.extend(p.lines);
                break;
            }
        }
    }
    assert_eq!(joined.join("\n"), whole);
}

#[test]
fn a_long_real_function_is_cut_and_joined_exactly() {
    let Some(img) = this_binary() else { return };
    let f = img.funcs().iter().filter(|f| f.size() < 200_000).max_by_key(|f| f.size()).expect("functions");
    assert!(f.size() > 500, "the biggest function is {} bytes", f.size());
    pieces_equal_whole(&img, f);
}

/// The test binary itself, unless it is not x86-64 (then the reason is printed and the test passes).
fn this_binary() -> Option<Image> {
    let exe = std::env::current_exe().expect("current exe");
    match Image::open(&exe) {
        Ok(img) => Some(img),
        Err(e @ Error::NotRead(_)) => {
            eprintln!("skipped: the test binary is not an x86-64 ELF or PE ({e})");
            None
        }
        Err(e) => panic!("cannot open the test binary: {e}"),
    }
}

const MARKER: &str = "nexo-bin-smoke-marker-7f3a9c41";

#[test]
fn smoke_on_the_test_binary() {
    let Some(img) = this_binary() else { return };
    let marker = std::hint::black_box(MARKER);
    assert_eq!(img.arch_name(), "x86-64");
    let o = info(&img);
    assert!(o.meta["counts"]["functions"].as_u64().unwrap() > 100, "{}", o.meta["counts"]);
    let o = strings(&img, Some("nexo-bin-smoke-marker-7f3a9c41")).unwrap();
    let p = o.page(None, 120);
    let hit = p.value["items"].as_array().unwrap().first().expect("the marker string is found");
    let addr = hit["addr"].as_u64().unwrap();
    let held = img.read_at(addr, 4096).unwrap();
    assert!(held.windows(marker.len()).any(|w| w == marker.as_bytes()));
    // `fn` at the entry decodes inside the bound
    let o = func(&img, &format!("{:#x}", img.entry), false).unwrap();
    let p = o.page(None, 120);
    assert!(p.lines.len() <= 120);
    assert!(p.lines[0].starts_with("fn "), "{}", p.lines[0]);
    assert!(p.lines.len() > 1);
}

#[test]
fn other_files_are_named_not_read() {
    let e = Image::from_bytes(b"just some text\nfor a file\n".to_vec(), "/x".into()).err().unwrap();
    assert!(matches!(e, Error::NotRead(_)) && e.exit_code() == 2);
    assert!(e.to_string().contains("text file"), "{e}");
    assert!(Image::from_bytes(Vec::new(), "/x".into()).err().unwrap().to_string().contains("empty"));
    // Mach-O arm64: magic, cpu type arm64, file type executable, no commands
    let mut macho = vec![0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    macho.resize(32, 0);
    macho[12] = 2;
    let e = Image::from_bytes(macho, "/x".into()).err().unwrap();
    assert_eq!(e.to_string(), "Mach-O arm64 is not read yet");
    // an ELF for another machine
    let mut elf = fixtures::elf64(false).bytes;
    elf[18] = 183; // EM_AARCH64
    let e = Image::from_bytes(elf, "/x".into()).err().unwrap();
    assert_eq!(e.to_string(), "ELF64 arm64 is not read yet (only x86-64 is)");
    // a PE for ARM64
    let mut pe = fixtures::pe64(false).bytes;
    pe[0x44] = 0x64;
    pe[0x45] = 0xaa;
    let e = Image::from_bytes(pe, "/x".into()).err().unwrap();
    assert!(e.to_string().ends_with("is not read yet (only x86-64 is)"), "{e}");
    // 32-bit ELF
    let mut elf32 = fixtures::elf64(false).bytes;
    elf32[4] = 1;
    assert!(Image::from_bytes(elf32, "/x".into()).err().unwrap().to_string().contains("is not read yet"));
}

/// Every op on whatever opened. Nothing may panic.
fn ask_everything(img: &Image) {
    let _ = info(img).page(None, 50);
    for f in img.funcs().iter().take(8) {
        let _ = func(img, &format!("{:#x}", f.start), true).map(|o| o.page(None, 50));
    }
    let _ = strings(img, Some("e")).map(|o| o.page(None, 50));
    let _ = find_bytes(img, "00 00").map(|o| o.page(None, 50));
    let _ = find_imm(img, 1337).map(|o| o.page(None, 50));
    for s in img.sections.iter().take(8) {
        for ty in [ElemType::U8, ElemType::Ptr, ElemType::Str, ElemType::F64] {
            let _ = bytes(img, s.addr, 64, ty);
        }
        let _ = img.addr_to_offset(s.addr.wrapping_add(s.size));
        let _ = img.offset_to_addr(s.file_off);
    }
}

#[test]
fn truncated_files_give_ok_or_err_never_a_panic() {
    for f in [fixtures::elf64(false), fixtures::elf64(true), fixtures::pe64(false), fixtures::pe64(true)] {
        let n = f.bytes.len();
        for i in 0..64 {
            let cut = n * i / 63;
            if let Ok(img) = Image::from_bytes(f.bytes[..cut].to_vec(), "/x".into()) {
                ask_everything(&img);
            }
        }
    }
}

#[test]
fn damaged_headers_give_ok_or_err_never_a_panic() {
    for f in [fixtures::elf64(true), fixtures::pe64(true)] {
        let n = f.bytes.len();
        for round in 0..3u8 {
            for i in 0..n.min(0x500) {
                let mut data = f.bytes.clone();
                data[i] ^= [0xff, 0x80, 0x01][round as usize];
                if let Ok(img) = Image::from_bytes(data, "/x".into()) {
                    ask_everything(&img);
                }
            }
        }
        // the header fields that hold sizes and counts, set to the largest values
        for i in (0..n.min(0x500)).step_by(4) {
            let mut data = f.bytes.clone();
            for b in data.iter_mut().skip(i).take(4) {
                *b = 0xff;
            }
            if let Ok(img) = Image::from_bytes(data, "/x".into()) {
                ask_everything(&img);
            }
        }
    }
}

#[test]
fn numbers_and_addresses_parse() {
    assert_eq!(parse_number("0x10").unwrap(), 16);
    assert_eq!(parse_number("-5").unwrap(), -5);
    assert_eq!(parse_number("1_000").unwrap(), 1000);
    assert!(parse_number("12h").is_err());
    assert!(parse_number("0x1ffffffffffffffff").is_err());
    assert_eq!(parse_addr("0x400000").unwrap(), 0x40_0000);
    assert!(parse_addr("-1").is_err());
}
