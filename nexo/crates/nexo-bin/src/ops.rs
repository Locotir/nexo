//! The ops. Each one takes the image and typed arguments and returns an [`Output`].

use iced_x86::{Formatter, Instruction, OpKind};
use serde_json::{json, Value};

use crate::disasm::{comment, describe, formatter, walk};
use crate::image::Section;
use crate::page::{shell_word, Output, Row, MIN_LINES};
use crate::strings::shorten;
use crate::{parse_addr, Error, Image, Result};

/// The most bytes `bytes` shows in one question.
pub const MAX_BYTES: u64 = 1 << 18;
/// The most places `find` collects.
const MAX_HITS: usize = 100_000;
/// The longest pattern `find` takes (bytes).
const MAX_PATTERN: usize = 64;

fn command(img: &Image, op: &str, rest: &str) -> String {
    let path = shell_word(&img.path.display().to_string());
    if rest.is_empty() {
        format!("nexo bin {op} {path}")
    } else {
        format!("nexo bin {op} {path} {rest}")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Rows with the line position as their key, for answers that have no address to resume from.
struct Lines(Vec<Row>);

impl Lines {
    fn text(&mut self, t: impl Into<String>) {
        let key = self.0.len() as u64;
        self.0.push(Row { key, text: t.into(), json: None });
    }
    fn item(&mut self, t: impl Into<String>, j: Value) {
        let key = self.0.len() as u64;
        self.0.push(Row { key, text: t.into(), json: Some(j) });
    }
}

/// Format, arch, entry, sections, counts and hardening.
pub fn info(img: &Image) -> Output {
    let funcs = img.funcs().len();
    let strings = img.strings().len();
    let size = img.data.len();
    let entry = match describe(img, img.entry, false) {
        Some(d) => format!("{:#x} ({d})", img.entry),
        None => format!("{:#x}", img.entry),
    };
    let mut l = Lines(Vec::new());
    l.text(format!("format      {} little-endian, {}", img.format.name(), img.arch_name()));
    l.text(format!("file        {size} bytes, sha256 {}", img.sha256));
    l.text(format!("entry       {entry}"));
    l.text(format!("base        {:#x}", img.image_base));
    l.text(format!(
        "counts      {funcs} functions, {} imports, {} exports, {strings} strings, {} symbols",
        img.imports.len(),
        img.exports.len(),
        img.symbols.len()
    ));
    let hard: Vec<String> = img.hardening.items.iter().map(|(k, v)| format!("{}: {v}", k.replace('_', " "))).collect();
    l.text(format!("hardening   {}", hard.join(" | ")));
    l.text(format!("sections    {}", img.sections.len()));
    for s in &img.sections {
        l.item(
            format!(
                "  {:>2}  {:<16} {:#012x}  {:>9}  {}{}  file {:#x}+{:#x}",
                s.index,
                s.name,
                s.addr,
                s.size,
                s.flags(),
                if s.alloc { "" } else { " (not loaded)" },
                s.file_off,
                s.file_size
            ),
            section_json(s),
        );
    }
    let hardening: serde_json::Map<String, Value> =
        img.hardening.items.iter().map(|(k, v)| ((*k).to_string(), json!(v))).collect();
    let meta = json!({
        "file": img.path.display().to_string(),
        "format": img.format.name(),
        "arch": img.arch_name(),
        "bits": 64,
        "endian": "little",
        "size": size,
        "sha256": img.sha256,
        "entry": img.entry,
        "image_base": img.image_base,
        "counts": {
            "functions": funcs, "imports": img.imports.len(), "exports": img.exports.len(),
            "strings": strings, "symbols": img.symbols.len(), "sections": img.sections.len(),
        },
        "hardening": hardening,
    });
    Output { op: "info", meta, rows: l.0, tail: 0, command: command(img, "info", "") }
}

fn section_json(s: &Section) -> Value {
    json!({"index": s.index, "name": s.name, "addr": s.addr, "size": s.size, "flags": s.flags(),
           "loaded": s.alloc, "file_offset": s.file_off, "file_size": s.file_size})
}

/// Find the function a user means: an address inside it, or its name.
fn resolve<'a>(img: &'a Image, target: &str) -> Result<&'a crate::funcs::Func> {
    let t = target.trim();
    if t.is_empty() {
        return Err(Error::Bad("give an address (0x401000) or a function name".into()));
    }
    let numeric = t.starts_with("0x") || t.starts_with("0X") || t.chars().all(|c| c.is_ascii_digit());
    if numeric {
        let a = parse_addr(t)?;
        return img.func_at(a).ok_or_else(|| match img.section_at(a) {
            Some(s) if s.exec => {
                Error::Bad(format!("{a:#x} is in {} but before the first function found there", s.name))
            }
            Some(s) => Error::Bad(format!("{a:#x} is in {}, which is data, not code (try: nexo bin bytes)", s.name)),
            None => Error::Bad(format!("{a:#x} is not inside any section")),
        });
    }
    let funcs = img.funcs();
    if let Some(f) = funcs.iter().find(|f| f.name == t) {
        return Ok(f);
    }
    let hits: Vec<_> = funcs.iter().filter(|f| f.name.contains(t)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => {
            let hint = if t.len() >= 3 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                format!("; for an address write 0x{t}")
            } else {
                String::new()
            };
            Err(Error::Bad(format!("no function is named {t:?}{hint}")))
        }
        many => {
            let list: Vec<String> = many.iter().take(5).map(|f| format!("{} ({:#x})", f.name, f.start)).collect();
            let more = if many.len() > 5 { format!(" and {} more", many.len() - 5) } else { String::new() };
            Err(Error::Bad(format!("{} functions match {t:?}: {}{more}; use an address", many.len(), list.join(", "))))
        }
    }
}

/// The whole function that holds the address (or has the name), one line per instruction.
pub fn func(img: &Image, target: &str, with_bytes: bool) -> Result<Output> {
    func_window(img, target, with_bytes, None, usize::MAX)
}

/// Like [`func`], but only the rows a page of `budget` lines starting at `from` can show are built.
/// The rest are counted (`Output::tail`), so the "more lines" figure stays exact.
pub fn func_window(img: &Image, target: &str, with_bytes: bool, from: Option<u64>, budget: usize) -> Result<Output> {
    let f = resolve(img, target)?;
    let bytes = img.read_at(f.start, (f.end - f.start) as usize).unwrap_or(&[]);
    let mut fmt = formatter();
    let mut rows: Vec<Row> = Vec::new();
    let mut line = String::new();
    // the heading row is the first row of the answer and counts against the budget
    let heading = from.is_none_or(|k| f.start >= k);
    let room = budget.max(MIN_LINES).saturating_sub(usize::from(heading));
    let mut total = 0usize;
    let mut wanted = 0usize;
    walk(bytes, f.start, |ins, raw| {
        total += 1;
        if from.is_some_and(|k| ins.ip() < k) {
            return;
        }
        wanted += 1;
        if rows.len() >= room {
            return;
        }
        line.clear();
        fmt.format(ins, &mut line);
        let note = comment(img, ins);
        let mut text = format!("{:#010x}  ", ins.ip());
        if with_bytes {
            text.push_str(&format!("{:<24}", hex(raw)));
        }
        text.push_str(&line);
        if let Some(n) = &note {
            text.push_str("  ; ");
            text.push_str(n);
        }
        rows.push(Row {
            key: ins.ip(),
            text,
            json: Some(json!({"addr": ins.ip(), "bytes": hex(raw), "text": line, "comment": note})),
        });
    });
    let tail = wanted - rows.len();
    if heading {
        let header = Row {
            key: f.start,
            text: format!(
                "fn {} {:#x}..{:#x} ({} bytes, {} instructions)",
                f.name,
                f.start,
                f.end,
                f.end - f.start,
                total
            ),
            json: None,
        };
        rows.insert(0, header);
    }
    let meta = json!({"name": f.name, "start": f.start, "end": f.end, "size": f.end - f.start, "instructions": total});
    let extra = if with_bytes { " --bytes" } else { "" };
    Ok(Output {
        op: "fn",
        meta,
        rows,
        tail,
        command: command(img, "fn", &format!("{}{extra}", shell_word(target.trim()))),
    })
}

/// How `bytes` reads memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemType {
    U8,
    U16,
    U32,
    U64,
    I32,
    F32,
    F64,
    Ptr,
    Str,
}

impl ElemType {
    pub const NAMES: [&'static str; 9] = ["u8", "u16", "u32", "u64", "i32", "f32", "f64", "ptr", "str"];

    pub fn parse(s: &str) -> Result<ElemType> {
        Ok(match s {
            "u8" => ElemType::U8,
            "u16" => ElemType::U16,
            "u32" => ElemType::U32,
            "u64" => ElemType::U64,
            "i32" => ElemType::I32,
            "f32" => ElemType::F32,
            "f64" => ElemType::F64,
            "ptr" => ElemType::Ptr,
            "str" => ElemType::Str,
            other => return Err(Error::Bad(format!("--as takes one of {}; got {other:?}", Self::NAMES.join(", ")))),
        })
    }

    pub fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    fn width(self) -> usize {
        match self {
            ElemType::U8 | ElemType::Str => 1,
            ElemType::U16 => 2,
            ElemType::U32 | ElemType::I32 | ElemType::F32 => 4,
            ElemType::U64 | ElemType::F64 | ElemType::Ptr => 8,
        }
    }

    fn per_row(self) -> usize {
        match self {
            ElemType::U8 => 16,
            ElemType::U16 | ElemType::U32 | ElemType::I32 | ElemType::F32 => 8,
            ElemType::U64 | ElemType::F64 => 4,
            ElemType::Ptr | ElemType::Str => 1,
        }
    }
}

/// `len` bytes at `addr` as a typed table.
pub fn bytes(img: &Image, addr: u64, len: u64, ty: ElemType) -> Result<Output> {
    if len == 0 {
        return Err(Error::Bad("the length is 0; ask for at least 1 byte".into()));
    }
    if len > MAX_BYTES {
        return Err(Error::Bad(format!(
            "{len} bytes is too many for one question; the limit is {MAX_BYTES}. Ask for a smaller piece."
        )));
    }
    let data = img.read_at(addr, len as usize).ok_or_else(|| match img.section_at(addr) {
        Some(s) => {
            Error::Bad(format!("{addr:#x} is in {}, which has no bytes in the file (it is filled at run time)", s.name))
        }
        None => Error::Bad(format!("{addr:#x} is not inside any section")),
    })?;
    let mut rows: Vec<Row> = Vec::new();
    let (w, per) = (ty.width(), ty.per_row());
    if ty == ElemType::Str {
        let mut pos = 0usize;
        while pos < data.len() {
            let end = data[pos..].iter().position(|&b| b == 0).map_or(data.len(), |n| pos + n);
            // escape here, once: `shorten` would escape the backslash of a `\x0a` a second time
            let text: String = data[pos..end]
                .iter()
                .map(|&b| match b {
                    b'"' => "\\\"".to_string(),
                    b'\\' => "\\\\".to_string(),
                    0x20..=0x7e => (b as char).to_string(),
                    _ => format!("\\x{b:02x}"),
                })
                .collect();
            let shown = if text.chars().count() > 200 {
                format!("{}...", text.chars().take(200).collect::<String>())
            } else {
                text.clone()
            };
            let a = addr + pos as u64;
            rows.push(Row {
                key: a,
                text: format!("{a:#010x}  \"{shown}\""),
                json: Some(json!({"addr": a, "text": text})),
            });
            pos = end + 1;
        }
    } else {
        let n = data.len() / w;
        let mut i = 0;
        while i < n {
            let k = per.min(n - i);
            let a = addr + (i * w) as u64;
            let mut cells: Vec<String> = Vec::new();
            let mut vals: Vec<Value> = Vec::new();
            for j in 0..k {
                let b = &data[(i + j) * w..(i + j + 1) * w];
                match ty {
                    ElemType::U8 => {
                        cells.push(format!("{:02x}", b[0]));
                        vals.push(json!(b[0]));
                    }
                    ElemType::U16 => {
                        let v = u16::from_le_bytes([b[0], b[1]]);
                        cells.push(format!("{v:#06x}"));
                        vals.push(json!(v));
                    }
                    ElemType::U32 => {
                        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        cells.push(format!("{v:#010x}"));
                        vals.push(json!(v));
                    }
                    ElemType::I32 => {
                        let v = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        cells.push(format!("{v:>11}"));
                        vals.push(json!(v));
                    }
                    ElemType::F32 => {
                        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        cells.push(format!("{v:>12}"));
                        vals.push(json!(v));
                    }
                    ElemType::U64 | ElemType::F64 | ElemType::Ptr => {
                        let raw = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
                        if ty == ElemType::F64 {
                            let v = f64::from_le_bytes(raw);
                            cells.push(format!("{v:>20}"));
                            vals.push(json!(v));
                        } else {
                            let v = u64::from_le_bytes(raw);
                            cells.push(format!("{v:#018x}"));
                            vals.push(json!(v));
                        }
                    }
                    ElemType::Str => {}
                }
            }
            let mut text = format!("{a:#010x}  {}", cells.join(" "));
            let mut j = json!({"addr": a, "values": vals});
            if ty == ElemType::U8 {
                let ascii: String =
                    data[i..i + k].iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
                text.push_str(&format!("{}  |{ascii}|", " ".repeat(3 * (per - k))));
                j["ascii"] = json!(ascii);
            }
            if ty == ElemType::Ptr {
                let v = vals[0].as_u64().unwrap_or(0);
                let what = describe(img, v, false);
                if let Some(wh) = &what {
                    text.push_str(&format!("  {wh}"));
                }
                j["target"] = json!(what);
            }
            rows.push(Row { key: a, text, json: Some(j) });
            i += k;
        }
        let rest = data.len() - n * w;
        if rest > 0 {
            let a = addr + (n * w) as u64;
            rows.push(Row {
                key: a,
                text: format!("{a:#010x}  ({rest} trailing bytes not shown: not a whole {})", ty.name()),
                json: None,
            });
        }
    }
    if (data.len() as u64) < len {
        let a = addr + data.len() as u64;
        rows.push(Row {
            key: a,
            text: format!("... the file holds no more bytes after {a:#x} here ({} of {len} read)", data.len()),
            json: None,
        });
    }
    let meta = json!({"addr": addr, "len": data.len(), "as": ty.name()});
    Ok(Output {
        op: "bytes",
        meta,
        rows,
        tail: 0,
        command: command(img, "bytes", &format!("{addr:#x} {len} --as {}", ty.name())),
    })
}

/// Strings (ASCII and UTF-16LE) whose text matches the regex; all of them without one.
pub fn strings(img: &Image, grep: Option<&str>) -> Result<Output> {
    let re = grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let all = img.strings();
    let mut rows = Vec::new();
    for s in all {
        if re.as_ref().is_some_and(|r| !r.is_match(&s.text)) {
            continue;
        }
        rows.push(Row {
            key: s.addr,
            text: format!("{:#010x}  {}\"{}\"", s.addr, if s.wide { "L" } else { "" }, shorten(&s.text, 200)),
            json: Some(json!({"addr": s.addr, "wide": s.wide, "chars": s.chars, "text": s.text})),
        });
    }
    let matched = rows.len();
    if rows.is_empty() {
        rows.push(Row { key: 0, text: format!("no string matches (of {} found)", all.len()), json: None });
    }
    let rest = grep.map(|g| format!("--grep {}", shell_word(g))).unwrap_or_default();
    Ok(Output {
        op: "strings",
        meta: json!({"total": all.len(), "matched": matched, "grep": grep}),
        rows,
        tail: 0,
        command: command(img, "strings", &rest),
    })
}

/// A hex pattern: bytes written with or without spaces, `0x` or commas; `??` matches any byte.
fn parse_pattern(s: &str) -> Result<Vec<Option<u8>>> {
    let cleaned: String = s
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ',' | '_'))
        .collect();
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.is_empty() || !chars.len().is_multiple_of(2) {
        return Err(Error::Bad(format!(
            "{s:?} is not hex bytes: write pairs of digits like \"48 8d 05\" (?? matches any byte)"
        )));
    }
    let mut out = Vec::new();
    for pair in chars.chunks(2) {
        if pair == ['?', '?'] {
            out.push(None);
            continue;
        }
        let t: String = pair.iter().collect();
        let b = u8::from_str_radix(&t, 16).map_err(|_| Error::Bad(format!("{t:?} in {s:?} is not a hex byte")))?;
        out.push(Some(b));
    }
    if out.len() > MAX_PATTERN {
        return Err(Error::Bad(format!("the pattern is {} bytes; the limit is {MAX_PATTERN}", out.len())));
    }
    if out.iter().all(Option::is_none) {
        return Err(Error::Bad("a pattern of only ?? matches everywhere; give at least one real byte".into()));
    }
    Ok(out)
}

/// Where a hit sits: the section and, in code, the function.
fn place(img: &Image, addr: u64) -> (String, String) {
    let sec = img.section_at(addr).map(|s| s.name.clone()).unwrap_or_default();
    let f = if img.is_exec(addr) { img.label(addr).unwrap_or_default() } else { String::new() };
    (sec, f)
}

fn hit_row(img: &Image, addr: u64, what: &str, kind: &str) -> Row {
    let (sec, f) = place(img, addr);
    Row {
        key: addr,
        text: format!("{addr:#010x}  {sec:<8} {f:<24} {what}"),
        json: Some(
            json!({"addr": addr, "section": sec, "function": if f.is_empty() { Value::Null } else { json!(f) }, "kind": kind, "what": what}),
        ),
    }
}

fn finish_hits(
    img: &Image,
    op: &'static str,
    mut rows: Vec<Row>,
    rest: String,
    mut meta: Value,
    capped: bool,
) -> Output {
    rows.sort_by_key(|r| r.key);
    rows.dedup_by_key(|r| r.key);
    meta["hits"] = json!(rows.len());
    meta["capped"] = json!(capped);
    if capped {
        let key = rows.last().map_or(0, |r| r.key);
        rows.push(Row {
            key,
            text: format!("... stopped after {MAX_HITS} matches; give a longer pattern to narrow it"),
            json: None,
        });
    } else if rows.is_empty() {
        rows.push(Row { key: 0, text: "no match in the mapped sections".into(), json: None });
    }
    Output { op, meta, rows, tail: 0, command: command(img, "find", &rest) }
}

/// Every place the byte pattern occurs in the mapped sections.
pub fn find_bytes(img: &Image, pattern: &str) -> Result<Output> {
    let pat = parse_pattern(pattern)?;
    let mut rows = Vec::new();
    let mut capped = false;
    'sec: for sec in img.sections.iter().filter(|s| s.alloc && s.file_size > 0) {
        let data = img.section_bytes(sec);
        if data.len() < pat.len() {
            continue;
        }
        for i in 0..=data.len() - pat.len() {
            if pat.iter().zip(&data[i..]).all(|(p, b)| p.is_none_or(|p| p == *b)) {
                rows.push(hit_row(img, sec.addr + i as u64, &hex(&data[i..i + pat.len()]), "bytes"));
                if rows.len() >= MAX_HITS {
                    capped = true;
                    break 'sec;
                }
            }
        }
    }
    let shown: Vec<String> = pat.iter().map(|p| p.map_or("??".to_string(), |b| format!("{b:02x}"))).collect();
    let rest = shell_word(&shown.join(" "));
    Ok(finish_hits(img, "find", rows, rest, json!({"pattern": shown.join(" ")}), capped))
}

fn imm_matches(ins: &Instruction, v: i128) -> bool {
    (0..ins.op_count()).any(|i| {
        let bits = match ins.op_kind(i) {
            OpKind::Immediate8 | OpKind::Immediate8_2nd => 8,
            OpKind::Immediate16 | OpKind::Immediate8to16 => 16,
            OpKind::Immediate32 | OpKind::Immediate8to32 => 32,
            OpKind::Immediate64 | OpKind::Immediate8to64 | OpKind::Immediate32to64 => 64,
            _ => return false,
        };
        if v < -(1i128 << (bits - 1)) || v > (1i128 << bits) - 1 {
            return false;
        }
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        (ins.immediate(i) & mask) == ((v as u64) & mask)
    })
}

/// Every instruction with the number as an immediate, and every place the data sections hold it as a
/// 4-byte (or, for a larger number, 8-byte) little-endian value.
pub fn find_imm(img: &Image, value: i128) -> Result<Output> {
    if value < i128::from(i64::MIN) || value > i128::from(u64::MAX) {
        return Err(Error::Bad("the number does not fit in 64 bits".into()));
    }
    let mut rows = Vec::new();
    let mut capped = false;
    let mut fmt = formatter();
    let mut line = String::new();
    // every function found, plus the part of each code section before its first function
    // (or all of it when no function was found there)
    let funcs = img.funcs();
    let mut ranges: Vec<(u64, u64)> = funcs.iter().map(|f| (f.start, f.end)).collect();
    for sec in img.code_sections() {
        let first = funcs.iter().map(|f| f.start).filter(|&a| sec.contains(a)).min();
        let head_end = first.unwrap_or(sec.addr.saturating_add(sec.backed_len()));
        if head_end > sec.addr {
            ranges.push((sec.addr, head_end));
        }
    }
    'code: for (start, end) in ranges {
        let Some(code) = img.read_at(start, (end - start) as usize) else { continue };
        let mut full = false;
        walk(code, start, |ins, _| {
            if full || !imm_matches(ins, value) {
                return;
            }
            line.clear();
            fmt.format(ins, &mut line);
            rows.push(hit_row(img, ins.ip(), &line, "imm"));
            full = rows.len() >= MAX_HITS;
        });
        if full {
            capped = true;
            break 'code;
        }
    }
    let needle: Vec<u8> = if (-(1i128 << 31)..(1i128 << 32)).contains(&value) {
        (value as u32).to_le_bytes().to_vec()
    } else {
        (value as u64).to_le_bytes().to_vec()
    };
    if !capped {
        'data: for sec in img.sections.iter().filter(|s| s.alloc && !s.exec && s.file_size > 0) {
            let data = img.section_bytes(sec);
            if data.len() < needle.len() {
                continue;
            }
            for i in 0..=data.len() - needle.len() {
                if data[i..i + needle.len()] == needle[..] {
                    rows.push(hit_row(
                        img,
                        sec.addr + i as u64,
                        &format!("data {}  {}", if needle.len() == 4 { "u32" } else { "u64" }, hex(&needle)),
                        "data",
                    ));
                    if rows.len() >= MAX_HITS {
                        capped = true;
                        break 'data;
                    }
                }
            }
        }
    }
    let rest = format!("--imm {value}");
    Ok(finish_hits(img, "find", rows, rest, json!({"imm": value.to_string()}), capped))
}
