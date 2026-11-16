//! The one shell command lexer: the text of a command line cut into the commands a POSIX shell would run, each with
//! its words (quotes, backslashes and `$'..'` taken out), its redirections and its heredoc.
//!
//! Nothing here runs, expands or opens anything, and it makes no decision: whether a command is allowed, asked or
//! hinted is the caller's. It reports what it saw instead (a substitution, an open quote, a `${x@P}`), so each
//! caller can be as strict as its purpose needs. It never fails: a line it cannot read to the end still gives the
//! commands up to there, with the fact that it was cut short.

use std::ops::Range;

/// What ended a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The end of the text.
    Input,
    /// `;`.
    Semi,
    /// A line end.
    Newline,
    /// `&&`.
    And,
    /// `||`.
    Or,
    /// `|`: its output goes into the next command.
    Pipe,
    /// A lone `&`: the command goes to the background.
    Amp,
    /// `(`: a group opens after it.
    Open,
    /// `)`: a group closes after it.
    Close,
    /// A substitution opens after it: `$(`, `<(`, `>(` or a backtick.
    Subst,
}

/// A redirection: `2>> log`, `< in`, `>&2`, `<<< text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The descriptor in front (`2` of `2>`).
    pub fd: Option<String>,
    /// `>`, `>>`, `>|`, `>&`, `&>`, `&>>`, `<`, `<<<`, `<&` or `<>`.
    pub op: String,
    /// The word it points at, as the program gets it; `None` when the text ended before one.
    pub target: Option<String>,
    /// The same word as written, quotes and backslashes in.
    pub raw_target: String,
    /// Only moves a descriptor (`>&2`, `<&-`): no file.
    pub dup: bool,
    /// How many words of the command came before it.
    pub at: usize,
}

impl Redirect {
    /// Whether it writes (or opens for writing) rather than reads.
    pub fn writes(&self) -> bool {
        self.op.starts_with('>') || self.op.starts_with('&') || self.op == "<>"
    }
}

/// A heredoc opened on a command (`<<EOF`, `<<-'EOF'`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heredoc {
    pub delim: String,
    /// `<<-`: tabs in front of the lines are not part of them.
    pub strip: bool,
    /// The lines up to the delimiter, each ended by a newline; `None` while the text ended before them.
    pub body: Option<String>,
}

/// One command: what lies between two of the separators of [`End`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    /// Its words, redirections left out; leading `NAME=value` assignments are words.
    pub words: Vec<String>,
    pub redirects: Vec<Redirect>,
    pub heredoc: Option<Heredoc>,
    /// The comment that closes it (`# text`, without the `#`), when comments are read.
    pub comment: Option<String>,
    pub end: End,
    /// Where it lies in the text, in characters, up to the character that ended it.
    pub span: Range<usize>,
}

/// A substitution found: something else would run, or expand, inside the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subst {
    /// Where it starts, in characters.
    pub at: usize,
    /// Inside double quotes.
    pub quoted: bool,
    /// A backtick, which closes with another one and cuts no group.
    pub backtick: bool,
}

/// A command line, lexed.
#[derive(Debug, Clone, Default)]
pub struct Lexed {
    /// Every command, the empty ones between two separators too: callers drop them.
    pub cmds: Vec<Cmd>,
    pub src: Vec<char>,
    /// The text ended inside a quote.
    pub open_quote: bool,
    /// `$(..)`, `<(..)`, `>(..)` and backticks, in order.
    pub substs: Vec<Subst>,
    /// A `$'..'` string.
    pub ansi: bool,
    /// A `$".."` string: bash drops the `$` (and may put a translation in place of the text), a plain `sh`
    /// keeps it, so the word it gives depends on the shell.
    pub locale: bool,
    /// A `${..}` with an `@` operator inside (`${x@P}` runs what it expands) or left open.
    pub braced_at: bool,
    /// An unquoted `{` or `}`: a group, or a brace expansion.
    pub braces: bool,
    /// A `<<` without a delimiter.
    pub bad_heredoc: bool,
}

impl Lexed {
    /// The text of a span.
    pub fn text(&self, r: Range<usize>) -> String {
        self.src[r.start.min(self.src.len())..r.end.min(self.src.len())].iter().collect()
    }
}

/// Lex `text`. With `comments`, a `#` that starts a word is a comment to the end of the line; without,
/// it is an ordinary character (a reader that must still see every word of the line, as the supervisor's
/// path check does).
pub fn lex(text: &str, comments: bool) -> Lexed {
    let mut lx = Lx {
        src: text.chars().collect(),
        i: 0,
        comments,
        out: Lexed::default(),
        cur: Cmd::empty(),
        start: 0,
        word: String::new(),
        has: false,
        wstart: 0,
        wlast: 0,
        pending: None,
    };
    lx.run();
    let mut out = lx.out;
    out.src = lx.src;
    out
}

impl Cmd {
    fn empty() -> Cmd {
        Cmd { words: Vec::new(), redirects: Vec::new(), heredoc: None, comment: None, end: End::Input, span: 0..0 }
    }
}

struct Lx {
    src: Vec<char>,
    i: usize,
    comments: bool,
    out: Lexed,
    cur: Cmd,
    start: usize,
    word: String,
    has: bool,
    wstart: usize,
    wlast: usize,
    /// The redirection of `cur` still waiting for its word.
    pending: Option<usize>,
}

impl Lx {
    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.i += usize::from(c.is_some());
        c
    }

    /// A character of the word being read; `at` is where it was in the text.
    fn put(&mut self, at: usize, c: char) {
        self.begin(at);
        self.word.push(c);
        self.wlast = self.i;
    }

    /// The word being read has started (a quote starts one even when it stays empty).
    fn begin(&mut self, at: usize) {
        if !self.has {
            self.has = true;
            self.wstart = at;
        }
        self.wlast = self.i;
    }

    fn run(&mut self) {
        while let Some(c) = self.peek() {
            let at = self.i;
            self.i += 1;
            match c {
                '\'' => {
                    self.begin(at);
                    loop {
                        match self.next() {
                            Some('\'') => break,
                            Some(ch) => self.word.push(ch),
                            None => {
                                self.out.open_quote = true;
                                break;
                            }
                        }
                    }
                    self.wlast = self.i;
                }
                '"' => self.double_quoted(at),
                '$' if self.peek() == Some('\'') => self.ansi_quoted(at),
                '$' if self.peek() == Some('(') => self.subst(at, false, false, true),
                '$' if self.peek() == Some('"') => {
                    self.out.locale = true;
                    self.dollar(at);
                }
                '$' => self.dollar(at),
                '`' => self.subst(at, false, true, false),
                '<' | '>' if self.peek() == Some('(') => self.subst(at, false, false, true),
                '\\' => match self.next() {
                    Some('\n') | None => {}
                    Some(ch) => self.put(at, ch),
                },
                '#' if self.comments && !self.has => {
                    let from = self.i;
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                    self.cur.comment = Some(self.src[from..self.i].iter().collect());
                }
                '\n' => {
                    self.end(End::Newline, at);
                    self.heredoc_bodies();
                    self.start = self.i;
                }
                ';' => self.end(End::Semi, at),
                '|' => {
                    if self.peek() == Some('|') {
                        self.i += 1;
                        self.end(End::Or, at);
                    } else {
                        self.end(End::Pipe, at);
                    }
                }
                '&' => {
                    if self.peek() == Some('>') {
                        // `&>` and `&>>`: both streams to a file
                        self.word_end();
                        self.i += 1;
                        let mut op = String::from("&>");
                        if self.peek() == Some('>') {
                            self.i += 1;
                            op.push('>');
                        }
                        self.redirect(None, op);
                    } else if self.peek() == Some('&') {
                        self.i += 1;
                        self.end(End::And, at);
                    } else {
                        self.end(End::Amp, at);
                    }
                }
                '>' => self.redirect_out(),
                '<' => self.redirect_in(),
                '(' => self.end(End::Open, at),
                ')' => self.end(End::Close, at),
                '{' | '}' => {
                    self.out.braces = true;
                    self.put(at, c);
                }
                ' ' | '\t' => self.word_end(),
                c => self.put(at, c),
            }
        }
        let at = self.src.len();
        self.end(End::Input, at);
    }

    fn double_quoted(&mut self, at: usize) {
        self.begin(at);
        loop {
            let here = self.i;
            let Some(ch) = self.next() else {
                self.out.open_quote = true;
                break;
            };
            match ch {
                '"' => break,
                '`' => {
                    self.subst(here, true, true, false);
                    self.begin(self.i);
                }
                '$' if self.peek() == Some('(') => {
                    self.subst(here, true, false, true);
                    self.begin(self.i);
                }
                '$' => {
                    self.note_braced();
                    self.word.push('$');
                }
                '\\' => match self.next() {
                    Some(n @ ('"' | '\\' | '$' | '`')) => self.word.push(n),
                    Some('\n') => {}
                    Some(n) => {
                        self.word.push('\\');
                        self.word.push(n);
                    }
                    None => {
                        self.out.open_quote = true;
                        break;
                    }
                },
                ch => self.word.push(ch),
            }
            self.wlast = self.i;
        }
        self.wlast = self.i;
    }

    /// `$'..'`: a string where a backslash decodes (`\n`, `\x41`, `é`). Its end is found first, the way bash
    /// finds it (a backslash takes the next character, whatever it is), and only the text inside is decoded:
    /// an escape that reads a character after it (`\c`) never takes the closing quote.
    fn ansi_quoted(&mut self, at: usize) {
        self.i += 1;
        self.begin(at);
        self.out.ansi = true;
        let from = self.i;
        let to = loop {
            match self.next() {
                Some('\'') => break self.i - 1,
                Some('\\') if self.next().is_some() => {}
                Some('\\') | None => {
                    self.out.open_quote = true;
                    break self.i;
                }
                Some(_) => {}
            }
        };
        let inside = &self.src[from..to];
        let mut k = 0;
        while k < inside.len() {
            if inside[k] == '\\' && k + 1 < inside.len() {
                k = ansi_escape(inside, k + 1, &mut self.word);
            } else {
                self.word.push(inside[k]);
                k += 1;
            }
        }
        self.wlast = self.i;
    }

    /// A `$` that opens neither a substitution nor `$'..'`: a character of the word.
    fn dollar(&mut self, at: usize) {
        self.note_braced();
        self.put(at, '$');
    }

    /// A `${..}` that follows: whether it holds an `@` operator.
    fn note_braced(&mut self) {
        if self.peek() == Some('{') {
            self.out.braced_at |= braced_has_at(&self.src, self.i + 1);
        }
    }

    /// A substitution opens at `at`: the word and the command in front of it end there; what is inside is lexed as
    /// part of the line, the way a flat reader sees it.
    fn subst(&mut self, at: usize, quoted: bool, backtick: bool, paren: bool) {
        self.out.substs.push(Subst { at, quoted, backtick });
        if paren {
            self.i += 1;
        }
        self.end(End::Subst, at);
    }

    /// The word being read is done: an argument, or what the last redirection points at.
    fn word_end(&mut self) {
        if !self.has {
            return;
        }
        let w = std::mem::take(&mut self.word);
        let raw: String = self.src[self.wstart..self.wlast.max(self.wstart)].iter().collect();
        self.has = false;
        match self.pending.take() {
            Some(k) => {
                let r = &mut self.cur.redirects[k];
                if r.op == ">&" || r.op == "<&" {
                    r.dup = w == "-" || (!w.is_empty() && w.chars().all(|c| c.is_ascii_digit()));
                }
                r.target = Some(w);
                r.raw_target = raw;
            }
            None => self.cur.words.push(w),
        }
    }

    /// The word before a redirection when it is only a descriptor number (`2>`): not an argument.
    fn stream_number(&mut self) -> Option<String> {
        if self.has && !self.word.is_empty() && self.word.chars().all(|c| c.is_ascii_digit()) {
            self.has = false;
            Some(std::mem::take(&mut self.word))
        } else {
            self.word_end();
            None
        }
    }

    fn redirect(&mut self, fd: Option<String>, op: String) {
        let at = self.cur.words.len();
        self.cur.redirects.push(Redirect { fd, op, target: None, raw_target: String::new(), dup: false, at });
        self.pending = Some(self.cur.redirects.len() - 1);
    }

    fn redirect_out(&mut self) {
        let fd = self.stream_number();
        let mut op = String::from(">");
        if let Some(c @ ('>' | '|' | '&')) = self.peek() {
            self.i += 1;
            op.push(c);
        }
        self.redirect(fd, op);
    }

    fn redirect_in(&mut self) {
        let fd = self.stream_number();
        match self.peek() {
            Some('<') => {
                self.i += 1;
                if self.peek() == Some('<') {
                    self.i += 1;
                    self.redirect(fd, "<<<".into());
                } else {
                    self.heredoc_start();
                }
            }
            Some(c @ ('&' | '>')) => {
                self.i += 1;
                self.redirect(fd, format!("<{c}"));
            }
            _ => self.redirect(fd, "<".into()),
        }
    }

    /// After `<<`: the delimiter that ends the body, which starts on the next line.
    fn heredoc_start(&mut self) {
        let strip = self.peek() == Some('-');
        if strip {
            self.i += 1;
        }
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
        let mut delim = String::new();
        while let Some(c) = self.peek() {
            match c {
                '\'' | '"' => {
                    self.i += 1;
                    while let Some(q) = self.next() {
                        if q == c {
                            break;
                        }
                        delim.push(q);
                    }
                }
                '\\' => {
                    self.i += 1;
                    delim.extend(self.next());
                }
                c if matches!(c, ' ' | '\t' | '\n' | '\r') || ";|&<>()".contains(c) => break,
                c => {
                    self.i += 1;
                    delim.push(c);
                }
            }
        }
        if delim.is_empty() {
            self.out.bad_heredoc = true;
            return;
        }
        self.cur.heredoc.get_or_insert(Heredoc { delim, strip, body: None });
    }

    /// At a line end: the bodies of the heredocs the line opened, in the order they were opened.
    fn heredoc_bodies(&mut self) {
        for k in 0..self.out.cmds.len() {
            let Some(h) = self.out.cmds[k].heredoc.as_ref().filter(|h| h.body.is_none()) else { continue };
            let (delim, strip) = (h.delim.clone(), h.strip);
            let body = self.read_body(&delim, strip);
            if let Some(h) = self.out.cmds[k].heredoc.as_mut() {
                h.body = Some(body);
            }
        }
    }

    fn read_body(&mut self, delim: &str, strip: bool) -> String {
        let mut body = String::new();
        while self.i < self.src.len() {
            let start = self.i;
            while self.i < self.src.len() && self.src[self.i] != '\n' {
                self.i += 1;
            }
            let line: String = self.src[start..self.i].iter().collect();
            self.i += usize::from(self.i < self.src.len());
            let line = line.trim_end_matches('\r');
            let tested = if strip { line.trim_start_matches('\t') } else { line };
            if tested == delim {
                break;
            }
            body.push_str(line);
            body.push('\n');
        }
        body
    }

    /// A command is done: `at` is where its terminator is, and `self.i` is past it.
    fn end(&mut self, end: End, at: usize) {
        self.word_end();
        self.pending = None;
        let mut cmd = std::mem::replace(&mut self.cur, Cmd::empty());
        cmd.end = end;
        cmd.span = self.start..at.max(self.start);
        self.out.cmds.push(cmd);
        self.start = self.i;
    }
}

/// Whether the `${` whose inside starts at `from` holds an `@` operator (`${x@P}`, `${a[@]}`);
/// unclosed counts as holding one.
fn braced_has_at(chars: &[char], from: usize) -> bool {
    let mut depth = 1;
    for &c in chars.iter().skip(from) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            '@' => return true,
            _ => {}
        }
    }
    true
}

/// Decodes the escape of `$'..'` whose letter is at `at` into `out`; the index after it.
pub fn ansi_escape(chars: &[char], at: usize, out: &mut String) -> usize {
    let radix_run = |from: usize, radix: u32, max: usize| {
        let digits: String = chars[from..].iter().take(max).take_while(|c| c.is_digit(radix)).collect();
        let n = digits.len();
        (u32::from_str_radix(&digits, radix).ok().and_then(char::from_u32), n)
    };
    let simple = match chars[at] {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'e' | 'E' => Some('\x1b'),
        'f' => Some('\x0c'),
        'v' => Some('\x0b'),
        '\\' | '\'' | '"' | '?' => Some(chars[at]),
        _ => None,
    };
    if let Some(c) = simple {
        out.push(c);
        return at + 1;
    }
    let (decoded, used) = match chars[at] {
        'x' => radix_run(at + 1, 16, 2),
        'u' => radix_run(at + 1, 16, 4),
        'U' => radix_run(at + 1, 16, 8),
        '0'..='7' => {
            let (c, n) = radix_run(at, 8, 3);
            (c, n.saturating_sub(1))
        }
        'c' if at + 1 < chars.len() => (char::from_u32(chars[at + 1] as u32 & 0x1f), 1),
        _ => (None, 0),
    };
    match decoded {
        Some(c) if used > 0 || chars[at].is_digit(8) => {
            out.push(c);
            at + 1 + used
        }
        _ => {
            out.push('\\');
            out.push(chars[at]);
            at + 1
        }
    }
}

/// `NAME=value`, a variable set for the command that follows.
pub fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<Vec<String>> {
        lex(text, true).cmds.into_iter().map(|c| c.words).filter(|w| !w.is_empty()).collect()
    }

    fn one(text: &str) -> Vec<String> {
        words(text).remove(0)
    }

    #[test]
    fn a_list_and_a_pipeline_split_into_commands() {
        assert_eq!(
            words("a b; c | d && e || f\ng &"),
            vec![vec!["a", "b"], vec!["c"], vec!["d"], vec!["e"], vec!["f"], vec!["g"]]
        );
    }

    #[test]
    fn single_quotes_are_literal_and_a_double_quote_keeps_a_backslash_before_other_letters() {
        assert_eq!(one(r#"echo 'a\b' "r\m" "a\"b" "a\\b""#), vec!["echo", r"a\b", r"r\m", r#"a"b"#, r"a\b"]);
    }

    #[test]
    fn a_backslash_newline_joins_the_lines_and_a_trailing_backslash_is_dropped() {
        assert_eq!(one("echo a\\\nb"), vec!["echo", "ab"]);
        assert_eq!(one("echo a\\"), vec!["echo", "a"]);
    }

    #[test]
    fn an_ansi_c_string_is_decoded_and_flagged() {
        let l = lex(r"$'\x72m' x $'it\'s'", false);
        assert!(l.ansi && !l.open_quote);
        assert_eq!(l.cmds[0].words, vec!["rm", "x", "it's"]);
    }

    #[test]
    fn an_escape_in_an_ansi_c_string_never_reads_past_the_quote_that_closes_it() {
        // bash ends `$'..'` first, a backslash taking the next character, and decodes after: `\c` takes no `'`
        assert_eq!(
            words(r"cat $'\c' ; touch pwned ; '\'"),
            vec![vec!["cat", r"\c"], vec!["touch", "pwned"], vec![r"\"]]
        );
        assert_eq!(one(r"echo $'\c\'x' $'a\\'"), vec!["echo", "\u{1c}'x", r"a\"]);
        assert!(lex(r"echo $'a\", false).open_quote);
    }

    #[test]
    fn a_locale_string_is_flagged_because_bash_drops_its_dollar_and_a_plain_sh_keeps_it() {
        assert!(lex(r#"rg x $"--pre" sh f"#, false).locale);
        assert!(!lex(r#"echo "$" "a$" \$"x" '$"y'"#, false).locale);
    }

    #[test]
    fn a_comment_starts_only_at_the_start_of_a_word() {
        assert_eq!(words("ls # $(rm x) > f\nrm a#b"), vec![vec!["ls"], vec!["rm", "a#b"]]);
        assert!(lex("ls # $(rm x)", true).substs.is_empty());
        assert_eq!(lex("ls # $(rm x)", false).substs.len(), 1);
    }

    #[test]
    fn a_non_breaking_space_does_not_split_a_word_as_in_bash() {
        assert_eq!(one("ls\u{a0}-la"), vec!["ls\u{a0}-la"]);
    }

    #[test]
    fn a_redirection_is_kept_out_of_the_words_and_reported_with_its_target() {
        let l = lex("cat a 2>&1 >out <in 3>&-", false);
        let c = &l.cmds[0];
        assert_eq!(c.words, vec!["cat", "a"]);
        assert_eq!(c.redirects.len(), 4);
        assert!(c.redirects[0].dup);
        assert_eq!(c.redirects[1].target.as_deref(), Some("out"));
    }

    #[test]
    fn a_heredoc_body_is_read_at_the_newline_and_is_not_lexed_as_commands() {
        let l = lex("cat <<'EOF' | tail\nrun $(rm x)\nEOF\nls", false);
        assert!(l.substs.is_empty());
        let h = l.cmds[0].heredoc.as_ref().unwrap();
        assert_eq!(h.delim, "EOF");
        assert_eq!(h.body.as_deref(), Some("run $(rm x)\n"));
        assert_eq!(l.cmds.last().unwrap().words, vec!["ls"]);
    }

    #[test]
    fn an_apostrophe_inside_a_heredoc_body_does_not_swallow_the_next_command() {
        let l = lex("cat <<EOF\nit's\nEOF\nrm x", false);
        assert!(!l.open_quote);
        assert_eq!(l.cmds.last().unwrap().words, vec!["rm", "x"]);
    }

    #[test]
    fn substitutions_quotes_and_groups_are_reported_not_decided() {
        // a backtick pair is two marks: the opening one and the closing one
        assert_eq!(lex("a $(b) `c` <(d) \"$(e)\"", false).substs.len(), 5);
        assert!(lex("echo 'open", false).open_quote);
        assert!(lex("echo ${x@P}", false).braced_at);
        assert!(!lex("echo ${x}", false).braced_at);
        assert!(lex("{ a; }", false).braces);
        assert_eq!(lex("(a)", false).cmds[0].end, End::Open);
    }

    #[test]
    fn only_a_letter_or_underscore_starts_a_variable_assignment() {
        assert!(is_assignment("A=1") && is_assignment("_a1=") && !is_assignment("1A=x") && !is_assignment("=x"));
        assert!(!is_assignment("a.b=1") && !is_assignment("--flag=1"));
    }
}
