//! `bin`: the reading side of `nexo bin` as a tool, so a model that wants to look inside a compiled program asks
//! for it in one call instead of going through objdump, strings or xxd in the shell. The same ops and flags as
//! `nexo bin` (the file is read as bytes, never run or loaded); an op that saves names, notes, types or a patched
//! copy is refused here and stays on the command line, where it asks.
//!
//! Only the short text rides in every request (the tool is `rare`); `<op> --help` prints the whole of one op.

use async_trait::async_trait;
use clap::error::ErrorKind;
use clap::Parser;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::bin_cmd::BinOp;

const OPS: &str = "\
overview <file>  the map in one call: main, imports by capability, top functions and strings, next commands (start here)
info <file>  format, entry, sections, hardening
funcs <file> [--grep RE] [--sort size|callers] [--min-size N]  functions with size, callers, callees
fn <file> <name|addr> [--bytes] [--to ADDR]  one function disassembled (Intel syntax)
decomp <file> <name|addr>...  pseudo-C
xref <file> <name|addr>, xref <file> --str TEXT  who calls, reads, writes or points at it
calls <file> <name>, tree <file> <name> [--up], path <file> <from> <to>  callers, callees, call chains
strings <file> [--grep RE], find <file> <hex|--imm N>, bytes <file> <addr> <len> [--as u32|ptr|str], slice, crypto, diff <old> <new>, names, managed (JVM, .NET), emu (one function in an emulator)
--lines N, --from ADDR, --json after any op; `<op> --help` for its flags
Never runs the file. name, note, proto, struct, var, sigs, patch and crypto --save are refused here: they save state (use `nexo bin` in the shell)";

/// The tool. It holds nothing: every call parses its own line.
pub struct Bin;

#[async_trait]
impl Tool for Bin {
    fn name(&self) -> &str {
        "bin"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bin".into(),
            description: format!(
                "Read a compiled program (ELF, PE, Mach-O, wasm, JVM, .NET) as bytes, without running it. One op per call, the file first; answers come in bounded pages.\n{OPS}"
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
        Some(Compact {
            description: "Read a compiled program (ELF, PE, Mach-O, wasm) without running it, instead of objdump or strings: `overview ./p`, then `funcs`, `fn ./p main`, `decomp`, `xref`. `--help`",
            params: &[],
        })
    }

    fn rare(&self) -> bool {
        true
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
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
        let words = match crate::re_mcp::split_words(line) {
            Ok(w) => w,
            Err(e) => return ToolOutput::error(e),
        };
        let mut words = words.as_slice();
        // a model that writes the shell form (`nexo bin fn ./p main`) gets the same answer
        if words.first().is_some_and(|w| w == "nexo") {
            words = &words[1..];
        }
        if words.first().is_some_and(|w| w == "bin") {
            words = &words[1..];
        }
        let mut argv: Vec<String> = vec!["nexo".into(), "bin".into()];
        match words.first().map(String::as_str) {
            None | Some("help") => argv.push("--help".into()),
            Some(_) => argv.extend(words.iter().cloned()),
        }
        let parsed = match crate::cli::Args::try_parse_from(argv) {
            Ok(p) => p,
            Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
                return ToolOutput::ok(e.to_string())
            }
            Err(e) => return ToolOutput::error(e.to_string()),
        };
        let Some(crate::cli::Cmd::Bin(mut b)) = parsed.cmd else {
            return ToolOutput::error("bin: not a bin op");
        };
        let Some(op) = b.op.as_mut() else {
            return ToolOutput::error("bin: give an op, for example `overview ./prog`; `--help` lists them");
        };
        if let Some(what) = saves(op) {
            return ToolOutput::error(format!(
                "refused: `{what}` saves names, notes, types or a file. Run it in the shell as `nexo bin {what} ...`, where it asks."
            ));
        }
        // the file is relative to the session's folder, as in the shell
        for f in op.files_mut() {
            if f.is_relative() {
                *f = ctx.cwd.join(&*f);
            }
        }
        match tokio::task::spawn_blocking(move || crate::bin_cmd::execute(&b)).await {
            Ok(Ok(text)) => ToolOutput::ok(text),
            Ok(Err(e)) => ToolOutput::error(format!("bin: {e}")),
            Err(e) => ToolOutput::error(format!("bin stopped working: {e}")),
        }
    }
}

/// The op's name when it saves something (names, notes, types, a patched copy, crypto constants to the data
/// folder), decided from the parsed op and not from the first word.
fn saves(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::Name { .. } => Some("name"),
        BinOp::Note { .. } => Some("note"),
        BinOp::Proto { .. } => Some("proto"),
        BinOp::Struct { .. } => Some("struct"),
        BinOp::Var { .. } => Some("var"),
        BinOp::Sigs { .. } => Some("sigs"),
        BinOp::Patch { .. } => Some("patch"),
        BinOp::Crypto { save: true, .. } => Some("crypto --save"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(cwd: &std::path::Path, command: &str) -> ToolOutput {
        Bin.run(json!({"command": command}), &ToolContext::new(cwd.to_path_buf())).await
    }

    /// An ELF or PE `nexo bin` opens: this test program.
    fn this_binary() -> std::path::PathBuf {
        std::env::current_exe().unwrap()
    }

    #[test]
    fn it_is_a_read_tool_and_every_request_carries_only_the_short_text() {
        let mut set = nexo_core::tool::ToolSet::new();
        set.add(std::sync::Arc::new(Bin));
        let spec = set.specs_for(false).pop().unwrap();
        let sent = serde_json::to_string(
            &json!({"name": spec.name, "description": spec.description, "parameters": spec.parameters}),
        )
        .unwrap();
        eprintln!("bin tool as offered: {} chars: {sent}", sent.len());
        assert!(sent.len() <= 303, "{} chars: {sent}", sent.len());
        assert!(Bin.rare() && Bin.kind() == ToolKind::Read);
        assert!(Bin.spec().description.len() > spec.description.len(), "the full text lists the ops");
        // the short text names the ops a model reaches for first
        for w in ["overview", "funcs", "fn", "decomp", "xref", "strings", "objdump", "--help"] {
            assert!(spec.description.contains(w), "{w}");
        }
    }

    #[tokio::test]
    async fn a_relative_file_is_read_from_the_session_folder_and_the_shell_form_works_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        for c in ["info prog", "info ./prog", "nexo bin info prog", "bin info prog"] {
            let o = call(dir.path(), c).await;
            assert!(!o.is_error, "{c}: {}", o.content);
            assert!(o.content.to_lowercase().contains("elf") || o.content.contains("PE"), "{c}: {}", o.content);
        }
        let o = call(dir.path(), "overview prog --lines 12").await;
        assert!(!o.is_error, "{}", o.content);
        let missing = call(dir.path(), "info nothere").await;
        assert!(missing.is_error, "{}", missing.content);
    }

    #[tokio::test]
    async fn the_ops_that_save_are_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        for c in [
            "name prog 0x1000 mine",
            "note prog 0x1000 a note",
            "crypto prog --save",
            "patch prog --at 0x1000 --nop 1 --out prog2",
            "sigs",
        ] {
            let o = call(dir.path(), c).await;
            assert!(o.is_error && o.content.contains("refused"), "{c}: {}", o.content);
        }
        assert!(!dir.path().join("prog2").exists());
    }

    #[tokio::test]
    async fn help_a_missing_command_and_a_bad_line_answer_in_words() {
        let dir = tempfile::tempdir().unwrap();
        for c in ["help", "--help", "fn --help"] {
            let o = call(dir.path(), c).await;
            assert!(!o.is_error && !o.content.is_empty(), "{c}: {}", o.content);
        }
        let o = Bin.run(json!({}), &ToolContext::new(dir.path().to_path_buf())).await;
        assert!(o.is_error && o.content.contains("command"), "{}", o.content);
        let o = call(dir.path(), "info 'unclosed").await;
        assert!(o.is_error && o.content.contains("quote"), "{}", o.content);
        let o = call(dir.path(), "frobnicate x").await;
        assert!(o.is_error, "{}", o.content);
        assert_eq!(Bin.summary(&json!({"command": "fn p main"})), "bin: fn p main");
    }
}
