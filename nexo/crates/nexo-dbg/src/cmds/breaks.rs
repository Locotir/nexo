//! Breakpoints: set, list, delete, enable and disable.

use super::*;

impl Session {
    pub(super) fn cmd_break(&mut self, once: bool, words: &[String], a: &Args) -> Result<Output> {
        let Some(spec) = a.pos.first() else {
            return usage(
                "break <address|*address|name|file:line|name+off|import> [if <expr>] [after N] [log <text>] [once]",
            );
        };
        if let Some(w) = words.iter().find(|w| w.ends_with(';')) {
            return usage(format!(
                "unexpected {w:?}: several commands go in one `do \"a; b\"` (or one debug call `a; b`)"
            ));
        }
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
        // `*place` is that exact address: no move past a function's prologue
        let exact = spec.starts_with('*');
        let spec = &spec.trim_start_matches('*').to_string();
        let by_line = self.source_place(spec, !exact);
        let from_line = by_line.is_some();
        // a name no loaded file has yet: the breakpoint waits for a library that has it
        let mut pending: Option<String> = None;
        let addr = match by_line {
            Some(r) => r?,
            None => match self.resolve_place(spec) {
                Ok(Place::Code(x)) => x,
                Ok(Place::ImportSlot(slot)) => {
                    // an import: stop in the library code when it is loaded; the library knows it by the import's
                    // name, not by `puts@got` or `__imp_puts`
                    let name = self.mods.import_of_slot(slot).unwrap_or_else(|| spec.trim_start_matches('!').into());
                    let of = if name == *spec { String::new() } else { format!(" ({spec} is its slot, {slot:#x})") };
                    match self.resolve_place(&format!("!{name}")) {
                        Ok(Place::Code(x)) => x,
                        _ if crate::libs::may_load_later(&name) => {
                            pending = Some(name);
                            0
                        }
                        _ => {
                            return fail(format!(
                                "{name} is an import{of} and its library is not loaded yet: run to the entry first (`cont`), or break at <library>!{name}"
                            ))
                        }
                    }
                }
                Ok(Place::Data(x)) => {
                    if self.mods.locate(x).is_some() && self.maps_exec(x) {
                        x
                    } else {
                        return fail(format!(
                            "{spec} ({x:#x}) is data, not code: use `watch` for a place that is written"
                        ));
                    }
                }
                // `addr_of` fails with this same lookup's error, or with the expression's own when a register the
                // selected frame has no value for is the reason (`break *$rax` after `up`)
                Err(e) => match self.addr_of(spec) {
                    Ok(x) if self.maps_exec(x) => x,
                    Err(_) if !exact && crate::libs::may_load_later(spec) => {
                        pending = Some(spec.clone());
                        0
                    }
                    Err(why) => return Err(why),
                    Ok(_) => return Err(e),
                },
            },
        };
        if let Some(name) = pending {
            let id = self.new_id();
            let loaded = self.loaded_names();
            self.bps.push(Bp {
                id,
                addr: 0,
                enabled: true,
                cond,
                after,
                hits: 0,
                log,
                once,
                call: false,
                actions: Vec::new(),
                name: Some(name.clone()),
            });
            let l = vec![format!(
                "breakpoint #{id} pending on {name}: no loaded file has it (loaded: {loaded}); it is set when a library that has it loads"
            )];
            return Ok(rows("break", l, json!({"id": id, "addr": 0, "pending": name})));
        }
        if let Some(c) = &cond {
            if self.state == State::Stopped {
                // a hit works the condition out with the hitting thread's own registers (frame #0), so it is
                // checked with the stopped thread's, whatever frame `up` or `frame N` selected
                let tid = self.cur;
                if let Err(e) = self.ev(tid).and_then(|mut ev| crate::expr::value(c, &mut ev)) {
                    if !matches!(&e, crate::Error::Fail(m) if m.contains("memory")) {
                        return fail(format!("the condition {c:?} does not work: {e}"));
                    }
                }
            }
        }
        let note = if from_line { None } else { self.boundary_check(addr)? };
        let id = self.new_id();
        let mut label = self.mods.label(addr, false).unwrap_or_default();
        if let Some(src) = self.mods.source(addr).filter(|s| !label.contains(s.as_str())) {
            label = format!("{label} ({src})");
        }
        // set by name in a library: found again by that name if the library is unloaded and loaded again
        let name = (!from_line && crate::libs::may_load_later(spec) && self.mods.in_main(addr).is_none())
            .then(|| spec.clone());
        self.bps.push(Bp {
            id,
            addr,
            enabled: true,
            cond,
            after,
            hits: 0,
            log,
            once,
            call: false,
            actions: Vec::new(),
            name,
        });
        let mut l = vec![format!("breakpoint #{id} at {addr:#x} {label}")];
        l.extend(note);
        Ok(rows("break", l, json!({"id": id, "addr": addr})))
    }

    /// Whether `addr` starts an instruction, found by decoding from the start of the function that holds it. `Err`
    /// when it is inside one (an int3 there would break the instruction); `Ok(Some(note))` when that could not be
    /// checked, so the model is told instead of assuming.
    fn boundary_check(&mut self, addr: u64) -> Result<Option<String>> {
        let Some(start) = self.mods.symbol_start(addr).filter(|s| *s <= addr) else {
            return Ok(Some(format!(
                "note: no function start is known for {addr:#x}, so whether it starts an instruction was not checked"
            )));
        };
        if start == addr {
            return Ok(None);
        }
        let want = (addr - start + 16).min(1 << 16) as usize;
        let mut bytes = self.read_vec(start, want);
        // our own int3 bytes are not the program's code
        for (i, byte) in bytes.iter_mut().enumerate() {
            if let Some(b) = self.inserted_byte(start + i as u64) {
                *byte = b;
            }
        }
        let regs = self.tid_regs().ok();
        let mut pos = 0usize;
        while start + (pos as u64) <= addr && pos < bytes.len() {
            let ip = start + pos as u64;
            let Some(i) =
                nexo_bin::decode_live_in(self.bits(), &bytes[pos..], ip, &|x| regs.as_ref().and_then(|r| r.get(x)))
            else {
                break;
            };
            if i.invalid {
                break;
            }
            if ip == addr {
                return Ok(None);
            }
            if ip + i.len as u64 > addr {
                let name = |s: &mut Self, a: u64| s.mods.label(a, false).unwrap_or_else(|| format!("{a:#x}"));
                let (here, next) = (name(self, ip), name(self, ip + i.len as u64));
                return fail(format!(
                    "{addr:#x} is inside the instruction at {here} ({}, {} bytes); instruction starts nearby: {here}, {next}",
                    i.text, i.len
                ));
            }
            pos += i.len;
        }
        Ok(Some(format!(
            "note: could not decode the code from {} to {addr:#x}, so whether it starts an instruction was not checked",
            self.mods.label(start, false).unwrap_or_else(|| format!("{start:#x}"))
        )))
    }

    fn maps_exec(&mut self, addr: u64) -> bool {
        self.t.maps().map(|m| m.iter().any(|m| m.exec && m.start <= addr && addr < m.end)).unwrap_or(false)
    }

    /// When the program ends: each breakpoint it never reached, with where it really was put. The program's own
    /// numbers are file addresses (what `nexo bin` prints) and are moved by the load shift of a PIE; the line says
    /// both, so a number typed from a listing that was never shifted shows.
    pub(super) fn unhit_lines(&mut self) -> Vec<String> {
        let shift = self.mods.main_bias();
        let todo: Vec<(u32, u64, bool, bool)> = self
            .bps
            .iter()
            .filter(|b| !b.call && b.hits == 0)
            .map(|b| (b.id, b.addr, b.enabled, b.cond.is_some()))
            .collect();
        let mut l = Vec::new();
        for (id, addr, en, cond) in todo {
            if addr == 0 {
                let name = self.bps.iter().find(|b| b.id == id).and_then(|b| b.name.clone()).unwrap_or_default();
                l.push(format!("breakpoint #{id} pending on {name} was never set: no library that has {name} loaded"));
                continue;
            }
            let label = self.mods.label(addr, false).unwrap_or_default();
            let mut s = format!("breakpoint #{id} at {addr:#x} {label} was never hit (hits 0)");
            if !en {
                s.push_str("; it was disabled");
            }
            if cond {
                s.push_str("; it has an `if` condition (hits count only the stops)");
            }
            match self.mods.in_main(addr) {
                Some(f) if shift != 0 => s.push_str(&format!(
                    "; in the program's image: file address {f:#x}, loaded at +{shift:#x} (a plain number you type is a file address and gets this shift added)"
                )),
                Some(_) => {}
                None => s.push_str("; this address is outside the program's own image"),
            }
            l.push(s);
        }
        if !l.is_empty() {
            l.extend(self.fork_lines());
        }
        l
    }

    /// The children the program forked, for an answer about breakpoints that were never hit: code that ran in a
    /// child let go ran without them, and a kept child counts its own hits.
    fn fork_lines(&self) -> Vec<String> {
        const SHOWN: usize = 4;
        let mut l = Vec::new();
        let gone: Vec<&crate::engine::ForkNote> = self.forks.iter().filter(|f| f.session.is_none()).collect();
        if !gone.is_empty() {
            let mut each: Vec<String> = gone
                .iter()
                .take(SHOWN)
                .map(|f| format!("pid {} ({} at {})", f.pid, if f.vfork { "vfork" } else { "fork" }, f.place))
                .collect();
            if gone.len() > SHOWN {
                each.push(format!("{} more", gone.len() - SHOWN));
            }
            let n = gone.len();
            l.push(format!(
                "the program forked {n} {} that ran without the debugger: {}; under `follow both` (or `follow child`) a forked child is kept as a session of its own with these breakpoints",
                if n == 1 { "child" } else { "children" },
                each.join(", ")
            ));
        }
        for f in self.forks.iter().filter(|f| f.session.is_some()).take(SHOWN) {
            let id = f.session.as_deref().unwrap_or("");
            l.push(format!(
                "pid {} (forked at {}) is session {id}: its hits count there (`-s {id} breaks`)",
                f.pid, f.place
            ));
        }
        l
    }

    pub(super) fn cmd_breaks(&mut self) -> Output {
        let mut l = Vec::new();
        #[allow(clippy::type_complexity)]
        let bps: Vec<(u32, u64, bool, Option<String>, u64, u64, Option<String>, bool, usize)> = self
            .bps
            .iter()
            .filter(|b| !b.call)
            .map(|b| (b.id, b.addr, b.enabled, b.cond.clone(), b.after, b.hits, b.log.clone(), b.once, b.actions.len()))
            .collect();
        let sites = self.bps.iter().filter(|b| b.call).count();
        if sites > 0 {
            let hits: u64 = self.bps.iter().filter(|b| b.call).map(|b| b.hits).sum();
            l.push(format!("indirect-call logging on {sites} sites, {hits} calls seen (`calls off`)"));
        }
        for (id, addr, en, cond, after, hits, log, once, acts) in bps {
            let label = match addr {
                0 => {
                    let name = self.bps.iter().find(|b| b.id == id).and_then(|b| b.name.clone()).unwrap_or_default();
                    format!("{name} (pending: no loaded file has it)")
                }
                _ => self.mods.label(addr, false).unwrap_or_default(),
            };
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
            if acts > 0 {
                s.push_str(&format!(" commands {acts}"));
            }
            l.push(s);
        }
        for w in &self.wps {
            let mut s = format!(
                "#{} watch {:#x} len {} {} ({}) hits {}",
                w.id,
                w.addr,
                w.len,
                match w.access {
                    Access::Exec => "x",
                    Access::Write => "w",
                    Access::ReadWrite => "r/w",
                },
                w.how.word(),
                w.hits
            );
            if let Some(c) = &w.cond {
                s.push_str(&format!(" if {c}"));
            }
            if w.after > 0 {
                s.push_str(&format!(" after {}", w.after));
            }
            if let Some(t) = &w.log {
                s.push_str(&format!(" log {t:?}"));
            }
            l.push(s);
        }
        if l.is_empty() {
            l.push("no breakpoints or watches".into());
        }
        rows("breaks", l, json!({}))
    }

    pub(super) fn cmd_delete(&mut self, a: &Args) -> Result<Output> {
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

    pub(super) fn cmd_enable(&mut self, on: bool, a: &Args) -> Result<Output> {
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
}
