//! One address and range syntax: every form resolved against a file (the static side; the debugger's side is
//! nexo-dbg's tests/addr_forms.rs).

use std::path::PathBuf;

use crate::fixtures;
use crate::{addr_in, close_names, place_of, range_in, AddrExpr, Image, Place, RangeExpr};

fn img() -> (fixtures::Fixture, Image) {
    let f = fixtures::elf64(false);
    let img = Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("the fixture opens");
    (f, img)
}

#[test]
fn every_address_form_parses_to_one_expression() {
    let name = |n: &str, off: i64| AddrExpr::Name { name: n.into(), off };
    for (text, want) in [
        ("0x401000", AddrExpr::Num(0x401000)),
        ("4198400", AddrExpr::Num(4198400)),
        ("main", name("main", 0)),
        ("main+0x10", name("main", 0x10)),
        ("main + 16", name("main", 16)),
        ("helper-0x2", name("helper", -2)),
        (".text", name(".text", 0)),
        (".data+8", name(".data", 8)),
        ("operator-", name("operator-", 0)),
        ("ns::f(int)", name("ns::f(int)", 0)),
        ("$rip", AddrExpr::Live("$rip".into())),
        ("$rsp+0x20", AddrExpr::Live("$rsp+0x20".into())),
        ("*ptr", AddrExpr::Live("*ptr".into())),
    ] {
        assert_eq!(AddrExpr::parse(text).unwrap(), want, "{text}");
    }
    assert!(AddrExpr::parse("  ").unwrap_err().to_string().contains("name+0x10"));
    for (text, want) in [
        ("main..helper", RangeExpr::Between("main".into(), "helper".into())),
        ("0x401000..0x401010", RangeExpr::Between("0x401000".into(), "0x401010".into())),
        ("main+4..main+0x8", RangeExpr::Between("main+4".into(), "main+0x8".into())),
        ("main+0x10", RangeExpr::Len("main".into(), 0x10)),
        ("main", RangeExpr::Whole("main".into())),
        (".data", RangeExpr::Whole(".data".into())),
    ] {
        assert_eq!(RangeExpr::parse(text).unwrap(), want, "{text}");
    }
    assert!(RangeExpr::parse("main..").unwrap_err().to_string().contains("A..B"));
}

#[test]
fn the_static_side_resolves_each_form_to_the_file_address() {
    let (f, img) = img();
    let text = img.sections.iter().find(|s| s.name == ".text").unwrap().addr;
    let data = img.sections.iter().find(|s| s.name == ".data").unwrap().addr;
    for (form, want) in [
        (format!("{:#x}", f.b), f.b),
        (f.b.to_string(), f.b),
        ("main".into(), f.a),
        ("helper".into(), f.b),
        ("main+0x4".into(), f.a + 4),
        ("main+4".into(), f.a + 4),
        ("helper-0x2".into(), f.b - 2),
        (".text".into(), text),
        (".data+8".into(), data + 8),
    ] {
        assert_eq!(addr_in(&img, &form).unwrap(), want, "{form}");
    }
    // the debugger's entry point gives the same file address and says what is there
    assert_eq!(place_of(&img, "main+0x4").unwrap(), Place::Code(f.a + 4));
    assert_eq!(place_of(&img, ".data").unwrap(), Place::Data(data));
    let e = addr_in(&img, "$rip").unwrap_err().to_string();
    assert!(e.contains("running program") && e.contains("debug"), "{e}");
    for (form, want) in [
        ("main..helper".to_string(), (f.a, f.b)),
        (format!("{:#x}..{:#x}", f.a, f.a + 8), (f.a, f.a + 8)),
        ("main+0x8".into(), (f.a, f.a + 8)),
        ("main".into(), (f.a, f.a + 16)),
        (format!("{:#x}", f.a + 3), (f.a, f.a + 16)),
    ] {
        assert_eq!(range_in(&img, &form).unwrap(), want, "{form}");
    }
    let s = img.sections.iter().find(|s| s.name == ".data").unwrap();
    assert_eq!(range_in(&img, ".data").unwrap(), (s.addr, s.addr + s.size));
    let e = range_in(&img, "helper..main").unwrap_err().to_string();
    assert!(e.contains("not after its start"), "{e}");
}

#[test]
fn an_unknown_name_names_the_close_ones_and_the_forms() {
    let (_, img) = img();
    assert_eq!(close_names(&img, "mian"), ["main"]);
    assert_eq!(close_names(&img, "HELP"), ["helper"]);
    for (t, e) in [
        ("mian", addr_in(&img, "mian").unwrap_err()),
        ("mian+4", addr_in(&img, "mian+4").unwrap_err()),
        ("hlper..main", range_in(&img, "hlper..main").unwrap_err()),
        ("mian+8 as a range", range_in(&img, "mian+8").unwrap_err()),
    ] {
        let e = e.to_string();
        assert!(e.contains("close names: main") || e.contains("close names: helper"), "{t}: {e}");
        assert!(e.contains("name+0x10"), "{t}: {e}");
    }
}
