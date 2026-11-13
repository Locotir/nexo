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
