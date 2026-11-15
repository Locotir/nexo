use similar::{ChangeTag, TextDiff};

use super::{DIFF_LINES, WHOLE_LINES};
use crate::util::cut_line;

/// Lines added and removed, and a unified diff cut to `DIFF_LINES` lines.
pub fn unified(old: &str, new: &str, name: &str) -> (usize, usize, String) {
    let diff = TextDiff::from_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff.unified_diff().context_radius(3).header(&format!("a/{name}"), &format!("b/{name}")).to_string();
    let lines: Vec<&str> = text.lines().collect();
    let shown = if lines.len() > DIFF_LINES {
        format!("{}\n… {} more diff lines", lines[..DIFF_LINES].join("\n"), lines.len() - DIFF_LINES)
    } else {
        text
    };
    (added, removed, shown)
}

/// Lines of context shown on each side of a change.
pub(super) const CONTEXT: usize = 3;
/// Numbered lines shown at most for the changed regions of one result.
pub(super) const SNIPPET_LINES: usize = 40;
/// Characters kept of a line in the snippet.
pub(super) const SNIPPET_LINE_CHARS: usize = 200;

/// The whole of `text`, numbered as `read` numbers it, when it is short enough to be shown whole: at most
/// [`WHOLE_LINES`] lines, none cut. None otherwise. The agent uses it to know a file was shown whole.
pub fn whole(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() || lines.len() > WHOLE_LINES || lines.iter().any(|l| l.chars().count() > SNIPPET_LINE_CHARS) {
        return None;
    }
    Some(lines.iter().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + 1)).collect())
}

/// What a changed file shows of itself: all of it when short, else the changed regions with context.
/// Empty when nothing changed.
pub fn now_text(old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    whole(new).unwrap_or_else(|| around(old, new))
}

/// The file as it is now around what changed, numbered as `read` numbers it: each changed
/// region with `CONTEXT` lines either side, gaps and a cut at `SNIPPET_LINES` lines marked `…`.
/// Empty when nothing changed.
pub fn around(old: &str, new: &str) -> String {
    let lines: Vec<&str> = new.lines().collect();
    let diff = TextDiff::from_lines(old, new);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for op in diff.ops().iter().filter(|o| o.tag() != similar::DiffTag::Equal) {
        let r = op.new_range();
        let (a, b) = (r.start.saturating_sub(CONTEXT), (r.end + CONTEXT).min(lines.len()));
        match spans.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => spans.push((a, b)),
        }
    }
    let (mut out, mut shown) = (String::new(), 0);
    for (k, (a, b)) in spans.into_iter().enumerate() {
        if shown >= SNIPPET_LINES {
            out.push_str("…\n");
            break;
        }
        if k > 0 {
            out.push_str("…\n");
        }
        let cut = b.min(a + (SNIPPET_LINES - shown));
        for (i, l) in lines.iter().enumerate().take(cut).skip(a) {
            out.push_str(&format!("{:>6}│ {}\n", i + 1, cut_line(l, SNIPPET_LINE_CHARS)));
        }
        shown += cut - a;
        if cut < b {
            out.push_str("…\n");
            break;
        }
    }
    out
}
