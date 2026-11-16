#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! `dump`: a function the program decrypts into fresh executable memory and a message it decrypts on the heap are
//! written to files and read back with `nexo-bin` at their own addresses, as the answer's `bin` call does; the
//! program's own image is rebuilt as an ELF file whose `.bss` holds what the program wrote there. Skipped without
//! the compiler.

use nexo_dbg::{open, split_words, Kind, Session};

/// `mov eax, edi; imul eax, eax, 7; add eax, 0x2a; ret`: f(3) is 63.
const CODE: [u8; 9] = [0x89, 0xf8, 0x6b, 0xc0, 0x07, 0x83, 0xc0, 0x2a, 0xc3];
const MSG: &[u8] = b"the secret is swordfish\0";
const KEY: u8 = 0x5a;

fn c_array(name: &str, plain: &[u8]) -> String {
    let bytes: Vec<String> = plain.iter().map(|b| format!("0x{:02x}", b ^ KEY)).collect();
    format!("static const unsigned char {name}[] = {{{}}};\n", bytes.join(", "))
}

fn source() -> String {
    let mut s = String::from("#include <stdio.h>\n#include <stdlib.h>\n#include <sys/mman.h>\n");
    s.push_str(&c_array("enc_code", &CODE));
    s.push_str(&c_array("enc_msg", MSG));
    s.push_str(
        r#"
unsigned int seen_at_run_time;
__attribute__((noinline)) int ready(unsigned char *code, char *msg) {
    return code[0] + msg[0];
}
int main(void) {
    unsigned char *code = mmap(0, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *msg = malloc(64);
    for (unsigned i = 0; i < sizeof enc_code; i++) code[i] = enc_code[i] ^ 0x5a;
    for (unsigned i = 0; i < sizeof enc_msg; i++) msg[i] = enc_msg[i] ^ 0x5a;
    seen_at_run_time = 0x1234abcd;
    ready(code, msg);
    printf("%d\n", ((int (*)(int))code)(3));
    return 0;
}
"#,
    );
    s
}

fn build() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("p.c"), source()).unwrap();
    let ok = std::process::Command::new("gcc")
        .args(["-g", "-O0", "-o", "p", "p.c"])
        .current_dir(dir.path())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("no gcc: skipped");
        return None;
    }
    let exe = dir.path().join("p");
    Some((dir, exe))
}

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

/// The text between the first backquotes after `read it`: the `bin` call the answer names.
fn bin_call(answer: &str) -> String {
    let line = answer.lines().find(|l| l.contains("read it")).unwrap_or_else(|| panic!("no read line: {answer}"));
    line.split('`').nth(1).unwrap_or_else(|| panic!("no call: {line}")).to_string()
}

/// The value of `--base` in a `bin` call.
fn base_of(call: &str) -> u64 {
    let w: Vec<&str> = call.split_whitespace().collect();
    let i = w.iter().position(|x| *x == "--base").expect("--base");
    nexo_bin::parse_addr(w[i + 1]).unwrap()
}

/// Stopped in `ready`, after the program decrypted its function and its message.
fn stopped_in_ready() -> Option<(tempfile::TempDir, Session)> {
    let (dir, exe) = build()?;
    let (mut s, _) = open(&Kind::Run { exe, args: vec![] }).expect("start");
    run(&mut s, "break ready");
    let stop = run(&mut s, "cont");
    assert!(stop.contains("ready"), "{stop}");
    Some((dir, s))
}

#[test]
fn a_function_decrypted_at_run_time_is_dumped_and_reads_back_as_code_at_its_address() {
    let Some((dir, mut s)) = stopped_in_ready() else { return };
    let path = dir.path().join("code.bin");
    let out = run(&mut s, &format!("dump $rdi 9 {}", path.display()));
    assert!(out.contains("wrote 9 bytes") && out.contains(&path.display().to_string()), "{out}");
    assert!(out.contains("rwx memory") && out.contains("no file"), "{out}");
    assert_eq!(std::fs::read(&path).unwrap(), CODE);

    let call = bin_call(&out);
    let base = base_of(&call);
    assert!(
        call.starts_with(&format!("bin fn {} {base:#x} --raw --base {base:#x} --arch x86-64", path.display())),
        "{call}"
    );
    let img = nexo_bin::Image::open_raw(&path, base, Some("x86-64")).unwrap();
    let text = nexo_bin::func_view(&img, &nexo_bin::FnQuery::new(&format!("{base:#x}"))).unwrap().text_all();
    assert!(text.contains("imul eax, 7") && text.contains("add eax, 0x2a"), "{text}");

    // writing the same file again says it replaced one
    let again = run(&mut s, &format!("dump $rdi..$rdi+9 {}", path.display()));
    assert!(again.contains("replaced the file"), "{again}");
    let end = run(&mut s, "cont");
    assert!(end.contains("exited with code 0"), "{end}");
    assert!(run(&mut s, "out --all").contains("63"));
}

#[test]
fn a_length_that_is_also_a_file_address_of_the_program_is_a_count_of_bytes() {
    let Some((dir, mut s)) = stopped_in_ready() else { return };
    // in gcc's default PIE, 0x1000 is `.init` and 0x400 lies in `.dynsym`: as places they would move by the load shift
    for (len, n) in [("4096", 4096usize), ("0x400", 0x400)] {
        let path = dir.path().join(format!("code-{len}.bin"));
        let out = run(&mut s, &format!("dump $rdi {len} {}", path.display()));
        assert!(out.contains(&format!("wrote {n} bytes")), "{out}");
        assert_eq!(std::fs::read(&path).unwrap()[..CODE.len()], CODE);
    }
}

#[test]
fn a_heap_buffer_decrypted_at_run_time_is_dumped_and_its_text_reads_back() {
    let Some((dir, mut s)) = stopped_in_ready() else { return };
    let path = dir.path().join("msg.bin");
    let out = run(&mut s, &format!("dump $rsi..$rsi+0x18 {}", path.display()));
    assert_eq!(std::fs::read(&path).unwrap(), MSG);
    assert!(out.contains("[heap]"), "{out}");
    let call = bin_call(&out);
    assert!(call.starts_with(&format!("bin strings {}", path.display())) && call.contains("--raw --base"), "{call}");
    let img = nexo_bin::Image::open_raw(&path, base_of(&call), Some("x86-64")).unwrap();
    assert_eq!(img.read_at(base_of(&call), 23), Some(&MSG[..23]));

    // nothing mapped there: no file, and the answer says why
    let e = s.exec(&split_words(&format!("dump 0x10..0x20 {}", dir.path().join("none.bin").display()))).unwrap_err();
    assert!(e.to_string().contains("cannot be read"), "{e}");
    assert!(!dir.path().join("none.bin").exists());
}

#[test]
fn the_program_image_is_rebuilt_as_an_elf_file_with_its_run_time_data() {
    let Some((dir, mut s)) = stopped_in_ready() else { return };
    let path = dir.path().join("p.dump");
    let out = run(&mut s, &format!("dump module p {}", path.display()));
    assert!(out.contains("is an ELF image") && out.contains("section headers, symbols"), "{out}");
    assert!(out.contains(&format!("`bin overview {}`", path.display())), "{out}");
    let img = nexo_bin::Image::open(&path).unwrap();
    assert!(matches!(img.format, nexo_bin::Format::Elf));
    let sym = |n: &str| img.symbols.iter().find(|x| x.name == n).unwrap_or_else(|| panic!("no {n}")).addr;
    // the code is the file's, and the .bss global holds the value the program wrote before the stop
    let text = nexo_bin::func_view(&img, &nexo_bin::FnQuery::new("ready")).unwrap().text_all();
    assert!(text.contains("ret"), "{text}");
    let v = img.read_at(sym("seen_at_run_time"), 4).expect("the .bss bytes are in the file");
    assert_eq!(v, 0x1234abcdu32.to_le_bytes());

    // a name that is no loaded module lists the ones that are
    let e = s.exec(&split_words("dump module nosuchlib")).unwrap_err().to_string();
    assert!(e.contains("no loaded module") && e.contains("p"), "{e}");
}
