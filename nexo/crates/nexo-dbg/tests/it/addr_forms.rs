#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]
//! One address syntax for `bin` and `debug`: each form names the same place in a running program as in the
//! file, moved by the load bias of a position-independent build. Registers and memory reads work only here.
//! Skipped (with a line saying so) when there is no C compiler.

use nexo_dbg::{open, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// A session stopped in `main` of a PIE program, and the program's file.
fn start(test: &str) -> Option<(Session, nexo_bin::Image, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        eprintln!("SKIPPED {test}: no C compiler, so nothing was checked");
        return None;
    };
    let src = dir.path().join("t.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n\
         __attribute__((noinline)) int add_one(int x) { return x + 1; }\n\
         int (*volatile table[1])(int) = { add_one };\n\
         __attribute__((noinline)) int apply(int i, int v) { return table[i](v); }\n\
         int main(void) {\n    printf(\"%d\\n\", apply(0, 1));\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("t");
    assert!(std::process::Command::new(cc)
        .args(["-O0", "-fpie", "-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let img = nexo_bin::Image::open(&exe).unwrap();
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break main");
    run(&mut s, "cont");
    Some((s, img, dir))
}

#[test]
fn each_form_names_the_file_address_plus_the_load_bias() {
    let Some((mut s, img, _d)) = start("each_form_names_the_file_address") else { return };
    let file = |t: &str| nexo_bin::addr_in(&img, t).unwrap_or_else(|e| panic!("{t}: {e}"));
    let bias = s.addr_of("main").unwrap().wrapping_sub(file("main"));
    // Linux loads a PIE program away from its link address (0); a macOS program links at 0x100000000 and, with
    // address randomization off for the program the debugger starts, loads right there
    if cfg!(target_os = "linux") {
        assert_ne!(bias, 0, "a PIE program is loaded away from its link address");
    }
    // the sections as each format names them
    let (text, data) =
        if cfg!(target_os = "macos") { ("__TEXT,__text", "__DATA,__data+8") } else { (".text", ".data+8") };
    let apply = file("apply");
    for form in [
        "main".to_string(),
        "apply".into(),
        "apply+0x4".into(),
        "apply+4".into(),
        "add_one-0x1".into(),
        text.into(),
        data.into(),
        "table".into(),
        format!("{apply:#x}"),
        format!("{apply}"),
    ] {
        assert_eq!(s.addr_of(&form).unwrap(), file(&form).wrapping_add(bias), "{form}");
    }
    // only a running program has registers and memory: the debugger evaluates them, the file refuses them
    let rsp = s.addr_of("$rsp").unwrap();
    assert_eq!(s.addr_of("$rsp+0x20").unwrap(), rsp + 0x20);
    assert_eq!(s.addr_of("*(long*)table").unwrap(), file("add_one").wrapping_add(bias));
    for live in ["$rip", "$rsp+0x20", "*(long*)table"] {
        assert!(nexo_bin::addr_in(&img, live).unwrap_err().to_string().contains("running program"), "{live}");
    }
}

#[test]
fn mem_and_dis_take_a_range_and_an_unknown_name_gets_the_close_ones() {
    let Some((mut s, _img, _d)) = start("mem_and_dis_take_a_range") else { return };
    let one = run(&mut s, "mem apply 16");
    assert_eq!(run(&mut s, "mem apply..apply+16"), one);
    assert_eq!(run(&mut s, "mem apply+0..apply+0x10"), one);
    let dis = run(&mut s, "dis apply..apply+8");
    let end = s.addr_of("apply+8").unwrap();
    let shown: Vec<u64> = dis
        .lines()
        .filter_map(|l| l.split_whitespace().find(|w| w.starts_with("0x")))
        .map(|w| u64::from_str_radix(w.trim_end_matches(':').trim_start_matches("0x"), 16).unwrap())
        .collect();
    assert!(!shown.is_empty() && shown.iter().all(|&a| a < end), "{dis}");
    assert_eq!(shown[0], s.addr_of("apply").unwrap(), "{dis}");
    // the old forms still work
    assert_eq!(run(&mut s, "dis apply 3").lines().count(), 3);
    let e = s.exec(&split_words("mem aply 8")).unwrap_err().to_string();
    assert!(e.contains("close names: apply"), "{e}");
    let e = s.exec(&split_words("mem apply+8..apply")).unwrap_err().to_string();
    assert!(e.contains("not after its start"), "{e}");
}
