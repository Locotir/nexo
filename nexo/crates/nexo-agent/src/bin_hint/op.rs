//! The op of `nexo bin` that gives what a reader program was asked for.

use crate::hint_cmd::{self, Seg};

/// The op of `nexo bin` that gives what `prog` was asked for, with the file.
pub(super) fn op_for(prog: &str, args: &[String], file: &str, next: Option<&Seg>) -> String {
    let f = hint_cmd::quoted(file);
    // short options of a cluster (`-Ws`): it stops at an option that takes a value, whose rest (`-x.data`) is not
    // a set of options
    let takes = if prog == "readelf" { "xpR" } else { "jbmME" };
    let has = |short: char, long: &str| {
        args.iter().any(|a| {
            let cluster = a.strip_prefix('-').filter(|r| !r.starts_with('-')).is_some_and(|r| {
                let upto = r.find(|c| takes.contains(c)).map_or(r.len(), |i| i + 1);
                r[..upto].contains(short)
            });
            cluster || (!long.is_empty() && a.starts_with(long))
        })
    };
    // the value of a short option that takes one: `-j SEC`, `-jSEC`, `-Wx SEC`
    let short_val = |opt: char| {
        args.iter().enumerate().find_map(|(i, a)| {
            let r = a.strip_prefix('-').filter(|r| !r.starts_with('-'))?;
            let at = r.find(|c| takes.contains(c))?;
            if !r[at..].starts_with(opt) {
                return None;
            }
            let rest = &r[at + 1..];
            if rest.is_empty() {
                args.get(i + 1).cloned()
            } else {
                Some(rest.trim_start_matches('=').to_string())
            }
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
            } else if has('s', "--full-contents") {
                // a hex dump of the section named with `-j`; with none, the section list
                let sec = short_val('j').or_else(|| valued("--section")).filter(|s| plain(s));
                match sec {
                    Some(sec) => bytes_of_section(&f, &sec),
                    None => format!("info {f}"),
                }
            } else {
                format!("info {f}")
            }
        }
        "readelf" => {
            let hex = short_val('x').or_else(|| valued("--hex-dump"));
            if let Some(sec) = hex {
                // a number is a section index: the section list gives the names
                if plain(&sec) && !sec.chars().all(|c| c.is_ascii_digit()) {
                    bytes_of_section(&f, &sec)
                } else {
                    format!("info {f}")
                }
            } else if has('s', "--syms") || has('s', "--symbols") || has('s', "--dyn-syms") {
                format!("funcs {f}")
            } else if has('p', "--string-dump") {
                format!("strings {f}")
            } else {
                format!("info {f}")
            }
        }
        "nm" => format!("funcs {f}"),
        "strings" | "grep" => match grep_after(next) {
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
        // a dump of bytes or a look at the headers: the map first, then `bytes <file> <addr> <len>` for the part
        // wanted
        "xxd" | "hexdump" | "hd" | "od" | "file" | "otool" | "cat" | "head" | "tail" | "dd" | "ls" | "stat" => {
            format!("info {f}")
        }
        // `ldd`, a script that parses the file, and gdb with no debug tool offered
        _ => format!("overview {f}"),
    }
}

/// `bytes <file> <section>`; a section that holds pointers by its standard meaning is read `--as ptr`, the rest
/// as plain bytes (a `.data` could hold anything).
pub(super) fn bytes_of_section(file: &str, sec: &str) -> String {
    let ptrs = sec == ".data.rel.ro"
        || sec.starts_with(".data.rel.ro.")
        || matches!(sec, ".init_array" | ".fini_array" | ".got" | ".got.plt");
    let sec = hint_cmd::quoted(sec);
    if ptrs {
        format!("bytes {file} {sec} --as ptr")
    } else {
        format!("bytes {file} {sec}")
    }
}

/// `| grep PATTERN` right after `strings`: the pattern, as `strings --grep` writes it (`(?i)` for `-i`).
pub(super) fn grep_after(next: Option<&Seg>) -> Option<String> {
    let s = next?;
    let first = hint_cmd::program(s.words.first()?);
    if !matches!(first.as_str(), "grep" | "egrep" | "rg" | "ugrep") {
        return None;
    }
    // the long options that only say how to read the file; any other is not looked into
    let mut flags = String::new();
    for w in s.words[1..].iter().filter(|w| w.starts_with('-')) {
        match w.strip_prefix("--") {
            Some("text" | "binary-files=text") => {}
            Some("ignore-case") => flags.push('i'),
            Some(_) => return None,
            None => flags.extend(w.chars().skip(1)),
        }
    }
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
