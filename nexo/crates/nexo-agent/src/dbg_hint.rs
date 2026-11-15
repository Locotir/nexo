//! Three one-line hints that name the `debug` call doing what the model just did by hand, each once per session:
//!
//! - `gdb` on a program of the project: the same session as one `debug` call (its `-ex` lines, the script of
//!   `-x file` or of a heredoc, with the breakpoints, `x`, `p`, `info registers` it holds);
//! - `strace` or `ltrace` on one: the `strace`/`ltrace` of the debugger, then `cont` and `show`;
//! - a program of the project that died by a fault signal (exit 128+N, a segfault is 139): the run that stops at it.
//!
//! Only when the `debug` tool is offered and the file is one `nexo bin` opens. Nothing is run or loaded: the command
//! is split by `hint_cmd` (heredocs, redirections, `cd x &&`, wrappers) and a gdb script is only read as text. A
//! script line the debugger has no word for is left out of the call (a register is kept, a gdb convenience variable
//! is not); the call is what the line says it is, a way to the same stops, not a promise of every gdb feature, and
//! when the script's every breakpoint, watch and catch was left out there is no line at all: `run; cont` would stop
//! nowhere the script did.

use std::path::Path;

mod detect;
mod gdb;
mod told;
mod trace;
mod translate;

use detect::fault;
use gdb::gdb_call;
pub use told::{round, told, Told};
use trace::trace_call;

use crate::hint_cmd;

/// Started every line before there were three kinds, so a session saved then has it for a gdb and for a fault.
pub const HEAD: &str = "[nexo: the debug tool answers this in one call:";
/// Starts the line for a fault.
pub const HEAD_FAULT: &str = "[nexo: the debug tool runs this to the fault in one call:";
/// Starts the line for a gdb session.
pub const HEAD_GDB: &str = "[nexo: the debug tool takes this gdb session in one call:";
/// Starts the line for strace and ltrace.
pub const HEAD_TRACE: &str = "[nexo: the debug tool traces these calls in one call:";

/// Exit codes of a shell whose program died by a fault signal: SIGILL, SIGABRT, SIGBUS, SIGFPE, SIGSEGV.
const FAULTS: [i32; 5] = [132, 134, 135, 136, 139];

/// gdb options that take a value: the word after them is not the program.
const GDB_VALUED: [&str; 18] = [
    "-iex",
    "-ix",
    "-c",
    "-d",
    "-s",
    "-e",
    "-se",
    "-cd",
    "-tty",
    "-b",
    "-l",
    "-i",
    "--core",
    "--directory",
    "--cd",
    "--exec",
    "--symbols",
    "--tty",
];
/// strace options that take a value.
const STRACE_VALUED: [&str; 13] = ["-o", "-s", "-u", "-E", "-a", "-b", "-P", "-I", "-S", "-X", "-O", "-U", "-H"];
/// ltrace options that take a value.
const LTRACE_VALUED: [&str; 12] = ["-o", "-s", "-u", "-a", "-A", "-D", "-F", "-l", "-n", "-w", "-x", "-L"];

/// The most commands of a translated session, and the most characters of the call.
const MAX_CMDS: usize = 12;
const MAX_CALL: usize = 420;

/// Which of the hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Gdb,
    Trace,
    Fault,
}

/// The hint a command earns, from its result text, among the kinds not told yet.
pub fn line(command: &str, result: &str, cwd: &Path, told: &Told) -> Option<(Kind, String)> {
    let segs = hint_cmd::segments(command, cwd)?;
    for (k, s) in segs.iter().enumerate() {
        let Some(first) = s.words.first() else { continue };
        let (kind, call) = match hint_cmd::program(first).as_str() {
            "gdb" if !told.gdb => (Kind::Gdb, gdb_call(&segs, k, cwd)),
            "strace" | "ltrace" if !told.trace => (Kind::Trace, trace_call(&segs, k, cwd)),
            _ => continue,
        };
        if let Some(call) = call {
            let head = if kind == Kind::Gdb { HEAD_GDB } else { HEAD_TRACE };
            return Some((kind, format!("{head} {}]", hint_cmd::tool_call("debug", &call))));
        }
    }
    if told.fault {
        return None;
    }
    fault(&segs, result, cwd).map(|l| (Kind::Fault, l))
}

#[cfg(test)]
mod tests;
