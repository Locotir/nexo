//! Memory: reading, writing, maps, modules and searching.

use super::*;

/// The most memory one `mem`, `dis` or `find` call reads.
pub(super) const READ_LIMIT: u64 = 1 << 20;

impl Session {
    pub(super) fn cmd_mem(&mut self, a: &Args) -> Result<Output> {
        let (Some(at), len) = (a.pos.first(), a.pos.get(1)) else {
            return usage(
                "mem <address|expr|A..B> [length] [--as u8|u16|u32|u64|i32|f32|f64|ptr|str|char[16]|item[4]|struct]",
            );
        };
        self.need_stopped()?;
        // the type first: an unknown one answers before the program's memory is touched
        let ty = a.flag("as").unwrap_or("u8");
        nexo_bin::resolve_type(self.mods.main_image(), ty)?;
        let (addr, len) = match self.span_of(at)? {
            Some((lo, hi)) => (lo, Some(hi - lo)),
            None => (
                self.addr_of(at)?,
                match len {
                    Some(l) => Some(self.addr_of(l)?),
                    None => None,
                },
            ),
        };
        self.read_typed("mem", addr, ty, len)
    }

    /// `len` bytes at `addr` (a struct or `T[N]` is as long as its type, anything else 64 bytes) as the type `ty`.
    pub(super) fn read_typed(&mut self, op: &'static str, addr: u64, ty: &str, len: Option<u64>) -> Result<Output> {
        let view = nexo_bin::resolve_type(self.mods.main_image(), ty)?;
        let len = len.or_else(|| view.fixed_size()).unwrap_or(64);
        if len > READ_LIMIT {
            return usage(format!("{op} reads at most {} KiB at a time; {len} bytes were asked", READ_LIMIT >> 10));
        }
        let bytes = self.read_vec(addr, len as usize);
        if bytes.is_empty() {
            return fail(format!("{addr:#x} cannot be read"));
        }
        let mods = std::cell::RefCell::new(&mut self.mods);
        let mut r = view.rows(&bytes, addr, &|x| mods.borrow_mut().label(x, true));
        if (bytes.len() as u64) < len {
            r.push(nexo_bin::Row {
                key: addr + bytes.len() as u64,
                text: format!("... only {} of {len} bytes are readable", bytes.len()),
                json: None,
            });
        }
        Ok(Output {
            op,
            meta: json!({"addr": addr, "len": bytes.len(), "as": view.name()}),
            rows: r,
            tail: 0,
            command: String::new(),
        })
    }

    pub(super) fn cmd_write(&mut self, a: &Args) -> Result<Output> {
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

    pub(super) fn cmd_maps(&mut self, a: &Args) -> Result<Output> {
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

    pub(super) fn cmd_modules(&mut self) -> Result<Output> {
        self.mods.refresh(&mut *self.t);
        let l = self.mods.list.iter().map(|m| format!("{:#014x}-{:#014x} {}", m.base, m.end, m.path)).collect();
        Ok(rows("modules", l, json!({})))
    }

    pub(super) fn cmd_find(&mut self, a: &Args) -> Result<Output> {
        self.need_stopped()?;
        let Some(first) = a.pos.first() else {
            return usage("find <hex bytes with ?? | \"text\"> [--in <map name | range>]");
        };
        let pat: Vec<Option<u8>> = if first.starts_with('"') {
            a.pos.join(" ").trim_matches('"').bytes().map(Some).collect()
        } else {
            parse_pattern(&a.pos.join(" "))?
        };
        if pat.is_empty() {
            return usage("an empty pattern matches everywhere");
        }
        let maps = self.t.maps()?;
        // `--in` names a map (part of its path) or, when none has that in its path, a range in any form
        let only = a.flag("in");
        let (only, span) = match only {
            Some(o) if !maps.iter().any(|m| m.path.contains(o)) => (None, Some(self.range_of(o)?)),
            other => (other, None),
        };
        let (mut l, mut scanned) = (Vec::new(), 0u64);
        'maps: for m in maps {
            if !m.read || only.is_some_and(|o| !m.path.contains(o)) || m.path == "[vvar]" || m.path == "[vsyscall]" {
                continue;
            }
            let (mut pos, end) = match span {
                Some((lo, hi)) => (m.start.max(lo), m.end.min(hi)),
                None => (m.start, m.end),
            };
            while pos < end {
                let n = ((end - pos) as usize).min((1 << 20) + pat.len());
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
}
