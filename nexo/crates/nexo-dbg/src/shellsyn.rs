//! Shell habits in the text a model sends to `debug` and `bin`, which run no shell. One reader for both tools:
//! `bin` (nexo-cli) already depends on this crate for its script units, so this is the lowest crate they share.
//! It only cuts a line into pieces and names the shell words; each tool decides what is a fact worth saying.

/// Words that belong to the shell. `echo` and `printf` are in the list too: `bin` has no such command, while
/// the debugger has its own (gdb's) `echo` and `printf`, so `debug` only counts them with a redirect or a pipe.
pub const SHELL_WORDS: &[&str] = &[
    "echo", "printf", "grep", "egrep", "cat", "head", "tail", "sed", "awk", "wc", "sort", "uniq", "cut", "tr", "ls",
    "cd", "pwd", "true", "false", "test", "sleep", "xargs", "tee", "which", "mkdir", "rm", "cp", "mv", "touch",
    "chmod", "export", "env", "bash", "sh", "time", "exit", "read", "dd", "od", "xxd", "hexdump",
];

/// Whether `w` is a shell word (see [`SHELL_WORDS`]).
pub fn is_shell_word(w: &str) -> bool {
    SHELL_WORDS.contains(&w.to_ascii_lowercase().as_str())
}

/// A redirection: its operator (`>`, `2>`, `>>`, `2>&1`, `<`) and what follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub op: String,
    pub target: String,
}

/// One command of a shell line: the text up to a `&&`, `||` or `|`, redirections taken out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Piece {
    /// The command as written, without its redirections and its pipeline.
    pub text: String,
    /// `&&` or `||` when the piece follows one.
    pub joiner: Option<&'static str>,
    /// What came after a `|` (the filter), as written.
    pub pipe: Option<String>,
    pub redirects: Vec<Redirect>,
}

impl Piece {
    /// The first word of the command (quotes not removed).
    pub fn first(&self) -> &str {
        self.text.split_whitespace().next().unwrap_or("")
    }
}

/// Cut one line (one `;` unit) at `&&`, `||`, `|` and redirections outside quotes. A quote that never closes is a
/// plain character. `lt`: whether `<` is a redirection (for `bin`; the debugger's `run ./p < file` is its own).
pub fn shell_pieces(line: &str, lt: bool) -> Vec<Piece> {
    let c: Vec<char> = line.chars().collect();
    let mut out: Vec<Piece> = Vec::new();
    let mut cur = Piece::default();
    let mut joiner: Option<&'static str> = None;
    let mut in_pipe = false;
    let mut i = 0;
    let flush = |cur: &mut Piece, out: &mut Vec<Piece>, joiner: Option<&'static str>| {
        cur.text = cur.text.trim().to_string();
        if let Some(p) = &mut cur.pipe {
            *p = p.trim().to_string();
        }
        if !cur.text.is_empty() || cur.pipe.is_some() || !cur.redirects.is_empty() {
            let mut p = std::mem::take(cur);
            p.joiner = joiner;
            out.push(p);
        }
    };
    while i < c.len() {
        let ch = c[i];
        let sink = |cur: &mut Piece, s: &str, in_pipe: bool| {
            if in_pipe {
                cur.pipe.get_or_insert_with(String::new).push_str(s)
            } else {
                cur.text.push_str(s)
            }
        };
        // an operator only counts at a word edge: `--grep a|b` and `x>y` are text, `| head` and `2>&1` are shell
        let edge = i == 0 || c[i - 1].is_whitespace() || matches!(c[i - 1], '"' | '\'');
        match ch {
            '\\' => {
                let end = (i + 2).min(c.len());
                sink(&mut cur, &c[i..end].iter().collect::<String>(), in_pipe);
                i = end;
                continue;
            }
            '"' | '\'' => {
                let close = (i + 1..c.len()).find(|&j| c[j] == ch && (ch == '\'' || c[j - 1] != '\\'));
                let end = close.map_or(i + 1, |j| j + 1);
                sink(&mut cur, &c[i..end].iter().collect::<String>(), in_pipe);
                i = end;
                continue;
            }
            '&' | '|' if edge && c.get(i + 1) == Some(&ch) && !in_pipe => {
                flush(&mut cur, &mut out, joiner);
                joiner = Some(if ch == '&' { "&&" } else { "||" });
                i += 2;
                continue;
            }
            '&' | '|' if edge && in_pipe && c.get(i + 1) == Some(&ch) => {
                flush(&mut cur, &mut out, joiner);
                joiner = Some(if ch == '&' { "&&" } else { "||" });
                in_pipe = false;
                i += 2;
                continue;
            }
            '|' if edge && !in_pipe => {
                in_pipe = true;
                cur.pipe = Some(String::new());
                i += 1;
                continue;
            }
            '>' | '<' if !in_pipe && (ch == '>' || lt) && (edge || fd_prefix(&cur.text)) => {
                // an fd number or `&` just before it belongs to the operator (`2>`, `&>`)
                let mut op = String::new();
                let t = cur.text.trim_end().len();
                let word_at = cur.text[..t].rfind(char::is_whitespace).map_or(0, |p| p + 1);
                let w = &cur.text[word_at..t];
                if t == cur.text.len() && !w.is_empty() && (w == "&" || w.chars().all(|x| x.is_ascii_digit())) {
                    op.push_str(w);
                    cur.text.truncate(word_at);
                }
                op.push(ch);
                i += 1;
                if ch == '>' && matches!(c.get(i), Some('>' | '&')) {
                    op.push(c[i]);
                    i += 1;
                }
                while c.get(i).is_some_and(|x| x.is_whitespace()) && !op.ends_with('&') {
                    i += 1;
                }
                let start = i;
                while i < c.len() && !c[i].is_whitespace() {
                    i += 1;
                }
                cur.redirects.push(Redirect { op, target: c[start..i].iter().collect() });
                continue;
            }
            _ => {}
        }
        sink(&mut cur, &ch.to_string(), in_pipe);
        i += 1;
    }
    flush(&mut cur, &mut out, joiner);
    out
}

/// Whether the text ends in an fd number or `&` (`2`, `&`) that starts a redirection like `2>`.
fn fd_prefix(text: &str) -> bool {
    let w = text.rsplit(char::is_whitespace).next().unwrap_or("");
    let before = &text[..text.len() - w.len()];
    (w == "&" || (!w.is_empty() && w.chars().all(|x| x.is_ascii_digit())))
        && (before.is_empty() || before.ends_with(char::is_whitespace))
}

/// The first shell variable in `text` (`$c`, `${c}`, `$(..)`), outside single quotes: the tools expand nothing.
pub fn variable_in(text: &str) -> Option<String> {
    let c: Vec<char> = text.chars().collect();
    let (mut i, mut single) = (0, false);
    while i < c.len() {
        match c[i] {
            '\'' => single = !single,
            '$' if !single => {
                let n = c.get(i + 1).copied().unwrap_or(' ');
                if n == '{' || n == '(' {
                    return Some(format!("${n}"));
                }
                if n.is_ascii_alphabetic() || n == '_' {
                    let end =
                        (i + 1..c.len()).find(|&j| !(c[j].is_ascii_alphanumeric() || c[j] == '_')).unwrap_or(c.len());
                    return Some(c[i..end].iter().collect());
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `NAME=value` at the start of a command: a shell assignment (`c=./prog`).
pub fn assignment(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let ok = !name.is_empty()
        && name.chars().next().is_some_and(|x| x.is_ascii_alphabetic() || x == '_')
        && name.chars().all(|x| x.is_ascii_alphanumeric() || x == '_');
    ok.then_some(name)
}

fn path_like(t: &str) -> bool {
    let t = t.trim_matches(|x| x == '"' || x == '\'');
    t.starts_with(['/', '.', '~', '$'])
        || t.contains('/')
        || (t.contains('.') && !t.starts_with(|x: char| x.is_ascii_digit()))
}

/// What in one `debug` unit is shell and not debugger: the shell words and operators, in the order written,
/// without repeats. Empty when the unit is plain debugger text. Careful with what the debugger means itself:
/// `printf` and `echo` are its own commands (only a redirect or a pipe makes them shell), `<` after `run` is its
/// input file, `>` and `&&` are expressions in `print a > b` and `break f if a && b`.
pub fn debug_shell_items(unit: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        let s = format!("`{s}`");
        if !items.contains(&s) {
            items.push(s)
        }
    };
    let pieces = shell_pieces(unit, false);
    let flagged: Vec<bool> = pieces
        .iter()
        .map(|p| {
            let first = p.first();
            let own = matches!(first, "printf" | "echo");
            let redirected = p.redirects.iter().any(|r| path_like(&r.target)) || p.pipe.is_some();
            is_shell_word(first) && (!own || redirected || pieces.len() > 1)
        })
        .collect();
    for (n, p) in pieces.iter().enumerate() {
        let first = p.first().to_string();
        let follows = n > 0 && matches!(first.as_str(), "run" | "cont" | "c" | "attach");
        if let Some(j) = p.joiner {
            if flagged[n] || flagged[n - 1] || follows && flagged.iter().any(|f| *f) {
                add(j);
            }
        }
        if flagged[n] {
            add(&first);
            for r in p.redirects.iter().filter(|r| path_like(&r.target)) {
                add(&r.op);
            }
            if p.pipe.is_some() {
                add("|");
            }
        } else if p.pipe.as_deref().is_some_and(|f| is_shell_word(f.split_whitespace().next().unwrap_or(""))) {
            add("|");
        }
    }
    items
}

/// The fact for items found by [`debug_shell_items`], before anything runs. `cwd`: where `run ./prog` is relative to.
pub fn debug_fact(items: &[String], cwd: &str) -> String {
    let list = match items {
        [one] => format!("{one} is shell"),
        [head @ .., last] => format!("{} and {last} are shell", head.join(", ")),
        [] => String::new(),
    };
    format!(
        "{list}; the debugger runs no shell, so nothing was run. Its working directory is {cwd}; write input files with the bash tool, then `run ./prog < file`, and send one debugger command per `;`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "/tmp/claude-1000/-home-lctr/0813d0c7-a644-4a6f-8cd3-c8c257f6635e/scratchpad/re-t5/t1d";

    fn items(unit: &str) -> Vec<String> {
        debug_shell_items(unit)
    }

    #[test]
    fn t1d_first_debug_call_is_all_shell() {
        let u = format!("cd {DIR} && printf '0 0 A\\n1048576 0 B\\n' > /tmp/crash.in && run ./paint < /tmp/crash.in");
        assert_eq!(items(&u), ["`cd`", "`&&`", "`printf`", "`>`"]);
    }

    #[test]
    fn t1d_second_debug_call_units_each_name_their_shell_part() {
        assert_eq!(items("printf '0 0 A\\n1048576 0 B\\n' > /tmp/crash.in"), ["`printf`", "`>`"]);
        assert_eq!(items(&format!("cd {DIR}")), ["`cd`"]);
        assert!(items("run ./paint < /tmp/crash.in").is_empty());
        assert!(items("cont").is_empty());
    }

    #[test]
    fn the_debuggers_own_words_and_expressions_are_not_shell() {
        for u in [
            "printf \"%d\\n\", $rax",
            "echo hello",
            "print a > b",
            "break f if a && b",
            "break f if x > 3 || y < 2",
            "x/4x $rsp",
            "run ./p < in.txt",
            "print a | b",
        ] {
            assert!(items(u).is_empty(), "{u}: {:?}", items(u));
        }
    }

    #[test]
    fn pieces_keep_quotes_and_redirects_apart() {
        let p = shell_pieces("decomp ./paint 0x1170 2>/dev/null | head -1", true);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].text, "decomp ./paint 0x1170");
        assert_eq!(p[0].redirects, [Redirect { op: "2>".into(), target: "/dev/null".into() }]);
        assert_eq!(p[0].pipe.as_deref(), Some("head -1"));
        let p = shell_pieces("note ./p 'a && b | c'; x", true);
        assert_eq!(p.len(), 1);
        let p = shell_pieces("cd /d && fn ./p main 2>&1 || true", true);
        assert_eq!(p.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["cd /d", "fn ./p main", "true"]);
        assert_eq!(p[1].joiner, Some("&&"));
        assert_eq!(p[1].redirects[0].op, "2>&");
        // text with an operator inside a word is not shell
        assert_eq!(shell_pieces("funcs ./p --grep main|init", true).len(), 1);
        assert!(shell_pieces("funcs ./p --grep main|init", true)[0].pipe.is_none());
        assert!(shell_pieces("find ./p x>y a&&b", true)[0].redirects.is_empty());
    }

    #[test]
    fn variables_and_assignments() {
        assert_eq!(variable_in("funcs $c").as_deref(), Some("$c"));
        assert_eq!(variable_in("a ${HOME}").as_deref(), Some("${"));
        assert_eq!(variable_in("cost $5 '$x'"), None);
        assert_eq!(assignment("c=/tmp/x"), Some("c"));
        assert_eq!(assignment("--x=1"), None);
        assert_eq!(assignment("./a=b"), None);
    }

    #[test]
    fn the_fact_names_the_working_directory() {
        let f = debug_fact(&["`cd`".into(), "`printf`".into(), "`>`".into()], "/w");
        assert!(f.starts_with("`cd`, `printf` and `>` are shell; the debugger runs no shell"), "{f}");
        assert!(f.contains("working directory is /w;"), "{f}");
    }
}
