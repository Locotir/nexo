//! Files named by calls and the agent's writes of them (engine::files).

use super::*;

#[test]
fn a_file_is_named_by_what_sets_it_apart() {
    assert_eq!(
        files_in("cd material; head -60 assembly-A.txt; sed -n 1,9p ../src/auth_db.rs"),
        vec![vec!["assembly".to_string(), "A".to_string()], vec!["auth".to_string(), "db".to_string()],]
    );
    assert!(files_in("wc -l material/* | grep -n \"a\\|b\" x -n 60,921p").is_empty());
    assert!(files_in("cd material; sed -n '/loc_35DE:/,/loc_36C9:/p' 1.5 src/").is_empty());
    assert_eq!(
        name_parts("Function A (assembly-A) and a word"),
        ["function", "A", "assembly", "A", "and", "a", "word"]
    );
}

#[test]
fn windows_paths_are_paths() {
    assert!(same_file(r"C:\proj\src\pricing.rs", r"src\pricing.rs"));
    assert!(same_file(r"C:\proj\src\pricing.rs", "src/pricing.rs"));
    assert!(same_file(r".\src\pricing.rs", "src/pricing.rs"));
    assert!(!same_file(r"C:\proj\src\pricing.rs", r"lib\pricing.rs"));
    assert_eq!(basename(r"C:\proj\src\pricing.rs"), "pricing.rs");
    assert_eq!(file_key(r".\src\\b\index.ts"), "src/b/index.ts");
    for name in
        [r"src\_try.rs", r"C:\Users\me\AppData\Local\Temp\out.json", r"%TEMP%\run\x.csv", r"C:\Windows\Temp\a.txt"]
    {
        assert!(scratch(name), "{name}");
    }
    assert!(!scratch(r"C:\proj\src\main.rs"));
    let parts =
        |v: &[&[&str]]| -> Vec<Vec<String>> { v.iter().map(|p| p.iter().map(|s| s.to_string()).collect()).collect() };
    assert_eq!(files_in(r"Read C:\proj\src\calc_util.rs"), parts(&[&["calc", "util"]]));
    assert_eq!(files_in(r"type src\calc.rs ..\lib\db.rs"), parts(&[&["calc"], &["db"]]));
    assert_eq!(files_in(r"Read C:/proj/src/calc.rs"), parts(&[&["calc"]]));
    // a pattern with escapes is no path
    assert!(files_in(r"grep -n 'foo\.txt' a\|b \d+\.rs").is_empty());
    // a changed file named in a failure, by its stem, on Windows too
    let failures = vec!["FAILED tests/test_calc.py::test_add".to_string()];
    assert!(named_in(r"C:\proj\src\calc.py", &failures));
}

#[test]
fn underscore_framework_files_are_not_scratch() {
    for name in [
        "_app.tsx",
        "pages/_document.tsx",
        "pkg/__init__.py",
        "__main__.py",
        "styles/_variables.scss",
        "_config.yml",
        "src/tmpl/page.html",
    ] {
        assert!(!scratch(name), "{name}");
    }
    for name in
        ["tmp_out.txt", "_tmp.rs", "_try.rs", "_scratch.md", "scratch.txt", "/tmp/out.json", "/var/tmp/run/x.csv"]
    {
        assert!(scratch(name), "{name}");
    }
    // over the budget, a framework file is kept like any other and the scratch file goes first
    let mut msgs = vec![json!({"role": "user", "content": "Set up the app shell."})];
    msgs.extend(tool(1, "write_file", json!({"path": "pages/_app.tsx", "content": source(0, 40)}), "ok", false));
    msgs.extend(tool(2, "write_file", json!({"path": "_try.rs", "content": source(300, 40)}), "ok", false));
    msgs.extend(tool(3, "write_file", json!({"path": "src/main.rs", "content": source(600, 40)}), "ok", false));
    let mut e = Engine::new(params("pin=4000").unwrap());
    e.sync(&msgs);
    e.p.pin = est_str(&e.wrote_text(msgs.len())) - 1;
    e.harvest(0, msgs.len());
    let kept: Vec<&str> = e.wrote.values().map(|w| w.path.as_str()).collect();
    assert!(kept.contains(&"pages/_app.tsx") && !kept.contains(&"_try.rs"), "{kept:?}");
}

#[test]
fn same_basename_in_two_dirs_keeps_both_writes_and_pins() {
    let mut msgs = vec![json!({"role": "user", "content": "Compare src/a/index.ts with src/b/index.ts."})];
    msgs.extend(tool(1, "read_file", json!({"path": "src/a/index.ts"}), &source(0, 10), false));
    msgs.extend(tool(2, "read_file", json!({"path": "src/b/index.ts"}), &source(100, 10), false));
    msgs.extend(tool(3, "write_file", json!({"path": "src/a/main.ts", "content": source(200, 10)}), "ok", false));
    msgs.extend(tool(4, "write_file", json!({"path": "src/b/main.ts", "content": source(300, 10)}), "ok", false));
    let edit = json!({"path": "src/b/main.ts", "old_str": "v1", "new_str": "w1"});
    msgs.extend(tool(5, "edit_file", edit, "ok", false));
    let mut e = Engine::new(params("pin=4000").unwrap());
    e.sync(&msgs);
    e.harvest(0, msgs.len());
    // both writes stay, and the edit marks only its own file
    let wrote: Vec<(&str, Option<usize>)> = e.wrote.values().map(|w| (w.path.as_str(), w.edited)).collect();
    assert_eq!(wrote, vec![("src/a/main.ts", None), ("src/b/main.ts", Some(9))]);
    // both reads are pinned
    let mut pinned: Vec<&str> = e.pins.values().map(|p| p.1.as_str()).collect();
    pinned.sort();
    assert_eq!(pinned, vec!["src/a/index.ts", "src/b/index.ts"]);
    assert_eq!(file_key(".//src//b/index.ts"), "src/b/index.ts");
    // a later write of one takes its pin and leaves the other's
    msgs.extend(step(6, "sed -i 's/v1/w1/' src/b/index.ts", "done"));
    e.sync(&msgs);
    e.harvest(0, msgs.len());
    let pinned: Vec<&str> = e.pins.values().map(|p| p.1.as_str()).collect();
    assert_eq!(pinned, vec!["src/a/index.ts"]);
    // a write named by the file name alone reaches every file of that name
    msgs.extend(step(7, "cd src && sed -i 's/v2/w2/' index.ts", "done"));
    msgs.extend(step(8, "cd src && sed -i 's/v3/w3/' main.ts", "done"));
    e.sync(&msgs);
    e.harvest(0, msgs.len());
    assert!(e.pins.is_empty(), "{:?}", e.pins);
    assert!(e.wrote.values().all(|w| w.edited.is_some()), "{:?}", e.wrote);
}

#[test]
fn failed_whole_write_keeps_the_earlier_version() {
    let mut msgs = vec![json!({"role": "user", "content": "Write the rates module."})];
    msgs.extend(tool(1, "write_file", json!({"path": "src/rates.rs", "content": source(0, 10)}), "ok", false));
    let mut e = Engine::new(params("pin=4000").unwrap());
    e.sync(&msgs);
    // the next write is sent, and its result says it failed
    let [call, result] =
        tool(2, "write_file", json!({"path": "src/rates.rs", "content": source(50, 10)}), "permission denied", true);
    msgs.push(call);
    e.sync(&msgs);
    msgs.push(result);
    e.sync(&msgs);
    let kept: Vec<(&str, usize, &str)> = e.wrote.values().map(|w| (w.path.as_str(), w.mi, w.text.as_str())).collect();
    assert_eq!(kept, vec![("src/rates.rs", 1, source(0, 10).as_str())]);
    // its lines are not taken for those of a later version
    let first = *e.fz.by_msg(1).iter().find(|&&i| e.tr.units[i].kind == UKind::Call).unwrap();
    assert!(e.superseded(first, 1).is_empty());
    // a write that went through replaces it
    msgs.extend(tool(3, "write_file", json!({"path": "src/rates.rs", "content": source(90, 10)}), "ok", false));
    e.sync(&msgs);
    assert_eq!(e.wrote.values().map(|w| w.mi).collect::<Vec<_>>(), vec![5]);
}
