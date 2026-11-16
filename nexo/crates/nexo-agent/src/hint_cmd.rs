//! A tolerant reading of a shell command, for the one-line hints only (`bin_hint`, `dbg_hint`, `ask_hint`).
//!
//! The parser the bash tool judges permissions with (`nexo_tools::bash::command_words`) gives up on a redirection,
//! a heredoc, a group or a `cd`: right for a permission, too strict for a hint, which is lost on
//! `cat > g.gdb <<'EOF' ... EOF` followed by `gdb -x g.gdb prog 2>&1 | tail`, on `cd build && gdb prog` or on
//! `./prog > out.txt`. This one splits the same text into the commands that run, each with the folder it runs
//! in (`cd`, groups), its input (`< file`, `<<< text`, a heredoc body) and the file it writes. A leading `VAR=x`,
//! `sudo`, `env`, `timeout 10` or `bash -c '...'` is looked through.
//!
//! Nothing here runs, expands or opens anything. A substitution (`$(..)`, backticks, `<(..)`) or an open quote makes
//! the whole command unreadable (`None`): its words say too little to judge.

use std::path::{Path, PathBuf};

mod scan;
mod words;

pub use nexo_tools::program::{callable, quoted, shell_quoted, slash, tool_call};
use scan::Scan;
pub use words::program;

/// Which tools of the session can take what a hint names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offer {
    /// The `bin` tool is offered (otherwise `nexo bin` goes through the shell).
    pub bin: bool,
    /// The `debug` tool is offered.
    pub debug: bool,
}

/// What a command reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// `< file`.
    File(String),
    /// `<<< text`, or the body of a heredoc.
    Text(String),
}

/// One command of a line: its words (redirections, `VAR=x` and wrappers taken out), where it runs, its input and
/// the file it writes (`> f`, `>> f`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seg {
    pub words: Vec<String>,
    pub dir: PathBuf,
    pub input: Option<Input>,
    pub output: Option<String>,
    /// A heredoc opened on this command whose body has not been read yet: its delimiter, and whether `<<-`.
    pending: Option<(String, bool)>,
}

/// The most of a heredoc body kept.
const BODY_CAP: usize = 16 * 1024;
/// A longer command is not read.
const MAX_COMMAND: usize = 100_000;
/// How deep `sh -c '...'` is looked into.
const MAX_DEPTH: u8 = 2;

/// The commands of `command` run from `cwd`, in order. `None` for a command with a substitution or an open quote.
pub fn segments(command: &str, cwd: &Path) -> Option<Vec<Seg>> {
    if command.len() > MAX_COMMAND {
        return None;
    }
    Scan::new(command, cwd, 0).run()
}

/// Whether the word `w`, from the folder `dir`, names a file `nexo bin` opens (read as bytes, the first 4096 only).
pub fn opens(dir: &Path, w: &str) -> bool {
    !w.is_empty() && !w.starts_with('-') && nexo_binmagic::opens(&dir.join(w))
}

/// Like [`opens`], and every other kind `nexo bin` reads: 32-bit programs, .NET assemblies, JVM classes and jars,
/// WebAssembly modules.
pub fn reads(dir: &Path, w: &str) -> bool {
    kind(dir, w).is_some()
}

/// What the file the word `w` names from `dir` is, when `nexo bin` reads it (see [`nexo_binmagic::kind_at`]).
pub fn kind(dir: &Path, w: &str) -> Option<nexo_binmagic::Compiled> {
    if w.is_empty() || w.starts_with('-') {
        return None;
    }
    nexo_binmagic::kind_at(&dir.join(w))
}

/// The file a hint names, as a call writes it: the way the command did when it ran in the session's folder, the
/// whole path when a `cd` moved it.
pub fn shown(cwd: &Path, dir: &Path, w: &str) -> String {
    if Path::new(w).is_absolute() || dir == cwd {
        slash(w)
    } else {
        slash(&dir.join(w).display().to_string())
    }
}

/// Whether a failed shell result is of a command that ran: the shell tool gives those a line with the exit code,
/// or one that says it was killed (a dump too long for the time limit is where a hint helps most). Other lines
/// nexo adds may follow it. A refusal, a hook's block, a cancel or an interruption has none.
pub fn ran(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        let code = l.strip_prefix("[exit code ").and_then(|r| r.strip_suffix(']'));
        code.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
            || l == "[killed by a signal]"
            || (l.starts_with("[killed: timed out after ") && l.ends_with(" s]"))
    })
}

#[cfg(test)]
mod tests;
