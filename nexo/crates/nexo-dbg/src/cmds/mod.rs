//! The commands a person (or an agent) gives to a running session, and how a stop is told.

use std::collections::HashMap;
use std::time::Duration;

use nexo_bin::{decode_live_in, parse_addr, parse_pattern, LiveIns, Output, Place};
use serde_json::json;

use crate::engine::{rows, Bp, Rec, Run, Session, State, StopInfo, Temp, TempKind, Why, Wp, HISTORY};
use crate::platform::{Access, Fault, HwSlot, Os, Tid};
use crate::{fail, usage, Result};

pub use dump::{dump_files, dump_part_in, dump_path_at, dumps_in};

mod breaks;
mod calls;
mod catch;
mod dump;
mod frame;
mod gcore;
mod gdb;
mod help;
mod io;
mod jump;
mod list;
mod memory;
mod quiet;
mod record;
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

const BOOL_FLAGS: [&str; 8] = ["all", "raw", "close", "keep-going", "once", "exact", "force", "stay"];

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
pub const READ_ONLY: [&str; 35] = [
    "status", "regs", "mem", "x", "print", "p", "args", "locals", "bt", "threads", "breaks", "maps", "modules", "find",
    "history", "out", "log", "dis", "disasm", "info", "watches", "sessions", "core", "help", "catches", "thread",
    "name", "note", "ptype", "whatis", "frame", "up", "down", "back", "list",
];

impl Session {
    fn tid_regs(&mut self) -> Result<crate::Regs> {
        self.need_stopped()?;
        self.t.regs(self.cur)
    }

    /// [`crate::modules::Mods::resolve`], reading the list of loaded files again when the name is not found: the
    /// list is read at the start and when an address falls outside it, so a library the loader brought in since
    /// (libc after the entry, a DLL after `LoadLibrary`) is not in it yet. A stopped program only: the list of an
    /// ended one is empty.
    pub(crate) fn resolve_place(&mut self, text: &str) -> Result<Place> {
        match self.mods.resolve(text) {
            Err(_) if self.state == State::Stopped => {
                self.mods.refresh(&mut *self.t);
                self.mods.resolve(text)
            }
            r => r,
        }
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
            match self.eval_frame(t) {
                Ok(v) => return Ok(v),
                // a register the selected frame has no value for: the files have no name for it, and only this
                // error says which register and why
                Err((e, true)) => return Err(e),
                Err(_) => {}
            }
        }
        Ok(self.mods.resolve(t)?.addr())
    }

    /// `A..B` (see [`nexo_bin::RangeExpr`]) as two addresses, each end any place [`Session::addr_of`] takes; `None`
    /// for one place, since here `$rsp+0x20` and `main+0x10` are addresses.
    pub fn span_of(&mut self, text: &str) -> Result<Option<(u64, u64)>> {
        if !nexo_bin::is_span(text) {
            return Ok(None);
        }
        let nexo_bin::RangeExpr::Between(a, b) = nexo_bin::RangeExpr::parse(text)? else { return Ok(None) };
        let (lo, hi) = (self.addr_of(&a)?, self.addr_of(&b)?);
        if hi <= lo {
            return usage(format!("{text:?} ends at {hi:#x}, not after its start {lo:#x}; {}", nexo_bin::RANGE_FORMS));
        }
        Ok(Some((lo, hi)))
    }

    /// A range in any form of [`nexo_bin::RangeExpr`], as memory addresses: `A..B` (each end any place
    /// [`Session::addr_of`] takes), `A+LEN`, or a function or section of the program (all of it).
    pub fn range_of(&mut self, text: &str) -> Result<(u64, u64)> {
        if let Some(r) = self.span_of(text)? {
            return Ok(r);
        }
        match nexo_bin::RangeExpr::parse(text)? {
            nexo_bin::RangeExpr::Len(a, n) => {
                let lo = self.addr_of(&a)?;
                Ok((lo, lo.saturating_add(n)))
            }
            nexo_bin::RangeExpr::Whole(t) => self.mods.file_range(&t),
            nexo_bin::RangeExpr::Between(..) => unreachable!("a span is answered by span_of"),
        }
    }

    /// One command. An answer read in a caller's frame (after `up` or `frame N`) starts with the line that names
    /// that frame, so a value is never taken for the stopped function's own.
    pub fn exec(&mut self, words: &[String]) -> Result<Output> {
        let in_frame = self.recording.is_none() && frame::reads_frame(words);
        let mut out = self.exec_cmd(words)?;
        if !in_frame {
            return Ok(out);
        }
        let Some(head) = self.frame_head() else { return Ok(out) };
        // a gdb spelling answered by the debugger's own command already has it
        if out.rows.first().is_none_or(|r| r.text != head) {
            let key = out.rows.first().map_or(0, |r| r.key);
            out.rows.insert(0, nexo_bin::Row { key, text: head, json: None });
        }
        if let Some(m) = out.meta.as_object_mut() {
            m.insert("frame".into(), json!(self.frame));
        }
        Ok(out)
    }

    fn exec_cmd(&mut self, words: &[String]) -> Result<Output> {
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
            "return" => self.cmd_return(&a),
            "jump" => self.cmd_jump(&a),
            "gcore" => self.cmd_gcore(&a),
            "list" => self.cmd_list(&a),
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
            "stops" => self.cmd_stops(&a),
            "signal" => self.cmd_signal(&a),
            "mem" | "x" => self.cmd_mem(&a),
            "write" => self.cmd_write(&a),
            "dump" => self.cmd_dump(&a),
            "print" | "p" => self.cmd_print(&a),
            "call" => self.cmd_call(&a),
            "ptype" | "whatis" => self.cmd_ptype(&a),
            "args" => self.cmd_args(&a),
            "locals" => self.cmd_locals(),
            "bt" => self.cmd_bt(&a),
            "dis" | "disasm" => self.cmd_dis(&a),
            "threads" => self.cmd_threads(&a),
            "thread" => self.cmd_thread(&a),
            "frame" => self.cmd_frame(&a),
            "up" | "down" => self.cmd_up(&a, cmd == "up"),
            "watch" => self.cmd_watch(&words[1..]),
            "maps" => self.cmd_maps(&a),
            "modules" => self.cmd_modules(),
            "find" => self.cmd_find(&a),
            "out" => Ok(self.cmd_out(&a)),
            "log" => {
                self.log_shown = self.log_total;
                Ok(rows("log", self.log.iter().cloned().collect(), json!({})))
            }
            "history" => Ok(self.cmd_history(&a)),
            "record" => self.cmd_record(&a),
            "back" => self.cmd_back(&a),
            "stdin" => crate::stdin::exec(self, words),
            "catch" if a.pos.first().is_some_and(|w| w == "syscall") => self.cmd_catch_syscall(&a.pos[1..]),
            "catch" if a.pos.first().is_some_and(|w| w == "load" || w == "unload") => self.cmd_catch_load(&a),
            "catch" if a.pos.first().is_some_and(|w| catch::catch_symbols(w).is_some()) => self.cmd_catch_event(&a),
            "catch" | "catches" => self.cmd_catch(&a),
            "strace" => self.cmd_strace(&a),
            "ltrace" => self.cmd_ltrace(&a),
            "hook" => self.cmd_hook(&words[1..]),
            "hooks" => self.cmd_hook(&[]),
            "heap" => self.cmd_heap(&a),
            "cover" => self.cmd_cover(&words[1..]),
            "kill" => self.cmd_kill(&a),
            "detach" => self.cmd_detach(),
            "follow" => self.cmd_follow(&a),
            "name" | "note" => self.cmd_keep(cmd == "name", &words[1..]),
            "do" => self.cmd_do(&words[1..]),
            "run" => self.cmd_run_live(),
            other => match crate::gdbdialect::hint(other) {
                Some(h) => usage(format!("{other}: {h}")),
                None => usage(format!(
                    "unknown command {other:?}: `help` lists them; program input is not typed as a line: `run <path> <<< \"3 20 X\\n\"` or `run <path> < file` gives it at the start, `stdin <text>` while it runs"
                )),
            },
        }
    }
}
