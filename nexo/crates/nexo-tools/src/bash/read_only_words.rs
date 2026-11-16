/// Whether `command` only looks at things: every part of the pipeline or list is one of the
/// reading programs, and nothing writes (no redirection to a file, no `sed -i`, no `find -delete`,
/// no substitution that could run something else). Such a command may run beside reads.
pub(super) fn only_looks(command: &str) -> bool {
    looks_only_in(nexo_core::shell::shell().kind, command)
}

/// [`only_looks`] in the shell of `kind` ([`nexo_core::shell::Shell::kind`]). PowerShell reads a line its own
/// way (curly quotes are quotes, a backtick escapes, `> /dev/null` names a file): the POSIX words say nothing
/// of what it runs, so none of its commands is a read.
pub(super) fn looks_only_in(kind: &str, command: &str) -> bool {
    kind != "powershell"
        && command_words(command).is_some_and(|segs| !segs.is_empty() && segs.iter().all(|s| reads_only(s)))
}

/// The words of each part of the pipeline or list of `command`, split the way a shell would for plain
/// text, and nothing run or expanded. `None` for a command that redirects to a file, groups, or
/// substitutes (`$(...)`, backticks), leaves a quote open, or holds a `$".."` (bash and a plain `sh` give it
/// different words): its words say too little to judge.
/// A `2>&1` or a write into `/dev/null` after the last word writes no file, so it is left out.
pub fn command_words(command: &str) -> Option<Vec<Vec<String>>> {
    use nexo_core::shell::{lex, End};
    let lx = lex(command, true);
    if lx.open_quote || !lx.substs.is_empty() || lx.braces || lx.bad_heredoc || lx.locale {
        return None;
    }
    let mut segs = Vec::new();
    for c in &lx.cmds {
        let writes_nothing = c.redirects.iter().all(|r| r.at == c.words.len() && throws_away(r));
        if !writes_nothing || c.heredoc.is_some() || matches!(c.end, End::Amp | End::Open | End::Close) {
            return None;
        }
        if !c.words.is_empty() {
            segs.push(c.words.clone());
        }
    }
    Some(segs)
}

/// `2>&1`, or output sent into `/dev/null` itself (not `/dev/nullx`).
fn throws_away(r: &nexo_core::shell::Redirect) -> bool {
    let to = r.target.as_deref();
    (r.fd.as_deref() == Some("2") && r.op == ">&" && to == Some("1")) || (r.writes() && to == Some("/dev/null"))
}

use nexo_dbg::{dbg_class, DbgClass};

/// Whether a command line holds a `nexo dbg` command that writes into a program or attaches to a process.
/// Every command of the line is read, whatever it redirects or substitutes: such a line still writes.
pub fn dbg_changes(command: &str) -> bool {
    nexo_core::shell::lex(command, true).cmds.iter().any(|c| {
        let s = &c.words;
        s.len() > 1 && s[0] == "nexo" && (s[1] == "dbg" || s[1] == "re") && dbg_class(&s[2..]) == DbgClass::Change
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
    let args = &words[1..];
    match words[0].as_str() {
        "cat" | "head" | "tail" | "grep" | "egrep" | "fgrep" | "ls" | "wc" | "stat" => true,
        // `--pre` and `--hostname-bin` run the program they name (`--pre-glob` picks the files `--pre` gets),
        // and `-z` runs the decompressors (`gzip`, `xz`, ...)
        "rg" => !args.iter().any(|w| {
            ["--pre", "--pre-glob", "--hostname-bin", "--search-zip"].iter().any(|o| is_long(w, o))
                || short_has(w, &['z'])
        }),
        // `-C` writes a compiled magic file, `-z` and `-Z` may unpack through other programs, `-p` writes
        // the file's times back; `file` takes any start of a long option (`--comp`)
        "file" => !args.iter().any(|w| {
            ["--compile", "--uncompress", "--uncompress-noreport", "--preserve-date"].iter().any(|o| is_long_cut(w, o))
                || short_has(w, &['C', 'z', 'Z', 'p'])
        }),
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
                            | "cfg"
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
        "sed" => sed_prints(args),
        "find" => !args.iter().any(|w| {
            matches!(
                w.as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprint0" | "-fprintf" | "-fls"
            )
        }),
        // `--ext-diff` runs `diff.external`, `--show-signature` and the `%G` placeholders run gpg, `--output`
        // writes a file; a global option (`-c core.pager=..`) puts something else in the subcommand's place
        "git" => {
            matches!(words.get(1).map(String::as_str), Some("log" | "diff" | "status" | "show"))
                && !words.iter().any(|w| {
                    w.starts_with("--output") || w == "--ext-diff" || w == "--show-signature" || w.contains("%G")
                })
        }
        _ => false,
    }
}

/// Whether `word` is the long option `name`, alone or with its value (`--pre`, `--pre=sh`).
fn is_long(word: &str, name: &str) -> bool {
    word.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
}

/// Whether `word` is the long option `name` as `getopt_long` reads it, which takes any start of the name.
fn is_long_cut(word: &str, name: &str) -> bool {
    let opt = word.split('=').next().unwrap_or_default();
    opt.len() > 2 && opt.starts_with("--") && name.starts_with(opt)
}

/// Whether `word` is a group of short options (`-bz`) that holds one of `letters`. A letter of a value
/// written onto its option (`-g*.gz`) counts too: that only asks more.
fn short_has(word: &str, letters: &[char]) -> bool {
    word.len() > 1 && word.starts_with('-') && !word.starts_with("--") && word.contains(letters)
}

/// `sed -n` whose every script only prints lines ([`prints_lines`]): each `-e` (or `--expression`) or, without
/// one, the first word that is not an option. An option after that word is not a read: GNU sed moves it in front,
/// BSD sed takes it for a file, so the two would run different scripts. `-i` writes the files, `-f` reads a script
/// this does not see, and an option not named here may take the next word as its value: none of them is a read.
fn sed_prints(args: &[String]) -> bool {
    let (mut quiet, mut scripts, mut operands) = (false, Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(w) = it.next() {
        let attached = w.strip_prefix("--expression=").or_else(|| w.strip_prefix("-e").filter(|s| !s.is_empty()));
        match w.as_str() {
            "--" => operands.extend(it.by_ref()),
            _ if w.starts_with('-') && w != "-" && !operands.is_empty() => return false,
            "-n" | "--quiet" | "--silent" => quiet = true,
            "-E" | "-r" | "-s" | "-z" | "-u" | "--regexp-extended" | "--separate" | "--null-data" | "--unbuffered"
            | "--posix" | "--sandbox" => {}
            "-e" | "--expression" => match it.next() {
                Some(s) => scripts.push(s.as_str()),
                None => return false,
            },
            _ if attached.is_some() => scripts.extend(attached),
            _ if w.starts_with('-') && w != "-" => return false,
            _ => operands.push(w),
        }
    }
    if scripts.is_empty() {
        scripts.extend(operands.first().map(|s| s.as_str()));
    }
    quiet && !scripts.is_empty() && scripts.iter().all(|s| prints_lines(s))
}

/// A `sed -n` script that only prints lines: `p`, after a line (`5p`, `$p`), a `/regex/` or a range of two
/// (`10,20p`, `/a/,/b/p`). Anything else could hold a command: `e` runs one, `w` writes a file.
pub(super) fn prints_lines(script: &str) -> bool {
    let Some(addr) = script.strip_suffix('p') else { return false };
    addr.is_empty()
        || after_address(addr)
            .is_some_and(|rest| rest.is_empty() || rest.strip_prefix(',').and_then(after_address) == Some(""))
}

/// What follows the sed address that `s` starts with: a line number, `$`, or `/regex/`. A regex takes no `/`
/// of its own, no `\` before the closing one and no line end: sed would read on past it, and a command could
/// hide there (`/x/e touch y #/p`).
fn after_address(s: &str) -> Option<&str> {
    if let Some(re) = s.strip_prefix('/') {
        let (re, rest) = re.split_once('/')?;
        return (!re.ends_with('\\') && !re.contains('\n')).then_some(rest);
    }
    if let Some(rest) = s.strip_prefix('$') {
        return Some(rest);
    }
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (digits > 0).then(|| &s[digits..])
}
