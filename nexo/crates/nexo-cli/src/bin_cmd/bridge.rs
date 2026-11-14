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

/// Whether `debug` can run this file here: an ELF64 x86-64 program (not a shared library, not a core file or an
/// object) with an execute bit, on a Linux x86-64 host. A dynamic program has an interpreter; a static one is
/// `ET_EXEC`.
pub fn runnable(path: &Path) -> bool {
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        return false;
    }
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
    Some(format!("{HEAD} `run {shown}; break *{addr:#x}; cont`"))
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
