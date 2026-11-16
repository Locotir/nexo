//! Reading values and the stack: print, args, backtrace, disassembly and threads.

use super::*;

impl Session {
    pub(super) fn cmd_print(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage("print <expression> [--as str|string|vector[:u8|u32|u64|ptr]|<type>]");
        }
        self.need_stopped()?;
        let src = a.pos.join(" ");
        let view = a.flag("as");
        if view.is_none() {
            if let Some(r) = self.print_var(&src) {
                return r;
            }
        }
        let val = self.eval_any(&src)?;
        let v = val.v;
        // `--as string` and `--as vector` read the object at that address as libstdc++ lays it out
        if let Some(kind) = view.filter(|k| *k != "str") {
            if kind.starts_with("string") || kind.starts_with("std::") || kind.starts_with("vector") {
                return self.std_view(kind, v);
            }
            // any other `--as` is a type (see `mem`): the value is the address of it
            return self.read_typed("print", v, kind, None);
        }
        let mut s = format!("{src} = {v:#x} ({})", v as i64);
        if let Some(l) = self.mods.label(v, true) {
            s.push_str(&format!(" -> {l}"));
        }
        // a `char *` reads as its text, as gdb shows it
        if view == Some("str") {
            s.push_str(&format!(" {}", self.cstr(v)));
        } else if let Some(t) = self.char_text(&val) {
            s.push_str(&format!(" {t}"));
        }
        Ok(rows("print", vec![s], json!({"value": v})))
    }

    /// `ptype <type|variable>`: the type, its size and fields, or the type of a variable of the stopped program.
    pub(super) fn cmd_ptype(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage(
                "ptype <type|variable>   (u32, char[16], item[4], struct node, a DWARF or PDB type, or a variable)",
            );
        }
        let src = a.pos.join(" ");
        let said = match nexo_bin::resolve_type(self.mods.main_image(), &src) {
            Ok(v) => return Ok(lines("ptype", v.describe(self.mods.main_image()), v.name())),
            Err(e) => e.to_string(),
        };
        // not a type: maybe a variable of the stopped program, whose type is then described
        if let Some(t) = self.var_type_name(&src) {
            return Ok(match nexo_bin::resolve_type(self.mods.main_image(), &t) {
                Ok(v) => {
                    let mut r = vec![nexo_bin::Row { key: 0, text: format!("{src}: {t}"), json: None }];
                    r.extend(v.describe(self.mods.main_image()));
                    lines("ptype", r, v.name())
                }
                Err(_) => rows("ptype", vec![format!("{src}: {t}")], json!({"type": t})),
            });
        }
        fail(said)
    }

    pub(super) fn cmd_args(&mut self, _a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        let regs = match self.t.os() {
            Os::Linux | Os::MacOS => ["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
            Os::Windows => ["rcx", "rdx", "r8", "r9", "", ""],
        };
        // the named parameters when the program carries variable info, then the registers they came in:
        // with it, only the registers of parameters that could not be read and, past the named ones, those of
        // a variadic function; without it, all of them
        let info = self.named_args();
        let (mut l, count, variadic, unread) = match info {
            Some(i) => (i.lines, Some(i.count), i.variadic, i.unread),
            None => (Vec::new(), None, false, Vec::new()),
        };
        for (i, n) in regs.iter().enumerate().filter(|(_, n)| !n.is_empty()) {
            let past = count.is_some_and(|c| i >= c);
            if past && !variadic || count.is_some() && !past && !unread.contains(&i) {
                continue;
            }
            let v = r.get(n).unwrap_or(0);
            let mut s = format!("arg{} {n} = {v:#x} ({})", i + 1, v as i64);
            if let Some(lab) = self.mods.label(v, true) {
                s.push_str(&format!(" -> {lab}"));
            }
            if past {
                s.push_str(" ... (variadic)");
            }
            l.push(s);
        }
        if l.is_empty() {
            l.push("no parameters".into());
        }
        Ok(rows("args", l, json!({})))
    }

    pub(super) fn cmd_bt(&mut self, a: &Args) -> Result<Output> {
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

    pub(super) fn cmd_dis(&mut self, a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        // `dis A..B` shows the instructions that start before B
        let (at, end) = match a.pos.first() {
            Some(t) => match self.span_of(t)? {
                Some((lo, hi)) => (lo, Some(hi)),
                None => (self.addr_of(t)?, None),
            },
            None => (r.rip, None),
        };
        // a range is every instruction that starts in it (an instruction is at least 1 byte); a count is that many
        let n = match (a.pos.get(1), end) {
            (Some(x), _) => parse_addr(x)?.max(1),
            (None, Some(hi)) => (hi - at).max(1),
            (None, None) => 8,
        };
        let bytes_wanted = n.saturating_mul(15).min(end.map_or(u64::MAX, |hi| hi - at + 15));
        if bytes_wanted > memory::READ_LIMIT + 15 {
            return usage(format!(
                "dis reads at most {} KiB of code at a time; {n} instructions or a range of {} bytes was asked: ask in parts",
                memory::READ_LIMIT >> 10,
                end.map_or(n, |hi| hi - at)
            ));
        }
        let n = n as usize;
        let bytes = self.read_vec(at, bytes_wanted as usize);
        let (mut pos, mut l) = (0usize, Vec::new());
        for _ in 0..n {
            let ip = at + pos as u64;
            if end.is_some_and(|hi| ip >= hi) {
                break;
            }
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

    pub(super) fn cmd_threads(&mut self) -> Result<Output> {
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

    pub(super) fn cmd_thread(&mut self, a: &Args) -> Result<Output> {
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
}

/// Described rows as an answer, numbered in order.
fn lines(op: &'static str, r: Vec<nexo_bin::Row>, ty: String) -> Output {
    let r = r.into_iter().enumerate().map(|(i, x)| nexo_bin::Row { key: i as u64, ..x }).collect();
    Output { op, meta: json!({"type": ty}), rows: r, tail: 0, command: String::new() }
}
