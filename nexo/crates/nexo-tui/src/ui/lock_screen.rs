use super::*;

/// The draft row while no model works: it cannot be typed into, it says what to do.
pub(super) fn draw_lock_input(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width < 3 {
        return;
    }
    let theme = &app.theme;
    let text = truncate(&format!("{LOCK_TITLE} · press Enter"), area.width.saturating_sub(2) as usize);
    let line = Line::from(vec![Span::styled("› ", theme.bold(theme.accent)), Span::styled(text, theme.dim())]);
    f.render_widget(Paragraph::new(line), Rect { height: 1, ..area });
}

/// How long the welcome card takes to slide away once a model works.
pub const UNLOCK_MS: u64 = 280;

/// The text of the card shown while no model works yet.
pub const LOCK_TITLE: &str = "Set up a model to start";

/// The welcome card of a locked home, centred in `area`. `away` (0 to 1) slides it up and fades it.
pub(super) fn draw_lock(f: &mut Frame, app: &App, area: Rect, away: f64) {
    let theme = &app.theme;
    let lines: Vec<(String, Style)> = vec![
        ("n e x o".to_string(), theme.bold(theme.accent)),
        (String::new(), Style::default()),
        (LOCK_TITLE.to_string(), if away > 0.5 { theme.dim() } else { theme.bold(theme.fg) }),
    ];
    let mut lines = lines;
    if let Some(why) = app.unreachable_note() {
        lines.push((String::new(), Style::default()));
        lines.extend(why.into_iter().map(|l| (l, Style::default().fg(theme.fg))));
    }
    lines.push((String::new(), Style::default()));
    lines.push(("Enter setup · /help commands · Ctrl+D quit".to_string(), theme.dim()));
    let h = lines.len() as u16;
    let came = if away == 0.0 { app.welcome_at(app.now()) } else { None };
    let slide = (away * 4.0).round() as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    crate::fx_stars::draw(f, app, area, top, slide, away);
    for (i, (text, style)) in lines.iter().enumerate() {
        let Some(y) = (top + i as u16).checked_sub(slide) else { continue };
        if y < area.y || y >= area.bottom() || text.is_empty() {
            continue;
        }
        let w = (crate::render::wrap::width(text) as u16).min(area.width);
        let r = Rect { x: area.x + (area.width - w) / 2, y, width: w, height: 1 };
        // each line fades in from the ground, a little after the one above
        let style = match came {
            Some(at) => {
                let k = crate::fx_chrome::welcome_level(i, h as usize, at, app.now());
                let to = style.fg.unwrap_or(theme.fg);
                style.fg(crate::motion::fade(theme, theme.bg, to, k))
            }
            None => *style,
        };
        f.render_widget(Paragraph::new(Line::from(Span::styled(truncate(text, w as usize), style))), r);
    }
}
