//! The pulldown-cmark event loop: each start, end and text event as renderer state and lines.

use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::super::wrap::width;
use super::{ListCtx, Renderer, TableCtx};

impl<'t> Renderer<'t> {
    pub(super) fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    self.block_start();
                    self.in_paragraph = true;
                }
                Tag::Heading { level, .. } => {
                    self.block_start();
                    // a heading reads by its weight and colour; the # marks are never shown
                    self.heading = Some(level);
                }
                Tag::BlockQuote(_) => {
                    self.block_start();
                    self.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.flush();
                    self.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    self.flush();
                    if self.lists.is_empty() {
                        self.block_start();
                    }
                    let count = self.list_sizes.pop_front().unwrap_or(1).max(1);
                    let digits = start.map(|s| (s + count as u64 - 1).to_string().len()).unwrap_or(1);
                    self.lists.push(ListCtx { next: start, marker_pending: false, digits, task: None });
                }
                Tag::Item => {
                    self.flush();
                    if let Some(l) = self.lists.last_mut() {
                        l.marker_pending = true;
                        l.task = None;
                    }
                }
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { dest_url, .. } => self.link = Some(dest_url.to_string()),
                Tag::Image { dest_url, .. } => {
                    self.push_text("[image: ");
                    self.link = Some(dest_url.to_string());
                }
                Tag::Table(aligns) => {
                    self.flush();
                    self.table = Some(TableCtx {
                        rows: Vec::new(),
                        head: Vec::new(),
                        in_head: false,
                        current: Vec::new(),
                        aligns: aligns.to_vec(),
                    });
                }
                Tag::TableHead => {
                    if let Some(t) = &mut self.table {
                        t.in_head = true;
                        t.current.clear();
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = &mut self.table {
                        t.current.clear();
                    }
                }
                Tag::TableCell => self.cell = Some(String::new()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    self.flush();
                    self.in_paragraph = false;
                }
                TagEnd::Heading(_) => {
                    self.flush();
                    self.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.quote = self.quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, text)) = self.code.take() {
                        self.code_block(&lang, &text);
                    }
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                }
                TagEnd::Item => {
                    self.flush();
                    if let Some(l) = self.lists.last_mut() {
                        if l.marker_pending {
                            // an empty item still shows its marker
                            l.marker_pending = true;
                            self.runs.push((String::new(), Style::default()));
                            self.flush();
                        }
                    }
                }
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link => {
                    if let Some(url) = self.link.take() {
                        if let Some(c) = &mut self.cell {
                            // a cell's text is not in the runs: the address goes after it, inside the cell
                            if !c.contains(&url) {
                                c.push_str(&format!(" ({url})"));
                            }
                            return;
                        }
                        let shown: String = self
                            .runs
                            .iter()
                            .rev()
                            .take_while(|(_, s)| s.add_modifier.contains(Modifier::UNDERLINED))
                            .map(|(t, _)| t.clone())
                            .collect();
                        if !shown.contains(&url) {
                            self.runs.push((format!(" ({url})"), self.theme.dim()));
                        }
                    }
                }
                TagEnd::Image => {
                    self.link = None;
                    self.push_text("]");
                }
                TagEnd::Table => {
                    if let Some(t) = self.table.take() {
                        self.table(t);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(t) = &mut self.table {
                        t.head = std::mem::take(&mut t.current);
                        t.in_head = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(t) = &mut self.table {
                        let row = std::mem::take(&mut t.current);
                        t.rows.push(row);
                    }
                }
                TagEnd::TableCell => {
                    let c = self.cell.take().unwrap_or_default();
                    if let Some(t) = &mut self.table {
                        t.current.push(c);
                    }
                }
                _ => {}
            },
            Event::Text(t) => self.push_text(&t),
            Event::Code(c) => {
                if self.cell.is_some() || self.code.is_some() {
                    self.push_text(&c);
                } else {
                    let s = self.base_style().patch(self.theme.code_inline());
                    self.runs.push((c.to_string(), s));
                }
            }
            Event::Html(h) | Event::InlineHtml(h) => self.push_text(&h),
            Event::InlineMath(m) | Event::DisplayMath(m) => self.push_text(&m),
            Event::FootnoteReference(r) => self.push_text(&format!("[^{r}]")),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush();
                self.block_start();
                let (_, rest) = self.prefixes();
                let pw: usize = rest.iter().map(|s| width(&s.content)).sum();
                let mut spans = rest;
                spans.push(Span::styled("─".repeat(self.width.saturating_sub(pw).max(3)), self.theme.dim()));
                self.out.push(Line::from(spans));
            }
            Event::TaskListMarker(done) => {
                if let Some(l) = self.lists.last_mut() {
                    l.task = Some(done);
                }
            }
        }
    }
}
