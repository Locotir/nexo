//! Once per session, when the person's own message names a compiled program (a word that is a file under the
//! 4096-byte magic test of `nexo_bin::opens`) or asks to reverse or debug a binary, the start note of that request
//! carries one line saying that `bin` reads a program without running it and `debug` runs it under a debugger, so
//! the model reaches for them before `gdb`, `objdump` or `strings`. Nothing about it is in the system prompt or in a
//! request that is not about a compiled program.
//!
//! The words are looked at as text and a name is only opened as bytes (4096 at most, regular files only): nothing is
//! run, loaded or expanded.

use std::path::Path;

use nexo_core::message::{Message, Role};

use crate::hint_cmd::{self, Offer};

/// Starts the line, so `told` finds it in a kept history.
pub const HEAD: &str = "compiled programs, read and debugged with nexo's own tools:";

/// The most words of a message looked at as names, and the most files asked about.
const WORDS: usize = 120;
const STATS: usize = 40;

/// Words that ask for reverse engineering by themselves.
const RE_WORDS: [&str; 12] = [
    "gdb",
    "lldb",
    "objdump",
    "readelf",
    "ghidra",
    "radare2",
    "r2",
    "ltrace",
    "strace",
    "crackme",
    "disassembly",
    "decompiler",
];
/// Verbs that ask for it with a noun of a compiled program.
const DEBUG_WORDS: [&str; 8] =
    ["debug", "debugging", "debugger", "breakpoint", "backtrace", "segfault", "crash", "crashes"];
const PROGRAM_WORDS: [&str; 8] = ["binary", "binaries", "executable", "elf", "exe", "compiled", "dll", "so"];

/// The line for the person's message `text` (their own words, never what nexo added), or `None` when it is not
/// about a compiled program. `offer` says which tools can take the call.
pub fn line(text: &str, cwd: &Path, offer: Offer) -> Option<String> {
    let file = named(text, cwd);
    if file.is_none() && !asks(text) {
        return None;
    }
    let f = file.as_deref().unwrap_or("<file>");
    let bin = if offer.bin {
        format!(
            "{} reads one without running it",
            hint_cmd::tool_call("bin", &format!("overview {}", hint_cmd::quoted(f)))
        )
    } else {
        format!("nexo bin overview {} reads one without running it", hint_cmd::shell_quoted(f))
    };
    if !offer.debug {
        return Some(format!(
            "{HEAD} {bin}, then funcs, fn, decomp, xref, strings; use that before objdump, strings or xxd."
        ));
    }
    let run = hint_cmd::tool_call("debug", &format!("run {}; cont", hint_cmd::quoted(f)));
    Some(format!(
        "{HEAD} {bin}; {run} runs it under a debugger (break, bt, regs, mem, strace); use them before gdb, objdump or strings."
    ))
}

/// The first word of `text` that is a compiled program the kit opens, as the person wrote it.
fn named(text: &str, cwd: &Path) -> Option<String> {
    let mut stats = 0;
    for raw in text.split_whitespace().take(WORDS) {
        let w = raw
            .trim_matches(|c: char| {
                matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '!' | '?')
            })
            .trim_end_matches('.');
        if w.is_empty()
            || w.len() > 300
            || w.starts_with(['@', '-', '~'])
            || w.contains("://")
            || w.contains(['*', '"', '\\'])
        {
            continue;
        }
        stats += 1;
        if stats > STATS {
            break;
        }
        if hint_cmd::reads(cwd, w) {
            return Some(w.to_string());
        }
    }
    None
}

/// Whether the words ask for reverse engineering or debugging a compiled program.
fn asks(text: &str) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .take(400)
        .map(str::to_ascii_lowercase)
        .collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    let reverse = words.windows(2).any(|p| p[0] == "reverse" && p[1].starts_with("engineer"))
        || words.iter().any(|w| {
            w.replace('-', "").starts_with("reverseengineer")
                || w == "reversing"
                || w.starts_with("disassembl")
                || w.starts_with("decompil")
        });
    reverse || has(&RE_WORDS) || (has(&DEBUG_WORDS) && has(&PROGRAM_WORDS))
}

/// Whether the session already carries the line in a message of the person's.
pub fn told(messages: &[Message]) -> bool {
    messages.iter().any(|m| m.role == Role::User && m.text().contains(HEAD))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: Offer = Offer { bin: true, debug: true };

    fn exe_in(dir: &Path, name: &str) {
        std::fs::copy(std::env::current_exe().unwrap(), dir.join(name)).unwrap();
    }

    #[test]
    fn a_message_that_names_a_compiled_file_gets_the_line_with_that_file() {
        let d = tempfile::tempdir().unwrap();
        exe_in(d.path(), "c-lcrypt");
        let l = line("prueba nexo contra c-lcrypt, porfa", d.path(), BOTH).unwrap();
        assert_eq!(
            l,
            format!(
                "{HEAD} bin {{\"command\": \"overview c-lcrypt\"}} reads one without running it; debug {{\"command\": \"run c-lcrypt; cont\"}} runs it under a debugger (break, bt, regs, mem, strace); use them before gdb, objdump or strings."
            )
        );
        let abs = d.path().join("c-lcrypt");
        let l = line(&format!("look at {} please.", abs.display()), d.path(), BOTH).unwrap();
        assert!(l.contains(&format!("overview {}\"", abs.display())), "{l}");
        // a name with a space is not split by this reading, and a quoted one is whole only in the shell form
        assert!(line("read ./c-lcrypt.", d.path(), BOTH).unwrap().contains("overview ./c-lcrypt\""));
    }

    #[test]
    fn the_offer_decides_the_form_and_the_debug_half() {
        let d = tempfile::tempdir().unwrap();
        exe_in(d.path(), "p");
        let only_bin = line("reverse p", d.path(), Offer { bin: true, debug: false }).unwrap();
        assert!(only_bin.contains("bin {\"command\": \"overview p\"}") && !only_bin.contains("debug {"), "{only_bin}");
        let shell = line("reverse p", d.path(), Offer::default()).unwrap();
        assert!(shell.contains("nexo bin overview p reads one") && !shell.contains("debug"), "{shell}");
    }

    #[test]
    fn a_request_to_reverse_or_debug_a_binary_gets_the_line_without_a_file() {
        let d = tempfile::tempdir().unwrap();
        for t in [
            "reverse engineer this binary and tell me what it does",
            "Reverse-engineering the crackme",
            "can you disassemble the thing",
            "decompile main",
            "use gdb on it",
            "run objdump",
            "debug the executable, it crashes",
            "why does my compiled program segfault? debug it",
        ] {
            let l = line(t, d.path(), BOTH).unwrap_or_else(|| panic!("no line for {t:?}"));
            assert!(l.contains("overview <file>") && l.contains("run <file>; cont"), "{t}: {l}");
        }
        // plain work in a project, or a name that is not a program, gets nothing
        std::fs::write(d.path().join("notes.txt"), "hello\n").unwrap();
        for t in [
            "debug this python script",
            "fix the crash in the parser",
            "the program prints a wrong total",
            "open notes.txt and read it",
            "see https://example.com/tool.exe",
            "",
        ] {
            assert_eq!(line(t, d.path(), BOTH), None, "{t:?}");
        }
    }

    #[test]
    fn a_hostile_word_is_only_text() {
        let d = tempfile::tempdir().unwrap();
        exe_in(d.path(), "p");
        for t in ["look at $(touch touched)", "see `touch touched` now", "run -rf /", "~/p", "@p", "a*b", "'p\"'"] {
            assert_eq!(named(t, d.path()), None, "{t:?}");
        }
        assert!(!d.path().join("touched").exists());
        // a very long message looks at the first words only
        let long = format!("{} p", "word ".repeat(500));
        assert_eq!(named(&long, d.path()), None);
        assert_eq!(named(&format!("{}p", "w ".repeat(10)), d.path()), Some("p".to_string()));
    }

    #[test]
    fn told_finds_the_line_in_a_message_of_the_person_and_nowhere_else() {
        let with = Message::user(format!("[project at the start of this request]\n{HEAD} x\n\nthe words"));
        assert!(told(std::slice::from_ref(&with)));
        // the model quoting it, or a message without it, is not nexo having said it
        assert!(!told(&[Message::assistant(HEAD)]));
        assert!(!told(&[Message::user("hi")]));
        assert!(!told(&[]));
    }
}
