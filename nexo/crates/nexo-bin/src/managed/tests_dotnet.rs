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
    private class Hidden { }
    protected class Prot { }
    internal class Intern { }
    public static void Wire() { Action a = Greet; a(); }
    public static string One() { return "x"; }
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
    assert!(t.lines().any(|l| l.contains("Demo.Calc.Guard") && l.contains("call Demo.Calc.Add")), "{t}");
    assert!(t.lines().any(|l| l.contains("Demo.Calc.Main")), "{t}");
    let t = ask(&d, ManagedQuery { target: Some("Demo.Calc"), ..Default::default() }).unwrap();
    assert!(t.lines().any(|l| l.contains("Limit: int") && l.contains("const")), "{t}");
    let t = ask(&d, ManagedQuery { grep: Some("Inner"), ..Default::default() }).unwrap();
    assert!(t.contains("Demo.Calc/Inner"), "{t}");
    let t = ask(&d, ManagedQuery { target: Some("Demo.Calc/Inner"), ..Default::default() }).unwrap();
    assert!(t.lines().any(|l| l.contains("field") && l.contains("V: int")), "{t}");
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

/// Run `f` on another thread and fail (instead of hanging the suite) when it takes over `secs`.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs)).unwrap_or_else(|_| panic!("did not finish in {secs} s"))
}

#[test]
fn an_array_rank_from_the_file_is_capped_not_allocated() {
    // rank 2^29 - 1: the old reader built a 512 MB string and looped that many times
    let sig = [0x14, 0x08, 0xDF, 0xFF, 0xFF, 0xFF, 0x00, 0x00];
    let t = within(5, move || dotnet::probe::read_type(&[], &sig));
    assert!(t.contains("not read") && t.len() < 200, "{t}");
    // size and bound counts above the rank are damage too
    let sig = [0x14, 0x08, 0x02, 0xDF, 0xFF, 0xFF, 0xFF, 0x00];
    let t = within(5, move || dotnet::probe::read_type(&[], &sig));
    assert!(t.contains("not read") && t.len() < 200, "{t}");
    // a real two-dimensional array still reads
    assert_eq!(dotnet::probe::read_type(&[], &[0x14, 0x08, 0x02, 0x00, 0x00]), "int[,]");
}

#[test]
fn typespecs_that_read_themselves_stop_at_the_work_budget() {
    // GENERICINST with 32 arguments, each the same TypeSpec again: 32^12 reads without a budget
    let mut gi = vec![0x15, 0x11, 0x06, 32];
    for _ in 0..32 {
        gi.extend_from_slice(&[0x11, 0x06]);
    }
    let t = within(5, move || dotnet::probe::read_type(&[gi], &[0x11, 0x06]));
    assert!(t.contains("not read") && t.len() < 100_000, "{} chars", t.len());
    // a function pointer with 255 parameters that each point back at it
    let mut fp = vec![0x1b, 0x00, 0x80, 0xFF, 0x08];
    for _ in 0..255 {
        fp.extend_from_slice(&[0x11, 0x06]);
    }
    let t = within(5, move || dotnet::probe::read_type(&[fp], &[0x11, 0x06]));
    assert!(t.contains("not read") && t.len() < 100_000, "{} chars", t.len());
}

fn clause(to: u32, tl: u32, ho: u32, hl: u32) -> Vec<u8> {
    let mut d = vec![0x41, 28, 0, 0];
    for v in [0u32, to, tl, ho, hl, 0] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d
}

#[test]
fn exception_clause_offsets_that_overflow_are_reported_not_wrapped() {
    let c = dotnet::probe::sections(&clause(0x10, 0x20, 0x30, 0x8), 0);
    assert_eq!(c.handlers.len(), 1);
    assert!(c.handlers[0].contains("0x0010..0x0030") && c.handlers[0].contains("0x0030..0x0038"), "{:?}", c.handlers);
    let c = dotnet::probe::sections(&clause(0xFFFF_FFF0, 0x20, 0xFFFF_FFF0, 0x20), 0);
    assert!(c.note.as_deref().is_some_and(|n| n.contains("overflow")), "{:?} {:?}", c.note, c.handlers);
}

/// A PE32 with sections `(va, vs, raw offset, raw size)`, a CLI header directory at `clr_rva` and
/// `cli_flags` at +16 of a CLI header placed at file offset `cli_off`.
fn tiny_pe(sections: &[(u32, u32, u32, u32)], clr_rva: u32, cli_off: usize, cli_flags: u32) -> Vec<u8> {
    let mut d = vec![0u8; 0x800];
    d[..2].copy_from_slice(b"MZ");
    d[0x3c] = 0x80;
    d[0x80..0x84].copy_from_slice(b"PE\0\0");
    d[0x86..0x88].copy_from_slice(&(sections.len() as u16).to_le_bytes());
    d[0x94..0x96].copy_from_slice(&224u16.to_le_bytes());
    let opt = 0x98;
    d[opt..opt + 2].copy_from_slice(&0x10bu16.to_le_bytes());
    d[opt + 92..opt + 96].copy_from_slice(&16u32.to_le_bytes());
    d[opt + 96 + 14 * 8..opt + 96 + 14 * 8 + 4].copy_from_slice(&clr_rva.to_le_bytes());
    d[opt + 96 + 14 * 8 + 4..opt + 96 + 14 * 8 + 8].copy_from_slice(&72u32.to_le_bytes());
    for (i, (va, vs, ro, rs)) in sections.iter().enumerate() {
        let s = opt + 224 + i * 40;
        for (o, v) in [(8, vs), (12, va), (16, rs), (20, ro)] {
            d[s + o..s + o + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    d[cli_off + 16..cli_off + 20].copy_from_slice(&cli_flags.to_le_bytes());
    d
}

#[test]
fn only_il_only_assemblies_are_claimed_and_mixed_mode_ones_are_named() {
    let secs = [(0x1000, 0x200, 0x200, 0x200)];
    assert_eq!(dotnet::probe::claims(&tiny_pe(&secs, 0x1000, 0x200, 1)), (true, false));
    // 0x2 (32-bit required) alone: native code is present, no ILONLY
    assert_eq!(dotnet::probe::claims(&tiny_pe(&secs, 0x1000, 0x200, 2)), (false, true));
    assert_eq!(dotnet::probe::claims(&tiny_pe(&secs, 0x1000, 0x200, 0)), (false, true));
    assert_eq!(dotnet::probe::claims(&tiny_pe(&secs, 0, 0x200, 0)), (false, false));
    assert!(!is_managed(&tiny_pe(&secs, 0x1000, 0x200, 0)));
    assert!(is_managed(&tiny_pe(&secs, 0x1000, 0x200, 1)));
}

#[test]
fn an_rva_past_the_sections_raw_data_does_not_land_in_the_next_section() {
    // section 1 holds 0x200 raw bytes of a 0x1000 virtual range; section 2's raw data follows it
    let secs = [(0x1000, 0x1000, 0x200, 0x200), (0x2000, 0x200, 0x400, 0x200)];
    let d = tiny_pe(&secs, 0x1300, 0x500, 1);
    let e = within(5, move || dotnet::probe::parse_err(&d));
    assert_eq!(e, "the CLI header is outside every section");
}

#[test]
fn rva_arithmetic_with_huge_section_values_does_not_panic() {
    let secs = [(0xFFFF_FF00, 0xFFFF_FFFF, 0xFFFF_FF00, 0xFFFF_FFFF)];
    let d = tiny_pe(&secs, 0xFFFF_FFF0, 0x200, 1);
    let _ = dotnet::probe::parse_err(&d);
    let _ = dotnet::probe::claims(&d);
}

#[test]
fn type_visibility_follows_the_nested_flags() {
    let v: Vec<&str> = (0..8).map(dotnet::probe::type_vis).collect();
    assert_eq!(
        v,
        [
            "internal",
            "public",
            "nested public",
            "nested private",
            "nested protected",
            "nested internal",
            "nested private protected",
            "nested protected internal"
        ]
    );
}

#[test]
fn a_real_assembly_labels_nested_types_by_their_nesting_visibility() {
    let d = asm!();
    let t = ask(&d, ManagedQuery { target: Some("Demo.Calc"), ..Default::default() }).unwrap();
    assert!(t.contains("class Demo.Calc [public"), "{t}");
    let all = ask(&d, ManagedQuery::default()).unwrap();
    assert!(all.contains("Demo.Calc/Inner") && all.contains("Demo.Calc/Hidden"), "{all}");
    let mut seen = Vec::new();
    for (n, want) in [
        ("Demo.Calc/Inner", "[nested public"),
        ("Demo.Calc/Hidden", "[nested private"),
        ("Demo.Calc/Prot", "[nested protected"),
        ("Demo.Calc/Intern", "[nested internal"),
    ] {
        let t = ask(&d, ManagedQuery { target: Some(n), ..Default::default() }).unwrap();
        seen.push((n, t.lines().next().unwrap_or("").to_string(), want));
    }
    for (n, head, want) in seen {
        assert!(head.contains(want), "{n}: {head}");
    }
}

#[test]
fn a_function_pointer_or_event_target_counts_as_a_reference_that_loads_the_address() {
    let d = asm!();
    let t = ask(&d, ManagedQuery { callers: Some("Demo.Calc.Greet"), ..Default::default() }).unwrap();
    assert!(t.lines().any(|l| l.contains("Demo.Calc.Guard") && l.contains("call ")), "{t}");
    assert!(
        t.lines().any(|l| l.contains("Demo.Calc.Wire") && l.contains("ldftn") && l.contains("loads address")),
        "{t}"
    );
}

/// Replace the first `ldstr <token> <next opcode>` found with the unknown opcode 0xA6 first.
fn break_ldstr(d: &mut [u8], next: u8) -> bool {
    let pos = d.windows(6).position(|w| w[0] == 0x72 && w[4] == 0x70 && w[5] == next);
    pos.map(|p| d[p] = 0xA6).is_some()
}

#[test]
fn strings_of_methods_whose_decoding_stopped_name_the_method_and_offset() {
    let mut d = asm!();
    assert!(break_ldstr(&mut d, 0x28), "the call after ldstr was not found");
    assert!(break_ldstr(&mut d, 0x0A), "the stloc after ldstr was not found");
    let t = ask(&d, ManagedQuery { strings: true, ..Default::default() }).unwrap();
    let line = t.lines().find(|l| l.contains("hello dotnet")).unwrap_or_else(|| panic!("{t}"));
    let ok = |l: &str| {
        l.contains("decoding of Demo.Calc.Greet stopped at offset 0x0001")
            && l.contains("decoding of Demo.Calc.One stopped at offset 0x0001")
            && l.contains("may load this string")
            && !l.contains("no code loads")
    };
    assert!(ok(line), "{t}");
    let line = t.lines().find(|l| l.ends_with("\"x\"")).unwrap_or_else(|| panic!("a 1 char string was dropped: {t}"));
    assert!(ok(line), "{t}");
}
