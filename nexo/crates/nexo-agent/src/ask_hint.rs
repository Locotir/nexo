//! Once per session, when the person's own message names a compiled program (a word that is a file under the
//! 4096-byte magic test of `nexo_bin::opens`) or asks to reverse or debug a binary, the start note of that request
//! carries one line saying that `bin` reads a program without running it and `debug` runs it under a debugger, so
//! the model reaches for them before `gdb`, `objdump` or `strings`. Nothing about it is in the system prompt or in a
//! request that is not about a compiled program.
//!
//! The words are looked at as text and a name is only opened as bytes (4096 at most, regular files only): nothing is
//! run, loaded or expanded. The trigger words are English and Spanish, with or without accents, in any case, and a
//! name typed in other case than the file's (`C-LCRYPT` for `c-lcrypt`) finds the file in the session's folder.

use std::path::Path;

use nexo_core::message::{Message, Role};

use crate::hint_cmd::{self, Offer};

/// Starts the line, so `told` finds it in a kept history.
/// It names no tool: the line may offer `bin` alone, and then it must not speak of a debugger.
pub const HEAD: &str = "compiled programs, with nexo's own tools:";

/// The most words of a message looked at as names, the most files asked about, and the most entries of the
/// session's folder read to match a name typed in other case.
const WORDS: usize = 120;
const STATS: usize = 40;
const ENTRIES: usize = 2000;

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
const DEBUG_WORDS: [&str; 12] = [
    "debug",
    "debugging",
    "debugger",
    "breakpoint",
    "backtrace",
    "segfault",
    "crash",
    "crashes",
    "depurar",
    "depura",
    "depurando",
    "depurador",
];
const PROGRAM_WORDS: [&str; 12] = [
    "binary",
    "binaries",
    "executable",
    "elf",
    "exe",
    "compiled",
    "dll",
    "binario",
    "binarios",
    "ejecutable",
    "ejecutables",
    "compilado",
];

/// The line for the person's message `text` (their own words, never what nexo added), or `None` when it is not
/// about a compiled program. `offer` says which tools can take the call.
pub fn line(text: &str, cwd: &Path, offer: Offer) -> Option<String> {
    let file = named(text, cwd);
    if file.is_none() && !asks(text) {
        return None;
    }
    // no file named: the placeholder is written bare, as the model fills it in
    let (f, shell_f) = match file.as_deref() {
        Some(f) => {
            let f = hint_cmd::slash(f);
            (hint_cmd::quoted(&f), hint_cmd::shell_quoted(&f))
        }
        None => ("<file>".to_string(), "<file>".to_string()),
    };
    let bin = if offer.bin {
        format!("{} reads one without running it", hint_cmd::tool_call("bin", &format!("overview {f}")))
    } else {
        format!("nexo bin overview {shell_f} reads one without running it")
    };
    if !offer.debug {
        return Some(format!(
            "{HEAD} {bin}, then funcs, fn, decomp, xref, strings; use that before objdump, strings or xxd."
        ));
    }
    let run = hint_cmd::tool_call("debug", &format!("run {f}; cont"));
    Some(format!(
        "{HEAD} {bin}; {run} runs it under a debugger (break, bt, regs, mem, strace); use them before gdb, objdump or strings."
    ))
}

/// The words of `text` that can name a file, as the person wrote them: no option, `@` mention, `~`, URL or glob.
fn name_words(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .take(WORDS)
        // a word with both kinds of quote is a shell trick, not a name
        .filter(|raw| !(raw.contains('"') && raw.contains('\'')))
        .map(|raw| {
            raw.trim_matches(|c: char| {
                matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';' | ':' | '!' | '?')
            })
            .trim_end_matches('.')
        })
        .filter(|w| {
            !(w.is_empty()
                || w.len() > 300
                || w.starts_with(['@', '-', '~'])
                || w.contains("://")
                || w.contains(['*', '"'])
                // a backslash is a character of a name elsewhere; on Windows it is the separator of a path
                || (!cfg!(windows) && w.contains('\\')))
        })
        .take(STATS)
        .collect()
}

/// The first word of `text` that is a compiled program the kit opens, as the person wrote it; failing that, the
/// file of the session's folder a bare word names in other case (`C-LCRYPT` for `c-lcrypt`).
fn named(text: &str, cwd: &Path) -> Option<String> {
    let words = name_words(text);
    if let Some(w) = words.iter().find(|w| hint_cmd::reads(cwd, w)) {
        return Some((*w).to_string());
    }
    let bare: Vec<&str> = words.iter().copied().filter(|w| !w.contains(['/', '\\'])).collect();
    if bare.is_empty() {
        return None;
    }
    let names: Vec<String> =
        std::fs::read_dir(cwd).ok()?.flatten().take(ENTRIES).filter_map(|e| e.file_name().into_string().ok()).collect();
    bare.iter().find_map(|w| {
        names.iter().find(|n| n.as_str() != *w && n.eq_ignore_ascii_case(w) && hint_cmd::reads(cwd, n)).cloned()
    })
}

/// A letter as the trigger words spell it: lower case, no Spanish accent.
fn fold(c: char) -> char {
    match c.to_lowercase().next().unwrap_or(c) {
        'á' => 'a',
        'é' => 'e',
        'í' => 'i',
        'ó' => 'o',
        'ú' | 'ü' => 'u',
        'ñ' => 'n',
        c => c,
    }
}

/// Whether the words ask for reverse engineering or debugging a compiled program.
fn asks(text: &str) -> bool {
    let folded: String = text.chars().map(fold).collect();
    let words: Vec<&str> =
        folded.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).filter(|w| !w.is_empty()).take(400).collect();
    let has = |list: &[&str]| words.iter().any(|w| list.contains(w));
    let reverse = words.windows(2).any(|p| {
        (p[0] == "reverse" && p[1].starts_with("engineer"))
            || (p[0] == "ingenieria" && matches!(p[1], "inversa" | "reversa"))
    }) || words.iter().any(|w| {
        w.replace('-', "").starts_with("reverseengineer")
            || *w == "reversing"
            || w.starts_with("disassembl")
            || w.starts_with("decompil")
            || w.starts_with("desensambl")
            || w.starts_with("descompil")
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
        let call = hint_cmd::tool_call(
            "bin",
            &format!("overview {}", hint_cmd::quoted(&hint_cmd::slash(&abs.display().to_string()))),
        );
        assert!(l.contains(&call), "{l}");
        // a name with a space is not split by this reading, and a quoted one is whole only in the shell form
        assert!(line("read ./c-lcrypt.", d.path(), BOTH).unwrap().contains("overview ./c-lcrypt\""));
    }

    #[test]
    fn a_name_typed_in_other_case_finds_the_file_of_the_folder() {
        let d = tempfile::tempdir().unwrap();
        exe_in(d.path(), "c-lcrypt");
        // the file as it is, not as it was typed, so the call works where names are case-sensitive
        #[cfg(unix)]
        {
            let l = line("prueba nexo contra mi programa opensource C-LCRYPT", d.path(), BOTH).unwrap();
            assert!(l.contains("overview c-lcrypt\"") && l.contains("run c-lcrypt; cont"), "{l}");
            assert_eq!(named("C-LCRYPT.", d.path()), Some("c-lcrypt".to_string()));
            // a path is taken as written
            assert_eq!(named("./C-LCRYPT", d.path()), None);
        }
        // names are not case-sensitive on Windows: the word as typed is already a file there, and the call works
        #[cfg(windows)]
        {
            let l = line("prueba nexo contra mi programa opensource C-LCRYPT", d.path(), BOTH).unwrap();
            assert!(l.contains("overview C-LCRYPT\"") && l.contains("run C-LCRYPT; cont"), "{l}");
            assert_eq!(named("./C-LCRYPT", d.path()), Some("./C-LCRYPT".to_string()));
        }
        // a word that is no file of the folder, or a text file, is nothing        std::fs::write(d.path().join("notes.txt"), "hello\n").unwrap();
        assert_eq!(named("NOTES.txt Other", d.path()), None);
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
            "Debug the EXECUTABLE, it crashes",
            "quiero hacer ingeniería inversa de este binario",
            "desensambla la función main",
            "descompila el ejecutable",
            "depura el binario, se cae",
            "Depurar el programa compilado",
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
            "fix the crash so the tests pass",
            "debug it so it works",
            "depura esta función de python",
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
