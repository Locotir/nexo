//! A unified diff of two texts, coloured.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use similar::{ChangeTag, DiffTag, TextDiff};

use crate::theme::Theme;

use super::code::detab;
use super::wrap::truncate;

/// Unified diff lines (three lines of context) between `old` and `new`. `start_line` is the
/// line of the file where both texts begin (0 counts as 1): the numbers carry on from it.
pub fn unified(old: &str, new: &str, start_line: usize, theme: &Theme) -> Vec<Line<'static>> {
    unified_rows(old, new, start_line, theme).0
}

/// [`unified`] and, for each row, the row's sign and text as written (tabs kept; empty on a header).
pub fn unified_rows(old: &str, new: &str, start_line: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<String>) {
    let mut srcs: Vec<String> = Vec::new();
    let skip = start_line.saturating_sub(1);
    let diff = TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for (i, group) in diff.grouped_ops(3).iter().enumerate() {
        if i > 0 {
            out.push(Line::from(Span::styled("  ⋯", theme.dim())));
            srcs.push(String::new());
        }
        let (mut a, mut b) = (0usize, 0usize);
        if let Some(first) = group.first() {
            a = first.old_range().start + 1 + skip;
            b = first.new_range().start + 1 + skip;
        }
        let (la, lb): (usize, usize) =
            (group.iter().map(|op| op.old_range().len()).sum(), group.iter().map(|op| op.new_range().len()).sum());
        out.push(Line::from(Span::styled(format!("@@ -{a},{la} +{b},{lb} @@"), theme.style(theme.hunk))));
        srcs.push(String::new());
        // one gutter: the line's number in the new text, or in the old one for a removed line
        let digits = (a + la).max(b + lb).max(1).to_string().len();
        for op in group {
            for change in diff.iter_changes(op) {
                let text = detab(change.value().trim_end_matches('\n'));
                let raw = change.value().trim_end_matches('\n');
                let (sign, style) = match change.tag() {
                    ChangeTag::Delete => ("-", theme.diff_del_line()),
                    ChangeTag::Insert => ("+", theme.diff_add_line()),
                    ChangeTag::Equal => (" ", theme.dim()),
                };
                srcs.push(format!("{sign} {raw}"));
                let n = change.new_index().or(change.old_index()).map_or(0, |i| i + 1 + skip);
                out.push(Line::from(vec![
                    Span::styled(format!("{n:>digits$} "), theme.dim()),
                    Span::styled(format!("{sign} {text}"), style),
                ]));
            }
        }
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled("(no change)", theme.dim())));
        srcs.push(String::new());
    }
    (out, srcs)
}

/// The old text on the left and the new on the right, `width` columns in all, each side
/// with its own line numbers (from `start_line`, as in `unified`); changed lines face each other.
pub fn side_by_side(old: &str, new: &str, start_line: usize, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let skip = start_line.saturating_sub(1);
    let diff = TextDiff::from_lines(old, new);
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let digits = (a.len().max(b.len()) + skip).max(1).to_string().len();
    let col = width.saturating_sub(3) / 2;
    let text_w = col.saturating_sub(digits + 1);
    let side = |n: Option<usize>, lines: &[&str], style: Style| -> Vec<Span<'static>> {
        match n {
            Some(i) => {
                let t = truncate(&detab(lines.get(i).copied().unwrap_or("")), text_w);
                let pad = text_w.saturating_sub(super::wrap::width(&t));
                vec![
                    Span::styled(format!("{:>digits$} ", i + 1 + skip), theme.dim()),
                    Span::styled(format!("{t}{}", " ".repeat(pad)), style),
                ]
            }
            None => vec![Span::raw(" ".repeat(col))],
        }
    };
    let mut out = Vec::new();
    for (gi, group) in diff.grouped_ops(3).iter().enumerate() {
        if gi > 0 {
            out.push(Line::from(Span::styled("  ⋯", theme.dim())));
        }
        for op in group {
            let (or, nr) = (op.old_range(), op.new_range());
            let changed = !matches!(op.tag(), DiffTag::Equal);
            for k in 0..or.len().max(nr.len()) {
                let l = (k < or.len()).then(|| or.start + k);
                let r = (k < nr.len()).then(|| nr.start + k);
                let (ls, rs) =
                    if changed { (theme.diff_del_line(), theme.diff_add_line()) } else { (theme.dim(), theme.dim()) };
                let mut spans = side(l, &a, ls);
                spans.push(Span::styled(" │ ", theme.dim()));
                spans.extend(side(r, &b, rs));
                out.push(Line::from(spans));
            }
        }
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled("(no change)", theme.dim())));
    }
    out
}

/// Added and removed line counts.
pub fn stats(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let mut add = 0;
    let mut del = 0;
    for c in diff.iter_all_changes() {
        match c.tag() {
            ChangeTag::Insert => add += 1,
            ChangeTag::Delete => del += 1,
            ChangeTag::Equal => {}
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_added_and_removed_lines() {
        let t = Theme::dark();
        let lines = unified("a\nb\nc\n", "a\nB\nc\nd\n", 1, &t);
        let texts: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert!(texts[0].starts_with("@@"));
        assert!(texts.contains(&"2 - b".to_string()), "{texts:?}");
        assert!(texts.contains(&"2 + B".to_string()));
        assert!(texts.contains(&"4 + d".to_string()));
        assert_eq!(stats("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
    }

    #[test]
    fn numbers_carry_on_from_the_line_the_text_starts_at() {
        let t = Theme::dark();
        let texts = |lines: Vec<Line<'static>>| -> Vec<String> {
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect()
        };
        let u = texts(unified("a\nb\n", "a\nB\n", 40, &t));
        assert_eq!(u[0], "@@ -40,2 +40,2 @@");
        assert!(u.contains(&"40   a".to_string()), "{u:?}");
        assert!(u.contains(&"41 - b".to_string()), "{u:?}");
        assert!(u.contains(&"41 + B".to_string()), "{u:?}");
        // 0, what an older session has, reads as the first line
        assert_eq!(texts(unified("a\n", "b\n", 0, &t)), texts(unified("a\n", "b\n", 1, &t)));
        let s = texts(side_by_side("a\nb\n", "a\nB\n", 40, 40, &t));
        assert!(s.iter().any(|l| l.starts_with("41 b") && l.contains("│ 41 B")), "{s:?}");
    }

    #[test]
    fn side_by_side_faces_old_and_new_lines_within_the_width() {
        let t = Theme::dark();
        let lines = side_by_side("a\nb\nc\n", "a\nB\nc\nd\n", 1, 30, &t);
        let texts: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.to_string()).collect()).collect();
        assert!(texts.iter().all(|l| super::super::wrap::width(l) <= 30), "{texts:?}");
        assert!(texts.iter().any(|l| l.starts_with("2 b") && l.contains("│ 2 B")), "{texts:?}");
        assert!(texts.iter().any(|l| l.trim_start().starts_with("│ 4 d")), "{texts:?}");
    }
}
