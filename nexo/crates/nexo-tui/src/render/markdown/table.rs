//! Tables: cells wrapped inside their column, aligned and boxed.

use pulldown_cmark::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::super::wrap::width;
use super::{Renderer, TableCtx};
use crate::regions::{Kind, Region};

/// `text` broken into lines of at most `w` columns, at spaces when it can.
fn wrap_cell(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        // a word wider than the column is cut where the column ends
        while width(&word) > w {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let head = truncate_hard(&word, w);
            word = word[head.len()..].to_string();
            lines.push(head);
        }
        if cur.is_empty() {
            cur = word;
        } else if width(&cur) + 1 + width(&word) <= w {
            cur.push(' ');
            cur.push_str(&word);
        } else {
            lines.push(std::mem::replace(&mut cur, word));
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The longest start of `s` that fits in `w` columns (at least one char).
fn truncate_hard(s: &str, w: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let mut next = out.clone();
        next.push(c);
        if width(&next) > w && !out.is_empty() {
            break;
        }
        out = next;
    }
    out
}

impl<'t> Renderer<'t> {
    pub(super) fn table(&mut self, t: TableCtx) {
        self.block_start();
        let (_, rest) = self.prefixes();
        let prefix_w: usize = rest.iter().map(|s| width(&s.content)).sum();
        let avail = self.width.saturating_sub(prefix_w).max(8);
        let mut rows: Vec<Vec<String>> = Vec::new();
        if !t.head.is_empty() {
            rows.push(t.head.clone());
        }
        rows.extend(t.rows.iter().cloned());
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![1usize; cols];
        for r in &rows {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(width(c.trim()));
            }
        }
        // shrink the widest columns until the table fits
        let sep = 3 * cols.saturating_sub(1);
        while widths.iter().sum::<usize>() + sep > avail {
            let (i, _) = widths.iter().enumerate().max_by_key(|(_, w)| **w).unwrap_or((0, &0));
            if widths[i] <= 3 {
                break;
            }
            widths[i] -= 1;
        }
        let has_head = !t.head.is_empty();
        let first_row = self.out.len();
        // a long cell wraps inside its column; the row grows to its tallest cell
        let wrapped: Vec<Vec<Vec<String>>> = rows
            .iter()
            .map(|r| {
                widths
                    .iter()
                    .enumerate()
                    .map(|(ci, w)| wrap_cell(r.get(ci).map(|s| s.trim()).unwrap_or(""), *w))
                    .collect()
            })
            .collect();
        // once a body row spans lines, a faint rule keeps the rows apart
        let ruled = wrapped.iter().skip(usize::from(has_head)).any(|c| c.iter().any(|l| l.len() > 1));
        let mut tsrc: Vec<String> = Vec::new();
        let mut tskip: Vec<u32> = Vec::new();
        for (ri, cells) in wrapped.iter().enumerate() {
            if ruled && ri > usize::from(has_head) {
                let mut spans = rest.clone();
                let line: Vec<String> = widths.iter().map(|w| "╌".repeat(*w)).collect();
                spans.push(Span::styled(line.join("╌┼╌"), self.theme.dim2()));
                self.out.push(Line::from(spans));
                tskip.push((self.out.len() - 1) as u32);
                tsrc.push(String::new());
            }
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            let style =
                if has_head && ri == 0 { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
            for li in 0..height {
                let mut spans = rest.clone();
                if li == 0 {
                    let full: Vec<&str> = (0..cols).map(|ci| rows[ri].get(ci).map_or("", |s| s.trim())).collect();
                    tsrc.push(full.join("\t"));
                } else {
                    tskip.push(self.out.len() as u32);
                    tsrc.push(String::new());
                }
                for (ci, w) in widths.iter().enumerate() {
                    let text = cells[ci].get(li).map(String::as_str).unwrap_or("");
                    let pad = w.saturating_sub(width(text));
                    let (left, right) = match t.aligns.get(ci) {
                        Some(Alignment::Right) => (pad, 0),
                        Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                        _ => (0, pad),
                    };
                    spans.push(Span::styled(format!("{}{text}{}", " ".repeat(left), " ".repeat(right)), style));
                    if ci + 1 < cols {
                        spans.push(Span::styled(" │ ", self.theme.dim()));
                    }
                }
                self.out.push(Line::from(spans));
            }
            if has_head && ri == 0 {
                let mut spans = rest.clone();
                let line: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                spans.push(Span::styled(line.join("─┼─"), self.theme.dim()));
                self.out.push(Line::from(spans));
                tskip.push((self.out.len() - 1) as u32);
                tsrc.push(String::new());
            }
        }
        if self.out.len() > first_row {
            let mut r = Region::new(Kind::Table, first_row, self.out.len() - 1, prefix_w);
            if tsrc.len() == self.out.len() - first_row {
                r.src = Some(std::sync::Arc::new(tsrc));
                r.skip = tskip;
            }
            self.meta.regions.push(r);
        }
    }
}
