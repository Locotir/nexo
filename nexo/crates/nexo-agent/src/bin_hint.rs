//! The first time a model reads a compiled program by hand (`objdump`, `readelf`, `nm`, `strings`, `xxd`,
//! `r2`, ...), the result of that command ends with one line naming the exact call of nexo's kit that does the
//! same: the `bin` tool when it is offered, `nexo bin` in the shell when it is not. Once per session, and only
//! when the file the command names is one `nexo bin` opens. No schema and no system text carry it: a person who
//! never opens a binary pays nothing. The line is part of the result, so it stays in the history, the saved
//! session and a compaction summary's source like any output; `told` tells a resumed or rewound history from a
//! fresh one.
//!
//! Nothing here runs or loads a file. The command is split by `hint_cmd` (redirections, heredocs, `cd x &&`,
//! wrappers), and a name it finds is only opened as bytes by `nexo_bin::opens`, which reads at most the first
//! 4096. `gdb` is read here only when the `debug` tool is not offered; with it, `dbg_hint` answers.

use std::path::Path;

mod op;

use nexo_core::message::{Message, Part};
use op::op_for;
use serde_json::Value;

use crate::hint_cmd::{self, Offer};

/// Starts the line when the `bin` tool is offered; `told` finds it in a kept history.
pub const HEAD: &str = "[nexo: the bin tool reads this file in pages, with names and references:";
/// Starts the line when it is not: the same call through the shell.
pub const HEAD_SHELL: &str = "[nexo: nexo bin reads this file in pages, with names and references:";
/// The one line sessions saved before the calls were exact have: still found by `told`.
const OLD_LINE: &str = "[nexo: this file can be read in pages with names and references: nexo re lists the kit (nexo bin reads, nexo dbg runs); nexo bin overview <file> first (main, imports by capability, next steps), then info|funcs|fn|xref|calls|tree|path|slice|strings|find|crypto]";

/// What follows the call.
const THEN: &str = "then fn|funcs|xref|decomp|strings";

/// The programs people use to read a compiled program by hand.
const READERS: [&str; 10] = ["objdump", "readelf", "nm", "strings", "xxd", "hexdump", "od", "r2", "radare2", "rizin"];

/// The most words of one command whose file is looked at.
const WORDS: usize = 12;

/// The line for `command`, run in `cwd`, when it reads a file `nexo bin` opens with one of the `READERS`.
pub fn line(command: &str, cwd: &Path, offer: Offer) -> Option<String> {
    let segs = hint_cmd::segments(command, cwd)?;
    // a command that already is `nexo bin` needs no hint
    if segs.iter().any(|s| {
        s.words.first().is_some_and(|w| hint_cmd::program(w) == "nexo")
            && s.words.get(1).is_some_and(|w| w == "bin" || w == "re")
    }) {
        return None;
    }
    for (k, s) in segs.iter().enumerate() {
        let Some(first) = s.words.first() else { continue };
        let prog = hint_cmd::program(first);
        // `gdb` on a program is the debugger's: here only where there is no `debug` tool
        if !(READERS.contains(&prog.as_str()) || (prog == "gdb" && !offer.debug)) {
            continue;
        }
        let args = &s.words[1..];
        let Some(file) = args.iter().take(WORDS).find(|w| hint_cmd::reads(&s.dir, w)) else { continue };
        let path = hint_cmd::shown(cwd, &s.dir, file);
        // what goes inside a double-quoted JSON string or a shell word must be plain
        if path.contains(['"', '\\', '\n', '$', '`']) {
            continue;
        }
        let op = op_for(&prog, args, &path, segs.get(k + 1));
        return Some(say(&op, offer));
    }
    None
}

/// The line that names `op` (an op of `nexo bin` with its file) as a `bin` call or a shell command.
fn say(op: &str, offer: Offer) -> String {
    if offer.bin {
        format!("{HEAD} {}, {THEN}]", hint_cmd::tool_call("bin", op))
    } else {
        format!("{HEAD_SHELL} nexo bin {op}, {THEN}]")
    }
}

/// Whether `messages` already hold the line in a tool result.
pub fn told(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        m.content.iter().any(|p| match p {
            Part::ToolResult { content, .. } => content
                .iter()
                .filter_map(Part::as_text)
                .any(|t| t.contains(HEAD) || t.contains(HEAD_SHELL) || t.contains(OLD_LINE)),
            _ => false,
        })
    })
}

/// The results of one round with the line added to the first that earns it. `told` is whether the session has
/// had it already; it is set when the line is added. Calls with `background` or `detach` have no output yet.
pub fn round(
    told: &mut bool,
    calls: &[(String, String, Value)],
    results: Vec<Part>,
    cwd: &Path,
    offer: Offer,
) -> Vec<Part> {
    if *told {
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
            // a command that was refused, blocked or interrupted never ran: its file is not looked at and the
            // one line of the session is not spent on it
            if !*told && (!is_error || hint_cmd::ran(&text)) {
                if let Some(l) = command.and_then(|c| line(c, cwd, offer)) {
                    *told = true;
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
