//! Which compiled files the session's lines named, and how long ago the last one came.
//!
//! The first line comes on the first file read by hand. Another comes only for a file no line named yet, and only
//! after [`GAP`] rounds without one: by then the first line is far back in a long history (or summarised away),
//! and a new file has facts of its own. A session that keeps reading one program pays one line; one that meets a
//! program now and then pays at most one line per [`GAP`] rounds.

use nexo_core::message::{Message, Part};

use super::{MARK, START};

/// Rounds of tool results after a line before another may come.
pub const GAP: usize = 20;

/// The heads of the lines sessions saved before the facts came first: still found by [`told`].
pub(super) const OLD: [&str; 3] = [
    "[nexo: the bin tool reads this file in pages, with names and references:",
    "[nexo: nexo bin reads this file in pages, with names and references:",
    "[nexo: this file can be read in pages with names and references: nexo re lists the kit",
];

/// Which files the lines named, and the rounds since the last line (`None` before the first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Told {
    files: Vec<String>,
    since: Option<usize>,
}

impl Told {
    /// Whether the session had a line.
    #[cfg(test)]
    pub fn said(&self) -> bool {
        self.since.is_some()
    }

    /// One more round; whether a line may come in it.
    pub(super) fn tick(&mut self) -> bool {
        if let Some(n) = &mut self.since {
            *n += 1;
        }
        self.open()
    }

    /// Whether a line may come now.
    pub(super) fn open(&self) -> bool {
        self.since.is_none_or(|n| n >= GAP)
    }

    /// Whether a line may come now for `file`.
    pub(super) fn may(&self, file: &str) -> bool {
        self.open() && !self.files.iter().any(|f| f == file)
    }

    pub(super) fn add(&mut self, file: String) {
        self.files.push(file);
        self.since = Some(0);
    }
}

/// What the lines in the tool results of `messages` tell: the files they named and the rounds since the last.
pub fn told(messages: &[Message]) -> Told {
    let mut t = Told::default();
    for m in messages {
        let mut round = false;
        let mut lines = Vec::new();
        for p in &m.content {
            let Part::ToolResult { content, .. } = p else { continue };
            round = true;
            for text in content.iter().filter_map(Part::as_text) {
                lines.extend(text.lines().filter(|l| l.starts_with(START) && l.contains(MARK)).map(file_of));
                if OLD.iter().any(|h| text.contains(h)) {
                    lines.push(None);
                }
            }
        }
        if !lines.is_empty() {
            t.files.extend(lines.into_iter().flatten());
            t.since = Some(0);
        } else if round {
            t.since = t.since.map(|n| n + 1);
        }
    }
    t
}

/// The file a line names: what comes between its start and the `: ` before the facts.
fn file_of(line: &str) -> Option<String> {
    let rest = line.strip_prefix(START)?;
    let (file, _) = rest.split_once(": ")?;
    Some(file.to_string())
}
