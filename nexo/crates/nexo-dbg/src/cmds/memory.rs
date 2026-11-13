//! Memory: reading, writing, maps, modules and searching.

use super::*;

impl Session {
    pub(super) fn cmd_mem(&mut self, a: &Args) -> Result<Output> {
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
        if ty == ElemType::Str {
            return Ok(rows("mem", str_rows(&bytes, addr, len), json!({"addr": addr, "len": bytes.len()})));
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
}

/// `mem <addr> <len> --as str`: the bytes as one string, 64 to a row, with where the C string ends.
fn str_rows(bytes: &[u8], addr: u64, len: u64) -> Vec<String> {
    let nul = bytes.iter().position(|&c| c == 0);
    let mut l = vec![match nul {
        Some(n) => format!("C string at {addr:#x}: {n} bytes, NUL at +{n:#x}"),
        None => format!("no NUL in the {} bytes read: the C string at {addr:#x} goes on past them", bytes.len()),
    }];
    // trailing zero bytes are one note, not a row of \0
    let end = bytes.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
    for (i, chunk) in bytes[..end].chunks(64).enumerate() {
        let t: String = chunk
            .iter()
            .map(|&c| match c {
                0 => "\\0".to_string(),
                b'"' => "\\\"".to_string(),
                b'\\' => "\\\\".to_string(),
                b'\n' => "\\n".to_string(),
                0x20..=0x7e => (c as char).to_string(),
                _ => format!("\\x{c:02x}"),
            })
            .collect();
        l.push(format!("{:#x}: \"{t}\"", addr + i as u64 * 64));
    }
    if end < bytes.len() {
        l.push(format!("+{end:#x}..+{:#x}: zero", bytes.len()));
    }
    if (bytes.len() as u64) < len {
        l.push(format!("... only {} of {len} bytes are readable", bytes.len()));
    }
    l
}
