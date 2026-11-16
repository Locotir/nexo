//! The line at the end of a `bin` answer that names the `debug` call stopping where the answer looked: a fact at
//! the moment of use, not a rule. Only for a file the `debug` tool can run on this host (an x86-64 ELF program on
//! Linux x86-64, not a shared library), and only for an op that is about one place.

use std::io::Read;
use std::path::Path;

use nexo_bin::{Image, Place};

use super::BinOp;

/// What starts the line; the tests and the curator find it by this.
pub const HEAD: &str = "to see it live: debug";

/// The one place the op looked at, as the text the op was given (`main`, `0x401a20`, `main+0x10`).
fn target_of(op: &BinOp) -> Option<&str> {
    match op {
        BinOp::Fn { target, .. } | BinOp::Tree { target, .. } | BinOp::Calls { target, .. } => Some(target),
        BinOp::Slice { at, .. } => Some(at),
        BinOp::Xref { target, .. } => target.as_deref(),
        BinOp::Decomp { targets, .. } => targets.first().map(String::as_str),
        // the map of the file shows `main`
        BinOp::Overview { .. } => Some("main"),
        _ => None,
    }
}

/// Whether `debug` can run this file here, with an execute bit: on Linux x86-64 an ELF64 x86-64 program (not a
/// shared library, not a core file or an object); on macOS x86-64 a Mach-O program (`MH_EXECUTE`, not a dylib or
/// a bundle) with code for this processor, thin or one slice of a universal file. Nowhere else: the gate is
/// `nexo_dbg::runs_here`, the one the `debug` tool and `nexo dbg` use.
pub fn runnable(path: &Path) -> bool {
    if !nexo_dbg::runs_here() {
        return false;
    }
    if cfg!(target_os = "macos") {
        return macho_program(path, HOST_CPU);
    }
    if !cfg!(target_os = "linux") {
        return false;
    }
    elf_program(path)
}

/// The Mach-O CPU type of this host (`CPU_TYPE_X86_64` or `CPU_TYPE_ARM64`).
const HOST_CPU: u32 = if cfg!(target_arch = "aarch64") { 0x0100_000c } else { 0x0100_0007 };

/// Whether `path` is a Mach-O program a `cpu` machine runs, with an execute bit: a thin 64-bit `MH_EXECUTE` file of
/// that CPU, or a universal file whose slice for that CPU is one.
fn macho_program(path: &Path, cpu: u32) -> bool {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    if !executable(&f) {
        return false;
    }
    let mut head = Vec::with_capacity(4096);
    if (&mut f).take(4096).read_to_end(&mut head).is_err() || !nexo_bin::macho_runs_on(&head, cpu) {
        return false;
    }
    let be = |b: &[u8], o: usize| b.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
    let le = |b: &[u8], o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    // a universal file: read the header of the slice for this CPU
    let slice = match be(&head, 0) {
        Some(m @ (0xcafe_babe | 0xcafe_babf)) => {
            let (wide, n) = (m == 0xcafe_babf, be(&head, 4).unwrap_or(0) as usize);
            let entry = if wide { 32 } else { 20 };
            let Some(i) = (0..n).find(|i| be(&head, 8 + i * entry) == Some(cpu)) else { return false };
            let at = 8 + i * entry + 8;
            let off = if wide {
                head.get(at..at + 8).map(|x| u64::from_be_bytes(x.try_into().unwrap_or([0; 8])))
            } else {
                be(&head, at).map(u64::from)
            };
            let mut h = [0u8; 16];
            match off {
                Some(o) if f.seek(SeekFrom::Start(o)).is_ok() && f.read_exact(&mut h).is_ok() => h.to_vec(),
                _ => return false,
            }
        }
        _ => head,
    };
    // MH_MAGIC_64, the CPU, and the file type MH_EXECUTE
    le(&slice, 0) == Some(0xfeed_facf) && le(&slice, 4) == Some(cpu) && le(&slice, 12) == Some(2)
}

/// Whether the file has an execute bit (always, off Unix).
fn executable(f: &std::fs::File) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = f;
        true
    }
}

/// Whether `path` is an ELF64 x86-64 program with an execute bit. A dynamic program has an interpreter; a static
/// one is `ET_EXEC`.
fn elf_program(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut h = [0u8; 64];
    if f.read_exact(&mut h).is_err() || &h[..4] != b"\x7fELF" || h[4] != 2 || h[5] != 1 {
        return false;
    }
    let u16_at = |o: usize| u16::from_le_bytes([h[o], h[o + 1]]);
    let u64_at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap_or([0; 8]));
    let (kind, machine, phoff, phsize, phnum) = (u16_at(16), u16_at(18), u64_at(32), u16_at(54), u16_at(56));
    if machine != 62 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if f.metadata().map(|m| m.permissions().mode() & 0o111 == 0).unwrap_or(true) {
            return false;
        }
    }
    match kind {
        2 => true,
        // a position independent program names its loader; a shared library does not
        3 => {
            use std::io::{Seek, SeekFrom};
            if phsize < 56 || phnum == 0 || phnum > 256 || f.seek(SeekFrom::Start(phoff)).is_err() {
                return false;
            }
            (0..phnum).any(|_| {
                let mut e = vec![0u8; phsize as usize];
                f.read_exact(&mut e).is_ok() && u32::from_le_bytes([e[0], e[1], e[2], e[3]]) == 3
            })
        }
        _ => false,
    }
}

/// The line for an answer of `op` that gave `text`, or `None`: the op is not about one place, the file cannot be
/// run here, the answer is JSON, or the person's `--lines` was already filled.
pub fn line(op: &BinOp, text: &str) -> Option<String> {
    let c = op.common();
    let file = op.file();
    if c.json || matches!(c.lines, Some(nexo_bin::Lines::Rows(n)) if text.lines().count() >= n.max(nexo_bin::MIN_LINES))
    {
        return None;
    }
    let target = target_of(op)?;
    if !runnable(file) {
        return None;
    }
    let img = Image::open(file).ok()?;
    let Place::Code(addr) = nexo_bin::place_of(&img, target).ok()? else { return None };
    let path = file.to_string_lossy();
    if path.contains(['"', '\\', '\n', ';']) {
        return None;
    }
    let shown = if path.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c)) {
        path.to_string()
    } else {
        format!("\"{path}\"")
    };
    Some(format!("{HEAD} `run {shown}; break {}; cont`", break_at(&img, addr)))
}

/// What `break` takes for `addr`. In a program with line info, the name of the function that starts there (when
/// the name gives back only that address): the debugger then stops past the prologue, where the parameters and
/// locals hold their values, while `*addr` stops before the prologue has stored them. Else `*addr`, which stops
/// where the name would.
fn break_at(img: &Image, addr: u64) -> String {
    if !img.sections.iter().any(|s| s.name == ".debug_line" && s.size > 0) {
        return format!("*{addr:#x}");
    }
    let plain = |n: &str| {
        let mut c = n.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
    };
    match nexo_bin::nearest_symbol(img, addr) {
        Some((n, 0)) if plain(&n) && matches!(nexo_bin::place_of(img, &n), Ok(Place::Code(a)) if a == addr) => n,
        _ => format!("*{addr:#x}"),
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod tests {
    use super::*;
    use clap::Parser;

    /// A C file built with `cc` into `dir`; `None` when there is no compiler here.
    pub(crate) fn build(dir: &Path, name: &str, flags: &[&str]) -> Option<std::path::PathBuf> {
        let src = dir.join(format!("{name}.c"));
        std::fs::write(&src, "int helper(int a) { return a * 3; }\nint main(void) { return helper(2) - 6; }\n").ok()?;
        let out = dir.join(name);
        let ok = std::process::Command::new("cc")
            .args(flags)
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(out)
    }

    fn op_of(args: &[&str]) -> BinOp {
        let mut argv = vec!["nexo", "bin"];
        argv.extend_from_slice(args);
        match crate::cli::Args::try_parse_from(argv).expect("parses").cmd {
            Some(crate::cli::Cmd::Bin(b)) => b.op.expect("an op"),
            other => panic!("not a bin command: {other:?}"),
        }
    }

    #[test]
    fn a_place_in_a_program_that_runs_here_gets_the_call_that_stops_there() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path(), "prog", &["-g0"]) else { return eprintln!("skipped: no cc") };
        let p = exe.display().to_string();
        let main = match nexo_bin::place_of(&Image::open(&exe).unwrap(), "main").unwrap() {
            Place::Code(a) => a,
            other => panic!("{other:?}"),
        };
        for op in [
            vec!["fn", &p, "main"],
            vec!["decomp", &p, "main"],
            vec!["xref", &p, "main"],
            vec!["calls", &p, "main"],
            vec!["overview", &p],
        ] {
            let l = line(&op_of(&op), "answer").unwrap_or_else(|| panic!("{op:?}"));
            assert_eq!(l, format!("{HEAD} `run {p}; break *{main:#x}; cont`"));
        }
        // an address names itself, even inside a function
        let l = line(&op_of(&["fn", &p, &format!("{:#x}", main + 4)]), "x").unwrap();
        assert!(l.ends_with(&format!("break *{:#x}; cont`", main + 4)), "{l}");
        // ops that are not about one place, JSON, and a name the file does not have
        assert!(line(&op_of(&["info", &p]), "x").is_none());
        assert!(line(&op_of(&["strings", &p]), "x").is_none());
        assert!(line(&op_of(&["fn", &p, "main", "--json"]), "x").is_none());
        assert!(line(&op_of(&["fn", &p, "nosuchfunction"]), "x").is_none());
    }

    #[test]
    fn with_line_info_a_function_start_is_named_so_the_stop_is_past_the_prologue() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path(), "prog", &["-g", "-O0"]) else { return eprintln!("skipped: no cc") };
        let p = exe.display().to_string();
        let helper = match nexo_bin::place_of(&Image::open(&exe).unwrap(), "helper").unwrap() {
            Place::Code(a) => a,
            other => panic!("{other:?}"),
        };
        // by name or by its address, the start of a function is its name; inside it, the address
        for t in ["helper".to_string(), format!("{helper:#x}")] {
            let l = line(&op_of(&["fn", &p, &t]), "x").unwrap();
            assert_eq!(l, format!("{HEAD} `run {p}; break helper; cont`"));
        }
        let l = line(&op_of(&["fn", &p, &format!("{:#x}", helper + 4)]), "x").unwrap();
        assert!(l.ends_with(&format!("break *{:#x}; cont`", helper + 4)), "{l}");
    }

    #[test]
    fn an_answer_that_fills_the_lines_asked_for_gets_no_line_and_one_that_fits_does() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path(), "prog", &["-g0"]) else { return eprintln!("skipped: no cc") };
        let p = exe.display().to_string();
        let op = op_of(&["fn", &p, "main", "--lines", "10"]);
        let text = |n: usize| vec!["row"; n].join("\n");
        assert!(line(&op, &text(10)).is_none());
        assert!(line(&op, &text(9)).is_some());
        // without --lines the default page is not the person's size
        assert!(line(&op_of(&["fn", &p, "main"]), &text(120)).is_some());
    }

    #[test]
    fn nothing_for_a_file_debug_cannot_run_here() {
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = build(dir.path(), "prog", &["-g0"]) else { return eprintln!("skipped: no cc") };
        assert!(runnable(&exe));
        // a shared library
        let Some(so) = build(dir.path(), "libprog.so", &["-shared", "-fPIC"]) else { return };
        assert!(!runnable(&so));
        assert!(line(&op_of(&["fn", &so.display().to_string(), "helper"]), "x").is_none());
        // a program without the execute bit
        let plain = dir.path().join("plain");
        std::fs::copy(&exe, &plain).unwrap();
        let mut perm = std::fs::metadata(&plain).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o644);
        std::fs::set_permissions(&plain, perm).unwrap();
        assert!(!runnable(&plain));
        // another architecture (the machine field of the header), a 32-bit class, an object file
        let bytes = std::fs::read(&exe).unwrap();
        for (what, at, value) in [("arm64", 18, 183u8), ("32-bit", 4, 1), ("object", 16, 1)] {
            let mut b = bytes.clone();
            b[at] = value;
            let f = dir.path().join(what);
            std::fs::write(&f, &b).unwrap();
            let mut perm = std::fs::metadata(&f).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
            std::fs::set_permissions(&f, perm).unwrap();
            assert!(!runnable(&f), "{what}");
        }
        // a PE file, a core-like file and a missing one
        let pe = dir.path().join("a.exe");
        std::fs::write(&pe, nexo_bin::fixtures::pe64(false).bytes).unwrap();
        assert!(!runnable(&pe) && !runnable(&dir.path().join("none")));
        assert!(line(&op_of(&["fn", &pe.display().to_string(), "0x1000"]), "x").is_none());
    }
}

#[cfg(all(test, unix))]
mod macho_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const X86: u32 = 0x0100_0007;
    const ARM: u32 = 0x0100_000c;

    fn file(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    /// A thin 64-bit Mach-O header: magic, CPU, subtype, file type.
    fn thin(cpu: u32, filetype: u32) -> Vec<u8> {
        [0xfeed_facfu32, cpu, 3, filetype].iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A universal file holding `slices` (CPU, file type), each header at 0x1000 * (index + 1).
    fn fat(slices: &[(u32, u32)]) -> Vec<u8> {
        let mut b = [0xcafe_babeu32.to_be_bytes(), (slices.len() as u32).to_be_bytes()].concat();
        for (i, (cpu, _)) in slices.iter().enumerate() {
            for w in [*cpu, 3, 0x1000 * (i as u32 + 1), 16, 12] {
                b.extend(w.to_be_bytes());
            }
        }
        for (i, (cpu, ft)) in slices.iter().enumerate() {
            b.resize(0x1000 * (i + 1), 0);
            b.extend(thin(*cpu, *ft));
        }
        b
    }

    #[test]
    fn a_mach_o_program_runs_on_its_cpu_with_an_execute_bit_and_a_library_or_another_cpu_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert!(macho_program(&file(d, "prog", &thin(X86, 2), 0o755), X86));
        assert!(!macho_program(&file(d, "prog-arm", &thin(ARM, 2), 0o755), X86));
        assert!(!macho_program(&file(d, "lib.dylib", &thin(X86, 6), 0o755), X86));
        assert!(!macho_program(&file(d, "bundle", &thin(X86, 8), 0o755), X86));
        assert!(!macho_program(&file(d, "plain", &thin(X86, 2), 0o644), X86));
        let both = file(d, "universal", &fat(&[(X86, 2), (ARM, 2)]), 0o755);
        assert!(macho_program(&both, X86) && macho_program(&both, ARM));
        // the slice for this CPU is the one read: here only the arm64 slice is a program
        let mixed = file(d, "mixed", &fat(&[(X86, 6), (ARM, 2)]), 0o755);
        assert!(!macho_program(&mixed, X86) && macho_program(&mixed, ARM));
        assert!(!macho_program(&file(d, "only-arm", &fat(&[(ARM, 2)]), 0o755), X86));
        // a Java class file shares the universal magic
        assert!(!macho_program(&file(d, "A.class", &[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52, 0, 0], 0o755), X86));
        assert!(!macho_program(&file(d, "elf", b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0", 0o755), X86));
        assert!(!macho_program(&d.join("none"), X86));
    }

    /// On a Mac the gate takes a program `cc` built and leaves its dylib; elsewhere it takes no Mach-O file at all.
    #[test]
    fn runnable_takes_a_mach_o_program_only_on_a_mac() {
        let dir = tempfile::tempdir().unwrap();
        let prog = file(dir.path(), "prog", &thin(X86, 2), 0o755);
        assert_eq!(runnable(&prog), cfg!(all(target_os = "macos", target_arch = "x86_64")));
        if !cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            return;
        }
        let src = dir.path().join("p.c");
        std::fs::write(&src, "int helper(int a) { return a * 3; }\nint main(void) { return helper(2) - 6; }\n")
            .unwrap();
        let cc = |out: &str, flags: &[&str]| {
            let o = dir.path().join(out);
            let ok = std::process::Command::new("cc").args(flags).arg("-o").arg(&o).arg(&src).status();
            ok.is_ok_and(|s| s.success()).then_some(o)
        };
        let Some(exe) = cc("built", &[]) else { return eprintln!("skipped: no cc") };
        assert!(runnable(&exe));
        let lib = cc("libp.dylib", &["-dynamiclib"]).unwrap();
        assert!(!runnable(&lib));
        let p = exe.display().to_string();
        let l = line(&op_from(&["fn", &p, "main"]), "x").expect("a line for a program that runs here");
        assert!(l.starts_with(&format!("{HEAD} `run {p}; break ")), "{l}");
    }

    fn op_from(args: &[&str]) -> BinOp {
        use clap::Parser;
        let mut argv = vec!["nexo", "bin"];
        argv.extend_from_slice(args);
        match crate::cli::Args::try_parse_from(argv).expect("parses").cmd {
            Some(crate::cli::Cmd::Bin(b)) => b.op.expect("an op"),
            other => panic!("not a bin command: {other:?}"),
        }
    }
}
