//! Harness-agnostic classification of tool calls, and the files a shell command writes.

use std::sync::LazyLock;

use serde_json::Value;

use crate::py::{Caps, Re, I, M, S};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Read,
    Write,
    Exec,
    Search,
    Other,
}

const PATH_KEYS: [&str; 7] = ["file_path", "path", "filepath", "filename", "target_file", "file", "notebook_path"];
const CMD_KEYS: [&str; 3] = ["command", "cmd", "script"];

/// The words of a tool name that give its kind, checked in this order. A word counts only as a whole word of
/// the name (split at `_`, `-`, `.`, `:` and case changes), so `get_thread` is not a read nor
/// `list_notifications` a print of a file.
const KIND_WORDS: [(Kind, &[&str]); 4] = [
    (Kind::Write, &["edit", "multiedit", "write", "create", "patch", "replace", "insert", "apply"]),
    (Kind::Read, &["read", "view", "open", "cat", "recall"]),
    (Kind::Search, &["grep", "glob", "search", "find", "list", "ls"]),
    (Kind::Exec, &["bash", "shell", "exec", "execute", "run", "terminal", "command"]),
];
/// What a kind word may be joined to in one word of a name (`readfile`, `listdir`, `runcommand`).
const OBJECT_WORDS: [&str; 11] =
    ["file", "files", "dir", "dirs", "directory", "path", "paths", "command", "cmd", "notebook", "s"];
/// Subcommands of an editor tool that only show a file.
const VIEW_COMMANDS: [&str; 3] = ["view", "read", "show"];

/// The lower-case words of a tool name.
fn name_words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            out.extend((!cur.is_empty()).then(|| std::mem::take(&mut cur)));
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    out.extend((!cur.is_empty()).then_some(cur));
    out
}

/// The kind a tool name gives, or `Other`.
fn name_kind(name: &str) -> Kind {
    let words = name_words(name);
    let has =
        |k: &str| words.iter().any(|w| w == k || w.strip_prefix(k).is_some_and(|rest| OBJECT_WORDS.contains(&rest)));
    // `get_file`, `fetch_file`: a read by its object
    let get_file = words.windows(2).any(|p| matches!(p[0].as_str(), "get" | "fetch") && p[1] == "file");
    for (kind, ws) in KIND_WORDS {
        if ws.iter().any(|w| has(w)) || (kind == Kind::Read && get_file) {
            return kind;
        }
    }
    Kind::Other
}

/// The files a call names: its path, then each entry of a `paths` list, without repeats.
fn files_of(a: &serde_json::Map<String, Value>) -> Vec<String> {
    let one = PATH_KEYS.iter().find_map(|k| a.get(*k).and_then(Value::as_str));
    let many = a.get("paths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    let mut out: Vec<String> = Vec::new();
    for p in one.into_iter().chain(many).filter(|p| !p.is_empty()) {
        if !out.iter().any(|q| q == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// (kind, path, command) of a tool call.
pub fn classify(name: &str, args: Option<&Value>) -> (Kind, Option<String>, Option<String>) {
    let args = args.and_then(Value::as_object);
    let kind = name_kind(name);
    let get = |keys: &[&str]| -> Option<String> {
        let a = args?;
        keys.iter().find_map(|k| a.get(*k).and_then(Value::as_str).map(str::to_string))
    };
    // a call that names several files (`paths`) holds no one file: its output is not any one file's, so it has
    // no path (every file of it would be called stale when one of them is edited)
    let path = match (kind, args.map(files_of).as_deref()) {
        (Kind::Read, Some([_, _, ..])) => None,
        (Kind::Read, Some([one])) => get(&PATH_KEYS).or_else(|| Some(one.clone())),
        _ => get(&PATH_KEYS),
    };
    let command = get(&CMD_KEYS);
    // an editor tool asked to show a file (`{"command": "view", "path": …}`) reads it
    if kind == Kind::Write && command.as_deref().is_some_and(|c| VIEW_COMMANDS.contains(&c)) {
        return (Kind::Read, path, command);
    }
    let kind = if kind == Kind::Other && command.as_deref().is_some_and(|c| !c.is_empty()) { Kind::Exec } else { kind };
    (kind, path, command)
}

/// Arguments that ask a read tool for a part of a file (a line range or a window), whatever the tool
/// calls them.
const RANGE_KEYS: [&str; 10] =
    ["offset", "limit", "start_line", "end_line", "line_start", "line_end", "start", "end", "range", "view_range"];

/// Whether a call reads a part of a file: a read tool asked for a range. The step after it is likely the
/// next part of the same file.
pub fn partial_read(name: &str, args: Option<&Value>) -> bool {
    if classify(name, args).0 != Kind::Read {
        return false;
    }
    args.and_then(Value::as_object).is_some_and(|a| RANGE_KEYS.iter().any(|k| a.get(*k).is_some_and(|v| !v.is_null())))
}

/// The lines a read call asks for, from 1 (`u64::MAX`: to the end of the file), whatever the tool calls
/// them: `offset` and `limit` (a first line and a count), a first and a last line, or a `view_range` pair. A
/// call that gives none reads the whole file.
pub fn read_range(args: Option<&Value>) -> (u64, u64) {
    const WHOLE: (u64, u64) = (1, u64::MAX);
    let Some(a) = args.and_then(Value::as_object) else { return WHOLE };
    let line = |v: Option<&Value>| v.and_then(Value::as_i64).filter(|&x| x > 0).map(|x| x as u64);
    let num = |k: &str| line(a.get(k));
    if let Some(Value::Array(v)) = a.get("view_range") {
        let s = line(v.first()).unwrap_or(1);
        return (s, line(v.get(1)).unwrap_or(u64::MAX).max(s));
    }
    if let Some(limit) = num("limit") {
        let s = num("offset").unwrap_or(1);
        return (s, s.saturating_add(limit - 1));
    }
    if let Some(s) = num("offset") {
        return (s, u64::MAX);
    }
    for (first, last) in [("start_line", "end_line"), ("line_start", "line_end"), ("start", "end")] {
        let (s, e) = (num(first), num(last));
        if s.is_some() || e.is_some() {
            let s = s.unwrap_or(1);
            return (s, e.unwrap_or(u64::MAX).max(s));
        }
    }
    WHOLE
}

/// Programs that print a file, a part of it, the lines of it that match or the names of files, as they are on
/// disk.
const PRINTERS: [&str; 15] =
    ["cat", "head", "tail", "sed", "nl", "tac", "less", "more", "grep", "egrep", "fgrep", "rg", "ls", "find", "tree"];
/// Programs that, fed by a pipe, keep some of the lines they get.
const FILTERS: [&str; 10] = ["cat", "head", "tail", "sed", "nl", "cut", "grep", "egrep", "fgrep", "rg"];
/// Commands that print nothing of a file: moving around, and separators between the files printed.
const QUIET: [&str; 4] = ["cd", "echo", "printf", "pwd"];

/// The simple commands of a shell line (split at `;`, `&&`, `||`, `|` and new lines outside quotes), each
/// with whether a pipe feeds it.
fn simple_commands(command: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let (mut cur, mut piped, mut quote) = (String::new(), false, None);
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some('\''), _) => cur.push(c),
            (_, '\\') => {
                cur.push(c);
                cur.extend(chars.next());
            }
            (Some(_), _) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, ';' | '\n') => out.push((std::mem::take(&mut cur), std::mem::replace(&mut piped, false))),
            (None, '&') if chars.peek() == Some(&'&') => {
                chars.next();
                out.push((std::mem::take(&mut cur), std::mem::replace(&mut piped, false)));
            }
            (None, '|') => {
                let pipe = chars.next_if_eq(&'|').is_none();
                out.push((std::mem::take(&mut cur), std::mem::replace(&mut piped, pipe)));
            }
            _ => cur.push(c),
        }
    }
    out.push((cur, piped));
    out
}

/// Whether a shell command only prints files, parts of them, the lines of them that match or their names (`cat`,
/// `sed -n`, `head`, `grep`, `ls`, … with `cd` and `echo` between): what comes back is the material itself,
/// as a read or a search tool would return it, and not the output of a process.
pub fn prints_files(command: &str) -> bool {
    let mut prints = false;
    for (cmd, piped) in simple_commands(command) {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let Some(&first) = words.first() else { continue };
        // an edit in place, or an action on each file found, is not a print
        if (first == "sed" && words.iter().any(|w| w.starts_with("-i") || *w == "--in-place"))
            || (first == "find"
                && words.iter().any(|w| w.starts_with("-exec") || w.starts_with("-ok") || *w == "-delete"))
        {
            return false;
        }
        if (piped && FILTERS.contains(&first)) || (!piped && QUIET.contains(&first)) {
            continue;
        }
        if piped || !PRINTERS.contains(&first) {
            return false;
        }
        prints = true;
    }
    prints
}

/// The words of a simple command as the shell splits them: a quoted run is one word, without its quotes.
fn shell_words(cmd: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    for c in cmd.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => cur.push(c),
            (None, '\'' | '"') => (quote, any) = (Some(c), true),
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}
/// The files a shell command that only prints files (`prints_files`) prints, as they are on disk: the
/// arguments of `cat`, `head`, `sed -n`, `grep`… that are no option, no option's value and no script or
/// pattern. Empty for any other command, and for what reads a pipe.
pub fn printed_files(command: &str) -> Vec<String> {
    if !prints_files(command) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for (cmd, piped) in simple_commands(command) {
        let words = shell_words(&cmd);
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let Some(&first) = words.first() else { continue };
        if piped
            || !matches!(
                first,
                "cat" | "head" | "tail" | "sed" | "nl" | "tac" | "less" | "more" | "grep" | "egrep" | "fgrep" | "rg"
            )
        {
            continue;
        }
        // the first bare word of `sed` and of a search is its script or pattern, unless `-e` gave it
        let mut skip = matches!(first, "sed" | "grep" | "egrep" | "fgrep" | "rg") && !words.contains(&"-e");
        let mut value = false;
        for w in &words[1..] {
            if value {
                value = false;
                continue;
            }
            if w.starts_with('-') {
                // `-n 40`, `-e 's/a/b/'`, `-A 3`: an option whose value is the next word
                value = match first {
                    "head" | "tail" => matches!(*w, "-n" | "-c"),
                    "sed" => matches!(*w, "-e" | "-f"),
                    "cat" | "nl" | "tac" | "less" | "more" => false,
                    _ => matches!(*w, "-e" | "-f" | "-A" | "-B" | "-C" | "-m" | "-g" | "-t"),
                };
                continue;
            }
            if std::mem::replace(&mut skip, false) {
                continue;
            }
            let p = *w;
            if !p.is_empty() && !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Arguments that hold the whole new text of a file.
const CONTENT_KEYS: [&str; 5] = ["content", "file_text", "contents", "new_content", "text"];
/// Write tools that change a part of a file (unless asked to create it).
static PARTIAL: LazyLock<Re> = LazyLock::new(|| Re::new(r"(edit|patch|replace|insert|apply)", I));

/// The whole text a write call puts in its file (`write`, `write_file`, `Write`, `create`, an editor's
/// `create` command); `None` for an edit of a part of it.
pub fn full_write<'a>(name: &str, args: Option<&'a Value>) -> Option<&'a str> {
    let a = args?.as_object()?;
    if PARTIAL.is_match(name) && a.get("command").and_then(Value::as_str) != Some("create") {
        return None;
    }
    CONTENT_KEYS.iter().find_map(|k| a.get(*k).and_then(Value::as_str))
}

/// Characters of the text arguments of an edit (what it removes and what it puts), its path left out.
pub fn edit_size(args: Option<&Value>) -> usize {
    let Some(a) = args.and_then(Value::as_object) else { return 0 };
    let mut n = 0;
    for (k, v) in a {
        if PATH_KEYS.contains(&k.as_str()) {
            continue;
        }
        n += match v {
            Value::String(s) => s.chars().count(),
            other => other.to_string().chars().count(),
        };
    }
    n
}

const FILE: &str = r"([\w.][\w./-]*\.\w+)";
static SHELL_WRITES: LazyLock<Vec<Re>> = LazyLock::new(|| {
    vec![
        Re::new(&format!(r#"(?<![<>&\d])>>?\s*['"]?{FILE}"#), 0),
        Re::new(&format!(r#"\btee\s+(?:-a\s+)?['"]?{FILE}"#), 0),
        Re::new(&format!(r#"\bsed\s+-i\S*\s+(?:-e\s+)?(?:'[^']*'|"[^"]*"|\S+)\s+['"]?{FILE}"#), 0),
    ]
});
static HEREDOC: LazyLock<Re> =
    LazyLock::new(|| Re::new(r#"(?P<head><<-?\s*(?P<q>['"]?)(?P<tag>\w+)(?P=q)[^\n]*\n).*?^\s*(?P=tag)\s*$"#, S | M));
const PATH: &str = r#"(?:(?:pathlib\.)?Path|open)\(\s*['"](?P<bound>[\w.][\w./-]*\.\w+)['"]"#;
static SCRIPT: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        &[
            format!(
                r#"(?P<bind>\b(?P<name>\w+)\s*=\s*(?:{PATH}\s*\)|['"](?P<literal>[\w./][\w./-]*\.\w+)['"](?!\s*[.\[(])))"#
            ),
            r#"|(?P<direct>(?:pathlib\.)?Path\(\s*['"](?P<written>[\w.][\w./-]*\.\w+)['"]\s*\)\.(?:write_text|write_bytes|open\(\s*['"][wa]))"#.to_string(),
            r#"|(?P<opened>\bopen\(\s*['"](?P<target>[\w.][\w./-]*\.\w+)['"]\s*,\s*['"][wax])"#.to_string(),
            r#"|(?P<via>\b(?P<user>\w+)\.(?:write_text|write_bytes|open\(\s*['"][wa])|\bopen\(\s*(?P<arg>\w+)\s*,\s*['"][wax])"#.to_string(),
        ]
        .concat(),
        0,
    )
});
static HELPER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^def (?P<fn>\w+)\((?P<body>.*?)(?=^\S|\Z)", S | M));
static WRITES: LazyLock<Re> =
    LazyLock::new(|| Re::new(r#"\.write_(?:text|bytes)\(|\bopen\([^)\n]*['"][wax]|\.open\(\s*['"][wa]"#, 0));

fn group<'t>(text: &'t str, c: &Caps, g: usize) -> Option<&'t str> {
    c.get(g).copied().flatten().map(|(a, b)| &text[a..b])
}

fn helper_writes(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (gf, gb) = (HELPER.group("fn"), HELPER.group("body"));
    for m in HELPER.captures_all(command) {
        let (Some(f), Some(body)) = (group(command, &m, gf), group(command, &m, gb)) else { continue };
        if WRITES.is_match(body) {
            let pat = format!(r#"(?<![\w.]){f}\(\s*['"]([\w./][\w./-]*\.\w+)['"]"#);
            if let Ok(r) = Re::try_new(&pat, 0) {
                out.extend(r.findall(command).into_iter().map(str::to_string));
            }
        }
    }
    out
}

// A here-document fed to `cat` or `tee`, with the rest of its command line: `cat > f <<'EOF'`, `cat <<EOF >f`,
// `tee f <<EOF`, up to the line that closes it.
static HEREDOC_WRITE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r#"^(?P<before>[^\n]*?)<<(?P<dash>-?)\s*(?P<q>['"]?)(?P<tag>\w+)(?P=q)(?P<after>[^\n]*)\n(?P<body>.*?)^[ \t]*(?P=tag)[ \t]*$"#,
        S | M,
    )
});
// Where the simple command fed by a here-document sends it whole: `cat` with its output sent to a file (`>>`
// appends: not a whole write), or `tee` (`-a`, `--append` append).
static HEREDOC_TARGET: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r#"^\s*(?:cat(?:\s+-)?\s*(?:[^>]*?(?<![>&\d])>(?!>)|>(?!>))|tee(?P<flags>(?:\s+-[-\w]*)*))\s*['"]?(?P<path>[^\s'";|&<>()]+)"#,
        0,
    )
});

/// The file a simple command fed by a here-document writes whole with it, if any. `next` is the command its
/// output is piped to: a bare `cat` piped to `tee f` writes `f`; anything else reads the text and writes its
/// own output.
fn heredoc_target(cmd: &str, next: Option<&str>) -> Option<String> {
    let first = cmd.split_whitespace().next()?;
    let cmd = if first == "cat" && cmd.split_whitespace().count() == 1 && next.is_some() { next? } else { cmd };
    let t = HEREDOC_TARGET.search(cmd)?;
    let flags = group(cmd, &t, HEREDOC_TARGET.group("flags")).unwrap_or("");
    if flags.split_whitespace().any(|f| f == "--append" || (!f.starts_with("--") && f.contains('a'))) {
        return None;
    }
    group(cmd, &t, HEREDOC_TARGET.group("path")).map(str::to_string)
}

/// The files a shell command writes whole through a here-document (`cat > f <<'EOF' … EOF`), with the text
/// written. Device paths are left out.
pub fn heredoc_writes(command: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let r = &*HEREDOC_WRITE;
    let (before, dash, after, body) = (r.group("before"), r.group("dash"), r.group("after"), r.group("body"));
    for m in r.captures_all(command) {
        // the simple command the here-document feeds: the end of the line before it and the start after it
        let head = simple_commands(group(command, &m, before).unwrap_or("")).pop().map(|c| c.0).unwrap_or_default();
        let tail = simple_commands(group(command, &m, after).unwrap_or(""));
        let next = tail.get(1).filter(|c| c.1).map(|c| c.0.as_str());
        let cmd = format!("{} {}", head, tail.first().map_or("", |c| c.0.as_str()));
        let Some(path) = heredoc_target(&cmd, next) else { continue };
        let path = path.as_str();
        if path.starts_with("/dev/") || path.starts_with('$') {
            continue;
        }
        let mut text = group(command, &m, body).unwrap_or("").to_string();
        if group(command, &m, dash).is_some_and(|d| d == "-") {
            text = text.split('\n').map(|l| l.trim_start_matches('\t')).collect::<Vec<_>>().join("\n");
        }
        let path = path.strip_prefix("./").unwrap_or(path).to_string();
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some(e) => e.1 = text,
            None => out.push((path, text)),
        }
    }
    out
}

/// The files a shell command writes, as far as its text says (temporary and device paths left out).
pub fn shell_writes(command: &str) -> Vec<String> {
    if command.is_empty() {
        return Vec::new();
    }
    let shell = HEREDOC.sub(command, r"\g<head>");
    let mut found: Vec<String> = Vec::new();
    for pat in SHELL_WRITES.iter() {
        found.extend(pat.findall(&shell).into_iter().map(str::to_string));
    }
    let mut names: Vec<(String, String)> = Vec::new();
    let s = &*SCRIPT;
    let g = |n: &str| s.group(n);
    let (bind, name, bound, literal) = (g("bind"), g("name"), g("bound"), g("literal"));
    let (direct, written, opened, target, user, arg) =
        (g("direct"), g("written"), g("opened"), g("target"), g("user"), g("arg"));
    for m in s.captures_all(command) {
        let get = |i| group(command, &m, i);
        if get(bind).is_some() {
            let v = get(bound).or(get(literal)).unwrap_or("");
            let k = get(name).unwrap_or("");
            match names.iter_mut().find(|(n, _)| n == k) {
                Some(e) => e.1 = v.to_string(),
                None => names.push((k.to_string(), v.to_string())),
            }
        } else if get(direct).is_some() {
            found.push(get(written).unwrap_or("").to_string());
        } else if get(opened).is_some() {
            found.push(get(target).unwrap_or("").to_string());
        } else if let Some(who) = get(user).filter(|u| !u.is_empty()).or(get(arg)) {
            if let Some((_, v)) = names.iter().find(|(n, _)| n == who) {
                found.push(v.clone());
            }
        }
    }
    found.extend(helper_writes(command));
    let mut out: Vec<String> = Vec::new();
    for p in found {
        let p = p.strip_prefix("./").unwrap_or(&p).to_string();
        if !out.contains(&p) && !p.starts_with("/tmp/") && !p.starts_with("/dev/") && !p.starts_with("tmp/") {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_here_document_sent_to_a_file_is_a_whole_write() {
        let cmd = "cd /w && cat > src/a.rs <<'EOF'\nfn a() -> i32 {\n    7\n}\nEOF\ncargo build";
        assert_eq!(heredoc_writes(cmd), vec![("src/a.rs".to_string(), "fn a() -> i32 {\n    7\n}\n".to_string())]);
        let cmd = "cat <<EOF > notes.txt\nluck 5 -> 112\nEOF";
        assert_eq!(heredoc_writes(cmd), vec![("notes.txt".to_string(), "luck 5 -> 112\n".to_string())]);
        assert_eq!(heredoc_writes("tee ./b.sh <<-X\n\techo hi\n\tX")[0], ("b.sh".to_string(), "echo hi\n".to_string()));
        // an append, a program reading its input and a heredoc to a device are not whole writes of a file
        assert!(heredoc_writes("cat >> log.txt <<EOF\nmore\nEOF").is_empty());
        assert!(heredoc_writes("tee -a log.txt <<EOF\nmore\nEOF").is_empty());
        assert!(heredoc_writes("node - <<'EOF' > out.txt\nconsole.log(1)\nEOF").is_empty());
        assert!(heredoc_writes("cat > /dev/null <<EOF\nx\nEOF").is_empty());
        // a program fed by the here-document writes its own output, wherever it goes
        assert!(heredoc_writes("node - <<'EOF' 2>&1 | tee run.log\nconsole.log(42)\nEOF").is_empty());
        assert!(heredoc_writes("cat <<'EOF' | node > out.csv\nconsole.log(1)\nEOF").is_empty());
        assert!(heredoc_writes("tee --append log.txt <<EOF\nmore\nEOF").is_empty());
        assert!(heredoc_writes("tee -i -a log.txt <<EOF\nmore\nEOF").is_empty());
        // a bare cat piped to tee, and a tee with other flags, write the text whole
        assert_eq!(heredoc_writes("cat <<EOF | tee c.txt\nhi\nEOF"), vec![("c.txt".to_string(), "hi\n".to_string())]);
        assert_eq!(heredoc_writes("tee -i d.txt <<EOF >/dev/null\nhi\nEOF")[0].0, "d.txt");
        // the last of two writes of one file is what it holds
        let two = "cat > a.txt <<EOF\none\nEOF\ncat > a.txt <<EOF\ntwo\nEOF";
        assert_eq!(heredoc_writes(two), vec![("a.txt".to_string(), "two\n".to_string())]);
    }

    #[test]
    fn kinds() {
        assert_eq!(classify("Read", Some(&json!({"file_path": "a.rs"}))).0, Kind::Read);
        assert_eq!(classify("mystery", Some(&json!({"command": "ls"}))).0, Kind::Exec);
    }

    #[test]
    fn an_editor_view_is_a_read() {
        let view = json!({"command": "view", "path": "a.rs", "view_range": [1, 20]});
        assert_eq!(classify("str_replace_based_edit_tool", Some(&view)).0, Kind::Read);
        assert!(partial_read("str_replace_editor", Some(&view)));
        let edit = json!({"command": "str_replace", "path": "a.rs", "old_str": "a", "new_str": "b"});
        assert_eq!(classify("str_replace_based_edit_tool", Some(&edit)).0, Kind::Write);
    }

    #[test]
    fn a_kind_word_counts_only_as_a_whole_word_of_the_name() {
        for (name, kind) in [
            ("Read", Kind::Read),
            ("read_file", Kind::Read),
            ("readFile", Kind::Read),
            ("readfile", Kind::Read),
            ("get_file_contents", Kind::Read),
            ("recall", Kind::Read),
            ("Edit", Kind::Write),
            ("MultiEdit", Kind::Write),
            ("multiedit", Kind::Write),
            ("apply_patch", Kind::Write),
            ("write_file", Kind::Write),
            ("Grep", Kind::Search),
            ("LS", Kind::Search),
            ("list_dir", Kind::Search),
            ("listdir", Kind::Search),
            ("codebase_search", Kind::Search),
            ("Bash", Kind::Exec),
            ("run_command", Kind::Exec),
            ("run_terminal_cmd", Kind::Exec),
            ("mcp__chat__get_thread", Kind::Other),
            ("slack_get_thread_replies", Kind::Other),
            ("list_notifications", Kind::Search),
            ("get_category", Kind::Other),
            ("get_spreadsheet", Kind::Other),
            ("get_details", Kind::Other),
            ("fetch_urls", Kind::Other),
        ] {
            assert_eq!(classify(name, None).0, kind, "{name}");
        }
    }

    #[test]
    fn a_command_that_only_prints_files_is_told_from_a_process() {
        for read in [
            "cat src/app.css",
            "cd material; sed -n 1,60p assembly-B.txt; echo ====; sed -n 1,60p assembly-C.txt",
            "cd data && head -100 rows.csv | cut -d, -f1-3",
            r#"sed -n 60,200p a.txt | grep -n "var_80\|var_C0""#,
            "grep -n 'a|b; c' notes.md",
            "nl -ba main.go | tail -40",
            "ls src; cat src/main.rs",
            "find . -name '*.csv' | head",
        ] {
            assert!(prints_files(read), "{read}");
        }
        for process in [
            "cargo test 2>&1 | tail -50",
            "./run.sh | head",
            "sed -i 's/a/b/' a.txt",
            "cd material; echo ====",
            "cat a.txt | sort | uniq -c",
            "find . -name '*.o' -delete",
            "ls -la; make",
            "cat a.txt || make",
        ] {
            assert!(!prints_files(process), "{process}");
        }
    }

    #[test]
    fn printed_files_are_the_files_a_print_command_reads() {
        let files = |c: &str| printed_files(c);
        assert_eq!(files("cat src/parser.rs"), ["src/parser.rs"]);
        assert_eq!(files("sed -n '40,80p' src/parser.rs"), ["src/parser.rs"]);
        assert_eq!(files("head -n 40 a.txt b.txt"), ["a.txt", "b.txt"]);
        assert_eq!(files("grep -n 'fn parse' src/parser.rs"), ["src/parser.rs"]);
        assert_eq!(files("grep -A 3 -e parse lib.rs"), ["lib.rs"]);
        assert_eq!(files("cd src && nl -ba main.rs | tail -40"), ["main.rs"]);
        assert!(files("sed -i 's/a/b/' a.txt").is_empty());
        assert!(files("cargo test 2>&1 | tail -50").is_empty());
    }

    #[test]
    fn the_lines_a_read_asks_for_are_read_whatever_the_tool_calls_them() {
        assert_eq!(read_range(Some(&json!({"path": "a.rs"}))), (1, u64::MAX));
        assert_eq!(read_range(None), (1, u64::MAX));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "offset": 10, "limit": 5}))), (10, 14));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "limit": 40}))), (1, 40));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "offset": 7}))), (7, u64::MAX));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "start_line": 3, "end_line": 9}))), (3, 9));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "end_line": 9}))), (1, 9));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "view_range": [20, 30]}))), (20, 30));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "view_range": [20, -1]}))), (20, u64::MAX));
        // a last line before the first, or a line of 0, reads as the first
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "start": 8, "end": 2}))), (8, 8));
        assert_eq!(read_range(Some(&json!({"path": "a.rs", "offset": 0, "limit": 0}))), (1, u64::MAX));
    }

    #[test]
    fn whole_writes_and_edits() {
        let w = json!({"path": "a.rs", "content": "fn a() {}"});
        assert_eq!(full_write("write", Some(&w)), Some("fn a() {}"));
        assert_eq!(full_write("write_file", Some(&json!({"file_path": "a.rs", "content": "x"}))), Some("x"));
        let e = json!({"path": "a.rs", "old_string": "a", "new_string": "bc"});
        assert_eq!(full_write("edit", Some(&e)), None);
        assert_eq!(edit_size(Some(&e)), 3);
        let create = json!({"command": "create", "path": "a.rs", "file_text": "y"});
        assert_eq!(full_write("str_replace_based_edit_tool", Some(&create)), Some("y"));
    }

    #[test]
    fn writes() {
        assert_eq!(shell_writes("echo hi > out.txt && tee -a log.md"), vec!["out.txt", "log.md"]);
        let script = "python3 - <<'EOF'\np = Path('a/b.txt')\np.write_text('x')\nEOF";
        assert_eq!(shell_writes(script), vec!["a/b.txt"]);
    }

    #[test]
    fn a_read_of_several_files_has_no_one_path() {
        let two = json!({"paths": ["a.rs", "b.rs"]});
        assert_eq!(classify("read", Some(&two)), (Kind::Read, None, None));
        // the same list with `path` among it is still several files
        assert_eq!(classify("read", Some(&json!({"path": "a.rs", "paths": ["b.rs"]}))).1, None);
        // one file named in a list is that file, and a plain `path` is unchanged
        assert_eq!(classify("read", Some(&json!({"paths": ["a.rs"]}))).1.as_deref(), Some("a.rs"));
        assert_eq!(classify("read", Some(&json!({"paths": ["a.rs", "a.rs"]}))).1.as_deref(), Some("a.rs"));
        assert_eq!(classify("read", Some(&json!({"path": "a.rs"}))).1.as_deref(), Some("a.rs"));
        assert_eq!(classify("read", Some(&json!({"path": "a.rs", "paths": ["a.rs"]}))).1.as_deref(), Some("a.rs"));
    }
}
