use super::*;

pub(super) fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
    app.picker_rows.clear();
    app.modal_bar = None;
    if matches!(app.modal, Some(Modal::Agents(_))) {
        return draw_agents(f, app, area);
    }
    if matches!(app.modal, Some(Modal::Info(_))) {
        return draw_info(f, app, area);
    }
    if matches!(app.modal, Some(Modal::Question(_))) {
        return draw_question(f, app, area);
    }
    let theme = app.theme.clone();
    let pop = app.modal_pop();
    let hovered = match app.hover {
        Hover::Picker(i) => Some(i),
        _ => None,
    };
    let (now, on) = (app.now(), app.fx_on());
    let psel = match &app.modal {
        Some(Modal::Picker(p)) => Some(p.selected),
        _ => None,
    };
    app.fx.pick_sel.note(psel, now, on);
    let glide = app.fx.pick_sel;
    let key = app.modal.as_ref().map_or(0, crate::dialog_size::modal_key);
    let gfx = matches!(app.modal, Some(Modal::Goal(_))).then(|| app.fx.goal.clone()).unwrap_or_default();
    let quiet = app.quiet();
    let card = card_rect(app);
    let Some(modal) = &mut app.modal else { return };
    if area.width < 12 || area.height < 5 {
        return draw_too_narrow(f, &theme, area, area.width < 12);
    }
    let cap = match modal {
        Modal::Todo(_) => 76,
        Modal::Sent(_) => 120,
        _ => 96,
    };
    let w = (area.width.saturating_sub(4)).min(cap);
    let max_h = area.height.saturating_sub(2);
    // laid out for the width it has now, so nothing is cut while it pops open
    let inner_w = pop_rect(Rect::new(0, 0, w, 3), pop).width.saturating_sub(4) as usize;
    let mut list_at: Option<usize> = None;
    // the jobs window: the line each job starts at, and the one to keep in sight
    let mut job_rows: Vec<usize> = Vec::new();
    let mut job_at = 0;
    // the todo window: where each task starts and how long it is, the selected one, the finished ones' header
    let mut todo_rows: Vec<(usize, usize)> = Vec::new();
    let mut todo_sel = (0usize, 1usize);
    let mut todo_head: Option<usize> = None;
    // the rules of a permission: pinned above the buttons, never scrolled out of sight
    let mut pinned: Vec<Line<'static>> = Vec::new();
    // the goal window: which pinned row is its Resume button
    let mut goal_button: Option<usize> = None;
    let (title, body, buttons): (String, Vec<Line<'static>>, Option<Buttons>) = match modal {
        Modal::Permission(p) => {
            let btn_h: u16 = if max_h < 9 { 1 } else { 2 };
            // the command or diff keeps at least two lines; the rules take what is left, whole or not at all
            let room = max_h.saturating_sub(2 + btn_h + 2) as usize;
            let all = Modal::permission_rules(&p.req, &theme, true);
            let n_rules = all.len().saturating_sub(1);
            // quiet: the rules wait behind the first `a`; the first screen is the question and the three keys
            // with no room to show a rule there is nothing to open: `a` then answers at once, as in full
            p.rules_gated = quiet && room > 0;
            pinned = if quiet && !p.rules_open {
                Vec::new()
            } else if all.len() < room {
                // a blank line sets the rules apart from the command when there is room for it
                std::iter::once(Line::default()).chain(all).collect()
            } else if all.len() == room {
                all
            } else {
                Modal::permission_rules(&p.req, &theme, false).into_iter().take(room).collect()
            };
            p.rules_shown = if pinned.len() > n_rules { n_rules } else { pinned.len() };
            let mut lines = Modal::permission_lines_with(&p.req, inner_w, &theme, false, false);
            if lines.len() as u16 + pinned.len() as u16 + 2 + btn_h > max_h {
                lines = Modal::permission_lines_with(&p.req, inner_w, &theme, true, false);
            }
            let b = crate::modal::permission_buttons(inner_w.saturating_sub(1));
            (crate::modal::permission_title(p.req.agent.as_deref(), &p.req.tool, inner_w), lines, Some(b))
        }
        Modal::Picker(p) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("› ", theme.style(theme.accent)),
                    Span::raw(p.query.clone()),
                    Span::styled("▏", theme.dim()),
                ]),
                Line::default(),
            ];
            if p.ranked.is_empty() {
                lines.push(Line::from(Span::styled("no match", theme.dim())));
            }
            for (i, &idx) in p.ranked.iter().enumerate() {
                let it = &p.items[idx];
                let armed = i == p.selected && p.armed.as_deref() == Some(it.value.as_str());
                let label = truncate(&it.label, inner_w.saturating_sub(2));
                let hint = if armed { "Del again delete · Esc keep" } else { it.hint.as_str() };
                let hint = truncate(hint, inner_w.saturating_sub(width(&label) + 4));
                let pad = inner_w.saturating_sub(width(&label) + width(&hint) + 2);
                let style = if armed {
                    // the row about to go: it turns to the error colour and breathes
                    let on = !app.opts.animations || (app.spinner / 3).is_multiple_of(2);
                    Style::default().fg(theme.bg).bg(theme.error).add_modifier(if on {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    })
                } else if glide.level(i, now) >= 1.0 {
                    theme.selection().add_modifier(Modifier::BOLD)
                } else if glide.level(i, now) > 0.0 {
                    crate::motion::lit_style(&theme, Style::default().bg(theme.modal_bg), glide.level(i, now))
                } else if hovered == Some(i) {
                    theme.hover()
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label}"), style),
                    Span::styled(" ".repeat(pad), style),
                    Span::styled(hint, style.patch(theme.dim())),
                    Span::styled(" ", style),
                ]));
            }
            let title = if p.kind == PickKind::Session && !p.items.is_empty() {
                format!(" {} · {} · Del delete ", p.title, p.items.len())
            } else {
                format!(" {} ", p.title)
            };
            (title, lines, None)
        }
        Modal::Help { .. } => (" help ".to_string(), Modal::help_lines(&theme, inner_w), None),
        Modal::Jobs(v) => {
            let (lines, starts, at) = v.lines(inner_w, &theme);
            job_rows = starts;
            job_at = at;
            pinned = vec![Line::default(), v.keys(inner_w, &theme)];
            (v.title(), lines, None)
        }
        Modal::Mcp(v) => {
            let (lines, starts, at) = v.lines(inner_w, &theme);
            job_rows = starts;
            job_at = at;
            pinned = vec![Line::default(), v.keys(inner_w, &theme)];
            (v.title(), lines, None)
        }
        Modal::Todo(v) => {
            let spinner = app.opts.animations.then_some(app.spinner);
            let pulse = if spinner.is_some() { ((app.spinner as f32 * 0.21).sin() + 1.0) / 2.0 } else { 0.0 };
            let b = v.body(inner_w, &theme, spinner, pulse, now);
            pinned = vec![Line::default(), v.keys(inner_w, &theme)];
            todo_sel = b.starts.get(v.selected).copied().unwrap_or((0, 1));
            todo_rows = b.starts;
            todo_head = b.done_head;
            (v.title(), b.lines, None)
        }
        Modal::Goal(v) => {
            let (rows, button) = v.pinned(inner_w, &theme);
            pinned = rows;
            goal_button = button;
            (v.title(), v.lines_fx(inner_w, &theme, nexo_core::mailbox::now_ms(), &gfx, now), None)
        }
        Modal::Sent(v) => {
            let lines = v.layout(inner_w, &theme);
            pinned = v.pinned(inner_w, &theme);
            (v.title(), lines, None)
        }
        Modal::Trust(t) => {
            let (lines, rows) = trust_rows(t, inner_w, &theme);
            pinned = rows;
            (" trust this folder ".to_string(), lines, None)
        }
        Modal::Info(_) | Modal::Agents(_) | Modal::Question(_) => return,
        Modal::Setup(s) => {
            let (lines, at) = s.lines(inner_w, &theme, app.spinner, max_h < 12);
            list_at = at;
            (s.title(), lines, None)
        }
    };
    // the gap above the buttons goes first when the screen is short
    let button_h = match (buttons.is_some(), max_h < 9) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => 2,
    };
    let pinned_h = pinned.len() as u16;
    // the size it took when it opened: a cursor, a filter or a longer line never stretches it
    // the first setup list asks for the room it would take with every fold open: what is opened
    // later is not cut, and the own-server row never ends up behind a scrollbar
    let open_extra = match modal {
        Modal::Setup(s) if s.step == crate::modal::setup::Step::Provider => {
            s.rows_if_open().saturating_sub(s.list.items.len())
        }
        _ => 0,
    };
    let want_h = ((body.len() + open_extra) as u16 + pinned_h + 2 + button_h).min(max_h).max(5);
    let room = Rect::new(0, 0, area.width, max_h.max(5));
    let (w, h) = app.dialog_size.modal.keep(key, f.area(), room, || (w, want_h));
    let h = h.max(5).min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let r = pop_rect(Rect::new(x, y, w, h), pop);
    app.dialog_size.modal.drawn = r;
    clear_card_beside(f, card, Rect::new(x, y, w, h), &theme);
    clear_margin(f, r, area, &theme);
    clear_bg(f, r, &theme);
    let block = Box_::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(pop_style(&theme, theme.border, pop))
        .title(Span::styled(title, theme.bold(theme.accent)))
        .style(Style::default().bg(theme.modal_bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let body_h = inner.height.saturating_sub(button_h + pinned_h) as usize;
    let scroll = match modal {
        Modal::Permission(p) => {
            p.scroll = p.scroll.min(body.len().saturating_sub(body_h));
            p.scroll
        }
        Modal::Help { scroll } => {
            *scroll = (*scroll).min(body.len().saturating_sub(body_h));
            *scroll
        }
        Modal::Jobs(v) => {
            if !v.detail {
                // the selected job and its lines stay in view
                let end = job_at + 1 + 2;
                v.scroll = end.saturating_sub(body_h);
            }
            v.scroll = v.scroll.min(body.len().saturating_sub(body_h));
            v.scroll
        }
        Modal::Mcp(v) => {
            // the selected server and its two lines stay in view
            let end = job_at + 2;
            v.scroll = end.saturating_sub(body_h);
            v.scroll = v.scroll.min(body.len().saturating_sub(body_h));
            v.scroll
        }
        Modal::Todo(v) => {
            // the selected task stays in view, headers above it too when there is room
            let (at, len) = todo_sel;
            let end = at + len;
            let top = match v.selected.checked_sub(1).and_then(|p| todo_rows.get(p)) {
                Some(&(s, l)) => s + l,
                None => 0,
            };
            v.scroll = end.saturating_sub(body_h).min(top.min(at));
            v.scroll = v.scroll.min(body.len().saturating_sub(body_h));
            v.scroll
        }
        Modal::Goal(v) => {
            v.scroll = v.scroll.min(body.len().saturating_sub(body_h));
            v.scroll
        }
        Modal::Sent(v) => {
            v.scroll = v.scroll.min(body.len().saturating_sub(body_h));
            v.scroll
        }
        Modal::Trust(_) => 0,
        Modal::Info(_) | Modal::Agents(_) | Modal::Question(_) => 0,
        Modal::Picker(p) => {
            // keep the selection in view: two header lines, then the rows
            let row = p.selected + 2;
            if row >= body_h {
                row + 1 - body_h
            } else {
                0
            }
        }
        Modal::Setup(s) => {
            // the list's selection in view, else the end (where the spinner and errors are)
            let row = match list_at {
                Some(at) => at + s.list.selected,
                // the line being typed, not the error under it
                None => crate::modal::setup::Setup::prompt_row(&body)
                    .or_else(|| crate::modal::setup::Setup::selected_row(&body))
                    .unwrap_or(body.len().saturating_sub(1)),
            };
            if row >= body_h {
                row + 1 - body_h
            } else {
                0
            }
        }
    };
    let body_area = Rect { height: body_h as u16, x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let shown: Vec<Line<'static>> = body.iter().skip(scroll).take(body_h).cloned().collect();
    f.render_widget(Paragraph::new(Text::from(shown)), body_area);
    let rows = match modal {
        Modal::Picker(p) => Some((2, p.ranked.len())),
        Modal::Setup(s) => list_at.map(|at| (at, s.list.ranked.len())),
        _ => None,
    };
    // a bar on the right border, for what scrolls: the text, or a list (moving its selection)
    let bar = match modal {
        Modal::Permission(_)
        | Modal::Help { .. }
        | Modal::Jobs(_)
        | Modal::Todo(_)
        | Modal::Mcp(_)
        | Modal::Goal(_)
        | Modal::Sent(_)
        | Modal::Trust(_) => Some(0),
        Modal::Picker(_) | Modal::Setup(_) => rows.map(|(first, _)| first),
        Modal::Info(_) | Modal::Agents(_) | Modal::Question(_) => None,
    };
    let bar = bar.map(|first| (r, body_area.y, body_h, body.len(), scroll, first));
    if let (Modal::Jobs(v), false) = (&*modal, job_rows.is_empty()) {
        if !v.detail {
            for (i, &line) in job_rows.iter().enumerate() {
                if line >= scroll && line < scroll + body_h {
                    app.picker_rows
                        .push((Rect::new(body_area.x, body_area.y + (line - scroll) as u16, body_area.width, 1), i));
                }
            }
        }
    }
    if let (Modal::Mcp(_), false) = (&*modal, job_rows.is_empty()) {
        for (i, &line) in job_rows.iter().enumerate() {
            for l in line..line + 2 {
                if l >= scroll && l < scroll + body_h {
                    app.picker_rows
                        .push((Rect::new(body_area.x, body_area.y + (l - scroll) as u16, body_area.width, 1), i));
                }
            }
        }
    }
    if matches!(modal, Modal::Todo(_)) {
        let visible = |line: usize| line >= scroll && line < scroll + body_h;
        for (i, &(at, len)) in todo_rows.iter().enumerate() {
            for line in at..at + len {
                if visible(line) {
                    app.picker_rows
                        .push((Rect::new(body_area.x, body_area.y + (line - scroll) as u16, body_area.width, 1), i));
                }
            }
        }
        if let Some(line) = todo_head.filter(|&l| visible(l)) {
            app.picker_rows.push((
                Rect::new(body_area.x, body_area.y + (line - scroll) as u16, body_area.width, 1),
                todo_rows.len(),
            ));
        }
    }
    if let Some((first, n)) = rows {
        for i in 0..n {
            let row = i + first;
            if row >= scroll && row < scroll + body_h {
                app.picker_rows
                    .push((Rect::new(body_area.x, body_area.y + (row - scroll) as u16, body_area.width, 1), i));
            }
        }
    }
    if pinned_h > 0 {
        let py = inner.y + inner.height - button_h - pinned_h;
        if matches!(modal, Modal::Trust(_)) {
            for i in 0..pinned_h {
                app.picker_rows.push((Rect::new(inner.x + 1, py + i, inner.width.saturating_sub(2), 1), i as usize));
            }
        }
        if let (Modal::Goal(_), Some(i)) = (&*modal, goal_button) {
            app.picker_rows.push((Rect::new(inner.x + 1, py + i as u16, inner.width.saturating_sub(2), 1), 0));
        }
        f.render_widget(
            Paragraph::new(Text::from(pinned)),
            Rect::new(inner.x + 1, py, inner.width.saturating_sub(2), pinned_h),
        );
    }
    if let (Some(bs), Modal::Permission(p)) = (buttons, modal) {
        let y = inner.y + inner.height - 1;
        let mut x = inner.x + 1;
        p.buttons.clear();
        let mut spans = Vec::new();
        let (bs, gap) = bs;
        for (label, key) in bs {
            let style = match key {
                'y' => theme.bold(theme.success),
                'a' => theme.bold(theme.accent),
                _ => theme.bold(theme.error),
            };
            let wl = width(&label) as u16;
            p.buttons.push((Rect::new(x, y, wl, 1), key));
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" ".repeat(gap)));
            x += wl + gap as u16;
        }
        f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1));
    }
    if let Some((r, y, h, content, offset, first)) = bar {
        modal_scrollbar(f, app, r, y, h, content, offset, first);
    }
}

/// The trust question: the text, and the two rows pinned under it (so a short screen never cuts them).
fn trust_rows(t: &crate::modal::Trust, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let mut lines = wrap_plain("Do you trust the files in this folder?", w, theme.bold(theme.accent), &[]);
    lines.push(Line::default());
    lines.extend(wrap_plain(&t.path, w, theme.bold(theme.fg), &[]));
    lines.push(Line::default());
    lines.extend(wrap_plain(&t.enables, w, Style::default(), &[]));
    if let Some(warning) = &t.warning {
        lines.extend(wrap_plain(warning, w, theme.bold(theme.warn), &[]));
    }
    // cut first when the screen is short
    lines.push(Line::default());
    let rows = crate::modal::Trust::ROWS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let on = i == t.selected;
            let key = if i == 0 { "y" } else { "n" };
            let text = format!("{} {label}", if on { "❯" } else { " " });
            let pad = w.saturating_sub(width(&text) + 1);
            let style = if on { theme.chosen() } else { Style::default() };
            Line::from(vec![
                Span::styled(text, style),
                Span::styled(" ".repeat(pad), style),
                Span::styled(key, if on { style } else { theme.dim() }),
            ])
        })
        .collect();
    (lines, rows)
}

/// A dialog's rectangle `r` while it pops open (`t` 0..1): smaller, around the same middle.
pub fn pop_rect(r: Rect, t: f64) -> Rect {
    if t >= 1.0 {
        return r;
    }
    let s = 0.85 + 0.15 * t.clamp(0.0, 1.0);
    let w = ((r.width as f64 * s).round() as u16).clamp(r.width.min(3), r.width);
    let h = ((r.height as f64 * s).round() as u16).clamp(r.height.min(3), r.height);
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}

/// A border that comes out of the dialog's ground while it pops open.
pub(super) fn pop_style(theme: &Theme, colour: ratatui::style::Color, t: f64) -> Style {
    if t >= 1.0 || theme.mono {
        return theme.style(colour);
    }
    theme.style(mix(theme.modal_bg, colour, t as f32))
}

/// The scrollbar of a dialog, on its right border beside the `len` rows of its body
/// from `y`, and remembered for the mouse. `first`: the body line a list starts at.
#[allow(clippy::too_many_arguments)]
pub(super) fn modal_scrollbar(
    f: &mut Frame,
    app: &mut App,
    r: Rect,
    y: u16,
    len: usize,
    content: usize,
    offset: usize,
    first: usize,
) {
    if content <= len || len == 0 || r.width < 3 {
        return;
    }
    let bar = Bar { target: Target::Modal, x: r.x + r.width - 1, y, len: len as u16, content, viewport: len, offset };
    let look = scrollbar::Look { hot: app.bar_hot(Target::Modal), pulse: 0.0, track: false };
    let theme = app.theme.clone();
    scrollbar::draw(f.buffer_mut(), &bar, &theme, look);
    app.modal_bar = Some(bar);
    app.modal_list_first = first;
}
