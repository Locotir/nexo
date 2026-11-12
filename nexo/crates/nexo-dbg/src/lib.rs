//! A debugger an agent drives one bounded call at a time: start a program or attach to one, stop it
//! where asked, and read what it holds. Linux x86-64 goes through ptrace and Windows x86-64 through
//! the Win32 Debug API; names and instruction text come from `nexo-bin`.
//!
//! Nothing here hides the debugger from the program it runs.

mod client;
mod cmds;
mod engine;
mod expr;
mod modules;
mod platform;
mod regs;
mod server;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod linux;

pub use client::{dbg_class, execute, header, parse, parse_header, Call, DbgClass, Header, Host};
pub use cmds::{help, split_words, Args, DEFAULT_WAIT, MAX_WAIT, READ_ONLY};
pub use engine::{Session, State, StopInfo};
pub use platform::{
    attach, launch, Access, Event, Fault, HwSlot, Launch, Map, Module, Os, Resume, Stop, Target, Tid, Touch,
};
pub use regs::Regs;
pub use server::{open, owner_of, request, serve, sessions, spawn, spawn_thread, state_dir, Kind, Reply, Start, IDLE};

use std::fmt;

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
