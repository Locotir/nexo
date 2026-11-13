//! Review fixes: imports by name, long lists, what was cut or not resolved.

use super::*;

use crate::refs::{Kind, Ref, Refs, SCANNED, TEST_MAX};
use crate::xops::BUILT;

fn last_line(o: &Output) -> String {
    whole(o).lines().last().unwrap_or("").to_string()
}

/// A real dynamic program of the machine, or None (the test is then skipped with a note).
fn system_file(path: &str) -> Option<Image> {
    match Image::open(Path::new(path)) {
        Ok(i) => Some(i),
        Err(e) => {
            eprintln!("skipped: {path}: {e}");
            None
        }
    }
}

#[test]
fn xref_of_an_import_name_answers_with_the_plt_call_sites() {
    let Some(img) = system_file("/usr/bin/ls") else { return };
    let funcs = img.funcs();
    // an import with a PLT stub that has callers
    let (name, stub, slot) = img
        .imports
        .iter()
        .filter_map(|i| {
            let f = funcs.iter().find(|f| f.name == format!("{}@plt", i.name))?;
            Some((i.name.clone(), f.start, i.slot?))
        })
        .find(|&(_, stub, _)| img.refs().at(stub).iter().filter(|r| r.kind == Kind::Call).count() >= 2)
        .expect("ls has an imported function called from at least two places");
    let calls_at_stub = img.refs().at(stub).iter().filter(|r| r.kind == Kind::Call).count();
    let slot_refs = img.refs().at(slot).len();
    assert!(slot_refs >= 1, "the stub's own jump refers to the slot");
    let o = xref(&img, &name).unwrap();
    let t = whole(&o);
    assert!(t.contains(&format!("{name}@plt")), "the answer is the stub: {t}");
    assert_eq!(o.meta["counts"]["call"].as_u64().unwrap() as usize, calls_at_stub, "{t}");
    assert_eq!(o.meta["references"].as_u64().unwrap() as usize, img.refs().at(stub).len() + slot_refs, "{t}");
    // the same answer when the stub is named, and its own address
    assert_eq!(whole(&xref(&img, &format!("{name}@plt")).unwrap()), t);
    assert_eq!(whole(&xref(&img, &hex(stub)).unwrap()), t);
}

/// 300 000 references to one place, made by hand, in the first section.
fn long_list(img: &Image, target: u64, n: u64) -> Refs {
    let src0 = img.code_sections().next().unwrap().addr;
    Refs::for_test((0..n).map(|i| Ref { target, src: src0 + i * 4, kind: Kind::Read }).collect())
}

#[test]
fn xref_str_with_many_occurrences_and_references_copies_nothing() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let n = 5000u64;
    img.set_refs_for_test(long_list(&img, f.string, n));
    BUILT.with(|c| c.set(0));
    // "o" is in "hello nexo bin" twice: the old code copied the whole list for each
    let o = xref_str(&img, "o", false).unwrap();
    assert_eq!(o.meta["references"], n);
    assert!(whole(&o).contains(&format!("({n} references)")), "{}", &whole(&o)[..300.min(whole(&o).len())]);
    assert_eq!(BUILT.with(|c| c.get()), 0, "the list is not copied per occurrence");
}

#[test]
fn a_page_of_xref_costs_the_page_not_the_list() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let n = 300_000u64;
    img.set_refs_for_test(long_list(&img, f.global.unwrap(), n));
    BUILT.with(|c| c.set(0));
    let g = hex(f.global.unwrap());
    let o = xref_window(&img, &g, None, 10).unwrap();
    assert_eq!(o.meta["references"], n);
    assert!(o.rows.len() <= 10 && o.tail as u64 >= n - 10, "{} rows, {} left", o.rows.len(), o.tail);
    // deep in the list: the page starts there and ends with the right row
    let o = xref_window(&img, &g, Some(n - 3), 10).unwrap();
    assert_eq!(o.rows.len(), 5, "the last five lines of the answer: {}", o.rows.len());
    assert_eq!(o.tail, 0);
    assert_eq!(o.rows[0].key, n - 3);
    let src_last = img.code_sections().next().unwrap().addr + (n - 1) * 4;
    assert!(o.rows[4].text.contains(&format!("{src_last:#010x}")), "{}", o.rows[4].text);
    assert_eq!(BUILT.with(|c| c.get()), 0, "no per-reference list is built");
    // xref --str the same
    let o = xref_str_window(&img, "hello", false, None, 10);
    assert!(o.unwrap().rows.len() <= 10);
}

#[test]
fn the_pointer_scan_stops_when_the_list_is_full() {
    let f = fixtures::elf64_xref();
    let words = |max: usize| {
        TEST_MAX.with(|c| c.set(max));
        SCANNED.with(|c| c.set(0));
        let img = open(&f);
        let _ = img.refs();
        let n = SCANNED.with(|c| c.get());
        TEST_MAX.with(|c| c.set(crate::MAX_REFS));
        n
    };
    let all = words(crate::MAX_REFS);
    let code = open(&f).refs().len() - 2; // the two table pointers
    let cut = words(code);
    assert!(all > 0 && cut < all, "full scan {all} words, cut scan {cut}");
}

#[test]
fn a_cut_is_said_and_code_references_are_kept_before_data_pointers() {
    let f = fixtures::elf64_xref();
    let (full_len, code) = {
        let img = open(&f);
        let r = img.refs();
        assert!(!r.capped && !r.capped_code);
        (r.len(), r.len() - 2)
    };
    let with_max = |max: usize| {
        TEST_MAX.with(|c| c.set(max));
        let img = open(&f);
        let _ = img.refs();
        TEST_MAX.with(|c| c.set(crate::MAX_REFS));
        img
    };
    // room for every code reference and one data pointer
    let img = with_max(code + 1);
    let r = img.refs();
    assert_eq!(r.len(), code + 1);
    assert_eq!((r.capped, r.capped_code), (true, false));
    assert_eq!(r.at(f.b).iter().filter(|x| x.kind == Kind::Call).count(), 2, "code kept");
    let line = last_line(&xref(&img, &hex(f.b)).unwrap());
    assert!(line.contains("cut at") && line.contains("all code references are kept"), "{line}");
    assert!(last_line(&calls(&img, &hex(f.a)).unwrap()).contains("cut at"));
    assert!(last_line(&xref_str(&img, "nexo", false).unwrap()).contains("cut at"));
    assert!(last_line(&func_list(&img, FuncSort::Addr, None, 0).unwrap()).contains("cut at"));
    // no room for all the code
    let img = with_max(code - 2);
    let r = img.refs();
    assert!(r.capped && r.capped_code && r.len() == code - 2, "{} of {full_len}", r.len());
    assert_eq!(r.at(f.ptrs.unwrap()).len() + r.at(f.b).iter().filter(|x| x.kind == Kind::Ptr).count(), 0);
    let line = last_line(&xref(&img, &hex(f.b)).unwrap());
    assert!(line.contains("some are missing") && line.contains("code references come first"), "{line}");
    // an uncut list says nothing
    let img = open(&f);
    assert!(!whole(&xref(&img, &hex(f.b)).unwrap()).contains("cut at"));
}

#[test]
fn indirect_calls_and_jumps_are_counted_and_said() {
    let f = fixtures::elf64_xref();
    let patched = |bytes: [u8; 2]| {
        let mut d = f.bytes.clone();
        let off = Image::from_bytes(d.clone(), "/x".into()).unwrap().addr_to_offset(f.d.unwrap()).unwrap() as usize;
        d[off..off + 2].copy_from_slice(&bytes);
        Image::from_bytes(d, "/x".into()).unwrap()
    };
    let img = patched([0xff, 0xd0]); // call rax
    assert_eq!((img.refs().indirect_calls, img.refs().indirect_jumps), (1, 0));
    assert_eq!(last_line(&xref(&img, &hex(f.b)).unwrap()), "1 indirect call is not resolved");
    assert_eq!(last_line(&calls(&img, &hex(f.a)).unwrap()), "1 indirect call is not resolved");
    let img = patched([0xff, 0xe0]); // jmp rax
    assert_eq!(last_line(&xref(&img, &hex(f.b)).unwrap()), "1 indirect jump is not resolved");
    // nothing indirect: no line
    let img = open(&f);
    assert!(!whole(&xref(&img, &hex(f.b)).unwrap()).contains("not resolved"));
    // both, with their numbers
    let img = open(&f);
    let mut r = Refs::for_test(vec![Ref { target: f.b, src: f.a + 4, kind: Kind::Call }]);
    r.indirect_calls = 12;
    r.indirect_jumps = 3;
    img.set_refs_for_test(r);
    assert_eq!(last_line(&xref(&img, &hex(f.b)).unwrap()), "12 indirect calls and 3 indirect jumps are not resolved");
    assert_eq!(last_line(&calls(&img, &hex(f.a)).unwrap()), "12 indirect calls and 3 indirect jumps are not resolved");
}

#[test]
fn relocations_that_store_a_pointer_into_the_image_are_data_pointers() {
    use object::elf::{R_X86_64_64, R_X86_64_GLOB_DAT, R_X86_64_IRELATIVE, R_X86_64_RELATIVE};
    assert_eq!(crate::image::reloc_pointer(R_X86_64_RELATIVE, None, 0x2000), Some(0x2000));
    assert_eq!(crate::image::reloc_pointer(R_X86_64_IRELATIVE, None, 0x2000), Some(0x2000));
    assert_eq!(crate::image::reloc_pointer(R_X86_64_64, Some(0x3000), 0x10), Some(0x3010));
    assert_eq!(crate::image::reloc_pointer(R_X86_64_64, None, 0x10), None, "an import is not in the image");
    assert_eq!(crate::image::reloc_pointer(R_X86_64_GLOB_DAT, Some(0x3000), 0), None);
    // a real library holds all three kinds
    let Some(img) = system_file("/usr/lib/libc.so.6") else { return };
    let data = std::fs::read("/usr/lib/libc.so.6").unwrap();
    use object::{Object, ObjectSymbol, ObjectSymbolTable};
    let elf = object::read::elf::ElfFile64::<object::Endianness>::parse(&*data).unwrap();
    let (mut rel, mut irel, mut abs) = (0usize, 0usize, 0usize);
    for (_, r) in elf.dynamic_relocations().unwrap() {
        let object::RelocationFlags::Elf { r_type } = r.flags() else { continue };
        let defined = match r.target() {
            object::RelocationTarget::Symbol(i) => {
                elf.dynamic_symbol_table().and_then(|t| t.symbol_by_index(i).ok()).is_some_and(|s| !s.is_undefined())
            }
            _ => false,
        };
        match r_type {
            R_X86_64_RELATIVE => rel += 1,
            R_X86_64_IRELATIVE => irel += 1,
            R_X86_64_64 if defined => abs += 1,
            _ => {}
        }
    }
    assert!(irel > 0, "libc has IRELATIVE relocations");
    assert_eq!(
        img.rel_ptrs.len(),
        rel + irel + abs,
        "relative {rel}, irelative {irel}, 64 against a defined symbol {abs}"
    );
}

#[test]
fn a_name_that_would_be_read_as_an_address_is_refused() {
    let f = fixtures::pe64_xref();
    let dir = data_dir();
    let img = open(&f);
    for bad in ["123", "0", "0x401000", "0XAB", "0x", "00777"] {
        let e = name_set(&img, dir.path(), &hex(f.b), bad).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{bad}");
        assert!(e.to_string().contains("read as an address"), "{bad}: {e}");
    }
    for ok in ["n123", "a0x", "x1", "_0"] {
        assert!(name_set(&img, dir.path(), &hex(f.b), ok).is_ok(), "{ok}");
    }
}

/// The xref ELF with the 14 bytes of F replaced, and F's address.
fn with_f(f: &Fixture, first: &[u8], rest: &[u8]) -> (Image, u64) {
    let at = f.f.unwrap();
    let mut d = f.bytes.clone();
    let off = Image::from_bytes(d.clone(), "/x".into()).unwrap().addr_to_offset(at).unwrap() as usize;
    let mut code = first.to_vec();
    code.extend_from_slice(&(f.table as u32).to_le_bytes());
    code.extend_from_slice(rest);
    assert_eq!(code.len(), 14, "F is 14 bytes");
    d[off..off + 14].copy_from_slice(&code);
    (Image::from_bytes(d, "/x".into()).unwrap(), at)
}

#[test]
fn an_address_written_inside_a_memory_operand_is_a_reference() {
    let f = fixtures::elf64_xref();
    let kinds = |img: &Image, at: u64| -> Vec<Kind> {
        img.refs().at(f.table).iter().filter(|r| r.src >= at && r.src < at + 14).map(|r| r.kind).collect()
    };
    let nops_ret: [u8; 7] = [0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0xc3];
    // mov rax, [rdx+table]: the table with a register added
    let (img, at) = with_f(&f, &[0x48, 0x8b, 0x82], &nops_ret);
    assert_eq!(kinds(&img, at), [Kind::Base]);
    let t = whole(&xref(&img, &hex(f.table)).unwrap());
    assert!(t.contains("used with a register added (1)"), "{t}");
    assert!(t.contains(&format!("[rdx+{:#x}]", f.table)), "{t}");
    // the number is found by find --imm too
    let o = find_imm(&img, i128::from(f.table)).unwrap();
    let row = whole(&o).lines().find(|l| l.starts_with(&format!("{at:#010x}"))).map(str::to_string);
    assert!(row.as_deref().is_some_and(|l| l.contains(&format!("[rdx+{:#x}]", f.table))), "{}", whole(&o));
    // mov eax, [table] and mov [table], eax: no register, so a plain read and a plain write
    let (img, at) = with_f(&f, &[0x8b, 0x04, 0x25], &nops_ret);
    assert_eq!(kinds(&img, at), [Kind::Read]);
    let (img, at) = with_f(&f, &[0x89, 0x04, 0x25], &nops_ret);
    assert_eq!(kinds(&img, at), [Kind::Write]);
    // jmp [rax*8+table]: a jump table. The table is named and the jump still counts as not resolved
    let (img, at) = with_f(&f, &[0xff, 0x24, 0xc5], &[0xcc; 7]);
    assert_eq!(kinds(&img, at), [Kind::Base]);
    assert_eq!(img.refs().indirect_jumps, 1);
    // mov rax, fs:[table] is thread storage, not the table
    let (img, at) = with_f(&f, &[0x64, 0x48, 0x8b, 0x04, 0x25], &[0x90, 0x90, 0x90, 0x90, 0xc3]);
    assert_eq!(kinds(&img, at), []);
    // mov rax, [rsp+table] is a stack slot
    let (img, at) = with_f(&f, &[0x48, 0x8b, 0x84, 0x24], &[0x90, 0x90, 0x90, 0x90, 0x90, 0xc3]);
    assert_eq!(kinds(&img, at), []);
}
