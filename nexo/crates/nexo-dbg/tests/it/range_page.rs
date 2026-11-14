#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! One range and page contract for `bin` and `debug`: the same range text names the same bytes in the file and
//! in the running program, and `--lines`, `--from` and `--grep` page every answer the same way.
//! Skipped (with a line saying so) when there is no C compiler.

use nexo_dbg::{open, parse, split_words, Kind, Session};

fn run(s: &mut Session, cmd: &str) -> nexo_bin::Output {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}"))
}

/// A session stopped in `main` of a program linked at a fixed address (file and memory addresses agree).
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
         __attribute__((noinline)) int apply(int i, int v) { int a = v; int b = i; return table[b](a) + a - b; }\n\
         int main(void) {\n    printf(\"%d\\n\", apply(0, 1));\n    return 0;\n}\n",
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
    let img = nexo_bin::Image::open(&exe).unwrap();
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break main");
    run(&mut s, "cont");
    Some((s, img, dir))
}

fn addrs(text: &str) -> Vec<u64> {
    text.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|w| w.strip_prefix("0x"))
        .filter_map(|w| u64::from_str_radix(w, 16).ok())
        .collect()
}

#[test]
fn the_same_range_text_gives_the_same_rows_in_bin_and_debug() {
    let Some((mut s, img, _d)) = start("the_same_range_text_gives_the_same_rows") else { return };
    // a range in the code of `apply`, which the breakpoint in `main` does not touch
    let (lo, hi) = nexo_bin::range_in(&img, "apply").unwrap();
    assert!(hi - lo >= 16);
    let forms = [
        "apply".to_string(),
        "apply..apply+0x10".into(),
        "apply+0x0..apply+16".into(),
        "apply+16".into(),
        format!("{lo:#x}..{:#x}", lo + 16),
    ];
    for (i, form) in forms.iter().enumerate() {
        let (a, b) = nexo_bin::range_in(&img, form).unwrap_or_else(|e| panic!("{form}: {e}"));
        assert_eq!((a, b), (lo, if i == 0 { hi } else { lo + 16 }), "{form}");
        // debug takes a span in `mem`; the other forms are the same range through `range_of`
        assert_eq!(s.range_of(form).unwrap(), (a, b), "{form}");
        let file = nexo_bin::bytes(&img, a, b - a, "u8").unwrap();
        let live = run(&mut s, &format!("mem {a:#x}..{b:#x}"));
        assert_eq!(live.text_all(), file.text_all(), "{form}");
    }
    // the same hits from `find --in`, in each form
    for form in &forms {
        let file = nexo_bin::find_bytes_in(&img, "48 89", Some(form)).unwrap();
        let live = run(&mut s, &format!("find 48 89 --in {form}"));
        let (f, l) = (addrs(&file.text_all()), addrs(&live.text_all()));
        assert!(!f.is_empty(), "{form}: {}", file.text_all());
        assert_eq!(f, l, "{form}");
        let (a, b) = nexo_bin::range_in(&img, form).unwrap();
        assert!(f.iter().all(|&x| x >= a && x + 2 <= b), "{form}");
    }
    // a section name is a range too, in both tools
    let (a, b) = nexo_bin::range_in(&img, ".text").unwrap();
    assert_eq!(s.range_of(".text").unwrap(), (a, b));
}

#[test]
fn a_range_is_not_cut_by_the_default_page_and_lines_all_says_every_row() {
    let Some((mut s, img, _d)) = start("a_range_is_not_cut_by_the_default_page") else { return };
    let (lo, hi) = nexo_bin::range_in(&img, "apply").unwrap();
    // 4096 bytes: 256 rows, far more than the default page
    let span = format!("{lo:#x}..{:#x}", lo + 4096);
    let out = run(&mut s, &format!("mem {span}"));
    assert!(out.rows.len() > nexo_bin::DEFAULT_LINES, "{}", out.rows.len());
    let page = parse(&split_words(&format!("mem {span}"))).unwrap().ask().page(&out).unwrap();
    assert_eq!(page.left, 0);
    assert_eq!(page.shown, out.rows.len());
    // a count is a page of the default size; `--lines` names the page, `all` is every row
    let counted = parse(&split_words("find 48 --lines 25")).unwrap();
    assert_eq!(counted.ask().budget(), 25);
    let all = parse(&split_words("find 48 --lines all")).unwrap();
    assert_eq!(all.ask().budget(), usize::MAX);
    assert!(parse(&split_words("find 48 --lines lots")).is_err());
    assert!(parse(&split_words("find 48 --grep (")).is_err());
    let _ = hi;
}

#[test]
fn grep_and_from_page_mem_and_dis() {
    let Some((mut s, img, _d)) = start("grep_and_from_page_mem_and_dis") else { return };
    let (lo, _) = nexo_bin::range_in(&img, "apply").unwrap();
    let span = format!("{lo:#x}..{:#x}", lo + 0x80);

    // mem: 16 bytes a row
    let out = run(&mut s, &format!("mem {span}"));
    assert_eq!(out.rows.len(), 8);
    let second = out.rows[1].key;
    let from = parse(&split_words(&format!("mem {span} --from {second:#x}"))).unwrap().ask().page(&out).unwrap();
    assert_eq!(from.lines, out.rows[1..].iter().map(|r| r.text.clone()).collect::<Vec<_>>());
    let want = &out.rows[3].text;
    let g = parse(&split_words(&format!("mem {span} --grep ^{}", &want[..want.find(' ').unwrap()]))).unwrap();
    let page = g.ask().page(&out).unwrap();
    assert_eq!(page.lines[0], "... --grep ^".to_string() + &want[..want.find(' ').unwrap()] + ": 1 of 8 rows match");
    assert_eq!(page.lines[1], *want);
    assert_eq!(page.lines.len(), 2);

    // dis: one row an instruction
    let dis = run(&mut s, &format!("dis {span}"));
    assert!(dis.rows.len() > 4, "{}", dis.text_all());
    let movs = dis.rows.iter().filter(|r| r.text.contains(" mov")).count();
    assert!(movs > 1);
    let g = parse(&split_words(&format!("dis {span} --grep mov"))).unwrap().ask().page(&dis).unwrap();
    assert_eq!(g.lines[0], format!("... --grep mov: {movs} of {} rows match", dis.rows.len()));
    assert_eq!(g.lines.len(), movs + 1);
    assert!(g.lines[1..].iter().all(|l| l.contains("mov")));
    let k = dis.rows[2].key;
    let f = parse(&split_words(&format!("dis {span} --from {k:#x}"))).unwrap().ask().page(&dis).unwrap();
    assert_eq!(f.lines.len(), dis.rows.len() - 2);
}

#[test]
fn dis_of_a_range_is_not_cut_at_200_instructions() {
    let Some((mut s, _img, _d)) = start("dis_of_a_range_is_not_cut_at_200") else { return };
    // 4 KiB of libc is well over 200 instructions
    let lo = s.addr_of("printf").unwrap();
    let dis = run(&mut s, &format!("dis {lo:#x}..{:#x}", lo + 4096));
    assert!(dis.rows.len() > 200, "{} rows", dis.rows.len());
    assert!(addrs(&dis.text_all().replace("=> ", "")).iter().all(|&x| x >= lo && x < lo + 4096));
    // over the read limit the answer says what is true and what to do
    let e = s.exec(&split_words(&format!("dis {lo:#x}..{:#x}", lo + (4 << 20)))).unwrap_err().to_string();
    assert!(e.contains("at most 1024 KiB"), "{e}");
}
