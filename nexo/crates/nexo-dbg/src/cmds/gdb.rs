//! The gdb dialect on a live session: `x/20bx`, `p/x`, `printf`, `echo`, `set $v = ...`, `commands ... end`, `display`,
//! `info registers|breakpoints|frame|sharedlibrary|proc mappings`, and the std::string / std::vector views. The
//! text work (formats, scripts, aliases) is in `crate::gdbdialect`.

use super::*;
use crate::expr;
use crate::gdbdialect as g;
use crate::platform::Tid;

/// Lines one `commands` run may write per hit; the rest is counted.
const ACTION_LINES: usize = 40;
/// Longest string `%s` and `x/s` read.
const STR_MAX: usize = 4096;
/// Elements and string bytes a std view shows.
const VIEW_SHOWN: usize = 64;
const VIEW_TEXT: usize = 400;

/// Commands a `commands` block may hold: they look, or keep a convenience variable.
const ACTION_OK: [&str; 24] = [
    "x", "print", "p", "output", "printf", "echo", "set", "regs", "mem", "args", "locals", "bt", "dis", "find",
    "status", "info", "maps", "modules", "threads", "out", "log", "history", "silent", "continue",
];

fn is_reg(n: &str) -> bool {
    crate::Regs::default().get(n).is_some()
}

/// The words of a `break` with the place put together: `break *(f + 0x10) if x` has the place `*(f + 0x10)`,
/// `break 'ns::f(int, char)'` has `ns::f(int, char)`. Unchanged words give `None`.
fn merge_place(words: &[String]) -> Option<Vec<String>> {
    let first = words.get(1)?;
    let is_key = |w: &str| matches!(w, "if" | "after" | "log" | "once" | "thread" | "task") || w.starts_with("--");
    let open = |s: &str| s.matches('(').count() as i64 - s.matches(')').count() as i64;
    let quoted = |s: &str| s.matches('\'').count() % 2 == 1;
    let mut spec = first.clone();
    let mut used = 1;
    while let Some(next) = words.get(1 + used) {
        let more =
            if spec.starts_with('*') { !is_key(next) } else { (open(&spec) > 0 || quoted(&spec)) && !is_key(next) };
        if !more {
            break;
        }
        spec.push(' ');
        spec.push_str(next);
        used += 1;
    }
    let unquoted = spec.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).map(str::to_string);
    if used == 1 && unquoted.is_none() {
        return None;
    }
    let mut out = vec![words[0].clone(), unquoted.unwrap_or(spec)];
    out.extend(words[1 + used..].iter().cloned());
    Some(out)
}

fn lines_of(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

impl Session {
    /// The gdb spellings. `Some` when the words were gdb's and have been answered; `None` for the debugger's own.
    pub(super) fn gdb_exec(&mut self, words: &[String]) -> Option<Result<Output>> {
        let first = words.first()?.as_str();
        let name = g::base_name(first);
        let fmt = first.split_once('/').map(|(_, f)| f);
        let rest = &words[1..];
        Some(match name {
            "x" if fmt.is_some() => self.gdb_x(fmt.unwrap_or(""), rest),
            "print" | "p" | "output" if fmt.is_some() => self.gdb_print(fmt, rest),
            "output" if fmt.is_none() => self.gdb_output(rest),
            "printf" => self.gdb_printf(rest),
            "echo" => Ok(rows("echo", lines_of(&g::unescape(&rest.join(" "))), json!({}))),
            "set" => self.gdb_set(words),
            "show" => Ok(rows("show", vec!["settings are fixed here: nothing to show".into()], json!({}))),
            "commands" => self.gdb_commands(rest),
            "silent" => Ok(rows("silent", Vec::new(), json!({}))),
            "end" => usage("`end` closes a `commands` block; as its own call it closes the session"),
            "display" => self.gdb_display(fmt, rest),
            "undisplay" => self.gdb_undisplay(rest),
            "delete" if rest.first().is_some_and(|w| w == "display") => self.gdb_undisplay(&rest[1..]),
            "condition" => self.gdb_condition(rest),
            "ignore" => self.gdb_ignore(rest),
            "info" if !rest.is_empty() => self.gdb_info(rest),
            "break" | "tbreak" => {
                let mut w = words.to_vec();
                w[0] = name.to_string();
                return merge_place(&w).map(|m| self.exec(&m));
            }
            "quit" => usage(g::hint("quit").unwrap_or("`end` closes the session")),
            n if n != first && fmt.is_none() => {
                let mut w = words.to_vec();
                w[0] = n.to_string();
                self.exec(&w)
            }
            _ => return None,
        })
    }

    /// While `commands` is open, every line is kept for the breakpoint until `end`.
    pub(super) fn record_line(&mut self, words: &[String]) -> Result<Output> {
        let name = g::base_name(words.first().map(String::as_str).unwrap_or(""));
        if name == "end" {
            let (ids, actions) = self.recording.take().unwrap_or_default();
            for b in self.bps.iter_mut().filter(|b| ids.contains(&b.id)) {
                b.actions = actions.clone();
            }
            let list = ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" ");
            let goes_on = actions.iter().any(|w| matches!(g::base_name(&w[0]), "continue" | "cont" | "c"));
            return Ok(rows(
                "commands",
                vec![format!(
                    "breakpoint {list}: {} commands{}",
                    actions.len(),
                    if goes_on { ", the program goes on after them" } else { ", it stops after them" }
                )],
                json!({}),
            ));
        }
        let ok = ACTION_OK.contains(&name)
            && (name != "set" || matches!(g::set_kind(words, &is_reg), g::SetKind::Var | g::SetKind::Noise));
        if !ok {
            self.recording = None;
            return usage(format!(
                "`{}` cannot run inside `commands`: only commands that look or keep a $variable (x, print, printf, echo, set $v = ..., bt, regs, mem, dis, find, info) and `continue`; the block was dropped",
                words.join(" ")
            ));
        }
        if let Some((_, acts)) = &mut self.recording {
            acts.push(words.to_vec());
        }
        Ok(rows("commands", Vec::new(), json!({})))
    }

    fn gdb_commands(&mut self, rest: &[String]) -> Result<Output> {
        let mut ids = Vec::new();
        for w in rest {
            let n: u32 = w.trim_start_matches('#').parse().map_err(|_| {
                crate::Error::Usage(format!("commands [N ...]: {w:?} is not a breakpoint number (`breaks` lists them)"))
            })?;
            if !self.bps.iter().any(|b| b.id == n && !b.call) {
                return fail(format!("no breakpoint #{n}: `breaks` lists them"));
            }
            ids.push(n);
        }
        if ids.is_empty() {
            match self.bps.iter().rev().find(|b| !b.call) {
                Some(b) => ids.push(b.id),
                None => return fail("`commands` belongs to a breakpoint: `break <place>` first"),
            }
        }
        let list = ids.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(" ");
        self.recording = Some((ids, Vec::new()));
        Ok(rows(
            "commands",
            vec![format!("commands for breakpoint {list}: the next lines up to `end` run at each hit")],
            json!({}),
        ))
    }

    /// Run the `commands` of breakpoint `id` at a hit, with the thread stopped; their output goes to the log. True
    /// when they end with `continue` (the program goes on), false when it must stop (no `continue`, or an error).
    pub(crate) fn run_actions(&mut self, tid: Tid, id: u32) -> bool {
        let Some(actions) = self.bps.iter().find(|b| b.id == id).map(|b| b.actions.clone()) else { return false };
        let (state, cur) = (self.state, self.cur);
        self.state = State::Stopped;
        self.cur = tid;
        let (mut go_on, mut budget, mut cut) = (false, ACTION_LINES, 0usize);
        for w in &actions {
            let name = g::base_name(&w[0]);
            match name {
                "silent" => continue,
                "continue" | "cont" | "c" => {
                    go_on = true;
                    break;
                }
                _ => {}
            }
            match self.exec(w) {
                Ok(o) => {
                    if name == "set" {
                        continue;
                    }
                    for r in o.rows {
                        for line in r.text.lines() {
                            if budget == 0 {
                                cut += 1;
                            } else {
                                budget -= 1;
                                self.log_line(line.to_string());
                            }
                        }
                    }
                }
                Err(e) => {
                    self.log_line(format!("#{id} `{}` failed: {e}", w.join(" ")));
                    break;
                }
            }
        }
        if cut > 0 {
            self.log_line(format!("#{id} {cut} more lines left out (at most {ACTION_LINES} per hit)"));
        }
        self.state = state;
        self.cur = cur;
        go_on
    }

    /// `x/NFU expr`: N units of U bytes shown as F, with gdb's line layout.
    fn gdb_x(&mut self, fmt: &str, rest: &[String]) -> Result<Output> {
        let f = g::parse_xfmt(fmt).map_err(crate::Error::Usage)?;
        if rest.is_empty() {
            return usage("x/FMT <address|expression>   e.g. x/20bx $rsp, x/s *(char**)($rbp-8), x/4i $pc, x/2gx buf");
        }
        self.need_stopped()?;
        let addr = self.addr_of(&rest.join(" "))?;
        match f.fmt {
            'i' => {
                let a = Args::new(&[format!("{addr:#x}"), f.count.to_string()])?;
                self.cmd_dis(&a)
            }
            's' => {
                let (mut at, mut l) = (addr, Vec::new());
                for _ in 0..f.count.min(200) {
                    let b = self.read_vec(at, STR_MAX);
                    if b.is_empty() {
                        l.push(format!("{at:#x}:\tcannot read memory here"));
                        break;
                    }
                    let end = b.iter().position(|&c| c == 0);
                    let text = String::from_utf8_lossy(&b[..end.unwrap_or(b.len())]).to_string();
                    let tag = self.mods.label(at, true).map(|x| format!(" <{x}>")).unwrap_or_default();
                    l.push(format!("{at:#x}{tag}:\t{text:?}{}", if end.is_none() { "..." } else { "" }));
                    at += end.map_or(b.len(), |e| e + 1) as u64;
                }
                Ok(rows("x", l, json!({"addr": addr})))
            }
            _ => {
                let total = f.count.saturating_mul(f.size);
                if total > 1 << 20 {
                    return usage("x reads at most 1 MiB at a time");
                }
                let bytes = self.read_vec(addr, total);
                if bytes.is_empty() {
                    return fail(format!("cannot access memory at {addr:#x}"));
                }
                let per = g::per_line(f.fmt, f.size);
                let units: Vec<u64> = bytes
                    .chunks_exact(f.size)
                    .map(|c| {
                        let mut b = [0u8; 8];
                        b[..f.size].copy_from_slice(c);
                        u64::from_le_bytes(b)
                    })
                    .collect();
                let mut l = Vec::new();
                for (i, row) in units.chunks(per).enumerate() {
                    let at = addr + (i * per * f.size) as u64;
                    let tag = self.mods.label(at, true).map(|x| format!(" <{x}>")).unwrap_or_default();
                    let cells: Vec<String> = row
                        .iter()
                        .map(|&u| {
                            if f.fmt == 'a' {
                                match self.mods.label(u, true) {
                                    Some(x) => format!("{u:#x} <{x}>"),
                                    None => format!("{u:#x}"),
                                }
                            } else {
                                g::show_unit(f.fmt, f.size, u)
                            }
                        })
                        .collect();
                    l.push(format!("{at:#x}{tag}:\t{}", cells.join("\t")));
                }
                if units.len() < f.count {
                    l.push(format!("... only {} of {} units are readable", units.len(), f.count));
                }
                Ok(rows("x", l, json!({"addr": addr, "len": bytes.len()})))
            }
        }
    }

    /// `print/x expr`, `output/d expr`: one value in a gdb format.
    fn gdb_print(&mut self, fmt: Option<&str>, rest: &[String]) -> Result<Output> {
        if rest.is_empty() {
            return usage("print/FMT <expression>   FMT: x d u o t c a z f s");
        }
        let src = rest.join(" ");
        let v = self.eval_any(&src)?;
        let letter = fmt.and_then(|f| f.chars().find(|c| c.is_ascii_alphabetic())).unwrap_or('x');
        let bits = if v.ty.stars == 0 { u32::from(v.ty.bytes) * 8 } else { 64 };
        let size = (bits / 8) as usize;
        let shown = match letter {
            's' => self.cstr(v.v),
            'a' => match self.mods.label(v.v, true) {
                Some(l) => format!("{:#x} <{l}>", v.v),
                None => format!("{:#x}", v.v),
            },
            'x' | 'd' | 'u' | 'o' | 't' | 'c' | 'z' | 'f' => {
                let cell = g::show_unit(letter, size.clamp(1, 8), v.v);
                // gdb prints the shortest form for x, t and z
                match letter {
                    'x' => format!("{:#x}", v.v & if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 }),
                    't' => cell
                        .trim_start_matches('0')
                        .to_string()
                        .chars()
                        .next()
                        .map_or("0".to_string(), |_| cell.trim_start_matches('0').to_string()),
                    _ => cell,
                }
            }
            other => return usage(format!("print/{other}: use x d u o t c a z f or s")),
        };
        Ok(rows("print", vec![format!("{src} = {shown}")], json!({"value": v.v})))
    }

    /// `output expr`: the value alone, as a number and, for a `char *`, its text.
    fn gdb_output(&mut self, rest: &[String]) -> Result<Output> {
        if rest.is_empty() {
            return usage("output <expression>");
        }
        let v = self.eval_any(&rest.join(" "))?;
        let line = match self.char_text(&v) {
            Some(t) => format!("{:#x} {t}", v.v),
            None => format!("{:#x}", v.v),
        };
        Ok(rows("output", vec![line], json!({"value": v.v})))
    }

    /// `printf "fmt", args...`.
    fn gdb_printf(&mut self, rest: &[String]) -> Result<Output> {
        let (fmt, args) = g::printf_parts(&rest.join(" ")).map_err(crate::Error::Usage)?;
        let pieces = g::parse_format(&g::unescape(&fmt)).map_err(crate::Error::Usage)?;
        let mut vals = Vec::new();
        for a in &args {
            vals.push(self.eval_any(a)?);
        }
        let mut text = String::new();
        let mut next = vals.iter();
        for p in &pieces {
            match p {
                g::Piece::Lit(s) => text.push_str(s),
                g::Piece::Conv(c) => {
                    let Some(v) = next.next() else { return usage("printf: fewer arguments than conversions") };
                    match c.spec {
                        's' => {
                            let b = self.read_vec(v.v, STR_MAX);
                            let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                            if b.is_empty() && v.v != 0 {
                                return fail(format!("cannot access memory at {:#x}", v.v));
                            }
                            let s = if v.v == 0 {
                                "(null)".to_string()
                            } else {
                                String::from_utf8_lossy(&b[..end]).to_string()
                            };
                            text.push_str(&g::render_str(c, &s));
                        }
                        'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                            let f = if v.ty.signed { v.v as i64 as f64 } else { v.v as f64 };
                            text.push_str(&g::render_float(c, f));
                        }
                        _ => text.push_str(&g::render_int(c, v.v)),
                    }
                }
            }
        }
        Ok(rows("printf", lines_of(&text), json!({})))
    }

    fn gdb_set(&mut self, words: &[String]) -> Result<Output> {
        match g::set_kind(words, &is_reg) {
            g::SetKind::Noise => Ok(rows(
                "set",
                vec![format!("ignored: {} (no pager, prompts or settings in this debugger)", words.join(" "))],
                json!({}),
            )),
            g::SetKind::Var => {
                let at = if words.get(1).is_some_and(|w| w == "var" || w == "variable") { 2 } else { 1 };
                let v = self.eval_any(&words[at..].join(" "))?;
                let name = words[at..].join(" ");
                let name = name.split('=').next().unwrap_or("").trim().to_string();
                Ok(rows("set", vec![format!("{name} = {:#x}", v.v)], json!({"value": v.v})))
            }
            g::SetKind::Register(r, e) => self.exec(&["setreg".to_string(), r, e]),
            g::SetKind::Args(_) => usage("the arguments go in `run <path> <args>`; a gdb script can use `set args` before `run`"),
            g::SetKind::Memory => usage(
                "`set var x = ...` and `set {type}addr = ...` write into the program: `write <addr> <hex bytes>` (asks); `set $name = expr` keeps a value for later expressions",
            ),
        }
    }

    fn gdb_display(&mut self, fmt: Option<&str>, rest: &[String]) -> Result<Output> {
        if rest.is_empty() {
            let l = self.displays.iter().map(|(i, w)| format!("{i}: {}", w.join(" "))).collect();
            return Ok(rows("display", l, json!({})));
        }
        let simple = fmt.is_some_and(|f| f.len() == 1 && "xduotacfz".contains(f));
        let head = match fmt {
            Some(f) if !simple => format!("x/{f}"),
            Some(f) => format!("p/{f}"),
            None => "print".to_string(),
        };
        let id = self.displays.iter().map(|d| d.0).max().unwrap_or(0) + 1;
        let mut w = vec![head];
        w.push(rest.join(" "));
        self.displays.push((id, w.clone()));
        let mut l = vec![format!("display {id}: {}", rest.join(" "))];
        if self.state == State::Stopped {
            if let Ok(o) = self.exec(&w) {
                l.extend(o.rows.into_iter().map(|r| format!("{id}: {}", r.text)));
            }
        }
        Ok(rows("display", l, json!({})))
    }

    fn gdb_undisplay(&mut self, rest: &[String]) -> Result<Output> {
        if rest.is_empty() {
            let n = self.displays.len();
            self.displays.clear();
            return Ok(rows("undisplay", vec![format!("{n} displays removed")], json!({})));
        }
        for w in rest {
            let id: u32 = w.parse().map_err(|_| crate::Error::Usage(format!("undisplay N: {w:?} is not a number")))?;
            if !self.displays.iter().any(|d| d.0 == id) {
                return fail(format!("no display {id}"));
            }
            self.displays.retain(|d| d.0 != id);
        }
        Ok(rows("undisplay", vec!["removed".into()], json!({})))
    }

    /// The displays as lines for the end of a stop report.
    pub(crate) fn display_lines(&mut self) -> Vec<String> {
        let mut l = Vec::new();
        for (id, w) in self.displays.clone() {
            match self.exec(&w) {
                Ok(o) => l.extend(o.rows.into_iter().map(|r| format!("{id}: {}", r.text))),
                Err(e) => l.push(format!("{id}: {} ({e})", w[1..].join(" "))),
            }
        }
        l
    }

    fn gdb_condition(&mut self, rest: &[String]) -> Result<Output> {
        let Some(n) = rest.first().and_then(|w| w.trim_start_matches('#').parse::<u32>().ok()) else {
            return usage("condition <breakpoint number> [expression]   (no expression removes the condition)");
        };
        let Some(i) = self.bps.iter().position(|b| b.id == n && !b.call) else {
            return fail(format!("no breakpoint #{n}: `breaks` lists them"));
        };
        let cond = rest[1..].join(" ");
        if cond.is_empty() {
            self.bps[i].cond = None;
            return Ok(rows("condition", vec![format!("breakpoint #{n} now stops at every hit")], json!({})));
        }
        self.bps[i].cond = Some(cond.clone());
        Ok(rows("condition", vec![format!("breakpoint #{n} stops when {cond}")], json!({})))
    }

    fn gdb_ignore(&mut self, rest: &[String]) -> Result<Output> {
        let (Some(n), Some(c)) = (
            rest.first().and_then(|w| w.trim_start_matches('#').parse::<u32>().ok()),
            rest.get(1).and_then(|w| w.parse::<u64>().ok()),
        ) else {
            return usage("ignore <breakpoint number> <count>");
        };
        let Some(i) = self.bps.iter().position(|b| b.id == n && !b.call) else {
            return fail(format!("no breakpoint #{n}: `breaks` lists them"));
        };
        self.bps[i].after = self.bps[i].hits + c;
        Ok(rows("ignore", vec![format!("breakpoint #{n} lets the next {c} hits go by")], json!({})))
    }

    /// `info registers|breakpoints|frame|sharedlibrary|proc mappings|threads|locals|args|symbol`.
    fn gdb_info(&mut self, rest: &[String]) -> Result<Output> {
        let sub = rest[0].as_str();
        let more = &rest[1..];
        let go = |s: &mut Session, w: &[&str]| s.exec(&w.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        match sub {
            "registers" | "reg" | "regs" | "r" | "all-registers" => {
                if more.is_empty() {
                    return go(self, &["regs"]);
                }
                let mut l = Vec::new();
                for n in more {
                    let a = Args::new(&[n.trim_start_matches('$').to_string()])?;
                    l.extend(self.cmd_regs(&a)?.rows.into_iter().map(|r| r.text));
                }
                Ok(rows("regs", l, json!({})))
            }
            "breakpoints" | "break" | "b" | "watchpoints" | "display" => {
                if sub == "display" {
                    return self.gdb_display(None, &[]);
                }
                go(self, &["breaks"])
            }
            "frame" | "f" => go(self, &["bt", "1"]),
            "sharedlibrary" | "shared" | "dll" | "sharedlib" => go(self, &["modules"]),
            "proc" if more.first().is_some_and(|w| w.starts_with("map")) => {
                let mut w = vec!["maps".to_string()];
                w.extend(more[1..].iter().cloned());
                self.exec(&w)
            }
            "proc" | "inferiors" | "program" => go(self, &["status"]),
            "threads" => go(self, &["threads"]),
            "locals" => go(self, &["locals"]),
            "args" => go(self, &["args"]),
            "symbol" | "address" => {
                let Some(e) = more.first() else { return usage("info symbol <address|expression>") };
                let a = self.addr_of(&more.join(" "))?;
                let _ = e;
                let l = match self.mods.label(a, true) {
                    Some(l) => format!("{a:#x} is {l}"),
                    None => format!("no symbol at {a:#x}"),
                };
                Ok(rows("info", vec![l], json!({"addr": a})))
            }
            "signals" | "signal" | "handle" => go(self, &["catches"]),
            "line" | "source" | "functions" | "variables" | "types" | "scope" | "files" | "sources" => usage(
                "no symbol tables are listed here: `find <text>` searches memory, `modules` and `maps` list what is loaded, `bt` and `dis` show where the program is",
            ),
            other => usage(format!(
                "info {other}: this debugger answers info registers [REG], breakpoints, frame, sharedlibrary, proc mappings, threads, locals, args, symbol <addr>"
            )),
        }
    }

    /// `print --as string <std::string address>` and `--as vector[:u8|u32|u64|ptr] <std::vector address>`: the
    /// libstdc++ layouts read as a person would write them.
    pub(super) fn std_view(&mut self, kind: &str, addr: u64) -> Result<Output> {
        let word = |s: &mut Session, at: u64| -> Option<u64> {
            let b = s.read_vec(at, 8);
            (b.len() == 8).then(|| u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])))
        };
        if kind == "string" || kind == "std::string" {
            let (Some(ptr), Some(len)) = (word(self, addr), word(self, addr + 8)) else {
                return fail(format!("cannot read a std::string at {addr:#x}"));
            };
            let inline = ptr == addr + 16;
            if len > 1 << 28 || (inline && len > 15) || (!inline && ptr < 0x1000) {
                return fail(format!(
                    "{addr:#x} does not look like a libstdc++ std::string (pointer {ptr:#x}, size {len}): `mem {addr:#x} 32` shows the raw bytes; `print --as string` wants the address of the string object"
                ));
            }
            let b = self.read_vec(ptr, (len as usize).min(VIEW_TEXT));
            let mut text = String::from_utf8_lossy(&b).to_string();
            if (len as usize) > VIEW_TEXT {
                text.push_str("...");
            }
            let line =
                format!("std::string at {addr:#x}: size {len}{} {text:?}", if inline { " (inline)" } else { "" });
            return Ok(rows("print", vec![line], json!({"len": len})));
        }
        let elem = kind
            .strip_prefix("vector")
            .or_else(|| kind.strip_prefix("std::vector"))
            .map(|t| t.trim_start_matches([':', '<']).trim_end_matches('>'));
        let Some(elem) = elem else { return usage("--as string | vector[:u8|u16|u32|u64|ptr]") };
        let size = match elem {
            "" | "u8" | "i8" | "char" | "byte" | "uint8_t" => 1,
            "u16" | "i16" | "short" | "uint16_t" => 2,
            "u32" | "i32" | "int" | "float" | "uint32_t" => 4,
            "u64" | "i64" | "ptr" | "long" | "double" | "uint64_t" | "size_t" => 8,
            other => return usage(format!("vector:{other}: element types are u8 u16 u32 u64 ptr")),
        };
        let (Some(begin), Some(end), Some(cap)) = (word(self, addr), word(self, addr + 8), word(self, addr + 16))
        else {
            return fail(format!("cannot read a std::vector at {addr:#x}"));
        };
        let bytes = end.wrapping_sub(begin);
        if end < begin || bytes > 1 << 30 || cap < end {
            return fail(format!(
                "{addr:#x} does not look like a libstdc++ std::vector (begin {begin:#x}, end {end:#x}, capacity end {cap:#x}): `print --as vector` wants the address of the vector object"
            ));
        }
        let count = (bytes / size as u64) as usize;
        let data = self.read_vec(begin, count.min(VIEW_SHOWN) * size);
        let mut l = vec![format!(
            "std::vector at {addr:#x}: {count} elements of {size} bytes ({bytes} bytes at {begin:#x}, room for {})",
            cap.wrapping_sub(begin) / size as u64
        )];
        for (i, row) in data.chunks(size * 16).enumerate() {
            let cells: Vec<String> = row
                .chunks_exact(size)
                .map(|c| {
                    let mut b = [0u8; 8];
                    b[..size].copy_from_slice(c);
                    g::show_unit('x', size, u64::from_le_bytes(b)).trim_start_matches("0x").to_string()
                })
                .collect();
            l.push(format!("[{}] {}", i * 16, cells.join(" ")));
        }
        if count > VIEW_SHOWN {
            l.push(format!("... {} more elements: `mem {begin:#x} {bytes}`", count - VIEW_SHOWN));
        }
        Ok(rows("print", l, json!({"count": count})))
    }

    /// A value from an expression that reads as text when its type says it points at characters.
    pub(super) fn char_text(&mut self, v: &expr::Val) -> Option<String> {
        (v.ty.ch && v.ty.stars == 1 && v.v != 0).then(|| self.cstr(v.v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::split_words(s)
    }

    #[test]
    fn a_break_place_with_spaces_is_put_together() {
        let m = merge_place(&w("break *(sym + 0x59d) if rax == 1")).unwrap();
        assert_eq!(m, ["break", "*(sym + 0x59d)", "if", "rax", "==", "1"]);
        assert!(merge_place(&w("break main if x")).is_none());
        let m = merge_place(&w("break 'ns::f(int, char)'")).unwrap();
        assert_eq!(m, ["break", "ns::f(int, char)"]);
        let m = merge_place(&w("tbreak *0x10 + 4")).unwrap();
        assert_eq!(m, ["tbreak", "*0x10 + 4"]);
        assert!(merge_place(&w("break")).is_none());
    }

    #[test]
    fn echo_and_text_lines_drop_the_last_newline() {
        assert_eq!(lines_of("a\nb\n"), ["a", "b"]);
        assert_eq!(lines_of("a"), ["a"]);
        assert!(lines_of("").is_empty());
    }
}
