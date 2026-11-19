//! The files the model has seen in this session, as they were when it saw them. `write` replaces
//! a whole file, so it does not replace one the model has not read in full, or one that changed
//! since (the person's editor, a formatter, a command): what is there would be lost unseen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Size and modification time: what tells a file changed.
type Stamp = (u64, Option<SystemTime>);

/// How many characters of a line `read` shows (`nexo-tools` `read::LINE_CHARS`); the text of a cut line says so.
const LINE_CHARS: usize = 2000;

/// How many line numbers a message lists before it says how many more there are.
const LISTED: usize = 5;

/// What of a file the model has seen under the stored stamp.
#[derive(Clone)]
enum Coverage {
    /// All of it, or all of it that `read` can show.
    Whole,
    /// Some lines. `spans` are the 1-based line ranges shown (sorted, merged), `cut` the lines shown only up to
    /// `LINE_CHARS` characters, `total` the line count of the file under the stored stamp. `before_change` is set
    /// when the model's own edit moved the line numbers and no read has been made since.
    Part { spans: Vec<(usize, usize)>, total: usize, cut: Vec<usize>, before_change: bool },
}

#[derive(Clone)]
struct Entry {
    stamp: Stamp,
    coverage: Coverage,
}

/// The files of a session. Cloning shares them.
#[derive(Clone, Default)]
pub struct Seen(Arc<Mutex<HashMap<PathBuf, Entry>>>);

fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    m.is_file().then(|| (m.len(), m.modified().ok()))
}

/// `a-b`, or `a` for one line.
fn range(a: usize, b: usize) -> String {
    if a == b {
        a.to_string()
    } else {
        format!("{a}-{b}")
    }
}

/// `a, b and c`; past `LISTED` items the rest is counted: `a, b, c, d, e and 7 more`.
fn list(items: &[String]) -> String {
    if items.len() > LISTED {
        return format!("{} and {} more", items[..LISTED].join(", "), items.len() - LISTED);
    }
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The lines in `1..=total` that `spans` (sorted, merged) leave out, as ranges.
fn gaps(spans: &[(usize, usize)], total: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut next = 1;
    for &(a, b) in spans {
        if a > next {
            out.push((next, a - 1));
        }
        next = next.max(b + 1);
    }
    if next <= total {
        out.push((next, total));
    }
    out
}

impl Seen {
    /// The model now knows `path` as it is: it wrote or edited it. A file it saw only in part keeps no line
    /// numbers: a change can move them, so only reads made after it count (see `lines`).
    pub fn mark(&self, path: &Path) {
        let Some(stamp) = stamp(path) else { return };
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let coverage = match m.get(&key(path)) {
            Some(Entry { coverage: Coverage::Part { .. }, .. }) => {
                Coverage::Part { spans: Vec::new(), total: 0, cut: Vec::new(), before_change: true }
            }
            _ => Coverage::Whole,
        };
        m.insert(key(path), Entry { stamp, coverage });
    }

    /// The model was shown all of `path`, or all of it that `read` can show (a picture, a binary, a file too
    /// large to load), or wrote it whole.
    pub fn whole(&self, path: &Path) {
        if let Some(stamp) = stamp(path) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key(path), Entry { stamp, coverage: Coverage::Whole });
        }
    }

    /// The model was shown lines `first..=last` of `path`, which has `total` lines; `cut` are the numbers of the
    /// shown lines that were cut at `LINE_CHARS` characters. Under the same stamp the lines join what was shown
    /// before; under another stamp (or for a file not seen) they replace it. The file counts as whole only when the
    /// lines shown cover all of it and none was cut: `read` cannot show more of a long line.
    pub fn lines(&self, path: &Path, first: usize, last: usize, total: usize, cut: &[usize]) {
        let Some(stamp) = stamp(path) else { return };
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (mut spans, mut cuts) = match m.get(&key(path)) {
            Some(Entry { stamp: s, coverage: Coverage::Whole }) if *s == stamp => return,
            Some(Entry { stamp: s, coverage: Coverage::Part { spans, cut, .. } }) if *s == stamp => {
                (spans.clone(), cut.clone())
            }
            _ => (Vec::new(), Vec::new()),
        };
        spans.push((first, last));
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (a, b) in spans {
            match merged.last_mut() {
                Some(l) if a <= l.1 + 1 => l.1 = l.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        cuts.extend_from_slice(cut);
        cuts.sort_unstable();
        cuts.dedup();
        let coverage = if cuts.is_empty() && gaps(&merged, total).is_empty() {
            Coverage::Whole
        } else {
            Coverage::Part { spans: merged, total, cut: cuts, before_change: false }
        };
        m.insert(key(path), Entry { stamp, coverage });
    }

    /// Whether the model saw `path` and it is not as it was then; false for a file it never saw.
    pub fn changed(&self, path: &Path) -> bool {
        let Some(now) = stamp(path) else { return false };
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)).is_some_and(|e| e.stamp != now)
    }

    /// The numbers of the lines `read` cut, when they are all that the model has not seen of `path`: every other
    /// line was shown and the file is as it was then. `read` shows no more of such a line, so reading again does
    /// not help. None in every other case.
    pub fn cut_only(&self, path: &Path) -> Option<Vec<usize>> {
        let now = stamp(path)?;
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)) {
            Some(Entry { stamp: s, coverage: Coverage::Part { spans, total, cut, before_change: false } })
                if *s == now && !cut.is_empty() && gaps(spans, *total).is_empty() =>
            {
                Some(cut.clone())
            }
            _ => None,
        }
    }

    /// Why replacing `path` would lose content the model has not seen; None when nothing would be
    /// lost (no such file, an empty one, or one unchanged since the model saw all of it).
    pub fn unseen(&self, path: &Path) -> Option<String> {
        let now = stamp(path)?;
        if now.0 == 0 {
            return None;
        }
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)) {
            None => Some("you have not read it in this session".into()),
            Some(e) if e.stamp != now => Some("it changed since you last read or wrote it".into()),
            Some(Entry { coverage: Coverage::Whole, .. }) => None,
            Some(Entry { coverage: Coverage::Part { before_change: true, .. }, .. }) => Some(
                "you were shown only part of it, before your last change to it; `read` shows it as it is now".into(),
            ),
            Some(Entry { coverage: Coverage::Part { spans, total, cut, .. }, .. }) => {
                let missing = gaps(spans, *total);
                let mut parts = Vec::new();
                if let Some(&(first, _)) = missing.first() {
                    let shown: Vec<String> = spans.iter().map(|&(a, b)| range(a, b)).collect();
                    let not_shown: Vec<String> = missing.iter().map(|&(a, b)| range(a, b)).collect();
                    parts.push(format!(
                        "you were shown only lines {} of its {total} lines ({} not shown; `read` with offset {first} shows them)",
                        list(&shown),
                        list(&not_shown)
                    ));
                }
                if !cut.is_empty() {
                    let numbers: Vec<String> = cut.iter().map(usize::to_string).collect();
                    let what = if cut.len() == 1 {
                        format!(
                            "line {} is longer than {LINE_CHARS} chars and was shown only up to char {LINE_CHARS}",
                            numbers[0]
                        )
                    } else {
                        format!(
                            "lines {} are longer than {LINE_CHARS} chars and were shown only up to char {LINE_CHARS}",
                            list(&numbers)
                        )
                    };
                    parts.push(if missing.is_empty() {
                        format!("you were shown all its {total} lines, but {what}")
                    } else {
                        what
                    });
                }
                Some(parts.join("; "))
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_safe_to_replace_only_as_it_was_seen() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.txt");
        let seen = Seen::default();
        assert_eq!(seen.unseen(&f), None, "a new file loses nothing");
        std::fs::write(&f, "").unwrap();
        assert_eq!(seen.unseen(&f), None, "an empty file loses nothing");
        std::fs::write(&f, "kept").unwrap();
        assert!(seen.unseen(&f).unwrap().contains("not read"));
        assert!(!seen.changed(&f), "a file never seen is not a changed one");
        seen.clone().mark(&f);
        assert_eq!(seen.unseen(&f), None);
        std::fs::write(&f, "kept, and more").unwrap();
        assert!(seen.unseen(&f).unwrap().contains("changed"));
        assert!(seen.changed(&f));
        // the same file by another spelling
        seen.mark(&f);
        assert_eq!(seen.unseen(&d.path().join(".").join("a.txt")), None);
    }

    #[test]
    fn a_file_read_in_parts_is_whole_only_when_every_line_was_shown() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "some text\n").unwrap();
        let seen = Seen::default();
        seen.lines(&p, 1, 700, 2593, &[]);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("lines 1-700") && why.contains("of its 2593 lines"), "{why}");
        assert!(why.contains("701-2593 not shown") && why.contains("offset 701"), "{why}");
        // the lines shown in two reads join: every line was shown, so nothing is lost
        seen.lines(&p, 701, 2593, 2593, &[]);
        assert_eq!(seen.unseen(&p), None);
        // another stamp: the old spans are gone
        std::fs::write(&p, "some other text, longer\n").unwrap();
        seen.lines(&p, 1, 10, 2593, &[]);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("lines 1-10 of its 2593 lines") && !why.contains("700"), "{why}");
        // a cut line keeps the file partial even when every line was shown
        let seen = Seen::default();
        seen.lines(&p, 1, 40, 40, &[5]);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("you were shown all its 40 lines, but line 5 is longer than 2000 chars"), "{why}");
        seen.lines(&p, 1, 40, 40, &[5, 9, 12, 13, 14, 15, 16]);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("lines 5, 9, 12, 13, 14 and 2 more are longer than 2000 chars"), "{why}");
    }

    #[test]
    fn a_change_drops_the_line_numbers_of_a_part_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.txt");
        std::fs::write(&p, "a long file\n".repeat(30)).unwrap();
        let seen = Seen::default();
        seen.lines(&p, 1, 2000, 3000, &[]);
        std::fs::write(&p, "a shorter file\n").unwrap();
        seen.mark(&p);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("before your last change"), "{why}");
        seen.lines(&p, 1, 10, 2000, &[]);
        let why = seen.unseen(&p).unwrap();
        assert!(why.contains("lines 1-10 of its 2000 lines"), "{why}");
        // a file seen whole stays whole through the model's own change
        let w = d.path().join("w.txt");
        std::fs::write(&w, "x\n").unwrap();
        seen.whole(&w);
        std::fs::write(&w, "xy\n").unwrap();
        seen.mark(&w);
        assert_eq!(seen.unseen(&w), None);
    }
}
