//! The emulator against small C programs compiled here. Where the program can print the answer itself, the
//! test runs it too and checks that emu says the same.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{emulate, EmuSpec};
use crate::Image;

/// The key stream the fixture's `decode` uses, so the test can encode the secret.
fn encode(text: &str) -> Vec<u8> {
    let mut k: u8 = 0x5a;
    text.bytes()
        .map(|b| {
            let e = b ^ k;
            k = k.wrapping_mul(7).wrapping_add(3);
            e
        })
        .collect()
}

const SECRET: &str = "nexo emu works!";

fn source() -> String {
    let bytes: Vec<String> = encode(SECRET).iter().map(|b| format!("{b:#04x}")).collect();
    format!(
        r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

unsigned char secret[] = {{{}}};

__attribute__((noipa)) void decode(char *out, const unsigned char *in, int n) {{
    unsigned char k = 0x5a;
    for (int i = 0; i < n; i++) {{
        out[i] = in[i] ^ k;
        k = (unsigned char)(k * 7 + 3);
    }}
    out[n] = 0;
}}

__attribute__((noipa)) unsigned int fnv(const char *s) {{
    unsigned int h = 2166136261u;
    while (*s) {{
        h ^= (unsigned char)*s++;
        h *= 16777619u;
    }}
    return h;
}}

__attribute__((noipa)) int check(const char *s) {{
    if (s[0] != 'K') return 0;
    if (s[1] + s[2] != 200) return 0;
    if ((s[3] ^ 0x21) != 'a') return 0;
    return 1;
}}

__attribute__((noipa)) long mix(long a, long b, long c, long d, long e, long f, long g, long h) {{
    return a * 1 + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 - (a ^ h);
}}

__attribute__((noipa)) int ratio(int a, int b) {{
    return a / b;
}}

__attribute__((noipa)) int deref(int *p) {{
    return *p + 1;
}}

__attribute__((noipa)) int spin(volatile int *p) {{
    while (*p == 0) {{ }}
    return 1;
}}

__attribute__((noipa)) int greet(const char *name) {{
    int n = (int)strlen(name);
    printf("hi %s %d\n", name, n);
    return n;
}}

__attribute__((noipa)) void xor_buf(unsigned char *b, int n, unsigned char k) {{
    for (int i = 0; i < n; i++) b[i] ^= k;
}}

__attribute__((noipa)) char *shout(const char *s) {{
    size_t n = strlen(s);
    char *p = malloc(n + 1);
    for (size_t i = 0; i <= n; i++) p[i] = (char)toupper((unsigned char)s[i]);
    return p;
}}

__attribute__((noipa)) int twice(void) {{
    char *volatile p = malloc(8);
    free(p);
    free(p);
    return 0;
}}

__attribute__((noipa)) unsigned int rolled(void) {{
    return (unsigned int)rand();
}}

int main(int argc, char **argv) {{
    if (argc > 2 && !strcmp(argv[1], "fnv")) {{ printf("%u\n", fnv(argv[2])); return 0; }}
    if (argc > 2 && !strcmp(argv[1], "check")) {{ printf("%d\n", check(argv[2])); return 0; }}
    if (argc > 2 && !strcmp(argv[1], "greet")) {{ greet(argv[2]); return 0; }}
    if (argc > 1 && !strcmp(argv[1], "mix")) {{ printf("%ld\n", mix(1, 2, 3, 4, 5, 6, 7, 8)); return 0; }}
    if (argc > 2 && !strcmp(argv[1], "shout")) {{ puts(shout(argv[2])); return 0; }}
    if (argc > 1 && !strcmp(argv[1], "twice")) {{ return twice(); }}
    if (argc > 1 && !strcmp(argv[1], "rand")) {{ printf("%u\n", rolled()); return 0; }}
    if (argc > 1 && !strcmp(argv[1], "spin")) {{ int z = 0; return spin(&z) + ratio(argc, argc) + deref(&z); }}
    char buf[64];
    decode(buf, secret, sizeof secret);
    puts(buf);
    return 0;
}}
"#,
        bytes.join(", ")
    )
}

/// Compile the fixture with `opt` (-O0, -O2...); None (with a message) when there is no C compiler.
fn build(dir: &Path, opt: &str) -> Option<PathBuf> {
    let src = dir.join("emu.c");
    std::fs::write(&src, source()).ok()?;
    let exe = dir.join(format!("emu{opt}"));
    let ok = ["cc", "gcc"].iter().any(|cc| {
        Command::new(cc)
            .args([opt, "-fcf-protection=full", "-o"])
            .arg(&exe)
            .arg(&src)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    // on Windows `-o emu-O0` makes `emu-O0.exe`, so the file named here is missing: skip there too
    if !ok || !exe.is_file() {
        eprintln!("skipped: no C compiler that builds the emu fixture as an ELF file here");
        return None;
    }
    Some(exe)
}

/// What the real program prints for these arguments (trimmed).
fn real(exe: &Path, args: &[&str]) -> String {
    let o = Command::new(exe).args(args).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn run(img: &Image, target: &str, args: &[&str], edit: impl FnOnce(&mut EmuSpec)) -> (String, serde_json::Value) {
    let mut spec = EmuSpec::new(target);
    spec.args = args.iter().map(|s| s.to_string()).collect();
    edit(&mut spec);
    let out = emulate(img, &spec).unwrap();
    let page = out.page(None, 400);
    (out.text_all(), page.value)
}

fn returned(v: &serde_json::Value) -> u64 {
    assert_eq!(v["stop"]["kind"], "returned", "{v:#}");
    v["return"].as_u64().unwrap()
}

const OPTS: [&str; 3] = ["-O0", "-O1", "-O2"];

#[test]
fn emu_decodes_the_rolling_xor_string_into_the_buffer_it_is_given() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        let secret = img.symbols.iter().find(|s| s.name == "secret").unwrap().addr;
        let n = SECRET.len().to_string();
        let (text, v) = run(&img, "decode", &["buf:64", &format!("{secret:#x}"), &n], |_| {});
        println!("{opt}\n{text}");
        assert_eq!(v["stop"]["kind"], "returned", "{text}");
        assert!(text.contains(&format!("\"{SECRET}\"")), "{text}");
        assert!(text.contains("arg1 buf"), "{text}");
        assert_eq!(real(&exe, &[]), SECRET);
    }
}

#[test]
fn emu_returns_the_same_hash_as_the_real_program() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        for word in ["hello", "nexo", ""] {
            let (text, v) = run(&img, "fnv", &[&format!("str:{word}")], |_| {});
            let want: u64 = real(&exe, &["fnv", word]).parse().unwrap();
            assert_eq!(returned(&v) & 0xffff_ffff, want, "{opt} {word:?}\n{text}");
        }
    }
}

#[test]
fn emu_shows_which_input_the_validator_accepts() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        for word in ["Kdd@", "Kdda", "kdd@", "Kcd@", "Ke c"] {
            let (text, v) = run(&img, "check", &[&format!("str:{word}")], |_| {});
            let want: u64 = real(&exe, &["check", word]).parse().unwrap();
            assert_eq!(returned(&v) & 0xffff_ffff, want, "{opt} {word:?}\n{text}");
        }
    }
}

#[test]
fn emu_passes_arguments_past_the_sixth_on_the_stack() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        let (text, v) = run(&img, "mix", &["1", "2", "3", "4", "5", "6", "7", "8"], |_| {});
        let want: i64 = real(&exe, &["mix"]).parse().unwrap();
        assert_eq!(returned(&v) as i64, want, "{opt}\n{text}");
    }
}

#[test]
fn emu_stops_on_an_import_it_has_no_stub_for_and_a_stub_value_gets_past_it() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-O0") else { return };
    let img = Image::open(&exe).unwrap();
    let (text, v) = run(&img, "rolled", &[], |_| {});
    assert_eq!(v["stop"]["kind"], "unknown_import", "{text}");
    assert!(text.contains("import `rand`") && text.contains("--stub rand=0"), "{text}");
    assert!(text.contains("rolled+"), "the call site is named: {text}");
    let (text, v) = run(&img, "rolled", &[], |s| s.stubs = vec!["rand=1234".into()]);
    assert_eq!(returned(&v), 1234, "{text}");
}

#[test]
fn emu_reports_a_divide_error_a_null_read_and_a_loop_that_does_not_end() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-O1") else { return };
    let img = Image::open(&exe).unwrap();
    let (text, v) = run(&img, "ratio", &["7", "0"], |_| {});
    assert_eq!(v["stop"]["kind"], "divide_error", "{text}");
    assert!(text.contains("ratio+"), "{text}");
    let (text, v) = run(&img, "ratio", &["-7", "2"], |_| {});
    assert_eq!(returned(&v) as u32 as i32, -3, "{text}");
    let (text, v) = run(&img, "deref", &["0"], |_| {});
    assert_eq!(v["stop"]["kind"], "fault", "{text}");
    assert!(text.contains("read of unmapped memory 0x0"), "{text}");
    let (text, v) = run(&img, "spin", &["buf:4"], |s| s.steps = 5000);
    assert_eq!(v["stop"]["kind"], "step_cap", "{text}");
    assert!(text.contains("--steps 50000"), "{text}");
}

#[test]
fn emu_stops_at_the_address_given_with_to() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-O0") else { return };
    let img = Image::open(&exe).unwrap();
    let f = crate::ops::resolve(&img, "fnv").unwrap();
    // the first conditional jump of fnv: the loop test
    let mut stop_at = None;
    crate::disasm::walk(img.read_at(f.start, f.size() as usize).unwrap(), f.start, |ins, _| {
        if stop_at.is_none() && ins.flow_control() == iced_x86::FlowControl::ConditionalBranch {
            stop_at = Some(ins.ip());
        }
    });
    let to = stop_at.unwrap();
    let (text, v) = run(&img, "fnv", &["str:ab"], |s| s.to = Some(to));
    assert_eq!(v["stop"]["kind"], "reached", "{text}");
    assert_eq!(v["stop"]["rip"].as_u64(), Some(to), "{text}");
}

#[test]
fn emu_prints_what_the_program_prints_through_the_printf_and_strlen_stubs() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        let (text, v) = run(&img, "greet", &["str:nexo"], |_| {});
        assert_eq!(returned(&v), 4, "{opt}\n{text}");
        assert_eq!(v["stdout"].as_str().unwrap().trim(), real(&exe, &["greet", "nexo"]), "{opt}\n{text}");
        assert!(text.contains("strlen("), "the call is in the log: {text}");
    }
}

#[test]
fn emu_follows_malloc_and_toupper_and_shows_the_heap_block_it_filled() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        let (text, v) = run(&img, "shout", &["str:quiet please"], |_| {});
        returned(&v);
        let want = real(&exe, &["shout", "quiet please"]);
        assert!(text.contains(&format!("\"{want}\"")) && text.contains("heap block #1"), "{opt}\n{text}");
    }
}

#[test]
fn emu_stops_on_a_double_free_and_names_the_block() {
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = build(dir.path(), "-O1") else { return };
    let img = Image::open(&exe).unwrap();
    let (text, v) = run(&img, "twice", &[], |_| {});
    assert_eq!(v["stop"]["kind"], "abort", "{text}");
    assert!(text.contains("heap block #1 was already freed"), "{text}");
}

#[test]
fn emu_runs_the_vectorized_xor_loop_with_its_sse_instructions() {
    let dir = tempfile::tempdir().unwrap();
    for opt in OPTS {
        let Some(exe) = build(dir.path(), opt) else { return };
        let img = Image::open(&exe).unwrap();
        let plain: Vec<u8> = (0..45u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        let hex: Vec<String> = plain.iter().map(|b| format!("{b:02x}")).collect();
        let (text, v) = run(&img, "xor_buf", &[&format!("hex:{}", hex.join("")), "45", "0x5c"], |s| s.dump = 64);
        returned(&v);
        let want: Vec<String> = plain.iter().take(16).map(|b| format!("{:02x}", b ^ 0x5c)).collect();
        assert!(text.contains(&want.join(" ")), "{opt}\n{text}");
        if opt == "-O2" {
            let (text, _) = run(&img, "xor_buf", &["buf:64", "64", "0x5c"], |s| s.trace = 200);
            assert!(text.contains("pxor") || text.contains("xorps"), "the loop is vectorized at -O2: {text}");
        }
    }
}
