//! When a model reads a compiled program by hand (`objdump`, `readelf`, `nm`, `strings`, `xxd`, `file`, `ldd`,
//! `cat`, `head`, `grep -a`, a `python` script that opens it, ...), the result of that command ends with one line
//! of facts: what the file is (format, architecture, system, size, stripped or not) and the exact call of nexo's
//! kit that gives the same, the `bin` tool when it is offered and `nexo bin` in the shell when it is not. Only
//! when the file the command names is one `nexo bin` reads; once per session, and again only for another file
//! after a long stretch without one (see [`Told`]). No schema and no system text carry it: a person who never
//! opens a binary pays nothing. The line is part of the result, so it stays in the history, the saved session
//! and a compaction summary's source like any output; `told` reads a resumed or rewound history.
//!
//! Nothing here runs or loads a file. The command is split by `hint_cmd` (redirections, heredocs, `cd x &&`,
//! wrappers), and a name it finds is only read as bytes by `nexo_bin::kind_at` (the first 4 KiB, the section
//! headers of an ELF file, 64 KiB of a PE file). `gdb` is read here only when the `debug` tool is not offered;
//! with it, `dbg_hint` answers.

use std::path::Path;

mod op;
mod script;
mod told;

use nexo_core::message::Part;
use op::op_for;
use script::Code;
use serde_json::Value;

use crate::hint_cmd::{self, Offer, Seg};
pub use told::{told, Told};

/// Starts the line, before the file's name.
const START: &str = "[nexo: ";
/// Follows the call in every line: `told` finds the lines by it.
pub const MARK: &str = " reads it in pages, with names and references";

/// What follows for the native ops, a WebAssembly module and managed code.
const THEN: &str = ", then fn|funcs|xref|decomp|strings";
const THEN_WASM: &str = ", then fn|funcs|xref|calls|strings";
const THEN_MANAGED: &str = ", then a class or method by name";

/// The programs people use to read a compiled program by hand, by the names [`op_for`] knows them; `gdb` too
/// when there is no `debug` tool.
const READERS: [&str; 18] = [
    "objdump", "readelf", "nm", "strings", "xxd", "hexdump", "hd", "od", "r2", "radare2", "rizin", "file", "ldd",
    "otool", "cat", "head", "tail", "dd",
];

/// The most words of one command, and of quoted strings of a script, whose file is looked at.
const WORDS: usize = 12;

/// The file `command`, run in `cwd`, reads by hand, as the line names it, and the line, when it is a file
/// `nexo bin` reads.
fn found(command: &str, cwd: &Path, offer: Offer) -> Option<(String, String)> {
    let segs = hint_cmd::segments(command, cwd)?;
    // a command that already is `nexo bin` needs no hint
    if segs.iter().any(|s| {
        s.words.first().is_some_and(|w| hint_cmd::program(w) == "nexo")
            && s.words.get(1).is_some_and(|w| w == "bin" || w == "re")
    }) {
        return None;
    }
    for (k, s) in segs.iter().enumerate() {
        let Some(prog) = s.words.first().and_then(|w| reader(&hint_cmd::program(w), s, offer)) else { continue };
        let Some((file, kind)) = file_of(prog, s) else { continue };
        let path = hint_cmd::shown(cwd, &s.dir, &file);
        // what goes inside a double-quoted JSON string or a shell word must be plain
        if !hint_cmd::callable(&path) {
            continue;
        }
        let args = &s.words[1..];
        // `grep -a` names its pattern in its own words; the others take it from a `| grep` after them
        let next = if prog == "grep" { Some(s) } else { segs.get(k + 1) };
        let op = match (kind.op, op_for(prog, args, &path, next)) {
            ("managed", _) => format!("managed {}", hint_cmd::quoted(&path)),
            // a WebAssembly module has no sections to dump: its map comes first
            ("overview", op) if kind.what.starts_with("WebAssembly") && op.starts_with("bytes ") => {
                format!("overview {}", hint_cmd::quoted(&path))
            }
            (_, op) => op,
        };
        let size = std::fs::metadata(s.dir.join(&file)).ok().map(|m| m.len());
        return Some((path.clone(), say(&path, &kind, size, &op, offer)));
    }
    None
}

/// The name [`op_for`] knows the program `prog` of the command `s` by, when it is read here as one that reads a
/// file by hand: `llvm-objdump` is `objdump`, `python3` is `python`, `grep` only with `-a`, `cat` only when it does
/// not copy the file into another.
fn reader(prog: &str, s: &Seg, offer: Offer) -> Option<&'static str> {
    let tool = match prog.rsplit_once('-') {
        Some((_, t)) if matches!(t, "objdump" | "readelf" | "readobj" | "nm" | "strings") => t,
        _ => prog,
    };
    let copies = s.output.is_some() || s.words.iter().any(|w| w.starts_with("of="));
    let text = |w: &String| w == "--text" || w == "--binary-files=text" || (is_short(w) && w.contains('a'));
    match tool {
        "readobj" => Some("readelf"),
        "gdb" if !offer.debug => Some("gdb"),
        "cat" | "head" | "tail" | "dd" if copies => None,
        "grep" | "egrep" | "fgrep" | "rg" | "ugrep" if s.words.iter().skip(1).any(text) => Some("grep"),
        "perl" => Some("python"),
        p if p == "py" || p.starts_with("python") || p.starts_with("pypy") => Some("python"),
        p => READERS.iter().find(|r| **r == p).copied(),
    }
}

/// A cluster of short options (`-ai`), not a long one or a number.
fn is_short(w: &str) -> bool {
    w.strip_prefix('-').is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_alphabetic()))
}

/// The file of `s` that `nexo bin` reads, as the command wrote it, and what it is: one of its first words (`if=`
/// of `dd`), its `< file`, and for a script (`python -c`, `perl -e`, a heredoc) a quoted string of it. A script
/// counts only when its code reads files and starts no program (see [`Code`]).
fn file_of(prog: &str, s: &Seg) -> Option<(String, nexo_bin::Compiled)> {
    let mut words: Vec<&str> = s.words[1..]
        .iter()
        .take(WORDS)
        .map(|w| if prog == "dd" { w.strip_prefix("if=").unwrap_or(w) } else { w })
        .collect();
    if let Some(hint_cmd::Input::File(f)) = &s.input {
        words.push(f);
    }
    let code = (prog == "python").then(|| Code::of(s));
    words.extend(code.as_ref().map(Code::literals).unwrap_or_default());
    let found = words.into_iter().find_map(|w| hint_cmd::kind(&s.dir, w).map(|k| (w.to_string(), k)))?;
    // looked at last: a script file is opened only when the command names a compiled one
    code.is_none_or(|c| c.reads(&s.dir)).then_some(found)
}

/// The first quoted strings of a script on one line each (`open('prog', 'rb')`): the files it may name.
fn literals(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = code;
    while out.len() < WORDS {
        let Some(i) = rest.find(['\'', '"']) else { break };
        let q = &rest[i..i + 1];
        let after = &rest[i + 1..];
        let Some(j) = after.find(q) else { break };
        let lit = &after[..j];
        if !lit.is_empty() && lit.len() <= 1024 && !lit.contains('\n') {
            out.push(lit);
        }
        rest = &after[j + 1..];
    }
    out
}

/// The line: the file, what it is, and `op` (an op of `nexo bin` with its file) as a `bin` call or a shell
/// command.
fn say(path: &str, kind: &nexo_bin::Compiled, size: Option<u64>, op: &str, offer: Offer) -> String {
    let then = match kind.op {
        "managed" => THEN_MANAGED,
        _ if kind.what.starts_with("WebAssembly") => THEN_WASM,
        _ => THEN,
    };
    let call = nexo_tools::program::bin_call(op, offer.bin);
    format!("{START}{path}: {}; {call}{MARK}{then}]", kind.facts(size))
}

/// The results of one round with the line added to the first that earns it, when [`Told`] lets one come. Calls
/// with `background` or `detach` have no output yet.
pub fn round(
    told: &mut Told,
    calls: &[(String, String, Value)],
    results: Vec<Part>,
    cwd: &Path,
    offer: Offer,
) -> Vec<Part> {
    if !told.tick() {
        return results;
    }
    results
        .into_iter()
        .map(|p| {
            let Part::ToolResult { call_id, name, mut content, is_error } = p else { return p };
            let command = calls
                .iter()
                .find(|(id, n, _)| *id == call_id && n == "bash")
                .filter(|(_, _, a)| !flag(a, "background") && !flag(a, "detach"))
                .and_then(|(_, _, a)| a.get("command").and_then(Value::as_str));
            let text: String = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
            // a command that was refused, blocked or interrupted never ran: its file is not looked at and no line
            // is spent on it
            if told.open() && (!is_error || hint_cmd::ran(&text)) {
                let hit = command.and_then(|c| found(c, cwd, offer)).filter(|(file, _)| told.may(file));
                if let Some((file, l)) = hit {
                    told.add(file);
                    content = vec![Part::text(format!("{text}\n\n{l}"))];
                }
            }
            Part::ToolResult { call_id, name, content, is_error }
        })
        .collect()
}

fn flag(args: &Value, key: &str) -> bool {
    nexo_tools::util::bool_arg(args, key)
}

#[cfg(test)]
mod tests;
