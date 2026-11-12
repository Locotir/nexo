use super::testkit::{demo_class, zip};
use super::*;
use std::path::Path;

fn ask(data: &[u8], q: ManagedQuery<'_>) -> Result<String> {
    let out = managed_window(Path::new("Calc.class"), data, &q, None, 200)?;
    Ok(out.page(None, 200).text())
}

#[test]
fn a_class_file_lists_its_type_with_counts() {
    let t = ask(&demo_class(), ManagedQuery::default()).unwrap();
    assert!(t.contains("JVM class Calc.class: 1 types, 5 methods, 2 fields"), "{t}");
    assert!(t.contains("class demo.Calc  5m 2f"), "{t}");
    assert!(t.contains("class version 52 (Java 8)"), "{t}");
}

#[test]
fn a_type_lists_fields_and_methods_with_readable_signatures() {
    let t = ask(&demo_class(), ManagedQuery { target: Some("Calc"), ..Default::default() }).unwrap();
    assert!(t.contains("field  public static final LIMIT: int"), "{t}");
    assert!(t.contains("field  private name: String"), "{t}");
    assert!(t.contains("method public static add(int, int) -> int  4 insns, 4 bytes"), "{t}");
    assert!(t.contains("main(String[]) -> void"), "{t}");
}

#[test]
fn a_method_is_disassembled_with_offsets_and_resolved_names() {
    let t = ask(&demo_class(), ManagedQuery { target: Some("Calc.greet"), ..Default::default() }).unwrap();
    assert!(t.contains("0x0000  getstatic java.lang.System.out: java.io.PrintStream"), "{t}");
    assert!(t.contains("0x0003  ldc \"hello jvm\""), "{t}");
    assert!(t.contains("0x0005  invokevirtual java.io.PrintStream.println(String) -> void"), "{t}");
    assert!(t.contains("0x0008  return"), "{t}");
    assert!(t.contains("calls 1: java.io.PrintStream.println"), "{t}");
    let t = ask(&demo_class(), ManagedQuery { target: Some("demo.Calc.add"), ..Default::default() }).unwrap();
    assert!(t.contains("0x0000  iload_0  ; line 10") && t.contains("0x0003  ireturn  ; line 11"), "{t}");
}

#[test]
fn a_switch_lists_every_case_on_its_own_line_and_branch_targets_are_checked() {
    let t = ask(&demo_class(), ManagedQuery { target: Some("Calc.pick"), ..Default::default() }).unwrap();
    assert!(t.contains("tableswitch 6 cases, default -> 0x0025"), "{t}");
    assert_eq!(t.matches("case ").count(), 6, "{t}");
    assert!(t.contains("case 5 -> 0x0024"), "{t}");
}

#[test]
fn an_unknown_opcode_stops_decoding_and_says_where() {
    let t = ask(&demo_class(), ManagedQuery { target: Some("Calc.broken"), ..Default::default() }).unwrap();
    assert!(t.contains("?? unknown opcode 0xfe"), "{t}");
    assert!(t.contains("WARNING decoding stopped at 0x0001: unknown opcode 0xfe"), "{t}");
}

#[test]
fn callers_name_the_call_site_and_the_strings_say_where_they_are_loaded() {
    let t = ask(&demo_class(), ManagedQuery { callers: Some("demo.Calc.add"), ..Default::default() }).unwrap();
    assert!(
        t.contains("1 call sites of demo.Calc.add")
            && t.contains("demo.Calc.main([Ljava/lang/String;)V +0x0002  invokestatic demo.Calc.add(int, int) -> int"),
        "{t}"
    );
    let t = ask(&demo_class(), ManagedQuery { callers: Some("Nope.x"), ..Default::default() }).unwrap();
    assert!(t.contains("no call to Nope.x"), "{t}");
    let t = ask(&demo_class(), ManagedQuery { strings: true, ..Default::default() }).unwrap();
    assert!(t.contains("demo.Calc.greet +0x0003  \"hello jvm\""), "{t}");
    assert!(t.contains("demo.Calc  \"only in the pool\""), "{t}");
    let t = ask(&demo_class(), ManagedQuery { strings: true, grep: Some("^hello"), ..Default::default() }).unwrap();
    assert!(t.contains("1 strings") && !t.contains("only in the pool"), "{t}");
}

#[test]
fn unknown_names_and_ambiguous_overloads_name_the_next_step() {
    let e = ask(&demo_class(), ManagedQuery { target: Some("Missing"), ..Default::default() }).unwrap_err();
    assert!(e.to_string().contains("list the types with `nexo bin managed Calc.class`"), "{e}");
    let mut b = testkit::ClassBuilder::new("a/B", "java/lang/Object");
    b.method(1, "f", "()V", &[0xb1], &[]);
    b.method(1, "f", "(I)V", &[0xb1], &[]);
    let e = ask(&b.build(), ManagedQuery { target: Some("B.f"), ..Default::default() }).unwrap_err();
    assert!(e.to_string().contains("a.B.f()V | a.B.f(I)V"), "{e}");
    let t = ask(&b.build(), ManagedQuery { target: Some("B.f(I)V"), ..Default::default() }).unwrap();
    assert!(t.contains("a.B.f(I)V"), "{t}");
}

#[test]
fn a_damaged_class_is_refused_with_the_reason() {
    let mut d = demo_class();
    d.truncate(40);
    let e = ask(&d, ManagedQuery::default()).unwrap_err();
    assert!(e.to_string().contains("not a readable class file"), "{e}");
}

#[test]
fn a_jar_reads_stored_and_deflated_classes_and_the_manifest() {
    let jar = zip(&[
        ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\nMain-Class: demo.Calc\r\n".to_vec(), true),
        ("demo/Calc.class", demo_class(), true),
        (
            "demo/Other.class",
            {
                let mut b = testkit::ClassBuilder::new("demo/Other", "demo/Calc");
                b.method(1, "run", "()V", &[0xb1], &[]);
                b.build()
            },
            false,
        ),
        ("res/readme.txt", b"hi".to_vec(), false),
    ]);
    let t = ask(&jar, ManagedQuery::default()).unwrap();
    assert!(t.contains("JAR Calc.class: 2 types") && t.contains("Main-Class demo.Calc"), "{t}");
    assert!(t.contains("class demo.Other  1m 0f  extends demo.Calc"), "{t}");
    assert!(t.contains("2 non-class entries not listed"), "{t}");
    let t = ask(&jar, ManagedQuery { target: Some("Other.run"), ..Default::default() }).unwrap();
    assert!(t.contains("0x0000  return"), "{t}");
}

#[test]
fn paging_a_long_listing_resumes_exactly() {
    let mut b = testkit::ClassBuilder::new("p/Many", "java/lang/Object");
    for i in 0..40 {
        b.method(1, &format!("m{i:02}"), "()V", &[0xb1], &[]);
    }
    let d = b.build();
    let q = ManagedQuery { target: Some("Many"), ..Default::default() };
    let first = managed_window(Path::new("M.class"), &d, &q, None, 30).unwrap().page(None, 30);
    assert!(first.text().contains("more lines: nexo bin managed"), "{}", first.text());
    let next = first.next.clone().unwrap();
    let from: u64 = next
        .split("--from ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .or_else(|_| u64::from_str_radix(next.split("--from 0x").nth(1).unwrap().split(' ').next().unwrap(), 16))
        .unwrap();
    let second = managed_window(Path::new("M.class"), &d, &q, Some(from), 30).unwrap().page(Some(from), 30);
    let all = managed_window(Path::new("M.class"), &d, &q, None, 500).unwrap().page(None, 500).text();
    for i in 0..40 {
        let name = format!("m{i:02}()");
        let hits = first.text().matches(&name).count() + second.text().matches(&name).count();
        assert!(hits >= 1 && all.contains(&name), "{name} missing from the pages");
    }
}

#[test]
fn javac_output_reads_when_a_jdk_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Hi.java");
    std::fs::write(&src, "public class Hi { static int twice(int x) { return x * 2; } public static void main(String[] a) { System.out.println(\"hi \" + twice(21)); } }").unwrap();
    match std::process::Command::new("javac").arg("-d").arg(dir.path()).arg(&src).output() {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("skipped: javac is not available");
            return;
        }
    }
    let data = std::fs::read(dir.path().join("Hi.class")).unwrap();
    let t = ask(&data, ManagedQuery { target: Some("Hi.main"), ..Default::default() }).unwrap();
    assert!(t.contains("invokestatic Hi.twice(int) -> int"), "{t}");
    assert!(t.contains("java.io.PrintStream.println"), "{t}");
    assert!(!t.contains("WARNING") && !t.contains("??"), "{t}");
    let t = ask(&data, ManagedQuery { strings: true, ..Default::default() }).unwrap();
    assert!(t.contains("hi "), "{t}");
}
