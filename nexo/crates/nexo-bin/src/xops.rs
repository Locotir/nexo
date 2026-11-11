//! Who uses what: `xref`, `funcs`, `calls`, and the names and notes that stay with the file.
//!
//! Each op makes a short list of small items (a reference, a function index), cuts it to the window
//! a page needs, and only then formats the lines. The text of an instruction is read for the lines
//! of the page alone.

use std::collections::HashMap;
use std::path::Path;

use iced_x86::IntelFormatter;
use serde_json::{json, Value};

use crate::disasm::{describe, formatter, text_at};
use crate::names;
use crate::ops::{command, numeric, resolve};
use crate::page::{cut, shell_word, Output, Row};
use crate::refs::{Kind, Ref, MAX_REFS};
use crate::strings::shorten;
use crate::{parse_addr, Error, Image, Result};

/// The most strings `xref --str` follows.
pub const MAX_STR_HITS: usize = 2000;
/// The most places of one string `xref --str` narrows the references to.
const MAX_OCCURRENCES: usize = 64;
/// Longest text of a string in a heading (characters).
const SHOWN_STRING: usize = 80;

/// A line of an answer with no JSON of its own.
fn text_row(t: impl Into<String>) -> Row {
    Row { key: 0, text: t.into(), json: None }
}

/// The name of the thing at `a`: an import, a function (with an offset when inside one), or what
/// `describe` finds.
fn name_of(img: &Image, a: u64) -> String {
    if let Some(&i) = img.slots.get(&a) {
        return img.imports[i].name.clone();
    }
    describe(img, a, false).unwrap_or_else(|| format!("{a:#x}"))
}

/// Where an address is, in a few words: `main+0x10` in code, the symbol or `.data+0x20` elsewhere.
fn place(img: &Image, a: u64) -> String {
    if img.is_exec(a) {
        if let Some(l) = img.label(a) {
            return l;
        }
    }
    describe(img, a, true).unwrap_or_else(|| format!("{a:#x}"))
}

/// The address a person means: a number inside a section, or the name of a function, a symbol or an import.
pub(crate) fn target_addr(img: &Image, t: &str) -> Result<u64> {
    // the references first: one decode pass gives them and the function list
    let _ = img.refs();
    let t = t.trim();
    if t.is_empty() {
        return Err(Error::Bad("give an address (0x401000) or a name".into()));
    }
    if numeric(t) {
        let a = parse_addr(t)?;
        if img.section_at(a).is_none() {
            return Err(Error::Bad(format!("{a:#x} is not inside any section")));
        }
        return Ok(a);
    }
    if let Some((&a, _)) = img.user.names.iter().find(|(_, n)| n.as_str() == t) {
        return Ok(a);
    }
    if let Some(f) = img.funcs().iter().find(|f| f.name == t) {
        return Ok(f.start);
    }
    if let Some(slot) = img.imports.iter().find(|i| i.name == t).and_then(|i| i.slot) {
        return Ok(slot);
    }
    if let Some(s) = img.symbols.iter().find(|s| s.name == t) {
        return Ok(s.addr);
    }
    if let Some(e) = img.exports.iter().find(|e| e.name == t) {
        return Ok(e.addr);
    }
    resolve(img, t).map(|f| f.start)
}

/// What a target is, for the heading.
fn target_desc(img: &Image, t: u64) -> String {
    if let Some(f) = img.func_at(t) {
        let name = img.func_name(f);
        return if f.start == t {
            format!("function {name}")
        } else {
            format!("inside function {name} at +{:#x}", t - f.start)
        };
    }
    if let Some(&i) = img.slots.get(&t) {
        return format!("import {}", img.imports[i].name);
    }
    describe(img, t, true).unwrap_or_else(|| format!("{t:#x}"))
}

fn note_suffix(img: &Image, a: u64) -> String {
    img.note_at(a).map(|n| format!("  ; note: {n}")).unwrap_or_default()
}

/// One reference as a line: the source, the kind, where it sits, the instruction for code.
fn ref_row(img: &Image, fmt: &mut IntelFormatter, r: &Ref, string: Option<u64>) -> Row {
    let at = place(img, r.src);
    let code = r.kind != Kind::Ptr && img.is_exec(r.src);
    let insn = if code { text_at(img, fmt, r.src) } else { None };
    let mut text = format!("  {:#010x}  {:<5} {at}", r.src, r.kind.word());
    if let Some(i) = &insn {
        text.push_str("  ");
        text.push_str(i);
    }
    text.push_str(&note_suffix(img, r.src));
    let holder = img.func_at(r.src);
    let mut j = json!({
        "addr": r.src, "kind": r.kind.word(), "target": r.target, "where": at,
        "function": holder.map(|f| img.func_name(f)), "offset": holder.map(|f| r.src - f.start),
        "section": img.section_at(r.src).map(|s| s.name.as_str()), "text": insn,
        "note": img.note_at(r.src),
    });
    if let Some(s) = string {
        j["string"] = json!(s);
    }
    Row { key: 0, text, json: Some(j) }
}

enum XItem {
    Head(String),
    Title(String),
    Ref(Ref),
    Str { at: usize, refs: usize },
    Ref2(Ref, u64),
    Empty(String),
}

fn counts_json(list: &[Ref]) -> Value {
    let mut m = serde_json::Map::new();
    for k in Kind::ALL {
        m.insert(k.word().into(), json!(list.iter().filter(|r| r.kind == k).count()));
    }
    Value::Object(m)
}

fn capped_note(img: &Image) -> String {
    if img.refs().capped {
        format!("  ; the list was cut at {MAX_REFS} references, some are missing")
    } else {
        String::new()
    }
}

/// Everything that calls, jumps to, reads, writes, takes the address of, or points at `target`.
pub fn xref(img: &Image, target: &str) -> Result<Output> {
    xref_window(img, target, None, usize::MAX)
}

/// Like [`xref`], but only the lines of one page are formatted.
pub fn xref_window(img: &Image, target: &str, from: Option<u64>, budget: usize) -> Result<Output> {
    let _ = img.refs();
    let t = target_addr(img, target)?;
    let refs = img.refs();
    let list = refs.at(t);
    let word = if list.len() == 1 { "reference" } else { "references" };
    let head = format!(
        "xref {t:#x}  {}  ; {} {word}{}{}",
        target_desc(img, t),
        list.len(),
        capped_note(img),
        note_suffix(img, t)
    );
    let mut items = vec![XItem::Head(head)];
    for k in Kind::ALL {
        let n = list.iter().filter(|r| r.kind == k).count();
        if n > 0 {
            items.push(XItem::Title(format!("{} ({n})", k.title())));
            items.extend(list.iter().filter(|r| r.kind == k).map(|r| XItem::Ref(*r)));
        }
    }
    if list.is_empty() {
        items.push(XItem::Empty(format!("no reference to {t:#x} was found")));
    }
    let mut fmt = formatter();
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        XItem::Head(s) | XItem::Title(s) | XItem::Empty(s) => text_row(s.clone()),
        XItem::Ref(r) => ref_row(img, &mut fmt, r, None),
        _ => text_row(""),
    });
    let holder = img.func_at(t);
    let meta = json!({
        "target": t, "description": target_desc(img, t), "references": list.len(), "counts": counts_json(list),
        "capped": refs.capped, "function": holder.map(|f| img.func_name(f)), "note": img.note_at(t),
    });
    Ok(Output { op: "xref", meta, rows, tail, command: command(img, "xref", &shell_word(target.trim())) })
}

/// The strings that contain `text`, then the references to each.
pub fn xref_str(img: &Image, text: &str, insensitive: bool) -> Result<Output> {
    xref_str_window(img, text, insensitive, None, usize::MAX)
}

/// Like [`xref_str`], but only the lines of one page are formatted.
pub fn xref_str_window(img: &Image, text: &str, insensitive: bool, from: Option<u64>, budget: usize) -> Result<Output> {
    let _ = img.refs();
    if text.is_empty() {
        return Err(Error::Bad("give some text to look for in the strings".into()));
    }
    let needle = if insensitive { text.to_ascii_lowercase() } else { text.to_string() };
    let refs = img.refs();
    let all = img.strings();
    let mut hits: Vec<usize> = Vec::new();
    let mut capped = false;
    let mut total_refs = 0usize;
    // the references of each string, narrowed to the literal that holds the match
    let mut per: Vec<Vec<Ref>> = Vec::new();
    for (i, s) in all.iter().enumerate() {
        let hay = if insensitive { s.text.to_ascii_lowercase() } else { s.text.clone() };
        let found: Vec<usize> = hay.match_indices(&needle).map(|(p, _)| p).take(MAX_OCCURRENCES).collect();
        if found.is_empty() {
            continue;
        }
        if hits.len() >= MAX_STR_HITS {
            capped = true;
            break;
        }
        let inside = refs.within(s.addr, s.byte_len);
        let mut mine: Vec<Ref> = Vec::new();
        for p in found {
            let (off, len) = if s.wide {
                (hay[..p].chars().count() as u64 * 2, needle.chars().count() as u64 * 2)
            } else {
                (p as u64, needle.len() as u64)
            };
            let a = s.addr.saturating_add(off);
            let b = a.saturating_add(len);
            let before = inside.partition_point(|r| r.target <= a);
            let lo = if before > 0 { inside[before - 1].target } else { a };
            let x = inside.partition_point(|r| r.target < lo);
            let y = inside.partition_point(|r| r.target < b);
            mine.extend_from_slice(&inside[x..y.max(x)]);
        }
        mine.sort_unstable_by_key(|r| (r.src, r.target, r.kind));
        mine.dedup();
        total_refs += mine.len();
        hits.push(i);
        per.push(mine);
    }
    let mut head = format!("xref --str {}  ; {} strings, {total_refs} references", shorten(text, 60), hits.len());
    if insensitive {
        head.push_str("  ; ignoring case");
    }
    if capped {
        head.push_str(&format!("  ; stopped at {MAX_STR_HITS} strings, add text to narrow it"));
    }
    head.push_str(&capped_note(img));
    let mut items = vec![XItem::Head(head)];
    if hits.is_empty() {
        items.push(XItem::Empty(format!("no string contains {:?} (of {} found)", shorten(text, 60), all.len())));
    }
    for (at, mine) in hits.iter().zip(&per) {
        items.push(XItem::Str { at: *at, refs: mine.len() });
        let s_addr = all[*at].addr;
        items.extend(mine.iter().map(|r| XItem::Ref2(*r, s_addr)));
    }
    let mut fmt = formatter();
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        XItem::Head(s) | XItem::Title(s) | XItem::Empty(s) => text_row(s.clone()),
        XItem::Str { at, refs } => {
            let s = &all[*at];
            let w = if s.wide { "L" } else { "" };
            let n = if *refs == 1 { "reference" } else { "references" };
            Row {
                key: 0,
                text: format!("string {:#010x} {w}\"{}\"  ({refs} {n})", s.addr, shorten(&s.text, SHOWN_STRING)),
                json: Some(
                    json!({"item": "string", "addr": s.addr, "wide": s.wide, "text": s.text, "references": refs}),
                ),
            }
        }
        XItem::Ref2(r, s) => ref_row(img, &mut fmt, r, Some(*s)),
        XItem::Ref(r) => ref_row(img, &mut fmt, r, None),
    });
    let meta = json!({"text": text, "ignore_case": insensitive, "strings": hits.len(), "references": total_refs,
        "capped": capped || refs.capped});
    let rest = format!("--str {}{}", shell_word(text), if insensitive { " --i" } else { "" });
    Ok(Output { op: "xref", meta, rows, tail, command: command(img, "xref", &rest) })
}

/// How `funcs` orders its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuncSort {
    Addr,
    Size,
    Callers,
    Callees,
}

impl FuncSort {
    pub const NAMES: [&'static str; 4] = ["addr", "size", "callers", "callees"];

    pub fn parse(s: &str) -> Result<FuncSort> {
        match s {
            "addr" => Ok(FuncSort::Addr),
            "size" => Ok(FuncSort::Size),
            "callers" => Ok(FuncSort::Callers),
            "callees" => Ok(FuncSort::Callees),
            _ => Err(Error::Bad(format!("{s:?} is not a sort; use addr, size, callers or callees"))),
        }
    }

    pub fn word(self) -> &'static str {
        FuncSort::NAMES[self as usize]
    }
}

/// The distinct functions, imports and addresses a function calls or jumps to.
fn callee_count(img: &Image, start: u64, end: u64) -> usize {
    let mut t: Vec<u64> =
        img.refs().from(start, end).filter(|r| matches!(r.kind, Kind::Call | Kind::Jump)).map(|r| r.target).collect();
    t.sort_unstable();
    t.dedup();
    t.len()
}

/// The functions found, with their callers and callees.
pub fn func_list(img: &Image, sort: FuncSort, grep: Option<&str>, min_size: u64) -> Result<Output> {
    func_list_window(img, sort, grep, min_size, None, usize::MAX)
}

/// Like [`func_list`], but only the lines of one page are formatted.
pub fn func_list_window(
    img: &Image,
    sort: FuncSort,
    grep: Option<&str>,
    min_size: u64,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    let re = grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let _ = img.refs();
    let funcs = img.funcs();
    // (function index, callers, callees)
    let mut list: Vec<(usize, usize, usize)> = funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| f.end.saturating_sub(f.start) >= min_size)
        .filter(|(_, f)| re.as_ref().is_none_or(|r| r.is_match(img.func_name(f)) || r.is_match(&f.name)))
        .map(|(i, f)| (i, img.refs().callers(f.start), callee_count(img, f.start, f.end)))
        .collect();
    let size = |i: usize| funcs[i].end.saturating_sub(funcs[i].start);
    match sort {
        FuncSort::Addr => {}
        FuncSort::Size => list.sort_by_key(|&(i, _, _)| (std::cmp::Reverse(size(i)), i)),
        FuncSort::Callers => list.sort_by_key(|&(i, c, _)| (std::cmp::Reverse(c), i)),
        FuncSort::Callees => list.sort_by_key(|&(i, _, c)| (std::cmp::Reverse(c), i)),
    }
    let matched = list.len();
    enum L {
        Head(String),
        Func(usize, usize, usize),
        Empty(String),
    }
    let mut items: Vec<L> =
        vec![L::Head(format!("{:<10}  {:>8}  {:>7}  {:>7}  name", "addr", "size", "callers", "callees"))];
    if list.is_empty() {
        items.push(L::Empty(format!("no function matches (of {} found)", funcs.len())));
    }
    items.extend(list.iter().map(|&(i, c, e)| L::Func(i, c, e)));
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        L::Head(s) | L::Empty(s) => text_row(s.clone()),
        L::Func(i, callers, callees) => {
            let f = &funcs[*i];
            let name = img.func_name(f);
            Row {
                key: 0,
                text: format!(
                    "{:#010x}  {:>8}  {callers:>7}  {callees:>7}  {name}{}",
                    f.start,
                    size(*i),
                    note_suffix(img, f.start)
                ),
                json: Some(json!({"addr": f.start, "end": f.end, "size": size(*i), "callers": callers,
                    "callees": callees, "name": name, "note": img.note_at(f.start)})),
            }
        }
    });
    let mut rest = String::new();
    if sort != FuncSort::Callers {
        rest.push_str(&format!("--sort {}", sort.word()));
    }
    if let Some(g) = grep {
        rest.push_str(&format!("{}--grep {}", if rest.is_empty() { "" } else { " " }, shell_word(g)));
    }
    if min_size > 0 {
        rest.push_str(&format!("{}--min-size {min_size}", if rest.is_empty() { "" } else { " " }));
    }
    let meta =
        json!({"total": funcs.len(), "matched": matched, "sort": sort.word(), "grep": grep, "min_size": min_size});
    Ok(Output { op: "funcs", meta, rows, tail, command: command(img, "funcs", &rest) })
}

/// Who calls a function and what it calls, with the number of call sites.
pub fn calls(img: &Image, target: &str) -> Result<Output> {
    calls_window(img, target, None, usize::MAX)
}

/// Like [`calls`], but only the lines of one page are formatted.
pub fn calls_window(img: &Image, target: &str, from: Option<u64>, budget: usize) -> Result<Output> {
    let _ = img.refs();
    let f = resolve(img, target)?;
    let refs = img.refs();
    let is_call = |r: &&Ref| matches!(r.kind, Kind::Call | Kind::Jump);
    // callers: by the function that holds the call site (the site itself when none does)
    let mut callers: HashMap<u64, usize> = HashMap::new();
    for r in refs.at(f.start).iter().filter(is_call) {
        let key = img.func_at(r.src).map_or(r.src, |h| h.start);
        *callers.entry(key).or_insert(0) += 1;
    }
    let mut callees: HashMap<u64, usize> = HashMap::new();
    for r in refs.from(f.start, f.end).filter(|r| is_call(r)) {
        *callees.entry(r.target).or_insert(0) += 1;
    }
    let order = |m: HashMap<u64, usize>| -> Vec<(u64, usize)> {
        let mut v: Vec<(u64, usize)> = m.into_iter().collect();
        v.sort_by_key(|&(a, n)| (std::cmp::Reverse(n), a));
        v
    };
    let (callers, callees) = (order(callers), order(callees));
    let sites = |v: &[(u64, usize)]| v.iter().map(|x| x.1).sum::<usize>();
    let pointers = refs.at(f.start).iter().filter(|r| r.kind == Kind::Ptr).count();
    enum L {
        Head(String),
        Title(String),
        Caller(u64, usize),
        Callee(u64, usize),
        None,
    }
    let mut items = vec![L::Head(format!(
        "calls {} {:#x}..{:#x}  ; {} callers ({} call sites), {} callees{}{}",
        img.func_name(f),
        f.start,
        f.end,
        callers.len(),
        sites(&callers),
        callees.len(),
        capped_note(img),
        note_suffix(img, f.start)
    ))];
    items.push(L::Title(format!("callers ({})", callers.len())));
    items.extend(callers.iter().map(|&(a, n)| L::Caller(a, n)));
    if callers.is_empty() {
        items.push(L::None);
    }
    items.push(L::Title(format!("callees ({})", callees.len())));
    items.extend(callees.iter().map(|&(a, n)| L::Callee(a, n)));
    if callees.is_empty() {
        items.push(L::None);
    }
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        L::Head(s) | L::Title(s) => text_row(s.clone()),
        L::None => text_row("  none"),
        L::Caller(a, n) => {
            let name = img.func_at(*a).map_or_else(|| format!("{a:#x}"), |h| img.func_name(h).to_string());
            Row {
                key: 0,
                text: format!("  {n:>5}  {a:#010x}  {name}{}", note_suffix(img, *a)),
                json: Some(json!({"table": "callers", "addr": a, "name": name, "sites": n, "note": img.note_at(*a)})),
            }
        }
        L::Callee(a, n) => {
            let name = name_of(img, *a);
            let import = img.slots.contains_key(a);
            Row {
                key: 0,
                text: format!("  {n:>5}  {a:#010x}  {name}{}", note_suffix(img, *a)),
                json: Some(json!({"table": "callees", "addr": a, "name": name, "sites": n, "import": import,
                    "note": img.note_at(*a)})),
            }
        }
    });
    let meta = json!({"name": img.func_name(f), "start": f.start, "end": f.end, "callers": callers.len(),
        "caller_sites": sites(&callers), "callees": callees.len(), "callee_sites": sites(&callees),
        "pointers": pointers, "note": img.note_at(f.start)});
    Ok(Output { op: "calls", meta, rows, tail, command: command(img, "calls", &shell_word(target.trim())) })
}

fn one_line(op: &'static str, text: String, j: Value, img: &Image) -> Output {
    Output {
        op,
        meta: j.clone(),
        rows: vec![Row { key: 0, text, json: Some(j) }],
        tail: 0,
        command: command(img, op, ""),
    }
}

/// Give an address a name that stays with the file.
pub fn name_set(img: &Image, data_dir: &Path, target: &str, name: &str) -> Result<Output> {
    let addr = target_addr(img, target)?;
    let old = names::set_name(img, data_dir, addr, name)?;
    let was = old.as_ref().map(|o| format!(" (was {o})")).unwrap_or_default();
    Ok(one_line(
        "name",
        format!("named {addr:#x} {name}{was}"),
        json!({"addr": addr, "name": name, "previous": old, "action": "set"}),
        img,
    ))
}

/// Remove the name at an address.
pub fn name_clear(img: &Image, data_dir: &Path, target: &str) -> Result<Output> {
    let addr = target_addr(img, target)?;
    let had = names::clear_name(img, data_dir, addr)?;
    let text =
        if had { format!("cleared the name at {addr:#x}") } else { format!("no name at {addr:#x}; nothing to clear") };
    Ok(one_line("name", text, json!({"addr": addr, "cleared": had, "action": "clear"}), img))
}

/// Leave a note at an address.
pub fn note_set(img: &Image, data_dir: &Path, target: &str, text: &str) -> Result<Output> {
    let addr = target_addr(img, target)?;
    let kept = names::set_note(img, data_dir, addr, text)?;
    Ok(one_line("note", format!("noted {addr:#x}: {kept}"), json!({"addr": addr, "note": kept, "action": "set"}), img))
}

/// Remove the note at an address.
pub fn note_clear(img: &Image, data_dir: &Path, target: &str) -> Result<Output> {
    let addr = target_addr(img, target)?;
    let had = names::clear_note(img, data_dir, addr)?;
    let text =
        if had { format!("cleared the note at {addr:#x}") } else { format!("no note at {addr:#x}; nothing to clear") };
    Ok(one_line("note", text, json!({"addr": addr, "cleared": had, "action": "clear"}), img))
}

/// The names and notes this file has, by address.
pub fn names_list(img: &Image) -> Output {
    names_list_window(img, None, usize::MAX)
}

/// Like [`names_list`], but only the lines of one page are formatted.
pub fn names_list_window(img: &Image, from: Option<u64>, budget: usize) -> Output {
    let mut addrs: Vec<u64> = img.user.names.keys().chain(img.user.notes.keys()).copied().collect();
    addrs.sort_unstable();
    addrs.dedup();
    let mut items: Vec<Option<u64>> = vec![None];
    items.extend(addrs.iter().map(|&a| Some(a)));
    let (rows, tail) = cut(&items, from, budget, |it| match it {
        None => text_row(format!("{} names, {} notes", img.user.names.len(), img.user.notes.len())),
        Some(a) => {
            let name = img.user.names.get(a);
            let note = img.user.notes.get(a);
            let mut text = format!("{a:#010x}  {}", name.map_or("-", String::as_str));
            if let Some(n) = note {
                text.push_str(&format!("  ; note: {n}"));
            }
            Row { key: 0, text, json: Some(json!({"addr": a, "name": name, "note": note})) }
        }
    });
    let meta = json!({"names": img.user.names.len(), "notes": img.user.notes.len()});
    Output { op: "names", meta, rows, tail, command: command(img, "names", "") }
}
