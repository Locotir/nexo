#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `calls <site>` logs one indirect call site and saves what it went to in the per-binary names file,
//! where `nexo bin decomp` reads it back. The program is a small C file; skipped when there is no C compiler.

use nexo_dbg::{open, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

#[test]
fn the_targets_of_one_call_site_are_saved_and_decomp_shows_them_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("no C compiler: skipped");
        return;
    };
    let src = dir.path().join("t.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n\
         __attribute__((noinline)) int add_one(int x) { return x + 1; }\n\
         __attribute__((noinline)) int double_it(int x) { return x * 2; }\n\
         int (*volatile table[2])(int) = { add_one, double_it };\n\
         __attribute__((noinline)) int apply(int i, int v) { return table[i](v); }\n\
         int main(void) {\n    int v = 1;\n    for (int i = 0; i < 4; i++) v = apply(i & 1, v);\n    printf(\"%d\\n\", v);\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("t");
    assert!(std::process::Command::new(cc)
        .args(["-O0", "-fno-pie", "-no-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap()
        .success());
    nexo_dbg::set_data_dir(data.clone());
    let (mut s, _) = open(&Kind::Run { exe: exe.clone(), args: vec![] }).expect("start");
    // the call instruction inside apply
    let dis = run(&mut s, "dis apply 20");
    let site = dis
        .lines()
        .find(|l| l.contains("call"))
        .and_then(|l| l.split_whitespace().find(|w| w.starts_with("0x")))
        .unwrap_or_else(|| panic!("no call in apply\n{dis}"))
        .trim_end_matches(':')
        .to_string();
    let on = run(&mut s, &format!("calls {site}"));
    assert!(on.contains("logging 1 indirect call site"), "{on}");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    drop(s);
    let img = nexo_bin::Image::open(&exe).unwrap();
    let mut img = img;
    nexo_bin::attach_names(&mut img, &data);
    let site_addr = nexo_bin::parse_number(&site).unwrap() as u64;
    let seen = nexo_bin::call_targets(&img, site_addr);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().any(|t| t.ends_with("add_one")) && seen.iter().any(|t| t.ends_with("double_it")), "{seen:?}");
    let text = nexo_bin::decomp_window(&img, &data, &["apply".to_string()], false, None, 100).unwrap();
    let text = text.page(None, 1000).text();
    assert!(text.contains("seen: add_one, double_it"), "{text}");
}
