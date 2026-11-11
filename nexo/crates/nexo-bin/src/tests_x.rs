//! Tests of the second half: references, the function list, calls, names and notes, and `fn` pages
//! that open at an address or run across functions.

use std::path::{Path, PathBuf};

use crate::fixtures::{self, Fixture, STRING};
use crate::tests::this_binary;
use crate::*;

fn open(f: &Fixture) -> Image {
    Image::from_bytes(f.bytes.clone(), PathBuf::from("/fx/prog")).expect("the fixture opens")
}

/// Both xref fixtures, named.
fn both() -> Vec<(&'static str, Fixture)> {
    vec![("elf", fixtures::elf64_xref()), ("pe", fixtures::pe64_xref())]
}

fn hex(a: u64) -> String {
    format!("{a:#x}")
}

fn whole(o: &Output) -> String {
    o.page(None, usize::MAX / 2).text()
}

/// The `refs` figure of a function row in `funcs` JSON: (callers, callees) by address.
fn row_of(img: &Image, addr: u64) -> (u64, u64) {
    let o = func_list(img, FuncSort::Addr, None, 0).unwrap();
    let p = o.page(None, usize::MAX / 2);
    let item = p.value["items"].as_array().unwrap().iter().find(|i| i["addr"] == addr).expect("function listed");
    (item["callers"].as_u64().unwrap(), item["callees"].as_u64().unwrap())
}

/// The first number after `--from ` in a resume command.
fn from_of(cmd: &str) -> u64 {
    parse_addr(cmd.split("--from ").nth(1).unwrap().split(' ').next().unwrap()).unwrap()
}

/// Read an answer piece by piece with the resume command in its last line and check the pieces
/// join to the whole answer. `make(from, budget)` is the op the command line would run.
fn pieces_join(what: &str, make: impl Fn(Option<u64>, usize) -> Output, budgets: &[usize]) {
    let unbounded = make(None, usize::MAX);
    let all = unbounded.text_all();
    assert_eq!(unbounded.tail, 0, "{what}: the unbounded answer builds every row");
    for &budget in budgets {
        let mut joined: Vec<String> = Vec::new();
        let mut from = None;
        let mut pieces = 0;
        loop {
            let o = make(from, budget);
            assert!(
                o.rows.len() <= budget.max(MIN_LINES),
                "{what}: {} rows built for a page of {budget}",
                o.rows.len()
            );
            let p = o.page(from, budget);
            assert!(p.lines.len() <= budget.max(MIN_LINES), "{what}: budget {budget}");
            pieces += 1;
            assert!(pieces < 20_000, "{what}: does not end");
            match p.next {
                Some(cmd) => {
                    joined.extend(p.lines[..p.lines.len() - 1].iter().cloned());
                    let next = from_of(&cmd);
                    assert!(from.is_none_or(|f| next > f), "{what}: the resume does not move on ({cmd})");
                    from = Some(next);
                }
                None => {
                    joined.extend(p.lines);
                    break;
                }
            }
        }
        assert_eq!(joined.join("\n"), all, "{what}: budget {budget}");
    }
}

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

fn text_of(o: &Output) -> String {
    o.page(None, 1000).text()
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

/// A fresh folder for the data dir of a test, removed when it goes out of scope.
struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn data_dir() -> TempDir {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "nexo-bin-x-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

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
    pieces_join("real xref", |from, b| xref_window(&img, &hex(hot), from, b).unwrap(), &budgets);
    pieces_join(
        "real funcs",
        |from, b| func_list_window(&img, FuncSort::Callers, None, 0, from, b).unwrap(),
        &[10, 500],
    );
    pieces_join(
        "real funcs by size",
        |from, b| func_list_window(&img, FuncSort::Size, Some("fmt"), 0, from, b).unwrap(),
        &[10, 500],
    );
    pieces_join("real calls", |from, b| calls_window(&img, &hex(hot), from, b).unwrap(), &budgets);
    pieces_join(
        "real strings refs",
        |from, b| strings_window(&img, Some("e"), StrSort::Refs, from, b).unwrap(),
        &[10, 500],
    );
    pieces_join("real xref --str", |from, b| xref_str_window(&img, "the", true, from, b).unwrap(), &[10, 500]);
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
                drop(name_set(i, std::env::temp_dir().as_path().join("nexo-bin-x").as_path(), "main", "x_name").err())
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

// ---- the real binary -------------------------------------------------------------------------

#[inline(never)]
fn xref_marker_target(x: u32) -> u32 {
    std::hint::black_box(x).wrapping_mul(0x9e37_79b1) ^ 0x1234_5678
}

#[inline(never)]
fn xref_marker_caller_one(x: u32) -> u32 {
    xref_marker_target(x).wrapping_add(1)
}

#[inline(never)]
fn xref_marker_caller_two(x: u32) -> u32 {
    xref_marker_target(x ^ 7).wrapping_add(2)
}

const XREF_STRING: &str = "xref-marker-string-3b9e5d0a";

#[inline(never)]
fn xref_marker_string_user() -> usize {
    std::hint::black_box(XREF_STRING).len()
}

#[test]
fn the_test_binary_shows_who_calls_a_marker_function_and_loads_a_marker_string() {
    let Some(img) = this_binary() else { return };
    let n = std::hint::black_box(3u32);
    let sum = xref_marker_caller_one(n).wrapping_add(xref_marker_caller_two(n));
    std::hint::black_box(sum);
    assert_eq!(xref_marker_string_user(), XREF_STRING.len());
    let o = xref(&img, "xref_marker_target").unwrap();
    let calls_n = o.meta["counts"]["call"].as_u64().unwrap();
    assert!(calls_n >= 2, "{}", whole(&o));
    let t = whole(&o);
    assert!(t.contains("xref_marker_caller_one") && t.contains("xref_marker_caller_two"), "{t}");
    // calls says the same from the function's side
    let c = calls(&img, "xref_marker_caller_one").unwrap();
    assert!(whole(&c).contains("xref_marker_target"), "{}", whole(&c));
    // the string
    let o = xref_str(&img, XREF_STRING, false).unwrap();
    assert!(o.meta["references"].as_u64().unwrap() >= 1, "{}", whole(&o));
    assert!(whole(&o).contains("xref_marker_string_user"), "{}", whole(&o));
}

// ---- fn: opens at an address, and runs across functions ----------------------------------------

/// A function of at least `n` instructions in the test binary.
fn long_function(img: &Image, n: usize) -> (u64, u64, usize) {
    for f in img.funcs() {
        if f.size() > 40_000 || f.size() < n as u64 * 2 {
            continue;
        }
        let mut starts = Vec::new();
        crate::disasm::walk(img.read_at(f.start, f.size() as usize).unwrap_or(&[]), f.start, |ins, _| {
            starts.push(ins.ip())
        });
        if starts.len() >= n {
            return (f.start, f.end, starts.len());
        }
    }
    panic!("no function of {n} instructions in the test binary");
}

#[test]
fn fn_at_an_address_opens_eight_instructions_before_and_marks_it() {
    let Some(img) = this_binary() else { return };
    let (start, _, count) = long_function(&img, 2000);
    let mut starts = Vec::new();
    crate::disasm::walk(img.read_at(start, 40_000).unwrap(), start, |ins, _| starts.push(ins.ip()));
    assert!(count >= 2000);
    for k in [0usize, 3, 8, 9, 20, 1500, 1999] {
        let asked = starts[k];
        // an address inside the instruction is the instruction
        for (budget, shift) in [(120usize, 0u64), (120, 1), (12, 0), (10, 0)] {
            let asked_addr = asked + shift.min(1);
            let target = hex(asked_addr);
            let o = func_window(&img, &target, false, None, budget).unwrap();
            let p = o.page(None, budget);
            assert!(p.lines.len() <= budget.max(MIN_LINES));
            let marked: Vec<&String> = p.lines.iter().filter(|l| l.starts_with("> ")).collect();
            if asked_addr == start {
                assert!(marked.is_empty());
                continue;
            }
            assert_eq!(marked.len(), 1, "k={k} budget={budget}: {:?}", p.lines);
            assert!(marked[0].starts_with(&format!("> {asked:#010x}  ")), "k={k}: {}", marked[0]);
            let json = o.page(None, budget).value;
            let flagged: Vec<_> = json["items"].as_array().unwrap().iter().filter(|i| i["asked"] == true).collect();
            assert_eq!(flagged.len(), 1);
            assert_eq!(flagged[0]["addr"], asked);
            let head = &p.lines[0];
            assert!(head.starts_with("fn ") && head.contains("; refs "), "{head}");
            let lead = LEAD_IN.min(budget.saturating_sub(4));
            if k > lead {
                // the page opens eight instructions before (fewer when the budget is small)
                let first = &p.lines[2];
                let first_addr = parse_addr(first.trim_start_matches(['>', ' ']).split("  ").next().unwrap()).unwrap();
                let before = starts.iter().position(|&a| a == asked).unwrap()
                    - starts.iter().position(|&a| a == first_addr).unwrap();
                assert_eq!(before, lead, "k={k} budget={budget}");
                assert!(head.contains(&format!("; showing from {first_addr:#x}")), "{head}");
                let earlier = &p.lines[1];
                assert!(earlier.starts_with(&format!("... {} earlier lines: nexo bin fn ", k - before)), "{earlier}");
                assert!(earlier.contains(&format!("--from {start:#x}")), "{earlier}");
                assert_eq!(o.meta["earlier_lines"].as_u64().unwrap() as usize, k - before);
            } else {
                assert!(!p.lines[1].starts_with("..."), "k={k}: {}", p.lines[1]);
            }
        }
    }
}

#[test]
fn the_earlier_lines_command_shows_the_lines_and_the_pages_after_the_cut_join_up() {
    let Some(img) = this_binary() else { return };
    let (start, _, _) = long_function(&img, 2000);
    let mut starts = Vec::new();
    crate::disasm::walk(img.read_at(start, 40_000).unwrap(), start, |ins, _| starts.push(ins.ip()));
    let asked = hex(starts[1200] + 1);
    let o = func_window(&img, &asked, false, None, 40).unwrap();
    let p = o.page(None, 40);
    let earlier = p.lines[1].clone();
    let cmd = earlier.split(": ").nth(1).unwrap();
    assert!(cmd.contains("--lines 1194") || cmd.contains("--lines"), "{cmd}");
    // that command is a plain `fn` from the function start: its first page begins at the function
    let from = from_of(cmd);
    assert_eq!(from, start);
    let lines: usize = cmd.rsplit("--lines ").next().unwrap().trim().parse().unwrap();
    let skip = 1200 - LEAD_IN;
    assert_eq!(lines, skip + 2, "{cmd}");
    let again = func_window(&img, &asked, false, Some(from), lines).unwrap().page(Some(from), lines);
    assert!(again.lines[0].starts_with("fn "), "{}", again.lines[0]);
    // the heading, then every instruction before the first one the first page showed, then the cut line
    assert_eq!(again.lines.len(), skip + 2, "{} lines", again.lines.len());
    let shown = |l: &str| parse_addr(l.trim_start_matches(['>', ' ']).split("  ").next().unwrap()).unwrap();
    assert_eq!(shown(&again.lines[skip]), starts[skip - 1]);
    let first_on_page_one = shown(&p.lines[2]);
    assert_eq!(first_on_page_one, starts[skip]);
    assert!(p.lines[0].contains(&format!("; showing from {first_on_page_one:#x}")));
    // the pages after the first one, with `--from`, keep going from where the first one stopped
    let last = p.lines.last().unwrap();
    assert!(last.contains("more lines: nexo bin fn "), "{last}");
    let next = from_of(last);
    let o2 = func_window(&img, &asked, false, Some(next), 40).unwrap();
    let p2 = o2.page(Some(next), 40);
    let shown_first: Vec<&String> = p.lines[..p.lines.len() - 1].iter().collect();
    let last_shown = shown_first.last().unwrap().trim_start_matches(['>', ' ']).split("  ").next().unwrap().to_string();
    assert!(parse_addr(&last_shown).unwrap() < next);
    assert!(p2.lines[0].trim_start_matches(['>', ' ']).starts_with(&format!("{next:#010x}")), "{}", p2.lines[0]);
    // unchanged lines in `fn` by name: no marker, no earlier line, a plain header
    let named = func(&img, img.funcs().iter().find(|f| f.start == start).unwrap().name.as_str(), false);
    if let Ok(o) = named {
        assert!(!text_of(&o).contains("\n> "));
    }
}

#[test]
fn fn_on_a_fixture_marks_the_instruction_that_holds_the_address() {
    for (name, f) in both() {
        let img = open(&f);
        let t = text_of(&func(&img, &hex(f.e.unwrap() + 10), false).unwrap());
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("fn "), "{name}");
        assert!(lines.iter().any(|l| l.starts_with(&format!("> {:#010x}  call", f.e.unwrap() + 10))), "{name}: {t}");
        assert_eq!(lines.iter().filter(|l| l.starts_with("> ")).count(), 1);
        assert!(
            lines.iter().filter(|l| !l.starts_with("fn ")).all(|l| l.starts_with("  ") || l.starts_with("> ")),
            "{name}: {t}"
        );
        // the first byte of a function and a name need no marker
        let t = text_of(&func(&img, &hex(f.e.unwrap()), false).unwrap());
        assert!(!t.contains("> ") && t.contains(&format!("\n{:#010x}  ", f.e.unwrap())), "{name}: {t}");
    }
}

#[test]
fn fn_to_runs_across_functions_with_a_header_each_and_folded_padding() {
    for (name, f) in both() {
        let img = open(&f);
        let to = hex(f.f.unwrap() + 14);
        let o = func_view(&img, &FnQuery { to: Some(&to), ..FnQuery::new(&hex(f.a)) }).unwrap();
        let t = text_of(&o);
        let headers: Vec<&str> = t.lines().filter(|l| l.starts_with("fn ")).collect();
        assert_eq!(headers.len(), 6, "{name}: {t}");
        assert!(
            headers[0].contains(&format!("{:#x}..", f.a)) && headers[5].contains(&format!("{:#x}..", f.f.unwrap())),
            "{name}"
        );
        // B's closing int3 bytes are one line; D's too
        assert!(t.contains("... 3 bytes of padding"), "{name}: {t}");
        assert!(t.matches("of padding").count() >= 3, "{name}: {t}");
        assert!(!t.contains("int3"), "{name}: padding is folded: {t}");
        assert_eq!(o.meta["functions"], 6);
        // JSON: instruction items carry their function
        let j = o.page(None, 200).value;
        assert!(j["items"].as_array().unwrap().iter().any(|i| i["function"].is_string() && i["text"].is_string()));
        assert!(j["items"].as_array().unwrap().iter().any(|i| i["padding_bytes"].is_u64()));
        // the start inside a function: its header says where the view starts; the end cuts a function short
        let mid = hex(f.b + 7);
        let end = hex(f.e.unwrap() + 10);
        let o = func_view(&img, &FnQuery { to: Some(&end), ..FnQuery::new(&mid) }).unwrap();
        let t = text_of(&o);
        assert!(
            t.contains(&format!("; showing from {:#x}", f.b + 7)) || t.lines().next().unwrap().contains("fn "),
            "{name}: {t}"
        );
        let last_header = t.lines().rfind(|l| l.starts_with("fn ")).unwrap();
        assert!(last_header.contains(&format!("; showing up to {:#x}", f.e.unwrap() + 10)), "{name}: {t}");
        assert!(
            !t.lines().any(|l| l.starts_with(&format!("{:#010x}", f.e.unwrap() + 10))),
            "{name}: --to is exclusive: {t}"
        );
        assert!(t.contains(&format!("{:#010x}  mov dword ptr", f.e.unwrap())), "{name}");
        // the end of the range at a function boundary shows no part of the next one
        let o = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&hex(f.a)) }).unwrap();
        assert_eq!(o.meta["functions"], 2, "{name}");
    }
}

#[test]
fn fn_to_resume_keeps_to_bytes_and_json() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let to = hex(f.f.unwrap() + 14);
    let start = hex(f.a);
    let q = FnQuery { to: Some(&to), with_bytes: true, budget: 12, json: true, ..FnQuery::new(&start) };
    let o = func_view(&img, &q).unwrap();
    let p = o.page_as(None, 12, true);
    let cmd = p.next.expect("cut");
    assert!(cmd.starts_with("nexo bin fn /fx/prog 0x400100 --to 0x40015e --bytes --json --from 0x"), "{cmd}");
    assert!(cmd.ends_with("--lines 12"), "{cmd}");
    assert!(p.lines.last().unwrap().contains(&cmd));
    // following the command moves forward
    let from = from_of(&cmd);
    assert!(from > 0);
    let o2 = func_view(&img, &FnQuery { from: Some(from), ..q }).unwrap();
    assert_eq!(o2.rows[0].key, from);
}

#[test]
fn fn_to_refuses_what_it_cannot_do_with_exit_2() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let run = |a: &str, to: &str| func_view(&img, &FnQuery { to: Some(to), ..FnQuery::new(a) });
    for (a, to, why) in [
        (hex(f.b).as_str(), hex(f.a).as_str(), "not after the start"),
        (hex(f.b).as_str(), hex(f.b).as_str(), "not after the start"),
        (hex(f.b).as_str(), hex(f.table_or_string()).as_str(), "is in .data, which is data"),
        (hex(f.table_or_string()).as_str(), hex(f.table_or_string() + 8).as_str(), "which is data"),
        (hex(f.b).as_str(), "0x10", "not after the start"),
        ("0x10", "0x20", "not inside any section"),
        (hex(f.b).as_str(), "0xffffffffffffffff", "ask for at most"),
        (hex(f.b).as_str(), "zzz", "not a number"),
    ] {
        let e = run(a, to).unwrap_err();
        assert!(e.to_string().contains(why), "{a}..{to}: {e}");
        if why != "not a number" {
            assert_eq!(e.exit_code(), 2, "{a}..{to}: {e}");
        }
    }
    // too long a range
    let e = run(&hex(f.a), &hex(f.a + MAX_RANGE + 1)).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    // a start that no function holds
    let mut d = f.bytes.clone();
    let _ = &mut d;
    // an unknown name is a plain question without an answer (exit 1), as for `fn` without --to
    assert_eq!(run("no_such_function", &hex(f.f.unwrap())).unwrap_err().exit_code(), 1);
}

trait StringAddr {
    fn table_or_string(&self) -> u64;
}

impl StringAddr for Fixture {
    fn table_or_string(&self) -> u64 {
        self.string
    }
}

#[test]
fn fn_to_shows_the_bytes_between_functions_of_a_real_binary() {
    let Some(img) = this_binary() else { return };
    let funcs = img.funcs();
    // a gap: two functions with unclaimed bytes between them (the end of one section and the next)
    let pair = funcs.windows(2).find(|w| w[0].end < w[1].start && w[1].start - w[0].end < 4096 && w[0].size() < 4000);
    let Some(w) = pair else {
        eprintln!("skipped: this binary has no gap between two functions");
        return;
    };
    let to = hex(w[1].start + 1);
    let o = func_view(&img, &FnQuery { to: Some(&to), budget: usize::MAX, ..FnQuery::new(&hex(w[0].start)) }).unwrap();
    let t = whole(&o);
    assert!(t.contains(&format!("... {} byte", w[1].start - w[0].end)), "{t}");
    assert!(t.contains("outside any function"), "{t}");
}

#[test]
fn fn_to_over_two_thousand_instructions_joins_and_stays_inside_the_budget() {
    let Some(img) = this_binary() else { return };
    let (start, end, count) = long_function(&img, 2000);
    let to = hex(end);
    let q = |from: Option<u64>, budget: usize| {
        func_view(&img, &FnQuery { to: Some(&to), from, budget, ..FnQuery::new(&hex(start)) }).unwrap()
    };
    let o = q(None, 120);
    assert!(o.rows.len() <= 120, "{} rows formatted for a page of 120", o.rows.len());
    // one header, the instructions, and the trailing int3 padding folded into one line
    let lines = o.tail + o.rows.len();
    assert!(lines + 32 >= count && lines <= count + 2, "{lines} lines for {count} instructions");
    pieces_join("real fn --to", q, &[120, 500]);
}

#[test]
fn a_range_from_a_name_starts_at_the_function() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let name = img.funcs().iter().find(|x| x.start == f.b).unwrap().name.clone();
    let a = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&name) }).unwrap();
    let b = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&hex(f.b)) }).unwrap();
    assert_eq!(a.text_all(), b.text_all());
    assert_eq!(a.meta["functions"], 1);
}

// ---- review fixes: imports by name, long lists, what was cut or not resolved -------------------------

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
