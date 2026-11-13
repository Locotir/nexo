//! Markdown to pre-wrapped styled lines (pulldown-cmark events in, lines out).

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

use super::wrap::{width, wrap_runs_ex};
use crate::regions::Meta;

struct ListCtx {
    /// Next number for an ordered list; None for bullets.
    next: Option<u64>,
    /// Whether the current item still waits for its first line (marker pending).
    marker_pending: bool,
    /// Columns of the widest number of the list, so `9.` and `10.` align.
    digits: usize,
    /// A task item: Some(done) once its checkbox was read.
    task: Option<bool>,
}

struct Renderer<'t> {
    theme: &'t Theme,
    /// The text's lines with their tabs, and a cursor past the fences already taken: set only when
    /// tabs are all the cleaning changed, so a fence copies with the tabs it was written with.
    raw: Option<(Vec<String>, usize)>,
    width: usize,
    out: Vec<Line<'static>>,
    runs: Vec<(String, Style)>,
    bold: u8,
    italic: u8,
    strike: u8,
    link: Option<String>,
    heading: Option<HeadingLevel>,
    lists: Vec<ListCtx>,
    quote: usize,
    code: Option<(String, String)>,
    table: Option<TableCtx>,
    cell: Option<String>,
    in_paragraph: bool,
    /// Item counts of the lists in document order (a pre-pass), for number alignment.
    list_sizes: std::collections::VecDeque<usize>,
    /// Code blocks shown for what they mean (Markdown rendered, JSON indented, tables).
    interpreted: bool,
    meta: Meta,
}

struct TableCtx {
    rows: Vec<Vec<String>>,
    head: Vec<String>,
    in_head: bool,
    current: Vec<String>,
    aligns: Vec<Alignment>,
}

impl<'t> Renderer<'t> {
    fn base_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(level) = self.heading {
            s = s.patch(self.theme.heading());
            if level == HeadingLevel::H1 {
                s = s.add_modifier(Modifier::UNDERLINED);
            }
        }
        // No italics (hard to read with dyslexia, docs/READABLE-TEXT.md): emphasis is a colour, or
        // bold without colours; strong text is bold in a colour of its own, so it stands out on a scan.
        let plain = self.heading.is_none() && self.link.is_none();
        if self.italic > 0 {
            s = if self.theme.mono {
                s.add_modifier(Modifier::BOLD)
            } else if plain {
                s.fg(self.theme.info)
            } else {
                s
            };
        }
        if self.bold > 0 {
            s = s.add_modifier(Modifier::BOLD);
            if plain && !self.theme.mono {
                s = s.fg(self.theme.accent2);
            }
        }
        if self.strike > 0 {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link.is_some() {
            s = s.fg(self.theme.accent).add_modifier(Modifier::UNDERLINED);
        }
        s
    }

    /// Prefixes of the first and following lines of the current block.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut rest: Vec<Span<'static>> = Vec::new();
        for _ in 0..self.quote {
            rest.push(Span::styled("▎ ", self.theme.style(self.theme.dim)));
        }
        let depth = self.lists.len();
        let mut first = rest.clone();
        for (i, l) in self.lists.iter_mut().enumerate() {
            let last = i + 1 == depth;
            let marker = match (l.task.filter(|_| last), l.next) {
                (Some(true), _) => "☑ ".to_string(),
                (Some(false), _) => "☐ ".to_string(),
                (None, Some(n)) => format!("{n:>w$}. ", w = l.digits),
                (None, None) => match i % 3 {
                    0 => "• ".to_string(),
                    1 => "◦ ".to_string(),
                    _ => "– ".to_string(),
                },
            };
            let pad = " ".repeat(width(&marker));
            if last && l.marker_pending {
                first.push(Span::styled(marker.clone(), self.theme.style(self.theme.accent)));
                l.marker_pending = false;
                if let Some(n) = l.next.as_mut() {
                    *n += 1;
                }
            } else {
                first.push(Span::raw(pad.clone()));
            }
            rest.push(Span::raw(pad));
        }
        (first, rest)
    }

    fn blank(&mut self) {
        if let Some(last) = self.out.last() {
            if !last.spans.is_empty() {
                self.out.push(Line::default());
            }
        }
    }

    /// Starts a block: a blank line between blocks, unless the block is the first line of a list item.
    fn block_start(&mut self) {
        let pending = self.lists.last().is_some_and(|l| l.marker_pending);
        if !pending {
            self.blank();
        }
    }

    fn flush(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.runs);
        let (first, rest) = self.prefixes();
        let (lines, conts) = wrap_runs_ex(&runs, self.width, &first, &rest);
        let base = self.out.len() as u32;
        self.meta.conts.extend(conts.into_iter().map(|c| crate::regions::Cont { line: c.line + base, ..c }));
        self.out.extend(lines);
    }

    fn push_text(&mut self, text: &str) {
        if let Some(c) = &mut self.cell {
            c.push_str(text);
            return;
        }
        if let Some((_, buf)) = &mut self.code {
            buf.push_str(text);
            return;
        }
        let style = self.base_style();
        self.runs.push((text.to_string(), style));
    }
}

/// `md` rendered at `width` columns.
pub fn render(md: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    render_as(md, width, theme, false)
}

/// `md` rendered at `width` columns, its code blocks interpreted when `interpreted`.
pub fn render_as(md: &str, width: usize, theme: &Theme, interpreted: bool) -> Vec<Line<'static>> {
    render_as_meta(md, width, theme, interpreted).0
}

/// [`render_as`] and the boxes and wrapped rows of its lines.
pub fn render_as_meta(md: &str, width: usize, theme: &Theme, interpreted: bool) -> (Vec<Line<'static>>, Meta) {
    // escapes cleaned before parsing: the parser splits "\x1b[31m" at the bracket, and a span
    // cleaned on its own would leave "[31m" behind
    let raw_md = md;
    let md = &*crate::render::wrap::clean(md);
    let raw = (raw_md.contains('\t') && raw_md.replace('\t', "    ") == md)
        .then(|| (raw_md.split('\n').map(str::to_string).collect::<Vec<_>>(), 0));
    let width = width.max(8);
    let mut r = Renderer {
        theme,
        raw,
        width,
        out: Vec::new(),
        runs: Vec::new(),
        bold: 0,
        italic: 0,
        strike: 0,
        link: None,
        heading: None,
        lists: Vec::new(),
        quote: 0,
        code: None,
        table: None,
        cell: None,
        in_paragraph: false,
        list_sizes: Default::default(),
        interpreted,
        meta: Meta::default(),
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    r.list_sizes = list_sizes(md, opts);
    for ev in Parser::new_ext(md, opts) {
        r.event(ev);
    }
    r.flush();
    if let Some((lang, text)) = r.code.take() {
        r.code_block(&lang, &text);
    }
    (r.out, r.meta)
}

/// How many items each list has, in document order (nested lists included, by their start).
fn list_sizes(md: &str, opts: Options) -> std::collections::VecDeque<usize> {
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (position in `out`, items so far)
    let mut out: Vec<usize> = Vec::new();
    for ev in Parser::new_ext(md, opts) {
        match ev {
            Event::Start(Tag::List(_)) => {
                stack.push((out.len(), 0));
                out.push(0);
            }
            Event::Start(Tag::Item) => {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
            }
            Event::End(TagEnd::List(_)) => {
                if let Some((pos, n)) = stack.pop() {
                    out[pos] = n;
                }
            }
            _ => {}
        }
    }
    out.into_iter().collect()
}

mod code_block;
mod events;
mod inline;
mod table;

pub use inline::*;

#[cfg(test)]
mod tests;
