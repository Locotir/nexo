use super::*;

/// The `/context` · `/usage` window: it grows open from the middle, a tab
/// row on top, `[✗]` on its border, the figures of the tab fill in.
pub(super) fn draw_info(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(Modal::Info(info)) = &app.modal else { return };
    if area.width < 30 || area.height < 6 {
        let theme = app.theme.clone();
        return draw_too_narrow(f, &theme, area, area.width < 30);
    }
    let (tab, grow, fill, hover, period) = (info.tab, info.grow(), info.fill(), info.hover, info.period);
    // a reading that came in changes what the usage tab holds: the window is sized again
    let version = (period, app.insights.version);
    if matches!(&app.modal, Some(Modal::Info(i)) if i.measured != Some(version)) {
        app.dialog_size.modal.remeasure();
        if let Some(Modal::Info(i)) = &mut app.modal {
            i.measured = Some(version);
        }
    }
    let theme = app.theme.clone();
    let inner_w = overlay::window_rect(area, 0).width.saturating_sub(4) as usize;
    let key = app.modal.as_ref().map_or(0, crate::dialog_size::modal_key);
    let body = overlay::lines(app, tab, inner_w, fill, period);
    // as tall as the tallest tab, and it keeps the size it took when it opened:
    // neither a tab change nor the figures filling in resize the window
    let tallest =
        (0..overlay::TABS.len()).map(|t| overlay::lines(app, t, inner_w, 1.0, period).len()).max().unwrap_or(0);
    let asked = overlay::window_rect(area, tallest);
    let (fw, fh) = app.dialog_size.modal.keep(key, f.area(), area, || (asked.width, asked.height));
    let full = Rect::new(area.x + (area.width - fw) / 2, area.y + (area.height - fh) / 2, fw, fh);
    let w = ((full.width as f64) * (0.7 + 0.3 * grow)).round() as u16;
    let h = (3.0 + (full.height.saturating_sub(3)) as f64 * grow).round() as u16;
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    app.dialog_size.modal.drawn = r;
    clear_card_beside(f, card_rect(app), full, &theme);
    clear_bg(f, r, &theme);
    let block = Box_::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.style(theme.border))
        .title(Span::styled(format!(" {} ", overlay::TABS[tab].to_lowercase()), theme.bold(theme.accent)))
        .style(Style::default().bg(theme.modal_bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let close = Rect::new(r.x + r.width.saturating_sub(6), r.y, 3, 1);
    let close_style = if hover { theme.bold(theme.error).add_modifier(Modifier::REVERSED) } else { theme.dim() };
    f.render_widget(Paragraph::new(Span::styled("[✗]", close_style)), close);
    let mut tabs = Vec::new();
    if inner.height >= 4 {
        let row = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1);
        let (spans, at) = overlay::tab_row(tab, &theme);
        tabs = at.iter().map(|(x, wl)| Rect::new(row.x + x, row.y, *wl, 1)).collect();
        f.render_widget(Paragraph::new(Line::from(spans)), row);
        let rule = "─".repeat(row.width as usize);
        f.render_widget(Paragraph::new(Span::styled(rule, theme.dim2())), Rect { y: row.y + 1, ..row });
    }
    let body_h = inner.height.saturating_sub(3) as usize;
    let Some(Modal::Info(info)) = &mut app.modal else { return };
    info.scroll = info.scroll.min(body.len().saturating_sub(body_h));
    let scroll = info.scroll;
    info.window = r;
    info.close = close;
    info.tabs = tabs;
    let body_len = body.len();
    if body_h > 0 {
        let body_area = Rect::new(inner.x + 1, inner.y + 2, inner.width.saturating_sub(2), body_h as u16);
        let more = if scroll + body_h < body.len() { "  ↓" } else { "" };
        let shown: Vec<Line<'static>> = body.into_iter().skip(scroll).take(body_h).collect();
        f.render_widget(Paragraph::new(Text::from(shown)), body_area);
        let days = if tab == 1 { " · d/w/m period" } else { "" };
        let foot = Rect::new(inner.x + 1, inner.y + inner.height - 1, inner.width.saturating_sub(2), 1);
        // the longest wording that fits: closing the window is the last thing to lose
        let hint = [
            format!("Tab switch · ↑↓ scroll{days} · Esc close{more}"),
            format!("Tab · ↑↓{} · Esc close{more}", if tab == 1 { " · d/w/m period" } else { "" }),
            format!("Esc close{more}"),
        ]
        .into_iter()
        .find(|h| width(h) <= foot.width as usize)
        .unwrap_or_else(|| "Esc".to_string());
        f.render_widget(Paragraph::new(Span::styled(truncate(&hint, foot.width as usize), theme.dim2())), foot);
        modal_scrollbar(f, app, r, inner.y + 2, body_h, body_len, scroll, 0);
    }
}

/// `/panel`: the agent sessions as cards.
pub(super) fn draw_agents(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let (now, fx) = (app.now(), app.fx_on());
    if let Some(Modal::Agents(v)) = &mut app.modal {
        v.draw(f, area, &theme, now, fx);
        app.dialog_size.modal.drawn = v.window;
    }
}
