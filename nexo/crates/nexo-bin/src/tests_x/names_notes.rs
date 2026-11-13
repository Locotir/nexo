//! Tests of the names and notes a person gives, and the file that keeps them.

use super::*;

#[test]
fn names_and_notes_show_everywhere_and_stay_with_the_file() {
    for (name, f) in both() {
        let dir = data_dir();
        let img = open(&f);
        let t = whole(&name_set(&img, dir.path(), &hex(f.b), "load_banner").unwrap());
        assert!(
            t.starts_with(&format!("named {:#x} load_banner (was ", f.b))
                || t.starts_with(&format!("named {:#x} load_banner", f.b)),
            "{t}"
        );
        note_set(&img, dir.path(), &hex(f.b), "prints the banner\tand returns").unwrap();
        name_set(&img, dir.path(), &hex(f.global.unwrap()), "g_counter").unwrap();
        // a new process: a new image that reads the file
        let mut again = open(&f);
        assert_eq!(attach_names(&mut again, dir.path()), None);
        let t = text_of(&func(&again, &hex(f.b), false).unwrap());
        let head = t.lines().next().unwrap();
        assert!(
            head.starts_with("fn load_banner ")
                && head.contains("; refs 2")
                && head.ends_with("; note: prints the banner and returns"),
            "{name}: {head}"
        );
        assert!(
            text_of(&func(&again, &hex(f.a), false).unwrap()).contains("call 0x")
                && text_of(&func(&again, &hex(f.a), false).unwrap()).contains("; load_banner"),
            "{name}"
        );
        let t = whole(&xref(&again, "load_banner").unwrap());
        assert!(
            t.starts_with(&format!("xref {:#x}  function load_banner  ; 3 references  ; note: prints the banner", f.b)),
            "{name}: {t}"
        );
        let t = whole(&xref(&again, &hex(f.global.unwrap())).unwrap());
        assert!(t.contains("g_counter"), "{name}: {t}");
        assert!(whole(&func_list(&again, FuncSort::Callers, None, 0).unwrap()).contains("load_banner  ; note: prints"));
        assert!(whole(&calls(&again, "load_banner").unwrap()).starts_with("calls load_banner "));
        assert!(whole(&calls(&again, &hex(f.a)).unwrap()).contains("load_banner  ; note: prints"), "{name}");
        let t = whole(&names_list(&again));
        assert!(
            t.contains("2 names, 1 notes") && t.contains(&format!("{:#010x}  load_banner  ; note: prints", f.b)),
            "{name}: {t}"
        );
        // the name is not kept for another file
        let mut other = Image::from_bytes(fixtures::elf64(false).bytes, "/fx/other".into()).unwrap();
        assert_eq!(attach_names(&mut other, dir.path()), None);
        assert_eq!(other.user.len(), 0);
        // clear
        assert!(whole(&name_clear(&img, dir.path(), &hex(f.b)).unwrap()).contains("cleared the name"));
        assert!(whole(&name_clear(&img, dir.path(), &hex(f.b)).unwrap()).contains("nothing to clear"));
        assert!(whole(
            &note_clear(&img, dir.path(), "load_banner")
                .unwrap_or_else(|_| note_clear(&img, dir.path(), &hex(f.b)).unwrap())
        )
        .contains("cleared the note"));
        let mut third = open(&f);
        attach_names(&mut third, dir.path());
        assert_eq!(third.user.names.len(), 1);
        assert!(third.user.notes.is_empty());
        assert!(text_of(&func(&third, &hex(f.b), false).unwrap()).starts_with("fn "));
    }
}

#[test]
fn bad_names_and_notes_are_refused() {
    let f = fixtures::pe64_xref();
    let dir = data_dir();
    let img = open(&f);
    let long = "a".repeat(65);
    for bad in ["", "has space", "semi;colon", "new\nline", "caf\u{e9}", "sub_401000", "sub_DEADbeef", &long] {
        let e = name_set(&img, dir.path(), &hex(f.b), bad).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{bad:?}");
    }
    assert!(name_set(&img, dir.path(), &hex(f.b), "sub_zzz").is_ok(), "sub_ followed by non-hex is fine");
    assert!(name_set(&img, dir.path(), &hex(f.b), &"a".repeat(64)).is_ok());
    // an import and a symbol name of the image
    let e = name_set(&img, dir.path(), &hex(f.b), "ExitProcess").unwrap_err();
    assert!(e.to_string().contains("already the name of a symbol or an import"), "{e}");
    let elf = open(&fixtures::elf64_xref());
    let e = name_set(&elf, dir.path(), &hex(fixtures::elf64_xref().b), "main").unwrap_err();
    assert!(e.to_string().contains("already the name"), "{e}");
    // an address outside every section; the same name twice
    assert!(name_set(&img, dir.path(), "0x10", "ok_name").unwrap_err().to_string().contains("not inside any section"));
    name_set(&img, dir.path(), &hex(f.a), "same_name").unwrap();
    assert!(name_set(&img, dir.path(), &hex(f.b), "same_name")
        .unwrap_err()
        .to_string()
        .contains("already the name of"));
    assert!(name_set(&img, dir.path(), &hex(f.a), "same_name").is_ok(), "renaming to its own name is fine");
    // notes: empty is refused, long ones are cut, control characters go
    assert!(note_set(&img, dir.path(), &hex(f.b), "  \t ").is_err());
    let kept = note_set(&img, dir.path(), &hex(f.b), &format!("x\u{7}y{}", "z".repeat(300))).unwrap();
    assert!(whole(&kept).len() < 260 && whole(&kept).contains("xyzzz"), "{}", whole(&kept));
    let mut again = open(&f);
    attach_names(&mut again, dir.path());
    assert_eq!(again.note_at(f.b).unwrap().chars().count(), 200);
}

#[test]
fn the_names_file_stops_at_twenty_thousand_entries() {
    let f = fixtures::elf64_xref();
    let dir = data_dir();
    let img = open(&f);
    let mut doc = serde_json::Map::new();
    for i in 0..(MAX_NAMES as u64) {
        let at = if i == 0 { f.string } else { f.string + 0x1000 + i };
        doc.insert(format!("{at:#x}"), serde_json::json!(format!("n{i}")));
    }
    let path = names_path(dir.path(), &img.sha256);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::json!({"version": 1, "names": doc, "notes": {}}).to_string()).unwrap();
    let mut loaded = open(&f);
    assert_eq!(attach_names(&mut loaded, dir.path()), None);
    assert_eq!(loaded.user.len(), MAX_NAMES);
    let e = name_set(&img, dir.path(), &hex(f.b), "one_too_many").unwrap_err();
    assert!(e.to_string().contains("20000"), "{e}");
    let e = note_set(&img, dir.path(), &hex(f.b), "one too many").unwrap_err();
    assert!(e.to_string().contains("20000"), "{e}");
    // replacing an entry that exists is still allowed
    assert!(name_set(&img, dir.path(), &hex(f.string), "replaced").is_ok());
    // a file with one more is refused whole
    doc = serde_json::Map::new();
    for i in 0..(MAX_NAMES as u64 + 1) {
        doc.insert(format!("{:#x}", f.string + 0x1000 + i), serde_json::json!(format!("n{i}")));
    }
    std::fs::write(&path, serde_json::json!({"names": doc}).to_string()).unwrap();
    let mut over = open(&f);
    assert!(attach_names(&mut over, dir.path()).unwrap().contains("more than 20000 entries"));
}

#[test]
fn sixteen_writers_at_once_lose_nothing() {
    let f = fixtures::elf64_xref();
    let dir = data_dir();
    let img = std::sync::Arc::new(open(&f));
    let root: PathBuf = dir.path().to_path_buf();
    let handles: Vec<_> = (0..16u64)
        .map(|i| {
            let (img, root) = (img.clone(), root.clone());
            let target = f.string + i;
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    name_set(&img, &root, &hex(target), &format!("writer_{i}")).map(|_| ())
                } else {
                    note_set(&img, &root, &hex(target), &format!("note from writer {i}")).map(|_| ())
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    let mut after = open(&f);
    assert_eq!(attach_names(&mut after, dir.path()), None);
    assert_eq!(after.user.names.len(), 8, "{:?}", after.user);
    assert_eq!(after.user.notes.len(), 8, "{:?}", after.user);
    for i in 0..16u64 {
        if i % 2 == 0 {
            assert_eq!(after.user_name(f.string + i), Some(format!("writer_{i}").as_str()));
        } else {
            assert_eq!(after.note_at(f.string + i), Some(format!("note from writer {i}").as_str()));
        }
    }
    // no temp file is left behind
    let left: Vec<_> = std::fs::read_dir(dir.path().join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn hostile_names_files_give_an_error_and_never_a_panic() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("cut", br#"{"version":1,"names":{"0x400110":"half"#.to_vec()),
        ("not an object", b"[1,2,3]".to_vec()),
        ("names is a list", br#"{"names":[]}"#.to_vec()),
        ("value is a number", br#"{"names":{"0x400110":5}}"#.to_vec()),
        ("key is not an address", br#"{"names":{"main":"x"}}"#.to_vec()),
        ("bad name inside", br#"{"names":{"0x400110":"has space"}}"#.to_vec()),
        ("note is a list", br#"{"notes":{"0x400110":["a"]}}"#.to_vec()),
        ("binary", vec![0xff, 0xfe, 0, 1, 2, 3]),
        ("deep", "[".repeat(100_000).into_bytes()),
        ("big", vec![b'['; 50 << 20]),
    ];
    cases.push(("huge number", br#"{"names":{"0xfffffffffffffffffff":"x"}}"#.to_vec()));
    for (what, body) in cases {
        let dir = data_dir();
        let path = names_path(dir.path(), &img.sha256);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &body).unwrap();
        // a read-only op runs without names and says why on the last line
        let mut broken = open(&f);
        let why = attach_names(&mut broken, dir.path()).unwrap_or_else(|| panic!("{what}: no complaint"));
        assert!(why.starts_with("names file not read: ") && why.contains(&img.sha256), "{what}: {why}");
        assert!(!why.contains('\n'), "{what}: one line");
        assert!(text_of(&func(&broken, &hex(f.b), false).unwrap()).starts_with("fn "), "{what}");
        assert_eq!(broken.user.len(), 0);
        // a write is refused and the file stays as it was
        let e = name_set(&img, dir.path(), &hex(f.b), "fresh_name").unwrap_err();
        assert!(e.to_string().contains(&img.sha256), "{what}: {e}");
        assert_eq!(std::fs::read(&path).unwrap().len(), body.len(), "{what}: the file was overwritten");
    }
    // an unreadable path: a folder where the file should be
    let dir = data_dir();
    std::fs::create_dir_all(names_path(dir.path(), &img.sha256)).unwrap();
    let mut b = open(&f);
    assert!(attach_names(&mut b, dir.path()).is_some());
    assert!(name_set(&img, dir.path(), &hex(f.b), "fresh_name").is_err());
}

#[cfg(unix)]
#[test]
fn the_names_file_gets_the_usual_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixtures::elf64_xref();
    let dir = data_dir();
    let img = open(&f);
    name_set(&img, dir.path(), &hex(f.b), "perm_check").unwrap();
    let path = names_path(dir.path(), &img.sha256);
    let plain = dir.path().join("plain");
    std::fs::write(&plain, "x").unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), mode(&plain), "the file follows the umask like any file nexo writes");
    assert_eq!(mode(&path) & 0o002, 0, "not world-writable");
}
