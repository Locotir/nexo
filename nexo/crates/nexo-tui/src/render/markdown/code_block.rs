//! Fenced and indented code blocks: a labelled box of highlighted lines.

use ratatui::text::{Line, Span};

use super::super::code;
use super::super::wrap::{clip_spans, width};
use super::Renderer;
use crate::regions::{Kind, Region};

impl<'t> Renderer<'t> {
    pub(super) fn code_block(&mut self, lang: &str, text: &str) {
        self.block_start();
        let (_, rest) = self.prefixes();
        let prefix_w: usize = rest.iter().map(|s| width(&s.content)).sum();
        let inner = self.width.saturating_sub(prefix_w + 2).max(8);
        let lang = lang.trim();
        let body = text.strip_suffix('\n').unwrap_or(text);
        let (lines, shown) = code::view(lang, body, inner, self.theme, self.interpreted);
        let label = if lang.is_empty() { "code".to_string() } else { lang.to_string() };
        let label = if shown { format!("{label} · view") } else { label };
        // the label stops one column short, so it never touches the scrollbar
        let bar = "─".repeat((inner + 1).saturating_sub(width(&label) + 1));
        let mut head = rest.clone();
        head.push(Span::styled(format!("{bar} "), self.theme.dim2()));
        head.push(Span::styled(label, self.theme.dim()));
        self.out.push(Line::from(head));
        let first_row = self.out.len();
        let bg = self.theme.code_block();
        let rail = self.theme.style(self.theme.border);
        for l in lines {
            let mut spans = rest.clone();
            spans.push(Span::styled("▎", rail));
            spans.push(Span::styled(" ", bg));
            let mut used = 0;
            for s in clip_spans(l.spans, inner) {
                used += width(&s.content);
                spans.push(Span::styled(s.content.to_string(), s.style.patch(bg)));
            }
            if used < inner {
                spans.push(Span::styled(" ".repeat(inner - used), bg));
            }
            self.out.push(Line::from(spans));
        }
        if self.out.len() > first_row {
            let mut r = Region::new(Kind::Code, first_row, self.out.len() - 1, prefix_w + 2);
            let mut src: Vec<String> = body.split('\n').map(str::to_string).collect();
            if let Some((raw, at)) = &mut self.raw {
                // the same lines as written, tabs and all
                let n = src.len();
                let hit = (*at..(raw.len() + 1).saturating_sub(n))
                    .find(|&i| raw[i..i + n].iter().zip(&src).all(|(a, b)| a.replace('\t', "    ") == *b));
                if let Some(i) = hit {
                    src = raw[i..i + n].to_vec();
                    *at = i + n;
                }
            }
            if !shown && src.len() == self.out.len() - first_row {
                r.src = Some(std::sync::Arc::new(src));
            }
            self.meta.regions.push(r);
        }
    }
}
