//! Reading values and the stack: print, args, backtrace, disassembly and threads.

use super::*;

impl Session {
    pub(super) fn cmd_print(&mut self, a: &Args) -> Result<Output> {
        if a.pos.is_empty() {
            return usage("print <expression> [--as str|string|vector[:u8|u32|u64|ptr]]");
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
            return self.std_view(kind, v);
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

    pub(super) fn cmd_args(&mut self, _a: &Args) -> Result<Output> {
        let r = self.tid_regs()?;
        let regs = match self.t.os() {
            Os::Linux => ["rdi", "rsi", "rdx", "rcx", "r8", "r9"],
            Os::Windows => ["rcx", "rdx", "r8", "r9", "", ""],
        };
        // the named parameters when the program carries variable info, then the registers they came in
        let mut l = self.named_args();
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
