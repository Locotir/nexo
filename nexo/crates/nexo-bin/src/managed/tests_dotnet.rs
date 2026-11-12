//! .NET reader tests. They build a tiny assembly with a C# compiler when one is installed
//! (`csc` or `mcs`) and skip with a message otherwise.

use super::*;
use std::path::Path;
use std::process::Command;

const SRC: &str = r#"
using System;
using System.Runtime.InteropServices;
namespace Demo {
  public class Calc {
    public const int Limit = 9;
    private string name = "calc";
    public static int Add(int a, int b) { return a + b; }
    public static void Greet() { Console.WriteLine("hello dotnet"); }
    public static int Pick(int x) {
      switch (x) { case 0: return 10; case 1: return 11; case 2: return 12; case 3: return 13; case 4: return 14; default: return -1; }
    }
    public static int Guard() { try { return Add(1, 2); } finally { Greet(); } }
    [DllImport("kernel32.dll")] public static extern int GetTickCount();
    public class Inner { public int V; }
    public static void Main(string[] args) { Console.WriteLine(Add(2, 3)); }
  }
}
"#;

fn build() -> Option<Vec<u8>> {
    static ONCE: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    ONCE.get_or_init(build_once).clone()
}

fn build_once() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("nexo-dotnet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let src = dir.join("calc.cs");
    std::fs::write(&src, SRC).ok()?;
    let out = dir.join("calc.exe");
    for tool in ["csc", "mcs"] {
        let ok = Command::new(tool)
            .arg("-nologo")
            .arg(format!("-out:{}", out.display()))
            .arg(&src)
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            let d = std::fs::read(&out).ok();
            let _ = std::fs::remove_dir_all(&dir);
            return d;
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    None
}

fn ask(data: &[u8], q: ManagedQuery<'_>) -> Result<String> {
    let out = managed_window(Path::new("calc.exe"), data, &q, None, 400)?;
    Ok(out.page(None, 400).text())
}

macro_rules! asm {
    () => {
        match build() {
            Some(d) => d,
            None => {
                eprintln!("skipped: no csc or mcs on this machine");
                return;
            }
        }
    };
}

#[test]
fn an_assembly_is_recognised_and_lists_its_types() {
    let d = asm!();
    assert!(is_managed(&d[..d.len().min(4096)]));
    let t = ask(&d, ManagedQuery::default()).unwrap();
    assert!(t.contains("Demo.Calc"), "{t}");
    assert!(t.contains("Demo.Calc/Inner"), "{t}");
    assert!(t.contains("entry point Demo.Calc.Main"), "{t}");
    assert!(t.contains("references mscorlib"), "{t}");
}

#[test]
fn methods_show_signatures_and_il_with_resolved_tokens() {
    let d = asm!();
    let t = ask(&d, ManagedQuery { target: Some("Calc.Greet"), ..Default::default() }).unwrap();
    assert!(t.contains("ldstr \"hello dotnet\""), "{t}");
    assert!(t.contains("call System.Console.WriteLine(string)"), "{t}");
    assert!(t.contains("0x0000"), "{t}");
    let t = ask(&d, ManagedQuery { target: Some("Demo.Calc"), ..Default::default() }).unwrap();
    assert!(t.contains("Add(int, int) -> int"), "{t}");
    assert!(t.contains("pinvoke kernel32.dll!GetTickCount"), "{t}");
}

#[test]
fn callers_strings_and_switch_work_on_il() {
    let d = asm!();
    let t = ask(&d, ManagedQuery { callers: Some("Demo.Calc.Add"), ..Default::default() }).unwrap();
    assert!(t.contains("call sites of Demo.Calc.Add"), "{t}");
    let t = ask(&d, ManagedQuery { strings: true, ..Default::default() }).unwrap();
    assert!(t.contains("hello dotnet") && t.contains("Demo.Calc.Greet"), "{t}");
    let t = ask(&d, ManagedQuery { target: Some("Calc.Pick"), ..Default::default() }).unwrap();
    assert!(t.contains("switch"), "{t}");
    let t = ask(&d, ManagedQuery { target: Some("Calc.Guard"), ..Default::default() }).unwrap();
    assert!(t.contains("finally"), "{t}");
}

#[test]
fn a_pe_without_a_clr_header_is_not_managed_and_garbage_is_an_error() {
    let mut pe = vec![0u8; 512];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c] = 0x80;
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    assert!(!is_managed(&pe));
    assert!(dotnet::parse(&pe).is_err());
    assert!(dotnet::parse(b"MZ\x90\0").is_err());
}

#[test]
fn truncated_and_flipped_assemblies_never_panic() {
    let d = asm!();
    for cut in (0..d.len()).step_by(37) {
        let _ = dotnet::parse(&d[..cut]);
    }
    let mut x = d.clone();
    for i in (0..x.len()).step_by(53) {
        x[i] ^= 0xA5;
        let _ = dotnet::parse(&x);
        x[i] ^= 0xA5;
    }
}

#[test]
fn a_real_framework_library_reads_in_bounded_time() {
    let Some(p) = std::fs::read_dir("/usr/lib/mono/4.5").ok().and_then(|mut r| {
        r.find_map(|e| e.ok().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n == "System.Core.dll")))
    }) else {
        eprintln!("skipped: no mono framework on this machine");
        return;
    };
    let d = std::fs::read(&p).unwrap();
    let t = std::time::Instant::now();
    let out = managed_window(&p, &d, &ManagedQuery::default(), None, 40).unwrap().page(None, 40).text();
    assert!(out.contains("types,"), "{out}");
    let out = managed_window(&p, &d, &ManagedQuery { strings: true, ..Default::default() }, None, 40).unwrap();
    assert!(!out.page(None, 40).text().is_empty());
    assert!(t.elapsed().as_secs() < 20);
}
