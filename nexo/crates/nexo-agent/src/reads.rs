//! Files the model already has: a `read` of a file it read (or that was attached), or a shell
//! command that only prints part of one (`cat`, `sed -n`, `head`, `tail`), when the file has not
//! changed since and every line asked for is in an earlier result still in what the model sees,
//! gets a short note instead of the same lines again. Asked again right after the note, it gets
//! the lines.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use nexo_core::message::{Message, Part};
use nexo_core::tool::{ToolDisplay, ToolOutput};

/// What the curator puts where it left something out.
const OMITTED: &str = "[curator]";
/// The ends of the lines that head a file put whole in a request: the person's `@path` attachment, and a project
/// file nexo included by itself.
pub(crate) use nexo_core::agent::{headed_file, ATTACHED, INCLUDED};
/// The fixed phrases of the note a repeated read gets (see `is_unchanged_note`).
const UNCHANGED: &str = ": unchanged since shown above";
const ASK_AGAIN: &str = "If that is no longer in the conversation, ask again and you get it in full.";

/// Whether `text` is the note a repeated read gets: it points at lines shown earlier, holds none.
pub(crate) fn is_unchanged_note(text: &str) -> bool {
    text.contains(UNCHANGED) && text.contains(ASK_AGAIN)
}

/// The line that heads a file the person attached whole to a request with `@path`.
pub fn attached_header(shown: &str) -> String {
    format!("@{shown} {ATTACHED}")
}

/// The line that heads a project file nexo put whole in a request by itself: no `@`, and it names nexo, so it
/// does not read as something the person attached.
pub fn included_header(shown: &str) -> String {
    format!("{shown} {INCLUDED}")
}

/// The lines asked for, 1-based: `From(a, Some(b))` is lines a to b, `From(a, None)` from a to
/// the end, `Last(n)` the last n.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Span {
    From(usize, Option<usize>),
    Last(usize),
}

/// Lines `start..=end` of a file, in the result of `anchor` (a call, or an attachment's header).
struct Piece {
    start: usize,
    end: usize,
    anchor: String,
}

struct Seen {
    hash: u64,
    pieces: Vec<Piece>,
    /// The last ask for lines it had was answered with a note.
    noted: bool,
}

#[derive(Default)]
pub struct Reads {
    seen: HashMap<PathBuf, Seen>,
    /// The anchors whose content the last model call saw whole.
    in_view: HashSet<String>,
    /// The files `edit` and `write` changed: what the file held then, the call that did it, and whether that
    /// call holds the whole file (a `write`) or only the change (an `edit`).
    wrote: HashMap<PathBuf, (u64, String, bool)>,
}

/// The key a file is kept under: its canonical path, so the same file reached by a symlink, a short Windows
/// name, a `\\?\` prefix or another drive letter case is one entry. A path that cannot be resolved is kept as given.
fn key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn digest(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Whether `pieces` together hold every line of `bytes` (a part of a file is not the file); with `view`, only
/// the pieces whose anchor is in it count.
fn covers(pieces: &[Piece], bytes: &[u8], view: Option<&HashSet<String>>) -> bool {
    let total = String::from_utf8_lossy(bytes).lines().count();
    let mut at = 1;
    while at <= total {
        let next = pieces
            .iter()
            .filter(|p| p.start <= at && at <= p.end && view.is_none_or(|v| v.contains(&p.anchor)))
            .map(|p| p.end)
            .max();
        match next {
            Some(e) => at = e + 1,
            None => return false,
        }
    }
    true
}

impl Reads {
    pub fn clear(&mut self) {
        self.seen.clear();
        self.in_view.clear();
        self.wrote.clear();
    }

    /// Notes what the model call about to be made carries whole.
    pub fn note_view(&mut self, messages: &[Message]) {
        self.in_view.clear();
        for m in messages {
            for p in &m.content {
                match p {
                    Part::ToolResult { call_id, content, is_error: false, .. } => {
                        if !content.iter().any(|c| matches!(c, Part::Text { text } if text.contains(OMITTED))) {
                            self.in_view.insert(call_id.clone());
                        }
                    }
                    Part::Text { text } if !text.contains(OMITTED) => {
                        for line in text.lines().filter(|l| headed_file(l).is_some()) {
                            self.in_view.insert(line.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Whether the model already met `path` as it is now: it read, wrote or was given it in this session
    /// and the file has not changed since. Such a file is not attached again by the token budget.
    pub fn met_unchanged(&self, path: &Path) -> bool {
        let Some(s) = self.seen.get(&key(path)) else { return false };
        let Ok(b) = std::fs::read(path) else { return false };
        if digest(&b) != s.hash {
            return false;
        }
        covers(&s.pieces, &b, None)
    }

    /// Why the model holds the whole of `path`, as it is now (`bytes`), in what it sees: a `write` of it, whose
    /// result is still in view, with the file unchanged since; or, for a file its own commands made (`made`),
    /// an output of it in full that is still in view. None for a file only `edit`ed (the view holds the change,
    /// not the file) and for anything changed since.
    pub fn held_whole(&self, path: &Path, bytes: &[u8], made: bool) -> Option<String> {
        let hash = digest(bytes);
        if let Some((_, call, whole)) = self.wrote.get(&key(path)).filter(|w| w.0 == hash) {
            return (*whole && self.in_view.contains(call)).then(|| format!("the file you wrote in call {call}"));
        }
        let s = self.seen.get(&key(path)).filter(|s| made && s.hash == hash)?;
        covers(&s.pieces, bytes, Some(&self.in_view))
            .then(|| "a file your own commands made, shown in full above".into())
    }

    /// A file attached whole to a request, under `header`.
    pub fn attached(&mut self, path: PathBuf, bytes: &[u8], header: String) {
        let end = String::from_utf8_lossy(bytes).lines().count();
        let pieces = vec![Piece { start: 1, end, anchor: header }];
        self.seen.insert(key(&path), Seen { hash: digest(bytes), pieces, noted: false });
    }

    /// After `edit` or `write` changed `path`: what the model knew of it is gone, and when the result of
    /// call `call_id` holds the file whole (a short one) the model has all of it. `whole`: the call itself
    /// holds the whole file (a `write`), not only a change.
    pub fn after_change(&mut self, path: &Path, call_id: &str, whole: bool) {
        let Ok(bytes) = std::fs::read(path) else { return };
        let text = String::from_utf8_lossy(&bytes);
        let pieces = match nexo_tools::edit::whole(&text) {
            Some(_) => vec![Piece { start: 1, end: text.lines().count(), anchor: call_id.to_string() }],
            None => Vec::new(),
        };
        self.wrote.insert(key(path), (digest(&bytes), call_id.to_string(), whole));
        self.seen.insert(key(path), Seen { hash: digest(&bytes), pieces, noted: false });
    }

    /// After a successful `read` of `path` (lines from `offset`, `limit` of them):
    /// the note the model gets instead of the output when it has those lines already.
    pub fn after_read(
        &mut self,
        path: &Path,
        shown: &str,
        offset: Option<u64>,
        limit: u64,
        call_id: &str,
        printed: usize,
    ) -> Option<String> {
        let start = offset.unwrap_or(1).max(1) as usize;
        let end = start + limit.max(1) as usize - 1;
        self.after_lines(path, shown, Span::From(start, Some(end)), call_id, printed)
    }

    /// After a call that printed `span` of `path`, in an output of `printed` characters: the note
    /// the model gets instead of the output when it has those lines already. An output shorter than
    /// the lines it was to hold was cut by the tool, and does not count as holding them.
    pub fn after_lines(
        &mut self,
        path: &Path,
        shown: &str,
        span: Span,
        call_id: &str,
        printed: usize,
    ) -> Option<String> {
        self.lines_gated(path, shown, span, call_id, printed, None)
    }

    /// `after_lines`, where `gate` (when given) says whether the last ask for lines of this file was answered
    /// with a note, in place of what the file remembers: the prints of one chained command are all one ask.
    fn lines_gated(
        &mut self,
        path: &Path,
        shown: &str,
        span: Span,
        call_id: &str,
        printed: usize,
        gate: Option<bool>,
    ) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        let hash = digest(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        let total = text.lines().count();
        let (start, end) = bounds(span, total);
        if start > end {
            return None;
        }
        let in_view = &self.in_view;
        let seen = self.seen.entry(key(path)).or_insert(Seen { hash, pieces: Vec::new(), noted: false });
        if seen.hash != hash {
            *seen = Seen { hash, pieces: Vec::new(), noted: false };
        }
        // walk from `start`, each time through the piece in view that reaches furthest
        let mut used: Vec<&Piece> = Vec::new();
        let mut at = start;
        while at <= end {
            let best = seen
                .pieces
                .iter()
                .filter(|p| p.start <= at && at <= p.end && in_view.contains(&p.anchor))
                .max_by_key(|p| p.end);
            match best {
                Some(p) => {
                    used.push(p);
                    at = p.end + 1;
                }
                None => break,
            }
        }
        if at > end && !gate.unwrap_or(seen.noted) {
            let whence = if used.iter().any(|p| p.anchor.ends_with(ATTACHED)) {
                "it was attached in full to a message above".to_string()
            } else if used.iter().any(|p| p.anchor.ends_with(INCLUDED)) {
                "nexo included it in full in a message above".to_string()
            } else {
                let parts: Vec<String> = used.iter().map(|p| format!("{}-{}", p.start, p.end)).collect();
                format!("lines {start}-{end} are inside your earlier output of lines {} above", parts.join(", "))
            };
            seen.noted = true;
            return Some(format!(
                "{shown}{UNCHANGED} ({}); not repeated: {whence}. {ASK_AGAIN}",
                nexo_core::count(end + 1 - start, "line")
            ));
        }
        seen.noted = false;
        let need: usize = text.lines().skip(start - 1).take(end + 1 - start).map(|l| l.chars().count()).sum::<usize>()
            + (end - start);
        if printed < need {
            return None;
        }
        seen.pieces.retain(|p| !(p.start == start && p.end == end));
        seen.pieces.push(Piece { start, end, anchor: call_id.to_string() });
        None
    }

    /// After a `bash` call that was not one print but several commands chained with `;` or `&&`, with
    /// `output` as the model would get it: `output` with a note in place of each print that repeats lines
    /// the model has already (see `after_lines`), None when no piece was repeated. A print counts only when
    /// the bytes it prints from the file as it is now are found, whole lines, in the output after the previous
    /// one; any other print is left as it is. The pieces not repeated are registered like a single print.
    pub fn after_chain(&mut self, cwd: &Path, cmd: &str, output: &str, call_id: &str) -> Option<String> {
        let prints = shell_chain(cmd)?;
        // whether the last ask for each file was answered with a note, as of before this command
        let mut gates: HashMap<PathBuf, bool> = HashMap::new();
        let mut at = 0;
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for p in prints {
            let abs = if p.path.is_absolute() { p.path.clone() } else { cwd.join(&p.path) };
            let Ok(bytes) = std::fs::read(&abs) else { continue };
            let Some(want) = print_bytes(&bytes, &p) else { continue };
            let Some((from, to)) = find_lines(output, at, &want) else { continue };
            at = to;
            let k = key(&abs);
            let gate = *gates.entry(k.clone()).or_insert_with(|| self.seen.get(&k).is_some_and(|s| s.noted));
            let shown = p.path.to_string_lossy();
            if let Some(note) = self.lines_gated(&abs, &shown, p.span, call_id, want.chars().count(), Some(gate)) {
                edits.push((from, to, note));
            }
        }
        if edits.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(output.len());
        let mut last = 0;
        for (from, to, note) in edits {
            out.push_str(&output[last..from]);
            out.push_str(&note);
            last = to;
        }
        out.push_str(&output[last..]);
        Some(out)
    }
}

/// How many characters of the lines asked for `out` holds: a `read` counts the lines it shows,
/// without their numbers, and an output the tool cut in the middle holds none for sure (what was
/// added to each line, such as `cat -n` numbers, can outweigh what the cut took out).
pub fn printed(out: &ToolOutput) -> usize {
    match &out.display {
        ToolDisplay::File { text, .. } => text.chars().count(),
        _ if out.content.contains(" characters omitted] …") => 0,
        _ => out.content.chars().count(),
    }
}

/// The first and last line (1-based) of a file of `total` lines that `span` asks for; start > end when none.
fn bounds(span: Span, total: usize) -> (usize, usize) {
    match span {
        Span::From(a, b) => (a.max(1), b.unwrap_or(total).min(total)),
        Span::Last(n) => (total.saturating_sub(n) + 1, total),
    }
}

/// One print among the commands of a chained shell command: the file (the directory of the `cd` before it
/// included), the lines, and whether they come numbered (`cat -n`).
#[derive(Debug, PartialEq)]
pub struct Print {
    pub path: PathBuf,
    pub span: Span,
    pub numbered: bool,
}

/// The prints in a command of several parts chained with `;` or `&&`, in order. The command is cut only where
/// those stand outside quotes; anything the scan cannot follow (a newline, a heredoc, a backtick, `$(`,
/// parentheses, braces, `||`, a lone `&`, a backslash outside quotes) gives None, and so does a command with no
/// print in it. The other parts may be anything; only a part that is a plain print (see `shell_print`) counts.
pub fn shell_chain(cmd: &str) -> Option<Vec<Print>> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        match quote {
            Some('\'') => {
                if c == '\n' {
                    return None;
                }
                if c == '\'' {
                    quote = None;
                }
                cur.push(c);
            }
            Some(_) => {
                match c {
                    '"' => quote = None,
                    '\n' | '`' => return None,
                    '$' if it.peek() == Some(&'(') => return None,
                    '\\' => {
                        cur.push(c);
                        match it.next() {
                            Some('\n') | None => return None,
                            Some(n) => cur.push(n),
                        }
                        continue;
                    }
                    _ => {}
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\n' | '`' | '(' | ')' | '{' | '}' | '\\' => return None,
                '<' if it.peek() == Some(&'<') => return None,
                '|' if it.peek() == Some(&'|') => return None,
                '&' if it.peek() == Some(&'&') => {
                    it.next();
                    parts.push(std::mem::take(&mut cur));
                }
                '&' => return None,
                ';' => parts.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(cur);
    // the directory later parts run in; None once a `cd` the scan cannot follow was met
    let mut dir = Some(PathBuf::new());
    let mut prints = Vec::new();
    for part in parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let w = words(part);
        if w.first().is_some_and(|c| c == "cd") {
            dir = match (w.as_slice(), dir) {
                ([_, d], Some(mut base)) if !d.starts_with(['-', '~']) && !d.contains(['$', '*', '?']) => {
                    base.push(d);
                    Some(base)
                }
                _ => None,
            };
            continue;
        }
        if part.contains(PLAIN_NOT) {
            continue;
        }
        let (Some(dir), Some((file, span, numbered))) = (&dir, print_of(&w)) else { continue };
        prints.push(Print { path: dir.join(file), span, numbered });
    }
    (!prints.is_empty()).then_some(prints)
}

/// The bytes `print` writes from `file` as it is now, as text, without the newline that ends them; None when
/// it prints nothing.
fn print_bytes(file: &[u8], print: &Print) -> Option<String> {
    let text = String::from_utf8_lossy(file);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (start, end) = bounds(print.span, lines.len());
    if start > end {
        return None;
    }
    let mut s = String::new();
    for (i, line) in lines[start - 1..end].iter().enumerate() {
        if print.numbered {
            s.push_str(&format!("{:>6}\t", start + i));
        }
        s.push_str(line);
    }
    if s.ends_with('\n') {
        s.pop();
    }
    (!s.is_empty()).then_some(s)
}

/// Where `want` stands in `output` at or after `from`, as whole lines: the first place that starts a line and
/// is followed by a line end or the end of the output.
fn find_lines(output: &str, from: usize, want: &str) -> Option<(usize, usize)> {
    let mut i = from;
    while let Some(off) = output.get(i..)?.find(want) {
        let (s, e) = (i + off, i + off + want.len());
        let starts = s == 0 || output.as_bytes()[s - 1] == b'\n';
        let ends = e == output.len() || output[e..].starts_with(['\n', '\r']);
        if starts && ends {
            return Some((s, e));
        }
        i = s + output[s..].chars().next()?.len_utf8();
    }
    None
}

/// The file and the lines a shell command prints, when printing them is all it does: `cat [-n] F`,
/// `sed -n 'A,Bp' F` (or `Ap`), `head [-n N | -N] F`, `tail [-n N | -n +N | -N] F`, after
/// `cd D;` or `cd D &&` if any. Pipes, redirections, globs, expansions and other commands: None.
pub fn shell_print(cmd: &str) -> Option<(PathBuf, Span)> {
    if cmd.contains(PLAIN_NOT) {
        return None;
    }
    let flat = cmd.replace("&&", ";");
    if flat.contains('&') {
        return None;
    }
    let segments: Vec<&str> = flat.split(';').map(str::trim).filter(|s| !s.is_empty()).collect();
    let (last, before) = segments.split_last()?;
    let mut dir = PathBuf::new();
    for s in before {
        let words = words(s);
        match words.as_slice() {
            [cd, d] if cd == "cd" => dir.push(d),
            _ => return None,
        }
    }
    let words = words(last);
    let (file, span, _) = print_of(&words)?;
    Some((dir.join(file), span))
}

/// What a command may not hold to be a plain print: pipes, redirections, expansions, globs, groups, escapes.
const PLAIN_NOT: [char; 13] = ['|', '>', '<', '$', '`', '*', '?', '(', ')', '{', '}', '\n', '\\'];

/// The file, the lines and whether they come numbered, of the plain print the `words` of a command make: `cat [-n] F`,
/// `sed -n 'A,Bp' F` (or `Ap`), `head [-n N | -N] F`, `tail [-n N | -n +N | -N] F`.
fn print_of(words: &[String]) -> Option<(&str, Span, bool)> {
    let count = |s: &str| s.parse::<usize>().ok();
    let (file, span, numbered) = match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["cat", f] => (*f, Span::From(1, None), false),
        ["cat", "-n", f] => (*f, Span::From(1, None), true),
        ["sed", "-n", script, f] => {
            let range = script.strip_suffix('p')?;
            let (a, b) = match range.split_once(',') {
                Some((a, b)) => (count(a)?, count(b)?),
                None => (count(range)?, count(range)?),
            };
            (*f, Span::From(a, Some(b)), false)
        }
        ["head", f] => (*f, Span::From(1, Some(10)), false),
        ["head", "-n", n, f] => (*f, Span::From(1, Some(count(n)?)), false),
        ["head", n, f] if n.starts_with('-') => (*f, Span::From(1, Some(count(&n[1..])?)), false),
        ["tail", f] => (*f, Span::Last(10), false),
        ["tail", "-n", n, f] => match n.strip_prefix('+') {
            Some(a) => (*f, Span::From(count(a)?, None), false),
            None => (*f, Span::Last(count(n)?), false),
        },
        ["tail", n, f] if n.starts_with('-') => (*f, Span::Last(count(&n[1..])?), false),
        _ => return None,
    };
    if file.starts_with('-') {
        return None;
    }
    Some((file, span, numbered))
}

/// The words of a simple command, quotes taken off (a quoted word with a space in it: no words).
fn words(s: &str) -> Vec<String> {
    let w: Vec<String> = s.split_whitespace().map(|w| w.trim_matches(['\'', '"']).to_string()).collect();
    if w.iter().any(|w| w.contains(['\'', '"'])) {
        return Vec::new();
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_one_entry_however_its_path_is_spelled() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\n").unwrap();
        // the path a tool call builds (not canonical) and the one a named file resolves to (canonical)
        let spelled = d.path().join("sub").join("..").join("a.txt");
        let real = p.canonicalize().unwrap();
        let mut r = Reads::default();
        r.after_change(&spelled, "w1", true);
        r.note_view(&[result("w1", "wrote a.txt")]);
        assert!(r.met_unchanged(&real));
        assert!(r.held_whole(&real, b"one\n", false).is_some_and(|how| how.contains("call w1")));
        assert!(r.held_whole(&spelled, b"one\n", false).is_some());
    }

    #[test]
    fn a_file_read_only_in_part_is_not_met_whole() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\nthree\nfour\n").unwrap();
        let mut r = Reads::default();
        r.after_read(&p, "a.txt", Some(1), 1, "c1", usize::MAX);
        assert!(!r.met_unchanged(&p), "the model saw 1 line of 4");
    }

    fn result(id: &str, text: &str) -> Message {
        Message::tool_results(vec![Part::ToolResult {
            call_id: id.into(),
            name: Some("read".into()),
            content: vec![Part::text(text)],
            is_error: false,
        }])
    }

    #[test]
    fn a_second_read_of_an_unchanged_file_in_view_gets_a_note_once() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let mut r = Reads::default();
        assert!(r.after_read(&p, "a.txt", None, 2000, "c1", usize::MAX).is_none());
        r.note_view(&[result("c1", "     1│ one")]);
        let note = r.after_read(&p, "a.txt", Some(2), 1, "c2", usize::MAX).unwrap();
        assert!(note.contains("not repeated"), "{note}");
        // asked again right after the note: the model wants the lines
        assert!(r.after_read(&p, "a.txt", None, 2000, "c3", usize::MAX).is_none());
    }

    #[test]
    fn a_changed_file_or_one_out_of_view_is_read_in_full() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut r = Reads::default();
        r.after_read(&p, "a.txt", None, 2000, "c1", usize::MAX);
        // the curator left the result out
        r.note_view(&[result("c1", "[curator] read output omitted as no longer needed")]);
        assert!(r.after_read(&p, "a.txt", None, 2000, "c2", usize::MAX).is_none());
        r.note_view(&[result("c2", "     1│ one")]);
        std::fs::write(&p, "one\nTWO\n").unwrap();
        assert!(r.after_read(&p, "a.txt", None, 2000, "c3", usize::MAX).is_none());
    }

    #[test]
    fn a_part_does_not_cover_more_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "1\n2\n3\n4\n").unwrap();
        let mut r = Reads::default();
        r.after_read(&p, "a.txt", Some(1), 2, "c1", usize::MAX);
        r.note_view(&[result("c1", "x")]);
        assert!(r.after_read(&p, "a.txt", None, 2000, "c2", usize::MAX).is_none());
    }

    #[test]
    fn an_attached_file_counts_as_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\n").unwrap();
        let mut r = Reads::default();
        let header = attached_header("a.txt");
        r.attached(p.clone(), b"one\n", header.clone());
        r.note_view(&[Message::user(format!("look\n\n{header}\n```txt\none\n```"))]);
        let note = r.after_read(&p, "a.txt", None, 2000, "c1", usize::MAX).unwrap();
        assert!(note.contains("attached"), "{note}");
    }

    #[test]
    fn a_file_nexo_included_counts_as_read_and_is_not_called_attached() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\n").unwrap();
        let mut r = Reads::default();
        let header = included_header("a.txt");
        assert_ne!(header, attached_header("a.txt"));
        assert!(!header.starts_with('@'), "{header}");
        r.attached(p.clone(), b"one\n", header.clone());
        assert!(r.met_unchanged(&p));
        r.note_view(&[Message::user(format!("[project at the start of this request]\n{header}\n```txt\none\n```"))]);
        let note = r.after_read(&p, "a.txt", None, 2000, "c1", usize::MAX).unwrap();
        assert!(note.contains("nexo included it in full") && !note.contains("attached"), "{note}");
    }

    #[test]
    fn parts_read_in_several_calls_together_cover_a_range() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, (1..=30).map(|i| format!("{i}\n")).collect::<String>()).unwrap();
        let mut r = Reads::default();
        assert!(r.after_lines(&p, "a.txt", Span::From(1, Some(12)), "c1", usize::MAX).is_none());
        assert!(r.after_lines(&p, "a.txt", Span::From(10, Some(30)), "c2", usize::MAX).is_none());
        r.note_view(&[result("c1", "x"), result("c2", "y")]);
        let note = r.after_lines(&p, "a.txt", Span::From(5, Some(20)), "c3", usize::MAX).unwrap();
        assert!(note.contains("lines 5-20") && note.contains("1-12, 10-30"), "{note}");
        // the last lines, already seen
        r.note_view(&[result("c1", "x"), result("c2", "y")]);
        assert!(r.after_lines(&p, "a.txt", Span::Last(5), "c4", usize::MAX).is_none(), "one note, then the lines");
        r.note_view(&[result("c1", "x"), result("c2", "y"), result("c4", "z")]);
        assert!(r.after_lines(&p, "a.txt", Span::Last(5), "c5", usize::MAX).is_some());
        // one of the two parts out of view: the lines come in full
        r.note_view(&[result("c2", "y")]);
        assert!(r.after_lines(&p, "a.txt", Span::From(5, Some(20)), "c6", usize::MAX).is_none());
    }

    #[test]
    fn an_output_cut_short_does_not_count_as_holding_the_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        let body: String = (1..=300).map(|i| format!("line {i} {}\n", "x".repeat(100))).collect();
        std::fs::write(&p, &body).unwrap();
        let mut r = Reads::default();
        // the tool kept the head and the tail of the output only
        assert!(r.after_lines(&p, "a.log", Span::From(1, None), "c1", 3_000).is_none());
        r.note_view(&[result("c1", "head … [characters omitted] … tail")]);
        assert!(r.after_lines(&p, "a.log", Span::From(150, Some(160)), "c2", 2_000).is_none(), "lines never sent");
        // a whole output does
        r.note_view(&[result("c2", "lines 150-160")]);
        assert!(r.after_lines(&p, "a.log", Span::From(152, Some(155)), "c3", 500).is_some());
    }

    #[test]
    fn numbered_lines_do_not_hide_a_cut() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.rs");
        let body: String = (1..=900).map(|i| format!("let value_{i:04} = compute({i});\n")).collect();
        std::fs::write(&p, &body).unwrap();
        // `cat -n`: about 7 more characters a line, capped in the middle by the shell tool
        let numbered: String = body.lines().enumerate().map(|(i, l)| format!("{:>6}\t{l}\n", i + 1)).collect();
        let n = numbered.chars().count();
        assert!(n > 31_000 && body.chars().count() < 30_000, "{n}");
        let head: String = numbered.chars().take(15_000).collect();
        let tail: String = numbered.chars().skip(n - 15_000).collect();
        let capped = format!("{head}\n… [{} characters omitted] …\n\n{tail}", n - 30_000);
        let mut r = Reads::default();
        let out = ToolOutput::ok(capped);
        assert!(r.after_lines(&p, "a.rs", Span::From(1, None), "c1", printed(&out)).is_none());
        r.note_view(&[result("c1", "numbered lines")]);
        assert!(r.after_lines(&p, "a.rs", Span::From(400, Some(450)), "c2", 2_000).is_none(), "lines never sent");
        // a `read` whose long line was cut: its numbers do not make up for it
        let q = d.path().join("b.txt");
        std::fs::write(&q, format!("short\n{}\n", "y".repeat(2_005))).unwrap();
        let shown = format!("     1│ short\n     2│ {}…\n", "y".repeat(1_999));
        let text = format!("short\n{}…\n", "y".repeat(1_999));
        let out = ToolOutput::ok(shown).with_display(ToolDisplay::File { path: "b.txt".into(), text, start_line: 1 });
        assert!(r.after_read(&q, "b.txt", None, 2000, "c3", printed(&out)).is_none());
        r.note_view(&[result("c3", "the read")]);
        assert!(r.after_read(&q, "b.txt", None, 2000, "c4", 10_000).is_none(), "the long line was never sent whole");
    }

    #[test]
    fn shell_commands_that_only_print_a_file_are_read() {
        let p = |s: &str| shell_print(s);
        assert_eq!(
            p("cd material; sed -n 60,150p assembly-A.txt"),
            Some(("material/assembly-A.txt".into(), Span::From(60, Some(150))))
        );
        assert_eq!(p("sed -n '7p' a.rs"), Some(("a.rs".into(), Span::From(7, Some(7)))));
        assert_eq!(p("cat -n src/app.ts"), Some(("src/app.ts".into(), Span::From(1, None))));
        assert_eq!(p("cd data && head -n 50 sales.csv"), Some(("data/sales.csv".into(), Span::From(1, Some(50)))));
        assert_eq!(p("head -20 x.log"), Some(("x.log".into(), Span::From(1, Some(20)))));
        assert_eq!(p("tail -n +40 x.log"), Some(("x.log".into(), Span::From(40, None))));
        assert_eq!(p("tail -5 x.log"), Some(("x.log".into(), Span::Last(5))));
        // anything more than printing: not a read
        for cmd in [
            "cat a.txt | grep x",
            "sed -n 1,5p a.txt > b.txt",
            "cat *.txt",
            "cat a.txt b.txt",
            "sed -i 1,5d a.txt",
            "rm a.txt; cat b.txt",
            "cat $HOME/a",
            "sleep 1 & cat a.txt",
            "sed -n '/x/p' a.txt",
            "cat 'my file.txt'",
        ] {
            assert_eq!(p(cmd), None, "{cmd}");
        }
    }

    /// Two files in a temp dir: `a.txt` with five lines, `b.txt` with four.
    fn two_files() -> (tempfile::TempDir, String, String) {
        let d = tempfile::tempdir().unwrap();
        let a: String = (1..=5).map(|i| format!("alpha {i}\n")).collect();
        let b: String = (1..=4).map(|i| format!("beta {i}\n")).collect();
        std::fs::write(d.path().join("a.txt"), &a).unwrap();
        std::fs::write(d.path().join("b.txt"), &b).unwrap();
        (d, a, b)
    }

    #[test]
    fn two_repeated_prints_chained_with_an_echo_each_get_a_note_and_the_echo_stays() {
        let (d, a, b) = two_files();
        let cmd = "cat a.txt; echo ----; cat b.txt";
        let out = format!("{a}----\n{b}");
        let mut r = Reads::default();
        assert!(r.after_chain(d.path(), cmd, &out, "c1").is_none(), "first time: nothing is repeated");
        r.note_view(&[result("c1", &out)]);
        let sent = r.after_chain(d.path(), cmd, &out, "c2").unwrap();
        assert_eq!(sent.matches("not repeated").count(), 2, "{sent}");
        assert!(sent.contains("a.txt: unchanged since shown above (5 lines)"), "{sent}");
        assert!(sent.contains("b.txt: unchanged since shown above (4 lines)"), "{sent}");
        assert!(sent.contains("\n----\n") && !sent.contains("alpha") && !sent.contains("beta"), "{sent}");
    }

    #[test]
    fn a_print_chained_with_a_grep_gets_a_note_and_the_grep_output_is_untouched() {
        let (d, a, _) = two_files();
        let cmd = "sed -n 2,4p a.txt && grep -n beta b.txt";
        let out = "alpha 2\nalpha 3\nalpha 4\n1:beta 1\n2:beta 2\n[exit code 0]";
        let mut r = Reads::default();
        assert!(r.after_lines(&d.path().join("a.txt"), "a.txt", Span::From(1, None), "c1", a.len()).is_none());
        r.note_view(&[result("c1", &a)]);
        let sent = r.after_chain(d.path(), cmd, out, "c2").unwrap();
        assert!(sent.starts_with("a.txt: unchanged since shown above (3 lines)"), "{sent}");
        assert!(sent.ends_with("\n1:beta 1\n2:beta 2\n[exit code 0]"), "{sent}");
        // a grep that found nothing ends the command with an error; the print is still what it is
        let failed = "alpha 2\nalpha 3\nalpha 4\n[exit code 1]";
        let mut r = Reads::default();
        assert!(r.after_lines(&d.path().join("a.txt"), "a.txt", Span::From(1, None), "c1", a.len()).is_none());
        r.note_view(&[result("c1", &a)]);
        let cmd = "sed -n 2,4p a.txt; grep nope b.txt";
        let sent = r.after_chain(d.path(), cmd, failed, "c3").unwrap();
        assert!(sent.contains("not repeated") && sent.ends_with("\n[exit code 1]"), "{sent}");
    }

    #[test]
    fn a_print_after_a_sed_in_place_of_the_same_file_in_the_command_gets_no_note() {
        let (d, a, _) = two_files();
        let p = d.path().join("a.txt");
        let mut r = Reads::default();
        assert!(r.after_lines(&p, "a.txt", Span::From(1, None), "c1", a.len()).is_none());
        r.note_view(&[result("c1", &a)]);
        // what `sed -i s/alpha/gamma/ a.txt; cat a.txt` leaves behind
        let after = a.replace("alpha", "gamma");
        std::fs::write(&p, &after).unwrap();
        assert!(r.after_chain(d.path(), "sed -i s/alpha/gamma/ a.txt; cat a.txt", &after, "c2").is_none());
        // a print before the change shows what the file held then, which is not what it holds now
        let now = after.replace("gamma", "delta");
        std::fs::write(&p, &now).unwrap();
        assert!(r.after_chain(d.path(), "cat a.txt; sed -i s/gamma/delta/ a.txt", &after, "c3").is_none());
    }

    #[test]
    fn a_semicolon_inside_quotes_is_not_a_split_point() {
        let one = |cmd: &str| shell_chain(cmd).unwrap();
        assert_eq!(
            one("echo 'x; cat a.txt'; cat b.txt"),
            vec![Print { path: "b.txt".into(), span: Span::From(1, None), numbered: false }]
        );
        assert_eq!(one("echo \"cat a.txt && cat c.txt\" ; sed -n 3,4p b.txt").len(), 1);
        // the quoted text alone holds no print
        assert!(shell_chain("echo 'cat a.txt; cat b.txt'").is_none());
        // `cd` carries to the prints after it, and only to those
        assert_eq!(
            one("cat top.txt; cd sub && head -n 3 x.log"),
            vec![
                Print { path: "top.txt".into(), span: Span::From(1, None), numbered: false },
                Print { path: "sub/x.log".into(), span: Span::From(1, Some(3)), numbered: false },
            ]
        );
    }

    #[test]
    fn what_the_scan_cannot_follow_is_left_as_it_is_today() {
        for cmd in [
            "cat a.txt; echo $(date)",
            "cat a.txt; echo `date`",
            "cat a.txt; cat <<EOF\nx\nEOF",
            "cat a.txt; cat <<EOF",
            "cat a.txt\ncat b.txt",
            "cat a.txt || echo none",
            "cat a.txt; (cd x; ls)",
            "cat a.txt; { ls; }",
            "sleep 1 & cat a.txt",
            "cat a.txt; echo \"$(date)\"",
            "cat a.txt; echo 'open",
        ] {
            assert!(shell_chain(cmd).is_none(), "{cmd}");
        }
        let (d, a, b) = two_files();
        let cmd = "cat a.txt; echo $(date); cat b.txt";
        let out = format!("{a}now\n{b}");
        let mut r = Reads::default();
        assert!(r.after_chain(d.path(), cmd, &out, "c1").is_none());
        r.note_view(&[result("c1", &out)]);
        assert!(r.after_chain(d.path(), cmd, &out, "c2").is_none());
    }

    #[test]
    fn one_repeated_and_one_new_print_give_one_note_and_the_new_lines_are_remembered() {
        let (d, a, b) = two_files();
        let mut r = Reads::default();
        let first = format!("{a}[exit code 0]");
        assert!(r.after_chain(d.path(), "cat a.txt; true", &first, "c1").is_none());
        r.note_view(&[result("c1", &first)]);
        let cmd = "cat a.txt; echo ----; cat b.txt";
        let out = format!("{a}----\n{b}");
        let sent = r.after_chain(d.path(), cmd, &out, "c2").unwrap();
        assert_eq!(sent.matches("not repeated").count(), 1, "{sent}");
        assert!(sent.contains("a.txt: unchanged") && sent.ends_with(&format!("----\n{b}")), "{sent}");
        // the lines of b.txt went to the model, so a later single print of them gets the note
        r.note_view(&[result("c1", &first), result("c2", &sent)]);
        let note = r.after_lines(&d.path().join("b.txt"), "b.txt", Span::From(2, Some(3)), "c3", 100).unwrap();
        assert!(note.contains("b.txt: unchanged since shown above (2 lines)"), "{note}");
    }

    #[test]
    fn a_numbered_print_inside_a_chain_is_matched_with_its_numbers() {
        let (d, a, _) = two_files();
        let numbered: String = a.lines().enumerate().map(|(i, l)| format!("{:>6}\t{l}\n", i + 1)).collect();
        let cmd = "cat -n a.txt; git status --short";
        let out = format!("{numbered}?? new.rs\n");
        let mut r = Reads::default();
        assert!(r.after_chain(d.path(), cmd, &out, "c1").is_none());
        r.note_view(&[result("c1", &out)]);
        let sent = r.after_chain(d.path(), cmd, &out, "c2").unwrap();
        assert!(sent.starts_with("a.txt: unchanged since shown above (5 lines)"), "{sent}");
        assert!(sent.ends_with("\n?? new.rs\n") && !sent.contains("alpha"), "{sent}");
        // the same lines without numbers are not what `cat -n` printed
        assert!(r.after_chain(d.path(), cmd, &format!("{a}?? new.rs\n"), "c3").is_none());
    }

    #[test]
    fn a_print_cut_short_or_followed_by_other_text_on_its_last_line_is_left_as_it_is() {
        let (d, a, _) = two_files();
        let cmd = "cat a.txt; echo end";
        let mut r = Reads::default();
        let out = format!("{a}end\n");
        assert!(r.after_chain(d.path(), cmd, &out, "c1").is_none());
        r.note_view(&[result("c1", &out)]);
        // the output lost its middle: no piece of it is found whole
        let cut = "alpha 1\nalpha 2\n… [30 characters omitted] …\n\nalpha 5\nend\n";
        assert!(r.after_chain(d.path(), cmd, cut, "c2").is_none());
        // a file with no final newline, whose last line runs into the next output
        std::fs::write(d.path().join("c.txt"), "x\ny").unwrap();
        assert!(r.after_chain(d.path(), "cat c.txt; echo end", "x\nyend\n", "c3").is_none());
    }
}
