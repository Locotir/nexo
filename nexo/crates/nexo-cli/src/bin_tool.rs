//! `bin`: the reading side of `nexo bin` as a tool, so a model that wants to look inside a compiled program asks
//! for it in one call instead of going through objdump, strings or xxd in the shell. The same ops and flags as
//! `nexo bin` (the file is read as bytes, never run or loaded). Like `debug`, the call has a kind decided from the
//! parsed op (`call_kind`): everything reads or keeps names, notes and types in nexo's data folder, so it is allowed
//! as a read; `patch` and `sigs --out` write a file, so they are a write and the permission mode asks. `a; b` runs
//! several ops in one call, split by the debugger's `script_units`.
//!
//! Only the short text rides in every request (the tool is `rare`); `<op> --help` prints the whole of one op.

use async_trait::async_trait;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::bin_cmd::BinOp;

mod clap_facts;
mod parse;
mod shell;
#[cfg(test)]
mod tests;

pub(crate) use clap_facts::bin_error;
use parse::{parse, Parsed};
use shell::{scan_shell, units};

const OPS: &str = "\
overview <file>  the map in one call: main, imports by capability, top functions and strings, next commands (start here)
info <file>  format, entry, sections, hardening
syms <file> [--kind needed|import|export|func|data|section|got|reloc|all]  the file's own tables (what nm, readelf and ldd show): needed libraries, imports with library, stub and GOT/IAT slot, exports, symbols; puts@got and __imp_X name a slot for bytes, xref and fn
funcs <file> [--grep RE] [--sort size|callers] [--min-size N]  functions with size, callers, callees
fn <file> <name|addr> [--bytes] [--to ADDR]  one function disassembled (Intel syntax); main and entry also name it in a stripped file
decomp <file> <name|addr>...  pseudo-C
xref <file> <name|addr> [--to ADDR|FN], xref <file> --str TEXT  who calls, reads, writes or points at it
calls <file> <name>, tree <file> <name> [--up], path <file> <from> <to>  callers, callees, call chains
cfg <file> <name|addr>  control-flow graph of one function: blocks, edges, loops, dominators (x86-64)
strings <file> [--grep RE], find <file> <hex|--imm N>, bytes <file> <addr> <len> [--as u32|char[16]|ptr|str|item[4]|a named or DWARF struct], slice, crypto, diff <old> <new>, names, managed (JVM, .NET), emu (one function in an emulator)
--lines N|all, --from K, --grep RE, --json after any op; `<op> --help` for its flags
--raw --base ADDR [--arch x86-64|x86-32|arm64] after any op: the file is bare code (a memory dump, shellcode, JIT output, an unpacked stub), one section loaded at ADDR, its first byte the entry
a; b  several ops in one call, each answer under its own `> op` line; an error stops the rest
Never runs the file. name, note, proto, struct, var, sigs and crypto --save keep names, types and signatures for this binary in nexo's data folder. patch --out and sigs --out write a file: the person is asked";

/// The tool. Every call parses its own line; it holds only whether the session has the `debug` tool beside it, the
/// one its short text and its answers name for running the file. `debug` is not registered on Linux arm64, nor by
/// default on macOS, nor with `tools.debug = false`, and there neither names it.
pub struct Bin {
    debug: bool,
}

impl Bin {
    /// `debug`: the session registers the `debug` tool too.
    pub const fn new(debug: bool) -> Bin {
        Bin { debug }
    }
}

/// The short text every request carries: the questions the tool answers, the ops a model reaches for first and the
/// readers it would use otherwise, then the tool that runs the file when the session has it.
const COMPACT: &str = "What an ELF, PE, Mach-O, wasm file imports, exports, calls, its asm, pseudo-C (objdump, strings): `overview ./p`, `funcs`, `fn ./p main`, `decomp`, `xref`. `debug` runs it. `--help`";
/// The short text where the session has no `debug` tool.
const COMPACT_ALONE: &str = "What an ELF, PE, Mach-O, wasm file imports, exports, calls, its asm, pseudo-C (objdump, strings): `overview ./p`, `funcs`, `fn ./p main`, `decomp`, `xref`. `--help`";

#[async_trait]
impl Tool for Bin {
    fn name(&self) -> &str {
        "bin"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bin".into(),
            description: format!(
                "Read a compiled program (ELF, PE, Mach-O, wasm, JVM, .NET) or bare code as bytes, without running it. One op per call (or `a; b`), the file first; answers come in bounded pages.\n{OPS}"
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "One op with its file and flags, e.g. `overview ./prog` or `fn ./prog main`."}
                },
                "required": ["command"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact { description: if self.debug { COMPACT } else { COMPACT_ALONE }, params: &[] })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    /// A script is as strong as its strongest op: a write if any op of it writes a file.
    fn call_kind(&self, args: &Value) -> ToolKind {
        let Some(line) = args.get("command").and_then(Value::as_str) else { return ToolKind::Read };
        let writes = units(line)
            .iter()
            .any(|u| matches!(parse(u), Ok(Parsed::Op(b)) if b.op.as_ref().is_some_and(writes_a_file)));
        if writes {
            ToolKind::Write
        } else {
            ToolKind::Read
        }
    }

    fn summary(&self, args: &Value) -> String {
        match args.get("command").and_then(Value::as_str) {
            Some(c) => format!("bin: {c}"),
            None => "bin".into(),
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(line) = args.get("command").and_then(Value::as_str) else {
            return ToolOutput::error("bin needs `command`, for example `overview ./prog`; `--help` lists the ops");
        };
        // `a; b` (or one op per line) is a script: each answer under its own line. An error is reported in its
        // place and the ops after it still run: ops read files and are independent of each other, and a script
        // that stopped at its first slip cost the whole call (unlike `debug`'s `do`, where a step needs the last)
        let parts = units(line);
        // shell syntax (a pipe, a redirect, `&&`, `cd`, `echo ---`, `$c`): said once for the call, the shell parts are
        // not run, the real ops still are
        let scan = scan_shell(&parts);
        if !scan.found {
            let [_, _, ..] = parts.as_slice() else {
                return one(parts.first().map_or(line, String::as_str), ctx, self.debug).await;
            };
        }
        let mut out: Vec<String> = Vec::new();
        let mut failed = false;
        let mut ran = 0;
        for seg in &scan.segs {
            if let Some(why) = &seg.skip {
                out.push(format!("> {}", seg.text));
                out.push(format!("not run: {why}"));
                continue;
            }
            let o = one(&seg.text, ctx, self.debug).await;
            ran += 1;
            failed |= o.is_error;
            if scan.segs.len() > 1 || !scan.found {
                out.push(format!("> {}", seg.text));
                out.push(if o.is_error { format!("error: {}", o.content) } else { o.content });
            } else {
                out.push(o.content);
            }
        }
        if let Some(note) = scan.note {
            out.insert(0, note);
        }
        // one op that fails is the call's error; in a script each error stays in its place
        if ran == 0 || (failed && scan.segs.len() == 1) {
            return ToolOutput::error(out.join("\n"));
        }
        ToolOutput::ok(out.join("\n"))
    }
}

/// One op: the file is relative to the session's folder, as in the shell, and the rules that cover `read` and
/// `edit` cover the files named inside it. `debug`: the session has the `debug` tool the answer's last line may name.
async fn one(unit: &str, ctx: &ToolContext, debug: bool) -> ToolOutput {
    let mut b = match parse(unit) {
        Ok(Parsed::Help(h)) => return ToolOutput::ok(h),
        Ok(Parsed::Op(b)) => b,
        Err(e) => return ToolOutput::error(e),
    };
    let Some(op) = b.op.as_mut() else { return ToolOutput::error("bin: give an op") };
    // the rules that cover `read` see this call's `command` and not the files named inside it, so each one is
    // asked here, as a `read`
    for f in op.files_mut() {
        if f.is_relative() {
            *f = ctx.cwd.join(&*f);
        }
        let Some(check) = &ctx.check else { continue };
        let path = f.to_string_lossy().to_string();
        match check("read", ToolKind::Read, &json!({ "path": path })) {
            Check::Allow => {}
            Check::Deny => return ToolOutput::error(format!("bin: a permission rule denies reading {path}")),
            Check::Ask => {
                return ToolOutput::error(format!(
                    "bin: reading {path} needs the person's OK under the permission rules, so it was not read"
                ))
            }
        }
    }
    // a file this op writes: the call was already asked about as a write, so only a rule that denies the place
    // stops it here (asking again would ask for the same OK twice)
    if let Some(f) = op.out_mut() {
        if f.is_relative() {
            *f = ctx.cwd.join(&*f);
        }
        if let Some(check) = &ctx.check {
            let path = f.to_string_lossy().to_string();
            if check("edit", ToolKind::Write, &json!({ "path": path })) == Check::Deny {
                return ToolOutput::error(format!("bin: a permission rule denies writing {path}"));
            }
        }
    }
    // an answer about one place in a program `debug` can run ends with the call that stops there, when the session
    // has that tool to take it
    let asked = tokio::task::spawn_blocking(move || {
        let text = crate::bin_cmd::execute(&b)?;
        let bridge = b.op.as_ref().filter(|_| debug).and_then(|op| crate::bin_cmd::bridge::line(op, &text));
        Ok::<_, nexo_bin::Error>(match bridge {
            Some(l) => format!("{text}\n{l}"),
            None => text,
        })
    });
    match asked.await {
        Ok(Ok(text)) => ToolOutput::ok(text),
        Ok(Err(e)) => ToolOutput::error(format!("bin: {e}")),
        Err(e) => ToolOutput::error(format!("bin stopped working: {e}")),
    }
}

/// Whether the op writes a file the person named (a patched copy, a signature file): a write. Names, notes, types
/// and `crypto --save` go to nexo's own data folder for this binary, as `debug`'s `name` and `note` do: a read.
fn writes_a_file(op: &BinOp) -> bool {
    matches!(op, BinOp::Patch { .. } | BinOp::Sigs { out: Some(_), .. })
}
