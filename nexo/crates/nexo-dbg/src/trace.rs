//! What the program does while it runs, written down as it happens: system calls (`strace`), and the
//! stop on a system call (`catch syscall`). A line is written only for what was seen: a call whose
//! start or end the debugger did not see says so.

use std::collections::{BTreeMap, HashMap, VecDeque};

use nexo_bin::Output;
use serde_json::json;

use crate::cmds::Args;
use crate::engine::{rows, Session, State, Why};
use crate::platform::{Sys, Tid};
use crate::syscalls::{self, Filter};
use crate::{usage, Result};

/// Trace lines kept; older ones are dropped and counted.
pub const TRACE_KEEP: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sys,
    Lib,
    Hook,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Sys => "strace",
            Kind::Lib => "ltrace",
            Kind::Hook => "hook",
        }
    }
}

pub struct Line {
    pub seq: u64,
    pub kind: Kind,
    /// The call's name, for filtering.
    pub name: String,
    pub text: String,
}

/// A system call whose start was seen and whose end was not yet.
struct Open {
    nr: u64,
    raw: [u64; 6],
    args: Option<Vec<String>>,
}

#[derive(Default)]
pub struct Tracing {
    /// `strace` is on, with what it writes down.
    pub sys: Option<Filter>,
    /// `catch syscall`: calls that stop the program at their start and their end.
    pub sys_catch: Option<Filter>,
    open: HashMap<Tid, Open>,
    pub lines: VecDeque<Line>,
    pub seq: u64,
    pub dropped: u64,
    /// Lines already told about in a stop report, per kind.
    pub told: BTreeMap<&'static str, u64>,
    pub counts: BTreeMap<String, u64>,
    /// `ltrace` is on: where the program calls its imports.
    pub lib: Option<crate::hooks::Lib>,
    /// Library calls and hooks whose entry was seen, waiting for their return.
    pub pending: Vec<crate::hooks::Pending>,
    /// Declared hooks.
    pub hooks: Vec<crate::hooks::Hook>,
    /// `cover`: blocks of the program's code and which were passed.
    pub cover: Option<crate::cover::Cover>,
}

impl Tracing {
    pub fn push(&mut self, kind: Kind, name: &str, text: String) {
        self.seq += 1;
        self.lines.push_back(Line { seq: self.seq, kind, name: name.to_string(), text });
        while self.lines.len() > TRACE_KEEP {
            self.lines.pop_front();
            self.dropped += 1;
        }
    }

    fn wants_syscalls(&self) -> bool {
        self.sys.is_some() || self.sys_catch.is_some()
    }
}

impl Session {
    /// The program stopped at a system call. `Some`: it stops for the person (`catch syscall`).
    pub(crate) fn on_syscall(&mut self, tid: Tid, s: Sys) -> Option<(Tid, Why)> {
        let pid = self.t.pid();
        let who = if tid != pid { format!("[{tid}] ") } else { String::new() };
        if s.entry {
            let name = syscalls::name(s.nr).map(str::to_string).unwrap_or_else(|| format!("syscall_{}", s.nr));
            let mut rd = |a: u64, n: usize| self.read_vec(a, n);
            let args = syscalls::args_at_entry(&name, &s.args, &mut rd);
            let text = syscalls::call_text(&name, args.as_deref(), &s.args);
            if let Some(old) = self.tr.open.insert(tid, Open { nr: s.nr, raw: s.args, args }) {
                // the end of the earlier call was not seen (a single step ran over it)
                let n = syscalls::name(old.nr).unwrap_or("?").to_string();
                if self.tr.sys.as_ref().is_some_and(|f| f.takes(&n)) {
                    let t = syscalls::call_text(&n, old.args.as_deref(), &old.raw);
                    self.tr.push(Kind::Sys, &n, format!("{who}{t} = ? (its return was not seen)"));
                }
            }
            if self.tr.sys_catch.as_ref().is_some_and(|f| f.takes(&name)) {
                return Some((tid, Why::Syscall(format!("{who}{text} (about to run)"))));
            }
            return None;
        }
        let open = self.tr.open.remove(&tid).filter(|o| o.nr == s.nr);
        let name = syscalls::name(s.nr).map(str::to_string).unwrap_or_else(|| format!("syscall_{}", s.nr));
        let ret = syscalls::ret_text(&name, s.ret, s.error);
        let text = match open {
            Some(mut o) => {
                if let Some(a) = o.args.as_mut() {
                    let mut rd = |a: u64, n: usize| self.read_vec(a, n);
                    syscalls::fill(&name, &o.raw, s.ret, s.error, &mut rd, a);
                }
                format!("{who}{} = {ret}", syscalls::call_text(&name, o.args.as_deref(), &o.raw))
            }
            None => format!("{who}{name}(start not seen) = {ret}"),
        };
        *self.tr.counts.entry(name.clone()).or_default() += 1;
        if self.tr.sys.as_ref().is_some_and(|f| f.takes(&name)) {
            self.tr.push(Kind::Sys, &name, text.clone());
        }
        if self.tr.sys_catch.as_ref().is_some_and(|f| f.takes(&name)) {
            return Some((tid, Why::Syscall(format!("{text} (returned)"))));
        }
        None
    }

    /// The program ended: calls that started and never returned are written down as such.
    pub(crate) fn trace_end(&mut self) {
        let open: Vec<(Tid, Open)> = self.tr.open.drain().collect();
        for (_, o) in open {
            let n = syscalls::name(o.nr).unwrap_or("?").to_string();
            if self.tr.sys.as_ref().is_some_and(|f| f.takes(&n)) {
                let t = syscalls::call_text(&n, o.args.as_deref(), &o.raw);
                self.tr.push(Kind::Sys, &n, format!("{t} = ? (the program ended in it)"));
            }
        }
    }

    fn sync_syscalls(&mut self) -> Result<()> {
        let on = self.tr.wants_syscalls();
        self.t.trace_syscalls(on)
    }

    /// `strace [names|groups]` | `strace off` | `strace show [names|groups]`.
    pub(crate) fn cmd_strace(&mut self, a: &Args) -> Result<Output> {
        match a.pos.first().map(String::as_str) {
            Some("off") => {
                let n = self.tr.lines.iter().filter(|l| l.kind == Kind::Sys).count();
                self.tr.sys = None;
                if self.state != State::Exited {
                    self.need_stopped()?;
                    self.sync_syscalls()?;
                }
                Ok(rows("strace", vec![format!("system call trace off ({n} lines kept: `strace show`)")], json!({})))
            }
            Some("show") => {
                let f = Filter::parse(&a.pos[1..]).map_err(crate::Error::Usage)?;
                Ok(self.show(Kind::Sys, &|n: &str| f.takes(n), "`strace` turns the trace on"))
            }
            _ => {
                let f = Filter::parse(&a.pos).map_err(crate::Error::Usage)?;
                self.need_stopped()?;
                let was = self.tr.sys.replace(f.clone());
                if let Err(e) = self.sync_syscalls() {
                    self.tr.sys = was;
                    return Err(e);
                }
                Ok(rows(
                    "strace",
                    vec![format!(
                        "tracing system calls: {}; `cont`, then `strace show` (a `step` over a `syscall` instruction is not seen)",
                        f.text()
                    )],
                    json!({"filter": f.text()}),
                ))
            }
        }
    }

    /// `catch syscall <names|groups>` | `catch syscall off`.
    pub(crate) fn cmd_catch_syscall(&mut self, words: &[String]) -> Result<Output> {
        self.need_stopped()?;
        if words.first().is_some_and(|w| w == "off") {
            self.tr.sys_catch = None;
            self.sync_syscalls()?;
            return Ok(rows("catch", vec!["no stop on system calls".into()], json!({})));
        }
        if words.is_empty() {
            return usage("catch syscall <name|file|net|process|memory>... | catch syscall off");
        }
        let f = Filter::parse(words).map_err(crate::Error::Usage)?;
        let was = self.tr.sys_catch.replace(f.clone());
        if let Err(e) = self.sync_syscalls() {
            self.tr.sys_catch = was;
            return Err(e);
        }
        Ok(rows(
            "catch",
            vec![format!("stops at the start and the end of: {}; `catch syscall off` stops that", f.text())],
            json!({"syscalls": f.text()}),
        ))
    }

    /// The kept lines of one kind that pass `keep`, oldest first, with a count of the most frequent.
    pub(crate) fn show(&mut self, kind: Kind, keep: &dyn Fn(&str) -> bool, how_on: &str) -> Output {
        let mut l: Vec<String> = Vec::new();
        let mut per: BTreeMap<&str, u64> = BTreeMap::new();
        let mut n = 0u64;
        for x in self.tr.lines.iter().filter(|x| x.kind == kind && keep(&x.name)) {
            *per.entry(x.name.as_str()).or_default() += 1;
            n += 1;
        }
        let mut top: Vec<(&str, u64)> = per.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let dropped = if self.tr.dropped > 0 {
            format!(" ({} older trace lines dropped; the last {TRACE_KEEP} are kept)", self.tr.dropped)
        } else {
            String::new()
        };
        if n == 0 {
            l.push(format!("no {} lines yet: {how_on}{dropped}", kind.word()));
        } else {
            let most: Vec<String> = top.iter().take(8).map(|(k, v)| format!("{k} {v}")).collect();
            l.push(format!("{n} calls{dropped}; most: {}", most.join(", ")));
        }
        for x in self.tr.lines.iter().filter(|x| x.kind == kind && keep(&x.name)) {
            l.push(format!("#{} {}", x.seq, x.text));
        }
        let last = self.tr.lines.iter().filter(|x| x.kind == kind).map(|x| x.seq).max().unwrap_or(0);
        self.tr.told.insert(kind.word(), last);
        rows(kind.word(), l, json!({"lines": n}))
    }

    /// For a stop report: trace lines written since they were last shown, one line per kind.
    pub(crate) fn trace_note(&self) -> Vec<String> {
        let mut out = Vec::new();
        for k in [Kind::Sys, Kind::Lib, Kind::Hook] {
            let told = self.tr.told.get(k.word()).copied().unwrap_or(0);
            let new: Vec<&Line> = self.tr.lines.iter().filter(|x| x.kind == k && x.seq > told).collect();
            if let Some(last) = new.last() {
                out.push(format!("{}: {} new lines, last: {} (`{} show`)", k.word(), new.len(), last.text, k.word()));
            }
        }
        out
    }
}
