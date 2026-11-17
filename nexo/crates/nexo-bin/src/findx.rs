//! `find --str`, `--asm` and `--seq`: text in the file's bytes, and instruction patterns in its code. Each answer
//! shows the first `limit` matches by address and says how many more there are.

use iced_x86::Formatter;
use regex::Regex;
use serde_json::{json, Value};

use crate::disasm::{formatter, walk_bits};
use crate::ops::{clipped, code_ranges, command, hex, hit_row, in_text};
use crate::page::{shell_word, Output, Row};
use crate::strings::shorten;
use crate::{Error, Image, Result};

/// Matches shown unless the caller says otherwise.
pub const DEFAULT_LIMIT: usize = 200;
/// Matches kept for sorting; the count goes on past it.
const KEEP: usize = 100_000;
/// The longest text `--str` takes (bytes).
const MAX_TEXT: usize = 4096;
/// The most instructions in one `--seq`.
const MAX_SEQ: usize = 16;

/// What a scan found: the rows kept and every match counted.
#[derive(Default)]
struct Hits {
    rows: Vec<Row>,
    total: usize,
}

impl Hits {
    fn add(&mut self, row: impl FnOnce() -> Row) {
        self.total += 1;
        if self.rows.len() < KEEP {
            self.rows.push(row());
        }
    }
}

fn check_limit(limit: usize) -> Result<()> {
    if limit == 0 {
        return Err(Error::Bad("--limit must be at least 1".into()));
    }
    Ok(())
}

/// The first `limit` rows by address, then a line that says how many matches there are beyond them.
fn finish(img: &Image, mut h: Hits, limit: usize, rest: String, mut meta: Value) -> Output {
    let exact = h.total == h.rows.len();
    h.rows.sort_by_key(|r| r.key);
    h.rows.dedup_by_key(|r| r.key);
    if exact {
        h.total = h.rows.len();
    }
    let shown = h.rows.len().min(limit);
    let beyond = h.total.saturating_sub(shown);
    h.rows.truncate(limit);
    meta["hits"] = json!(shown);
    meta["total"] = json!(h.total);
    meta["limit"] = json!(limit);
    meta["beyond"] = json!(beyond);
    meta["capped"] = json!(beyond > 0);
    let mut rows = h.rows;
    if beyond > 0 {
        let key = rows.last().map_or(0, |r| r.key);
        rows.push(Row {
            key,
            text: format!(
                "... {beyond} more matches beyond --limit {limit} ({} in all); raise --limit or narrow with --in",
                h.total
            ),
            json: None,
        });
    } else if rows.is_empty() {
        rows.push(Row { key: 0, text: "no match in the mapped sections".into(), json: None });
    }
    let rest = if limit == DEFAULT_LIMIT { rest } else { format!("{rest} --limit {limit}") };
    Output { op: "find", meta, rows, tail: 0, command: command(img, "find", &rest) }
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Every place the file holds `text` (as UTF-8 bytes, and with `wide` as UTF-16LE too), with the section and, in code,
/// the function.
pub fn find_str_in(img: &Image, text: &str, wide: bool, within: Option<&str>, limit: usize) -> Result<Output> {
    check_limit(limit)?;
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(Error::Bad(format!("--str takes 1 to {MAX_TEXT} bytes of text")));
    }
    let span = within.map(|w| crate::place::range_in(img, w)).transpose()?;
    let mut needles: Vec<(&str, Vec<u8>)> = vec![("ascii", text.as_bytes().to_vec())];
    if wide {
        needles.push(("wide", utf16le(text)));
    }
    let mut hits = Hits::default();
    for sec in img.sections.iter().filter(|s| s.alloc && s.file_size > 0) {
        let (base, data) = clipped(sec.addr, img.section_bytes(sec), span);
        for (kind, needle) in &needles {
            if data.len() < needle.len() {
                continue;
            }
            for i in 0..=data.len() - needle.len() {
                if data[i..i + needle.len()] != needle[..] {
                    continue;
                }
                let addr = base + i as u64;
                hits.add(|| {
                    let mut what = format!("{kind} \"{}\"", shorten(text, 60));
                    // a match inside a longer string: show the string
                    if let Some((s, _)) = img.string_at(addr) {
                        if s.wide == (*kind == "wide") && s.text != text {
                            what.push_str(&format!(" in \"{}\"", shorten(&s.text, 60)));
                        }
                    }
                    hit_row(img, addr, &what, kind)
                });
            }
        }
    }
    let rest = format!("--str {}{}{}", shell_word(text), if wide { " --wide" } else { "" }, in_text(within));
    let mut meta = json!({"str": text, "wide": wide});
    if let Some((lo, hi)) = span {
        meta["in"] = json!([lo, hi]);
    }
    Ok(finish(img, hits, limit, rest, meta))
}

/// One decoded instruction of a code range.
struct Ins {
    ip: u64,
    raw: Vec<u8>,
    text: String,
}

/// The instructions of every code range (functions and the code before the first one), in order, one `Vec` per
/// range; those inside `span` only matter to the caller, but a sequence may start just before it.
fn decode_ranges(img: &Image, span: Option<(u64, u64)>, mut each: impl FnMut(&[Ins])) {
    let mut fmt = formatter();
    for (start, end) in code_ranges(img) {
        if span.is_some_and(|(lo, hi)| end <= lo || start >= hi) {
            continue;
        }
        let Some(code) = img.read_at(start, (end - start) as usize) else { continue };
        let mut list: Vec<Ins> = Vec::new();
        walk_bits(img.bits, code, start, |ins, raw| {
            if ins.is_invalid() {
                return;
            }
            let mut text = String::new();
            fmt.format(ins, &mut text);
            list.push(Ins { ip: ins.ip(), raw: raw.to_vec(), text });
        });
        each(&list);
    }
}

fn inside(span: Option<(u64, u64)>, ip: u64) -> bool {
    span.is_none_or(|(lo, hi)| ip >= lo && ip < hi)
}

fn with_text(mut row: Row, raw: &[u8], text: &str) -> Row {
    if let Some(j) = row.json.as_mut() {
        j["bytes"] = json!(hex(raw));
        j["text"] = json!(text);
    }
    row
}

/// Every instruction whose disassembly (`mov eax, 0x539`) the regular expression matches, in executable code.
pub fn find_asm_in(img: &Image, pattern: &str, within: Option<&str>, limit: usize) -> Result<Output> {
    check_limit(limit)?;
    if img.arm64 {
        img.require_x86("find --asm")?;
    }
    let re = Regex::new(pattern).map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let span = within.map(|w| crate::place::range_in(img, w)).transpose()?;
    let mut hits = Hits::default();
    decode_ranges(img, span, |list| {
        for ins in list.iter().filter(|i| inside(span, i.ip) && re.is_match(&i.text)) {
            hits.add(|| {
                let what = format!("{:<24} {}", hex(&ins.raw), ins.text);
                with_text(hit_row(img, ins.ip, &what, "asm"), &ins.raw, &ins.text)
            });
        }
    });
    let rest = format!("--asm {}{}", shell_word(pattern), in_text(within));
    let mut meta = json!({"asm": pattern});
    if let Some((lo, hi)) = span {
        meta["in"] = json!([lo, hi]);
    }
    Ok(finish(img, hits, limit, rest, meta))
}

/// The text of an instruction as it is compared: lower case, one space between words, none around commas.
fn norm(text: &str) -> String {
    text.to_ascii_lowercase()
        .split(',')
        .map(|part| part.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `text` is `pat` with each `*` standing for any text (also none). Both are already normalised.
fn glob(pat: &str, text: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pat == text;
    }
    if !text.starts_with(first) {
        return false;
    }
    let mut pos = first.len();
    for mid in &parts[1..parts.len() - 1] {
        match text[pos..].find(mid) {
            Some(i) => pos += i + mid.len(),
            None => return false,
        }
    }
    text.len() >= pos + last.len() && text.ends_with(last)
}

/// The instruction patterns of a `--seq`: split at `;` or a new line, `*` is any operand text.
fn parse_seq(seq: &str) -> Result<Vec<String>> {
    let pats: Vec<String> = seq.split([';', '\n']).map(norm).filter(|p| !p.is_empty()).collect();
    if pats.is_empty() {
        return Err(Error::Bad("--seq needs instruction patterns, like \"mov eax, *; call *\"".into()));
    }
    if pats.len() > MAX_SEQ {
        return Err(Error::Bad(format!("--seq takes at most {MAX_SEQ} instructions")));
    }
    Ok(pats)
}

/// Every place where consecutive instructions match the patterns of `seq` (`*` stands for any operand text, the
/// match is on the text the disassembler prints), one row at the first instruction.
pub fn find_seq_in(img: &Image, seq: &str, within: Option<&str>, limit: usize) -> Result<Output> {
    check_limit(limit)?;
    if img.arm64 {
        img.require_x86("find --seq")?;
    }
    let pats = parse_seq(seq)?;
    let span = within.map(|w| crate::place::range_in(img, w)).transpose()?;
    let mut hits = Hits::default();
    decode_ranges(img, span, |list| {
        if list.len() < pats.len() {
            return;
        }
        for win in list.windows(pats.len()) {
            if !inside(span, win[0].ip) || !win.iter().zip(&pats).all(|(i, p)| glob(p, &norm(&i.text))) {
                continue;
            }
            hits.add(|| {
                let raw: Vec<u8> = win.iter().flat_map(|i| i.raw.iter().copied()).collect();
                let text = win.iter().map(|i| i.text.as_str()).collect::<Vec<_>>().join(" ; ");
                let what = format!("{:<24} {text}", hex(&raw));
                with_text(hit_row(img, win[0].ip, &what, "seq"), &raw, &text)
            });
        }
    });
    let rest = format!("--seq {}{}", shell_word(seq), in_text(within));
    let mut meta = json!({"seq": pats});
    if let Some((lo, hi)) = span {
        meta["in"] = json!([lo, hi]);
    }
    Ok(finish(img, hits, limit, rest, meta))
}
