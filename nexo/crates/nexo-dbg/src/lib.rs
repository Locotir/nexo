//! A debugger an agent drives one bounded call at a time: start a program or attach to one, stop it
//! where asked, and read what it holds. Linux x86-64 goes through ptrace and Windows x86-64 through
//! the Win32 Debug API; names and instruction text come from `nexo-bin`.
//!
//! Nothing here hides the debugger from the program it runs.

#![deny(clippy::undocumented_unsafe_blocks)]
#![deny(unsafe_op_in_unsafe_fn)]

mod argshape;
mod callargs;
mod client;
mod cmds;
mod commands;
mod corefile;
mod coreout;
mod cover;
mod engine;
mod expr;
mod gdbdialect;
mod heap;
mod hooks;
mod knowledge;
mod libs;
mod lines;
mod memimage;
mod minidump;
mod modules;
mod pe_unwind;
mod places;
mod platform;
mod procinfo;
mod rebuild;
mod record;
mod regs;
mod remote;
mod rsp;
mod server;
mod shellsyn;
mod stdin;
mod stub;
mod syscalls;
// Not on macOS: `cc -g` there puts the DWARF in a dSYM beside the program (or leaves it in the object files its debug
// map names), so the program bytes this test damages hold no debug sections.
#[cfg(all(test, not(target_os = "macos")))]
mod tests_harden;
mod trace;
mod undo;
mod vars;
mod vector;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod mach;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod macos;
#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;

pub use client::{
    dbg_class, do_parts, execute, header, parse, parse_header, pick_session, read_only_refusal, Call, DbgClass, Header,
    Host,
};
pub use cmds::{
    dump_files, dump_part_in, dump_path_at, dumps_in, help, split_words, Args, DEFAULT_WAIT, MAX_WAIT, READ_ONLY,
};
pub use commands::{canonical, COMMANDS};
pub use engine::{Session, State, StopInfo};
pub use gdbdialect::Plan as GdbPlan;
pub use knowledge::set_data_dir;
pub use platform::{
    attach, launch, runs_here, Access, Event, Fault, HwSlot, Launch, LaunchOpts, Map, Module, Os, Resume, Stop, Target,
    Tid, Touch,
};
pub use regs::Regs;
pub use server::{
    decode_env, encode_env, open, owner_of, request, serve, sessions, spawn, spawn_thread, state_dir, Kind, Reply,
    Start, IDLE,
};
pub use shellsyn::{
    assignment, debug_fact, debug_shell_items, is_shell_word, shell_pieces, variable_in, Piece, Redirect, SHELL_WORDS,
};
pub use stub::serve_gdb;

use std::fmt;

/// A script cut into commands at `;` and at line ends outside quotes (double or single) (`#` lines left out): one call may carry
/// a whole gdb script.
pub fn script_units(text: &str) -> Vec<String> {
    gdbdialect::units(text)
}

/// A gdb script that starts with breakpoints and ends with `run` becomes the debugger's shape: the session is
/// started first, the commands run in it, `cont` stands for `run`. `Ok(None)` when the script needs no change.
pub fn gdb_plan(parts: &[String]) -> std::result::Result<Option<GdbPlan>, String> {
    gdbdialect::plan(parts, &|n| Regs::default().get(n).is_some())
}

#[derive(Debug)]
pub enum Error {
    /// The command was not understood; the text says what is expected.
    Usage(String),
    /// The command was understood and could not be done.
    Fail(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Fail(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usage(s) | Error::Fail(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for Error {}

impl From<nexo_bin::Error> for Error {
    fn from(e: nexo_bin::Error) -> Error {
        match e {
            nexo_bin::Error::Usage(s) => Error::Usage(s),
            other => Error::Fail(other.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Fail(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn fail<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Fail(msg.into()))
}

pub(crate) fn usage<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Usage(msg.into()))
}
