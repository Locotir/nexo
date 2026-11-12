//! The first time a shell command runs `gdb` on a program, or a program of the project dies by a signal
//! (exit 128+N: a segfault is 139), the result ends with one line naming the `debug` call that explains it.
//! Once per session, only when the `debug` tool is offered, and only for a file `nexo bin` opens. Like
//! `bin_hint.rs`, nothing is run or loaded: the command is only split into words.

use std::path::Path;

use nexo_core::message::{Message, Part};
use serde_json::Value;

/// Every line starts with this, so `told` finds it in a kept history.
pub const HEAD: &str = "[nexo: the debug tool answers this in one call:";

/// Exit codes of a shell whose program died by a fault signal: SIGILL, SIGABRT, SIGBUS, SIGFPE, SIGSEGV.
const FAULTS: [i32; 5] = [132, 134, 135, 136, 139];

/// gdb options that take a value: the word after them is not the program.
const GDB_VALUED: [&str; 12] = ["-ex", "-x", "-iex", "-ix", "-p", "-c", "-d", "-s", "-e", "-se", "-cd", "-tty"];

/// The line for a command that earns it, from its result text.
pub fn line(command: &str, result: &str, cwd: &Path) -> Option<String> {
    let (rest, input) = take_input(command)?;
    let parts = nexo_tools::bash::command_words(&rest)?;
    let local = |w: &str| !w.starts_with('-') && nexo_bin::opens(&cwd.join(w));
    // gdb on a program of the project
    for p in &parts {
        if p.first().is_some_and(|w| program(w) == "gdb") {
            let target = match p.iter().position(|w| w == "--args") {
                Some(i) => p.get(i + 1..).map(<[String]>::to_vec),
                None => {
                    let mut it = p.iter().skip(1);
                    let mut found = None;
                    while let Some(w) = it.next() {
                        if GDB_VALUED.contains(&w.as_str()) {
                            it.next();
                        } else if local(w) {
                            found = Some(vec![w.clone()]);
                            break;
                        }
                    }
                    found
                }
            };
            let target = target.filter(|t| t.first().is_some_and(|w| local(w)))?;
            return Some(text(&target, None, "where it stops or crashes, with file:line, cause and locals"));
        }
    }
    // a program of the project that died by a fault signal
    let code = exit_code(result)?;
    if !FAULTS.contains(&code) {
        return None;
    }
    let i = parts.iter().rposition(|p| p.first().is_some_and(|w| local(w)))?;
    // only the last part's code is the shell's: `./p | head` says nothing about ./p
    if i + 1 != parts.len() {
        return None;
    }
    let fed = match (i, input) {
        (_, Some(f)) => Some(f),
        (0, None) => None,
        (_, None) => echoed(&parts[i - 1]),
    };
    let why = format!("stops at the fault (exit {code}) with file:line, cause and locals");
    Some(text(&parts[i], fed.as_deref(), &why))
}

fn text(prog: &[String], input: Option<&str>, why: &str) -> String {
    let mut run = String::from("run");
    for w in prog {
        run.push(' ');
        run.push_str(&word(w));
    }
    if let Some(i) = input {
        run.push(' ');
        run.push_str(i);
    }
    format!("{HEAD} debug {{\"command\": \"{}; cont\"}} {why}]", run.replace('"', "\\\""))
}

/// A word as the debug command language reads it: double quotes around one with spaces.
fn word(w: &str) -> String {
    if w.is_empty() || w.chars().any(char::is_whitespace) {
        format!("\"{w}\"")
    } else {
        w.to_string()
    }
}

/// `echo a b` or `printf 'a\n'` before the program: what it fed in, as `<<< "text"`.
fn echoed(p: &[String]) -> Option<String> {
    let (cmd, args) = p.split_first()?;
    let args: Vec<&String> = match cmd.as_str() {
        "echo" => args.iter().filter(|a| !a.starts_with('-')).collect(),
        "printf" if args.len() == 1 && !args[0].contains('%') => args.iter().collect(),
        _ => return None,
    };
    let t = args.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
    let t = t.trim_end_matches("\\n");
    (!t.contains('"')).then(|| format!("<<< \"{t}\""))
}

/// The command without its one input redirection (`< file`, `<<< text`), and that input as the debug
/// command language writes it. `None` when anything else redirects or the input cannot be written plainly.
fn take_input(command: &str) -> Option<(String, Option<String>)> {
    let Some(at) = command.find('<') else { return Some((command.to_string(), None)) };
    let (head, tail) = command.split_at(at);
    let here = tail.starts_with("<<<");
    let after = tail.trim_start_matches('<').trim_start();
    let (src, rest) = match after.chars().next()? {
        q @ ('\'' | '"') => {
            let end = after[1..].find(q)? + 1;
            (&after[1..end], &after[end + 1..])
        }
        _ => {
            let end = after.find(|c: char| c.is_whitespace() || "|;&".contains(c)).unwrap_or(after.len());
            (&after[..end], &after[end..])
        }
    };
    if src.is_empty() || src.contains('"') || rest.contains('<') || (!here && tail.starts_with("<<")) {
        return None;
    }
    let input = if here { format!("<<< \"{src}\"") } else { format!("< {}", word(src)) };
    Some((format!("{head} {rest}"), Some(input)))
}

/// The exit code the shell tool wrote at the end of a result.
fn exit_code(text: &str) -> Option<i32> {
    text.lines().rev().map(str::trim).find_map(|l| l.strip_prefix("[exit code ")?.strip_suffix(']')?.parse().ok())
}

fn program(word: &str) -> String {
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
}

/// Whether `messages` already hold the line in a tool result.
pub fn told(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        m.content.iter().any(|p| match p {
            Part::ToolResult { content, .. } => content.iter().filter_map(Part::as_text).any(|t| t.contains(HEAD)),
            _ => false,
        })
    })
}

/// The results of one round with the line added to the first that earns it; `told` is set when it is.
pub fn round(told: &mut bool, calls: &[(String, String, Value)], results: Vec<Part>, cwd: &Path) -> Vec<Part> {
    if *told {
        return results;
    }
    results
        .into_iter()
        .map(|p| {
            let Part::ToolResult { call_id, name, mut content, is_error } = p else { return p };
            if !*told {
                let command = calls
                    .iter()
                    .find(|(id, n, _)| *id == call_id && n == "bash")
                    .filter(|(_, _, a)| !nexo_tools::util::bool_arg(a, "background"))
                    .and_then(|(_, _, a)| a.get("command").and_then(Value::as_str));
                let text: String = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
                if let Some(l) = command.and_then(|c| line(c, &text, cwd)) {
                    *told = true;
                    content = vec![Part::text(format!("{text}\n\n{l}"))];
                }
            }
            Part::ToolResult { call_id, name, content, is_error }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir_with_prog() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), d.path().join("paint")).unwrap();
        d
    }

    #[test]
    fn a_segfault_of_a_project_program_names_the_debug_call_with_its_input() {
        let d = dir_with_prog();
        let segv = "Segmentation fault (core dumped)\n\n[exit code 139]";
        let l = line("echo '1 -1 #' | ./paint", segv, d.path()).unwrap();
        assert_eq!(
            l,
            r#"[nexo: the debug tool answers this in one call: debug {"command": "run ./paint <<< \"1 -1 #\"; cont"} stops at the fault (exit 139) with file:line, cause and locals]"#
        );
        let l = line("./paint big < in.txt", segv, d.path()).unwrap();
        assert!(l.contains(r#""run ./paint big < in.txt; cont""#), "{l}");
        let l = line("./paint <<< '3 4 x'", "[exit code 134]", d.path()).unwrap();
        assert!(l.contains(r#"run ./paint <<< \"3 4 x\"; cont"#), "{l}");
    }

    #[test]
    fn gdb_on_a_project_program_names_the_debug_call() {
        let d = dir_with_prog();
        let l = line("gdb -batch -ex run -ex bt ./paint", "", d.path()).unwrap();
        assert!(l.contains(r#""run ./paint; cont""#), "{l}");
        let l = line("gdb -q -batch -ex r --args ./paint 1 2", "", d.path()).unwrap();
        assert!(l.contains(r#""run ./paint 1 2; cont""#), "{l}");
    }

    #[test]
    fn no_line_for_clean_exits_other_programs_or_a_pipe_after_the_program() {
        let d = dir_with_prog();
        for (c, r) in [
            ("./paint", "ok"),
            ("./paint", "[exit code 1]"),
            ("./paint | head", "[exit code 139]"),
            ("python3 x.py", "[exit code 139]"),
            ("./missing", "[exit code 139]"),
            ("gdb ./missing", ""),
            ("./paint > out.txt", "[exit code 139]"),
        ] {
            assert_eq!(line(c, r, d.path()), None, "{c} {r}");
        }
    }

    #[test]
    fn once_per_session_and_found_again_in_a_kept_history() {
        let d = dir_with_prog();
        let calls = vec![("a".to_string(), "bash".to_string(), json!({"command": "./paint"}))];
        let res = || vec![Part::tool_result("a", "boom\n\n[exit code 139]", true)];
        let mut told_now = false;
        let out = round(&mut told_now, &calls, res(), d.path());
        assert!(told_now);
        let again = round(&mut told_now, &calls, res(), d.path());
        assert!(told(&[Message::tool_results(out)]));
        assert!(!told(&[Message::tool_results(again)]));
    }
}
