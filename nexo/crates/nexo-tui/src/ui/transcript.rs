use super::*;

pub(super) fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    app.transcript_area = area;
    if area.height == 0 || area.width == 0 {
        return;
    }
    let text_w = area.width.saturating_sub(1).max(1) as usize;
    let height = area.height as usize;
    app.layout(text_w);
    app.settle(height);
    app.reveal_step();
    let scroll = app.glide(height);
    let theme = app.theme.clone();
    let hovered_head = match app.hover {
        Hover::Head(i) => Some(i),
        _ => None,
    };
    let query = app.search.as_ref().filter(|s| !s.query.is_empty()).map(|s| s.query.to_lowercase());
    let current_match = app.search.as_ref().and_then(|s| s.matches.get(s.current).copied());
    let selected = app.selected;
    let (now, on) = (app.now(), app.fx_on());
    app.fx.block_sel.note(selected, now, on);
    let glide = app.fx.block_sel;
    let selection = app.selection.clone();
    let mut out: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut line = scroll;
    let end = (scroll + height).min(app.total_lines);
    let mut bi = match app.offsets.binary_search(&scroll) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    while line < end && bi < app.blocks.len() {
        let off = app.offsets[bi];
        let next = app.offsets.get(bi + 1).copied().unwrap_or(app.total_lines);
        if line >= next {
            bi += 1;
            continue;
        }
        let n = app.shown(bi, text_w);
        let j = line - off;
        let mut l = if j < n { app.lines(bi, text_w)[j].clone() } else { Line::default() };
        // the first line in sight of a card whose head scrolled away shows the head instead
        if line == scroll && j > 0 && j + 1 < n && sticky(&app.blocks[bi]) {
            l = app.lines(bi, text_w)[0].clone().patch_style(Style::default().bg(theme.bg_alt));
        }
        let t = app.fade(bi);
        if t < 1.0 {
            l = fade_in(l, &theme, t);
            if on && matches!(app.blocks[bi], crate::render::blocks::Block::User { .. }) {
                l = crate::fx_stream::band_in(l, &theme, t);
            }
        }
        // the selected block: a bar down its margin, the whole height of it, and its head lit
        let lvl = glide.level(bi, now);
        if lvl > 0.0 && j < n {
            // the bar and the lit head ease in on the new block and out on the old one
            let bar = crate::motion::fade(&theme, theme.border, theme.accent, lvl);
            l = mark_margin(l, theme.style(bar).add_modifier(Modifier::BOLD));
            if j == 0 {
                l = l.patch_style(if lvl >= 1.0 {
                    theme.selection()
                } else {
                    crate::motion::lit_style(&theme, Style::default(), lvl)
                });
            }
        } else if hovered_head == Some(bi) && j == 0 {
            l = l.patch_style(theme.hover());
        }
        if let Some(q) = &query {
            l = highlight(l, q, theme.highlight(), current_match == Some(line));
        }
        if let Some((from, mut to)) = selection.as_ref().and_then(|s| s.covers(line)) {
            // inside a box the highlight stops at the end of the row, not at the screen edge
            if to == usize::MAX && selection.as_ref().is_some_and(|s| s.region.is_some() && !s.free) {
                to = l.width();
            }
            l = style_cols(l, from, to, theme.highlight());
        }
        // the person's message: its band is filled to the edge here, on screen only, so copying
        // its text never picks up the padding
        if j < n
            && matches!(&app.blocks[bi], crate::render::blocks::Block::User { text, .. } if !crate::render::blocks::is_job_note(text))
        {
            let used = l.width();
            if used < text_w {
                l.spans.push(Span::styled(" ".repeat(text_w - used), l.style));
            }
        }
        out.push(l);
        line += 1;
    }
    f.render_widget(Paragraph::new(Text::from(out)), Rect { width: text_w as u16, ..area });
    app.transcript_bar = None;
    let now = std::time::Instant::now();
    let mut pulse = anim::pulse(app.unseen_at, now);
    // the thumb is lit while the view scrolls, then fades back
    if app.fx_on() {
        let at = app.now();
        app.note_bar(at);
        pulse = pulse.max(crate::fx_chrome::fade_out(app.fx.bar_lit, at, crate::fx_chrome::BAR_MS));
    }
    if app.total_lines > height && area.width > 1 {
        let bar = Bar {
            target: Target::Transcript,
            x: area.x + area.width - 1,
            y: area.y,
            len: area.height,
            content: app.total_lines,
            viewport: height,
            offset: scroll,
        };
        let look = scrollbar::Look { hot: app.bar_hot(Target::Transcript), pulse, track: true };
        scrollbar::draw(f.buffer_mut(), &bar, &theme, look);
        app.transcript_bar = Some(bar);
    }
    app.pill = Rect::default();
    let unseen = app.unseen();
    if unseen > 0 && area.height > 1 {
        let pill = format!(" ↓ {unseen} new ");
        let w = width(&pill) as u16;
        if area.width > w + 2 {
            let r = Rect::new(area.x + area.width - w - 2, area.y + area.height - 1, w, 1);
            clear_bg(f, r, &theme);
            f.render_widget(Paragraph::new(Span::styled(pill, pill_style(&theme, app.hover == Hover::Pill, pulse))), r);
            app.pill = r;
        }
    }
    app.evict_far(scroll, height);
}

/// The "new below" pill: lit under the pointer, beating while news arrives.
fn pill_style(theme: &Theme, hot: bool, pulse: f32) -> Style {
    let s = theme.bar().add_modifier(Modifier::BOLD);
    if theme.mono {
        return if hot { s.add_modifier(Modifier::UNDERLINED) } else { s };
    }
    if hot {
        return s.bg(theme.accent).fg(theme.bg);
    }
    s.bg(mix(theme.bar_bg, theme.accent, pulse * 0.6))
}

/// The line with a bar in its first column, when that column is the margin.
fn mark_margin(line: Line<'static>, style: Style) -> Line<'static> {
    let Line { style: line_style, alignment, mut spans } = line;
    if let Some(first) = spans.first_mut() {
        if let Some(rest) = first.content.strip_prefix(' ') {
            let rest = rest.to_string();
            first.content = rest.into();
            spans.insert(0, Span::styled("┃", style));
        }
    }
    Line { style: line_style, alignment, spans }
}

/// Blocks whose first line names them, kept in sight while the rest scrolls past.
fn sticky(b: &crate::render::blocks::Block) -> bool {
    use crate::render::blocks::Block;
    matches!(b, Block::Tool { .. } | Block::Reasoning { .. } | Block::Shell { .. })
}

/// A line `t` (0..1) of the way from the ground to its own colours.
pub(super) fn fade_in(line: Line<'static>, theme: &Theme, t: f32) -> Line<'static> {
    let eased = 1.0 - (1.0 - t).powi(2);
    let spans = line
        .spans
        .into_iter()
        .map(|s| {
            let fg = s.style.fg.or(line.style.fg).unwrap_or(theme.fg);
            let style = s.style.fg(mix(theme.bg, fg, eased));
            Span::styled(s.content, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style)
}

/// Marks every occurrence of `q` (lower-case) in the line.
pub(super) fn highlight(line: Line<'static>, q: &str, style: Style, current: bool) -> Line<'static> {
    let mut spans = Vec::new();
    for s in line.spans {
        let text = s.content.to_string();
        let lower = text.to_lowercase();
        // offsets found in `lower` cut `text` only where both have the same character boundaries
        let aligned = lower.len() == text.len() && lower.char_indices().all(|(i, _)| text.is_char_boundary(i));
        if !aligned || !lower.contains(q) {
            if lower.contains(q) {
                spans.push(Span::styled(text, s.style.patch(style)));
            } else {
                spans.push(s);
            }
            continue;
        }
        let mut pos = 0;
        while let Some(i) = lower[pos..].find(q) {
            let start = pos + i;
            if start > pos {
                spans.push(Span::styled(text[pos..start].to_string(), s.style));
            }
            let st = if current { style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { style };
            spans.push(Span::styled(text[start..start + q.len()].to_string(), st));
            pos = start + q.len();
        }
        if pos < text.len() {
            spans.push(Span::styled(text[pos..].to_string(), s.style));
        }
    }
    Line::from(spans).style(line.style)
}
