//! Text selection over the transcript: a drag from an anchor to a head, in
//! absolute coordinates of the laid-out transcript (line, column).

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::regions::{Cont, Kind};
use crate::render::wrap::width;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// The box a drag started in: block `block`, region `idx` of it, rows `start..=end` of the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionRef {
    pub block: usize,
    pub idx: usize,
    pub start: usize,
    pub end: usize,
    pub deco: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    /// The button is still down.
    pub dragging: bool,
    /// The box the drag started in: it stays inside it.
    pub region: Option<RegionRef>,
    /// Columns of decoration left of the box, never painted nor copied.
    pub deco: usize,
    /// Made by a double or triple click: a drag does not move it.
    pub fixed: bool,
    /// Started with Alt or Ctrl: no box, no clamp.
    pub free: bool,
}

impl Selection {
    pub fn new(anchor: Pos) -> Selection {
        Selection { anchor, head: anchor, dragging: true, region: None, deco: 0, fixed: false, free: false }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head && !self.fixed
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
        let from = if line == s.line { s.col } else { 0 }.max(self.deco);
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

/// Display width of `s`.
pub fn width_of(s: &str) -> usize {
    width(s)
}

/// The columns `a..b` of the word under column `col` of `text`, not reaching left of `lo`:
/// letters, digits and `_` make a word, any other glyph a run of its own kind. None on spaces.
pub fn word_bounds(text: &str, col: usize, lo: usize) -> Option<(usize, usize)> {
    fn class(g: &str) -> u8 {
        let c = g.chars().next().unwrap_or(' ');
        if c.is_whitespace() {
            0
        } else if c.is_alphanumeric() || c == '_' {
            1
        } else {
            2
        }
    }
    let mut cells: Vec<(usize, usize, u8)> = Vec::new();
    let mut at = 0;
    for g in text.graphemes(true) {
        let w = width(g);
        cells.push((at, at + w, class(g)));
        at += w;
    }
    let i = cells.iter().position(|(a, b, _)| *a <= col && col < *b)?;
    let k = cells[i].2;
    if k == 0 || cells[i].0 < lo {
        return None;
    }
    let mut a = i;
    while a > 0 && cells[a - 1].2 == k && cells[a - 1].0 >= lo {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < cells.len() && cells[b + 1].2 == k {
        b += 1;
    }
    Some((cells[a].0, cells[b].1))
}

/// What the layout knows of one selected row, for a copy that takes the content and not the screen.
#[derive(Debug, Clone, Default)]
pub struct LineInfo {
    pub text: String,
    /// The row continues the one above (a wrap).
    pub cont: Option<Cont>,
    /// The box the row sits in: (block, region).
    pub region: Option<(usize, usize)>,
    pub deco: usize,
    /// The row's original text, when the box keeps it.
    pub src: Option<String>,
    /// A heading of the box, not content.
    pub skip: bool,
    /// Blank columns before the row's rail.
    pub margin: usize,
    /// `src` is the row's whole content, not laid out in the row's columns (a table row).
    pub whole: bool,
}

impl Selection {
    /// The selected text from the rows' layout data (`infos[k]` is row `range().0.line + k`).
    /// Inside a box only the box's content comes out, its decoration dropped and its indentation
    /// kept; from prose, boxes crossed are taken whole and the rest as today; free, as today.
    /// Rows the layout wrapped are joined back with the spaces the wrap took out.
    pub fn extract_clean(&self, infos: &[LineInfo]) -> String {
        let (s, _) = self.range();
        let contained = self.region.map(|r| (r.block, r.idx)).filter(|_| !self.free);
        let mut logical: Vec<String> = Vec::new();
        for (k, info) in infos.iter().enumerate() {
            let (from, to) = self.covers(s.line + k).unwrap_or((0, 0));
            let boxed = info.region.is_some() && !self.free;
            let piece = if let Some(own) = contained {
                if info.region != Some(own) || info.skip {
                    continue;
                }
                box_piece(info, from, to)
            } else if boxed {
                if info.skip {
                    continue;
                }
                box_piece(info, from, to)
            } else if let Some(c) = info.cont {
                slice_cols(&info.text, from.max(c.deco as usize), to)
            } else {
                let body = slice_cols(&info.text, info.margin, usize::MAX);
                let piece = strip_rail(&body);
                if is_rule(&piece) {
                    // the rule over a code block crossed from prose: screen, not content
                    continue;
                }
                let shift = width(&info.text) - width(&piece);
                if from <= shift {
                    // started in the margin or the rail: the requested columns move with them
                    let to = if to == usize::MAX { to } else { to.saturating_sub(shift) };
                    slice_cols(&piece, 0, to)
                } else {
                    slice_cols(&info.text, from, to)
                }
            };
            match info.cont {
                Some(c) if !logical.is_empty() && k > 0 => {
                    if let Some(last) = logical.last_mut() {
                        last.push_str(&" ".repeat(c.gap as usize));
                        last.push_str(&piece);
                    }
                }
                _ => logical.push(piece),
            }
        }
        let mut logical: Vec<String> = logical.into_iter().map(|l| l.trim_end().to_string()).collect();
        // a drag that ends on a block's blank separator row copies no trailing newline
        while logical.len() > 1 && logical.last().is_some_and(String::is_empty) {
            logical.pop();
        }
        if self.free || (contained.is_none() && !infos.iter().any(|i| i.region.is_some())) {
            strip_gutter(logical).join("\n")
        } else {
            logical.join("\n")
        }
    }
}

/// The content of a row of a box between columns `from` and `to` of the screen.
fn box_piece(info: &LineInfo, from: usize, to: usize) -> String {
    let deco = info.cont.map_or(info.deco, |c| info.deco.max(c.deco as usize));
    let from = from.max(deco);
    match &info.src {
        Some(src) if info.whole => src.clone(),
        Some(src) if info.cont.is_none() => {
            let base = info.deco;
            slice_tabs(src, from - base, if to == usize::MAX { to } else { to.saturating_sub(base) })
        }
        _ => slice_cols(&info.text, from, to),
    }
}

/// The glyphs the transcript puts in front of a block's first line.
const RAILS: &[char] = &['▌', '▎', '◆', '▸', '▾', '✓', '✗', '○', '⊘', '▲', '●', '◇'];

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

/// A row drawn as a rule, `────── js`: the line over a code block with its language, or a
/// horizontal rule; it has no text of its own to copy.
fn is_rule(row: &str) -> bool {
    let row = row.trim();
    let label = row.trim_start_matches('─');
    row.len() - label.len() >= 3 * '─'.len_utf8()
        && (label.is_empty() || (label.starts_with(' ') && !label[1..].contains(' ')))
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

/// [`slice_cols`] over a source text whose tabs count four columns, as the layout draws them.
pub fn slice_tabs(s: &str, from: usize, to: usize) -> String {
    if !s.contains('\t') {
        return slice_cols(s, from, to);
    }
    let mut out = String::new();
    let mut col = 0;
    for g in s.graphemes(true) {
        let w = if g == "\t" { 4 } else { width(g) };
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
