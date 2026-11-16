//! `dump`: memory of the stopped program written to a file, so code that exists only at run time (unpacked,
//! decrypted, JIT output, shellcode) and the data it works on can be read with `bin` like any file.

use std::path::PathBuf;

use super::*;
use crate::rebuild::{self, Kind};

/// The most one `dump` writes.
const DUMP_LIMIT: u64 = 256 << 20;

const DUMP_USAGE: &str = "dump <A..B|A+LEN|function|section> [path] | dump <addr> <len> [path] | dump module <name> [path] | dump [binary] memory <path> <start> <end>";

/// What a `dump` asks for, from its words after `dump`.
enum Ask<'a> {
    /// A range in any form `range_of` takes, or a start and a length.
    Range(&'a str, Option<&'a str>),
    Module(&'a str),
}

/// The parts of `dump`'s words (those after `dump`) and the index among them of the path, when one is given.
fn parse(w: &[String]) -> Option<(Ask<'_>, Option<usize>)> {
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    let is_num = |s: &str| parse_addr(s).is_ok();
    // gdb: `dump [binary] memory FILE START END`
    let skip = usize::from(w.first() == Some(&"binary"));
    match w.get(skip..)? {
        ["memory", ..] => {
            let path = Some(skip + 1).filter(|i| *i < w.len());
            match (w.get(skip + 2), w.get(skip + 3)) {
                (Some(_), Some(_)) => Some((Ask::Range("", None), path)),
                _ => None,
            }
        }
        ["module"] => None,
        ["module", name] => Some((Ask::Module(name), None)),
        ["module", name, _] => Some((Ask::Module(name), Some(2))),
        [at] => Some((Ask::Range(at, None), None)),
        [at, len] if is_num(len) => Some((Ask::Range(at, Some(len)), None)),
        [at, _] => Some((Ask::Range(at, None), Some(1))),
        [at, len, _] if is_num(len) => Some((Ask::Range(at, Some(len)), Some(2))),
        _ => None,
    }
}

/// The index in `words` (a command, `dump` first) of the file a `dump` writes, so a caller can make a relative path
/// absolute against the folder it means before the words reach the session.
pub fn dump_path_at(words: &[String]) -> Option<usize> {
    if words.first().map(String::as_str) != Some("dump") {
        return None;
    }
    parse(&words[1..]).and_then(|(_, p)| p).map(|i| i + 1)
}

/// The machine the program's code is for, in the words `bin --arch` takes.
fn arch_of(img: &nexo_bin::Image) -> &'static str {
    match (img.arm64, img.bits) {
        (true, _) => "arm64",
        (false, 32) => "x86-32",
        _ => "x86-64",
    }
}

impl Session {
    pub(super) fn cmd_dump(&mut self, a: &Args) -> Result<Output> {
        let Some((ask, path_at)) = parse(&a.pos) else { return usage(DUMP_USAGE) };
        self.need_stopped()?;
        let path = path_at.map(|i| a.pos[i].as_str());
        match ask {
            Ask::Module(name) => self.dump_module(name, path),
            Ask::Range(at, len) => {
                let (lo, hi) = if at.is_empty() {
                    // gdb's form: the path comes first, then the two ends
                    let skip = usize::from(a.pos[0] == "binary");
                    (self.addr_of(&a.pos[skip + 2])?, self.addr_of(&a.pos[skip + 3])?)
                } else {
                    match len {
                        Some(n) => {
                            let lo = self.addr_of(at)?;
                            (lo, lo.saturating_add(self.addr_of(n)?))
                        }
                        None => self.range_of(at)?,
                    }
                };
                self.dump_range(lo, hi, path)
            }
        }
    }

    /// Where the file goes: the path given (relative to the debugger's folder), or one named after the program and
    /// the address in the debugger's own folder.
    fn dump_target(&self, path: Option<&str>, name: String) -> Result<PathBuf> {
        let p = match path {
            Some(p) => {
                let p = PathBuf::from(p);
                if p.is_relative() {
                    std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
                } else {
                    p
                }
            }
            None => {
                let dir = crate::server::state_dir().join("dumps");
                std::fs::create_dir_all(&dir).map_err(|e| crate::Error::Fail(format!("{}: {e}", dir.display())))?;
                dir.join(name)
            }
        };
        if p.is_dir() {
            return usage(format!("{} is a folder: dump takes the path of the file to write", p.display()));
        }
        Ok(p)
    }

    /// The program's memory from `lo` for `len` bytes, with zeros where it cannot be read, and those places.
    fn read_whole(&mut self, lo: u64, len: usize) -> (Vec<u8>, Vec<(u64, u64)>) {
        let hi = lo.saturating_add(len as u64);
        let mut buf = vec![0u8; len];
        let mut holes: Vec<(u64, u64)> = Vec::new();
        let mut hole = |a: u64, b: u64| match holes.last_mut() {
            Some(h) if h.1 == a => h.1 = b,
            _ => holes.push((a, b)),
        };
        // the map says which pages can be read, so a large reserved area is not tried page by page
        let maps = self.t.maps().unwrap_or_default();
        let mut parts: Vec<(u64, u64)> = maps
            .iter()
            .filter(|m| m.read && m.end > lo && m.start < hi)
            .map(|m| (m.start.max(lo), m.end.min(hi)))
            .collect();
        if maps.is_empty() {
            parts.push((lo, hi));
        }
        parts.sort_unstable();
        let mut at = lo;
        for (s, e) in parts {
            if s > at {
                hole(at, s);
            }
            let mut a = s.max(at);
            while a < e {
                let i = (a - lo) as usize;
                let want = ((e - a) as usize).min(1 << 20);
                let n = self.t.read(a, &mut buf[i..i + want]);
                a += n as u64;
                if n < want {
                    // an unreadable page: zeros up to its end, then the next page is tried
                    let edge = ((a | 0xfff) + 1).min(e);
                    buf[(a - lo) as usize..(edge - lo) as usize].fill(0);
                    hole(a, edge);
                    a = edge;
                }
            }
            at = at.max(e);
        }
        if at < hi {
            hole(at, hi);
        }
        (buf, holes)
    }

    /// Write `bytes` to `path` and say so: the size, where from, whether a file was replaced.
    fn write_dump(&self, path: &std::path::Path, bytes: &[u8], what: String) -> Result<String> {
        let existed = path.exists();
        std::fs::write(path, bytes).map_err(|e| crate::Error::Fail(format!("writing {}: {e}", path.display())))?;
        let replaced = if existed { " (it replaced the file that was there)" } else { "" };
        Ok(format!("wrote {} bytes ({what}) to {}{replaced}", bytes.len(), path.display()))
    }

    fn hole_rows(holes: &[(u64, u64)]) -> Option<String> {
        let n: u64 = holes.iter().map(|(a, b)| b - a).sum();
        let (Some(first), true) = (holes.first(), n > 0) else { return None };
        let more = if holes.len() > 1 { format!(" and {} more place(s)", holes.len() - 1) } else { String::new() };
        Some(format!("{n} bytes could not be read and are zeros in the file: {:#x}..{:#x}{more}", first.0, first.1))
    }

    fn dump_range(&mut self, lo: u64, hi: u64, path: Option<&str>) -> Result<Output> {
        if hi <= lo {
            return usage(format!("the range {lo:#x}..{hi:#x} is empty: {DUMP_USAGE}"));
        }
        if hi - lo > DUMP_LIMIT {
            return usage(format!(
                "dump writes at most {} MiB at a time; {lo:#x}..{hi:#x} is {} bytes",
                DUMP_LIMIT >> 20,
                hi - lo
            ));
        }
        let prog = self.mods.exe_path().file_name().map_or("program".into(), |n| n.to_string_lossy().into_owned());
        let target = self.dump_target(path, format!("{prog}-{lo:x}-{hi:x}.bin"))?;
        let (bytes, holes) = self.read_whole(lo, (hi - lo) as usize);
        if holes.iter().map(|(a, b)| b - a).sum::<u64>() == hi - lo {
            return fail(format!("{lo:#x}..{hi:#x} cannot be read: no byte of it is mapped readable"));
        }
        let mut lines = vec![self.write_dump(&target, &bytes, format!("memory {lo:#x}..{hi:#x}"))?];
        let maps = self.t.maps().unwrap_or_default();
        let inside: Vec<&crate::platform::Map> = maps.iter().filter(|m| m.end > lo && m.start < hi).collect();
        let exec = inside.iter().any(|m| m.exec);
        if let Some(m) = inside.first() {
            let perms: String = [(m.read, 'r'), (m.write, 'w'), (m.exec, 'x')]
                .iter()
                .map(|(on, c)| if *on { *c } else { '-' })
                .collect();
            let owner = if m.path.is_empty() { "no file (allocated at run time)".to_string() } else { m.path.clone() };
            let label = self.mods.label(lo, true).map(|l| format!(", {lo:#x} is {l}")).unwrap_or_default();
            let more =
                if inside.len() > 1 { format!(" (and {} more map(s))", inside.len() - 1) } else { String::new() };
            lines.push(format!("{lo:#x} lies in {perms} memory {:#x}..{:#x} of {owner}{label}{more}", m.start, m.end));
        }
        lines.extend(Self::hole_rows(&holes));
        let arch = arch_of(self.mods.main_image());
        let file = nexo_bin::shell_word(&target.display().to_string());
        let flags = format!("--raw --base {lo:#x} --arch {arch}");
        let read = if exec {
            let op = if hi - lo <= 0x4000 { format!("fn {file} {lo:#x}") } else { format!("overview {file}") };
            format!("read it as code at its own addresses: `bin {op} {flags}`")
        } else {
            format!(
                "read it at its own addresses: `bin strings {file} {flags}`, `bin bytes {file} {lo:#x} {} {flags}`",
                (hi - lo).min(256)
            )
        };
        lines.push(read);
        let meta = json!({"path": target.display().to_string(), "lo": lo, "hi": hi, "arch": arch,
            "unread": holes.iter().map(|(a, b)| b - a).sum::<u64>()});
        Ok(rows("dump", lines, meta))
    }

    fn dump_module(&mut self, name: &str, path: Option<&str>) -> Result<Output> {
        self.mods.refresh(&mut *self.t);
        let shorts: Vec<String> = (0..self.mods.list.len()).map(|i| self.mods.short(i)).collect();
        let low = name.to_lowercase();
        let mut hits: Vec<usize> = (0..shorts.len()).filter(|&i| shorts[i].eq_ignore_ascii_case(name)).collect();
        if hits.is_empty() {
            hits = (0..shorts.len())
                .filter(|&i| {
                    shorts[i].to_lowercase().contains(&low) || self.mods.list[i].path.to_lowercase().contains(&low)
                })
                .collect();
        }
        let idx = match hits.as_slice() {
            [i] => *i,
            [] => {
                let some = shorts.iter().take(12).cloned().collect::<Vec<_>>().join(", ");
                return fail(format!("no loaded module is named {name:?}; loaded: {some} (`modules` lists all)"));
            }
            many => {
                let names = many.iter().map(|&i| self.mods.list[i].path.clone()).collect::<Vec<_>>().join(", ");
                return fail(format!("{name:?} names {} modules: {names}", many.len()));
            }
        };
        let (mpath, base, end) = {
            let m = &self.mods.list[idx];
            (m.path.clone(), m.base, m.end)
        };
        let short = shorts[idx].clone();
        let head = self.read_vec(base, 0x1000);
        let kind = rebuild::kind(&head);
        let len = match kind {
            Kind::Elf => rebuild::elf_span(&head).map(|(_, n)| n),
            Kind::Pe => rebuild::pe_span(&head),
            Kind::Other => None,
        }
        .unwrap_or(end.saturating_sub(base));
        if len == 0 || len > DUMP_LIMIT {
            return fail(format!(
                "{short} at {base:#x} spans {len} bytes; dump writes 1 byte to {} MiB",
                DUMP_LIMIT >> 20
            ));
        }
        let ext = std::path::Path::new(&short).extension().map(|e| e.to_string_lossy().to_lowercase());
        let name = match ext.as_deref() {
            Some(e @ ("exe" | "dll" | "sys")) => format!("{short}-{base:x}.{e}"),
            _ => format!("{short}-{base:x}.dump"),
        };
        let target = self.dump_target(path, name)?;
        let (mem, holes) = self.read_whole(base, len as usize);
        // the file it came from, to carry over what the loader does not map (section headers, symbols)
        let disk = std::fs::metadata(&mpath)
            .ok()
            .filter(|m| m.is_file() && m.len() <= 1 << 30)
            .and_then(|_| std::fs::read(&mpath).ok());
        let file = nexo_bin::shell_word(&target.display().to_string());
        let range = format!("{base:#x}..{:#x}", base + len);
        let (bytes, mut lines) = match kind {
            Kind::Elf => {
                let r = rebuild::elf(&mem, disk.as_deref(), &mpath).map_err(crate::Error::Fail)?;
                let start = rebuild::elf_span(&head).map_or(0, |(s, _)| s);
                let shift = base.wrapping_sub(start);
                let mut l = vec![format!("{short} is an ELF image at {range}; the file has its segments at their file addresses with the bytes they hold now")];
                l.push(if shift == 0 {
                    "its file addresses are the memory addresses".to_string()
                } else {
                    format!("memory address = file address + {shift:#x} (the load shift)")
                });
                l.extend(r.notes);
                l.push(format!("read it: `bin overview {file}`"));
                (r.bytes, l)
            }
            Kind::Pe => {
                let r = rebuild::pe(&mem, base, disk.as_deref(), &mpath).map_err(crate::Error::Fail)?;
                let mut l = vec![format!("{short} is a PE image at {range}; the file has each section at its address with the bytes it holds now, and image base {base:#x}, so its addresses are the memory addresses")];
                l.extend(r.notes);
                l.push(format!("read it: `bin overview {file}`"));
                (r.bytes, l)
            }
            Kind::Other => {
                let arch = arch_of(self.mods.main_image());
                let l = vec![
                    format!("{short} at {range} is written as it lies in memory (it is not rebuilt: only ELF and PE images are)"),
                    format!("read it at its own addresses: `bin overview {file} --raw --base {base:#x} --arch {arch}`"),
                ];
                (mem, l)
            }
        };
        let wrote = self.write_dump(&target, &bytes, format!("module {mpath}"))?;
        lines.insert(0, wrote);
        if let Some(h) = Self::hole_rows(&holes) {
            lines.insert(lines.len() - 1, h);
        }
        let meta = json!({"path": target.display().to_string(), "module": mpath, "base": base, "len": len,
            "unread": holes.iter().map(|(a, b)| b - a).sum::<u64>()});
        Ok(rows("dump", lines, meta))
    }
}

#[cfg(test)]
mod tests {
    use super::dump_path_at;

    fn at(s: &str) -> Option<usize> {
        dump_path_at(&crate::split_words(s))
    }

    #[test]
    fn the_path_of_every_form_is_found() {
        assert_eq!(at("dump 0x1000..0x2000 out.bin"), Some(2));
        assert_eq!(at("dump 0x1000..0x2000"), None);
        assert_eq!(at("dump $rdi 64 out.bin"), Some(3));
        assert_eq!(at("dump $rdi 64"), None);
        assert_eq!(at("dump module libc.so.6 libc.dump"), Some(3));
        assert_eq!(at("dump module libc.so.6"), None);
        assert_eq!(at("dump binary memory out.bin 0x1000 0x2000"), Some(3));
        assert_eq!(at("dump memory out.bin 0x1000 0x2000"), Some(2));
        assert_eq!(at("mem 0x1000 out.bin"), None);
    }
}
