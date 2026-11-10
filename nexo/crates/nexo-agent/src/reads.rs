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
/// The end of the line that heads a file attached whole.
pub(crate) const ATTACHED: &str = "(attached in full, as it is now):";

/// The line that heads a file attached whole to a request.
pub fn attached_header(shown: &str) -> String {
    format!("@{shown} {ATTACHED}")
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
}

fn digest(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

impl Reads {
    pub fn clear(&mut self) {
        self.seen.clear();
        self.in_view.clear();
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
                        for line in text.lines().filter(|l| l.starts_with('@') && l.ends_with(ATTACHED)) {
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
        let Some(s) = self.seen.get(path) else { return false };
        std::fs::read(path).is_ok_and(|b| digest(&b) == s.hash)
    }

    /// A file attached whole to a request, under `header`.
    pub fn attached(&mut self, path: PathBuf, bytes: &[u8], header: String) {
        let end = String::from_utf8_lossy(bytes).lines().count();
        let pieces = vec![Piece { start: 1, end, anchor: header }];
        self.seen.insert(path, Seen { hash: digest(bytes), pieces, noted: false });
    }

    /// After `edit` or `write` changed `path`: what the model knew of it is gone, and when the result of
    /// call `call_id` holds the file whole (a short one) the model has all of it.
    pub fn after_change(&mut self, path: &Path, call_id: &str) {
        let Ok(bytes) = std::fs::read(path) else { return };
        let text = String::from_utf8_lossy(&bytes);
        let pieces = match nexo_tools::edit::whole(&text) {
            Some(_) => vec![Piece { start: 1, end: text.lines().count(), anchor: call_id.to_string() }],
            None => Vec::new(),
        };
        self.seen.insert(path.to_path_buf(), Seen { hash: digest(&bytes), pieces, noted: false });
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
        let bytes = std::fs::read(path).ok()?;
        let hash = digest(&bytes);
        let text = String::from_utf8_lossy(&bytes);
        let total = text.lines().count();
        let (start, end) = match span {
            Span::From(a, b) => (a.max(1), b.unwrap_or(total).min(total)),
            Span::Last(n) => (total.saturating_sub(n) + 1, total),
        };
        if start > end {
            return None;
        }
        let in_view = &self.in_view;
        let seen = self.seen.entry(path.to_path_buf()).or_insert(Seen { hash, pieces: Vec::new(), noted: false });
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
        if at > end && !seen.noted {
            let whence = if used.iter().any(|p| p.anchor.starts_with('@')) {
                "it was attached in full to a message above".to_string()
            } else {
                let parts: Vec<String> = used.iter().map(|p| format!("{}-{}", p.start, p.end)).collect();
                format!("lines {start}-{end} are inside your earlier output of lines {} above", parts.join(", "))
            };
            seen.noted = true;
            return Some(format!(
                "{shown}: unchanged since shown above ({}); not repeated: {whence}. \
                 If that is no longer in the conversation, ask again and you get it in full.",
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

/// The file and the lines a shell command prints, when printing them is all it does: `cat [-n] F`,
/// `sed -n 'A,Bp' F` (or `Ap`), `head [-n N | -N] F`, `tail [-n N | -n +N | -N] F`, after
/// `cd D;` or `cd D &&` if any. Pipes, redirections, globs, expansions and other commands: None.
pub fn shell_print(cmd: &str) -> Option<(PathBuf, Span)> {
    if cmd.contains(['|', '>', '<', '$', '`', '*', '?', '(', ')', '{', '}', '\n', '\\']) {
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
    let count = |s: &str| s.parse::<usize>().ok();
    let (file, span) = match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["cat", f] | ["cat", "-n", f] => (*f, Span::From(1, None)),
        ["sed", "-n", script, f] => {
            let range = script.strip_suffix('p')?;
            let (a, b) = match range.split_once(',') {
                Some((a, b)) => (count(a)?, count(b)?),
                None => (count(range)?, count(range)?),
            };
            (*f, Span::From(a, Some(b)))
        }
        ["head", f] => (*f, Span::From(1, Some(10))),
        ["head", "-n", n, f] => (*f, Span::From(1, Some(count(n)?))),
        ["head", n, f] if n.starts_with('-') => (*f, Span::From(1, Some(count(&n[1..])?))),
        ["tail", f] => (*f, Span::Last(10)),
        ["tail", "-n", n, f] => match n.strip_prefix('+') {
            Some(a) => (*f, Span::From(count(a)?, None)),
            None => (*f, Span::Last(count(n)?)),
        },
        ["tail", n, f] if n.starts_with('-') => (*f, Span::Last(count(&n[1..])?)),
        _ => return None,
    };
    if file.starts_with('-') {
        return None;
    }
    Some((dir.join(file), span))
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
}
