//! A tolerant reading of a shell command, for the one-line hints only (`bin_hint`, `dbg_hint`, `ask_hint`).
//!
//! The parser the bash tool judges permissions with (`nexo_tools::bash::command_words`) gives up on a redirection,
//! a heredoc, a group or a `cd`: right for a permission, too strict for a hint, which is lost on
//! `cat > g.gdb <<'EOF' ... EOF` followed by `gdb -x g.gdb prog 2>&1 | tail`, on `cd build && gdb prog` or on
//! `./prog > out.txt`. This one splits the same text into the commands that run, each with the folder it runs
//! in (`cd`, groups), its input (`< file`, `<<< text`, a heredoc body) and the file it writes. A leading `VAR=x`,
//! `sudo`, `env`, `timeout 10` or `bash -c '...'` is looked through.
//!
//! Nothing here runs, expands or opens anything. A substitution (`$(..)`, backticks, `<(..)`) or an open quote makes
//! the whole command unreadable (`None`): its words say too little to judge.

use std::path::{Path, PathBuf};

/// Which tools of the session can take what a hint names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offer {
    /// The `bin` tool is offered (otherwise `nexo bin` goes through the shell).
    pub bin: bool,
    /// The `debug` tool is offered.
    pub debug: bool,
}

/// What a command reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// `< file`.
    File(String),
    /// `<<< text`, or the body of a heredoc.
    Text(String),
}

/// One command of a line: its words (redirections, `VAR=x` and wrappers taken out), where it runs, its input and
/// the file it writes (`> f`, `>> f`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seg {
    pub words: Vec<String>,
    pub dir: PathBuf,
    pub input: Option<Input>,
    pub output: Option<String>,
    /// A heredoc opened on this command whose body has not been read yet: its delimiter, and whether `<<-`.
    pending: Option<(String, bool)>,
}

/// The most of a heredoc body kept.
const BODY_CAP: usize = 16 * 1024;
/// A longer command is not read.
const MAX_COMMAND: usize = 100_000;
/// How deep `sh -c '...'` is looked into.
const MAX_DEPTH: u8 = 2;

/// The commands of `command` run from `cwd`, in order. `None` for a command with a substitution or an open quote.
pub fn segments(command: &str, cwd: &Path) -> Option<Vec<Seg>> {
    if command.len() > MAX_COMMAND {
        return None;
    }
    Scan::new(command, cwd, 0).run()
}

/// The program a command word names, without its folder or `.exe`, in lower case.
pub fn program(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

/// Whether the word `w`, from the folder `dir`, names a file `nexo bin` opens (read as bytes, the first 4096 only).
pub fn opens(dir: &Path, w: &str) -> bool {
    !w.is_empty() && !w.starts_with('-') && nexo_bin::opens(&dir.join(w))
}

/// Like [`opens`], and a WebAssembly module too.
pub fn reads(dir: &Path, w: &str) -> bool {
    opens(dir, w) || (!w.is_empty() && !w.starts_with('-') && nexo_bin::is_wasm(&dir.join(w)))
}

/// The file a hint names, as a call writes it: the way the command did when it ran in the session's folder, the
/// whole path when a `cd` moved it.
pub fn shown(cwd: &Path, dir: &Path, w: &str) -> String {
    if Path::new(w).is_absolute() || dir == cwd {
        slash(w)
    } else {
        slash(&dir.join(w).display().to_string())
    }
}

/// A path as a call writes it. On Windows the separator is `/`, which every Windows program and the shell there
/// take, so a name never carries a backslash the call languages would read as an escape; elsewhere a backslash is
/// a character of the name and stays (the callers then leave such a name out).
pub fn slash(w: &str) -> String {
    if cfg!(windows) {
        w.replace('\\', "/")
    } else {
        w.to_string()
    }
}

/// A word as the command language of the `debug` and `bin` tools reads it, and as a shell would: double quotes
/// around one with anything but plain characters. Callers keep `"`, `\`, `$` and backticks out of what they quote.
pub fn quoted(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c)) {
        w.to_string()
    } else {
        format!("\"{w}\"")
    }
}

/// A word as a shell reads it: single quotes around one with anything but plain characters.
pub fn shell_quoted(w: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c);
    if !w.is_empty() && w.chars().all(plain) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// `tool {"command": "<command>"}`: the call of a tool as the hint lines write it.
pub fn tool_call(tool: &str, command: &str) -> String {
    format!("{tool} {{\"command\": \"{}\"}}", command.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Whether a failed shell result is of a command that ran: the shell tool gives those a line with the exit code,
/// or one that says it was killed (a dump too long for the time limit is where a hint helps most). Other lines
/// nexo adds may follow it. A refusal, a hook's block, a cancel or an interruption has none.
pub fn ran(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        let code = l.strip_prefix("[exit code ").and_then(|r| r.strip_suffix(']'));
        code.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
            || l == "[killed by a signal]"
            || (l.starts_with("[killed: timed out after ") && l.ends_with(" s]"))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redir {
    None,
    Out,
    /// A file for a stream other than stdout (`2> err.txt`): set aside, it is not the command's output.
    Other,
    In,
    Here,
}

struct Scan {
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
    fn new(command: &str, dir: &Path, depth: u8) -> Scan {
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

    fn run(mut self) -> Option<Vec<Seg>> {
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

/// `NAME=value`: a variable set for the command that follows.
fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(n, _)| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The script of `sh -c '...'` (also `-lc`, `-ec`).
fn shell_script(words: &[String]) -> Option<String> {
    let at = words
        .iter()
        .skip(1)
        .position(|a| a.starts_with('-') && !a.starts_with("--") && a.ends_with('c') && a.len() <= 4)?;
    words.get(at + 2).cloned()
}

/// The command a line of wrappers runs: `env A=1`, `sudo -u x`, `time`, `nohup`, `exec`, `nice -n 5`,
/// `timeout -s KILL 10`, `stdbuf -oL`.
fn strip_wrappers(mut w: Vec<String>) -> Vec<String> {
    loop {
        let Some(first) = w.first() else { return w };
        let rest = &w[1..];
        let opts = |valued: &[&str]| {
            let mut n = 0;
            while let Some(o) = rest.get(n).filter(|o| o.starts_with('-') && o.len() > 1) {
                n += 1 + valued.contains(&o.as_str()) as usize;
            }
            // a valued option at the end of the line (`sudo -u`) has no value to skip
            n.min(rest.len())
        };
        let skip = match program(first).as_str() {
            "env" => rest.iter().take_while(|a| a.starts_with('-') || is_assignment(a)).count(),
            "sudo" | "doas" => opts(&["-u", "-g", "-h", "-p", "-C", "-D", "-R", "-T", "-U"]),
            "time" | "nohup" | "exec" | "setsid" | "catchsegv" | "unbuffer" | "stdbuf" => opts(&[]),
            "nice" => opts(&["-n"]),
            "timeout" => {
                let n = opts(&["-s", "-k", "--signal", "--kill-after"]);
                let duration = |d: &String| {
                    !d.is_empty() && d.chars().all(|c| c.is_ascii_digit() || c == '.' || "smhd".contains(c))
                };
                n + rest.get(n).is_some_and(duration) as usize
            }
            _ => return w,
        };
        w.drain(..(1 + skip).min(w.len()));
        if w.first().is_some_and(|x| is_assignment(x)) {
            w.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(command: &str) -> Vec<Seg> {
        segments(command, Path::new("/work")).unwrap_or_else(|| panic!("unreadable: {command:?}"))
    }

    fn words(command: &str) -> Vec<Vec<String>> {
        at(command).into_iter().map(|s| s.words).collect()
    }

    fn w(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_pipeline_a_list_and_quotes_split_like_a_shell() {
        assert_eq!(
            words("objdump -d 'my prog' | head -5 && ls; echo \"a b\""),
            [w(&["objdump", "-d", "my prog"]), w(&["head", "-5"]), w(&["ls"]), w(&["echo", "a b"])]
        );
        assert_eq!(words("a\\ b c"), [w(&["a b", "c"])]);
        assert_eq!(words("# note, it's here\nls"), [w(&["ls"])]);
        assert_eq!(words("ls \\\n -l"), [w(&["ls", "-l"])]);
    }

    #[test]
    fn redirections_are_set_aside_not_taken_for_arguments() {
        let s = &at("gdb -q prog 2>&1 | tail -40")[0];
        assert_eq!(s.words, w(&["gdb", "-q", "prog"]));
        let s = &at("./paint big > out.txt 2>/dev/null &")[0];
        assert_eq!((s.words.clone(), s.output.clone()), (w(&["./paint", "big"]), Some("out.txt".to_string())));
        let s = &at("./paint &> all.txt")[0];
        assert_eq!((s.words.clone(), s.output.clone()), (w(&["./paint"]), Some("all.txt".to_string())));
        let s = &at("./paint >> log < in.txt")[0];
        assert_eq!(s.output.as_deref(), Some("log"));
        assert_eq!(s.input, Some(Input::File("in.txt".into())));
        let s = &at("./paint <<< '3 4 x'")[0];
        assert_eq!((s.words.clone(), s.input.clone()), (w(&["./paint"]), Some(Input::Text("3 4 x".into()))));
        assert_eq!(words("echo 5 1>&2"), [w(&["echo", "5"])]);
    }

    #[test]
    fn a_heredoc_body_goes_to_the_command_that_opened_it_and_is_never_read_as_commands() {
        let s = at("cat > g1.gdb <<'EOF'\nbreak *main\nrun $(rm -rf x) `y`\nend\nEOF\ngdb -x g1.gdb prog 2>&1 | tail");
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!(s[0].words, w(&["cat"]));
        assert_eq!(s[0].output.as_deref(), Some("g1.gdb"));
        assert_eq!(s[0].input, Some(Input::Text("break *main\nrun $(rm -rf x) `y`\nend\n".into())));
        assert_eq!(s[1].words, w(&["gdb", "-x", "g1.gdb", "prog"]));
        // piped into the program, `<<-` with tabs, and a body that never ends
        let s = at("cat <<EOF | gdb -batch prog\nrun\nEOF\nls");
        assert_eq!(
            (s[0].input.clone(), s[1].words.clone(), s[2].words.clone()),
            (Some(Input::Text("run\n".into())), w(&["gdb", "-batch", "prog"]), w(&["ls"]))
        );
        let s = at("cat <<-EOF\n\tx\n\tEOF\nls");
        assert_eq!((s[0].input.clone(), s.len()), (Some(Input::Text("\tx\n".into())), 2));
        assert_eq!(at("cat <<EOF\nnever closed")[0].input, Some(Input::Text("never closed\n".into())));
    }

    #[test]
    fn cd_and_groups_say_where_a_command_runs() {
        let s = at("cd /tmp/tests && gdb -batch prog; ls");
        assert_eq!((s[0].dir.clone(), s[1].dir.clone()), (PathBuf::from("/tmp/tests"), PathBuf::from("/tmp/tests")));
        let s = at("cd build; ./paint");
        assert_eq!(s[0].dir, PathBuf::from("/work/build"));
        // a group gives the folder back when it closes; `cd -`, `~` and `$X` say nothing
        let s = at("(cd build && ./a) ; ./b");
        assert_eq!((s[0].dir.clone(), s[1].dir.clone()), (PathBuf::from("/work/build"), PathBuf::from("/work")));
        assert_eq!(at("cd ~/x && ./a")[0].dir, PathBuf::from("/work"));
        assert_eq!(at("cd $HOME && ./a")[0].dir, PathBuf::from("/work"));
        assert_eq!(at("cd - && ./a")[0].dir, PathBuf::from("/work"));
    }

    #[test]
    fn variables_and_wrappers_are_looked_through() {
        assert_eq!(words("FOO=1 BAR=2 gdb -batch p"), [w(&["gdb", "-batch", "p"])]);
        assert_eq!(words("sudo -u me env A=1 timeout -s KILL 10 gdb p"), [w(&["gdb", "p"])]);
        assert_eq!(words("time nice -n 5 ./p x"), [w(&["./p", "x"])]);
        assert_eq!(words("stdbuf -oL ./p"), [w(&["./p"])]);
        assert_eq!(words("sh -c 'cd /x && gdb -batch p'"), [w(&["gdb", "-batch", "p"])]);
        assert_eq!(at("bash -lc \"cd /x; ./p\"")[0].dir, PathBuf::from("/x"));
        // `command -v` and `which` are not a run
        assert_eq!(words("command -v gdb"), [w(&["command", "-v", "gdb"])]);
        assert_eq!(words("which gdb"), [w(&["which", "gdb"])]);
    }

    #[test]
    fn a_wrapper_option_with_no_value_left_is_read_without_panicking() {
        // the line may be cut anywhere: `sudo -u` at the end has no user to skip
        for c in ["sudo -u", "sudo -u; ls", "timeout -s", "timeout -s KILL", "nice -n", "env -", "sudo -n -u -g"] {
            let _ = at(c);
        }
        assert_eq!(strip_wrappers(w(&["sudo", "-u"])), Vec::<String>::new());
        assert_eq!(strip_wrappers(w(&["timeout", "-s"])), Vec::<String>::new());
        assert_eq!(words("sudo -u me gdb p; sudo -u")[0], w(&["gdb", "p"]));
    }

    #[test]
    fn a_substitution_or_an_open_quote_is_unreadable_and_nothing_is_run() {
        for c in [
            "objdump -d $(touch touched) prog",
            "objdump -d `touch touched` prog",
            "echo \"$(touch touched)\"",
            "echo \"`touch touched`\"",
            "cat <(echo x)",
            "tee >(cat) x",
            "objdump -d 'unclosed",
            "echo \"unclosed",
        ] {
            assert!(segments(c, Path::new("/work")).is_none(), "{c}");
        }
        assert_eq!(segments("", Path::new("/work")).unwrap(), Vec::<Seg>::new());
        assert!(segments(&"a ".repeat(MAX_COMMAND), Path::new("/")).is_none());
        // single quotes keep every character as it is
        assert_eq!(words("echo '$(x) `y`'"), [w(&["echo", "$(x) `y`"])]);
    }

    #[test]
    fn small_helpers() {
        assert_eq!(program("/usr/bin/GDB.exe"), "gdb");
        assert_eq!(program("C:\\tools\\objdump.exe"), "objdump");
        assert_eq!(quoted("a b"), "\"a b\"");
        assert_eq!(quoted("ab"), "ab");
        assert_eq!(quoted("(?i)a|b"), "\"(?i)a|b\"");
        assert_eq!(quoted("./p-1.2"), "./p-1.2");
        assert_eq!(shell_quoted("a b"), "'a b'");
        assert_eq!(shell_quoted("./p-1.2"), "./p-1.2");
        assert_eq!(shell_quoted("it's"), "'it'\\''s'");
        assert_eq!(tool_call("debug", "run \"a b\"; cont"), r#"debug {"command": "run \"a b\"; cont"}"#);
        assert!(ran("x\n\n[exit code 1]") && ran("[exit code -1]\n") && ran("x\n\n[exit code 127]\n\n[nexo: more]"));
        assert!(
            ran("partial\n\n[killed: timed out after 120 s]")
                && ran("[killed by a signal]")
                && !ran("x\n\n[cancelled]")
        );
        assert!(!ran("Permission denied: [exit code ]") && !ran("[exit code 1] and more") && !ran("exit code 1"));
    }
}
