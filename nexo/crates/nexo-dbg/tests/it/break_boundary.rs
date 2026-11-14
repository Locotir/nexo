#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A breakpoint inside an instruction is refused with the instruction and its neighbours; gdb's `disas` is `dis`;
//! a stray input line says how program input is given. Skipped when there is no C compiler.

use nexo_dbg::{open, split_words, Kind, Session};

fn start() -> Option<(Session, tempfile::TempDir)> {
    let dir = tempfile::tempdir().unwrap();
    let cc = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))?;
    let src = dir.path().join("t.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\nint main(void) {\n    volatile long a[4] = {1, 2, 3, 4};\n    printf(\"%ld\\n\", a[2] + a[3]);\n    return 0;\n}\n",
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
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    s.exec(&split_words("break main")).unwrap();
    s.exec(&split_words("cont")).unwrap();
    Some((s, dir))
}

fn starts(s: &mut Session) -> Vec<(u64, String)> {
    let t = s.exec(&split_words("dis main 12")).unwrap().text_all();
    t.lines()
        .filter_map(|l| {
            let l = l.trim_start_matches("=>").trim();
            let (a, rest) = l.split_once(": ")?;
            Some((u64::from_str_radix(a.trim_start_matches("0x"), 16).ok()?, rest.to_string()))
        })
        .collect()
}

#[test]
fn a_breakpoint_inside_an_instruction_is_refused_and_one_on_a_start_is_not() {
    let Some((mut s, _d)) = start() else { return eprintln!("SKIPPED: no C compiler") };
    let ins = starts(&mut s);
    let (a, b) = ins.windows(2).map(|w| (w[0].0, w[1].0)).find(|(a, b)| b - a >= 2).expect("a multi-byte instruction");
    let e = s.exec(&split_words(&format!("break *{:#x}", a + 1))).unwrap_err().to_string();
    assert!(e.contains(&format!("{:#x} is inside the instruction at ", a + 1)), "{e}");
    assert!(e.contains(&format!("{} bytes); instruction starts nearby: ", b - a)), "{e}");
    let ok = s.exec(&split_words(&format!("break *{b:#x}"))).unwrap().text_all();
    assert!(ok.contains("breakpoint #"), "{ok}");
    assert!(!ok.contains("not checked"), "{ok}");
    let off = a + 1 - s.addr_of("main").unwrap();
    let by_name = s.exec(&split_words(&format!("break main+{off:#x}"))).unwrap_err();
    assert!(by_name.to_string().contains("is inside the instruction at main+"), "{by_name}");
}

#[test]
fn disas_is_dis_and_a_stray_line_names_the_way_to_give_input() {
    let Some((mut s, _d)) = start() else { return eprintln!("SKIPPED: no C compiler") };
    let d = s.exec(&split_words("disas main 3")).unwrap().text_all();
    assert!(d.contains("0x"), "{d}");
    let e = s.exec(&split_words("3 20 X")).unwrap_err().to_string();
    assert!(e.contains("run <path> <<<") && e.contains("stdin <text>"), "{e}");
}
