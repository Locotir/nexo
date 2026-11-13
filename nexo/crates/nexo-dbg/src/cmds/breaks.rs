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
        let addr = match self.source_place(spec, !exact) {
            Some(r) => r?,
            None => match self.mods.resolve(spec) {
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
                        return fail(format!(
                            "{spec} ({x:#x}) is data, not code: use `watch` for a place that is written"
                        ));
                    }
                }
                Err(e) => match self.addr_of(spec) {
                    Ok(x) if self.maps_exec(x) => x,
                    _ => return Err(e),
                },
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
        let mut label = self.mods.label(addr, false).unwrap_or_default();
        if let Some(src) = self.mods.source(addr).filter(|s| !label.contains(s.as_str())) {
            label = format!("{label} ({src})");
        }
        self.bps.push(Bp { id, addr, enabled: true, cond, after, hits: 0, log, once, call: false });
        Ok(rows("break", vec![format!("breakpoint #{id} at {addr:#x} {label}")], json!({"id": id, "addr": addr})))
    }

    fn maps_exec(&mut self, addr: u64) -> bool {
        self.t.maps().map(|m| m.iter().any(|m| m.exec && m.start <= addr && addr < m.end)).unwrap_or(false)
    }

    pub(super) fn cmd_breaks(&mut self) -> Output {
        let mut l = Vec::new();
        #[allow(clippy::type_complexity)]
        let bps: Vec<(u32, u64, bool, Option<String>, u64, u64, Option<String>, bool)> = self
            .bps
            .iter()
            .filter(|b| !b.call)
            .map(|b| (b.id, b.addr, b.enabled, b.cond.clone(), b.after, b.hits, b.log.clone(), b.once))
            .collect();
        let sites = self.bps.iter().filter(|b| b.call).count();
        if sites > 0 {
            let hits: u64 = self.bps.iter().filter(|b| b.call).map(|b| b.hits).sum();
            l.push(format!("indirect-call logging on {sites} sites, {hits} calls seen (`calls off`)"));
        }
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
