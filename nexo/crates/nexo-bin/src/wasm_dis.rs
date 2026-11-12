//! Text for WebAssembly instructions, and the `fn` view of one function.

use std::collections::BTreeMap;

use serde_json::json;
use wasmparser::{BlockType, Operator};

use crate::page::{shell_word, Output, Row};
use crate::strings::shorten;
use crate::wasm::Module;
use crate::wasm_ops::{resolve, string_runs};
use crate::{Error, Result};

/// `I32TruncSatF32S` becomes `i32.trunc_sat_f32_s`, `BrIf` becomes `br_if`.
pub(crate) fn mnemonic(debug: &str) -> String {
    let name = debug.split([' ', '{', '(']).next().unwrap_or(debug);
    let mut words: Vec<String> = Vec::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() || words.is_empty() {
            words.push(c.to_ascii_lowercase().to_string());
        } else if let Some(w) = words.last_mut() {
            w.push(c);
        }
    }
    const SPACES: [&str; 12] =
        ["i32", "i64", "f32", "f64", "v128", "local", "global", "memory", "table", "ref", "data", "elem"];
    let first = words[0].clone();
    // `i8x16`, `i16x8`, `f32x4`... are lane types of the vector operators
    if SPACES.contains(&first.as_str()) || first.contains('x') && first.starts_with(['i', 'f']) && first.len() > 3 {
        if words.len() == 1 {
            return first;
        }
        return format!("{first}.{}", words[1..].join("_"));
    }
    words.join("_")
}

/// The immediates of an instruction that has no special case: read from its debug text, never dropped.
fn generic_args(debug: &str) -> String {
    let Some(i) = debug.find(" { ") else { return String::new() };
    let rest = debug[i + 3..].trim_end_matches(" }");
    if let Some(m) = rest.find("memarg: MemArg {") {
        let field = |k: &str| {
            let s = &rest[m..];
            let at = s.find(&format!("{k}: "))? + k.len() + 2;
            s[at..].split([',', ' ']).next()?.parse::<u64>().ok()
        };
        let mut out = Vec::new();
        if let Some(o) = field("offset").filter(|&o| o > 0) {
            out.push(format!("offset={o}"));
        }
        if let Some(a) = field("align") {
            out.push(format!("align={}", 1u64 << a.min(63)));
        }
        if let Some(mem) = field("memory").filter(|&x| x > 0) {
            out.push(format!("memory={mem}"));
        }
        let other = rest[..m].trim_end_matches([',', ' ']);
        if !other.is_empty() {
            out.insert(0, other.to_string());
        }
        return out.join(" ");
    }
    if rest.contains(['{', '(']) {
        return rest.to_string();
    }
    rest.split(", ").map(|kv| kv.split(": ").nth(1).unwrap_or(kv)).collect::<Vec<_>>().join(" ")
}

fn block_type(b: &BlockType) -> String {
    match b {
        BlockType::Empty => String::new(),
        BlockType::Type(t) => format!(" (result {t})"),
        BlockType::FuncType(i) => format!(" (type {i})"),
    }
}

/// An open `block`, `loop`, `if` or `try`: its name and where it began.
pub(crate) struct Open {
    kind: &'static str,
    start: usize,
}

/// One instruction as text, with a `;` note for what a number means: a callee, a string, a branch target.
/// `open` is the stack of blocks around it, updated here.
pub(crate) fn render(
    m: &Module<'_>,
    func: u32,
    strings: &BTreeMap<u64, String>,
    op: &Operator<'_>,
    off: usize,
    open: &mut Vec<Open>,
) -> (String, Option<String>) {
    let dbg = format!("{op:?}");
    let name = mnemonic(&dbg);
    let target = |open: &Vec<Open>, depth: u32| -> String {
        match open.len().checked_sub(1 + depth as usize).and_then(|i| open.get(i)) {
            Some(o) if o.kind == "loop" => format!("-> loop at {:#x}", o.start),
            Some(o) => format!("-> end of {} at {:#x}", o.kind, o.start),
            None => "-> function end (return)".to_string(),
        }
    };
    match op {
        Operator::Block { blockty } | Operator::Loop { blockty } | Operator::If { blockty } => {
            let kind = if matches!(op, Operator::Loop { .. }) {
                "loop"
            } else if matches!(op, Operator::If { .. }) {
                "if"
            } else {
                "block"
            };
            open.push(Open { kind, start: off });
            (format!("{name}{}", block_type(blockty)), None)
        }
        Operator::End => {
            open.pop();
            (name, None)
        }
        Operator::Call { function_index } | Operator::ReturnCall { function_index } => {
            (format!("{name} {function_index}"), Some(m.name(*function_index)))
        }
        Operator::RefFunc { function_index } => (format!("{name} {function_index}"), Some(m.name(*function_index))),
        Operator::CallIndirect { type_index, table_index }
        | Operator::ReturnCallIndirect { type_index, table_index } => (
            format!("{name} type {type_index} table {table_index}"),
            Some(format!("{}; the target is a table entry, not known statically", m.sig(*type_index))),
        ),
        Operator::Br { relative_depth } | Operator::BrIf { relative_depth } => {
            (format!("{name} {relative_depth}"), Some(target(open, *relative_depth)))
        }
        Operator::BrTable { targets } => {
            let list: Vec<String> = targets.targets().map(|t| t.map_or("?".to_string(), |d| d.to_string())).collect();
            (format!("{name} {} default {}", list.join(" "), targets.default()), None)
        }
        Operator::I32Const { value } => {
            let note = strings.get(&u64::from(*value as u32)).map(|s| format!("string? \"{}\"", shorten(s, 40)));
            (format!("{name} {value}"), note)
        }
        Operator::I64Const { value } => {
            let note = u64::try_from(*value)
                .ok()
                .and_then(|v| strings.get(&v))
                .map(|s| format!("string? \"{}\"", shorten(s, 40)));
            (format!("{name} {value}"), note)
        }
        Operator::LocalGet { local_index }
        | Operator::LocalSet { local_index }
        | Operator::LocalTee { local_index } => {
            (format!("{name} {local_index}"), m.local_names.get(&(func, *local_index)).map(|n| format!("${n}")))
        }
        Operator::GlobalGet { global_index } | Operator::GlobalSet { global_index } => {
            (format!("{name} {global_index}"), m.global_names.get(global_index).map(|n| format!("${n}")))
        }
        Operator::F32Const { value } => (format!("{name} {:?}", f32::from_bits(value.bits())), None),
        Operator::F64Const { value } => (format!("{name} {:?}", f64::from_bits(value.bits())), None),
        _ => {
            let args = generic_args(&dbg);
            (if args.is_empty() { name } else { format!("{name} {args}") }, None)
        }
    }
}

/// The disassembly of one function, one row per instruction keyed by its file offset.
pub(crate) fn fn_view(m: &Module<'_>, path: &str, target: &str, with_bytes: bool) -> Result<Output> {
    let index = resolve(m, target)?;
    let f = m.func(index).ok_or_else(|| Error::Bad(format!("no function {index}")))?;
    let command = format!(
        "nexo bin fn {} {}{}",
        shell_word(path),
        shell_word(&index.to_string()),
        if with_bytes { " --bytes" } else { "" }
    );
    let mut rows: Vec<Row> = Vec::new();
    let mut head = |t: String| {
        let key = rows.len() as u64;
        rows.push(Row { key, text: t, json: None });
    };
    let callers = m.callers().get(&index).map_or(0, Vec::len);
    let title = format!(
        "func[{index}] {}  {}{}",
        m.name(index),
        m.sig(f.type_idx),
        if m.start == Some(index) { "  (start function)" } else { "" }
    );
    let Some(body) = f.body.clone() else {
        let (module, name) = f.import.clone().unwrap_or_default();
        head(title);
        head(format!("  imported from module {module:?} as {name:?}: no body in this file; {callers} direct callers"));
        head(format!("  next: nexo bin calls {} {index}", shell_word(path)));
        return Ok(Output { op: "fn", meta: json!({"func": index, "imported": true}), rows, tail: 0, command });
    };
    head(format!("{title}  body {:#x}..{:#x} ({} bytes), {callers} direct callers", body.start, body.end, body.len()));
    if let Some(b) = m.body(index) {
        if let Ok(r) = b.get_locals_reader() {
            let locals: Vec<String> = r.into_iter().flatten().map(|(n, t)| format!("{n} x {t}")).collect();
            if !locals.is_empty() {
                head(format!("  locals (after the parameters): {}", locals.join(", ")));
            }
        }
    }
    let ops = m.ops(index).ok_or_else(|| Error::Bad(format!("func {index} has no body")))?;
    let strings = string_map(m);
    let (ops, failed) = match ops {
        Ok(o) => (o, None),
        // keep what decoded before the error: the scan knows where it stopped
        Err(e) => {
            let stop = m.scan(index).and_then(|s| s.bad.clone());
            let mut partial = Vec::new();
            if let Some(b) = m.body(index) {
                if let Ok(r) = b.get_operators_reader() {
                    for item in r.into_iter_with_offsets() {
                        match item {
                            Ok(x) => partial.push(x),
                            Err(_) => break,
                        }
                    }
                }
            }
            (partial, Some((stop, e.to_string())))
        }
    };
    let mut open: Vec<Open> = Vec::new();
    for (i, (op, off)) in ops.iter().enumerate() {
        let end = ops.get(i + 1).map_or(body.end, |x| x.1);
        let depth_before = open.len();
        let (text, note) = render(m, index, &strings, op, *off, &mut open);
        let depth = match op {
            Operator::End => open.len(),
            Operator::Else => depth_before.saturating_sub(1),
            _ => depth_before,
        };
        let bytes = if with_bytes {
            let hex: Vec<String> = m.raw(*off..end).iter().take(10).map(|b| format!("{b:02x}")).collect();
            format!("{:<30}", hex.join(" "))
        } else {
            String::new()
        };
        let line = format!(
            "  {off:#08x}  {bytes}{}{text}{}",
            "  ".repeat(depth),
            note.as_deref().map_or(String::new(), |n| format!("  ; {n}"))
        );
        rows.push(Row {
            key: *off as u64,
            text: line,
            json: Some(json!({"offset": off, "op": text, "note": note, "len": end - off})),
        });
    }
    if let Some((stop, why)) = failed {
        let at = stop.map_or(0, |s| s.0);
        let key = ops.last().map_or(0, |x| x.1 as u64) + 1;
        rows.push(Row {
            key,
            text: format!("  !! decoding stops at {at:#x}: {why}; the rest of the body is not decoded"),
            json: Some(json!({"error": why, "offset": at})),
        });
    }
    Ok(Output {
        op: "fn",
        meta: json!({"func": index, "name": m.name(index), "body": [body.start, body.end]}),
        rows,
        tail: 0,
        command,
    })
}

/// Start addresses of the printable strings in active data segments, with their text.
pub(crate) fn string_map(m: &Module<'_>) -> BTreeMap<u64, String> {
    string_runs(m).into_iter().filter_map(|s| s.mem_addr.map(|a| (a, s.text))).collect()
}
