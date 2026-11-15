//! The scanner behind [`super::segments`]: one pass over the text, quotes, redirections, heredocs and groups.

use std::path::{Path, PathBuf};

use super::program;
use super::words::{is_assignment, shell_script, strip_wrappers};
use super::{Input, Seg, BODY_CAP, MAX_DEPTH};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redir {
    None,
    Out,
    /// A file for a stream other than stdout (`2> err.txt`): set aside, it is not the command's output.
    Other,
    In,
    Here,
}

pub(super) struct Scan {
    src: Vec<char>,
    i: usize,
    out: Vec<Seg>,
    cur: Seg,
    word: String,
    has: bool,
    redir: Redir,
    dir: PathBuf,
    stack: Vec<PathBuf>,
    depth: u8,
}

impl Scan {
    pub(super) fn new(command: &str, dir: &Path, depth: u8) -> Scan {
        Scan {
            src: command.chars().collect(),
            i: 0,
            out: Vec::new(),
            cur: Seg::default(),
            word: String::new(),
            has: false,
            redir: Redir::None,
            dir: dir.to_path_buf(),
            stack: Vec::new(),
            depth,
        }
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.i += c.is_some() as usize;
        c
    }

    pub(super) fn run(mut self) -> Option<Vec<Seg>> {
        while let Some(c) = self.next() {
            match c {
                '\'' => {
                    self.has = true;
                    loop {
                        match self.next()? {
                            '\'' => break,
                            ch => self.word.push(ch),
                        }
                    }
                }
                '"' => {
                    self.has = true;
                    loop {
                        match self.next()? {
                            '"' => break,
                            '`' => return None,
                            '$' if self.peek() == Some('(') => return None,
                            '\\' => match self.next()? {
                                ch @ ('"' | '\\' | '$' | '`') => self.word.push(ch),
                                '\n' => {}
                                ch => {
                                    self.word.push('\\');
                                    self.word.push(ch);
                                }
                            },
                            ch => self.word.push(ch),
                        }
                    }
                }
                '\\' => match self.next() {
                    Some('\n') | None => {}
                    Some(ch) => {
                        self.has = true;
                        self.word.push(ch);
                    }
                },
                '`' => return None,
                '$' if self.peek() == Some('(') => return None,
                '#' if !self.has => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\n' => {
                    self.seg_end();
                    self.heredoc_bodies();
                }
                ';' | '|' => self.seg_end(),
                '&' => {
                    if self.peek() == Some('>') {
                        // `&>` and `&>>`: both streams to a file
                        self.word_end();
                        self.i += 1;
                        if self.peek() == Some('>') {
                            self.i += 1;
                        }
                        self.redir = Redir::Out;
                    } else {
                        self.seg_end();
                    }
                }
                '>' => self.redirect_out()?,
                '<' => self.redirect_in()?,
                '(' => {
                    self.seg_end();
                    self.stack.push(self.dir.clone());
                }
                ')' => {
                    self.seg_end();
                    if let Some(d) = self.stack.pop() {
                        self.dir = d;
                    }
                }
                c if c.is_whitespace() => self.word_end(),
                c => {
                    self.has = true;
                    self.word.push(c);
                }
            }
        }
        self.seg_end();
        self.out.retain(|s| !s.words.is_empty());
        Some(self.out)
    }

    /// The word being read is done: an argument, or what the last redirection points at.
    fn word_end(&mut self) {
        if !self.has {
            return;
        }
        let w = std::mem::take(&mut self.word);
        self.has = false;
        match std::mem::replace(&mut self.redir, Redir::None) {
            Redir::Out => self.cur.output = Some(w),
            Redir::Other => {}
            Redir::In => self.cur.input = Some(Input::File(w)),
            Redir::Here => self.cur.input = Some(Input::Text(w)),
            Redir::None => {
                if !(self.cur.words.is_empty() && is_assignment(&w)) {
                    self.cur.words.push(w);
                }
            }
        }
    }

    /// The word before a redirection that is only a stream number (`2>`): not an argument. Says whether it named a
    /// stream other than stdout (`2`, not `1`).
    fn stream_number(&mut self) -> bool {
        if self.has && !self.word.is_empty() && self.word.chars().all(|c| c.is_ascii_digit()) {
            let other = self.word != "1";
            self.word.clear();
            self.has = false;
            other
        } else {
            self.word_end();
            false
        }
    }

    fn redirect_out(&mut self) -> Option<()> {
        let other = self.stream_number();
        match self.peek() {
            Some('(') => return None,
            Some('>' | '|') => self.i += 1,
            Some('&') => {
                self.i += 1;
                // `>&2`, `>&-`: a stream moved, no file
                if self.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
                    while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
                        self.i += 1;
                    }
                    return Some(());
                }
            }
            _ => {}
        }
        self.redir = if other { Redir::Other } else { Redir::Out };
        Some(())
    }

    fn redirect_in(&mut self) -> Option<()> {
        self.stream_number();
        match self.peek() {
            Some('(') => None,
            Some('<') => {
                self.i += 1;
                if self.peek() == Some('<') {
                    self.i += 1;
                    self.redir = Redir::Here;
                    Some(())
                } else {
                    self.heredoc_start()
                }
            }
            Some('&') => {
                self.i += 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
                    self.i += 1;
                }
                Some(())
            }
            _ => {
                self.redir = Redir::In;
                Some(())
            }
        }
    }

    /// After `<<`: the delimiter that ends the body, which starts on the next line.
    fn heredoc_start(&mut self) -> Option<()> {
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
                c if c.is_whitespace() || ";|&<>()".contains(c) => break,
                c => {
                    self.i += 1;
                    delim.push(c);
                }
            }
        }
        if delim.is_empty() {
            return None;
        }
        self.cur.pending.get_or_insert((delim, strip));
        Some(())
    }

    /// At a line end: the bodies of the heredocs the line opened, in the order they were opened.
    fn heredoc_bodies(&mut self) {
        for k in 0..self.out.len() {
            if let Some((delim, strip)) = self.out[k].pending.take() {
                let body = self.read_body(&delim, strip);
                self.out[k].input = Some(Input::Text(body));
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
            self.i += (self.i < self.src.len()) as usize;
            let line = line.trim_end_matches('\r');
            let tested = if strip { line.trim_start_matches('\t') } else { line };
            if tested == delim {
                break;
            }
            if body.len() < BODY_CAP {
                body.push_str(line);
                body.push('\n');
            }
        }
        body
    }

    /// A command is done (`;`, `|`, `&&`, a newline, a group edge): looked through, tracked for `cd`, kept.
    fn seg_end(&mut self) {
        self.word_end();
        self.redir = Redir::None;
        let mut seg = std::mem::take(&mut self.cur);
        if seg.words.is_empty() && seg.pending.is_none() {
            return;
        }
        seg.words.retain(|w| w != "{" && w != "}");
        seg.words = strip_wrappers(seg.words);
        seg.dir = self.dir.clone();
        let waits = seg.pending.is_some();
        match seg.words.first().map(|w| program(w)).as_deref() {
            Some("cd") => {
                // `cd`, `cd -` and `cd ~`/`cd $X` say nothing about a folder: what follows is searched where it was
                if let Some(t) = seg.words.get(1).filter(|t| !t.starts_with(['-', '~']) && !t.contains('$')) {
                    self.dir = self.dir.join(t);
                }
                if !waits {
                    return;
                }
            }
            Some("bash" | "sh" | "zsh" | "dash" | "ksh") => {
                if let Some(script) = shell_script(&seg.words).filter(|_| self.depth < MAX_DEPTH) {
                    if let Some(inner) = Scan::new(&script, &self.dir, self.depth + 1).run() {
                        self.out.extend(inner);
                    }
                    if !waits {
                        return;
                    }
                }
            }
            _ => {}
        }
        self.out.push(seg);
    }
}
