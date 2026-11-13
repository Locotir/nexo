use super::*;

/// The rows of a question: each option (its description under it), then the row to write
/// one's own answer. With the line each row starts at. The row the cursor is on is a band across
/// the dialog, and an answer being written wraps onto as many lines as it needs (its last
/// [`TYPED_LINES`] show), so its end is always in view.
pub fn question_rows(a: &crate::modal::Ask, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines = Vec::new();
    let mut starts = Vec::new();
    let cursor = |i: usize| if i == a.selected { "❯ " } else { "  " };
    // the chosen row filled to the full width, so the band reaches the border
    let band = |spans: Vec<Span<'static>>, on: bool| {
        if !on {
            return Line::from(spans);
        }
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let mut spans: Vec<Span<'static>> =
            spans.into_iter().map(|s| Span::styled(s.content, s.style.patch(theme.chosen()))).collect();
        spans.push(Span::styled(" ".repeat(w.saturating_sub(used)), theme.chosen()));
        Line::from(spans)
    };
    for (i, o) in a.q.options.iter().enumerate() {
        let on = i == a.selected;
        let tick = if !a.q.multi {
            String::new()
        } else if a.picked[i] {
            "[x] ".to_string()
        } else {
            "[ ] ".to_string()
        };
        starts.push(lines.len());
        let head = format!("{}{}. {tick}", cursor(i), i + 1);
        // labels and descriptions are markdown too: **bold** and `code` read as such, never as marks
        let mut row = vec![Span::raw(head.clone())];
        row.extend(inline_md(&o.label, w.saturating_sub(width(&head)), theme, Style::default()));
        lines.push(band(row, on));
        if !o.description.is_empty() {
            let pad = " ".repeat(width(&head));
            for l in crate::render::markdown::render(&o.description, w.saturating_sub(pad.len()), theme) {
                let mut spans = vec![Span::raw(pad.clone())];
                spans.extend(l.spans.into_iter().map(|s| tint_plain(s, theme.dim())));
                lines.push(Line::from(spans));
            }
        }
    }
    let n = a.q.options.len();
    let on = a.selected == n;
    let head = format!("{}{}. ", cursor(n), n + 1);
    starts.push(lines.len());
    if a.typing || !a.other.is_empty() {
        // room for the caret at the end of the last line
        let room = w.saturating_sub(width(&head) + 1).max(1);
        let mut pieces = hard_wrap(&a.other, room);
        let hidden = pieces.len().saturating_sub(TYPED_LINES);
        pieces.drain(..hidden);
        let last = pieces.len() - 1;
        for (k, piece) in pieces.into_iter().enumerate() {
            let lead = if k == 0 { head.clone() } else { " ".repeat(width(&head)) };
            let lead = if k == 0 && hidden > 0 { lead.replacen(". ", ".…", 1) } else { lead };
            let mut spans = vec![Span::raw(lead), Span::raw(piece)];
            if a.typing && k == last {
                spans.push(Span::styled("▏", theme.style(theme.accent)));
            }
            lines.push(band(spans, on));
        }
    } else {
        let hint = if a.q.plan.is_some() { "Tell the model what to change…" } else { "Something else…" };
        let spans = vec![Span::raw(head), Span::styled(hint.to_string(), theme.dim())];
        lines.push(band(spans, on));
    }
    (lines, starts)
}

/// A title shown as a filled tag in `colour`, the dialog's background as its ink.
fn badge(theme: &Theme, colour: ratatui::style::Color) -> Style {
    if theme.mono {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(theme.modal_bg).bg(colour).add_modifier(Modifier::BOLD)
    }
}

/// The most lines an answer being written takes in a question; past it, its start scrolls away.
const TYPED_LINES: usize = 5;

/// `s` cut into pieces at most `room` columns wide, keeping every character (spaces included,
/// so the caret sits where the next one will go). Never empty.
fn hard_wrap(s: &str, room: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > room && used > 0 {
            out.push(String::new());
            used = 0;
        }
        out.last_mut().expect("never empty").push(c);
        used += cw;
    }
    out
}

/// A question from the model: the question (and the plan, as markdown) scrolls on top, the
/// options stay at the bottom.
pub(super) fn draw_question(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let pop = app.modal_pop();
    let key = app.modal.as_ref().map_or(0, crate::dialog_size::modal_key);
    let card = card_rect(app);
    let Some(Modal::Question(a)) = &mut app.modal else { return };
    if area.width < 20 || area.height < 6 {
        return draw_too_narrow(f, &theme, area, area.width < 20);
    }
    let w = (area.width.saturating_sub(4)).min(100);
    let inner_w = w.saturating_sub(4) as usize;
    let mut body = Vec::new();
    if let Some(plan) = &a.q.plan {
        body.extend(crate::render::markdown::render(plan, inner_w, &theme));
        body.push(Line::default());
        body.push(Line::from(Span::styled("─".repeat(inner_w), theme.dim2())));
    }
    // the question is markdown as well: a heading or a list in it reads as one, not as # and -
    for l in crate::render::markdown::render(&a.q.question, inner_w, &theme) {
        body.push(Line::from(l.spans.into_iter().map(|s| tint_plain(s, theme.bold(theme.fg))).collect::<Vec<_>>()));
    }
    let (rows, starts) = question_rows(a, inner_w, &theme);
    let a_selected = a.selected;
    // a short area is used whole, so the options and a line of the question fit
    let max_h = area.height.saturating_sub(if area.height < 10 { 0 } else { 2 });
    let rows_h = rows.len() as u16;
    // the size it took when it opened: moving the cursor (a description, a typed answer) never resizes it
    let want_h = (body.len() as u16 + 1 + rows_h + 2).min(max_h);
    let (w, h) = app.dialog_size.modal.keep(key, f.area(), Rect::new(0, 0, area.width, max_h), || (w, want_h));
    let still = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let r = pop_rect(still, pop);
    app.dialog_size.modal.drawn = r;
    clear_card_beside(f, card, still, &theme);
    clear_bg(f, r, &theme);
    let title = if a.q.plan.is_some() { " plan ".to_string() } else { format!(" {} ", a.q.header.to_lowercase()) };
    // a plan wears the colour of plan mode; any other question the warning colour
    let frame_c = if a.q.plan.is_some() { theme.accent2 } else { theme.warn };
    let block = Box_::default()
        .borders(Borders::ALL)
        // a question waits for the person: a heavier frame in the warning colour, so it is not missed
        .border_type(BorderType::Thick)
        .border_style(pop_style(&theme, frame_c, pop))
        .title(Span::styled(title, badge(&theme, frame_c)))
        .style(Style::default().bg(theme.modal_bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    // the options always show; the question and the plan get what is left
    let rows_h = rows_h.min(inner.height);
    let body_h = inner.height.saturating_sub(rows_h + 1) as usize;
    a.scroll = a.scroll.min(body.len().saturating_sub(body_h));
    let scroll = a.scroll;
    let x = inner.x + 1;
    let iw = inner.width.saturating_sub(2);
    let body_len = body.len();
    if body_h > 0 {
        let more = scroll + body_h < body.len();
        let shown: Vec<Line<'static>> = body.into_iter().skip(scroll).take(body_h).collect();
        f.render_widget(Paragraph::new(Text::from(shown)), Rect::new(x, inner.y, iw, body_h as u16));
        if more {
            let tag = " PgDn ↓ ";
            let tx = r.x + r.width.saturating_sub(width(tag) as u16 + 2);
            f.render_widget(Paragraph::new(Span::styled(tag, theme.dim())), Rect::new(tx, r.y + r.height - 1, 8, 1));
        }
    }
    let ry = inner.y + inner.height - rows_h;
    let ends: Vec<usize> = starts.iter().skip(1).copied().chain([rows.len()]).collect();
    // short screen: the options scroll so the chosen one stays whole in view
    let off = ends.get(a_selected).map_or(0, |e| e.saturating_sub(rows_h as usize));
    let shown_rows: Vec<Line<'static>> = rows.iter().skip(off).take(rows_h as usize).cloned().collect();
    f.render_widget(Paragraph::new(Text::from(shown_rows)), Rect::new(x, ry, iw, rows_h));
    for (i, (s, e)) in starts.iter().zip(ends).enumerate() {
        if e > off && *s < off + rows_h as usize {
            let top = (*s).max(off) - off;
            let hgt = (e.min(off + rows_h as usize) - (*s).max(off)) as u16;
            app.picker_rows.push((Rect::new(x, ry + top as u16, iw, hgt), i));
        }
    }
    modal_scrollbar(f, app, r, inner.y, body_h, body_len, scroll, 0);
}
