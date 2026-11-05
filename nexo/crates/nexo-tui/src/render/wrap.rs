//! Word wrapping of styled runs into lines of a given width.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::regions::Cont;

/// Display width of a string.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cuts `s` to at most `w` columns, with an ellipsis when cut.
pub fn truncate(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let gw = width(g);
        if used + gw > w.saturating_sub(1) {
            break;
        }
        out.push_str(g);
        used += gw;
    }
    out.push('…');
    out
}

/// Whether `c` makes the terminal and the layout disagree on where text sits: control
/// characters (tabs, escapes) and the joiners and selectors that one terminal draws as one
/// cell and another as two. A cell the layout thinks is covered is never redrawn, so text
/// would stay behind on scroll. Zero-width spaces and marks, and the bidirectional overrides that
/// make text read in another order than it is stored ("Trojan Source"), go too.
pub fn unsafe_char(c: char) -> bool {
    c.is_control() && c != '\n'
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{FE0E}'
                | '\u{FE0F}'
                | '\u{1F3FB}'..='\u{1F3FF}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// `s` on one line: cleaned, and its line breaks as spaces. For labels and names that a bar or a
/// table draws in a single row.
pub fn one_line(s: &str) -> String {
    clean(s).split(['\n', '\r']).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `s` with what the terminal would draw differently from the layout taken out: escape
/// sequences (colours from a command's output) dropped, tabs as four spaces, other control
/// characters, emoji joiners, presentation selectors and skin tones dropped.
pub fn clean(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\t' => out.push_str("    "),
            // a carriage return writes over the line, as a terminal does: a progress bar shows its last state
            '\r' => {
                if it.peek().is_some_and(|d| *d != '\n') {
                    out.truncate(out.rfind('\n').map_or(0, |i| i + 1));
                }
            }
            '\x1b' => match it.next() {
                // CSI: parameters up to a final byte in @..~
                Some('[') => {
                    for d in it.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                // OSC and other strings: up to BEL or ESC \\
                Some(']' | 'P' | '_' | '^') => {
                    while let Some(d) = it.next() {
                        if d == '\x07' {
                            break;
                        }
                        if d == '\x1b' {
                            it.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                _ => {}
            },
            c if unsafe_char(c) => {}
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Every span of `lines` passed through [`clean`].
pub fn clean_lines(lines: &mut [Line<'static>]) {
    for l in lines {
        for sp in &mut l.spans {
            if let std::borrow::Cow::Owned(t) = clean(&sp.content) {
                sp.content = t.into();
            }
        }
    }
}

struct Wrapper<'a> {
    lines: Vec<Line<'static>>,
    conts: Vec<Cont>,
    cur: Vec<Span<'static>>,
    cur_w: usize,
    max_w: usize,
    rest: &'a [Span<'static>],
    rest_w: usize,
    at_start: bool,
    prefix_len: usize,
}

impl Wrapper<'_> {
    fn limit(&self) -> usize {
        self.max_w.max(self.prefix_len + 1)
    }

    /// A break the layout made: the next row continues this one.
    fn soft(&mut self) {
        let mut gap = 0usize;
        'count: for s in self.cur.iter().rev() {
            for c in s.content.chars().rev() {
                if !c.is_whitespace() {
                    break 'count;
                }
                gap += 1;
            }
        }
        self.newline();
        self.conts.push(Cont { line: self.lines.len() as u32, gap: gap.min(255) as u8, deco: self.rest_w as u16 });
    }

    fn newline(&mut self) {
        trim_end(&mut self.cur);
        self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        self.cur = self.rest.to_vec();
        self.cur_w = self.rest_w;
        self.prefix_len = self.rest_w;
        self.at_start = true;
    }

    fn word(&mut self, word: &str, style: Style) {
        if word.is_empty() {
            return;
        }
        let w = width(word);
        if self.cur_w + w > self.limit() && !self.at_start {
            self.soft();
        }
        if self.cur_w + w > self.limit() {
            // wider than a whole line: cut by graphemes
            let mut piece = String::new();
            let mut pw = 0;
            for g in word.graphemes(true) {
                let gw = width(g);
                if self.cur_w + pw + gw > self.limit() && pw > 0 {
                    self.cur.push(Span::styled(std::mem::take(&mut piece), style));
                    self.soft();
                    pw = 0;
                }
                piece.push_str(g);
                pw += gw;
            }
            if !piece.is_empty() {
                self.cur.push(Span::styled(piece, style));
                self.cur_w += pw;
            }
            self.at_start = false;
            return;
        }
        self.cur.push(Span::styled(word.to_string(), style));
        self.cur_w += w;
        self.at_start = false;
    }
}

/// Wraps styled runs into lines no wider than `max_w`, breaking at spaces
/// (and inside a word only when the word alone is wider than the line).
/// `first` and `rest` are the prefixes of the first and the following lines.
pub fn wrap_runs(
    runs: &[(String, Style)],
    max_w: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> Vec<Line<'static>> {
    wrap_runs_ex(runs, max_w, first, rest).0
}

/// [`wrap_runs`] and the rows that continue the one above (a break the layout made, not the text).
pub fn wrap_runs_ex(
    runs: &[(String, Style)],
    max_w: usize,
    first: &[Span<'static>],
    rest: &[Span<'static>],
) -> (Vec<Line<'static>>, Vec<Cont>) {
    let first_w: usize = first.iter().map(|s| width(&s.content)).sum();
    let rest_w: usize = rest.iter().map(|s| width(&s.content)).sum();
    let mut w = Wrapper {
        lines: Vec::new(),
        conts: Vec::new(),
        cur: first.to_vec(),
        cur_w: first_w,
        max_w,
        rest,
        rest_w,
        at_start: true,
        prefix_len: first_w,
    };
    for (text, style) in runs {
        let mut word = String::new();
        for ch in clean(text).chars() {
            if ch == '\n' {
                w.word(&word, *style);
                word.clear();
                w.newline();
                continue;
            }
            word.push(ch);
            if ch == ' ' {
                w.word(&word, *style);
                word.clear();
            }
        }
        w.word(&word, *style);
    }
    trim_end(&mut w.cur);
    w.lines.push(Line::from(w.cur));
    (w.lines, w.conts)
}

fn trim_end(spans: &mut Vec<Span<'static>>) {
    while let Some(last) = spans.last_mut() {
        let trimmed = last.content.trim_end().to_string();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            last.content = trimmed.into();
            break;
        }
    }
}

/// Plain text wrapped at `max_w` with a prefix on every line.
pub fn wrap_plain(text: &str, max_w: usize, style: Style, prefix: &[Span<'static>]) -> Vec<Line<'static>> {
    wrap_plain_ex(text, max_w, style, prefix).0
}

/// [`wrap_plain`] and its continuation rows.
pub fn wrap_plain_ex(
    text: &str,
    max_w: usize,
    style: Style,
    prefix: &[Span<'static>],
) -> (Vec<Line<'static>>, Vec<Cont>) {
    let mut out = Vec::new();
    let mut conts = Vec::new();
    for raw in text.split('\n') {
        let runs = [(raw.to_string(), style)];
        let (lines, cs) = wrap_runs_ex(&runs, max_w, prefix, prefix);
        conts.extend(cs.into_iter().map(|c| Cont { line: c.line + out.len() as u32, ..c }));
        out.extend(lines);
    }
    (out, conts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_takes_out_what_the_terminal_draws_differently() {
        assert!(matches!(clean("plain · text ─"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(clean("a\tb"), "a    b");
        assert_eq!(clean("\x1b[31mred\x1b[0m ok"), "red ok");
        assert_eq!(clean("\x1b]8;;http://x\x07link\x1b]8;;\x1b\\"), "link");
        assert_eq!(clean("50%\r100%"), "100%", "a carriage return writes over the line");
        assert_eq!(clean("a\nb 50%\rb 100%\r\nc"), "a\nb 100%\nc");
        assert_eq!(clean("⚠\u{FE0F} x"), "⚠ x");
        assert_eq!(width(&clean("👨\u{200D}👩")), 4, "one cell pair per emoji, as every terminal draws them");
        assert_eq!(clean("👍\u{1F3FD}"), "👍");
        let mut ls = vec![Line::from(vec![Span::raw("\tx"), Span::raw("y")])];
        clean_lines(&mut ls);
        assert_eq!(text(&ls[0]), "    xy");
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn wraps_at_spaces_and_cuts_long_words() {
        let runs = [("one two three four".to_string(), Style::default())];
        let lines = wrap_runs(&runs, 9, &[], &[]);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["one two", "three", "four"]);
        let runs = [("abcdefghijkl".to_string(), Style::default())];
        let lines = wrap_runs(&runs, 5, &[], &[]);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["abcde", "fghij", "kl"]);
    }

    #[test]
    fn keeps_prefixes_and_newlines() {
        let runs = [("a\nb c".to_string(), Style::default())];
        let p = [Span::raw("> ")];
        let lines = wrap_runs(&runs, 20, &p, &p);
        let t: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(t, vec!["> a", "> b c"]);
        assert_eq!(truncate("hello world", 6), "hello…");
        let lines = wrap_plain("", 10, Style::default(), &[]);
        assert_eq!(lines.len(), 1);
    }
}

#[cfg(test)]
mod cont_tests {
    use super::*;

    #[test]
    fn marks_the_rows_the_layout_wrapped_and_the_spaces_it_trimmed() {
        let runs = [("one  two three".to_string(), Style::default())];
        let (lines, conts) = wrap_runs_ex(&runs, 8, &[], &[Span::raw("  ")]);
        assert_eq!(lines.len(), 3);
        assert_eq!(conts, vec![Cont { line: 1, gap: 2, deco: 2 }, Cont { line: 2, gap: 1, deco: 2 }]);
        let runs = [("abcdefghij".to_string(), Style::default())];
        let (_, conts) = wrap_runs_ex(&runs, 5, &[], &[]);
        assert_eq!(conts, vec![Cont { line: 1, gap: 0, deco: 0 }]);
        let (lines, conts) = wrap_plain_ex("a\nb c d e", 3, Style::default(), &[]);
        assert_eq!(lines.len(), 4);
        assert!(conts.iter().all(|c| c.line >= 2), "a hard newline is not a wrap");
    }
}
