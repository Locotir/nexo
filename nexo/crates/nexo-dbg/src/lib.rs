//! A debugger an agent drives one bounded call at a time: start a program or attach to one, stop it
//! where asked, and read what it holds. Linux x86-64 goes through ptrace and Windows x86-64 through
//! the Win32 Debug API; names and instruction text come from `nexo-bin`.
//!
//! Nothing here hides the debugger from the program it runs.

mod client;
mod cmds;
mod commands;
mod corefile;
mod cover;
mod engine;
mod expr;
mod gdbdialect;
mod hooks;
mod knowledge;
mod lines;
mod minidump;
mod modules;
mod pe_unwind;
mod places;
mod platform;
mod regs;
mod remote;
mod rsp;
mod server;
mod stub;
mod syscalls;
#[cfg(test)]
mod tests_harden;
mod trace;
mod vars;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux;
#[cfg(all(windows, target_arch = "x86_64"))]
mod windows;

pub use client::{
    dbg_class, do_parts, execute, header, parse, parse_header, read_only_refusal, Call, DbgClass, Header, Host,
};
pub use cmds::{help, split_words, Args, DEFAULT_WAIT, MAX_WAIT, READ_ONLY};
pub use commands::{canonical, COMMANDS};
pub use engine::{Session, State, StopInfo};
pub use gdbdialect::Plan as GdbPlan;
pub use knowledge::set_data_dir;
pub use platform::{
    attach, launch, Access, Event, Fault, HwSlot, Launch, Map, Module, Os, Resume, Stop, Target, Tid, Touch,
};
pub use regs::Regs;
pub use server::{open, owner_of, request, serve, sessions, spawn, spawn_thread, state_dir, Kind, Reply, Start, IDLE};
pub use stub::serve_gdb;

use std::fmt;

/// A script cut into commands at `;` and at line ends outside double quotes (`#` lines left out): one call may carry
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
