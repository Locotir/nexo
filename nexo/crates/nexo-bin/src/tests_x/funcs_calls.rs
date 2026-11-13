//! Tests of the function list, `calls`, the `strings` refs column and the `fn` header.

use super::*;

#[test]
fn funcs_sorts_filters_and_counts() {
    for (name, f) in both() {
        let img = open(&f);
        let addrs = |o: &Output| -> Vec<u64> {
            let p = o.page(None, 1000);
            p.value["items"].as_array().unwrap().iter().map(|i| i["addr"].as_u64().unwrap()).collect()
        };
        let by_addr = addrs(&func_list(&img, FuncSort::Addr, None, 0).unwrap());
        assert_eq!(by_addr, vec![f.a, f.b, f.c, f.d.unwrap(), f.e.unwrap(), f.f.unwrap()], "{name}");
        // callers: B has 2, C has 1, the rest 0 (ties by address)
        let by_callers = addrs(&func_list(&img, FuncSort::Callers, None, 0).unwrap());
        assert_eq!(by_callers, vec![f.b, f.c, f.a, f.d.unwrap(), f.e.unwrap(), f.f.unwrap()], "{name}");
        // callees: A calls B and C (2), C calls ExitProcess in the PE, E calls B (1)
        let by_callees = addrs(&func_list(&img, FuncSort::Callees, None, 0).unwrap());
        assert_eq!(by_callees[0], f.a, "{name}");
        assert!(by_callees[1..3].contains(&f.e.unwrap()), "{name}: {by_callees:x?}");
        // size: E is the biggest (to the next function), then F... the order is by size, then address
        let sizes: Vec<u64> = {
            let p = func_list(&img, FuncSort::Size, None, 0).unwrap().page(None, 1000);
            p.value["items"].as_array().unwrap().iter().map(|i| i["size"].as_u64().unwrap()).collect()
        };
        assert!(sizes.windows(2).all(|w| w[0] >= w[1]), "{name}: {sizes:?}");
        assert_eq!(row_of(&img, f.b), (2, 0), "{name}");
        assert_eq!(row_of(&img, f.a), (0, 2), "{name}");
        // --min-size and --grep
        let big = addrs(&func_list(&img, FuncSort::Addr, None, 20).unwrap());
        assert_eq!(big, vec![f.e.unwrap()], "{name}");
        let none = func_list(&img, FuncSort::Addr, None, u64::MAX).unwrap();
        assert!(whole(&none).contains("no function matches (of 6 found)"), "{name}");
        let grep = func_list(&img, FuncSort::Addr, Some("^sub_"), 0).unwrap();
        assert_eq!(
            grep.meta["matched"].as_u64().unwrap() as usize,
            img.funcs().iter().filter(|x| x.name.starts_with("sub_")).count()
        );
        assert!(func_list(&img, FuncSort::Addr, Some("("), 0).is_err());
        // the resume command keeps every flag
        let small = func_list(&img, FuncSort::Size, Some("."), 1).unwrap();
        let p = small.page(None, 10);
        assert!(p.next.is_none());
        let o = func_list_window(&img, FuncSort::Size, Some("."), 1, None, 10).unwrap();
        assert!(o.command.ends_with("--sort size --grep . --min-size 1"), "{}", o.command);
    }
}

#[test]
fn calls_gives_two_tables_with_call_sites() {
    for (name, f) in both() {
        let img = open(&f);
        let o = calls(&img, &hex(f.a)).unwrap();
        let t = whole(&o);
        assert!(t.contains("0 callers (0 call sites), 2 callees") && t.contains("callers (0)\n  none"), "{name}: {t}");
        assert!(t.contains(&format!("{:#010x}", f.b)) && t.contains(&format!("{:#010x}", f.c)), "{name}: {t}");
        let o = calls(&img, &hex(f.b)).unwrap();
        let t = whole(&o);
        assert!(t.contains("2 callers (2 call sites), 0 callees"), "{name}: {t}");
        assert_eq!(o.meta["pointers"], 1);
        let items = o.page(None, 50).value["items"].clone();
        assert_eq!(items[0]["table"], "callers");
        assert_eq!(items[0]["sites"], 1);
        // a callee that is an import
        if name == "pe" {
            let t = whole(&calls(&img, &hex(f.c)).unwrap());
            assert!(t.contains("ExitProcess"), "{t}");
            let j = calls(&img, &hex(f.c)).unwrap().page(None, 50).value;
            assert_eq!(j["items"][1]["import"], true);
        }
        // E calls B once and nothing calls E
        let t = whole(&calls(&img, &hex(f.e.unwrap())).unwrap());
        assert!(t.contains("callees (1)"), "{name}: {t}");
    }
}

#[test]
fn strings_have_a_refs_column_and_sort_by_it() {
    for (name, f) in both() {
        let img = open(&f);
        let o = strings(&img, None).unwrap();
        assert!(text_of(&o).contains(&format!("{:#010x}     2  \"{STRING}\"", f.string)), "{name}");
        let o = strings_window(&img, None, StrSort::Refs, None, 100).unwrap();
        let p = o.page(None, 100);
        let items = p.value["items"].as_array().unwrap();
        assert_eq!(items[0]["addr"], f.string, "{name}");
        assert_eq!(items[0]["refs"], 2);
        let refs: Vec<u64> = items.iter().map(|i| i["refs"].as_u64().unwrap()).collect();
        assert!(refs.windows(2).all(|w| w[0] >= w[1]), "{name}: {refs:?}");
        assert!(o.command.ends_with("--sort refs"), "{}", o.command);
    }
}

#[test]
fn fn_header_shows_how_many_callers() {
    for (name, f) in both() {
        let img = open(&f);
        let t = text_of(&func(&img, &hex(f.b), false).unwrap());
        assert!(t.lines().next().unwrap().ends_with("; refs 2"), "{name}: {t}");
        let t = text_of(&func(&img, &hex(f.d.unwrap()), false).unwrap());
        assert!(t.lines().next().unwrap().ends_with("; refs 0"), "{name}: {t}");
        assert_eq!(func(&img, &hex(f.b), false).unwrap().meta["refs"], 2);
    }
}
