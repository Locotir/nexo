//! `fn`: one function's disassembly, the page that holds an address inside it, or a range that
//! crosses function edges (`--to`).
//!
//! Only the lines a page can show are formatted. Everything else is counted.

use iced_x86::{Formatter, Instruction, IntelFormatter, Mnemonic};
use serde_json::json;

use crate::disasm::{comment, formatter, walk};
use crate::funcs::Func;
use crate::ops::{command, hex, numeric, resolve};
use crate::page::{shell_word, window, Output, Row, MIN_LINES};
use crate::{parse_addr, Error, Image, Result};

/// Instructions shown before the one that holds an address that was asked for (fewer when the
/// line budget is the smallest one).
pub const LEAD_IN: usize = 8;
/// The most bytes one `--to` range may cover.
pub const MAX_RANGE: u64 = 1 << 24;

/// One question to `fn`.
#[derive(Debug, Clone, Copy)]
pub struct FnQuery<'a> {
    /// An address (inside a function or not) or a function name.
    pub target: &'a str,
    /// Show every instruction from the target up to (not including) this address, across functions.
    pub to: Option<&'a str>,
    pub with_bytes: bool,
    /// Start at this address (a line position with `to`).
    pub from: Option<u64>,
    /// Lines in a page.
    pub budget: usize,
    /// The caller asks for JSON: the commands inside the answer ask for it too.
    pub json: bool,
}

impl<'a> FnQuery<'a> {
    pub fn new(target: &'a str) -> FnQuery<'a> {
        FnQuery { target, to: None, with_bytes: false, from: None, budget: usize::MAX, json: false }
    }
}

/// Answer a [`FnQuery`].
pub fn func_view(img: &Image, q: &FnQuery) -> Result<Output> {
    // the references first: one decode pass gives them and the function list
    let _ = img.refs();
    match q.to {
        Some(to) => range(img, q, to),
        None => one(img, q),
    }
}

fn is_padding(ins: &Instruction) -> bool {
    matches!(ins.mnemonic(), Mnemonic::Int3 | Mnemonic::Nop)
}

fn bytes_of<'a>(img: &'a Image, f: &Func) -> &'a [u8] {
    img.read_at(f.start, f.end.saturating_sub(f.start) as usize).unwrap_or(&[])
}

/// The header line of a function. `extra` is what a partial view adds.
fn header(img: &Image, f: &Func, instructions: usize, extra: &str) -> String {
    let mut t = format!(
        "fn {} {:#x}..{:#x} ({} bytes, {instructions} instructions)  ; refs {}",
        img.func_name(f),
        f.start,
        f.end,
        f.end.saturating_sub(f.start),
        img.refs().callers(f.start)
    );
    t.push_str(extra);
    if let Some(r) = img.raw_name_at(f.start) {
        t.push_str(&format!("  ; raw {r}"));
    }
    if let Some(n) = img.note_at(f.start) {
        t.push_str(&format!("  ; note: {n}"));
    }
    t
}

/// One instruction line. `mark` adds the first column: `>` for the asked instruction, a space otherwise.
fn instr_row(
    img: &Image,
    fmt: &mut IntelFormatter,
    ins: &Instruction,
    raw: &[u8],
    with_bytes: bool,
    mark: Option<bool>,
    function: Option<&str>,
) -> Row {
    let mut line = String::new();
    fmt.format(ins, &mut line);
    let note = comment(img, ins);
    let mut text = String::new();
    if let Some(m) = mark {
        text.push_str(if m { "> " } else { "  " });
    }
    text.push_str(&format!("{:#010x}  ", ins.ip()));
    if with_bytes {
        text.push_str(&format!("{:<24}", hex(raw)));
    }
    text.push_str(&line);
    if let Some(n) = &note {
        text.push_str("  ; ");
        text.push_str(n);
    }
    let mut j = json!({"addr": ins.ip(), "bytes": hex(raw), "text": line, "comment": note});
    if mark == Some(true) {
        j["asked"] = json!(true);
    }
    if let Some(name) = function {
        j["function"] = json!(name);
    }
    Row { key: ins.ip(), text, json: Some(j) }
}

fn one(img: &Image, q: &FnQuery) -> Result<Output> {
    let f = resolve(img, q.target)?;
    let t = q.target.trim();
    let bytes = bytes_of(img, f);
    // an address inside the function, not its first byte: the page opens near it and marks it
    let asked = if numeric(t) { parse_addr(t).ok().filter(|&a| a != f.start) } else { None };
    let (mut total, mut holder) = (0usize, None::<usize>);
    walk(bytes, f.start, |ins, raw| {
        if let Some(a) = asked {
            if holder.is_none() && ins.ip().saturating_add(raw.len() as u64) > a {
                holder = Some(total);
            }
        }
        total += 1;
    });
    let budget = q.budget.max(MIN_LINES);
    // The cut line takes one line of the budget, so the asked line has to be within `budget - 1` rows:
    // the heading, the "earlier lines" line, the lead-in and the asked instruction.
    let lead = LEAD_IN.min(budget.saturating_sub(4));
    let skip = match (q.from, holder) {
        (None, Some(k)) => k.saturating_sub(lead),
        _ => 0,
    };
    let earlier = skip > 0;
    let heading = q.from.is_none_or(|k| f.start >= k);
    let room = budget.saturating_sub(usize::from(heading) + usize::from(earlier));
    let mut fmt = formatter();
    let mut rows: Vec<Row> = Vec::new();
    let (mut i, mut wanted, mut first_ip) = (0usize, 0usize, None::<u64>);
    walk(bytes, f.start, |ins, raw| {
        let n = i;
        i += 1;
        if n < skip || q.from.is_some_and(|k| ins.ip() < k) {
            return;
        }
        wanted += 1;
        if rows.len() >= room {
            return;
        }
        first_ip.get_or_insert(ins.ip());
        let mark = asked.map(|_| holder == Some(n));
        rows.push(instr_row(img, &mut fmt, ins, raw, q.with_bytes, mark, None));
    });
    let tail = wanted - rows.len();
    let base = command(img, "fn", &format!("{}{}", shell_word(t), if q.with_bytes { " --bytes" } else { "" }));
    if earlier {
        let n = (skip + 2).max(MIN_LINES);
        let json = if q.json { " --json" } else { "" };
        let cmd = format!("{base}{json} --from {:#x} --lines {n}", f.start);
        rows.insert(0, Row { key: f.start, text: format!("... {skip} earlier lines: {cmd}"), json: None });
    }
    if heading {
        let extra = match (earlier, first_ip) {
            (true, Some(a)) => format!("  ; showing from {a:#x}"),
            _ => String::new(),
        };
        rows.insert(0, Row { key: f.start, text: header(img, f, total, &extra), json: None });
    }
    let mut meta = json!({"name": img.func_name(f), "start": f.start, "end": f.end,
        "size": f.end.saturating_sub(f.start), "instructions": total,
        "refs": img.refs().callers(f.start), "note": img.note_at(f.start), "raw_name": img.raw_name_at(f.start)});
    if earlier {
        meta["earlier_lines"] = json!(skip);
        meta["showing_from"] = json!(first_ip);
    }
    if let Some(a) = asked {
        meta["asked"] = json!(a);
    }
    Ok(Output { op: "fn", meta, rows, tail, command: base })
}

/// What one function adds to a range, counted without formatting a line.
struct Part {
    f: usize,
    /// First address shown and the limit: an instruction below it is shown.
    lo: u64,
    hi: u64,
    /// Instruction lines shown (the closing padding is not among them).
    instrs: usize,
    /// The padding that closes the part: where it starts and how many bytes it takes (0: none).
    pad_from: u64,
    pad_bytes: u64,
    /// Bytes between the end of the function before and the start of this one.
    gap: u64,
    /// Instructions the whole function has.
    all: usize,
    /// Line position of the first line this part adds.
    first_line: usize,
}

impl Part {
    fn lines(&self) -> usize {
        usize::from(self.gap > 0) + 1 + self.instrs + usize::from(self.pad_bytes > 0)
    }
}

fn check_code(img: &Image, addr: u64, what: &str) -> Result<()> {
    match img.section_at(addr) {
        Some(s) if s.exec => Ok(()),
        Some(s) => Err(Error::Usage(format!("{what} {addr:#x} is in {}, which is data, not code", s.name))),
        None => Err(Error::Usage(format!("{what} {addr:#x} is not inside any section"))),
    }
}

fn plural(n: u64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn range(img: &Image, q: &FnQuery, to: &str) -> Result<Output> {
    let t = q.target.trim();
    let s0 = if numeric(t) { parse_addr(t)? } else { resolve(img, t)?.start };
    let s1 = parse_addr(to)?;
    if s1 <= s0 {
        return Err(Error::Usage(format!("--to {s1:#x} is not after the start {s0:#x}; give a higher address")));
    }
    if s1 - s0 > MAX_RANGE {
        return Err(Error::Usage(format!("the range is {} bytes; ask for at most {MAX_RANGE}", s1 - s0)));
    }
    check_code(img, s0, "the start")?;
    check_code(img, s1 - 1, "the end")?;
    let funcs = img.funcs();
    let i0 =
        funcs.partition_point(|f| f.start <= s0).checked_sub(1).filter(|&i| s0 < funcs[i].end).ok_or_else(|| {
            Error::Usage(format!("no function holds the start {s0:#x}; start at a function or inside one"))
        })?;
    // pass 1: what each function adds, without building a line
    let mut parts: Vec<Part> = Vec::new();
    let mut line = 0usize;
    for (n, f) in funcs.iter().enumerate().skip(i0) {
        if f.start >= s1 {
            break;
        }
        let hi = f.end.min(s1);
        let mut lo = if n == i0 { u64::MAX } else { f.start };
        let (mut all, mut in_range, mut trail_n, mut trail_from, mut last_end) = (0usize, 0usize, 0usize, 0u64, 0u64);
        walk(bytes_of(img, f), f.start, |ins, raw| {
            all += 1;
            let end = ins.ip().saturating_add(raw.len() as u64);
            if lo == u64::MAX && end > s0 {
                lo = ins.ip();
            }
            if lo != u64::MAX && ins.ip() >= lo && ins.ip() < hi {
                in_range += 1;
                if is_padding(ins) {
                    if trail_n == 0 {
                        trail_from = ins.ip();
                    }
                    trail_n += 1;
                } else {
                    trail_n = 0;
                }
                last_end = end;
            }
        });
        if lo == u64::MAX {
            lo = f.start;
        }
        // a part that is only padding is shown as the instructions it has
        if trail_n == in_range {
            trail_n = 0;
        }
        let gap = parts.last().map_or(0, |p| f.start.saturating_sub(funcs[p.f].end));
        let part = Part {
            f: n,
            lo,
            hi,
            instrs: in_range - trail_n,
            pad_from: trail_from,
            pad_bytes: if trail_n > 0 { last_end.saturating_sub(trail_from) } else { 0 },
            gap,
            all,
            first_line: line,
        };
        line = line.saturating_add(part.lines());
        parts.push(part);
    }
    let total = line;
    // pass 2: the lines of the window
    let (start, end) = window(q.from, q.budget, total);
    let mut fmt = formatter();
    let mut rows: Vec<Row> = Vec::new();
    let first = parts.partition_point(|p| p.first_line + p.lines() <= start);
    let keep = |pos: usize| pos >= start && pos < end;
    for p in &parts[first..] {
        if p.first_line >= end {
            break;
        }
        let f = &funcs[p.f];
        let name = img.func_name(f).to_string();
        let mut pos = p.first_line;
        if p.gap > 0 {
            if keep(pos) {
                rows.push(Row {
                    key: pos as u64,
                    text: format!("... {} outside any function", plural(p.gap, "byte")),
                    json: Some(json!({"addr": f.start.saturating_sub(p.gap), "gap_bytes": p.gap})),
                });
            }
            pos += 1;
        }
        let extra = match (p.lo > f.start, p.hi < f.end) {
            (true, true) => format!("  ; showing {:#x} to {:#x}", p.lo, p.hi),
            (true, false) => format!("  ; showing from {:#x}", p.lo),
            (false, true) => format!("  ; showing up to {:#x}", p.hi),
            (false, false) => String::new(),
        };
        if keep(pos) {
            rows.push(Row { key: pos as u64, text: header(img, f, p.all, &extra), json: None });
        }
        pos += 1;
        let stop = if p.pad_bytes > 0 { p.pad_from } else { p.hi };
        walk(bytes_of(img, f), f.start, |ins, raw| {
            if ins.ip() < p.lo || ins.ip() >= stop || pos >= end {
                return;
            }
            if keep(pos) {
                let mut row = instr_row(img, &mut fmt, ins, raw, q.with_bytes, None, Some(&name));
                row.key = pos as u64;
                rows.push(row);
            }
            pos += 1;
        });
        if p.pad_bytes > 0 && keep(pos) {
            rows.push(Row {
                key: pos as u64,
                text: format!("... {} of padding", plural(p.pad_bytes, "byte")),
                json: Some(json!({"addr": p.pad_from, "padding_bytes": p.pad_bytes, "function": name})),
            });
        }
    }
    let tail = total - end;
    let mut rest = format!("{} --to {}", shell_word(t), shell_word(to.trim()));
    if q.with_bytes {
        rest.push_str(" --bytes");
    }
    let meta = json!({"start": s0, "to": s1, "functions": parts.len(), "lines": total});
    Ok(Output { op: "fn", meta, rows, tail, command: command(img, "fn", &rest) })
}
