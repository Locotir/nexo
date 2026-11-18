//! Reading values and the stack: print, args, backtrace, disassembly and threads.

use super::*;

/// Threads `threads` lists before saying how many more there are, and the most frames `threads --bt N` shows for one.
const THREADS_SHOWN: usize = 64;
const THREADS_BT_MAX: usize = 50;

impl Session {
    pub(super) fn cmd_print(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage("print <expression> [--as raw|str|string|vector[:u8|u32|u64|ptr]|<type>]");
        }
        self.need_stopped()?;
        let src = a.pos.join(" ");
        // `--as raw` prints a variable as its raw layout, without the container views
        let raw = a.flag("as") == Some("raw");
        let view = a.flag("as").filter(|k| *k != "raw");
        let calls = crate::expr::has_call(&src);
        if view.is_none() && !calls {
            if let Some(r) = self.print_var(&src, raw) {
                return r;
            }
        }
        let tid = self.cur;
        let val = if calls {
            match self.eval_calling(&src, a.wait()?)? {
                Ok(v) => v,
                // a call that did not come back with a value: what happened instead
                Err(told) => return Ok(told),
            }
        } else {
            self.eval_any(&src)?
        };
        let v = val.v;
        if calls && view.is_none() {
            let mut l = vec![format!("{src} = {}", self.value_text(v)), self.back_line(tid)];
            l.extend(self.new_output_line());
            return Ok(rows("print", l, json!({"value": v, "thread": tid})));
        }
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

    /// `call f(1, "abc")`: run a function of the stopped program and give its value; `print` does the same when its
    /// expression calls one.
    pub(super) fn cmd_call(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage("call <function>(<args>)   (numbers, expressions and \"text\" as arguments)");
        }
        let src = a.pos.join(" ");
        if !crate::expr::has_call(&src) {
            return usage(format!(
                "`{src}` calls nothing: `call {src}(...)` runs it with those arguments, `print {src}` gives its value"
            ));
        }
        let mut o = self.cmd_print(a)?;
        if o.op == "print" {
            o.op = "call";
        }
        Ok(o)
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
        self.need_stopped()?;
        // a 32-bit program's caller pushed the arguments: they start at the frame's CFA, 4 bytes each
        let x32 = self.bits() == 32;
        let (r, cfa) = if x32 { self.sel_frame()? } else { (self.sel_regs()?, None) };
        let regs = match self.t.os() {
            _ if x32 => ["", "", "", "", "", ""],
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
        // without variable info: the first words the caller pushed, read as 4-byte values
        if x32 && count.is_none() {
            match cfa {
                Some(c) => {
                    for i in 0..6u64 {
                        let at = c + 4 * i;
                        let Some(v) = self.read_ptr(at) else { break };
                        let mut s = format!("arg{} [{at:#x}] = {v:#x} ({})", i + 1, v as u32 as i32);
                        if let Some(lab) = self.mods.label(v, true) {
                            s.push_str(&format!(" -> {lab}"));
                        }
                        l.push(s);
                    }
                    l.push("(the stack words past the return address: how many are arguments is not known without variable info)".into());
                }
                None => l.push("no unwind rule gives this frame's CFA, where the pushed arguments start".into()),
            }
        }
        for (i, n) in regs.iter().enumerate().filter(|(_, n)| !n.is_empty()) {
            let past = count.is_some_and(|c| i >= c);
            if past && !variadic || count.is_some() && !past && !unread.contains(&i) {
                continue;
            }
            let Some(v) = r.get(n) else {
                l.push(format!("arg{} {n} = not saved in this frame", i + 1));
                continue;
            };
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
        let mut l = self.frame_lines(&f);
        if self.frame > 0 {
            l.push(format!("frame #{} is selected (`frame 0` selects where the thread stopped)", self.frame));
        }
        Ok(rows("bt", l, json!({"frames": f.len(), "selected": self.frame})))
    }

    pub fn frame_lines(&mut self, f: &[crate::modules::Frame]) -> Vec<String> {
        // the frames past the return address of a call made by the debugger are the stopped code's, unwound from
        // registers the call changed: the chain ends there with what the thread was doing before the call
        let cut = self.call_cut(f);
        let shown = cut.as_ref().map_or(f.len(), |(i, _)| *i);
        let mut l: Vec<String> = f[..shown].iter().enumerate().map(|(i, fr)| self.frame_line(i, fr)).collect();
        l.extend(cut.map(|(_, line)| line));
        l
    }

    pub(super) fn cmd_dis(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        // in a caller's frame `=>` marks where it goes on after the call
        let r = self.sel_regs()?;
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
            let Some(i) = decode_live_in(self.bits(), &bytes[pos.min(bytes.len())..], ip, &|x| r.get(x)) else { break };
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

    /// `threads [--bt N]`: every thread with its pc and symbol; on a live Linux process also its name, its state and
    /// the system call it is blocked in. `--bt N` adds each thread's backtrace, at most N frames each.
    pub(super) fn cmd_threads(&mut self, a: &Args) -> Result<Output> {
        let bt = match a.flag("bt") {
            None => None,
            Some(v) => {
                let n = parse_addr(v)
                    .map_err(|_| crate::Error::Usage(format!("--bt {v:?} is not a number: threads --bt N")))?;
                Some(n.clamp(1, THREADS_BT_MAX as u64) as usize)
            }
        };
        let all = self.t.threads();
        let mut l = Vec::new();
        if self.state == State::Stopped {
            let (live, pid) = (self.t.local_proc(), self.t.pid());
            for &tid in all.iter().take(THREADS_SHOWN) {
                let pc = self.t.regs(tid).map(|r| r.rip).unwrap_or(0);
                let name = self.mods.label(pc, false).unwrap_or_default();
                let mut row = format!("{} thread {tid} at {pc:#x} {name}", if tid == self.cur { "*" } else { " " });
                if live {
                    let tail = crate::procinfo::row_tail(&crate::procinfo::read_task(pid, tid), &crate::syscalls::name);
                    if !tail.is_empty() {
                        row.push_str("  | ");
                        row.push_str(&tail);
                    }
                }
                l.push(row);
                if let Some(n) = bt {
                    match self.frames_of(tid, n + 1) {
                        Ok(f) => {
                            let more = f.len() > n;
                            let shown = &f[..f.len().min(n)];
                            l.extend(self.frame_lines(shown).into_iter().map(|s| format!("      {s}")));
                            if more {
                                l.push("      ... more frames (`bt 50` for the selected thread)".into());
                            }
                        }
                        Err(e) => l.push(format!("      no backtrace: {e}")),
                    }
                }
            }
            if all.len() > THREADS_SHOWN {
                l.push(format!("... {} more threads not shown", all.len() - THREADS_SHOWN));
            }
            if !live {
                l.push("(thread name, state and system call: live Linux processes only)".into());
            }
        } else {
            l.push(format!("{} thread(s); `stop` first to see where they are", all.len()));
        }
        Ok(rows("threads", l, json!({"threads": all.len()})))
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
        self.frame = 0;
        Ok(rows("thread", vec![format!("current thread {tid}")], json!({})))
    }
}

/// Described rows as an answer, numbered in order.
fn lines(op: &'static str, r: Vec<nexo_bin::Row>, ty: String) -> Output {
    let r = r.into_iter().enumerate().map(|(i, x)| nexo_bin::Row { key: i as u64, ..x }).collect();
    Output { op, meta: json!({"type": ty}), rows: r, tail: 0, command: String::new() }
}
