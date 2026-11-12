//! Source lines from DWARF (`.debug_line`): the file and line an address in code belongs to. Only plain
//! (uncompressed) ELF sections are read; a file with none has no lines, which is not an error.

use nexo_bin::Image;

/// Biggest `.debug_line` read: past it the table is not built (a build of the whole of a very large program).
const MAX_BYTES: usize = 96 << 20;

struct Row {
    addr: u64,
    file: u32,
    line: u32,
    /// Ends a sequence: the address is the first one past the code, not a line.
    end: bool,
}

/// Code that was inlined: where it sits, and the line of the call it was inlined at.
struct Inline {
    lo: u64,
    hi: u64,
    depth: u32,
    file: u32,
    line: u32,
}

pub struct Lines {
    files: Vec<String>,
    rows: Vec<Row>,
    inlined: Vec<Inline>,
}

/// Where an address is: the line of the code itself, and, when it was inlined, the line it was called from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Where {
    pub file: String,
    pub line: u32,
    /// The innermost line (inside the inlined code), when different from the call site above.
    pub inside: Option<(String, u32)>,
}

impl Lines {
    /// The table of an image, or `None` when it has no (readable) line info.
    pub fn of(img: &Image) -> Option<Lines> {
        let get = |name: &str| -> &[u8] {
            img.sections.iter().find(|s| s.name == name).map_or(&[][..], |s| img.section_bytes(s))
        };
        if get(".debug_line").is_empty() || get(".debug_line").len() > MAX_BYTES || get(".debug_info").is_empty() {
            return None;
        }
        let load = |id: gimli::SectionId| -> Result<gimli::EndianSlice<'_, gimli::LittleEndian>, gimli::Error> {
            Ok(gimli::EndianSlice::new(get(id.name()), gimli::LittleEndian))
        };
        let dwarf = gimli::Dwarf::load(load).ok()?;
        let mut out = Lines { files: Vec::new(), rows: Vec::new(), inlined: Vec::new() };
        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let Ok(unit) = dwarf.unit(header) else { continue };
            let Some(program) = unit.line_program.clone() else { continue };
            let comp_dir = unit.comp_dir.map(|d| d.to_string_lossy().into_owned());
            let mut seen: Vec<(u64, u32)> = Vec::new();
            let header = program.header().clone();
            let header = &header;
            let mut rows = program.rows();
            while let Ok(Some((h, row))) = rows.next_row() {
                let id = row.file_index();
                let file = match seen.iter().find(|(k, _)| *k == id) {
                    Some((_, f)) => *f,
                    None => {
                        let name = h.file(id).map(|f| {
                            let mut p = String::new();
                            if let Some(d) = f.directory(h) {
                                if let Ok(d) = dwarf.attr_string(&unit, d) {
                                    p = d.to_string_lossy().into_owned();
                                }
                            }
                            let n = dwarf.attr_string(&unit, f.path_name()).map(|n| n.to_string_lossy().into_owned());
                            let n = n.unwrap_or_default();
                            if n.starts_with('/') || p.is_empty() {
                                n
                            } else {
                                format!("{p}/{n}")
                            }
                        });
                        let mut name = name.unwrap_or_default();
                        if !name.starts_with('/') {
                            if let Some(c) = &comp_dir {
                                name = format!("{c}/{name}");
                            }
                        }
                        out.files.push(name);
                        let f = (out.files.len() - 1) as u32;
                        seen.push((id, f));
                        f
                    }
                };
                out.rows.push(Row {
                    addr: row.address(),
                    file,
                    line: row.line().map_or(0, |l| l.get() as u32),
                    end: row.end_sequence(),
                });
            }
            let name_of = |id: u64, files: &mut Vec<String>| -> Option<u32> {
                let f = header.file(id)?;
                let mut p = String::new();
                if let Some(d) = f.directory(header) {
                    if let Ok(d) = dwarf.attr_string(&unit, d) {
                        p = d.to_string_lossy().into_owned();
                    }
                }
                let n = dwarf.attr_string(&unit, f.path_name()).ok()?.to_string_lossy().into_owned();
                let mut name = if n.starts_with('/') || p.is_empty() { n } else { format!("{p}/{n}") };
                if !name.starts_with('/') {
                    if let Some(c) = &comp_dir {
                        name = format!("{c}/{name}");
                    }
                }
                files.push(name);
                Some((files.len() - 1) as u32)
            };
            let mut cache: Vec<(u64, Option<u32>)> = Vec::new();
            let mut entries = unit.entries();
            while let Ok(Some(e)) = entries.next_dfs() {
                let depth = e.depth();
                if e.tag() != gimli::DW_TAG_inlined_subroutine {
                    continue;
                }
                let (Some(gimli::AttributeValue::FileIndex(fi)), Some(line)) = (
                    e.attr_value(gimli::DW_AT_call_file),
                    e.attr_value(gimli::DW_AT_call_line).and_then(|v| v.udata_value()),
                ) else {
                    continue;
                };
                let file = match cache.iter().find(|(k, _)| *k == fi) {
                    Some((_, f)) => *f,
                    None => {
                        let f = name_of(fi, &mut out.files);
                        cache.push((fi, f));
                        f
                    }
                };
                let Some(file) = file else { continue };
                let Ok(mut ranges) = dwarf.die_ranges(&unit, e) else { continue };
                while let Ok(Some(r)) = ranges.next() {
                    if r.end > r.begin {
                        out.inlined.push(Inline {
                            lo: r.begin,
                            hi: r.end,
                            depth: depth.max(0) as u32,
                            file,
                            line: line as u32,
                        });
                    }
                }
            }
        }
        if out.rows.is_empty() {
            return None;
        }
        // stable: an end-of-sequence row sorts before a row starting at the same address
        out.rows.sort_by_key(|r| (r.addr, !r.end));
        Some(out)
    }

    /// The place of the code at `addr` (a file address): the last row at or below it, unless a sequence ended
    /// before reaching it. Inlined code is placed at the line of its outermost call, with the inner line aside.
    pub fn at(&self, addr: u64) -> Option<Where> {
        let i = self.rows.partition_point(|r| r.addr <= addr).checked_sub(1)?;
        let r = &self.rows[i];
        if r.end || r.line == 0 {
            return None;
        }
        let file = self.files.get(r.file as usize)?;
        let outer = self.inlined.iter().filter(|n| addr >= n.lo && addr < n.hi).min_by_key(|n| n.depth);
        Some(match outer {
            Some(n) => Where {
                file: self.files.get(n.file as usize)?.clone(),
                line: n.line,
                inside: Some((file.clone(), r.line)),
            },
            None => Where { file: file.clone(), line: r.line, inside: None },
        })
    }
}

/// A path as a person reads it in a call chain: the last two parts (`src/main.rs`).
pub fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let from = parts.len().saturating_sub(2);
    parts[from..].join("/")
}

impl Where {
    /// `src/main.rs:42`, or `src/main.rs:42, inlined ptr/mod.rs:2160`.
    pub fn text(&self) -> String {
        let head = format!("{}:{}", short_path(&self.file), self.line);
        match &self.inside {
            Some((f, l)) => format!("{head}, inlined {}:{l}", short_path(f)),
            None => head,
        }
    }
}
