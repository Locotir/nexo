//! Tests of answers read piece by piece, pages, and the single decode pass for references and functions.

use super::*;

#[test]
fn every_new_op_joins_from_pieces_to_the_whole() {
    for (name, f) in both() {
        let img = open(&f);
        let budgets = [10usize, 11, 12, 13, 25];
        pieces_join(&format!("{name} xref"), |from, b| xref_window(&img, &hex(f.b), from, b).unwrap(), &budgets);
        pieces_join(
            &format!("{name} xref global"),
            |from, b| xref_window(&img, &hex(f.global.unwrap()), from, b).unwrap(),
            &budgets,
        );
        pieces_join(
            &format!("{name} xref --str"),
            |from, b| xref_str_window(&img, "o", false, from, b).unwrap(),
            &budgets,
        );
        pieces_join(
            &format!("{name} funcs"),
            |from, b| func_list_window(&img, FuncSort::Callers, None, 0, from, b).unwrap(),
            &budgets,
        );
        pieces_join(&format!("{name} calls"), |from, b| calls_window(&img, &hex(f.a), from, b).unwrap(), &budgets);
        pieces_join(
            &format!("{name} strings"),
            |from, b| strings_window(&img, None, StrSort::Addr, from, b).unwrap(),
            &budgets,
        );
        pieces_join(
            &format!("{name} strings refs"),
            |from, b| strings_window(&img, None, StrSort::Refs, from, b).unwrap(),
            &budgets,
        );
        pieces_join(&format!("{name} names"), |from, b| names_list_window(&img, from, b), &budgets);
        pieces_join(
            &format!("{name} fn"),
            |from, b| func_window(&img, &hex(f.e.unwrap()), false, from, b).unwrap(),
            &budgets,
        );
        let to = hex(f.f.unwrap() + 14);
        pieces_join(
            &format!("{name} fn --to"),
            |from, b| func_view(&img, &FnQuery { to: Some(&to), from, budget: b, ..FnQuery::new(&hex(f.a)) }).unwrap(),
            &budgets,
        );
    }
    // the same on a real binary, where the lists are long
    let Some(img) = this_binary() else { return };
    let hot = img.funcs().iter().max_by_key(|f| img.refs().callers(f.start)).unwrap().start;
    assert!(img.refs().callers(hot) > 10, "the busiest function has {} callers", img.refs().callers(hot));
    let budgets = [10usize, 37, 120];
    let img = &img;
    // Every page of the whole list is cut again from the whole list, so the walk costs (pages x functions): on a
    // test binary with tens of thousands of functions, 10-line pages took minutes. The page sizes
    // follow the list instead, which still walks 40 and 4 pages of it; the 10-line edge is on the fixtures above.
    // The six walks are independent, so they run side by side.
    let nfuncs = img.funcs().len();
    std::thread::scope(|sc| {
        sc.spawn(|| pieces_join("real xref", |from, b| xref_window(img, &hex(hot), from, b).unwrap(), &budgets));
        sc.spawn(|| {
            pieces_join(
                "real funcs",
                |from, b| func_list_window(img, FuncSort::Callers, None, 0, from, b).unwrap(),
                &[(nfuncs / 40).max(10), (nfuncs / 4).max(10)],
            )
        });
        sc.spawn(|| {
            pieces_join(
                "real funcs by size",
                |from, b| func_list_window(img, FuncSort::Size, Some("fmt"), 0, from, b).unwrap(),
                &[10, 500],
            )
        });
        sc.spawn(|| pieces_join("real calls", |from, b| calls_window(img, &hex(hot), from, b).unwrap(), &budgets));
        sc.spawn(|| {
            pieces_join(
                "real strings refs",
                |from, b| strings_window(img, Some("e"), StrSort::Refs, from, b).unwrap(),
                &[10, 500],
            )
        });
        sc.spawn(|| {
            pieces_join("real xref --str", |from, b| xref_str_window(img, "the", true, from, b).unwrap(), &[10, 500])
        });
    });
}

/// The pages are cut from a short list: the rows built for a page are the page, not the answer.
#[test]
fn a_page_of_a_long_list_formats_only_its_own_rows() {
    let Some(img) = this_binary() else { return };
    let o = func_list_window(&img, FuncSort::Addr, None, 0, None, 10).unwrap();
    assert!(o.rows.len() <= 10 && o.tail > 1000, "{} rows, {} left", o.rows.len(), o.tail);
    let hot = img.funcs().iter().max_by_key(|f| img.refs().callers(f.start)).unwrap().start;
    let o = xref_window(&img, &hex(hot), None, 10).unwrap();
    assert!(o.rows.len() <= 10 && o.tail > 0, "{} rows, {} left", o.rows.len(), o.tail);
    let o = strings_window(&img, None, StrSort::Refs, None, 10).unwrap();
    assert!(o.rows.len() <= 10 && o.tail > 100);
    // deep in the list the window starts there
    let o = func_list_window(&img, FuncSort::Addr, None, 0, Some(500), 10).unwrap();
    assert_eq!(o.rows[0].key, 500);
}

#[test]
fn refs_and_functions_come_from_one_decode_pass() {
    use crate::disasm::DECODED;
    let f = fixtures::elf64_xref();
    let code = |img: &Image| img.code_sections().map(|s| img.section_bytes(s).len()).sum::<usize>();
    let decoded = || DECODED.with(|c| c.get());
    let fresh = || {
        DECODED.with(|c| c.set(0));
        open(&f)
    };
    let img = fresh();
    let _ = img.refs();
    let _ = img.funcs();
    assert_eq!(decoded(), code(&img), "refs first: one pass for both");
    let img = fresh();
    let _ = img.funcs();
    let _ = img.refs();
    assert_eq!(decoded(), 2 * code(&img), "functions alone are found first when asked first");
    // every op that needs references asks for them before the functions
    let b = hex(f.b);
    type Op<'a> = Box<dyn Fn(&Image) + 'a>;
    let ops: Vec<(&str, Op<'_>)> = vec![
        ("xref", Box::new(|i| drop(xref(i, "main").unwrap()))),
        ("xref addr", Box::new(|i| drop(xref(i, &b).unwrap()))),
        ("xref --str", Box::new(|i| drop(xref_str(i, "nexo", false).unwrap()))),
        ("funcs", Box::new(|i| drop(func_list(i, FuncSort::Callers, None, 0).unwrap()))),
        ("calls", Box::new(|i| drop(calls(i, "main").unwrap()))),
        ("strings", Box::new(|i| drop(strings(i, None).unwrap()))),
        ("fn", Box::new(|i| drop(func(i, "main", false).unwrap()))),
        (
            "fn --to",
            Box::new(|i| drop(func_view(i, &FnQuery { to: Some("0x400150"), ..FnQuery::new("main") }).unwrap())),
        ),
        (
            "name",
            Box::new(|i| {
                let dir = data_dir();
                drop(name_set(i, dir.path(), "main", "x_name").err())
            }),
        ),
    ];
    for (what, op) in ops {
        let img = fresh();
        op(&img);
        assert_eq!(decoded(), code(&img), "{what}: decoded {} bytes, the code is {}", decoded(), code(&img));
    }
}

#[test]
fn indirect_jumps_that_are_not_import_slots_are_counted() {
    // a `jmp rax` in A's place: one indirect jump the table recovery of a later version would resolve
    let f = fixtures::elf64_xref();
    let mut d = f.bytes.clone();
    let img0 = Image::from_bytes(d.clone(), "/x".into()).unwrap();
    let off = img0.addr_to_offset(f.f.unwrap() + 13).unwrap() as usize; // F's final ret
    assert_eq!(d[off], 0xc3);
    d[off] = 0xcc;
    d[off - 1] = 0xe0; // ff e0 = jmp rax over the end of lea's last displacement byte is wrong: keep it simple
    let _ = d;
    let img = open(&f);
    assert_eq!(img.refs().indirect_jumps, 0);
    // E: replace `ret` with `jmp rax` needs two bytes; use the padding after F's neighbour: patch D's `nop; ret` into `jmp rax`
    let mut d = f.bytes.clone();
    let off = Image::from_bytes(d.clone(), "/x".into()).unwrap().addr_to_offset(f.d.unwrap()).unwrap() as usize;
    d[off] = 0xff;
    d[off + 1] = 0xe0;
    let img = Image::from_bytes(d, "/x".into()).unwrap();
    assert_eq!(img.refs().indirect_jumps, 1);
}

#[test]
fn references_to_odd_addresses_do_not_overflow() {
    for (_, f) in both() {
        let img = open(&f);
        for t in ["0xffffffffffffffff", "0xfffffffffffff000", "0", "1"] {
            assert!(xref(&img, t).is_err(), "{t}");
            assert!(calls(&img, t).is_err(), "{t}");
        }
        assert!(xref(&img, "18446744073709551616").is_err());
        assert!(func_list(&img, FuncSort::Callers, None, u64::MAX).is_ok());
        // a page start far beyond the end
        let o = xref_window(&img, &hex(f.b), Some(u64::MAX), 10).unwrap();
        assert!(o.page(Some(u64::MAX), 10).lines[0].starts_with("... nothing at or after"));
        let o = func_list_window(&img, FuncSort::Addr, None, 0, Some(u64::MAX), 10).unwrap();
        assert_eq!(o.rows.len(), 0);
    }
}
