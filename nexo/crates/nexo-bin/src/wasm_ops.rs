//! The ops for WebAssembly modules: `info`, `overview`, `funcs`, `fn`, `calls`, `xref` and `strings`,
//! answered in the same bounded rows as for ELF and PE (see `nexo-bin`'s `Output`).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::page::{cut, shell_word, Output, Row};
use crate::strings::shorten;
use crate::wasm::{Module, MAGIC, MIN_STRING};
use crate::wasm_dis::{fn_view, string_map};
use crate::{Error, Result};

/// Most strings kept from the data segments.
const MAX_STRINGS: usize = 200_000;

/// What to ask of a module. `op` is one of [`WASM_OPS`]; the rest are the options of that op.
#[derive(Debug, Clone, Default)]
pub struct WasmAsk<'a> {
    pub op: &'a str,
    /// A function: decimal index, hex file offset inside its body, or a name.
    pub target: Option<&'a str>,
    /// `funcs` and `strings`: a regular expression on the name or the text.
    pub grep: Option<&'a str>,
    /// `funcs`: addr, size, callers or callees.
    pub sort: &'a str,
    pub min_size: u64,
    /// `xref --str`: the text to look for in the strings, and whether to ignore case.
    pub str_text: Option<&'a str>,
    pub insensitive: bool,
    /// `fn`: show the instruction bytes.
    pub with_bytes: bool,
    pub from: Option<u64>,
    pub budget: usize,
}

/// The `nexo bin` ops that read a WebAssembly module.
pub const WASM_OPS: [&str; 7] = ["info", "overview", "funcs", "fn", "calls", "xref", "strings"];

/// `true` when the file starts with the WebAssembly signature. The file is only opened and 4 bytes read.
pub fn is_wasm(path: &Path) -> bool {
    use std::io::Read;
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == MAGIC
}

/// Answer one question about a WebAssembly module.
pub fn wasm_answer(path: &Path, ask: &WasmAsk<'_>) -> Result<Output> {
    let data = std::fs::read(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    wasm_answer_bytes(&data, &path.display().to_string(), ask)
}

/// Like [`wasm_answer`] for bytes already in memory; `path` only appears in the resume commands.
pub fn wasm_answer_bytes(data: &[u8], path: &str, ask: &WasmAsk<'_>) -> Result<Output> {
    if !WASM_OPS.contains(&ask.op) {
        return Err(Error::Usage(format!(
            "`{}` does not read WebAssembly modules yet; use one of: {}",
            ask.op,
            WASM_OPS.join(", ")
        )));
    }
    let m = Module::parse(data)?;
    let budget = ask.budget.max(crate::MIN_LINES);
    match ask.op {
        "info" => Ok(info(&m, path)),
        "overview" => Ok(overview(&m, path)),
        "funcs" => funcs(&m, path, ask, budget),
        "fn" => fn_view(&m, path, need(ask)?, ask.with_bytes),
        "calls" => calls(&m, path, need(ask)?),
        "xref" => match (ask.str_text, ask.target) {
            (Some(t), _) => xref_str(&m, path, t, ask.insensitive),
            (None, Some(t)) => xref(&m, path, t),
            (None, None) => Err(Error::Usage("give a function (index, name or hex offset), or --str TEXT".into())),
        },
        "strings" => strings(&m, path, ask.grep),
        _ => unreachable!("checked above"),
    }
}

fn need<'a>(ask: &WasmAsk<'a>) -> Result<&'a str> {
    ask.target.ok_or_else(|| {
        Error::Usage("give a function: its index (12), a hex offset inside its body (0x1a4) or its name".into())
    })
}

/// A function from what the model typed: `12` is an index, `0x1a4` a file offset inside a body, anything
/// else a name (exact first, then a part of one name; several matches are listed, never picked).
pub(crate) fn resolve(m: &Module<'_>, target: &str) -> Result<u32> {
    let t = target.trim();
    if t.is_empty() {
        return Err(Error::Bad("give a function: index, hex offset or name".into()));
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        let off = usize::from_str_radix(&h.replace('_', ""), 16)
            .map_err(|_| Error::Bad(format!("{t:?} is not a hex offset")))?;
        return m.func_at(off).map(|f| f.index).ok_or_else(|| {
            Error::Bad(format!(
                "{t} is not inside any function body (bodies lie in the code section; `nexo bin funcs` lists them)"
            ))
        });
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let i: u32 = t.parse().map_err(|_| Error::Bad(format!("{t} is not a function index")))?;
        return if m.func(i).is_some() {
            Ok(i)
        } else {
            Err(Error::Bad(format!(
                "no function {i}: the module has {} (0 to {})",
                m.funcs.len(),
                m.funcs.len().saturating_sub(1)
            )))
        };
    }
    let t = t.strip_prefix("func[").and_then(|x| x.strip_suffix(']')).unwrap_or(t);
    if let Ok(i) = t.parse::<u32>() {
        return resolve(m, &i.to_string());
    }
    let named: Vec<(u32, String)> = m.funcs.iter().map(|f| (f.index, m.name(f.index))).collect();
    let exact: Vec<u32> = named.iter().filter(|(_, n)| n == t).map(|x| x.0).collect();
    let pick = |v: Vec<u32>, how: &str| -> Result<u32> {
        match v.as_slice() {
            [one] => Ok(*one),
            _ => {
                let list: Vec<String> = v.iter().take(8).map(|i| format!("{i} {}", m.name(*i))).collect();
                Err(Error::Bad(format!(
                    "{} functions {how} {t:?}: {}{}; give the index",
                    v.len(),
                    list.join(", "),
                    if v.len() > 8 { ", ..." } else { "" }
                )))
            }
        }
    };
    if !exact.is_empty() {
        return pick(exact, "are named");
    }
    let raw: Vec<u32> = m
        .funcs
        .iter()
        .filter(|f| {
            f.import.as_ref().is_some_and(|(_, n)| n == t)
                || m.exports.iter().any(|e| e.kind == "func" && e.index == f.index && e.name == t)
        })
        .map(|f| f.index)
        .collect();
    if !raw.is_empty() {
        return pick(raw, "are exported or imported as");
    }
    let part: Vec<u32> = named.iter().filter(|(_, n)| n.contains(t)).map(|x| x.0).collect();
    if part.is_empty() {
        return Err(Error::Bad(format!("no function named {t:?}; `nexo bin funcs --grep {t}` searches the names")));
    }
    pick(part, "contain")
}

/// A printable run of bytes in a data segment.
pub(crate) struct StrRun {
    pub seg: usize,
    pub mem_addr: Option<u64>,
    pub file_off: usize,
    pub text: String,
}

/// Printable ASCII runs of `MIN_STRING` bytes or more in the data segments.
pub(crate) fn string_runs(m: &Module<'_>) -> Vec<StrRun> {
    let mut out = Vec::new();
    for s in &m.segments {
        let bytes = m.raw(s.file_off..s.file_off + s.len);
        let mut i = 0;
        while i < bytes.len() {
            let printable = |b: u8| (0x20..0x7f).contains(&b) || b == b'\t' || b == b'\n' || b == b'\r';
            if !printable(bytes[i]) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && printable(bytes[i]) {
                i += 1;
            }
            if i - start >= MIN_STRING {
                out.push(StrRun {
                    seg: s.index,
                    mem_addr: s.mem_addr.map(|a| a + start as u64),
                    file_off: s.file_off + start,
                    text: String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                });
                if out.len() >= MAX_STRINGS {
                    return out;
                }
            }
        }
    }
    out
}

/// Rows with the line position as key.
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

fn cmd(op: &str, path: &str, rest: &str) -> String {
    let p = shell_word(path);
    if rest.is_empty() {
        format!("nexo bin {op} {p}")
    } else {
        format!("nexo bin {op} {p} {rest}")
    }
}

fn info(m: &Module<'_>, path: &str) -> Output {
    let imported = m.first_defined;
    let defined = m.funcs.len() as u32 - imported;
    let mut l = Lines(Vec::new());
    l.text(format!(
        "format      WebAssembly module, version {}, {} bytes, sha256 {}",
        m.version,
        m.data.len(),
        m.sha256
    ));
    l.text(format!(
        "counts      {} types, {} imports ({imported} functions), {defined} defined functions, {} exports, {} data segments",
        m.types.len(),
        m.imports.len(),
        m.exports.len(),
        m.segments.len()
    ));
    l.text(format!("start       {}", m.start.map_or("none".to_string(), |s| format!("func[{s}] {}", m.name(s)))));
    for s in m.memories.iter().chain(&m.tables).chain(&m.globals) {
        l.text(format!("            {s}"));
    }
    for i in m.imports.iter().filter(|i| i.kind != "func") {
        l.text(format!("            imported {} {}.{}", i.kind, i.module, i.name));
    }
    let named = m.funcs.iter().filter(|f| f.name.is_some()).count();
    l.text(format!(
        "names       {named} of {} functions named by the name section{}",
        m.funcs.len(),
        if named == 0 { " (stripped: names fall back to exports and imports)" } else { "" }
    ));
    let broken: Vec<_> = m.scans.iter().enumerate().filter(|(_, s)| s.bad.is_some()).collect();
    if !broken.is_empty() {
        let first = broken[0].0 as u32 + m.first_defined;
        l.text(format!(
            "problem     {} function bodies do not decode (first: func[{first}] at {:#x}); the rest of those bodies is not analysed",
            broken.len(),
            broken[0].1.bad.as_ref().map_or(0, |b| b.0)
        ));
    }
    if !m.producers.is_empty() {
        let p: Vec<String> = m.producers.iter().map(|(f, n, v)| format!("{f}: {n} {v}")).collect();
        l.text(format!("producers   {}", p.join(" | ")));
    }
    l.text(format!("sections    {} custom", m.customs.len()));
    for (name, off, size) in &m.customs {
        let hint = if name.starts_with(".debug") { "  (DWARF debug info; not read yet)" } else { "" };
        l.item(
            format!("  custom {name:<22} {off:#010x}  {size} bytes{hint}"),
            json!({"name": name, "offset": off, "size": size}),
        );
    }
    l.text(format!("next        nexo bin overview {0} | nexo bin funcs {0} | nexo bin strings {0}", shell_word(path)));
    Output {
        op: "info",
        meta: json!({"format": "wasm", "version": m.version, "size": m.data.len(), "sha256": m.sha256,
            "functions": m.funcs.len(), "imported_functions": imported, "exports": m.exports.len(),
            "start": m.start, "named_functions": named}),
        rows: l.0,
        tail: 0,
        command: cmd("info", path, ""),
    }
}

/// Imports grouped by what they let the module do.
fn capability(module: &str, name: &str) -> &'static str {
    if module.starts_with("wasi") {
        if name.starts_with("sock_") {
            "network"
        } else if name.starts_with("fd_") || name.starts_with("path_") {
            "files"
        } else if name.starts_with("proc_") || name.starts_with("args_") || name.starts_with("environ_") {
            "process"
        } else {
            "system (clock, random, poll)"
        }
    } else if module == "env"
        || module.starts_with("wbg")
        || module.starts_with("__wbindgen")
        || module.starts_with("gojs")
    {
        "host (JavaScript glue)"
    } else {
        "other"
    }
}

fn overview(m: &Module<'_>, path: &str) -> Output {
    let mut l = Lines(Vec::new());
    let callers = m.callers();
    let defined = m.funcs.len() as u32 - m.first_defined;
    l.text(format!(
        "wasm v{}  {} bytes  {defined} functions + {} imported  {} exports  sha256 {}",
        m.version,
        m.data.len(),
        m.first_defined,
        m.exports.len(),
        &m.sha256[..16]
    ));
    if !m.producers.is_empty() {
        let p: Vec<String> = m.producers.iter().map(|(_, n, v)| format!("{n} {v}")).collect();
        l.text(format!("built by    {}", p.join(", ")));
    }
    let mut entries: Vec<String> = Vec::new();
    if let Some(s) = m.start {
        entries.push(format!("start func[{s}] {}", m.name(s)));
    }
    for e in m.exports.iter().filter(|e| e.kind == "func").take(12) {
        entries.push(format!("export {} = func[{}]", e.name, e.index));
    }
    let nexp = m.exports.iter().filter(|e| e.kind == "func").count();
    l.text(format!(
        "entry       {}{}",
        if entries.is_empty() {
            "none (no start function, no exported functions)".to_string()
        } else {
            entries.join("; ")
        },
        if nexp > 12 { format!("; {} more exports", nexp - 12) } else { String::new() }
    ));
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for i in m.imports.iter().filter(|i| i.kind == "func") {
        groups.entry(capability(&i.module, &i.name)).or_default().push(format!("{}.{}", i.module, i.name));
    }
    if groups.is_empty() {
        l.text("imports     none: the module is self-contained");
    }
    for (cap, names) in &groups {
        let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
        l.text(format!(
            "imports     {cap}: {}{}",
            shown.join(", "),
            if names.len() > 8 { format!(", {} more", names.len() - 8) } else { String::new() }
        ));
    }
    let mut hot: Vec<(u32, usize)> = callers.iter().map(|(f, v)| (*f, v.len())).collect();
    hot.sort_by_key(|&(f, n)| (std::cmp::Reverse(n), f));
    l.text("most called:");
    for (f, n) in hot.iter().take(8) {
        l.item(format!("  func[{f}] {}  {n} call sites", m.name(*f)), json!({"func": f, "callers": n}));
    }
    let mut big: Vec<(u32, usize)> =
        m.funcs.iter().filter_map(|f| f.body.as_ref().map(|b| (f.index, b.len()))).collect();
    big.sort_by_key(|&(f, n)| (std::cmp::Reverse(n), f));
    l.text("largest:");
    for (f, n) in big.iter().take(5) {
        l.item(format!("  func[{f}] {}  {n} bytes", m.name(*f)), json!({"func": f, "size": n}));
    }
    let runs = string_runs(m);
    let mut used: Vec<(&crate::wasm_ops::StrRun, usize)> = runs
        .iter()
        .map(|s| {
            let n = s.mem_addr.map_or(0, |a| {
                m.scans.iter().map(|sc| sc.consts.iter().filter(|c| c.0 as u64 == a).count()).sum::<usize>()
            });
            (s, n)
        })
        .filter(|x| x.1 > 0)
        .collect();
    used.sort_by_key(|x| std::cmp::Reverse(x.1));
    if !used.is_empty() {
        l.text("strings used by code (an i32.const equal to the string's start address):");
        for (s, n) in used.iter().take(6) {
            l.item(
                format!("  mem {:#x}  \"{}\"  {n} uses", s.mem_addr.unwrap_or(0), shorten(&s.text, 50)),
                json!({"mem": s.mem_addr, "text": s.text, "uses": n}),
            );
        }
    }
    l.text(format!(
        "next        nexo bin fn {0} <index|name> | nexo bin calls {0} <fn> | nexo bin xref {0} --str TEXT",
        shell_word(path)
    ));
    Output { op: "overview", meta: json!({"format": "wasm"}), rows: l.0, tail: 0, command: cmd("overview", path, "") }
}

fn funcs(m: &Module<'_>, path: &str, ask: &WasmAsk<'_>, budget: usize) -> Result<Output> {
    let re =
        ask.grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let callers = m.callers();
    let n_callers = |i: u32| callers.get(&i).map_or(0, Vec::len);
    let n_callees = |i: u32| m.scan(i).map_or(0, |s| s.calls.len());
    let size = |i: u32| m.func(i).and_then(|f| f.body.as_ref()).map_or(0, |b| b.len());
    let mut list: Vec<u32> = m
        .funcs
        .iter()
        .filter(|f| re.as_ref().is_none_or(|r| r.is_match(&m.name(f.index))))
        .filter(|f| size(f.index) as u64 >= ask.min_size)
        .map(|f| f.index)
        .collect();
    match ask.sort {
        "" | "callers" => list.sort_by_key(|&i| (std::cmp::Reverse(n_callers(i)), i)),
        "callees" => list.sort_by_key(|&i| (std::cmp::Reverse(n_callees(i)), i)),
        "size" => list.sort_by_key(|&i| (std::cmp::Reverse(size(i)), i)),
        "addr" => {}
        other => return Err(Error::Usage(format!("cannot sort by {other:?}: use addr, size, callers or callees"))),
    }
    let total = list.len();
    let (mut rows, tail) = cut(&list, ask.from, budget, |&i| {
        let f = m.func(i).expect("listed");
        let src = m.name_source(i);
        let place = f.body.as_ref().map_or("import".to_string(), |b| format!("{:#08x}", b.start));
        Row {
            key: 0,
            text: format!(
                "func[{i}] {place:<8} {:>6}B  callers {:>3}  callees {:>3}  {}  {}",
                size(i),
                n_callers(i),
                n_callees(i),
                m.name(i),
                m.sig(f.type_idx)
            ),
            json: Some(
                json!({"index": i, "name": m.name(i), "name_source": src, "offset": f.body.as_ref().map(|b| b.start),
                "size": size(i), "callers": n_callers(i), "callees": n_callees(i),
                "indirect_calls": m.scan(i).map_or(0, |s| s.indirect.len()), "imported": f.import.is_some(),
                "sig": m.sig(f.type_idx)}),
            ),
        }
    });
    let head = format!(
        "{total} functions{} (sorted by {})",
        if ask.grep.is_some() { " match" } else { "" },
        if ask.sort.is_empty() { "callers" } else { ask.sort }
    );
    rows.insert(0, Row { key: ask.from.unwrap_or(0).saturating_sub(0), text: head, json: None });
    let mut o = Output { op: "funcs", meta: json!({"total": total}), rows, tail, command: String::new() };
    let mut rest = String::new();
    if let Some(g) = ask.grep {
        rest.push_str(&format!("--grep {}", shell_word(g)));
    }
    if !ask.sort.is_empty() {
        rest.push_str(&format!(" --sort {}", ask.sort));
    }
    if ask.min_size > 0 {
        rest.push_str(&format!(" --min-size {}", ask.min_size));
    }
    o.command = cmd("funcs", path, rest.trim());
    Ok(o)
}

fn calls(m: &Module<'_>, path: &str, target: &str) -> Result<Output> {
    let index = resolve(m, target)?;
    let callers = m.callers();
    let mut l = Lines(Vec::new());
    let mut up: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    for &(c, off) in callers.get(&index).map_or(&[][..], Vec::as_slice) {
        let e = up.entry(c).or_insert((0, off));
        e.0 += 1;
    }
    let mut down: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let scan = m.scan(index);
    for &(c, off) in scan.map_or(&[][..], |s| s.calls.as_slice()) {
        let e = down.entry(c).or_insert((0, off));
        e.0 += 1;
    }
    let taken: Vec<_> = m
        .scans
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            s.ref_funcs.iter().filter(move |r| r.0 == index).map(move |r| (i as u32 + m.first_defined, r.1))
        })
        .collect();
    let in_table = m.in_tables.contains(&index);
    l.text(format!(
        "calls func[{index}] {}  ; {} callers ({} call sites), {} callees, {} indirect calls inside{}{}",
        m.name(index),
        up.len(),
        up.values().map(|x| x.0).sum::<usize>(),
        down.len(),
        scan.map_or(0, |s| s.indirect.len()),
        if in_table { ", sits in a function table (may be called indirectly)" } else { "" },
        if taken.is_empty() {
            String::new()
        } else {
            format!(", its address is taken {} times (ref.func)", taken.len())
        }
    ));
    l.text("callers:");
    for (c, (n, first)) in &up {
        l.item(
            format!("  func[{c}] {}  x{n}  first at {first:#x}", m.name(*c)),
            json!({"dir": "caller", "func": c, "calls": n, "first": first}),
        );
    }
    if up.is_empty() {
        l.text("  none direct");
    }
    l.text("callees:");
    for (c, (n, first)) in &down {
        l.item(
            format!("  func[{c}] {}  x{n}  first at {first:#x}", m.name(*c)),
            json!({"dir": "callee", "func": c, "calls": n, "first": first}),
        );
    }
    if down.is_empty() {
        l.text("  none direct");
    }
    if let Some(s) = scan {
        for (t, off) in &s.indirect {
            l.item(
                format!("  call_indirect at {off:#x} {}  (target decided at run time)", m.sig(*t)),
                json!({"dir": "indirect", "offset": off, "type": t}),
            );
        }
    }
    Ok(Output {
        op: "calls",
        meta: json!({"func": index}),
        rows: l.0,
        tail: 0,
        command: cmd("calls", path, &index.to_string()),
    })
}

fn xref(m: &Module<'_>, path: &str, target: &str) -> Result<Output> {
    let index = resolve(m, target)?;
    let mut l = Lines(Vec::new());
    let callers = m.callers();
    let direct = callers.get(&index).map_or(&[][..], Vec::as_slice);
    l.text(format!("xref func[{index}] {}  ; {} direct call sites", m.name(index), direct.len()));
    for &(c, off) in direct {
        l.item(
            format!("  call   {off:#08x}  in func[{c}] {}", m.name(c)),
            json!({"kind": "call", "offset": off, "func": c}),
        );
    }
    for (i, s) in m.scans.iter().enumerate() {
        for &(f, off) in s.ref_funcs.iter().filter(|r| r.0 == index) {
            let c = i as u32 + m.first_defined;
            let _ = f;
            l.item(
                format!("  addr   {off:#08x}  ref.func in func[{c}] {}", m.name(c)),
                json!({"kind": "ref.func", "offset": off, "func": c}),
            );
        }
    }
    if m.in_tables.contains(&index) {
        l.item("  table  an element segment puts it in a function table".to_string(), json!({"kind": "table"}));
    }
    for e in m.exports.iter().filter(|e| e.kind == "func" && e.index == index) {
        l.item(format!("  export {:?}", e.name), json!({"kind": "export", "name": e.name}));
    }
    if m.start == Some(index) {
        l.item("  start  the module's start function".to_string(), json!({"kind": "start"}));
    }
    if direct.is_empty() && !m.in_tables.contains(&index) && m.start != Some(index) && l.0.len() == 1 {
        l.text("  nothing reaches it directly; call_indirect sites can still (see `calls` for the signature)");
    }
    Ok(Output {
        op: "xref",
        meta: json!({"func": index}),
        rows: l.0,
        tail: 0,
        command: cmd("xref", path, &index.to_string()),
    })
}

fn strings(m: &Module<'_>, path: &str, grep: Option<&str>) -> Result<Output> {
    let re = grep.map(regex::Regex::new).transpose().map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
    let runs = string_runs(m);
    let mut users: BTreeMap<u64, Vec<(u32, usize)>> = BTreeMap::new();
    for (i, s) in m.scans.iter().enumerate() {
        for &(v, off) in &s.consts {
            users.entry(v as u64).or_default().push((i as u32 + m.first_defined, off));
        }
    }
    let mut l = Lines(Vec::new());
    let mut shown = 0;
    for s in runs.iter().filter(|s| re.as_ref().is_none_or(|r| r.is_match(&s.text))) {
        shown += 1;
        let mem = s.mem_addr.map_or(format!("seg{}", s.seg), |a| format!("mem {a:#x}"));
        let refs = s.mem_addr.and_then(|a| users.get(&a)).map_or(&[][..], Vec::as_slice);
        let who: Vec<String> = refs.iter().take(3).map(|(f, off)| format!("func[{f}]@{off:#x}")).collect();
        let more = if refs.len() > 3 { format!(" +{}", refs.len() - 3) } else { String::new() };
        l.item(
            format!(
                "{mem:<14} file {:#08x}  {:>4}  \"{}\"{}",
                s.file_off,
                s.text.len(),
                shorten(&s.text, 100),
                if who.is_empty() { String::new() } else { format!("  <- {}{more}", who.join(", ")) }
            ),
            json!({"mem": s.mem_addr, "file_offset": s.file_off, "text": s.text,
                "used_by": refs.iter().map(|(f, o)| json!({"func": f, "offset": o})).collect::<Vec<_>>()}),
        );
    }
    let head = format!(
        "{shown} strings of {MIN_STRING}+ printable bytes in {} data segments (`<-` marks an i32.const equal to the start address; a string reached through a computed address shows none)",
        m.segments.len()
    );
    let mut rows = vec![Row { key: 0, text: head, json: None }];
    for (i, mut r) in l.0.into_iter().enumerate() {
        r.key = i as u64 + 1;
        rows.push(r);
    }
    let rest = grep.map(|g| format!("--grep {}", shell_word(g))).unwrap_or_default();
    Ok(Output { op: "strings", meta: json!({"count": shown}), rows, tail: 0, command: cmd("strings", path, &rest) })
}

fn xref_str(m: &Module<'_>, path: &str, text: &str, insensitive: bool) -> Result<Output> {
    if text.is_empty() {
        return Err(Error::Bad("give some text to look for in the strings".into()));
    }
    let lower = text.to_ascii_lowercase();
    let strings = string_map(m);
    let runs = string_runs(m);
    let mut l = Lines(Vec::new());
    let mut hits = 0;
    for s in runs.iter().filter(|s| {
        if insensitive {
            s.text.to_ascii_lowercase().contains(&lower)
        } else {
            s.text.contains(text)
        }
    }) {
        hits += 1;
        let mem = s.mem_addr.map_or(format!("seg{}", s.seg), |a| format!("mem {a:#x}"));
        l.item(format!("{mem}  \"{}\"", shorten(&s.text, 100)), json!({"mem": s.mem_addr, "text": s.text}));
        let Some(a) = s.mem_addr else { continue };
        for (i, sc) in m.scans.iter().enumerate() {
            for &(v, off) in sc.consts.iter().filter(|c| c.0 as u64 == a) {
                let _ = v;
                let f = i as u32 + m.first_defined;
                l.item(
                    format!("    {off:#08x}  i32.const in func[{f}] {}", m.name(f)),
                    json!({"func": f, "offset": off}),
                );
            }
        }
    }
    let _ = strings;
    let mut rows = vec![Row { key: 0, text: format!("{hits} strings contain {text:?}"), json: None }];
    for (i, mut r) in l.0.into_iter().enumerate() {
        r.key = i as u64 + 1;
        rows.push(r);
    }
    let rest = format!("--str {}{}", shell_word(text), if insensitive { " --i" } else { "" });
    Ok(Output { op: "xref", meta: json!({"hits": hits}), rows, tail: 0, command: cmd("xref", path, &rest) })
}
