//! The first time a model reads a compiled program by hand (`objdump`, `strings`, `xxd`, ...), the result of
//! that command ends with one line saying `nexo bin` exists. Once per session, and only when the file the
//! command names is one `nexo bin` opens. No tool, no schema and no system text carry it: a person who never
//! opens a binary pays nothing. The line is part of the result, so it stays in the history, the saved session
//! and a compaction summary's source like any output; `told` tells a resumed or rewound history from a fresh one.
//!
//! Nothing here runs or loads a file. The command is split by the parser the bash tool judges commands with, and
//! a name it finds is only opened as bytes by `nexo_bin::opens`, which reads at most the first 4096.

use std::path::Path;

use nexo_core::message::{Message, Part};
use serde_json::Value;

/// The line added to the result. It starts with `[nexo:` so it is told apart from the command's own output.
pub const LINE: &str = "[nexo: this file can be read in pages with names and references: nexo bin info|funcs|fn|xref|calls|strings|find <file> ... (nexo bin --help)]";

/// The programs people use to read a compiled program by hand.
const READERS: [&str; 11] =
    ["objdump", "readelf", "nm", "strings", "xxd", "hexdump", "od", "gdb", "r2", "radare2", "rizin"];

/// The most words of one command whose file is looked at.
const WORDS: usize = 12;

/// Whether `command`, run in `cwd`, reads a file `nexo bin` opens with one of the `READERS`.
pub fn wanted(command: &str, cwd: &Path) -> bool {
    let Some(parts) = nexo_tools::bash::command_words(command) else { return false };
    // a command that already is `nexo bin` needs no hint
    if parts.iter().any(|p| p.first().is_some_and(|w| program(w) == "nexo") && p.get(1).is_some_and(|w| w == "bin")) {
        return false;
    }
    parts
        .iter()
        .filter(|p| p.first().is_some_and(|w| READERS.contains(&program(w).as_str())))
        .flat_map(|p| p.iter().skip(1).take(WORDS))
        // an option is not a file; a name that starts with a dash is not looked at
        .filter(|w| !w.is_empty() && !w.starts_with('-'))
        .any(|w| nexo_bin::opens(&cwd.join(w)))
}

/// The program a command word names, without its folder or `.exe`, in lower case.
fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

/// Whether `messages` already hold the line in a tool result.
pub fn told(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        m.content.iter().any(|p| match p {
            Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).any(|t| t.contains(LINE)),
            _ => false,
        })
    })
}

/// The results of one round with the line added to the first that earns it. `told` is whether the session has
/// had it already; it is set when the line is added. Calls with `background` or `detach` have no output yet.
pub fn round(told: &mut bool, calls: &[(String, String, Value)], results: Vec<Part>, cwd: &Path) -> Vec<Part> {
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
            if !*told && command.is_some_and(|c| wanted(c, cwd)) {
                *told = true;
                let text: String = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
                content = vec![Part::text(format!("{text}\n\n{LINE}"))];
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

    /// This test program: an ELF or PE x86-64 file `nexo_bin::opens`, read as bytes only.
    fn this_binary() -> std::path::PathBuf {
        std::env::current_exe().unwrap()
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

    #[test]
    fn the_line_is_the_agreed_text() {
        assert_eq!(
            LINE,
            "[nexo: this file can be read in pages with names and references: nexo bin info|funcs|fn|xref|calls|strings|find <file> ... (nexo bin --help)]"
        );
    }

    #[test]
    fn every_reader_with_a_binary_asks_for_the_line() {
        let cwd = tempfile::tempdir().unwrap();
        let exe = this_binary();
        for r in READERS {
            assert!(wanted(&format!("{r} -x '{}'", exe.display()), cwd.path()), "{r}");
        }
        assert!(wanted(&format!("/usr/bin/objdump -d '{}' | head -50", exe.display()), cwd.path()));
        assert!(wanted(&format!("strings '{}' 2>&1 | grep http", exe.display()), cwd.path()));
        assert!(wanted(&format!("cat x; xxd '{}'", exe.display()), cwd.path()));
    }

    #[test]
    fn a_name_is_looked_up_from_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
        assert!(wanted("objdump -d prog", dir.path()));
        assert!(wanted("objdump -d ./prog", dir.path()));
        assert!(!wanted("objdump -d prog", &dir.path().join("elsewhere")));
    }

    #[test]
    fn not_for_text_missing_other_commands_or_nexo_bin_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello\nworld\n").unwrap();
        std::fs::write(dir.path().join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        let exe = this_binary();
        let exe = exe.display();
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
            String::new(),
        ] {
            assert!(!wanted(&c, dir.path()), "{c}");
        }
    }

    #[test]
    fn a_hostile_name_is_only_text_nothing_runs_or_expands() {
        let dir = tempfile::tempdir().unwrap();
        let exe = this_binary();
        // each of these would create `touched` if anything ran it; none is a file nexo bin opens
        for c in [
            format!("objdump -d $(touch touched) '{}'", exe.display()),
            format!("objdump -d `touch touched` '{}'", exe.display()),
            "objdump -d '$(touch touched)'".to_string(),
            "objdump -d 'a\ntouch touched'".to_string(),
            "objdump -d\ntouch touched".to_string(),
            "objdump -d -- -rf".to_string(),
            format!("objdump -d '-{}'", exe.display()),
            "strings ; touch touched".to_string(),
        ] {
            assert!(!wanted(&c, dir.path()), "{c:?}");
        }
        assert!(!dir.path().join("touched").exists());
        // a dash or a newline inside a real name is just a name
        let odd = dir.path().join("a\nb-$(touch touched)");
        if std::fs::copy(&exe, &odd).is_ok() {
            assert!(wanted("objdump -d 'a\nb-$(touch touched)'", dir.path()));
            assert!(!dir.path().join("touched").exists());
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
        let exe = this_binary();
        let hit = format!("objdump -d '{}'", exe.display());
        let calls = vec![call("a", "ls"), call("b", &hit), call("c", &hit)];
        let results = vec![result("a", "files"), result("b", "disassembly"), result("c", "again")];
        let mut told = false;
        let out = round(&mut told, &calls, results, dir.path());
        assert!(told);
        assert_eq!(texts(&out), ["files".to_string(), format!("disassembly\n\n{LINE}"), "again".to_string()]);
        // later rounds: nothing
        let again = round(&mut told, &calls, vec![result("b", "more")], dir.path());
        assert_eq!(texts(&again), ["more"]);
    }

    #[test]
    fn a_failed_command_still_gets_the_line_and_a_background_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", this_binary().display());
        let mut told = false;
        let bg = vec![("a".to_string(), "bash".to_string(), json!({"command": hit, "background": true}))];
        let out = round(&mut told, &bg, vec![result("a", "job started")], dir.path());
        assert!(!told && texts(&out) == ["job started"]);
        let calls = vec![call("b", &hit)];
        let err = Part::tool_result("b", "bash: objdump: command not found", true);
        let out = round(&mut told, &calls, vec![err], dir.path());
        assert!(told);
        assert!(texts(&out)[0].ends_with(LINE));
        assert!(matches!(out[0], Part::ToolResult { is_error: true, .. }));
    }

    #[test]
    fn only_bash_calls_are_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let hit = format!("objdump -d '{}'", this_binary().display());
        let calls = vec![("a".to_string(), "read".to_string(), json!({"command": hit}))];
        let mut told = false;
        let out = round(&mut told, &calls, vec![result("a", "x")], dir.path());
        assert!(!told && texts(&out) == ["x"]);
    }

    #[test]
    fn told_finds_the_line_in_a_kept_history_and_nothing_else() {
        let with = Message::tool_results(vec![result("b", &format!("out\n\n{LINE}"))]);
        let without = Message::tool_results(vec![result("b", "out")]);
        // the model quoting the line is not nexo having said it
        let quoted = Message::assistant(LINE);
        assert!(told(&[Message::user("hi"), with]));
        assert!(!told(&[Message::user(LINE), without, quoted]));
        assert!(!told(&[]));
    }
}
