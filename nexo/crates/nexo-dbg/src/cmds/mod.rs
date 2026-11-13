//! The commands a person (or an agent) gives to a running session, and how a stop is told.

use std::collections::HashMap;
use std::time::Duration;

use nexo_bin::{decode_live, parse_addr, parse_pattern, typed_rows, ElemType, LiveIns, Output, Place};
use serde_json::json;

use crate::engine::{rows, signal_name, Bp, Rec, Run, Session, State, StopInfo, Temp, TempKind, Why, Wp, HISTORY};
use crate::platform::{Access, Fault, HwSlot, Os, Tid};
use crate::{fail, usage, Result};

mod breaks;
mod calls;
mod catch;
mod gdb;
mod help;
mod io;
mod memory;
mod regs;
mod run;
mod stack;
mod stop;
mod watch;

pub use help::{help, split_words};

/// Seconds a `cont`, `step` over a long call or `wait` holds the answer unless told otherwise.
pub const DEFAULT_WAIT: u64 = 8;
/// The most a single answer waits.
pub const MAX_WAIT: u64 = 600;

/// Words split into plain words and `--flag value` pairs (`--all`, `--raw`, `--close` carry none).
pub struct Args {
    pub pos: Vec<String>,
    flags: HashMap<String, String>,
}

const BOOL_FLAGS: [&str; 6] = ["all", "raw", "close", "keep-going", "once", "exact"];

impl Args {
    pub fn new(words: &[String]) -> Result<Args> {
        let mut pos = Vec::new();
        let mut flags = HashMap::new();
        let mut it = words.iter();
        while let Some(w) = it.next() {
            if let Some(name) = w.strip_prefix("--").filter(|n| !n.is_empty()) {
                if BOOL_FLAGS.contains(&name) {
                    flags.insert(name.to_string(), String::new());
                } else {
                    let Some(v) = it.next() else { return usage(format!("--{name} needs a value")) };
                    flags.insert(name.to_string(), v.clone());
                }
            } else {
                pos.push(w.clone());
            }
        }
        Ok(Args { pos, flags })
    }

    pub fn flag(&self, n: &str) -> Option<&str> {
        self.flags.get(n).map(String::as_str)
    }

    pub fn has(&self, n: &str) -> bool {
        self.flags.contains_key(n)
    }

    fn num(&self, n: &str) -> Result<Option<u64>> {
        match self.flag(n) {
            None => Ok(None),
            Some(v) => {
                Ok(Some(parse_addr(v).map_err(|_| crate::Error::Usage(format!("--{n} {v:?} is not a number")))?))
            }
        }
    }

    pub fn wait(&self) -> Result<Duration> {
        Ok(Duration::from_secs(self.num("wait")?.unwrap_or(DEFAULT_WAIT).min(MAX_WAIT)))
    }
}

/// Commands that only look (they never change the program or the debugger beyond what they show).
pub const READ_ONLY: [&str; 28] = [
    "status", "regs", "mem", "x", "print", "p", "args", "locals", "bt", "threads", "breaks", "maps", "modules", "find",
    "history", "out", "log", "dis", "disasm", "info", "watches", "sessions", "core", "help", "catches", "thread",
    "name", "note",
];

impl Session {
    fn tid_regs(&mut self) -> Result<crate::Regs> {
        self.need_stopped()?;
        self.t.regs(self.cur)
    }

    /// A place as a number: a plain number is the program's file address when it falls in the program (the numbers
    /// `nexo bin` prints) and a memory address otherwise; anything else is an expression.
    ///
    /// A leading `*` is an exact-place marker only in front of a plain number (`*0x401000`, as `break *ADDR`); in front
    /// of anything else it is a dereference, as in gdb (`x/s *(char**)($rbp-8)` reads the pointer stored there).
    pub fn addr_of(&mut self, text: &str) -> Result<u64> {
        let t = text.trim();
        let t = match t.strip_prefix('*') {
            Some(r) if parse_addr(r.trim()).is_ok() => r.trim(),
            _ => t,
        };
        if let Some(r) = self.source_place(t, false) {
            return r;
        }
        if parse_addr(t).is_ok() {
            return Ok(self.mods.resolve(t)?.addr());
        }
        if self.state == State::Stopped {
            if let Ok(v) = self.eval(t) {
                return Ok(v);
            }
        }
        Ok(self.mods.resolve(t)?.addr())
    }

    pub fn exec(&mut self, words: &[String]) -> Result<Output> {
        let Some(cmd) = words.first() else { return usage("a command is needed: `help` lists them") };
        if self.recording.is_some() {
            return self.record_line(words);
        }
        if let Some(r) = self.gdb_exec(words) {
            return r;
        }
        let a = Args::new(&words[1..])?;
        match cmd.as_str() {
            "help" => Ok(help()),
            "status" | "info" => self.status(),
            "regs" => self.cmd_regs(&a),
            "setreg" => self.cmd_setreg(&a),
            "calls" => self.cmd_calls(&a),
            "break" | "b" | "tbreak" => self.cmd_break(cmd == "tbreak", &words[1..], &a),
            "breaks" | "watches" => Ok(self.cmd_breaks()),
            "delete" | "del" => self.cmd_delete(&a),
            "enable" | "disable" => self.cmd_enable(cmd == "enable", &a),
            "cont" | "c" | "continue" => {
                let w = a.wait()?;
                self.go(Run::default(), w)
            }
            "step" | "s" => self.cmd_step(&a, false),
            "next" | "n" => self.cmd_step(&a, true),
            "finish" => self.cmd_finish(&a),
            "until" => self.cmd_until(&a),
            "wait" => {
                let w = a.wait()?;
                if self.state == State::Running {
                    self.settle(w)
                } else {
                    self.status()
                }
            }
            "stop" => self.stop_now(),
            "mem" | "x" => self.cmd_mem(&a),
            "write" => self.cmd_write(&a),
            "print" | "p" => self.cmd_print(&a),
            "args" => self.cmd_args(&a),
            "locals" => self.cmd_locals(),
            "bt" => self.cmd_bt(&a),
            "dis" | "disasm" => self.cmd_dis(&a),
            "threads" => self.cmd_threads(),
            "thread" => self.cmd_thread(&a),
            "watch" => self.cmd_watch(&a),
            "maps" => self.cmd_maps(&a),
            "modules" => self.cmd_modules(),
            "find" => self.cmd_find(&a),
            "out" => Ok(self.cmd_out(&a)),
            "log" => {
                self.log_shown = self.log_total;
                Ok(rows("log", self.log.iter().cloned().collect(), json!({})))
            }
            "history" => Ok(self.cmd_history(&a)),
            "stdin" => self.cmd_stdin(&a),
            "catch" if a.pos.first().is_some_and(|w| w == "syscall") => self.cmd_catch_syscall(&a.pos[1..]),
            "catch" | "catches" => self.cmd_catch(&a),
            "strace" => self.cmd_strace(&a),
            "ltrace" => self.cmd_ltrace(&a),
            "hook" => self.cmd_hook(&words[1..]),
            "hooks" => self.cmd_hook(&[]),
            "cover" => self.cmd_cover(&words[1..]),
            "kill" => self.cmd_kill(),
            "detach" => self.cmd_detach(),
            "name" | "note" => self.cmd_keep(cmd == "name", &words[1..]),
            "do" => self.cmd_do(&words[1..]),
            other => match crate::gdbdialect::hint(other) {
                Some(h) => usage(format!("{other}: {h}")),
                None => usage(format!("unknown command {other:?}: `help` lists them")),
            },
        }
    }
}
