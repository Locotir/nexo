//! The live process a command looks at (`pmap 1234`, `lsof -p 1234`, `ps -fp 1234`, `gdb -p 1234`,
//! `strace -p1234`, `cat /proc/1234/maps`), and the program that process runs.

use std::path::PathBuf;

use super::words::program;

/// The programs whose `-p N` (or `--pid N`, `--attach-pid N`) names a process to look at.
const PID_OPTION: [&str; 6] = ["gdb", "lldb", "strace", "ltrace", "lsof", "ps"];

/// The process number the command `words` looks at, as it wrote it: the `-p` of a tracer or a lister, the
/// number `pmap` takes, or the number of a `/proc/N/...` word of any command.
pub fn pid_of(words: &[String]) -> Option<String> {
    let (first, args) = words.split_first()?;
    let prog = program(first);
    let digits = |s: &str| !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit());
    let from_proc = args.iter().find_map(|a| {
        let n = a.strip_prefix("/proc/")?.split('/').next()?;
        digits(n).then(|| n.to_string())
    });
    if prog == "pmap" {
        return args.iter().find(|a| digits(a)).cloned().or(from_proc);
    }
    if !PID_OPTION.contains(&prog.as_str()) {
        return from_proc;
    }
    let after = |i: usize| args.get(i + 1).filter(|n| digits(n)).cloned();
    args.iter()
        .enumerate()
        .find_map(|(i, a)| {
            if matches!(a.as_str(), "-p" | "-pid" | "--pid" | "--attach-pid" | "--attach") {
                return after(i);
            }
            if let Some(n) = a.strip_prefix("--pid=").or_else(|| a.strip_prefix("--attach=")) {
                return digits(n).then(|| n.to_string());
            }
            // a cluster of short options that ends with `p`, its number joined (`-p1234`, `-fp1234`) or next
            // (`-fp 1234`)
            let r = a.strip_prefix('-').filter(|r| !r.starts_with('-'))?;
            let at = r.find('p')?;
            if !r[..at].bytes().all(|b| b.is_ascii_alphabetic()) {
                return None;
            }
            let rest = &r[at + 1..];
            if rest.is_empty() {
                after(i)
            } else {
                digits(rest).then(|| rest.to_string())
            }
        })
        .or(from_proc)
}

/// The program the process `pid` runs, when this user may look at it: reading `/proc/N/exe` fails for a process
/// of another user, for one that is gone, and where there is no `/proc` (Windows, macOS).
pub fn exe_of(pid: &str) -> Option<PathBuf> {
    // a program removed since it started reads as `/x/p (deleted)`, a name no call can open
    std::fs::read_link(PathBuf::from("/proc").join(pid).join("exe"))
        .ok()
        .filter(|p| p.is_absolute() && !p.to_string_lossy().ends_with(" (deleted)"))
}

/// `/proc/N/exe` as the program it names, so that the call names a file that stays when the process ends.
pub fn resolved(w: &str) -> Option<String> {
    let n = w.strip_prefix("/proc/")?.strip_suffix("/exe")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    exe_of(n).map(|p| p.display().to_string())
}

/// Programs a session uses on the person's program, asked for by name to learn whether one is installed (`which
/// gdb`) or named as the means (`the make command`), not as the program to read: the readers, debuggers,
/// compilers, build tools, shells and interpreters.
const TOOLING: [&str; 43] = [
    "objdump", "readelf", "nm", "strings", "xxd", "hexdump", "file", "ldd", "otool", "r2", "radare2", "rizin", "gdb",
    "lldb", "strace", "ltrace", "valgrind", "perf", "nexo", "gcc", "g++", "cc", "c++", "clang", "clang++", "rustc",
    "cargo", "make", "cmake", "ninja", "ld", "as", "python", "python3", "perl", "node", "git", "go", "java", "upx",
    "bash", "sh", "zsh",
];

/// The file the shell runs for the bare program name `w`, unless `w` is one of the session's own tools (`gdb`,
/// `objdump`, a cross `aarch64-linux-gnu-gcc`).
pub fn on_path(w: &str) -> Option<String> {
    if w.is_empty() || w.starts_with('-') || w.contains(['/', '\\']) {
        return None;
    }
    let p = program(w);
    let base = p.rsplit('-').next().unwrap_or(&p);
    if TOOLING.contains(&p.as_str()) || TOOLING.contains(&base) || p.starts_with("python") {
        return None;
    }
    nexo_core::shell::find_program(w).map(|p| p.display().to_string())
}
