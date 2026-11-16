#![cfg(all(any(target_os = "linux", target_os = "macos"), target_arch = "x86_64"))]
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
    // Where the first multi-byte instruction sits depends on the compiler (an `endbr64` makes it `main` itself).
    let main = s.addr_of("main").unwrap();
    let off = a + 1 - main;
    let by_name = s.exec(&split_words(&format!("break main+{off:#x}"))).unwrap_err();
    let start = match a - main {
        0 => "main".to_string(),
        d => format!("main+{d:#x}"),
    };
    assert!(by_name.to_string().contains(&format!("is inside the instruction at {start} (")), "{by_name}");
}

#[test]
fn disas_is_dis_and_a_stray_line_names_the_way_to_give_input() {
    let Some((mut s, _d)) = start() else { return eprintln!("SKIPPED: no C compiler") };
    let d = s.exec(&split_words("disas main 3")).unwrap().text_all();
    assert!(d.contains("0x"), "{d}");
    let e = s.exec(&split_words("3 20 X")).unwrap_err().to_string();
    assert!(e.contains("run <path> <<<") && e.contains("stdin <text>"), "{e}");
}

#[test]
fn run_inside_a_live_session_says_the_program_is_already_running() {
    let Some((mut s, _d)) = start() else { return eprintln!("SKIPPED: no C compiler") };
    let e = s.exec(&split_words("run ./t")).unwrap_err().to_string();
    assert!(e.contains("the program is already running (pid "), "{e}");
    assert!(e.contains("stopped at 0x") && e.contains("main"), "{e}");
    assert!(e.contains("`cont`") && e.contains("`kill`") && e.contains("`end`"), "{e}");
    assert!(!e.contains("unknown command"), "{e}");
    s.exec(&split_words("kill")).unwrap();
    let e = s.exec(&split_words("run")).unwrap_err().to_string();
    assert!(e.contains("has ended (killed by the debugger)") && e.contains("`end`"), "{e}");
}

#[test]
fn a_breakpoint_never_reached_is_listed_when_the_program_ends() {
    let dir = tempfile::tempdir().unwrap();
    let Some(cc) = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|c| std::process::Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
    else {
        return eprintln!("SKIPPED: no C compiler");
    };
    let src = dir.path().join("u.c");
    std::fs::write(
        &src,
        "#include <stdio.h>\n__attribute__((noinline)) int divide(int a, int b) { return a / b; }\nint main(int argc, char **argv) {\n    if (argc > 5) return divide(argc, 2);\n    printf(\"ok\\n\");\n    return 0;\n}\n",
    )
    .unwrap();
    let exe = dir.path().join("u");
    assert!(std::process::Command::new(cc)
        .args(["-O0", "-fpie", "-pie", "-o"])
        .arg(&exe)
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    let ok = s.exec(&split_words("break divide")).unwrap().text_all();
    assert!(ok.contains("breakpoint #1"), "{ok}");
    s.exec(&split_words("break main")).unwrap();
    s.exec(&split_words("cont")).unwrap();
    let out = s.exec(&split_words("cont")).unwrap().text_all();
    assert!(out.contains("exited with code 0"), "{out}");
    assert!(out.contains("breakpoint #1 at 0x"), "{out}");
    assert!(out.contains("divide") && out.contains("was never hit (hits 0)"), "{out}");
    // the line names the load offset of a program loaded away from its link address: a PIE on Linux; a macOS
    // program the debugger starts loads at its link address, so there it has none
    if cfg!(target_os = "linux") {
        assert!(out.contains("file address 0x") && out.contains("loaded at +0x"), "{out}");
    }
    assert!(!out.contains("breakpoint #2 at"), "main was hit: {out}");
}
