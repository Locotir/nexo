//! Bad and hostile input: files cut at many offsets, headers, tables and DWARF with fields set to huge or
//! circular values. Every op of `nexo bin` must finish in time, must not panic, must stay inside the page
//! budget and must say what is wrong when it refuses.
//!
//! Deterministic: a fixed seed and a fixed case count, no fuzzer. `NEXO_BIN_HARDEN_CASES=20000` (and
//! `NEXO_BIN_HARDEN_SEED=7`) run a longer round on a developer machine.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use object::{Object, ObjectSection};

use crate::fixtures;
use crate::*;

/// One case may take this long (debug build, tiny files): longer means a hang.
const CASE_TIME: Duration = Duration::from_secs(20);

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// A file to mutate and the byte ranges in it that matter: headers, tables, DWARF, relocations.
struct Seed {
    name: String,
    bytes: Vec<u8>,
    regions: Vec<(usize, usize)>,
}

fn u16_at(b: &[u8], o: usize) -> usize {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]) as usize)
}
fn u64_at(b: &[u8], o: usize) -> usize {
    b.get(o..o + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()) as usize)
}

fn seed(name: &str, bytes: Vec<u8>) -> Seed {
    let len = bytes.len();
    let mut regions = vec![(0, len.min(0x400))];
    if bytes.starts_with(b"\x7fELF") {
        let (phoff, shoff) = (u64_at(&bytes, 0x20), u64_at(&bytes, 0x28));
        regions.push((phoff, u16_at(&bytes, 0x38) * 56));
        regions.push((shoff, u16_at(&bytes, 0x3c) * 64));
    }
    if let Ok(o) = object::File::parse(&bytes[..]) {
        for s in o.sections() {
            let n = s.name().unwrap_or("");
            let wanted = n.starts_with(".debug")
                || n.starts_with(".rela")
                || n.starts_with(".rel")
                || matches!(
                    n,
                    ".symtab"
                        | ".strtab"
                        | ".dynsym"
                        | ".dynstr"
                        | ".dynamic"
                        | ".eh_frame"
                        | ".eh_frame_hdr"
                        | ".idata"
                        | ".edata"
                        | ".pdata"
                        | ".reloc"
                        | ".rdata"
                        | ".gnu.hash"
                        | ".hash"
                        | ".plt"
                        | ".got"
                        | ".init_array"
                        | ".note.gnu.build-id"
                );
            if let (true, Some((off, size))) = (wanted, s.file_range()) {
                regions.push((off as usize, (size as usize).min(2048)));
            }
        }
    }
    regions.retain(|&(o, n)| n > 0 && o < len);
    Seed { name: name.to_string(), bytes, regions }
}

fn seeds() -> Vec<Seed> {
    let mut v = vec![
        seed("elf-fixture", fixtures::elf64(true).bytes),
        seed("elf-xref-fixture", fixtures::elf64_xref().bytes),
        seed("pe-fixture", fixtures::pe64(true).bytes),
        seed("pe-xref-fixture", fixtures::pe64_xref().bytes),
    ];
    if cfg!(target_os = "linux") {
        if let Ok(b) = std::fs::read("/usr/bin/true") {
            if b.len() < 200_000 {
                v.push(seed("usr-bin-true", b));
            }
        }
        if let Some(b) = c_with_dwarf(&["-fno-pie", "-no-pie"]) {
            v.push(seed("c-dwarf", b));
        }
        if let Some(b) = c_with_dwarf(&["-fPIE", "-pie"]) {
            v.push(seed("c-pie", b));
        }
        if let Some(b) = c_with_dwarf(&["-shared", "-fPIC"]) {
            v.push(seed("c-shared", b));
        }
    }
    v
}

/// A small C program with debug info, when a C compiler is here. `None` (a skip) when it is not.
fn c_with_dwarf(flags: &[&str]) -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().ok()?;
    let src = dir.path().join("p.c");
    let out = dir.path().join("p");
    std::fs::write(
        &src,
        "#include <stdio.h>\nstruct pt { int x; long y; };\nstatic int sq(int v) { return v * v; }\n\
         int main(int argc, char **argv) { struct pt p = { argc, 2 }; printf(\"%d %s\\n\", sq(p.x), argv[0]); return 0; }\n",
    )
    .ok()?;
    let ok = std::process::Command::new("cc")
        .args(["-g", "-O1"])
        .args(flags)
        .arg("-o")
        .arg(&out)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        eprintln!("skipped the DWARF seed: the C compiler failed");
        return None;
    }
    let b = std::fs::read(&out).ok()?;
    // a dynamic C binary is about 20 KB with debug info; a static one would be far too big for this test
    (b.len() < 400_000).then_some(b)
}

const SMASH: [u64; 12] = [
    0,
    1,
    0xffff,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_ffff,
    0xffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
    u64::MAX,
    0x1000,
    0x40,
    4,
];

/// The k-th mutation of a seed.
fn mutate(s: &Seed, rng: &mut Rng, k: usize) -> (String, Vec<u8>) {
    let len = s.bytes.len();
    let mut b = s.bytes.clone();
    match k % 4 {
        0 => {
            // cut: at region edges first, then anywhere
            let at = match rng.below(3) {
                0 => {
                    let (o, n) = s.regions[rng.below(s.regions.len())];
                    if rng.below(2) == 0 {
                        o
                    } else {
                        (o + n).min(len)
                    }
                }
                _ => rng.below(len),
            };
            b.truncate(at);
            (format!("cut at {at:#x} of {len:#x}"), b)
        }
        1 => {
            let (o, n) = s.regions[rng.below(s.regions.len())];
            let at = (o + rng.below(n)).min(len - 1);
            let bit = 1u8 << rng.below(8);
            b[at] ^= bit;
            (format!("flip bit {bit:#x} at {at:#x}"), b)
        }
        _ => {
            let (o, n) = s.regions[rng.below(s.regions.len())];
            let width = [2usize, 4, 8][rng.below(3)];
            let at = (o + rng.below(n.max(1))) / width * width;
            let mut v = SMASH[rng.below(SMASH.len())];
            match rng.below(5) {
                0 => v = len as u64,
                1 => v = at as u64,
                2 => v = len as u64 + 1,
                _ => {}
            }
            if at + width <= len {
                b[at..at + width].copy_from_slice(&v.to_le_bytes()[..width]);
            }
            (format!("set {width} bytes at {at:#x} to {v:#x}"), b)
        }
    }
}

/// An answer's size is its budget: pages never exceed it, lines stay short.
fn check_out(op: &str, o: &Output) -> std::result::Result<(), String> {
    let p = o.page(None, DEFAULT_LINES);
    if p.lines.len() > DEFAULT_LINES {
        return Err(format!("{op}: {} lines on a page of {DEFAULT_LINES}", p.lines.len()));
    }
    if let Some(l) = p.lines.iter().find(|l| l.len() > 20_000) {
        return Err(format!("{op}: a line of {} bytes: {}", l.len(), l.chars().take(80).collect::<String>()));
    }
    Ok(())
}

fn check_err(op: &str, e: &Error) -> std::result::Result<(), String> {
    let t = e.to_string();
    if t.trim().is_empty() || t.len() > 600 {
        return Err(format!("{op}: an error of {} bytes: {}", t.len(), t.chars().take(100).collect::<String>()));
    }
    if t.contains("panicked") || t.contains("unwrap()") {
        return Err(format!("{op}: error text is a panic message: {t}"));
    }
    Ok(())
}

/// Every op of `nexo bin` that reads the file, on one (possibly broken) file.
fn battery(file: Vec<u8>) -> std::result::Result<(), String> {
    let img = match Image::from_bytes(file, PathBuf::from("/fx/mut")) {
        Ok(i) => i,
        Err(e) => return check_err("open", &e),
    };
    let entry = format!("{:#x}", img.entry);
    let first = img.funcs().first().map(|f| format!("{:#x}", f.start)).unwrap_or_else(|| entry.clone());
    let last = img.funcs().last().map(|f| format!("{:#x}", f.start)).unwrap_or_else(|| entry.clone());
    let to = format!("{:#x}", img.entry.wrapping_add(64));
    let none = std::path::Path::new("/nonexistent-nexo");
    let trace = std::env::var_os("NEXO_BIN_HARDEN_TRACE").is_some();
    macro_rules! step {
        ($op:expr, $e:expr) => {{
            if trace {
                eprintln!("op {}", $op);
            }
            match $e {
                Ok(o) => check_out($op, &o)?,
                Err(e) => check_err($op, &e)?,
            }
        }};
    }
    step!("info", Ok::<_, Error>(info(&img)));
    step!("overview", overview_window(&img, None, DEFAULT_LINES));
    for sort in ["addr", "size", "callers", "callees"] {
        step!("funcs", func_list_window(&img, FuncSort::parse(sort).unwrap(), None, 0, None, DEFAULT_LINES));
    }
    step!("strings", strings_window(&img, None, StrSort::parse("addr").unwrap(), None, DEFAULT_LINES));
    step!("crypto", crypto_window(&img, None, None, DEFAULT_LINES));
    step!("xref", xref_window(&img, &first, None, DEFAULT_LINES));
    step!("xref-entry", xref_window(&img, &entry, None, DEFAULT_LINES));
    step!("xref-str", xref_str_window(&img, "a", true, None, DEFAULT_LINES));
    step!("calls", calls_window(&img, &first, None, DEFAULT_LINES));
    for t in [&entry, &first, &last] {
        let q = FnQuery { budget: DEFAULT_LINES, ..FnQuery::new(t) };
        step!("fn", func_view(&img, &q));
        let q = FnQuery { to: Some(&to), budget: DEFAULT_LINES, ..FnQuery::new(t) };
        step!("fn-range", func_view(&img, &q));
        step!("decomp", decomp_window(&img, none, std::slice::from_ref(t), true, None, DEFAULT_LINES));
        step!("tree", tree_window(&img, t, false, 3, false, None, DEFAULT_LINES));
        step!("tree-up", tree_window(&img, t, true, 3, true, None, DEFAULT_LINES));
        step!("slice", slice_window(&img, t, Some("rax"), None, None, None, DEFAULT_LINES));
        let addr = parse_addr(t).unwrap_or(0);
        step!("bytes", crate::bytes(&img, addr, 64, ElemType::parse("u8").unwrap()));
        step!("emu", emulate(&img, &EmuSpec { steps: 2000, secs: 2, ..EmuSpec::new(t) }));
    }
    step!("path", path_window(&img, &first, &last, true, 3, None, DEFAULT_LINES));
    step!("find-imm", find_imm(&img, 1337));
    step!("find-bytes", find_bytes(&img, "48 89 ??"));
    step!("names", Ok::<_, Error>(names_list_window(&img, None, DEFAULT_LINES)));
    Ok(())
}

// where the last panic of a case happened
thread_local! {
    static WHERE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn install_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() != Some("harden-case") {
                return previous(info);
            }
            let bt = std::backtrace::Backtrace::force_capture().to_string();
            let frame = bt
                .lines()
                .map(str::trim)
                .find(|l| l.contains("nexo_bin::") && !l.contains("tests_harden"))
                .unwrap_or("")
                .to_string();
            let at = format!(
                "{} {frame}",
                info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default()
            );
            WHERE.with(|w| *w.borrow_mut() = at);
        }));
    });
}

/// Run one case in its own thread: a panic or a hang becomes a message.
fn run_case(bytes: Vec<u8>) -> std::result::Result<(), String> {
    install_hook();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("harden-case".into())
        .stack_size(16 << 20)
        .spawn(move || {
            let r = std::panic::catch_unwind(|| battery(bytes));
            let _ = tx.send(match r {
                Ok(r) => r,
                Err(p) => {
                    let m = p
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    Err(format!("PANIC {m} at {}", WHERE.with(|w| w.borrow().clone())))
                }
            });
        })
        .expect("thread");
    match rx.recv_timeout(CASE_TIME) {
        Ok(r) => r,
        Err(_) => Err(format!("HANG: no answer in {}s", CASE_TIME.as_secs())),
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
fn no_op_panics_or_hangs_on_mutated_files() {
    let total = env_num("NEXO_BIN_HARDEN_CASES", 320) as usize;
    let mut rng = Rng(env_num("NEXO_BIN_HARDEN_SEED", 0x6e65_786f) | 1);
    let seeds = seeds();
    let per = (total / seeds.len()).max(8);
    let mut failures: Vec<String> = Vec::new();
    for s in &seeds {
        // the unmutated file must pass too
        if let Err(m) = run_case(s.bytes.clone()) {
            failures.push(format!("{} unmutated: {m}", s.name));
        }
        for k in 0..per {
            let (what, bytes) = mutate(s, &mut rng, k);
            if let Err(m) = run_case(bytes.clone()) {
                // keep the file so the case can be replayed
                let dump = std::env::temp_dir().join(format!("nexo-harden-{}-{k}.bin", s.name));
                let _ = std::fs::write(&dump, &bytes);
                failures.push(format!("{}: {what}: {m} (file {})", s.name, dump.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// Replay one saved case: `NEXO_BIN_HARDEN_FILE=/tmp/nexo-harden-x.bin NEXO_BIN_HARDEN_TRACE=1 cargo test replay`.
#[test]
fn replay_a_saved_case() {
    let Some(path) = std::env::var_os("NEXO_BIN_HARDEN_FILE") else { return };
    let bytes = std::fs::read(path).expect("the case file");
    if let Err(m) = run_case(bytes) {
        panic!("{m}");
    }
}

#[test]
fn a_cut_file_says_how_long_it_is_and_what_its_headers_promise() {
    for s in seeds() {
        let len = s.bytes.len();
        for at in [0usize, 2, 3, 10, 40, 64, 100, 200, len / 2, len - 1] {
            if at >= len {
                continue;
            }
            match Image::from_bytes(s.bytes[..at].to_vec(), PathBuf::from("/x")) {
                Err(e) => {
                    let t = e.to_string();
                    let names_size = t.contains(&format!("{at} bytes")) || t.contains("empty");
                    assert!(names_size || t.contains("text file"), "{} cut at {at}: {t}", s.name);
                    if at >= 64 && !t.contains("text file") {
                        assert!(t.contains("offset") || t.contains("reaches byte"), "{} cut at {at}: {t}", s.name);
                    }
                }
                Ok(img) => {
                    // a file that still opens must say it is cut
                    assert!(!img.warnings.is_empty(), "{} cut at {at} opens without a warning", s.name);
                    assert!(info(&img).text_all().contains("warning"), "{} cut at {at}", s.name);
                }
            }
        }
    }
}

#[test]
fn a_good_file_has_no_warning() {
    for s in seeds() {
        let img = Image::from_bytes(s.bytes.clone(), PathBuf::from("/x")).unwrap();
        assert!(img.warnings.is_empty(), "{}: {:?}", s.name, img.warnings);
    }
}

#[test]
fn emu_refuses_a_section_that_claims_gigabytes() {
    let f = fixtures::elf64(false);
    let mut img = Image::from_bytes(f.bytes, PathBuf::from("/x")).unwrap();
    img.sections.iter_mut().filter(|s| s.alloc).for_each(|s| s.size = 1 << 40);
    let e = emulate(&img, &EmuSpec::new(&format!("{:#x}", f.a))).expect_err("refused").to_string();
    assert!(e.contains("MiB") && e.contains("section"), "{e}");
}

#[test]
fn slicing_at_the_start_of_a_function_does_not_panic() {
    let f = fixtures::elf64_xref();
    let img = Image::from_bytes(f.bytes, PathBuf::from("/x")).unwrap();
    for a in [f.a, f.b, f.c] {
        let _ = slice_window(&img, &format!("{a:#x}"), Some("rax"), None, None, None, DEFAULT_LINES);
    }
}

#[test]
fn a_prototype_with_non_ascii_text_is_read_or_refused_without_a_panic() {
    for text in [
        "int f(char *\u{fffd}x)",
        "int \u{e9}f(int a)",
        "\u{fffd}(",
        "int f(\u{4e2d}\u{6587} a, \u{fffd})",
        "\u{fffd}\u{fffd} f()",
    ] {
        let _ = crate::decomp::types::parse_proto(text);
    }
}
