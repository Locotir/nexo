/// Whether `command` only looks at things: every part of the pipeline or list is one of the
/// reading programs, and nothing writes (no redirection to a file, no `sed -i`, no `find -delete`,
/// no substitution that could run something else). Such a command may run beside reads.
pub(super) fn only_looks(command: &str) -> bool {
    command_words(command).is_some_and(|segs| !segs.is_empty() && segs.iter().all(|s| reads_only(s)))
}

/// The words of each part of the pipeline or list of `command`, split the way a shell would for plain
/// text, and nothing run or expanded. `None` for a command that redirects to a file, groups, or
/// substitutes (`$(...)`, backticks), or leaves a quote open: its words say too little to judge.
pub fn command_words(command: &str) -> Option<Vec<Vec<String>>> {
    let cleaned = command.replace("2>&1", " ").replace("&>/dev/null", " ").replace(">/dev/null", " ");
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = cleaned.chars().peekable();
    macro_rules! end_word {
        () => {
            if has_word {
                segments.last_mut().unwrap().push(std::mem::take(&mut word));
                has_word = false;
            }
        };
    }
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if q == '"' && (c == '`' || (c == '$' && chars.peek() == Some(&'('))) {
                    return None;
                } else if q == '"' && c == '\\' {
                    word.extend(chars.next());
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has_word = true;
                }
                '\\' => {
                    has_word = true;
                    word.extend(chars.next());
                }
                '>' | '<' | '`' | '(' | ')' | '{' | '}' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '|' | ';' | '\n' => {
                    end_word!();
                    if c == '|' && chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    segments.push(Vec::new());
                }
                '&' => {
                    if chars.next() != Some('&') {
                        return None;
                    }
                    end_word!();
                    segments.push(Vec::new());
                }
                c if c.is_whitespace() => end_word!(),
                c => {
                    word.push(c);
                    has_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if has_word {
        segments.last_mut().unwrap().push(word);
    }
    Some(segments.into_iter().filter(|s| !s.is_empty()).collect())
}

use nexo_dbg::{dbg_class, DbgClass};

/// Whether a command line holds a `nexo dbg` command that writes into a program or attaches to a process.
pub fn dbg_changes(command: &str) -> bool {
    command_words(command).is_some_and(|segs| {
        segs.iter().any(|s| {
            s.len() > 1 && s[0] == "nexo" && (s[1] == "dbg" || s[1] == "re") && dbg_class(&s[2..]) == DbgClass::Change
        })
    })
}

/// Whether a call of `tool` writes into a debugged program or attaches to a process: the shell with such a
/// `nexo dbg` line, or the `debug` tool with such a command.
pub fn changes_a_program(tool: &str, args: &serde_json::Value) -> bool {
    let Some(command) = args.get("command").and_then(serde_json::Value::as_str) else { return false };
    if tool == "debug" {
        return dbg_class(&nexo_dbg::split_words(command)) == DbgClass::Change;
    }
    dbg_changes(command)
}

/// One command (its words) of the reading kind.
pub(super) fn reads_only(words: &[String]) -> bool {
    let flags = || words[1..].iter().filter(|w| w.starts_with('-'));
    match words[0].as_str() {
        "cat" | "head" | "tail" | "grep" | "egrep" | "fgrep" | "rg" | "ls" | "wc" | "stat" | "file" => true,
        // `nexo bin` reads a file as bytes and prints. `name` and `note` also save into nexo's data folder, so they ask
        "nexo" if words.get(1).map(String::as_str) == Some("dbg") => dbg_class(&words[2..]) == DbgClass::Control,
        // `nexo re <word>` is `nexo bin <word>` or `nexo dbg <word>`: it reads when either way it would
        "nexo" if words.get(1).map(String::as_str) == Some("re") => {
            let mut as_bin = words.to_vec();
            as_bin[1] = "bin".into();
            words.len() == 2 || reads_only(&as_bin) || dbg_class(&words[2..]) == DbgClass::Control
        }
        "nexo" => {
            words.get(1).map(String::as_str) == Some("bin")
                && (matches!(
                    words.get(2).map(String::as_str),
                    Some(
                        "info"
                            | "overview"
                            | "funcs"
                            | "fn"
                            | "xref"
                            | "calls"
                            | "tree"
                            | "path"
                            | "slice"
                            | "strings"
                            | "find"
                            | "names"
                            | "bytes"
                            | "diff"
                            | "asm"
                    )
                ) || (words.get(2).map(String::as_str) == Some("crypto") && !words.iter().any(|w| w == "--save")))
        }
        "sed" => {
            flags().any(|w| w == "-n" || w == "--quiet" || w == "--silent")
                && !flags().any(|w| w.starts_with("--in-place") || (!w.starts_with("--") && w.contains('i')))
                && words[1..].iter().find(|w| !w.starts_with('-')).is_some_and(|s| prints_lines(s))
        }
        "find" => !words[1..].iter().any(|w| {
            matches!(
                w.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
            )
        }),
        "git" => {
            matches!(words.get(1).map(String::as_str), Some("log" | "diff" | "status" | "show"))
                && !words.iter().any(|w| w.starts_with("--output") || w == "--ext-diff")
        }
        _ => false,
    }
}

/// A `sed -n` script that only prints lines: `5p`, `10,20p`, `$p`, `/text/p`.
pub(super) fn prints_lines(script: &str) -> bool {
    let Some(addr) = script.strip_suffix('p') else { return false };
    addr.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '$')
        || (addr.len() >= 2 && addr.starts_with('/') && addr.ends_with('/') && !addr.contains(';'))
}
