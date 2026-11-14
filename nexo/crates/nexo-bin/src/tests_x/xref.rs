//! Tests of `xref`: calls, pointers, reads, writes and string loads.

use super::*;

#[test]
fn xref_lists_calls_pointers_reads_writes_and_string_loads() {
    for (name, f) in both() {
        let img = open(&f);
        let (g, p) = (f.global.unwrap(), f.ptrs.unwrap());
        // B: called by A and E, pointed at by the table (2 calls + 1 data pointer)
        let o = xref(&img, &hex(f.b)).unwrap();
        let t = whole(&o);
        assert!(t.starts_with(&format!("xref {:#x}  function ", f.b)) && t.contains("; 3 references"), "{name}: {t}");
        assert!(t.contains("calls (2)") && t.contains("pointers in data (1)"), "{name}: {t}");
        assert!(
            t.contains(&format!("{:#010x}  call ", f.a + 4))
                && t.contains(&format!("{:#010x}  call ", f.e.unwrap() + 10))
        );
        assert!(t.contains(&format!("{:#010x}  ptr ", p)), "{name}: {t}");
        assert_eq!(o.meta["counts"]["call"], 2, "{name}");
        assert_eq!(o.meta["counts"]["ptr"], 1);
        // C: called by A, pointed at by the table's second slot
        let t = whole(&xref(&img, &hex(f.c)).unwrap());
        assert!(t.contains("calls (1)") && t.contains(&format!("{:#010x}  ptr ", p + 8)), "{name}: {t}");
        // D: nothing refers to it
        let o = xref(&img, &hex(f.d.unwrap())).unwrap();
        assert_eq!(o.meta["references"], 0, "{name}");
        assert!(whole(&o).contains("no reference to"), "{name}");
        // the global: written by E, read by F
        let o = xref(&img, &hex(g)).unwrap();
        let t = whole(&o);
        assert_eq!(
            (o.meta["counts"]["write"].as_u64(), o.meta["counts"]["read"].as_u64()),
            (Some(1), Some(1)),
            "{name}: {t}"
        );
        assert!(t.contains(&format!("{:#010x}  write", f.e.unwrap())), "{name}: {t}");
        assert!(t.contains(&format!("{:#010x}  read", f.f.unwrap())), "{name}: {t}");
        assert!(t.contains("mov dword ptr [") && t.contains("mov eax, ["), "{name}: {t}");
        // a function by name, and an address inside it is answered as given
        let named = img.funcs().iter().find(|x| x.start == f.b).unwrap().name.clone();
        assert_eq!(whole(&xref(&img, &named).unwrap()), whole(&xref(&img, &hex(f.b)).unwrap()), "{name}");
        let inside = whole(&xref(&img, &hex(f.b + 7)).unwrap());
        assert!(
            inside.contains("inside function") && inside.contains("+0x7") && inside.contains("0 references"),
            "{name}: {inside}"
        );
        // JSON items carry the kind, the holder and the instruction
        let p = xref(&img, &hex(f.b)).unwrap().page(None, 50);
        let items = p.value["items"].as_array().unwrap();
        assert_eq!(items.len(), 3, "{name}");
        assert_eq!(items[0]["kind"], "call");
        assert_eq!(items[0]["addr"], f.a + 4);
        assert!(items[0]["text"].as_str().unwrap().starts_with("call "), "{name}");
        assert_eq!(items[2]["kind"], "ptr");
        assert!(items[2]["text"].is_null());
    }
}

#[test]
fn xref_str_finds_the_loads_of_each_string_that_matches() {
    for (name, f) in both() {
        let img = open(&f);
        let o = xref_str(&img, "nexo", false).unwrap();
        let t = whole(&o);
        assert!(t.contains(&format!("string {:#010x} \"{STRING}\"  (2 references)", f.string)), "{name}: {t}");
        assert!(
            t.contains(&format!("{:#010x}  addr ", f.b)) && t.contains(&format!("{:#010x}  addr ", f.f.unwrap() + 6))
        );
        assert_eq!(o.meta["strings"], 1);
        assert_eq!(o.meta["references"], 2);
        // exact bytes: case matters unless --i
        let o = xref_str(&img, "NEXO", false).unwrap();
        assert_eq!(o.meta["strings"], 0, "{name}");
        assert!(whole(&o).contains("no string contains"), "{name}");
        let o = xref_str(&img, "NEXO", true).unwrap();
        assert_eq!((o.meta["strings"].as_u64(), o.meta["references"].as_u64()), (Some(1), Some(2)), "{name}");
        assert!(whole(&o).contains("ignoring case"));
        assert!(xref_str(&img, "", false).is_err());
        // JSON: a heading item for the string, then its references
        let p = xref_str(&img, "hello", false).unwrap().page(None, 50);
        let items = p.value["items"].as_array().unwrap();
        assert_eq!(items[0]["item"], "string");
        assert_eq!(items[0]["references"], 2);
        assert_eq!(items[1]["string"], f.string);
    }
}

/// A packed run of strings: the references are narrowed to the literal that holds the match.
#[test]
fn xref_str_in_a_packed_run_keeps_only_the_literal_that_matches() {
    let f = fixtures::elf64_xref();
    let mut data = f.bytes.clone();
    let img0 = Image::from_bytes(data.clone(), "/x".into()).unwrap();
    // the 16 bytes in front of the string become text, so the data holds one run: "alpha beta gammahello nexo bin"
    let first = img0.addr_to_offset(f.string - 16).unwrap() as usize;
    data[first..first + 16].copy_from_slice(b"alpha beta gamma");
    // F loads the first literal instead of the second: move its lea displacement back by 16
    let lea = img0.addr_to_offset(f.f.unwrap() + 6).unwrap() as usize;
    let disp = i32::from_le_bytes(data[lea + 3..lea + 7].try_into().unwrap()) - 16;
    data[lea + 3..lea + 7].copy_from_slice(&disp.to_le_bytes());
    let img = Image::from_bytes(data, "/x".into()).unwrap();
    let only = |text: &str| -> Vec<u64> {
        let p = xref_str(&img, text, false).unwrap().page(None, 50);
        p.value["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "addr")
            .map(|i| i["addr"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(only("nexo"), vec![f.b], "the second literal is loaded by B only");
    assert_eq!(only("beta"), vec![f.f.unwrap() + 6], "the first literal is loaded by F only");
    let mut both = only("gammahello");
    both.sort_unstable();
    assert_eq!(both, vec![f.b, f.f.unwrap() + 6], "a match that spans both literals sees both");
}

/// (source, kind, target) of every reference row of an answer, in the order shown.
fn ref_items(o: &Output) -> Vec<(u64, String, u64)> {
    let p = o.page(None, usize::MAX / 2);
    p.value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["addr"].as_u64().unwrap(), i["kind"].as_str().unwrap().to_string(), i["target"].as_u64().unwrap()))
        .collect()
}

/// What one `xref` per address of `lo..hi` finds, all together.
fn union_of_singles(img: &Image, lo: u64, hi: u64) -> Vec<(u64, String, u64)> {
    let mut v: Vec<_> = (lo..hi).flat_map(|a| ref_items(&xref(img, &hex(a)).unwrap())).collect();
    v.sort();
    v
}

#[test]
fn xref_over_a_range_is_the_union_of_single_xrefs() {
    for (name, f) in both() {
        let img = open(&f);
        let g = f.global.unwrap();
        for (lo, hi) in [(f.a, f.text_end), (f.string, f.string + 16), (g, g + 4)] {
            let o = xref_range_window(&img, &hex(lo), &hex(hi), None, usize::MAX).unwrap();
            let shown = ref_items(&o);
            assert!(shown.windows(2).all(|w| w[0].2 <= w[1].2), "{name}: grouped by target, in order: {shown:?}");
            let mut got = shown.clone();
            got.sort();
            let want = union_of_singles(&img, lo, hi);
            assert!(!want.is_empty(), "{name}: {lo:#x}..{hi:#x} has references");
            assert_eq!(got, want, "{name}: {lo:#x}..{hi:#x}");
            assert_eq!(o.meta["references"].as_u64(), Some(want.len() as u64), "{name}");
            let t = whole(&o);
            assert!(t.starts_with(&format!("xref {lo:#x}..{hi:#x}  ")), "{name}: {t}");
            // one heading line per address that is pointed at, each before its references
            let mut targets: Vec<u64> = want.iter().map(|r| r.2).collect();
            targets.dedup();
            targets.sort_unstable();
            targets.dedup();
            assert_eq!(o.meta["addresses"].as_u64(), Some(targets.len() as u64), "{name}: {t}");
            for x in targets {
                assert!(t.lines().any(|l| l.starts_with(&format!("{x:#x}  "))), "{name}: {x:#x} in {t}");
            }
        }
        // the page bound is the one every xref has, and the command to ask again keeps --to
        let o = xref_range_window(&img, &hex(f.a), &hex(f.text_end), None, MIN_LINES).unwrap();
        let p = o.page(None, MIN_LINES);
        assert!(p.lines.len() <= MIN_LINES && p.next.is_none(), "{name}: {p:?}");
        assert_eq!(p.lines.join("\n"), whole(&o), "{name}");
        assert!(o.command.ends_with(&format!(" {:#x} --to {:#x}", f.a, f.text_end)), "{name}: {}", o.command);
        // a range that ends before it starts is a usage problem
        let e = xref_range_window(&img, &hex(f.b), &hex(f.a), None, usize::MAX).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{name}: {e}");
        let none = xref_range_window(&img, &hex(f.d.unwrap() + 1), &hex(f.d.unwrap() + 2), None, usize::MAX).unwrap();
        assert!(whole(&none).contains("no reference into"), "{name}");
    }
}

#[test]
fn xref_over_a_function_name_covers_its_whole_body() {
    for (name, f) in both() {
        let img = open(&f);
        let b = img.funcs().iter().find(|x| x.start == f.b).unwrap().clone();
        let n = img.func_name(&b).to_string();
        let o = xref_range_window(&img, &n, &n, None, usize::MAX).unwrap();
        assert_eq!((o.meta["target"].as_u64(), o.meta["to"].as_u64()), (Some(b.start), Some(b.end)), "{name}");
        let mut got = ref_items(&o);
        got.sort();
        assert_eq!(got, union_of_singles(&img, b.start, b.end), "{name}");
        let t = whole(&o);
        assert!(t.starts_with(&format!("xref {:#x}..{:#x}  function {n}", b.start, b.end)), "{name}: {t}");
        assert!(
            t.contains(&format!("{:#010x}  call ", f.a + 4)) && t.contains(&format!("{:#010x}  ptr ", f.ptrs.unwrap())),
            "{name}: {t}"
        );
        assert!(o.command.ends_with(&format!(" {n} --to {n}")), "{name}: {}", o.command);
    }
}
