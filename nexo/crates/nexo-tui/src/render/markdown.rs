//! Markdown to pre-wrapped styled lines (pulldown-cmark events in, lines out).

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

use super::code;
use super::wrap::{clip_spans, truncate, width, wrap_runs_ex};
use crate::regions::{Kind, Meta, Region};

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

    fn code_block(&mut self, lang: &str, text: &str) {
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

    fn table(&mut self, t: TableCtx) {
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

    fn event(&mut self, ev: Event<'_>) {
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

/// The plain text of rendered lines (for searching and copying).
pub fn text_of(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// `md` as wrapped lines inside `room` columns: plain text takes `base`, every line starts with
/// `prefix`, blank lines at the edges are dropped.
pub fn md_lines(
    md: &str,
    room: usize,
    theme: &Theme,
    base: Style,
    interpreted: bool,
    prefix: &[Span<'static>],
) -> Vec<Line<'static>> {
    md_lines_meta(md, room, theme, base, interpreted, prefix).0
}

/// [`md_lines`] and its wrapped rows (the boxes inside are left to the caller, who boxes the whole).
pub fn md_lines_meta(
    md: &str,
    room: usize,
    theme: &Theme,
    base: Style,
    interpreted: bool,
    prefix: &[Span<'static>],
) -> (Vec<Line<'static>>, Meta) {
    let pw: usize = prefix.iter().map(|s| width(&s.content)).sum();
    let (mut lines, mut meta) = render_as_meta(md, room.saturating_sub(pw), theme, interpreted);
    while lines.last().is_some_and(|l| text_of(l).trim().is_empty()) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| text_of(l).trim().is_empty()).count();
    meta.regions.clear();
    meta.drop_front(lead);
    meta.truncate(lines.len() - lead.min(lines.len()));
    meta.add_deco(pw);
    let lines = lines
        .into_iter()
        .skip(lead)
        .map(|l| {
            let mut v = prefix.to_vec();
            v.extend(l.spans.into_iter().map(|s| tint_plain(s, base)));
            Line::from(v)
        })
        .collect();
    (lines, meta)
}

/// `s` without inline marks (backticks, `**`, `__`), for one-line places that cannot style.
pub fn strip_md(s: &str) -> String {
    s.replace("**", "").replace("__", "").replace('`', "")
}

/// One line of a message as styled runs: backticks and `**` become code and bold, plain text takes
/// `base`. A line without marks, or one that would read as a list, heading or quote, stays as is.
pub fn inline_runs(line: &str, theme: &Theme, base: Style) -> Vec<(String, Style)> {
    let t = line.trim_start();
    let blockish = t.starts_with(['#', '>', '|'])
        || ["- ", "* ", "+ "].iter().any(|p| t.starts_with(p))
        || t.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(". ");
    if blockish || !(t.contains('`') || t.contains("**") || t.contains("__")) {
        return vec![(line.to_string(), base)];
    }
    let indent = &line[..line.len() - t.len()];
    let mut runs = Vec::new();
    if !indent.is_empty() {
        runs.push((indent.to_string(), base));
    }
    runs.extend(inline_md(t, usize::MAX / 2, theme, base).into_iter().map(|s| (s.content.into_owned(), s.style)));
    runs
}

/// Whether plain text is a markdown report (a `#` heading, `**bold**` or a table) and not log or
/// command output.
pub fn looks_markdown(text: &str) -> bool {
    let is_heading = |l: &str| {
        let h = l.trim_start_matches('#');
        l.starts_with('#') && l.len() - h.len() <= 6 && h.starts_with(' ')
    };
    // a lone `# x` further down is more likely a shell comment
    let heading =
        text.lines().next().is_some_and(is_heading) || text.lines().any(|l| l.starts_with("##") && is_heading(l));
    let bold = text.matches("**").count() >= 2;
    let table = text.lines().any(|l| {
        let t = l.trim();
        t.starts_with('|') && t.contains("---")
    });
    heading || bold || table
}

/// A span of rendered markdown: plain text (no colour of its own) takes `base`; code, links and
/// the like keep theirs.
pub fn tint_plain(s: Span<'static>, base: Style) -> Span<'static> {
    if s.style.fg.is_none() {
        Span::styled(s.content, base.patch(s.style))
    } else {
        s
    }
}

/// `md` (one line of it) rendered as markdown spans, cut to `room` columns with an ellipsis.
pub fn inline_md(md: &str, room: usize, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let lines = render(md, 10_000, theme);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (k, l) in lines.into_iter().filter(|l| !text_of(l).trim().is_empty()).enumerate() {
        if k > 0 {
            spans.push(Span::raw(" "));
        }
        spans.extend(l.spans.into_iter().map(|s| tint_plain(s, base)));
    }
    let mut used = 0;
    let mut out = Vec::new();
    for s in spans {
        let sw = width(&s.content);
        if used + sw > room {
            let left = room.saturating_sub(used);
            if left > 0 {
                out.push(Span::styled(truncate(&s.content, left), s.style));
            }
            break;
        }
        used += sw;
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_style_marks_and_leave_block_syntax_alone() {
        let t = Theme::dark();
        let runs = inline_runs("run `nexo login` now", &t, Style::default());
        let joined: String = runs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "run nexo login now");
        assert!(runs.len() >= 3, "{runs:?}");
        assert_eq!(inline_runs("  - a `b`", &t, Style::default()), vec![("  - a `b`".to_string(), Style::default())]);
        assert_eq!(strip_md("**bold** `code` __x__"), "bold code x");
        assert!(looks_markdown("## Report\ntext"));
        assert!(looks_markdown("| a | b |\n|---|---|\n| 1 | 2 |"));
        assert!(!looks_markdown("#!/bin/sh\nls -la\n# comment"));
        let l = md_lines("a **b** c", 20, &t, Style::default(), false, &[Span::raw("> ")]);
        assert_eq!(text_of(&l[0]), "> a b c");
    }

    fn texts(md: &str, w: usize) -> Vec<String> {
        render(md, w, &Theme::dark()).iter().map(text_of).collect()
    }

    #[test]
    fn headings_lists_and_paragraphs() {
        let t = texts(
            "# Title\n\nSome *emphasis* and **bold** text.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n",
            40,
        );
        assert_eq!(t[0], "Title");
        assert_eq!(t[1], "");
        assert_eq!(t[2], "Some emphasis and bold text.");
        assert!(t.contains(&"• one".to_string()));
        assert!(t.contains(&"  ◦ nested".to_string()));
        assert!(t.contains(&"1. first".to_string()));
        assert!(t.contains(&"2. second".to_string()));
    }

    #[test]
    fn emphasis_is_a_colour_not_italics_and_strong_text_stands_out() {
        let style_of = |theme: &Theme, word: &str| {
            render("plain *soft* and **loud**", 40, theme)
                .iter()
                .flat_map(|l| l.spans.clone())
                .find(|s| s.content.contains(word))
                .unwrap()
                .style
        };
        let t = Theme::dark();
        for theme in [Theme::dark(), Theme::light(), Theme::mono()] {
            for w in ["plain", "soft", "loud"] {
                assert!(!style_of(&theme, w).add_modifier.contains(Modifier::ITALIC), "{} {w}", theme.name);
            }
        }
        assert_eq!(style_of(&t, "soft").fg, Some(t.info));
        assert_eq!(style_of(&t, "loud").fg, Some(t.accent2));
        assert!(style_of(&t, "loud").add_modifier.contains(Modifier::BOLD));
        assert!(style_of(&Theme::mono(), "soft").add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn task_items_show_a_checkbox_in_place_of_the_bullet() {
        let t = texts("- [ ] open\n- [x] done\n  - plain\n- after", 40);
        assert_eq!(t[0], "☐ open");
        assert_eq!(t[1], "☑ done");
        assert_eq!(t[2], "  ◦ plain");
        assert_eq!(t[3], "• after");
    }

    #[test]
    fn code_fences_carry_a_language_label_and_keep_lines() {
        let t = texts("Before\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nAfter", 40);
        let i = t.iter().position(|l| l.starts_with("───") && l.ends_with(" rust")).expect("label");
        assert_eq!(t[i + 1].trim_end(), "▎ fn main() {");
        assert_eq!(t[i + 2].trim_end(), "▎     let x = 1;");
        assert_eq!(t[i + 3].trim_end(), "▎ }");
        assert_eq!(t.last().unwrap(), "After");
    }

    #[test]
    fn numbered_lists_align_and_inline_code_keeps_its_text() {
        let md = (1..=10).map(|i| format!("{i}. item")).collect::<Vec<_>>().join("\n");
        let t = texts(&md, 40);
        assert_eq!(t[0], " 1. item");
        assert_eq!(t[9], "10. item");
        let t = texts("use `x` here", 40);
        assert_eq!(t[0], "use x here");
    }

    #[test]
    fn tables_align_columns() {
        let t = texts("| a | bb |\n|---|---|\n| 1 | 2 |\n", 40);
        assert_eq!(t[0], "a │ bb");
        assert_eq!(t[1], "──┼───");
        assert_eq!(t[2], "1 │ 2 ");
    }

    #[test]
    fn long_cells_wrap_inside_their_column_and_numbers_keep_their_alignment() {
        let t = texts(
            "| name | note | $ |\n|---|---|---:|\n| a | one two three four five six | 4.99 |\n| b | x | 10 |\n",
            24,
        );
        assert!(t.iter().all(|l| width(l) <= 24), "{t:?}");
        let all = t.join("\n");
        assert!(all.contains("six") && !all.contains('…'), "nothing is cut: {t:?}");
        assert!(t.iter().any(|l| l.ends_with("│ 4.99")) && t.iter().any(|l| l.ends_with("│   10")), "{t:?}");
        assert!(t.iter().any(|l| l.contains('╌')), "wrapped rows are kept apart: {t:?}");
    }

    #[test]
    fn quotes_links_and_long_lines_wrap() {
        let t = texts("> quoted text here\n\nsee [docs](https://example.com) now", 14);
        assert!(t[0].starts_with("▎ quoted"));
        assert!(t.iter().any(|l| l.contains("docs")));
        assert!(t.iter().all(|l| width(l) <= 14), "{t:?}");
    }

    #[test]
    fn a_long_code_line_always_ends_in_an_ellipsis_whatever_the_width() {
        let md = "```rust\nlet value = compute(first_argument, second_argument, third_argument);\n```";
        for w in 10..40 {
            let t = texts(md, w);
            let row = t.iter().find(|l| l.starts_with('▎')).expect("a code row");
            assert!(width(row) <= w, "{w}: {row:?}");
            assert!(row.trim_end().ends_with('…'), "cut without a mark at {w}: {row:?}");
        }
    }

    #[test]
    fn a_link_in_a_table_cell_stays_in_its_cell() {
        let t = texts("| name | site |\n|---|---|\n| a | [docs](https://example.com/very/long/path) |\n\nafter", 40);
        let all = t.join("\n");
        assert!(t.iter().all(|l| width(l) <= 40), "{all}");
        let n = t.len();
        assert_eq!(t[n - 1], "after", "{all}");
        assert!(t[n - 2].is_empty() && t[n - 3].contains('│'), "no URL line left behind the table: {all}");
        assert!(all.contains("docs") && all.contains("example.com"), "{all}");
    }

    #[test]
    fn an_unclosed_fence_while_streaming_still_renders() {
        let t = texts("text\n\n```py\nprint(1)", 30);
        assert!(t.iter().any(|l| l.ends_with(" py")));
        assert!(t.iter().any(|l| l.contains("print(1)")));
    }

    /// The screen at `cols`x`rows` with a long answer ending in a Rust fence: the row of the fence label.
    fn label_row(cols: u16, rows: u16) -> Vec<char> {
        use nexo_core::agent::AgentEvent;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let opts = crate::TuiOptions {
            cwd: std::path::PathBuf::from("/tmp/project"),
            provider: "deepseek".into(),
            model: "deepseek-chat".into(),
            models: vec![("deepseek".into(), "deepseek-chat".into())],
            providers: vec![],
            theme: "nexo".into(),
            mouse: true,
            show_reasoning: true,
            timestamps: false,
            fold_after: 4,
            group_tools: true,
            complete_words: true,
            suggest: false,
            keys: Default::default(),
            notify_after: 15,
            animations: false,
            todo_strip: false,
            status_line: vec![],
            status_command: String::new(),
            vim: false,
            prompt: None,
            session_id: None,
            sessions: vec![],
            skills: vec![],
        };
        let mut a = crate::App::new(opts, tx);
        let lead: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let text = format!("{}\n\n```rust\nfn main() {{}}\n```\n", lead.join("\n\n"));
        a.handle_agent(AgentEvent::TextDelta { text });
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let rows = crate::ui::rows_of(term.backend().buffer());
        let row = rows.iter().find(|r| r.trim_end().ends_with("rust") || r.contains("rust ")).expect("the label row");
        row.chars().collect()
    }

    /// The label is followed by one empty column, and only then the scrollbar's.
    fn assert_gap(cols: u16, rows: u16) {
        let row = label_row(cols, rows);
        let n = cols as usize;
        assert_eq!(row.len(), n, "{row:?}");
        let label_end = row.iter().rposition(|c| *c == 't').expect("rust");
        assert_eq!(label_end, n - 3, "label ends two columns before the edge: {row:?}");
        assert_eq!(row[n - 2], ' ', "{row:?}");
        assert_ne!(row[n - 1], ' ', "the scrollbar is drawn: {row:?}");
    }

    #[test]
    fn the_code_label_leaves_a_gap_before_the_scrollbar_at_80x24() {
        assert_gap(80, 24);
    }

    #[test]
    fn the_code_label_leaves_a_gap_before_the_scrollbar_at_40x12() {
        assert_gap(40, 12);
    }
}
