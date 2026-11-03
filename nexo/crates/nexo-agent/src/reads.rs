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

/// What the curator puts where it left something out.
const OMITTED: &str = "[curator]";
/// The end of the line that heads a file attached whole.
const ATTACHED: &str = "(attached in full, as it is now):";

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

    /// A file attached whole to a request, under `header`.
    pub fn attached(&mut self, path: PathBuf, bytes: &[u8], header: String) {
        let end = String::from_utf8_lossy(bytes).lines().count();
        let pieces = vec![Piece { start: 1, end, anchor: header }];
        self.seen.insert(path, Seen { hash: digest(bytes), pieces, noted: false });
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
    ) -> Option<String> {
        let start = offset.unwrap_or(1).max(1) as usize;
        let end = start + limit.max(1) as usize - 1;
        self.after_lines(path, shown, Span::From(start, Some(end)), call_id)
    }

    /// After a call that printed `span` of `path`: the note the model gets instead of the
    /// output when it has those lines already.
    pub fn after_lines(&mut self, path: &Path, shown: &str, span: Span, call_id: &str) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        let hash = digest(&bytes);
        let total = String::from_utf8_lossy(&bytes).lines().count();
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
                "{shown}: unchanged since you last saw it, so the lines are not repeated: {whence}. \
                 If that is no longer in the conversation, ask again and you get it in full."
            ));
        }
        seen.noted = false;
        seen.pieces.retain(|p| !(p.start == start && p.end == end));
        seen.pieces.push(Piece { start, end, anchor: call_id.to_string() });
        None
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
        assert!(r.after_read(&p, "a.txt", None, 2000, "c1").is_none());
        r.note_view(&[result("c1", "     1│ one")]);
        let note = r.after_read(&p, "a.txt", Some(2), 1, "c2").unwrap();
        assert!(note.contains("not repeated"), "{note}");
        // asked again right after the note: the model wants the lines
        assert!(r.after_read(&p, "a.txt", None, 2000, "c3").is_none());
    }

    #[test]
    fn a_changed_file_or_one_out_of_view_is_read_in_full() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut r = Reads::default();
        r.after_read(&p, "a.txt", None, 2000, "c1");
        // the curator left the result out
        r.note_view(&[result("c1", "[curator] read output omitted as no longer needed")]);
        assert!(r.after_read(&p, "a.txt", None, 2000, "c2").is_none());
        r.note_view(&[result("c2", "     1│ one")]);
        std::fs::write(&p, "one\nTWO\n").unwrap();
        assert!(r.after_read(&p, "a.txt", None, 2000, "c3").is_none());
    }

    #[test]
    fn a_part_does_not_cover_more_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "1\n2\n3\n4\n").unwrap();
        let mut r = Reads::default();
        r.after_read(&p, "a.txt", Some(1), 2, "c1");
        r.note_view(&[result("c1", "x")]);
        assert!(r.after_read(&p, "a.txt", None, 2000, "c2").is_none());
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
        let note = r.after_read(&p, "a.txt", None, 2000, "c1").unwrap();
        assert!(note.contains("attached"), "{note}");
    }

    #[test]
    fn parts_read_in_several_calls_together_cover_a_range() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, (1..=30).map(|i| format!("{i}\n")).collect::<String>()).unwrap();
        let mut r = Reads::default();
        assert!(r.after_lines(&p, "a.txt", Span::From(1, Some(12)), "c1").is_none());
        assert!(r.after_lines(&p, "a.txt", Span::From(10, Some(30)), "c2").is_none());
        r.note_view(&[result("c1", "x"), result("c2", "y")]);
        let note = r.after_lines(&p, "a.txt", Span::From(5, Some(20)), "c3").unwrap();
        assert!(note.contains("lines 5-20") && note.contains("1-12, 10-30"), "{note}");
        // the last lines, already seen
        r.note_view(&[result("c1", "x"), result("c2", "y")]);
        assert!(r.after_lines(&p, "a.txt", Span::Last(5), "c4").is_none(), "one note, then the lines");
        r.note_view(&[result("c1", "x"), result("c2", "y"), result("c4", "z")]);
        assert!(r.after_lines(&p, "a.txt", Span::Last(5), "c5").is_some());
        // one of the two parts out of view: the lines come in full
        r.note_view(&[result("c2", "y")]);
        assert!(r.after_lines(&p, "a.txt", Span::From(5, Some(20)), "c6").is_none());
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
