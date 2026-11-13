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

use nexo_core::message::{Message, Part};
use serde_json::Value;

use crate::hint_cmd::{self, Offer, Seg};

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

/// The op of `nexo bin` that gives what `prog` was asked for, with the file.
fn op_for(prog: &str, args: &[String], file: &str, next: Option<&Seg>) -> String {
    let f = hint_cmd::quoted(file);
    let has = |short: char, long: &str| {
        args.iter().any(|a| {
            (a.starts_with('-') && !a.starts_with("--") && a.contains(short))
                || (!long.is_empty() && a.starts_with(long))
        })
    };
    let valued = |long: &str| {
        args.iter().enumerate().find_map(|(i, a)| match a.strip_prefix(long) {
            Some(v) if v.starts_with('=') => Some(v[1..].to_string()),
            Some("") => args.get(i + 1).cloned(),
            _ => None,
        })
    };
    // a symbol or an address the call can carry: C++ and Rust names have `<>` and `~`, none has a `$` that a shell
    // would read, and one that starts with `-` would be taken for an option
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 200
            && !s.starts_with('-')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || "_.@:<>~+-".contains(c))
    };
    // written as one word in the tool's language and in a shell (`<>` stay inside double quotes)
    let word = |s: &str| hint_cmd::quoted(s);
    // `--disassemble=SYM` only: a bare `--disassemble` takes no value, the word after it is the file
    let eq = |long: &str| args.iter().find_map(|a| a.strip_prefix(long)?.strip_prefix('=').map(str::to_string));
    match prog {
        "objdump" => {
            if let Some(sym) = eq("--disassemble").filter(|s| plain(s)) {
                format!("fn {f} {}", word(&sym))
            } else if let Some(at) = valued("--start-address").filter(|s| plain(s)) {
                // `--stop-address` is where the same range ends: `fn --to`
                match valued("--stop-address").filter(|s| plain(s)) {
                    Some(to) => format!("fn {f} {} --to {}", word(&at), word(&to)),
                    None => format!("fn {f} {}", word(&at)),
                }
            } else if has('d', "--disassemble") || has('D', "--disassemble-all") || has('S', "--source") {
                format!("overview {f}")
            } else if has('t', "--syms") || has('T', "--dynamic-syms") {
                format!("funcs {f}")
            } else {
                format!("info {f}")
            }
        }
        "readelf" => {
            if has('s', "--syms") || has('s', "--symbols") || has('s', "--dyn-syms") {
                format!("funcs {f}")
            } else if has('p', "--string-dump") || has('x', "--hex-dump") {
                format!("strings {f}")
            } else {
                format!("info {f}")
            }
        }
        "nm" => format!("funcs {f}"),
        "strings" => match grep_after(next) {
            Some(pat) => format!("strings {f} --grep {}", hint_cmd::quoted(&pat)),
            None => format!("strings {f}"),
        },
        "r2" | "radare2" | "rizin" => {
            let cmds = args.iter().position(|a| a == "-c" || a == "-qc" || a == "-Qc").and_then(|i| args.get(i + 1));
            match cmds.map(|c| c.split(';').map(str::trim).collect::<Vec<_>>()) {
                Some(c) => {
                    let at = c.iter().find_map(|x| {
                        let (cmd, sym) = x.split_once('@')?;
                        let sym = sym.trim();
                        (cmd.trim().starts_with("pd") && plain(sym)).then(|| sym.to_string())
                    });
                    if let Some(sym) = at {
                        format!("fn {f} {}", word(&sym))
                    } else if c.iter().any(|x| x.starts_with("afl")) {
                        format!("funcs {f}")
                    } else if c.iter().any(|x| x.starts_with("iz")) {
                        format!("strings {f}")
                    } else if c.iter().any(|x| x.starts_with("ii") || x.starts_with("iI") || x.starts_with("iS")) {
                        format!("info {f}")
                    } else {
                        format!("overview {f}")
                    }
                }
                None => format!("overview {f}"),
            }
        }
        // a dump of bytes: the map first, then `bytes <file> <addr> <len>` for the part wanted
        "xxd" | "hexdump" | "od" => format!("info {f}"),
        // gdb with no debug tool offered
        _ => format!("overview {f}"),
    }
}

/// `| grep PATTERN` right after `strings`: the pattern, as `strings --grep` writes it (`(?i)` for `-i`).
fn grep_after(next: Option<&Seg>) -> Option<String> {
    let s = next?;
    let first = hint_cmd::program(s.words.first()?);
    if !matches!(first.as_str(), "grep" | "egrep" | "rg") {
        return None;
    }
    let flags: String = s.words[1..].iter().filter(|w| w.starts_with('-')).flat_map(|w| w.chars().skip(1)).collect();
    // a flag that changes what matches (inverted, fixed text, whole words, counts) is not a plain pattern
    if flags
        .chars()
        .any(|c| matches!(c, 'v' | 'F' | 'w' | 'x' | 'c' | 'l' | 'L' | 'o' | 'A' | 'B' | 'C' | 'e' | 'f' | 'm'))
    {
        return None;
    }
    let pat = s.words[1..].iter().find(|w| !w.starts_with('-'))?;
    let ok =
        !pat.is_empty() && pat.len() <= 60 && pat.chars().all(|c| c.is_ascii_alphanumeric() || "_.-:/ |".contains(c));
    ok.then(|| if flags.contains('i') { format!("(?i){pat}") } else { pat.clone() })
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
mod tests {
    use super::*;
    use serde_json::json;

    const TOOL: Offer = Offer { bin: true, debug: true };
    const SHELL: Offer = Offer { bin: false, debug: false };

    /// This test program: an ELF or PE x86-64 file `nexo_bin::opens`, read as bytes only.
    fn this_binary() -> std::path::PathBuf {
        std::env::current_exe().unwrap()
    }

    /// A path as a command and a call write it: `/` for the separator on Windows too.
    fn slashed(p: impl AsRef<Path>) -> String {
        hint_cmd::slash(&p.as_ref().display().to_string())
    }

    fn call(id: &str, command: &str) -> (String, String, Value) {
        (id.into(), "bash".into(), json!({"command": command}))
    }

    fn result(id: &str, text: &str) -> Part {
        Part::tool_result(id, text, false)
    }

    fn texts(parts: &[Part]) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).collect(),
                _ => String::new(),
            })
            .collect()
    }

    fn wanted(command: &str, cwd: &Path) -> bool {
        line(command, cwd, TOOL).is_some()
    }

    #[test]
    fn a_command_that_never_ran_gets_no_line_and_does_not_spend_it() {
        let cwd = tempfile::tempdir().unwrap();
        let command = format!("objdump -d '{}'", slashed(this_binary()));
        for refusal in ["Permission denied: the person said no", "Blocked by a hook: no", "interrupted by the person"] {
            let mut told = false;
            let out = round(
                &mut told,
                &[call("c1", &command)],
                vec![Part::tool_result("c1", refusal, true)],
                cwd.path(),
                TOOL,
            );
            assert_eq!(texts(&out), [refusal.to_string()], "{refusal}");
            assert!(!told, "{refusal}: the one line of the session is still there for a command that runs");
        }
        // a command that ran and failed (the reader is not installed) is where the line helps most
        let mut told = false;
        let failed = "sh: objdump: command not found\n\n[exit code 127]";
        let out =
            round(&mut told, &[call("c1", &command)], vec![Part::tool_result("c1", failed, true)], cwd.path(), TOOL);
        assert!(texts(&out)[0].contains(HEAD) && told, "{:?}", texts(&out));
    }

    #[test]
    fn a_line_that_already_uses_nexo_bin_gets_none_even_with_a_reader_in_it() {
        let cwd = tempfile::tempdir().unwrap();
        let exe = slashed(this_binary());
        assert!(wanted(&format!("echo x; xxd '{exe}'"), cwd.path()), "the same line without nexo bin asks for it");
        assert!(!wanted(&format!("nexo bin fn '{exe}' main; xxd '{exe}'"), cwd.path()));
        assert!(!wanted(&format!("nexo bin info '{exe}' | strings '{exe}'"), cwd.path()));
    }

    /// A named pipe nobody writes to: opening it never returns, so its kind is asked first.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let pipe = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("no mkfifo here: not checked");
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let cwd = dir.path().to_path_buf();
        std::thread::spawn(move || tx.send(wanted("strings pipe", &cwd)));
        let got = rx.recv_timeout(std::time::Duration::from_secs(5));
        // let a reader that did block go, so no thread outlives the test; opened for both ways, which never waits
        if got.is_err() {
            let _ = std::fs::OpenOptions::new().read(true).write(true).open(&pipe);
        }
        assert_eq!(got, Ok(false), "the check must come back at once and say no");
        assert!(!nexo_bin::opens(dir.path()), "a folder is not a program either");
    }

    #[test]
    fn a_webassembly_module_asks_for_the_line_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("m.wasm"), b"\0asm\x01\0\0\0").unwrap();
        assert!(wanted("strings m.wasm", dir.path()));
        assert!(!wanted("strings notes.txt", dir.path()));
    }

    #[test]
    fn the_line_names_the_exact_call_as_a_tool_call_or_in_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        let l = line("objdump -d prog | head -50", dir.path(), TOOL).unwrap();
        assert_eq!(
            l,
            "[nexo: the bin tool reads this file in pages, with names and references: bin {\"command\": \"overview prog\"}, then fn|funcs|xref|decomp|strings]"
        );
        let l = line("objdump -d prog | head -50", dir.path(), SHELL).unwrap();
        assert_eq!(
            l,
            "[nexo: nexo bin reads this file in pages, with names and references: nexo bin overview prog, then fn|funcs|xref|decomp|strings]"
        );
        // a name with a space is quoted for the language of the tool, and cannot break the JSON
        std::fs::copy(this_binary(), dir.path().join("my prog")).unwrap();
        let l = line("nm 'my prog'", dir.path(), TOOL).unwrap();
        assert!(l.contains("funcs \\\"my prog\\\""), "{l}");
    }

    #[test]
    fn each_reader_asks_for_the_op_that_gives_the_same() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("p")).unwrap();
        let op = |c: &str| {
            let l = line(c, dir.path(), SHELL).unwrap_or_else(|| panic!("no line for {c}"));
            let rest = l.strip_prefix(HEAD_SHELL).unwrap().trim_start().strip_prefix("nexo bin ").unwrap().to_string();
            rest.trim_end_matches(&format!(", {THEN}]")).to_string()
        };
        assert_eq!(op("objdump -d p"), "overview p");
        assert_eq!(op("objdump -d -M intel p"), "overview p");
        assert_eq!(op("objdump -dS p"), "overview p");
        assert_eq!(op("objdump --disassemble=main p"), "fn p main");
        assert_eq!(op("objdump --start-address=0x401000 -d p"), "fn p 0x401000");
        // the paste: a range of one binary, both ends, either way of writing the option
        assert_eq!(op("objdump -d --start-address=0x11950 --stop-address=0x140a0 p"), "fn p 0x11950 --to 0x140a0");
        assert_eq!(op("objdump -d --start-address 0x11950 --stop-address 0x140a0 p"), "fn p 0x11950 --to 0x140a0");
        assert_eq!(op("objdump -d --stop-address=0x140a0 --start-address=0x11950 p"), "fn p 0x11950 --to 0x140a0");
        assert_eq!(op("objdump -d --start-address=0x11950 p"), "fn p 0x11950");
        // a bare `--disassemble` has no symbol: the word after it is the file, not a name
        assert_eq!(op("objdump --disassemble p"), "overview p");
        // a symbol is one word for the language of the tool and for a shell: `<>` and `~` are quoted, `$` and a
        // leading dash never get through
        assert_eq!(op("objdump '--disassemble=foo<int>::bar' p"), "fn p \"foo<int>::bar\"");
        assert_eq!(op("objdump --disassemble=~Foo p"), "fn p \"~Foo\"");
        assert_eq!(op("objdump '--disassemble=a$b' p"), "overview p");
        assert_eq!(op("objdump --disassemble=-x p"), "overview p");
        assert_eq!(op("r2 -qc 'pdf @ sym.a<b>' p"), "fn p \"sym.a<b>\"");
        assert_eq!(op("objdump -t p"), "funcs p");
        assert_eq!(op("objdump -T p"), "funcs p");
        assert_eq!(op("objdump -x p"), "info p");
        assert_eq!(op("objdump -h p"), "info p");
        assert_eq!(op("readelf -s p"), "funcs p");
        assert_eq!(op("readelf -Ws p"), "funcs p");
        assert_eq!(op("readelf -S p"), "info p");
        assert_eq!(op("readelf -p .rodata p"), "strings p");
        assert_eq!(op("nm -C p"), "funcs p");
        assert_eq!(op("strings -n 8 p"), "strings p");
        assert_eq!(op("strings p | grep -i http"), "strings p --grep \"(?i)http\"");
        assert_eq!(op("strings p | grep -v http"), "strings p");
        assert_eq!(op("strings p | grep 'a|b' | head"), "strings p --grep \"a|b\"");
        assert_eq!(op("xxd p | head"), "info p");
        assert_eq!(op("hexdump -C p"), "info p");
        assert_eq!(op("od -x p"), "info p");
        assert_eq!(op("r2 -A p"), "overview p");
        assert_eq!(op("r2 -qc 'pdf @ main' p"), "fn p main");
        assert_eq!(op("rizin -c 'aaa; afl' p"), "funcs p");
        assert_eq!(op("r2 -c iz p"), "strings p");
        // gdb is the debugger's where the debug tool is offered, and the map where it is not
        assert_eq!(line("gdb -batch p", dir.path(), TOOL), None);
        assert_eq!(op("gdb -batch p"), "overview p");
    }

    #[test]
    fn the_shapes_of_a_real_session_are_read_absolute_paths_cd_redirects_and_wrappers() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("tests");
        std::fs::create_dir(&sub).unwrap();
        std::fs::copy(this_binary(), sub.join("c-lcrypt")).unwrap();
        let abs = slashed(sub.join("c-lcrypt"));
        // an absolute path, with the output redirected and piped
        assert!(wanted(&format!("objdump -d {abs} 2>&1 | head -100"), dir.path()));
        assert!(wanted(&format!("objdump -d {abs} > /tmp/dump.txt 2>/dev/null"), dir.path()));
        // `cd` first: the file is found from there, and the call carries the whole path
        let l = line("cd tests && objdump -d c-lcrypt | head", dir.path(), TOOL).unwrap();
        assert!(l.contains(&hint_cmd::tool_call("bin", &format!("overview {}", hint_cmd::quoted(&abs)))), "{l}");
        assert!(!wanted("objdump -d c-lcrypt", dir.path()), "from here the name is nothing");
        // wrappers, variables, a script run by the shell
        assert!(wanted(&format!("sudo -n nm {abs}"), dir.path()));
        assert!(wanted(&format!("LC_ALL=C strings {abs}"), dir.path()));
        assert!(wanted(&format!("timeout 10 readelf -h {abs}"), dir.path()));
        assert!(wanted("bash -c 'cd tests; strings c-lcrypt'", dir.path()));
    }

    #[test]
    fn not_for_text_missing_other_commands_or_nexo_bin_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello\nworld\n").unwrap();
        std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        let exe = slashed(this_binary());
        for c in [
            "strings notes.txt".to_string(),
            "objdump -d run.sh".into(),
            "objdump -d missing.out".into(),
            "objdump -d .".into(),
            format!("nexo bin fn '{exe}' main"),
            format!("nexo bin strings '{exe}'; strings '{exe}'"),
            format!("cat '{exe}'"),
            format!("ls -l '{exe}'"),
            format!("file '{exe}'"),
            format!("echo objdump '{exe}'"),
            format!("which objdump '{exe}'"),
            String::new(),
        ] {
            assert!(!wanted(&c, dir.path()), "{c}");
        }
    }

    #[test]
    fn a_hostile_name_is_only_text_nothing_runs_or_expands() {
        let dir = tempfile::tempdir().unwrap();
        let exe = this_binary();
        let shown = slashed(&exe);
        // each of these would create `touched` if anything ran it; none is a file nexo bin opens
        for c in [
            format!("objdump -d $(touch touched) '{}'", shown),
            format!("objdump -d `touch touched` '{}'", shown),
            "objdump -d '$(touch touched)'".to_string(),
            "objdump -d 'a\ntouch touched'".to_string(),
            "objdump -d\ntouch touched".to_string(),
            "objdump -d -- -rf".to_string(),
            format!("objdump -d '-{}'", shown),
            "strings ; touch touched".to_string(),
            "strings <(touch touched)".to_string(),
        ] {
            assert!(!wanted(&c, dir.path()), "{c:?}");
        }
        assert!(!dir.path().join("touched").exists());
        // a dash or a newline inside a real name is just a name, and a name the call cannot carry gets no line
        let odd = dir.path().join("a\nb-$(touch touched)");
        if std::fs::copy(&exe, &odd).is_ok() {
            assert!(!wanted("objdump -d 'a\nb-$(touch touched)'", dir.path()), "no newline in a call");
            assert!(!dir.path().join("touched").exists());
        }
        let quoted = dir.path().join("a\"b");
        if std::fs::copy(&exe, &quoted).is_ok() {
            assert!(!wanted("objdump -d 'a\"b'", dir.path()), "no double quote in a call");
        }
        let dashed = dir.path().join("-prog");
        std::fs::copy(&exe, &dashed).unwrap();
        assert!(!wanted("objdump -d -prog", dir.path()), "an option, not a file");
        assert!(wanted("objdump -d ./-prog", dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_binary_is_read_as_bytes_and_a_link_to_nothing_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(this_binary(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("dangling")).unwrap();
        assert!(wanted("nm link", dir.path()));
        assert!(!wanted("nm dangling", dir.path()));
    }

    #[test]
    fn the_line_closes_the_first_result_that_earns_it_and_only_that_one() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", slashed(this_binary()));
        let calls = vec![call("a", "ls"), call("b", &hit), call("c", &hit)];
        let results = vec![result("a", "files"), result("b", "disassembly"), result("c", "again")];
        let mut told = false;
        let out = round(&mut told, &calls, results, dir.path(), TOOL);
        assert!(told);
        let said = texts(&out);
        assert_eq!((said[0].as_str(), said[2].as_str()), ("files", "again"));
        assert!(said[1].starts_with("disassembly\n\n[nexo: the bin tool reads this file"), "{said:?}");
        // later rounds: nothing
        let again = round(&mut told, &calls, vec![result("b", "more")], dir.path(), TOOL);
        assert_eq!(texts(&again), ["more"]);
    }

    #[test]
    fn a_failed_command_still_gets_the_line_and_a_background_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", slashed(this_binary()));
        let mut told = false;
        let bg = vec![("a".to_string(), "bash".to_string(), json!({"command": hit, "background": true}))];
        let out = round(&mut told, &bg, vec![result("a", "job started")], dir.path(), TOOL);
        assert!(!told && texts(&out) == ["job started"]);
        let calls = vec![call("b", &hit)];
        let err = Part::tool_result("b", "bash: objdump: command not found\n\n[exit code 127]", true);
        let out = round(&mut told, &calls, vec![err], dir.path(), TOOL);
        assert!(told);
        assert!(texts(&out)[0].contains(HEAD));
        assert!(matches!(out[0], Part::ToolResult { is_error: true, .. }));
    }

    #[test]
    fn only_bash_calls_are_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", slashed(this_binary()));
        let calls = vec![("a".to_string(), "read".to_string(), json!({"command": hit}))];
        let mut told = false;
        let out = round(&mut told, &calls, vec![result("a", "x")], dir.path(), TOOL);
        assert!(!told && texts(&out) == ["x"]);
    }

    #[test]
    fn told_finds_either_line_and_the_old_one_in_a_kept_history_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", slashed(this_binary()));
        for offer in [TOOL, SHELL] {
            let l = line(&hit, dir.path(), offer).unwrap();
            let with = Message::tool_results(vec![result("b", &format!("out\n\n{l}"))]);
            assert!(told(&[Message::user("hi"), with]));
        }
        let old = Message::tool_results(vec![result("b", &format!("out\n\n{OLD_LINE}"))]);
        assert!(told(&[old]), "a session saved with the old line has it");
        let without = Message::tool_results(vec![result("b", "out")]);
        // the model quoting the line is not nexo having said it
        let quoted = Message::assistant(HEAD);
        assert!(!told(&[Message::user(HEAD), without, quoted]));
        assert!(!told(&[]));
    }
}
