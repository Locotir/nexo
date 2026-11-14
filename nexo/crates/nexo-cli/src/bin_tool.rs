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
use clap::Parser;
use nexo_core::tool::Check;
use nexo_core::{Compact, Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

use crate::bin_cmd::BinOp;

const OPS: &str = "\
overview <file>  the map in one call: main, imports by capability, top functions and strings, next commands (start here)
info <file>  format, entry, sections, hardening
funcs <file> [--grep RE] [--sort size|callers] [--min-size N]  functions with size, callers, callees
fn <file> <name|addr> [--bytes] [--to ADDR]  one function disassembled (Intel syntax)
decomp <file> <name|addr>...  pseudo-C
xref <file> <name|addr> [--to ADDR|FN], xref <file> --str TEXT  who calls, reads, writes or points at it
calls <file> <name>, tree <file> <name> [--up], path <file> <from> <to>  callers, callees, call chains
cfg <file> <name|addr>  control-flow graph of one function: blocks, edges, loops, dominators (x86-64)
strings <file> [--grep RE], find <file> <hex|--imm N>, bytes <file> <addr> <len> [--as u32|char[16]|ptr|str|item[4]|a named or DWARF struct], slice, crypto, diff <old> <new>, names, managed (JVM, .NET), emu (one function in an emulator)
--lines N|all, --from K, --grep RE, --json after any op; `<op> --help` for its flags
a; b  several ops in one call, each answer under its own `> op` line; an error stops the rest
Never runs the file. name, note, proto, struct, var, sigs and crypto --save keep names, types and signatures for this binary in nexo's data folder. patch --out and sigs --out write a file: the person is asked";

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
                "Read a compiled program (ELF, PE, Mach-O, wasm, JVM, .NET) as bytes, without running it. One op per call (or `a; b`), the file first; answers come in bounded pages.\n{OPS}"
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
            description: "Read a compiled program (ELF, PE, Mach-O, wasm) without running it, instead of objdump or strings: `overview ./p`, `funcs`, `fn ./p main`, `decomp`, `xref`. `debug` runs it. `--help`",
            params: &[],
        })
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
        // `a; b` (or one op per line) is a script, as in `debug`: each answer under its own line, and an error
        // stops the rest (`debug`'s `do` does the same)
        let parts = units(line);
        let [_, _, ..] = parts.as_slice() else {
            return one(parts.first().map_or(line, String::as_str), ctx).await;
        };
        let mut out: Vec<String> = Vec::new();
        for (n, part) in parts.iter().enumerate() {
            out.push(format!("> {part}"));
            let o = one(part, ctx).await;
            if o.is_error {
                out.push(format!("error: {}", o.content));
                let left = parts[n + 1..].join("; ");
                if !left.is_empty() {
                    out.push(format!("... script stopped at the error; not run: {left}"));
                }
                break;
            }
            out.push(o.content);
        }
        ToolOutput::ok(out.join("\n"))
    }
}

/// The ops of one call: cut at `;` and at line ends outside double quotes, as `debug` cuts a script. A part with a
/// quote still open (a note that says `a; b` inside single quotes) is joined to the next, so text stays whole.
fn units(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for u in nexo_dbg::script_units(line) {
        match out.last_mut() {
            Some(last) if open => {
                last.push_str("; ");
                last.push_str(&u);
            }
            _ => out.push(u),
        }
        open = out.last().is_some_and(|l| crate::re_mcp::split_words(l).is_err());
    }
    out
}

/// One op line, understood: the answer of `--help`, or the op to run.
enum Parsed {
    Help(String),
    Op(Box<crate::bin_cmd::BinArgs>),
}

fn parse(unit: &str) -> Result<Parsed, String> {
    let words = crate::re_mcp::split_words(unit)?;
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
            return Ok(Parsed::Help(e.to_string()))
        }
        Err(e) => return Err(e.to_string()),
    };
    let Some(crate::cli::Cmd::Bin(b)) = parsed.cmd else {
        return Err("bin: not a bin op".into());
    };
    if b.op.is_none() {
        return Err("bin: give an op, for example `overview ./prog`; `--help` lists them".into());
    }
    Ok(Parsed::Op(b))
}

/// One op: the file is relative to the session's folder, as in the shell, and the rules that cover `read` and
/// `edit` cover the files named inside it.
async fn one(unit: &str, ctx: &ToolContext) -> ToolOutput {
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
    // an answer about one place in a program `debug` can run ends with the call that stops there
    let asked = tokio::task::spawn_blocking(move || {
        let text = crate::bin_cmd::execute(&b)?;
        let bridge = b.op.as_ref().and_then(|op| crate::bin_cmd::bridge::line(op, &text));
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
        // 298 chars before the text named `debug` (+12)
        assert!(sent.len() <= 310, "{} chars: {sent}", sent.len());
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

    fn kind(command: &str) -> ToolKind {
        Bin.call_kind(&json!({ "command": command }))
    }

    /// Every op reads or keeps names, notes and types in nexo's data folder (a read, as `debug`'s `name` and `note`
    /// are); the ops that write a file the person named are a write. A script is as strong as its strongest op.
    #[test]
    fn each_op_has_the_kind_of_what_it_does() {
        for c in [
            "overview prog",
            "info prog",
            "funcs prog --grep x",
            "fn prog main",
            "decomp prog main",
            "xref prog main",
            "calls prog main",
            "cfg prog main",
            "cfg prog main --grep loop --lines 20",
            "strings prog",
            "find prog 90",
            "bytes prog 0x1000 8",
            "names prog",
            "name prog 0x1000 mine",
            "name prog 0x1000 --clear",
            "note prog 0x1000 a note",
            "proto prog main 'int main(void)'",
            "struct prog Item 0:i32:id",
            "var prog main a1 count",
            "crypto prog --save",
            "sigs",
            "sigs --all",
            "nexo bin name prog 0x1000 mine",
            "help",
            "--help",
            "frobnicate x",
            "info 'unclosed",
        ] {
            assert_eq!(kind(c), ToolKind::Read, "{c}");
        }
        for c in [
            "patch prog --at 0x1000 --nop 1 --out prog2",
            "bin patch prog --fn main --bytes 90 --out prog2",
            "sigs --all --out s.sig",
            "info prog; patch prog --at 0x1000 --nop 1 --out prog2",
            "patch prog --at 0x1000 --nop 1 --out prog2\ninfo prog",
        ] {
            assert_eq!(kind(c), ToolKind::Write, "{c}");
        }
        // a `;` inside quotes is text, not a second op
        assert_eq!(kind("note prog 0x1 'x; patch prog --at 1 --nop 1 --out q'"), ToolKind::Read);
        assert_eq!(Bin.call_kind(&json!({})), ToolKind::Read);
        assert_eq!(Bin.kind(), ToolKind::Read);
    }

    /// What the person is asked, from the kind the tool reports, under each mode of the real policy.
    #[test]
    fn a_patch_is_asked_about_like_a_file_edit_and_the_naming_ops_are_not() {
        use nexo_agent::{Decision, Policy};
        use nexo_core::permission::PermissionMode;
        let cwd = std::path::Path::new("/work/proj");
        let decide = |mode, command: &str| {
            let args = json!({ "command": command });
            Policy { mode, ..Default::default() }.decide("bin", Bin.call_kind(&args), &args, cwd)
        };
        let patch = "patch prog --at 0x1000 --nop 1 --out prog2";
        // the same answers as `edit` of a file, in the default mode and where only reads run
        let edit = |mode| {
            Policy { mode, ..Default::default() }.decide("edit", ToolKind::Write, &json!({"path": "prog2"}), cwd)
        };
        assert_eq!(decide(PermissionMode::Ask, patch), Decision::Ask);
        assert_eq!(decide(PermissionMode::Ask, patch), edit(PermissionMode::Ask));
        assert!(matches!(decide(PermissionMode::ReadOnly, patch), Decision::Deny(_)));
        assert!(matches!(decide(PermissionMode::Plan, patch), Decision::Deny(_)));
        // a mode that lets writes through does not ask
        assert_eq!(decide(PermissionMode::Auto, patch), Decision::Allow);
        // a patch in a script asks for the script
        assert_eq!(decide(PermissionMode::Ask, &format!("info prog; {patch}")), Decision::Ask);
        // an allow rule for the tool covers it
        let args = json!({ "command": patch });
        let ruled = Policy { allow: vec!["bin".into()], ..Default::default() };
        assert_eq!(ruled.decide("bin", Bin.call_kind(&args), &args, cwd), Decision::Allow);
        // what only reads or names things is never asked, in the default mode, and read-only mode allows it
        for c in ["info prog", "name prog 0x1000 mine", "note prog 0x1000 hi", "proto prog main 'int f(void)'", "sigs"]
        {
            for mode in [PermissionMode::Ask, PermissionMode::ReadOnly] {
                assert_eq!(decide(mode, c), Decision::Allow, "{c}");
            }
        }
    }

    /// A rule that denies writing the place stops the patch, and nothing is written; the ask is the turn's, so a
    /// rule that only asks does not ask a second time.
    #[tokio::test]
    async fn a_patch_writes_the_copy_in_the_session_folder_unless_a_rule_denies_the_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        let patch = "patch prog --at 0x1000 --nop 1 --out prog2";
        let ctx = rules(&["edit(**/prog2)"], dir.path());
        let o = Bin.run(json!({ "command": patch }), &ctx).await;
        assert!(o.is_error && o.content.contains("denies writing"), "{}", o.content);
        assert!(!dir.path().join("prog2").exists());
        let o = Bin.run(json!({ "command": patch }), &ToolContext::new(dir.path().to_path_buf())).await;
        // 0x1000 is in this test program's headers or code: either the copy exists or the answer says why not
        if o.is_error {
            eprintln!("patch refused by nexo-bin: {}", o.content);
            assert!(!o.content.contains("refused"), "{}", o.content);
        } else {
            assert!(dir.path().join("prog2").is_file(), "{}", o.content);
        }
    }

    #[tokio::test]
    async fn two_ops_in_one_call_answer_in_order_each_under_its_own_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        let o = call(dir.path(), "info prog; strings prog --lines 3").await;
        assert!(!o.is_error, "{}", o.content);
        let (a, b) = (o.content.find("> info prog").unwrap(), o.content.find("> strings prog --lines 3").unwrap());
        assert!(a < b, "{}", o.content);
        let alone = call(dir.path(), "info prog").await.content;
        assert!(o.content[a..b].contains(alone.lines().next().unwrap()), "{}", o.content);
        assert!(!o.content.contains("script stopped"), "{}", o.content);
        // one op per line is the same call, and one op alone has no header, as before
        let lines = call(dir.path(), "info prog\nstrings prog --lines 3").await;
        assert_eq!(lines.content, o.content);
        assert!(!alone.contains("> info"), "{alone}");
        assert_eq!(call(dir.path(), "info prog;").await.content, alone);
    }

    /// As `debug`'s `do`: the error is reported in its place, the ops after it are named and not run, and the call
    /// itself is not an error.
    #[tokio::test]
    async fn an_error_in_a_script_is_reported_and_the_rest_is_not_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        let o = call(dir.path(), "info nothere; info prog; info prog").await;
        assert!(!o.is_error, "{}", o.content);
        assert!(o.content.starts_with("> info nothere\nerror: bin: "), "{}", o.content);
        assert!(o.content.contains("... script stopped at the error; not run: info prog; info prog"), "{}", o.content);
        assert!(!o.content.contains("> info prog"), "{}", o.content);
        // an error in the last op has nothing left to name; a bad word is an error of its own part
        let last = call(dir.path(), "info prog; frobnicate x").await;
        assert!(last.content.contains("> info prog") && last.content.contains("error: "), "{}", last.content);
        assert!(!last.content.contains("script stopped"), "{}", last.content);
    }

    #[test]
    fn a_semicolon_in_quotes_is_text_and_not_the_end_of_an_op() {
        assert_eq!(units("info a; info b"), ["info a", "info b"]);
        assert_eq!(units("note p 0x1 'a; b'"), ["note p 0x1 'a; b'"]);
        assert_eq!(units("note p 0x1 \"a; b\"; info p"), ["note p 0x1 \"a; b\"", "info p"]);
        assert_eq!(units("note p 0x1 'a; b'; info p"), ["note p 0x1 'a; b'", "info p"]);
        assert!(units("# only a comment").is_empty());
    }

    /// The rules of a session: a real policy, asked the way the turn asks it.
    fn rules(deny: &[&str], cwd: &std::path::Path) -> ToolContext {
        let policy = nexo_agent::Policy { deny: deny.iter().map(|d| d.to_string()).collect(), ..Default::default() };
        let rules_cwd = cwd.to_path_buf();
        let mut ctx = ToolContext::new(cwd.to_path_buf());
        ctx.check = Some(std::sync::Arc::new(move |tool: &str, kind, args: &Value| {
            match policy.decide(tool, kind, args, &rules_cwd) {
                nexo_agent::Decision::Allow => Check::Allow,
                nexo_agent::Decision::Ask => Check::Ask,
                nexo_agent::Decision::Deny(_) => Check::Deny,
            }
        }));
        ctx
    }

    #[tokio::test]
    async fn the_rules_that_cover_read_cover_the_files_named_inside_the_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("locked")).unwrap();
        std::fs::copy(this_binary(), dir.path().join("locked").join("prog")).unwrap();
        std::fs::copy(this_binary(), dir.path().join("open")).unwrap();
        let ctx = rules(&["read(**/locked/*)"], dir.path());
        // relative, absolute, in the shell form, and as the second file of a diff: all the same file to the rules
        let abs = dir.path().join("locked/prog");
        for c in [
            "info locked/prog".to_string(),
            format!("info {}", abs.display()),
            "nexo bin overview ./locked/prog".to_string(),
            "diff open locked/prog".to_string(),
            "diff locked/prog open".to_string(),
        ] {
            let o = Bin.run(json!({ "command": c }), &ctx).await;
            assert!(o.is_error && o.content.contains("denies reading"), "{c}: {}", o.content);
        }
        // a file the rules do not name is read, with the same context
        let o = Bin.run(json!({"command": "info open"}), &ctx).await;
        assert!(!o.is_error, "{}", o.content);
        // no rules at all (no check): read as before
        let o = call(dir.path(), "info locked/prog").await;
        assert!(!o.is_error, "{}", o.content);
    }

    #[tokio::test]
    async fn a_file_the_rules_ask_about_is_not_read_without_the_person() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        ctx.check = Some(std::sync::Arc::new(move |tool: &str, kind, args: &Value| {
            log.lock().unwrap().push((tool.to_string(), kind, args["path"].as_str().unwrap().to_string()));
            Check::Ask
        }));
        let o = Bin.run(json!({"command": "info prog"}), &ctx).await;
        assert!(o.is_error && o.content.contains("needs the person's OK"), "{}", o.content);
        let asked = seen.lock().unwrap().clone();
        assert_eq!(
            asked,
            [("read".to_string(), ToolKind::Read, dir.path().join("prog").to_string_lossy().to_string())]
        );
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

    /// Both directions on a real program: the `bin` answer names the `debug` call, the call stops where the
    /// answer looked, and the stop names the `bin` call that reads the function it is in.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[tokio::test]
    async fn a_bin_answer_and_a_debug_stop_name_each_others_calls() {
        use clap::Parser;
        let dir = tempfile::tempdir().unwrap();
        let Some(exe) = crate::bin_cmd::bridge::tests::build(dir.path(), "prog", &["-g0"]) else {
            return eprintln!("skipped: no cc");
        };
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let answer = Bin.run(json!({"command": "fn prog helper"}), &ctx).await;
        assert!(!answer.is_error, "{}", answer.content);
        let last = answer.content.lines().last().unwrap();
        assert_eq!(answer.content.matches("to see it live").count(), 1, "{}", answer.content);
        let call = last
            .strip_prefix("to see it live: debug `")
            .and_then(|l| l.strip_suffix('`'))
            .unwrap_or_else(|| panic!("{last}"));
        // every unit of the call parses in the debugger, and the break is at the address the answer is about
        let helper = match nexo_bin::place_of(&nexo_bin::Image::open(&exe).unwrap(), "helper").unwrap() {
            nexo_bin::Place::Code(a) => a,
            other => panic!("{other:?}"),
        };
        let units = nexo_dbg::script_units(call);
        assert_eq!(units.len(), 3, "{units:?}");
        for u in &units {
            nexo_dbg::parse(&nexo_dbg::split_words(u)).unwrap_or_else(|e| panic!("{u}: {e}"));
        }
        assert_eq!(
            nexo_dbg::parse(&nexo_dbg::split_words(&units[1])).unwrap().words,
            ["break", &format!("*{helper:#x}")]
        );
        assert!(answer.content.contains("helper"), "{}", answer.content);
        // the call stops there, and the stop names the `bin` call for the function
        let state = tempfile::tempdir().unwrap();
        let dbg = nexo_tools::debug::Debug::in_dir(state.path());
        let stop = dbg.run(json!({"command": call}), &ctx).await;
        assert!(!stop.is_error, "{}", stop.content);
        assert!(stop.content.contains("helper"), "{}", stop.content);
        let bridge = stop.content.lines().last().unwrap().to_string();
        let words = bridge.strip_prefix("to read this function: bin ").unwrap_or_else(|| panic!("{}", stop.content));
        let argv: Vec<String> =
            ["nexo", "bin"].iter().map(|s| s.to_string()).chain(crate::re_mcp::split_words(words).unwrap()).collect();
        assert!(
            matches!(crate::cli::Args::try_parse_from(&argv).unwrap().cmd, Some(crate::cli::Cmd::Bin(_))),
            "{words}"
        );
        let read = Bin.run(json!({"command": words}), &ctx).await;
        assert!(!read.is_error && read.content.contains("helper"), "{}", read.content);
        // stepping inside the same function does not repeat the line
        let again = dbg.run(json!({"command": "step"}), &ctx).await;
        assert!(!again.content.contains("to read this function"), "{}", again.content);
        dbg.run(json!({"command": "end"}), &ctx).await;
    }
}
