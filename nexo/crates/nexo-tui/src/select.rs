//! Text selection over the transcript: a drag from an anchor to a head, in
//! absolute coordinates of the laid-out transcript (line, column).

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::render::wrap::width;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    /// The button is still down.
    pub dragging: bool,
}

impl Selection {
    pub fn new(anchor: Pos) -> Selection {
        Selection { anchor, head: anchor, dragging: true }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The two ends in order.
    pub fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn lines(&self) -> usize {
        let (s, e) = self.range();
        e.line - s.line + 1
    }

    /// The columns of `line` the selection covers, `from..to` (`to` is
    /// `usize::MAX` when the line is covered to its end).
    pub fn covers(&self, line: usize) -> Option<(usize, usize)> {
        let (s, e) = self.range();
        if line < s.line || line > e.line {
            return None;
        }
        let from = if line == s.line { s.col } else { 0 };
        let to = if line == e.line { e.col + 1 } else { usize::MAX };
        Some((from, to))
    }

    /// The selected text; `text(line)` gives the plain text of absolute line
    /// `line` (None past the end). Lines taken from their first column lose
    /// the transcript's rails (`▌ `, `◆ `, the two-column indent) and, when
    /// every line has one, the line-number gutter of a file view, so what is
    /// pasted is the content, not the decoration.
    pub fn extract(&self, text: impl Fn(usize) -> Option<String>) -> String {
        let (s, e) = self.range();
        let mut out = Vec::new();
        for line in s.line..=e.line {
            let t = text(line).unwrap_or_default();
            let (from, to) = self.covers(line).unwrap_or((0, 0));
            let piece = if from == 0 { strip_rail(&t) } else { t.clone() };
            // the rail took two columns: the requested columns move with it
            let shift = if from == 0 { width(&t) - width(&piece) } else { 0 };
            let to = if to == usize::MAX { to } else { to.saturating_sub(shift) };
            out.push(slice_cols(&piece, from, to).trim_end().to_string());
        }
        strip_gutter(out).join("\n")
    }
}

/// The glyphs the transcript puts in front of a block's first line.
const RAILS: &[char] = &['▌', '◆', '▸', '▾', '✓', '✗', '○', '⊘', '▲', '●', '◇'];

/// `line` without its two-column rail (a glyph and a space, or two spaces).
pub fn strip_rail(line: &str) -> String {
    let mut chars = line.chars();
    match (chars.next(), chars.next()) {
        (Some(' '), Some(' ')) => chars.collect(),
        (Some(g), Some(' ')) if RAILS.contains(&g) => chars.collect(),
        (Some(g), None) if RAILS.contains(&g) => String::new(),
        _ => line.to_string(),
    }
}

/// `lines` without their `  12│ ` gutters when every non-empty line has one.
pub fn strip_gutter(lines: Vec<String>) -> Vec<String> {
    fn gutter_end(l: &str) -> Option<usize> {
        let t = l.trim_start();
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || !t[digits..].starts_with('│') {
            return None;
        }
        let head = l.len() - t.len() + digits + '│'.len_utf8();
        Some(if l[head..].starts_with(' ') { head + 1 } else { head })
    }
    let all = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| gutter_end(l).is_some());
    if !all || lines.iter().all(|l| l.trim().is_empty()) {
        return lines;
    }
    lines.into_iter().map(|l| gutter_end(&l).map(|i| l[i..].to_string()).unwrap_or(l)).collect()
}

/// The graphemes of `s` whose columns fall in `from..to`.
pub fn slice_cols(s: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut col = 0;
    for g in s.graphemes(true) {
        let w = width(g);
        if col >= to {
            break;
        }
        if col + w > from {
            out.push_str(g);
        }
        col += w;
    }
    out
}

/// `line` with `style` patched onto the columns `from..to`; when the
/// selection runs past the text, the empty columns are drawn too.
pub fn style_cols(line: Line<'static>, from: usize, to: usize, style: Style) -> Line<'static> {
    let mut spans = Vec::new();
    let mut col = 0;
    for s in line.spans {
        let text = s.content.to_string();
        let w = width(&text);
        if col + w <= from || col >= to {
            spans.push(Span::styled(text, s.style));
            col += w;
            continue;
        }
        let (mut before, mut inside, mut after) = (String::new(), String::new(), String::new());
        let mut c = col;
        for g in text.graphemes(true) {
            let gw = width(g);
            if c + gw <= from {
                before.push_str(g);
            } else if c < to {
                inside.push_str(g);
            } else {
                after.push_str(g);
            }
            c += gw;
        }
        if !before.is_empty() {
            spans.push(Span::styled(before, s.style));
        }
        if !inside.is_empty() {
            spans.push(Span::styled(inside, s.style.patch(style)));
        }
        if !after.is_empty() {
            spans.push(Span::styled(after, s.style));
        }
        col += w;
    }
    if to != usize::MAX && col < to {
        let start = col.max(from);
        if start < to {
            spans.push(Span::styled(" ".repeat(to - start), style));
        }
    } else if to == usize::MAX && col <= from {
        spans.push(Span::styled(" ", style));
    }
    Line::from(spans).style(line.style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn orders_the_ends_and_covers_the_lines_between() {
        let mut s = Selection::new(Pos { line: 5, col: 4 });
        s.head = Pos { line: 3, col: 2 };
        assert_eq!(s.range(), (Pos { line: 3, col: 2 }, Pos { line: 5, col: 4 }));
        assert_eq!(s.lines(), 3);
        assert_eq!(s.covers(2), None);
        assert_eq!(s.covers(3), Some((2, usize::MAX)));
        assert_eq!(s.covers(4), Some((0, usize::MAX)));
        assert_eq!(s.covers(5), Some((0, 5)));
        assert_eq!(s.covers(6), None);
        assert!(!s.is_empty());
        assert!(Selection::new(Pos { line: 1, col: 1 }).is_empty());
    }

    #[test]
    fn extracts_by_columns_across_wrapped_lines() {
        let lines = ["▌ hello world", "  second line", "  third"];
        let mut s = Selection::new(Pos { line: 0, col: 2 });
        s.head = Pos { line: 2, col: 4 };
        let got = s.extract(|i| lines.get(i).map(|l| l.to_string()));
        // the first line is taken from column 2 (after the rail); the others
        // from their first column, so their indent rail goes
        assert_eq!(got, "hello world\nsecond line\nthi");
        assert_eq!(slice_cols("日本語abc", 2, 7), "本語a");
    }

    #[test]
    fn a_reverse_drag_from_a_gap_gives_the_same_text() {
        let lines = ["◆ first", "", "  code line", "  more"];
        let mut s = Selection::new(Pos { line: 3, col: 5 });
        s.head = Pos { line: 1, col: 0 };
        let got = s.extract(|i| lines.get(i).map(|l| l.to_string()));
        assert_eq!(got, "\ncode line\nmore");
        // the drag covered the rail's two cells and three letters
        let mut s = Selection::new(Pos { line: 0, col: 0 });
        s.head = Pos { line: 0, col: 4 };
        assert_eq!(s.extract(|i| lines.get(i).map(|l| l.to_string())), "fir");
    }

    #[test]
    fn rails_and_line_number_gutters_are_stripped() {
        assert_eq!(strip_rail("▌ hello"), "hello");
        assert_eq!(strip_rail("  indented"), "indented");
        assert_eq!(strip_rail("plain"), "plain");
        assert_eq!(strip_rail("✓"), "");
        let got = strip_gutter(vec!["  1│ fn main() {".into(), " 12│     x".into(), "".into()]);
        assert_eq!(got, vec!["fn main() {", "    x", ""]);
        let kept = strip_gutter(vec!["  1│ a".into(), "not numbered".into()]);
        assert_eq!(kept[1], "not numbered");
        assert_eq!(kept[0], "  1│ a", "mixed lines keep their gutters");
        let lines = ["  1│ use x;", "  2│ fn f() {}"];
        let mut s = Selection::new(Pos { line: 0, col: 0 });
        s.head = Pos { line: 1, col: 40 };
        assert_eq!(s.extract(|i| lines.get(i).map(|l| l.to_string())), "use x;\nfn f() {}");
    }

    #[test]
    fn styles_only_the_covered_columns() {
        let l = Line::from(vec![Span::raw("abc"), Span::raw("def")]);
        let out = style_cols(l, 1, 4, Style::default().bg(ratatui::style::Color::Blue));
        assert_eq!(text(&out), "abcdef");
        let styled: Vec<String> =
            out.spans.iter().filter(|s| s.style.bg.is_some()).map(|s| s.content.to_string()).collect();
        assert_eq!(styled, vec!["bc", "d"]);
        let out = style_cols(Line::from("ab"), 0, 5, Style::default().bg(ratatui::style::Color::Blue));
        assert_eq!(text(&out), "ab   ", "the empty columns are drawn too");
    }
}
