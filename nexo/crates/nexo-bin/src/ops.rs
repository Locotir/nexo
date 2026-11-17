//! The ops. Each one takes the image and typed arguments and returns an [`Output`].

use iced_x86::{Formatter, Instruction, OpKind};
use serde_json::{json, Value};

use crate::disasm::{describe, formatter, walk_bits};
use crate::image::Section;
use crate::page::{shell_word, Output, Row, MIN_LINES};
use crate::strings::shorten;
use crate::{Error, Image, Result};

/// The most bytes `bytes` shows in one question.
pub const MAX_BYTES: u64 = 1 << 18;
/// The most places `find` collects.
const MAX_HITS: usize = 100_000;
/// The longest pattern `find` takes (bytes).
const MAX_PATTERN: usize = 64;

pub(crate) fn command(img: &Image, op: &str, rest: &str) -> String {
    // a slice picked from a universal Mach-O file stays picked in the next command
    let path = shell_word(&img.path.display().to_string()) + &crate::image::reopen_flags(img);
    if rest.is_empty() {
        format!("nexo bin {op} {path}")
    } else {
        format!("nexo bin {op} {path} {rest}")
    }
}

/// `nexo bin OP FILE --arch ARCH`: the same file read through another slice.
pub(crate) fn command_arch(img: &Image, op: &str, arch: &str) -> String {
    format!("nexo bin {op} {} --arch {arch}", shell_word(&img.path.display().to_string()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
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
    l.text(format!("format      {} little-endian, {}", img.format_name(), img.arch_name()));
    if img.format == crate::Format::Raw {
        l.text(
            "loaded      as given (--raw): no headers, so the base, the arch and the entry (the first byte) are the ones \
             named; one section holds every byte; decomp reads calls with the System V registers"
                .to_string(),
        );
    }
    l.text(format!("file        {size} bytes, sha256 {}", img.sha256));
    for w in &img.warnings {
        l.text(format!("warning     {w}"));
    }
    l.text(format!("entry       {entry}"));
    l.text(format!("base        {:#x}", img.image_base));
    l.text(format!(
        "counts      {funcs} functions, {} imports, {} exports, {strings} strings, {} symbols",
        img.imports.len(),
        img.exports.len(),
        img.symbols.len()
    ));
    let hard: Vec<String> = img.hardening.items.iter().map(|(k, v)| format!("{}: {v}", k.replace('_', " "))).collect();
    if !hard.is_empty() || img.format != crate::Format::Raw {
        l.text(format!("hardening   {}", hard.join(" | ")));
    }
    for t in crate::macho::info_lines(img) {
        l.text(t);
    }
    if let Some(t) = img.debug_info_line() {
        l.text(t);
    }
    if let Some(p) = &img.pdb {
        l.text(match &p.note {
            None => format!(
                "pdb         {} ({} functions, {} source lines, {} types)",
                p.path.display(),
                p.funcs.len(),
                p.lines.len(),
                p.types.structs.len()
            ),
            Some(n) => format!("pdb         {n}"),
        });
    }
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
    let mut meta = json!({
        "file": img.path.display().to_string(),
        "format": img.format_name(),
        "arch": img.arch_name(),
        "bits": img.bits,
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
        "warnings": img.warnings,
    });
    if let Some(m) = crate::macho::info_json(img) {
        meta["macho"] = m;
    }
    Output { op: "info", meta, rows: l.0, tail: 0, command: command(img, "info", "") }
}

fn section_json(s: &Section) -> Value {
    json!({"index": s.index, "name": s.name, "addr": s.addr, "size": s.size, "flags": s.flags(),
           "loaded": s.alloc, "file_offset": s.file_off, "file_size": s.file_size})
}

/// A target written as a number (0x401000 or plain digits), not a name.
pub(crate) fn numeric(t: &str) -> bool {
    let t = t.trim();
    t.starts_with("0x") || t.starts_with("0X") || (!t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
}

/// Find the function a user means: an address inside it, or its name (any form of [`crate::place::AddrExpr`]).
pub(crate) fn resolve<'a>(img: &'a Image, target: &str) -> Result<&'a crate::funcs::Func> {
    let t = target.trim();
    if t.is_empty() {
        return Err(Error::Bad("give an address (0x401000) or a function name".into()));
    }
    match crate::place::AddrExpr::parse(t)? {
        crate::place::AddrExpr::Num(a) => func_at_addr(img, a),
        crate::place::AddrExpr::Name { off: 0, .. } => resolve_name(img, t),
        _ => crate::place::func_of(img, t).or_else(|e| resolve_name(img, t).map_err(|_| e)),
    }
}

/// The function that holds `a`, or why there is none.
pub(crate) fn func_at_addr(img: &Image, a: u64) -> Result<&crate::funcs::Func> {
    img.func_at(a).ok_or_else(|| match img.section_at(a) {
        Some(s) if s.exec => gap_in_code(img, s, a),
        Some(s) => Error::Bad(format!("{a:#x} is in {}, which is data, not code (try: nexo bin bytes)", s.name)),
        None => Error::Bad(format!("{a:#x} is not inside any section")),
    })
}

/// What lies at a code address no function holds: before the first function, or (in bare code) after one ended,
/// with the calls that read it as code or as bytes.
fn gap_in_code(img: &Image, s: &crate::image::Section, a: u64) -> Error {
    let funcs = img.funcs();
    let n = funcs.partition_point(|f| f.start <= a);
    let sec_end = s.addr + s.backed_len();
    let Some(before) = funcs[..n].last().filter(|f| s.contains(f.start) && a < sec_end) else {
        return Error::Bad(format!("{a:#x} is in {} but before the first function found there", s.name));
    };
    let next = funcs.get(n).map_or(sec_end, |f| f.start).min(sec_end);
    Error::Bad(format!(
        "{a:#x} is in {} after the end of {} ({:#x}), {} bytes before {}; as code: {}; as bytes: {}",
        s.name,
        img.func_name(before),
        before.end,
        next - a,
        funcs
            .get(n)
            .filter(|f| f.start < sec_end)
            .map_or("the end of the section".to_string(), |f| img.func_name(f).to_string()),
        command(img, "fn", &format!("{a:#x} --to {:#x} --code", next.min(a + 0x100))),
        command(img, "bytes", &format!("{a:#x} {}", (next - a).min(0x100)))
    ))
}

/// The function a known place stands for: the one at `main` or the entry, or the stub that jumps through a slot.
fn known_func<'a>(img: &'a Image, t: &str, k: crate::place::Known) -> Result<&'a crate::funcs::Func> {
    use crate::place::Known;
    match k {
        Known::Main(a) | Known::Entry(a) => func_at_addr(img, a),
        Known::Slot(i, slot) => crate::place::stub_of(img, i).ok_or_else(|| {
            let imp = &img.imports[i];
            let sec = img.section_at(slot).map_or(String::new(), |s| format!(" in {}", s.name));
            Error::Bad(format!(
                "{t} is the slot at {slot:#x}{sec} that the loader fills with the address of {}; it holds data, not \
                 code, and no stub in this file jumps through it; its bytes: {}; who reads it: {}",
                imp.name,
                command(img, "bytes", &format!("{t} --as ptr")),
                command(img, "xref", t)
            ))
        }),
    }
}

/// A function by its name, part of it, or a section name (the function at its start).
fn resolve_name<'a>(img: &'a Image, t: &str) -> Result<&'a crate::funcs::Func> {
    let funcs = img.funcs();
    if let Some(f) = funcs.iter().find(|f| img.func_name(f) == t || f.name == t) {
        return Ok(f);
    }
    if let Some(f) = img.addr_of_raw(t).and_then(|a| img.func_at(a)) {
        return Ok(f);
    }
    // before the names that only contain `t`: in a stripped file `main` is otherwise `__libc_start_main@plt`
    if let Some(k) = crate::place::known_place(img, t) {
        return known_func(img, t, k);
    }
    let raw_has = |f: &crate::funcs::Func| img.raw_name_at(f.start).is_some_and(|r| r.contains(t));
    let mut hits: Vec<_> =
        funcs.iter().filter(|f| img.func_name(f).contains(t) || f.name.contains(t) || raw_has(f)).collect();
    // a signature written with other spacing (`const&` for `const &`) still finds the function
    if hits.is_empty() && t.contains('(') {
        let want = squash(t);
        hits = funcs.iter().filter(|f| squash(img.func_name(f)).contains(&want)).collect();
    }
    match hits.as_slice() {
        [one] => Ok(one),
        // `ns::f` names the function itself: its `.cold` part, its lambdas and the templates that only mention
        // it do not count when exactly one function carries that name
        many if many.len() > 1 && hot_exact(many, img, t).len() == 1 => Ok(hot_exact(many, img, t)[0]),
        [] => {
            if let Some(s) = img.sections.iter().find(|s| s.name == t) {
                return func_at_addr(img, s.addr);
            }
            // a name that starts with `.` is a section name: say which sections the file has
            if t.starts_with('.') && t.len() > 1 {
                let have: Vec<&str> =
                    img.sections.iter().filter(|s| s.alloc && !s.name.is_empty()).map(|s| s.name.as_str()).collect();
                if !have.is_empty() {
                    return Err(Error::Bad(format!(
                        "no section {t:?} in this file; its loaded sections: {}",
                        have.join(" ")
                    )));
                }
            }
            let hint = if t.len() >= 3 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                format!("; for an address write 0x{t}")
            } else if img.imports.iter().any(|i| i.name == t) {
                format!("; {t} is an import, its code is not in this file (who uses it: nexo bin xref)")
            } else {
                String::new()
            };
            let close = crate::place::close_note(img, t);
            let next = if hint.is_empty() && close.is_empty() {
                format!("; find it with: {}", command(img, "funcs", &format!("--grep {t}")))
            } else {
                String::new()
            };
            Err(Error::Bad(format!("no function is named {t:?}{hint}{close}{next}; {}", crate::place::ADDR_FORMS)))
        }
        many => {
            let exact = hot_exact(many, img, t);
            let many = if exact.len() > 1 { &exact[..] } else { many };
            let list: Vec<String> =
                many.iter().take(5).map(|f| format!("{} ({:#x})", img.func_name(f), f.start)).collect();
            let more = if many.len() > 5 { format!(" and {} more", many.len() - 5) } else { String::new() };
            Err(Error::Bad(format!("{} functions match {t:?}: {}{more}; use an address", many.len(), list.join(", "))))
        }
    }
}

/// The whole function that holds the address (or has the name), one line per instruction. An address
/// inside the function opens the page a few instructions before it (see [`crate::fnview`]).
pub fn func(img: &Image, target: &str, with_bytes: bool) -> Result<Output> {
    func_window(img, target, with_bytes, None, usize::MAX)
}

/// Like [`func`], but only the rows a page of `budget` lines starting at `from` can show are built.
/// The rest are counted (`Output::tail`), so the "more lines" figure stays exact.
pub fn func_window(img: &Image, target: &str, with_bytes: bool, from: Option<u64>, budget: usize) -> Result<Output> {
    crate::fnview::func_view(
        img,
        &crate::fnview::FnQuery { with_bytes, from, budget, ..crate::fnview::FnQuery::new(target) },
    )
}

/// `len` bytes at `addr` read as the type `ty` (see [`crate::typeview`]): the packed table for a scalar, one row per
/// field for a struct.
pub fn bytes(img: &Image, addr: u64, len: u64, ty: &str) -> Result<Output> {
    let view = crate::typeview::resolve(img, ty)?;
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
    let mut rows = view.rows(data, addr, &|v| describe(img, v, false));
    if (data.len() as u64) < len {
        let a = addr + data.len() as u64;
        rows.push(Row {
            key: a,
            text: format!("... the file holds no more bytes after {a:#x} here ({} of {len} read)", data.len()),
            json: None,
        });
    }
    let as_ = view.name();
    let meta = json!({"addr": addr, "len": data.len(), "as": as_});
    Ok(Output {
        op: "bytes",
        meta,
        rows,
        tail: 0,
        command: command(img, "bytes", &format!("{addr:#x} {len} --as {}", shell_word(&as_))),
    })
}

/// How `strings` orders its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrSort {
    /// By address, lowest first.
    Addr,
    /// Most references first.
    Refs,
}

impl StrSort {
    pub const NAMES: [&'static str; 2] = ["addr", "refs"];

    pub fn parse(s: &str) -> Result<StrSort> {
        match s {
            "addr" => Ok(StrSort::Addr),
            "refs" => Ok(StrSort::Refs),
            _ => Err(Error::Bad(format!("{s:?} is not a sort; use addr or refs"))),
        }
    }
}

/// Strings (ASCII and UTF-16LE) whose text matches the regex; all of them without one.
pub fn strings(img: &Image, grep: Option<&str>) -> Result<Output> {
    strings_window(img, grep, StrSort::Addr, None, usize::MAX)
}

/// Like [`strings`], with an order and only the rows of one page built. Sorted by address the rows are
/// keyed by address; sorted by references, by position in the list.
pub fn strings_window(
    img: &Image,
    grep: Option<&str>,
    sort: StrSort,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let re = grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let all = img.strings();
    let refs = img.refs();
    let count = |s: &crate::strings::Str| refs.within(s.addr, s.byte_len).len();
    let mut list: Vec<(usize, usize)> = all
        .iter()
        .enumerate()
        .filter(|(_, s)| re.as_ref().is_none_or(|r| r.is_match(&s.text)))
        .map(|(i, s)| (i, count(s)))
        .collect();
    if sort == StrSort::Refs {
        list.sort_by_key(|&(i, n)| (std::cmp::Reverse(n), i));
    }
    let matched = list.len();
    let budget = budget.max(MIN_LINES);
    let (start, end) = match (sort, from) {
        (StrSort::Addr, Some(k)) => {
            let s = list.partition_point(|&(i, _)| all[i].addr < k);
            (s, s.saturating_add(budget).min(list.len()))
        }
        _ => crate::page::window(from, budget, list.len()),
    };
    let mut rows: Vec<Row> = list[start..end]
        .iter()
        .enumerate()
        .map(|(n, &(i, refs))| {
            let s = &all[i];
            Row {
                key: if sort == StrSort::Addr { s.addr } else { (start + n) as u64 },
                text: format!(
                    "{:#010x}  {refs:>4}  {}\"{}\"",
                    s.addr,
                    if s.wide { "L" } else { "" },
                    shorten(&s.text, 200)
                ),
                json: Some(json!({"addr": s.addr, "wide": s.wide, "chars": s.chars, "text": s.text, "refs": refs})),
            }
        })
        .collect();
    let tail = list.len() - end;
    if rows.is_empty() && matched == 0 {
        rows.push(Row { key: 0, text: format!("no string matches (of {} found)", all.len()), json: None });
    }
    let mut rest = String::new();
    if sort == StrSort::Refs {
        rest.push_str("--sort refs");
    }
    if let Some(g) = grep {
        rest.push_str(&format!("{}--grep {}", if rest.is_empty() { "" } else { " " }, shell_word(g)));
    }
    Ok(Output {
        op: "strings",
        meta: json!({"total": all.len(), "matched": matched, "grep": grep,
            "sort": if sort == StrSort::Refs { "refs" } else { "addr" }}),
        rows,
        tail,
        command: command(img, "strings", &rest),
    })
}

/// A hex pattern: bytes written with or without spaces, `0x` or commas; `??` matches any byte.
pub fn parse_pattern(s: &str) -> Result<Vec<Option<u8>>> {
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

pub(crate) fn hit_row(img: &Image, addr: u64, what: &str, kind: &str) -> Row {
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

/// The part of a section's bytes inside `within` (all of it without one), and the address it starts at.
pub(crate) fn clipped(sec_addr: u64, data: &[u8], within: Option<(u64, u64)>) -> (u64, &[u8]) {
    let Some((lo, hi)) = within else { return (sec_addr, data) };
    let from = lo.saturating_sub(sec_addr).min(data.len() as u64) as usize;
    let to = hi.saturating_sub(sec_addr).min(data.len() as u64) as usize;
    (sec_addr + from as u64, &data[from..to.max(from)])
}

/// What `--in RANGE` adds to the command that asked.
pub(crate) fn in_text(within: Option<&str>) -> String {
    within.map_or(String::new(), |w| format!(" --in {}", shell_word(w)))
}

/// Every place the byte pattern occurs in the mapped sections.
pub fn find_bytes(img: &Image, pattern: &str) -> Result<Output> {
    find_bytes_in(img, pattern, None)
}

/// Like [`find_bytes`], within a range written in any form of [`crate::place::RangeExpr`]: a match lies wholly
/// inside it.
pub fn find_bytes_in(img: &Image, pattern: &str, within: Option<&str>) -> Result<Output> {
    let pat = parse_pattern(pattern)?;
    let span = within.map(|w| crate::place::range_in(img, w)).transpose()?;
    let mut rows = Vec::new();
    let mut capped = false;
    'sec: for sec in img.sections.iter().filter(|s| s.alloc && s.file_size > 0) {
        let (base, data) = clipped(sec.addr, img.section_bytes(sec), span);
        if data.len() < pat.len() {
            continue;
        }
        for i in 0..=data.len() - pat.len() {
            if pat.iter().zip(&data[i..]).all(|(p, b)| p.is_none_or(|p| p == *b)) {
                rows.push(hit_row(img, base + i as u64, &hex(&data[i..i + pat.len()]), "bytes"));
                if rows.len() >= MAX_HITS {
                    capped = true;
                    break 'sec;
                }
            }
        }
    }
    let shown: Vec<String> = pat.iter().map(|p| p.map_or("??".to_string(), |b| format!("{b:02x}"))).collect();
    let rest = format!("{}{}", shell_word(&shown.join(" ")), in_text(within));
    let mut meta = json!({"pattern": shown.join(" ")});
    if let Some((lo, hi)) = span {
        meta["in"] = json!([lo, hi]);
    }
    Ok(finish_hits(img, "find", rows, rest, meta, capped))
}

/// Every function found, plus the part of each code section before its first function (or all of it when no
/// function was found there): the pieces of code that are read from their first byte.
pub(crate) fn code_ranges(img: &Image) -> Vec<(u64, u64)> {
    let funcs = img.funcs();
    let mut ranges: Vec<(u64, u64)> = funcs.iter().map(|f| (f.start, f.end)).collect();
    for sec in img.code_sections() {
        let first = funcs.iter().map(|f| f.start).filter(|&a| sec.contains(a)).min();
        let head_end = first.unwrap_or(sec.addr.saturating_add(sec.backed_len()));
        if head_end > sec.addr {
            ranges.push((sec.addr, head_end));
        }
    }
    ranges
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

/// Every instruction with the number as an immediate or as the address written inside a memory
/// operand (`[rdx+0x429a80]`, kind `disp` in JSON), and every place the data sections hold it as a
/// 4-byte (or, for a larger number, 8-byte) little-endian value.
pub fn find_imm(img: &Image, value: i128) -> Result<Output> {
    find_imm_in(img, value, None)
}

/// Like [`find_imm`], within a range written in any form of [`crate::place::RangeExpr`].
pub fn find_imm_in(img: &Image, value: i128, within: Option<&str>) -> Result<Output> {
    let span = within.map(|w| crate::place::range_in(img, w)).transpose()?;
    if img.arm64 {
        img.require_x86("find-imm")?;
    }
    if value < i128::from(i64::MIN) || value > i128::from(u64::MAX) {
        return Err(Error::Bad("the number does not fit in 64 bits".into()));
    }
    let mut rows = Vec::new();
    let mut capped = false;
    let mut fmt = formatter();
    let mut line = String::new();
    let ranges = code_ranges(img);
    'code: for (start, end) in ranges {
        let Some(code) = img.read_at(start, (end - start) as usize) else { continue };
        let mut full = false;
        walk_bits(img.bits, code, start, |ins, _| {
            if full || span.is_some_and(|(lo, hi)| ins.ip() < lo || ins.ip() >= hi) {
                return;
            }
            let what = if imm_matches(ins, value) {
                "imm"
            } else if u64::try_from(value).is_ok_and(|v| crate::refs::written_address(ins).is_some_and(|(a, _)| a == v))
            {
                "disp"
            } else {
                return;
            };
            line.clear();
            fmt.format(ins, &mut line);
            rows.push(hit_row(img, ins.ip(), &line, what));
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
    // bare code is one section: the bytes outside its functions are its data
    let raw = img.format == crate::Format::Raw;
    if !capped {
        'data: for sec in img.sections.iter().filter(|s| s.alloc && (!s.exec || raw) && s.file_size > 0) {
            let (base, data) = clipped(sec.addr, img.section_bytes(sec), span);
            if data.len() < needle.len() {
                continue;
            }
            for i in 0..=data.len() - needle.len() {
                if data[i..i + needle.len()] == needle[..] && !(raw && img.func_at(base + i as u64).is_some()) {
                    rows.push(hit_row(
                        img,
                        base + i as u64,
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
    let rest = format!("--imm {value}{}", in_text(within));
    let mut meta = json!({"imm": value.to_string()});
    if let Some((lo, hi)) = span {
        meta["in"] = json!([lo, hi]);
    }
    Ok(finish_hits(img, "find", rows, rest, meta, capped))
}

/// A demangled name without spaces, to compare signatures written in different styles.
fn squash(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The name before its argument list: `ns::f(int) const [clone .isra.0]` is `ns::f`.
fn bare_name(name: &str) -> &str {
    name[..name.find('(').unwrap_or(name.len())].trim_end()
}

/// True when `name` is the function `target` writes: the same name with or without the argument list, and not one of
/// the pieces the compiler splits off it (`[clone .cold]`) or a lambda inside it.
fn names_the_function(target: &str, name: &str) -> bool {
    if name.contains(".cold") || name.contains("lambda") {
        return false;
    }
    if target.contains('(') {
        return squash(name).starts_with(&squash(target)) && !name.contains("::{");
    }
    bare_name(name) == target
}

/// Those of `hits` that are the function `target` names, as above.
fn hot_exact<'a>(hits: &[&'a crate::funcs::Func], img: &Image, target: &str) -> Vec<&'a crate::funcs::Func> {
    hits.iter()
        .copied()
        .filter(|f| names_the_function(target, img.func_name(f)) || names_the_function(target, &f.name))
        .collect()
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn a_name_without_arguments_finds_the_function_itself() {
        let hot = "LCRYPT::loadFileAsBits(std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> > const&)";
        assert!(names_the_function("LCRYPT::loadFileAsBits", hot));
        // the compiler's clone of the same function is still the function; its cold half is not
        assert!(names_the_function("LCRYPT::loadFileAsBits", &format!("{hot} [clone .isra.0]")));
        assert!(!names_the_function("LCRYPT::loadFileAsBits", &format!("{hot} [clone .isra.0] [clone .cold]")));
        assert!(!names_the_function(
            "LCRYPT::processBytes",
            "LCRYPT::processBytes(int)::{lambda(unsigned long)#1}::operator()(unsigned long) const"
        ));
        assert!(!names_the_function(
            "LCRYPT::processBytes",
            "std::_Sp_counted_ptr_inplace<LCRYPT::processBytes(int)>::_M_dispose()"
        ));
        assert!(!names_the_function("LCRYPT::encrypt", "LCRYPT::encryptAll(int)"));
    }

    #[test]
    fn a_signature_is_compared_without_spaces() {
        let hot = "LCRYPT::encrypt(std::__cxx11::basic_string<char, std::char_traits<char>, std::allocator<char> > const&, int)";
        assert!(names_the_function(
            "LCRYPT::encrypt(std::__cxx11::basic_string<char,std::char_traits<char>,std::allocator<char>>const&,int)",
            hot
        ));
        assert!(!names_the_function("LCRYPT::encrypt(int)", hot));
        assert_eq!(squash("a b\tc"), "abc");
    }
}
