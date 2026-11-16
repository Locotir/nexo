//! Whether a `python` or `perl` command reads the compiled file it names or runs it. A parser opens the file
//! (`open(p, 'rb')`, `lief.parse`); a test runner or a launcher starts it (`subprocess.run`, `os.system`, perl's
//! `system`), and running a project program is not reading it by hand. The code tells which by its text: what is
//! written on the line, or the start of the script file, read as bytes and never run.

use std::io::Read;
use std::path::Path;

use crate::hint_cmd::{Input, Seg};

/// The most of a script file looked at.
const CAP: u64 = 64 * 1024;

/// What opens a file to read it: `open` (`open(p, 'rb')`, `Path(p).open()`, perl's `open(F, '<', p)`), standard
/// input, a memory map, and the parsers of compiled files.
const READS: [&str; 9] = ["open", "read_bytes", "stdin", "STDIN", "mmap", "fromfile", "lief", "pefile", "elftools"];

/// What starts a program or loads a library into the script. A quote next to `|` is perl's pipe open (`'-|'`,
/// `"./prog |"`).
const RUNS: [&str; 18] = [
    "subprocess",
    "popen",
    "system(",
    "system ",
    "exec(",
    "exec ",
    "execv",
    "execl",
    "spawn",
    "pexpect",
    "pwn",
    "ctypes",
    "qx",
    "`",
    "|'",
    "|\"",
    "'|",
    "\"|",
];

/// The code a `python` or `perl` command runs.
pub(super) enum Code<'a> {
    /// Written on the line: `-c`, `-e`, a heredoc or `<<<`. Its quoted strings may name the file.
    Line(&'a str),
    /// `perl -n` or `-p`: the code on the line runs on each line of the files named.
    Lines(&'a str),
    /// A script file, as the command names it.
    File(&'a str),
    /// A module (`-m`), or no code found.
    None,
}

impl<'a> Code<'a> {
    pub(super) fn of(s: &'a Seg) -> Self {
        let args = &s.words[1..];
        for (i, w) in args.iter().enumerate() {
            if w == "-m" {
                return Code::None;
            }
            // `-c CODE`, `-e CODE` and their clusters (`-we`, `-lne`)
            if super::is_short(w) && (w.ends_with('c') || w.ends_with('e')) {
                return match args.get(i + 1) {
                    Some(c) if w.contains(['n', 'p']) => Code::Lines(c),
                    Some(c) => Code::Line(c),
                    None => Code::None,
                };
            }
            // `-` reads the code from the input
            if w == "-" {
                break;
            }
            if !w.starts_with('-') {
                return Code::File(w);
            }
        }
        match &s.input {
            Some(Input::Text(t)) => Code::Line(t),
            _ => Code::None,
        }
    }

    /// The quoted strings written on the line, which may name the file.
    pub(super) fn literals(&self) -> Vec<&'a str> {
        match self {
            Code::Line(c) | Code::Lines(c) => super::literals(c),
            Code::File(_) | Code::None => Vec::new(),
        }
    }

    /// Whether the code, run in `dir`, reads the files it names and starts no program.
    pub(super) fn reads(&self, dir: &Path) -> bool {
        match self {
            Code::Line(c) => reads(c),
            Code::Lines(c) => !runs(c),
            Code::File(f) => start_of(&dir.join(f)).is_some_and(|c| reads(&c)),
            Code::None => false,
        }
    }
}

fn runs(code: &str) -> bool {
    RUNS.iter().any(|r| code.contains(r))
}

fn reads(code: &str) -> bool {
    !runs(code) && READS.iter().any(|r| code.contains(r))
}

/// The start of the script file at `path`, when it is a plain file (a named pipe would never answer).
fn start_of(path: &Path) -> Option<String> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(CAP).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
