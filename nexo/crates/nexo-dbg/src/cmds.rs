//! The commands a person (or an agent) gives to a running session, and how a stop is told.

use std::collections::HashMap;
use std::time::Duration;

use nexo_bin::{decode_live, parse_addr, parse_pattern, typed_rows, ElemType, LiveIns, Output, Place};
use serde_json::json;

use crate::engine::{rows, signal_name, Bp, Rec, Run, Session, State, StopInfo, Temp, TempKind, Why, Wp, HISTORY};
use crate::platform::{Access, Fault, HwSlot, Os, Tid};
use crate::{fail, usage, Result};

/// Registers listed in a stop report; the rest are one `regs` away.
const CHANGED_SHOWN: usize = 6;

/// Seconds a `cont`, `step` over a long call or `wait` holds the answer unless told otherwise.
pub const DEFAULT_WAIT: u64 = 8;
/// The most a single answer waits.
pub const MAX_WAIT: u64 = 600;

/// Words split into plain words and `--flag value` pairs (`--all`, `--raw`, `--close` carry none).
pub struct Args {
    pub pos: Vec<String>,
    flags: HashMap<String, String>,
}

const BOOL_FLAGS: [&str; 5] = ["all", "raw", "close", "keep-going", "once"];

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
pub const READ_ONLY: [&str; 27] = [
    "status", "regs", "mem", "x", "print", "p", "args", "bt", "threads", "breaks", "maps", "modules", "find",
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
    pub fn addr_of(&mut self, text: &str) -> Result<u64> {
        let t = text.trim();
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
        let a = Args::new(&words[1..])?;
        match cmd.as_str() {
            "help" => Ok(help()),
            "status" | "info" => self.status(),
            "regs" => self.cmd_regs(&a),
            "setreg" => self.cmd_setreg(&a),
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
            "bt" => self.cmd_bt(&a),
            "dis" | "disasm" => self.cmd_dis(&a),
            "threads" => self.cmd_threads(),
            "thread" => self.cmd_thread(&a),
            "watch" => self.cmd_watch(&a),
            "maps" => self.cmd_maps(&a),
            "modules" => self.cmd_modules(),
            "find" => self.cmd_find(&a),
            "out" => Ok(self.cmd_out(&a)),
            "log" => Ok(rows("log", self.log.iter().cloned().collect(), json!({}))),
            "history" => Ok(self.cmd_history(&a)),
            "stdin" => self.cmd_stdin(&a),
            "catch" | "catches" => self.cmd_catch(&a),
            "kill" => self.cmd_kill(),
            "detach" => self.cmd_detach(),
            "name" | "note" => self.cmd_keep(cmd == "name", &words[1..]),
            "do" => self.cmd_do(&words[1..]),
            other => usage(format!("unknown command {other:?}: `help` lists them")),
        }
    }

    fn status(&mut self) -> Result<Output> {
        let mut l = vec![format!(
            "pid {} {} | {} | {} breakpoints, {} watches | {} stops so far",
            self.t.pid(),
            if self.t.launched() { "started by the debugger" } else { "attached" },
            match self.state {
                State::Stopped => "stopped".to_string(),
                State::Running => "running".to_string(),
                State::Exited => format!("ended ({})", self.exit.clone().unwrap_or_default()),
            },
            self.bps.len(),
            self.wps.len(),
            self.nstop
        )];
        if self.state == State::Stopped {
            let r = self.t.regs(self.cur)?;
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            l.push(format!("thread {} at {:#x} {name}", self.cur, r.rip));
        }
        if let Some(o) = self.new_output_line() {
            l.push(o);
        }
        Ok(rows(
            "status",
            l,
            json!({"state": match self.state { State::Stopped => "stopped", State::Running => "running", State::Exited => "exited" }, "pid": self.t.pid()}),
        ))
    }

    fn cmd_regs(&mut self, a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        if let Some(n) = a.pos.first() {
            let Some(v) = r.get(n) else { return usage(format!("no register named {n:?}")) };
            return Ok(rows("regs", vec![format!("{n} = {v:#x} ({})", v as i64)], json!({})));
        }
        let mut l = Vec::new();
        let named = r.named();
        for chunk in named.chunks(4) {
            l.push(chunk.iter().map(|(n, v)| format!("{n:<7}{v:#018x}")).collect::<Vec<_>>().join("  "));
        }
        l.push(format!("flags {}", r.flags_text()));
        let sp = r.rsp();
        let mut stack = Vec::new();
        for i in 0..4u64 {
            let mut b = [0u8; 8];
            if self.t.read(sp + i * 8, &mut b) == 8 {
                stack.push(format!("{:#x}", u64::from_le_bytes(b)));
            }
        }
        if !stack.is_empty() {
            l.push(format!("[rsp] {}", stack.join(" ")));
        }
        Ok(rows("regs", l, json!({"thread": self.cur})))
    }

    fn cmd_setreg(&mut self, a: &Args) -> Result<Output> {
        let (Some(n), Some(v)) = (a.pos.first(), a.pos.get(1..)) else {
            return usage("setreg <register> <value or expression>");
        };
        let val = self.addr_of(&v.join(" "))?;
        let mut r = self.tid_regs()?;
        if !r.set(n, val) {
            return usage(format!("{n:?} cannot be set: use a 64-bit register, rip or eflags"));
        }
        self.t.set_regs(self.cur, &r)?;
        Ok(rows("setreg", vec![format!("{n} = {val:#x}")], json!({})))
    }

    fn cmd_break(&mut self, once: bool, words: &[String], a: &Args) -> Result<Output> {
        let Some(spec) = a.pos.first() else {
            return usage("break <address|name|name+off|import> [if <expr>] [after N] [log <text>] [once]");
        };
        let (mut cond, mut after, mut log) = (None, 0u64, None);
        let mut once = once || a.has("once");
        let rest: Vec<&String> = words.iter().skip_while(|w| *w != spec).skip(1).collect();
        let mut i = 0;
        let mut cur: Option<&str> = None;
        let mut buf: Vec<&str> = Vec::new();
        let mut flush = |cur: &mut Option<&str>, buf: &mut Vec<&str>| -> Result<()> {
            match cur.take() {
                Some("if") => cond = Some(buf.join(" ")),
                Some("log") => log = Some(buf.join(" ")),
                Some("after") => {
                    after =
                        parse_addr(&buf.join(" ")).map_err(|_| crate::Error::Usage("after needs a number".into()))?
                }
                _ => {}
            }
            buf.clear();
            Ok(())
        };
        while i < rest.len() {
            match rest[i].as_str() {
                k @ ("if" | "log" | "after") => {
                    flush(&mut cur, &mut buf)?;
                    cur = Some(k);
                }
                "once" => {
                    flush(&mut cur, &mut buf)?;
                    once = true;
                }
                w if cur.is_some() => buf.push(w),
                w if w.starts_with("--") => {}
                w => {
                    return usage(format!(
                        "unexpected {w:?}: after the place come `if <expr>`, `after N`, `log <text>`, `once`"
                    ))
                }
            }
            i += 1;
        }
        flush(&mut cur, &mut buf)?;
        let addr = match self.mods.resolve(spec) {
            Ok(Place::Code(x)) => x,
            Ok(Place::ImportSlot(_)) => {
                // an import: stop in the library code when it is loaded, in the stub otherwise
                match self.mods.resolve(&format!("!{}", spec.trim_start_matches('!'))) {
                    Ok(Place::Code(x)) => x,
                    _ => {
                        return fail(format!(
                            "{spec} is an import and its library is not loaded yet: run to the entry first (`cont`), or break at <library>!{spec}"
                        ))
                    }
                }
            }
            Ok(Place::Data(x)) => {
                if self.mods.locate(x).is_some() && self.maps_exec(x) {
                    x
                } else {
                    return fail(format!("{spec} ({x:#x}) is data, not code: use `watch` for a place that is written"));
                }
            }
            Err(e) => match self.addr_of(spec) {
                Ok(x) if self.maps_exec(x) => x,
                _ => return Err(e),
            },
        };
        if let Some(c) = &cond {
            if self.state == State::Stopped {
                if let Err(e) = self.eval(c) {
                    if !matches!(&e, crate::Error::Fail(m) if m.contains("memory")) {
                        return fail(format!("the condition {c:?} does not work: {e}"));
                    }
                }
            }
        }
        let id = self.new_id();
        let label = self.mods.label(addr, false).unwrap_or_default();
        self.bps.push(Bp { id, addr, enabled: true, cond, after, hits: 0, log, once });
        Ok(rows("break", vec![format!("breakpoint #{id} at {addr:#x} {label}")], json!({"id": id, "addr": addr})))
    }

    fn maps_exec(&mut self, addr: u64) -> bool {
        self.t.maps().map(|m| m.iter().any(|m| m.exec && m.start <= addr && addr < m.end)).unwrap_or(false)
    }

    fn cmd_breaks(&mut self) -> Output {
        let mut l = Vec::new();
        #[allow(clippy::type_complexity)]
        let bps: Vec<(u32, u64, bool, Option<String>, u64, u64, Option<String>, bool)> = self
            .bps
            .iter()
            .map(|b| (b.id, b.addr, b.enabled, b.cond.clone(), b.after, b.hits, b.log.clone(), b.once))
            .collect();
        for (id, addr, en, cond, after, hits, log, once) in bps {
            let label = self.mods.label(addr, false).unwrap_or_default();
            let mut s = format!("#{id} {addr:#x} {label} hits {hits}");
            if !en {
                s.push_str(" (disabled)");
            }
            if let Some(c) = cond {
                s.push_str(&format!(" if {c}"));
            }
            if after > 0 {
                s.push_str(&format!(" after {after}"));
            }
            if let Some(t) = log {
                s.push_str(&format!(" log {t:?}"));
            }
            if once {
                s.push_str(" once");
            }
            l.push(s);
        }
        for w in &self.wps {
            l.push(format!(
                "#{} watch {:#x} len {} {} hits {}",
                w.id,
                w.hw.addr,
                w.hw.len,
                match w.hw.access {
                    Access::Exec => "x",
                    Access::Write => "w",
                    Access::ReadWrite => "r/w",
                },
                w.hits
            ));
        }
        if l.is_empty() {
            l.push("no breakpoints or watches".into());
        }
        rows("breaks", l, json!({}))
    }

    fn cmd_delete(&mut self, a: &Args) -> Result<Output> {
        let Some(w) = a.pos.first() else { return usage("delete <id>|all") };
        if w == "all" {
            let n = self.bps.len() + self.wps.len();
            self.bps.clear();
            self.wps.clear();
            self.apply_watches()?;
            return Ok(rows("delete", vec![format!("deleted {n}")], json!({})));
        }
        let id: u32 =
            w.trim_start_matches('#').parse().map_err(|_| crate::Error::Usage(format!("{w:?} is not an id")))?;
        let before = self.bps.len() + self.wps.len();
        self.bps.retain(|b| b.id != id);
        self.wps.retain(|x| x.id != id);
        if self.bps.len() + self.wps.len() == before {
            return fail(format!("no breakpoint or watch #{id}"));
        }
        self.apply_watches()?;
        Ok(rows("delete", vec![format!("deleted #{id}")], json!({})))
    }

    fn cmd_enable(&mut self, on: bool, a: &Args) -> Result<Output> {
        let Some(w) = a.pos.first() else { return usage("enable|disable <id>|all") };
        let all = w == "all";
        let id: u32 = if all {
            0
        } else {
            w.trim_start_matches('#').parse().map_err(|_| crate::Error::Usage(format!("{w:?} is not an id")))?
        };
        let mut n = 0;
        for b in self.bps.iter_mut().filter(|b| all || b.id == id) {
            b.enabled = on;
            n += 1;
        }
        if n == 0 {
            return fail(format!("no breakpoint #{id}"));
        }
        Ok(rows("enable", vec![format!("{} {n}", if on { "enabled" } else { "disabled" })], json!({})))
    }

    fn cmd_step(&mut self, a: &Args, over: bool) -> Result<Output> {
        let n = match a.pos.first() {
            Some(x) => parse_addr(x).map_err(|_| crate::Error::Usage("step needs a count".into()))?.clamp(1, 100_000),
            None => 1,
        };
        let w = a.wait()?;
        let mut last = None;
        for i in 0..n {
            self.need_stopped()?;
            let run = if over { self.next_run()? } else { Run { step: Some(self.cur), temp: None } };
            let out = self.go(run, w)?;
            let again = matches!(self.history.back(), Some(r) if r.why == "step") && self.state == State::Stopped;
            last = Some(out);
            if !again || i + 1 == n {
                break;
            }
        }
        Ok(last.unwrap_or_else(|| rows("step", vec![], json!({}))))
    }

    /// A step that does not go into calls: a temporary breakpoint after the call instruction.
    fn next_run(&mut self) -> Result<Run> {
        let r = self.t.regs(self.cur)?;
        let bytes = self.read_vec(r.rip, 16);
        let ins = decode_live(&bytes, r.rip, &|n| r.get(n));
        match ins {
            Some(i) if i.call => Ok(Run {
                step: None,
                temp: Some(Temp { addr: r.rip + i.len as u64, min_sp: r.rsp(), kind: TempKind::Next }),
            }),
            _ => Ok(Run { step: Some(self.cur), temp: None }),
        }
    }

    fn frames(&mut self, max: usize) -> Result<Vec<crate::modules::Frame>> {
        let r = self.tid_regs()?;
        let Session { t, mods, .. } = self;
        let mut rd = |a: u64| {
            let mut b = [0u8; 8];
            (t.read(a, &mut b) == 8).then(|| u64::from_le_bytes(b))
        };
        Ok(mods.backtrace(&r, &mut rd, max))
    }

    fn cmd_finish(&mut self, a: &Args) -> Result<Output> {
        let f = self.frames(3)?;
        let Some(caller) = f.get(1) else {
            return fail("cannot find the caller of this frame (no unwind data): use `until <address>`");
        };
        let w = a.wait()?;
        self.go(Run { step: None, temp: Some(Temp { addr: caller.pc, min_sp: caller.sp, kind: TempKind::Finish }) }, w)
    }

    fn cmd_until(&mut self, a: &Args) -> Result<Output> {
        let Some(t) = a.pos.first() else { return usage("until <address|name>") };
        let addr = self.addr_of(t)?;
        let w = a.wait()?;
        self.go(Run { step: None, temp: Some(Temp { addr, min_sp: 0, kind: TempKind::Until }) }, w)
    }

    fn cmd_mem(&mut self, a: &Args) -> Result<Output> {
        let (Some(at), len) = (a.pos.first(), a.pos.get(1)) else {
            return usage("mem <address|expr> [length] [--as u8|u16|u32|u64|i32|f32|f64|ptr|str]");
        };
        self.need_stopped()?;
        let addr = self.addr_of(at)?;
        let ty = match a.flag("as") {
            Some(t) => ElemType::parse(t)?,
            None => ElemType::U8,
        };
        let len = match len {
            Some(l) => self.addr_of(l)?,
            None => 64,
        };
        if len > 1 << 20 {
            return usage("mem reads at most 1 MiB at a time");
        }
        let bytes = self.read_vec(addr, len as usize);
        if bytes.is_empty() {
            return fail(format!("{addr:#x} cannot be read"));
        }
        let mods = std::cell::RefCell::new(&mut self.mods);
        let mut r = typed_rows(&bytes, addr, ty, &|x| mods.borrow_mut().label(x, true));
        if (bytes.len() as u64) < len {
            r.push(nexo_bin::Row {
                key: u64::MAX,
                text: format!("... only {} of {len} bytes are readable", bytes.len()),
                json: None,
            });
        }
        Ok(Output {
            op: "mem",
            meta: json!({"addr": addr, "len": bytes.len()}),
            rows: r,
            tail: 0,
            command: String::new(),
        })
    }

    fn cmd_write(&mut self, a: &Args) -> Result<Output> {
        let (Some(at), Some(hex)) = (a.pos.first(), a.pos.get(1..)) else {
            return usage("write <address> <hex bytes>");
        };
        self.need_stopped()?;
        let addr = self.addr_of(at)?;
        let pat = parse_pattern(&hex.join(" "))?;
        let Some(bytes) = pat.into_iter().collect::<Option<Vec<u8>>>() else {
            return usage("write takes exact bytes, no `??`");
        };
        if bytes.is_empty() || bytes.len() > 4096 {
            return usage("write takes 1 to 4096 bytes");
        }
        self.t.write(addr, &bytes)?;
        Ok(rows("write", vec![format!("wrote {} bytes at {addr:#x}", bytes.len())], json!({})))
    }

    fn cmd_print(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage("print <expression> [--as str]");
        }
        self.need_stopped()?;
        let src = a.pos.join(" ");
        let v = self.eval(&src)?;
        let mut s = format!("{src} = {v:#x} ({})", v as i64);
        if let Some(l) = self.mods.label(v, true) {
            s.push_str(&format!(" -> {l}"));
        }
        if a.flag("as") == Some("str") {
            s.push_str(&format!(" {}", self.cstr(v)));
        }
        Ok(rows("print", vec![s], json!({"value": v})))
    }

    fn cmd_args(&mut self, _a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        let regs = match self.t.os() {
            Os::Linux => ["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
            Os::Windows => ["rcx", "rdx", "r8", "r9", "", ""],
        };
        let mut l = Vec::new();
        for (i, n) in regs.iter().enumerate().filter(|(_, n)| !n.is_empty()) {
            let v = r.get(n).unwrap_or(0);
            let mut s = format!("arg{} {n} = {v:#x} ({})", i + 1, v as i64);
            if let Some(lab) = self.mods.label(v, true) {
                s.push_str(&format!(" -> {lab}"));
            }
            l.push(s);
        }
        Ok(rows("args", l, json!({})))
    }

    fn cmd_bt(&mut self, a: &Args) -> Result<Output> {
        let max = match a.pos.first() {
            Some(x) => parse_addr(x)?.clamp(1, 1000) as usize,
            None => 50,
        };
        let f = self.frames(max)?;
        Ok(rows("bt", self.frame_lines(&f), json!({"frames": f.len()})))
    }

    pub fn frame_lines(&mut self, f: &[crate::modules::Frame]) -> Vec<String> {
        f.iter()
            .enumerate()
            .map(|(i, fr)| {
                let look = if i == 0 { fr.pc } else { fr.pc.wrapping_sub(1) };
                let name = self.mods.label(look, false).unwrap_or_default();
                let at = self.mods.source(look).map(|s| format!("  ({s})")).unwrap_or_default();
                format!("#{i:<2} {:#x} {name}{at}", fr.pc)
            })
            .collect()
    }

    fn cmd_dis(&mut self, a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        let at = match a.pos.first() {
            Some(t) => self.addr_of(t)?,
            None => r.rip,
        };
        let n = match a.pos.get(1) {
            Some(x) => parse_addr(x)?.clamp(1, 200) as usize,
            None => 8,
        };
        let bytes = self.read_vec(at, n * 15);
        let (mut pos, mut l) = (0usize, Vec::new());
        for _ in 0..n {
            let ip = at + pos as u64;
            let Some(i) = decode_live(&bytes[pos.min(bytes.len())..], ip, &|x| r.get(x)) else { break };
            let mark = if ip == r.rip { "=>" } else { "  " };
            let name = if pos == 0 {
                self.mods.label(ip, false).map(|n| format!("  <{n}>")).unwrap_or_default()
            } else {
                String::new()
            };
            l.push(format!("{mark} {ip:#x}: {}{name}", i.text));
            pos += i.len;
        }
        Ok(rows("dis", l, json!({})))
    }

    fn cmd_threads(&mut self) -> Result<Output> {
        let mut l = Vec::new();
        if self.state == State::Stopped {
            for tid in self.t.threads() {
                let pc = self.t.regs(tid).map(|r| r.rip).unwrap_or(0);
                let name = self.mods.label(pc, false).unwrap_or_default();
                l.push(format!("{} thread {tid} at {pc:#x} {name}", if tid == self.cur { "*" } else { " " }));
            }
        } else {
            l.push(format!("{} thread(s); `stop` first to see where they are", self.t.threads().len()));
        }
        Ok(rows("threads", l, json!({})))
    }

    fn cmd_thread(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        let Some(n) = a.pos.first() else {
            return Ok(rows("thread", vec![format!("current thread {}", self.cur)], json!({})));
        };
        let tid: Tid = n.parse().map_err(|_| crate::Error::Usage("thread <tid>".into()))?;
        if !self.t.threads().contains(&tid) {
            return fail(format!("no thread {tid}: `threads` lists them"));
        }
        self.cur = tid;
        Ok(rows("thread", vec![format!("current thread {tid}")], json!({})))
    }

    fn slots(&self) -> [Option<HwSlot>; 4] {
        let mut s = [None; 4];
        for w in &self.wps {
            s[w.slot] = Some(w.hw);
        }
        s
    }

    fn apply_watches(&mut self) -> Result<()> {
        if self.state == State::Exited {
            return Ok(());
        }
        let s = self.slots();
        self.t.set_watch(&s)
    }

    fn cmd_watch(&mut self, a: &Args) -> Result<Output> {
        let Some(at) = a.pos.first() else { return usage("watch <address|expr> [length 1|2|4|8] [r|w|x]") };
        self.need_stopped()?;
        let addr = self.addr_of(at)?;
        let mut len = 8u8;
        let mut acc = Access::Write;
        for w in &a.pos[1..] {
            match w.as_str() {
                "w" => acc = Access::Write,
                "r" | "rw" => acc = Access::ReadWrite,
                "x" => acc = Access::Exec,
                n => {
                    len = parse_addr(n)
                        .ok()
                        .and_then(|v| u8::try_from(v).ok())
                        .filter(|v| [1, 2, 4, 8].contains(v))
                        .ok_or_else(|| crate::Error::Usage("length is 1, 2, 4 or 8".into()))?
                }
            }
        }
        if acc == Access::Exec {
            len = 1;
        }
        if addr % len as u64 != 0 {
            return usage(format!("{addr:#x} is not aligned to {len}: the CPU watches aligned pieces"));
        }
        let Some(slot) = (0..4).find(|i| !self.wps.iter().any(|w| w.slot == *i)) else {
            return fail("all 4 hardware watches are used: `delete <id>` one first");
        };
        let id = self.new_id();
        let last = self.read_vec(addr, len as usize);
        self.wps.push(Wp { id, slot, hw: HwSlot { addr, len, access: acc }, last, hits: 0 });
        if let Err(e) = self.apply_watches() {
            self.wps.pop();
            return Err(e);
        }
        Ok(rows("watch", vec![format!("watch #{id} {addr:#x} len {len}")], json!({"id": id})))
    }

    fn cmd_maps(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped().or_else(|e| if self.state == State::Running { Ok(()) } else { Err(e) })?;
        let f = a.pos.first();
        let mut l = Vec::new();
        for m in self.t.maps()? {
            if f.is_some_and(|f| !m.path.contains(f.as_str())) {
                continue;
            }
            l.push(format!(
                "{:#014x}-{:#014x} {}{}{} {:>9} {}",
                m.start,
                m.end,
                if m.read { 'r' } else { '-' },
                if m.write { 'w' } else { '-' },
                if m.exec { 'x' } else { '-' },
                m.end - m.start,
                m.path
            ));
        }
        Ok(rows("maps", l, json!({})))
    }

    fn cmd_modules(&mut self) -> Result<Output> {
        self.mods.refresh(&mut *self.t);
        let l = self.mods.list.iter().map(|m| format!("{:#014x}-{:#014x} {}", m.base, m.end, m.path)).collect();
        Ok(rows("modules", l, json!({})))
    }

    fn cmd_find(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        let Some(first) = a.pos.first() else { return usage("find <hex bytes with ?? | \"text\"> [--in <map name>]") };
        let pat: Vec<Option<u8>> = if first.starts_with('"') {
            a.pos.join(" ").trim_matches('"').bytes().map(Some).collect()
        } else {
            parse_pattern(&a.pos.join(" "))?
        };
        if pat.is_empty() {
            return usage("an empty pattern matches everywhere");
        }
        let only = a.flag("in");
        let (mut l, mut scanned) = (Vec::new(), 0u64);
        'maps: for m in self.t.maps()? {
            if !m.read || only.is_some_and(|o| !m.path.contains(o)) || m.path == "[vvar]" || m.path == "[vsyscall]" {
                continue;
            }
            let mut pos = m.start;
            while pos < m.end {
                let n = ((m.end - pos) as usize).min((1 << 20) + pat.len());
                let buf = self.read_vec(pos, n);
                if buf.is_empty() {
                    break;
                }
                scanned += buf.len() as u64;
                for i in 0..buf.len().saturating_sub(pat.len() - 1) {
                    if pat.iter().enumerate().all(|(j, p)| p.is_none_or(|b| buf[i + j] == b)) {
                        let at = pos + i as u64;
                        let lab = self.mods.label(at, true).unwrap_or_else(|| m.path.clone());
                        l.push(format!("{at:#x} {lab}"));
                        if l.len() >= 100 {
                            l.push("... 100 matches: narrow with `--in <map name>`".into());
                            break 'maps;
                        }
                    }
                }
                pos += (1 << 20) as u64;
                if scanned > 1 << 29 {
                    l.push("... scanned 512 MiB: narrow with `--in <map name>`".into());
                    break 'maps;
                }
            }
        }
        if l.is_empty() {
            l.push(format!("not found in {scanned} bytes"));
        }
        Ok(rows("find", l, json!({})))
    }

    fn cmd_out(&mut self, a: &Args) -> Output {
        self.drain_output();
        let from = if a.has("all") { 0 } else { self.out_seen.min(self.out.len()) };
        let text = String::from_utf8_lossy(&self.out[from..]).to_string();
        self.out_seen = self.out.len();
        let mut l: Vec<String> = text.lines().map(str::to_string).collect();
        if self.out_dropped > 0 && a.has("all") {
            l.insert(0, format!("... {} older bytes were dropped (the last 1 MiB is kept)", self.out_dropped));
        }
        if l.is_empty() {
            l.push("no new output".into());
        }
        rows("out", l, json!({}))
    }

    fn cmd_history(&mut self, a: &Args) -> Output {
        let n = a.pos.first().and_then(|x| x.parse::<usize>().ok()).unwrap_or(10);
        let skip = self.history.len().saturating_sub(n);
        let mut l = Vec::new();
        for r in self.history.iter().skip(skip) {
            let name = self.mods.label(r.regs.rip, false).unwrap_or_default();
            l.push(format!(
                "stop {} thread {} {} at {:#x} {name} | rax {:#x} rdi {:#x} rsi {:#x} rsp {:#x}",
                r.n,
                r.tid,
                r.why,
                r.regs.rip,
                r.regs.gp[0],
                r.regs.gp[5],
                r.regs.gp[4],
                r.regs.rsp()
            ));
        }
        if l.is_empty() {
            l.push("no stops yet".into());
        }
        rows("history", l, json!({}))
    }

    fn cmd_stdin(&mut self, a: &Args) -> Result<Output> {
        if a.has("close") {
            self.t.stdin(None)?;
            return Ok(rows("stdin", vec!["standard input closed".into()], json!({})));
        }
        if a.pos.is_empty() {
            return usage(
                "stdin <text> (a newline is added; --raw sends exactly the text, \\n \\t escapes work) | stdin --close",
            );
        }
        let mut text = a.pos.join(" ").replace("\\n", "\n").replace("\\t", "\t");
        if !a.has("raw") {
            text.push('\n');
        }
        let n = self.t.stdin(Some(text.as_bytes()))?;
        Ok(rows("stdin", vec![format!("sent {n} bytes")], json!({})))
    }

    fn cmd_catch(&mut self, a: &Args) -> Result<Output> {
        let names: Vec<(u32, &str)> = (1..32).map(|n| (n, signal_name(n))).filter(|(_, s)| *s != "signal").collect();
        let by = |w: &str| -> Result<u32> {
            names
                .iter()
                .find(|(n, s)| s.eq_ignore_ascii_case(w) || s[3..].eq_ignore_ascii_case(w) || n.to_string() == w)
                .map(|(n, _)| *n)
                .ok_or_else(|| crate::Error::Usage(format!("{w:?} is not a signal: SIGSEGV, SIGABRT, 11 ...")))
        };
        match a.pos.first().map(String::as_str) {
            Some("add") | Some("on") => {
                for w in &a.pos[1..] {
                    self.catch.insert(by(w)?);
                }
            }
            Some("del") | Some("off") => {
                for w in &a.pos[1..] {
                    self.catch.remove(&by(w)?);
                }
            }
            Some("all") => self.catch = names.iter().map(|(n, _)| *n).filter(|n| ![17, 28].contains(n)).collect(),
            Some(_) => return usage("catch [add|del <signal>...] | catch all"),
            None => {}
        }
        let l: Vec<String> = self.catch.iter().map(|n| signal_name(*n).to_string()).collect();
        Ok(rows("catch", vec![format!("stops on: {}", l.join(" "))], json!({})))
    }

    fn cmd_kill(&mut self) -> Result<Output> {
        self.uninsert_all();
        self.t.kill()?;
        self.state = State::Exited;
        self.exit = Some("killed by the debugger".into());
        Ok(rows("kill", vec!["program killed".into()], json!({})))
    }

    fn cmd_detach(&mut self) -> Result<Output> {
        if self.state == State::Stopped {
            self.uninsert_all();
        }
        self.t.detach()?;
        self.state = State::Exited;
        self.exit = Some("detached".into());
        Ok(rows("detach", vec!["the program goes on without the debugger".into()], json!({})))
    }

    /// Several commands in one call: `do "break f; cont; args; bt"`. It stops at the first command that fails,
    /// and when the program ends; `--keep-going` goes on after a failure.
    fn cmd_do(&mut self, words: &[String]) -> Result<Output> {
        let a = Args::new(words)?;
        if a.pos.is_empty() {
            return usage("do \"<command>; <command>; ...\" [--keep-going]");
        }
        let script = a.pos.join(" ");
        let mut out: Vec<String> = Vec::new();
        for part in script.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let w = split_words(part);
            if w.first().is_some_and(|c| c == "do") {
                return usage("`do` inside `do` is not allowed");
            }
            out.push(format!("> {part}"));
            match self.exec(&w) {
                Ok(o) => out.extend(o.rows.iter().map(|r| r.text.clone())),
                Err(e) => {
                    out.push(format!("error: {e}"));
                    if !a.has("keep-going") {
                        out.push("... script stopped at the error".into());
                        break;
                    }
                }
            }
            if self.state == State::Exited && !matches!(w[0].as_str(), "out" | "log" | "history" | "status") {
                out.push("... script stopped: the program ended".into());
                break;
            }
        }
        Ok(rows("do", out, json!({})))
    }

    /// Tell the stop: why, where, the instruction, what changed.
    pub fn stopped(&mut self, tid: Tid, why: Why) -> Output {
        self.nstop += 1;
        self.drain_output();
        let mut l: Vec<String> = Vec::new();
        let headline = match &why {
            Why::Entry => "stopped at the program's entry".to_string(),
            Why::Step => "step".to_string(),
            Why::Next => "next".to_string(),
            Why::Finish => "finish: returned".to_string(),
            Why::Until => "reached the address".to_string(),
            Why::Break(ids) => {
                format!("breakpoint {}", ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" "))
            }
            Why::Watch(ids) => format!("watch {}", ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" ")),
            Why::Signal(f) => format!("{} (signal {})", signal_name(f.number), f.number),
            Why::Int3 => "int3 in the program (not ours)".to_string(),
            Why::Interrupt => "stopped on request".to_string(),
            Why::Exec => "the program started another program (exec): addresses are new".to_string(),
            Why::Exited(c) => format!("the program exited with code {c}"),
            Why::Killed(s) => format!("the program was killed by {}", signal_name(*s as u32)),
        };
        let event = match &why {
            Why::Entry => "entry",
            Why::Step => "step",
            Why::Next => "next",
            Why::Finish => "finish",
            Why::Until => "until",
            Why::Break(_) => "breakpoint",
            Why::Watch(_) => "watch",
            Why::Signal(_) => "signal",
            Why::Int3 => "int3",
            Why::Interrupt => "interrupt",
            Why::Exec => "exec",
            Why::Exited(_) => "exit",
            Why::Killed(_) => "killed",
        };
        let short = match &why {
            Why::Step | Why::Next => "step".to_string(),
            other => format!("{other:?}").split(['(', ' ']).next().unwrap_or("").to_lowercase(),
        };
        if matches!(why, Why::Exited(_) | Why::Killed(_)) {
            l.push(headline);
            if let Some(o) = self.new_output_line() {
                l.push(o);
            }
            self.last_stop = Some(StopInfo { n: self.nstop, event: event.into(), place: "-".into(), addr: 0 });
            self.history.push_back(Rec { n: self.nstop, tid, why: short, regs: crate::Regs::default() });
            return rows("exit", l, json!({"state": "exited"}));
        }
        l.push(format!("{headline} (thread {tid})"));
        if let Ok(r) = self.t.regs(tid) {
            let name = self.mods.label(r.rip, false).unwrap_or_default();
            let at = self.mods.source(r.rip).map(|s| format!("  ({s})")).unwrap_or_default();
            l.push(format!("pc {:#x} {name}{at}", r.rip));
            self.last_stop = Some(StopInfo {
                n: self.nstop,
                event: event.into(),
                place: if name.is_empty() { "?".into() } else { name.clone() },
                addr: r.rip,
            });
            let bytes = self.read_vec(r.rip, 16);
            let ins: Option<LiveIns> = decode_live(&bytes, r.rip, &|n| r.get(n));
            if let Some(i) = &ins {
                l.push(format!("=> {}", i.text));
                // the next two as well: what the program is about to do, without a `dis` call
                let (mut at, mut len) = (r.rip, i.len as u64);
                for _ in 0..2 {
                    at += len;
                    let b = self.read_vec(at, 16);
                    let Some(n) = decode_live(&b, at, &|_| None) else { break };
                    l.push(format!("   {:#x} {}", at, n.text));
                    len = n.len as u64;
                }
            }
            if let Why::Watch(ids) = &why {
                for id in ids {
                    let Some(i) = self.wps.iter().position(|w| w.id == *id) else { continue };
                    let (addr, len) = (self.wps[i].hw.addr, self.wps[i].hw.len as usize);
                    let now = self.read_vec(addr, len);
                    let old = std::mem::replace(&mut self.wps[i].last, now.clone());
                    self.wps[i].hits += 1;
                    l.push(format!("watch #{id} {addr:#x}: {} -> {}", hex(&old), hex(&now)));
                }
            }
            if let Why::Signal(f) = &why {
                l.extend(self.triage(f, &r, ins.as_ref()));
            }
            let changed = match self.prev.get(&tid) {
                Some(p) => r.changed(p),
                None => Vec::new(),
            };
            if !changed.is_empty() {
                let more = changed.len().saturating_sub(CHANGED_SHOWN);
                let mut line = format!(
                    "changed: {}",
                    changed
                        .iter()
                        .take(CHANGED_SHOWN)
                        .map(|(n, o, v)| format!("{n} {o:#x} -> {v:#x}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if more > 0 {
                    line.push_str(&format!(" (+{more} more: `regs`)"));
                }
                l.push(line);
            }
            self.prev.insert(tid, r);
            if let Why::Signal(_) = &why {
                if let Ok(f) = self.frames(8) {
                    l.push("backtrace:".into());
                    let fl = self.frame_lines(&f);
                    l.extend(fl.into_iter().map(|s| format!("  {s}")));
                }
            }
            self.history.push_back(Rec { n: self.nstop, tid, why: short, regs: r });
            while self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        if let Some(o) = self.new_output_line() {
            l.push(o);
        }
        if !self.log.is_empty() {
            l.push(format!("log: {} lines (`log` shows them)", self.log.len()));
        }
        rows("stop", l, json!({"state": "stopped", "thread": tid}))
    }

    /// `name <addr> <name>` and `note [<0xaddr>] <text>`: keep a finding for this binary. A note without an
    /// address is for the place the program stopped at. Answers with one `saved:` line.
    fn cmd_keep(&mut self, is_name: bool, w: &[String]) -> Result<Output> {
        let Some(dir) = crate::knowledge::data_dir() else {
            return fail("nothing can be kept here: this host has no data folder for names and notes");
        };
        let mine = |s: &mut Session, a: u64| -> Result<u64> {
            let (base, bias) = (s.mods.main_base(), s.mods.main_bias());
            if base == 0 || s.mods.locate(a) != s.mods.locate(base) {
                return fail(format!(
                    "{a:#x} is not inside the program itself; only its own code and data can be named"
                ));
            }
            Ok(a.wrapping_sub(bias))
        };
        let (addr, text) = if is_name {
            if w.len() != 2 {
                return usage("usage: name <addr> <name>   (a name for a function or data; letters, digits and _ . : $ @ < > ~ -)");
            }
            let a = self.addr_of(&w[0])?;
            (mine(self, a)?, w[1].clone())
        } else {
            let explicit = w.len() > 1 && w[0].starts_with("0x");
            if explicit {
                let a = self.addr_of(&w[0])?;
                (mine(self, a)?, w[1..].join(" "))
            } else {
                if w.is_empty() {
                    return usage(
                        "usage: note [0xaddr] <text>   (without an address the note is for the current place)",
                    );
                }
                let pc = self.tid_regs()?.rip;
                (mine(self, pc)?, w.join(" "))
            }
        };
        let target = format!("{addr:#x}");
        let img = self.mods.main_image();
        let r = if is_name {
            nexo_bin::name_set(img, &dir, &target, &text)
        } else {
            nexo_bin::note_set(img, &dir, &target, &text)
        };
        let o = r.map_err(|e| crate::Error::Fail(e.to_string()))?;
        if let Some(why) = self.mods.reload_names(&dir) {
            return fail(why);
        }
        let line = o.rows.first().map(|r| r.text.clone()).unwrap_or_default();
        let line = format!("saved: {line} (kept for this binary)");
        Ok(rows(if is_name { "name" } else { "note" }, vec![line], json!({"addr": addr})))
    }

    /// The likely cause of a fault, from the signal, the address and the instruction that did it.
    fn triage(&mut self, f: &Fault, r: &crate::Regs, ins: Option<&LiveIns>) -> Vec<String> {
        let mut l = Vec::new();
        let sp = r.rsp();
        match f.number {
            11 | 7 => {
                let addr = f.addr.unwrap_or(0);
                let what = if f.code == 1 { "address not mapped" } else if f.code == 2 { "mapped but not allowed" } else { "bad access" };
                let kind = if addr < 0x1000 {
                    format!("null pointer (offset {addr:#x} into a null object)")
                } else if addr.abs_diff(sp) < 0x10000 {
                    "next to the stack pointer: stack overflow or a corrupted stack".to_string()
                } else if addr == r.rip {
                    "jumped to an address that holds no code (a bad function pointer or a smashed return address)".to_string()
                } else if addr > 0x0000_8000_0000_0000 {
                    "not a valid user address (an uninitialised or corrupted pointer)".to_string()
                } else {
                    self.mods.label(addr, true).map_or(what.to_string(), |n| format!("{what}; near {n}"))
                };
                l.push(format!("fault address {addr:#x}: {kind}"));
                if let Some(i) = ins {
                    for m in &i.mem {
                        if let Some(a) = m.addr {
                            l.push(format!(
                                "the instruction {} {} byte(s) at {a:#x}",
                                if m.write { "writes" } else { "reads" },
                                m.size
                            ));
                        }
                    }
                }
            }
            6 => l.push("abort(): the program stopped itself (an assert, a panic, a failed check, or a corrupted heap): read the frames below".into()),
            8 => l.push("arithmetic fault: an integer division by zero or an overflow in the instruction above".into()),
            4 => l.push("illegal instruction: the code is damaged or the jump went into data".into()),
            _ => {}
        }
        l
    }
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "?".into();
    }
    b.iter().rev().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

/// Words split like a shell would with double quotes (no escapes).
pub fn split_words(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut q, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                q = !q;
                any = true;
            }
            c if c.is_whitespace() && !q => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn help() -> Output {
    let l = [
        "run: nexo dbg run <exe> [args]   attach: nexo dbg attach <pid>   core: nexo dbg core <file>",
        "go:    cont [--wait N] | step [N] | next [N] | finish | until <addr> | stop | wait [--wait N] | kill | detach",
        "break: break <addr|name|name+off|import> [if <expr>] [after N] [log <text with {expr}>] [once] | breaks | delete <id>|all | enable/disable <id>",
        "look:  regs [name] | bt [N] | dis [addr] [N] | mem <addr> [len] [--as u8|u32|u64|ptr|str] | print <expr> [--as str] | args | threads | thread <tid>",
        "       maps [filter] | modules | find <hex|\"text\"> [--in <map>] | out [--all] | log | history [N] | status",
        "change (asks): setreg <reg> <value> | write <addr> <hex> | watch <addr> [len] [r|w|x] | stdin <text> | catch add|del <SIG>",
        "script: do \"break f; cont; args; bt\" [--keep-going]     expressions: rax+8, *(u32*)(rdi+8), main+4, rsp, sym == 3",
        "flags, first in the command: -s <id> (the session when several are live) | --lines N (rows per answer, default 40) | --from <addr> (continue a long answer from this row)",
        "addresses: a number inside the program's sections is a file address (as `nexo bin` shows); any other number is a memory address",
    ];
    rows("help", l.iter().map(|s| s.to_string()).collect(), json!({}))
}
