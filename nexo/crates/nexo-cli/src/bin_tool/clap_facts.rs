use super::*;

/// What an unknown flag or an unknown op says, for the model's `bin` tool and for `nexo bin` alike: clap's first
/// line only (its usage block and `tip: ... '-- --asm'` mislead here), then nexo's own fact: the flags the op takes,
/// or what does the job of an op that is not one. `words` are those after `bin`. `None`: any other error.
pub(crate) fn bin_error(e: &clap::Error, words: &[String]) -> Option<String> {
    let text = e.to_string();
    let text = text.strip_prefix("error: ").unwrap_or(&text);
    let first = text.lines().next().unwrap_or_default();
    let op = words.first().map(String::as_str).unwrap_or_default();
    match e.kind() {
        ErrorKind::UnknownArgument => {
            let mut out = first.to_string();
            if let Some(flags) = op_flags(op) {
                out.push_str(&format!("\n`{op}` takes: {flags}"));
            }
            Some(out)
        }
        ErrorKind::InvalidSubcommand => {
            Some(not_an_op(op, words.get(1..).unwrap_or(&[])).unwrap_or_else(|| unknown_op(op)))
        }
        ErrorKind::MissingRequiredArgument | ErrorKind::WrongNumberOfValues | ErrorKind::TooFewValues => {
            // clap's first line, then the op's own usage line instead of its whole block
            let usage = op_usage(op).or_else(|| {
                let u = text.lines().find_map(|l| l.strip_prefix("Usage: nexo bin "))?;
                Some(u.to_string())
            })?;
            Some(format!("{}\nusage: {usage}", first.trim_end_matches(':')))
        }
        _ => None,
    }
}

/// The usage line of one op, from the list the tool's description carries (`fn <file> <name|addr> [--to ADDR]`),
/// with `<file>` as `./prog`.
pub(super) fn op_usage(op: &str) -> Option<String> {
    if op.is_empty() {
        return None;
    }
    OPS.lines().find_map(|line| {
        let head = line.split("  ").next().unwrap_or(line);
        head.split(", ")
            .find(|piece| piece.split_whitespace().next() == Some(op) && piece.contains('<'))
            .map(|piece| piece.replace("<file>", "./prog"))
    })
}

pub(super) fn bin_command() -> Option<clap::Command> {
    crate::cli::Args::command().find_subcommand("bin").cloned()
}

/// The flags one op takes, as `--a, --b`, for the answer to a flag it does not have.
pub(super) fn op_flags(op: &str) -> Option<String> {
    let bin = bin_command()?;
    let sub = bin.find_subcommand(op)?;
    let mut flags: Vec<String> =
        sub.get_arguments().filter_map(|a| a.get_long().map(|l| format!("--{l}"))).filter(|f| f != "--help").collect();
    flags.dedup();
    Some(if flags.is_empty() { "no flags".into() } else { flags.join(", ") })
}

/// A word that starts an op line but is not an op: a shell word, or the name of another tool, gets a fact that
/// names what does the job here; anything else is left to clap's own answer. `rest` are the words after it, whose
/// first one that is not a flag is taken as the file, so the fact carries the real command.
pub(super) fn not_an_op(word: &str, rest: &[String]) -> Option<String> {
    if word.starts_with('-') {
        return None;
    }
    let bin = bin_command()?;
    if bin.find_subcommand(word).is_some() {
        return None;
    }
    let w = word.to_ascii_lowercase();
    let file = rest
        .iter()
        .find(|a| !a.is_empty() && !a.starts_with('-'))
        .map_or_else(|| "./prog".into(), |f| nexo_bin::shell_word(f));
    let syms = |kind: &str, what: &str| format!("`{word}` is not a bin op; `syms {file}{kind}` lists {what}");
    match w.as_str() {
        "disasm" | "disassemble" | "disassembly" | "dis" | "objdump" | "listing" => Some(format!(
            "`{word}` is not a bin op. `fn ./prog <name or addr>` shows the disassembly of a function (`--to ADDR` for a range, `--bytes` for the bytes); `decomp` gives pseudo-C; `asm` assembles."
        )),
        "imports" | "import" | "imp" => Some(syms(" --kind import", "the imports with their library, stub and GOT/IAT slot")),
        "exports" | "export" | "exp" => Some(syms(" --kind export", "the exports")),
        "symbols" | "symbol" | "sym" | "symtab" | "nm" => Some(syms(
            "",
            "the needed libraries, imports, exports and function and data symbols (`--kind all` adds sections, slots and relocations)",
        )),
        "dynsym" | "dynsyms" => Some(syms(" --kind import,export", "the dynamic symbols: imports and exports")),
        "readelf" | "otool" | "dumpbin" => Some(format!(
            "`{word}` is a shell tool, not a bin op; `syms {file} --kind all` lists the file's tables (libraries, imports, exports, symbols, sections, slots, relocations) and `info {file}` its headers and hardening"
        )),
        "sections" | "section" | "segments" | "shdr" => Some(syms(" --kind section", "the sections with address, size, flags and file offset")),
        "relocs" | "relocations" | "reloc" | "rela" | "rel" => Some(syms(" --kind reloc", "the relocations with type, symbol and target")),
        "got" | "plt" | "iat" | "slots" => Some(syms(" --kind got", "the import slots (puts@got, __imp_X) with the stub that jumps through each")),
        "needed" | "libs" | "libraries" | "deps" | "dylibs" | "dlls" => Some(syms(" --kind needed", "the libraries the file names")),
        "ldd" => Some(format!(
            "`ldd` is a shell tool that runs the program's loader; `syms {file} --kind needed` lists the libraries the file names, without running it"
        )),
        "xxd" | "hexdump" | "od" | "file" => Some(format!(
            "`{word}` is a shell tool, not a bin op; `info`, `overview`, `funcs` and `bytes` read a program here (`--help` lists the ops)"
        )),
        "echo" | "printf" => Some(format!(
            "{word} is a shell command, not a bin op; results already come one per op, so a separator is not needed"
        )),
        _ if nexo_dbg::is_shell_word(&w) => Some(format!("`{word}` is a shell command, not a bin op: bin runs no shell")),
        _ if word.contains('/') || word.contains('\\') => Some(format!(
            "`{word}` is a program path, and the first word of an op line is read as the op name, so it was not run (bin never runs a file). `overview {word}` reads it; to run it use `debug` (`run {word} ARGS`) or bash"
        )),
        _ => None,
    }
}

/// The ops clap knows (visible subcommands), by name.
pub(super) fn op_names() -> Vec<String> {
    bin_command()
        .map(|b| b.get_subcommands().filter(|s| !s.is_hide_set()).map(|s| s.get_name().to_string()).collect())
        .unwrap_or_default()
}

pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// What to say of a word that is no op: the real ops close to it in name (a shared start of 3 letters, one inside the
/// other, or a small edit distance) or in meaning (a word of the op's line in the tool's list), else every op name once.
pub(super) fn unknown_op(word: &str) -> String {
    let w = word.to_ascii_lowercase();
    let names = op_names();
    let stem = |s: &str| s.chars().take(4).collect::<String>();
    let w = w.as_str();
    let close: Vec<&String> = names
        .iter()
        .filter(|n| {
            let n = n.as_str();
            let by_name = (n.len() >= 3 && w.contains(n))
                || (w.len() >= 3 && n.contains(w))
                || (w.len() >= 3 && n.len() >= 3 && w.chars().take(3).eq(n.chars().take(3)))
                || edit_distance(w, n) <= 2.min(n.len() / 3);
            let by_meaning = w.len() >= 4
                && OPS.lines().any(|l| {
                    let head = l.split("  ").next().unwrap_or("");
                    head.split(", ").any(|p| p.split_whitespace().next() == Some(n))
                        && l.split("  ").nth(1).is_some_and(|d| {
                            d.split(|c: char| !c.is_alphanumeric()).any(|x| x.len() >= 4 && stem(x) == stem(w))
                        })
                });
            by_name || by_meaning
        })
        .collect();
    if close.is_empty() {
        format!("`{word}` is not a bin op. The ops are: {}", names.join(", "))
    } else {
        let list = close.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(" or ");
        format!("`{word}` is not a bin op; did you mean {list}? (`--help` lists the ops)")
    }
}
