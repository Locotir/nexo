//! Drawing the title bar over the transcript.

use super::*;

/// " nexo", and " # root" after it when nexo runs as root, so that is never out of sight.
pub(super) const ROOT_MARK: &str = " # root";
/// Shown after the brand while a newer nexo waits on `main`.
pub(super) const UPDATE_MARK: &str = "  ⬆ update ";

pub(super) fn brand_width(app: &App) -> usize {
    5 + if app.root { width(ROOT_MARK) } else { 0 } + if app.update_behind.is_some() { width(UPDATE_MARK) } else { 0 }
}

pub(super) fn segs_width(app: &App, segs: &[Vec<Seg>]) -> usize {
    // the brand + " │ " between every group
    brand_width(app) + segs.iter().map(|g| 3 + g.iter().map(|s| width(&s.text)).sum::<usize>()).sum::<usize>()
}

pub(in crate::ui) fn draw_title(f: &mut Frame, app: &mut App, area: Rect) {
    app.status_hits.clear();
    if area.height == 0 {
        return;
    }
    let now = app.now();
    app.note_gauge(now);
    app.note_effort(now);
    let theme = &app.theme.clone();
    let hover = app.hover;
    let lit = |hit: StatusHit| match hover {
        Hover::Status(h) if h == hit => Modifier::UNDERLINED,
        _ => Modifier::empty(),
    };
    let base = theme.bar();
    // quiet, with the bar's pieces not listed by hand: no billed-token total, no curator figures
    let lean = app.quiet() && app.opts.status_line.is_empty();
    let tok_real = fmt_k(app.session_usage.total_input() + app.session_usage.output);
    let tok = if app.opts.animations { crate::motion::fixed(&fmt_k(app.tok_shown), &tok_real) } else { tok_real };
    let glow = if app.opts.animations {
        crate::motion::hump(app.fx.turn_done, app.now(), crate::motion::GLOW_MS)
    } else {
        0.0
    };
    // every call re-sends its context, so this total of billed tokens grows much faster than mem
    // narrow bars keep the cost and give the room to the gauge
    let mut right = Vec::new();
    let mut clock = false;
    // while a turn runs, how long it has been running
    if let (true, Some(t)) = (app.busy, app.turn_started) {
        // a discreet spinner rides with the clock; with animations off it stays the plain clock glyph
        let glyph = if app.opts.animations { SPINNER[app.spinner % SPINNER.len()] } else { "⏱" };
        right.push(Seg {
            text: format!(" {glyph} {}", crate::render::blocks::fmt_secs(t.elapsed().as_secs())),
            style: base.fg(theme.accent),
        });
        clock = true;
    }
    // `[ui] status_command`: the person's script draws the bar; the turn's clock stays at the end
    if let Some(line) = app.status_cmd.as_ref().and_then(|s| s.line.clone()) {
        right.push(Seg { text: " ".to_string(), style: base });
        let w = area.width as usize;
        let right_w: usize = right.iter().map(|s| width(&s.text)).sum();
        // quiet: the message, queued requests and jobs ride at the right of the script's line
        let (notes, notes_w) = quiet_notes(app, w, right_w);
        let mut pieces = crate::status_cmd::spans(&line, base);
        // the script's colours are checked against nothing: only a 24-bit theme keeps them; on 256
        // or 16 colours and in mono the bar's own colours stand in. Italics never reach the screen.
        let own_colours = theme.mono || theme.degraded;
        pieces.iter_mut().for_each(|(_, s)| {
            if own_colours {
                (s.fg, s.bg) = (base.fg, base.bg);
            }
            *s = s.remove_modifier(Modifier::ITALIC);
        });
        let pieces = crate::status_cmd::fit(pieces, w.saturating_sub(right_w + notes_w + 1));
        let used = 1 + pieces.iter().map(|(t, _)| width(t)).sum::<usize>();
        let mut spans = vec![Span::styled(" ", base)];
        spans.extend(pieces.into_iter().map(|(t, s)| Span::styled(t, s)));
        spans.push(Span::styled(" ".repeat(w.saturating_sub(used + notes_w + right_w)), base));
        app.hint_hits.extend(note_hits(&notes, used + w.saturating_sub(used + notes_w + right_w) + NOTE_GAP, area));
        spans.extend(note_spans(app, &notes, notes_w));
        spans.extend(right.into_iter().map(|s| Span::styled(s.text, s.style)));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let pieces = status_pieces(&app.opts.status_line);
    // what is left of the subscription plan: `5h 63% left ↻2h · wk 88% left ↻4d`, warm from 80%, red when spent
    // (a provider on a key shows its per-minute limits in /usage only)
    let plan = app.plan_usage.as_ref().filter(|p| !p.is_limits() || app.on_plan());
    if let (true, true, Some(plan)) = (pieces.contains(&"plan"), area.width >= 60, plan) {
        let now = nexo_core::auth::now() as i64;
        for it in plan.items(now, area.width >= 100).into_iter().take(2) {
            if !right.is_empty() {
                right.push(Seg { text: " ·".to_string(), style: base.fg(theme.border) });
            }
            let colour = if it.pct >= 100.0 {
                theme.error
            } else if it.pct >= 80.0 {
                theme.warn
            } else {
                theme.dim
            };
            right.push(Seg { text: format!(" {}", it.text), style: base.fg(colour) });
        }
    }
    if pieces.contains(&"tokens") && !lean && (area.width >= 80 || app.cost <= 0.0 || !pieces.contains(&"cost")) {
        // a separator only between two pieces, never one left alone
        if !right.is_empty() {
            right.push(Seg { text: " ·".to_string(), style: base.fg(theme.border) });
        }
        let tstyle = if glow > 0.0 {
            base.fg(crate::motion::fade(theme, theme.bar_fg, theme.accent2, glow)).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        right.push(Seg { text: format!(" Σ {tok} billed"), style: tstyle });
    }
    // the cost, against the session's limit when there is one, coloured as it nears it
    let limit = app.budget.and_then(|(_, _, l, _)| l);
    let shown = if app.opts.animations { app.cost_shown } else { app.cost };
    if pieces.contains(&"cost") && app.on_plan() && app.cost > 0.0 {
        // a subscription: the price is what the API would have charged, not money spent
        let sep = if right.is_empty() { " " } else { " · " };
        right.push(Seg { text: sep.to_string(), style: base });
        right.push(Seg { text: crate::app::plan_value(app.cost), style: base.fg(theme.dim) });
    } else if pieces.contains(&"cost") && (app.cost > 0.0 || limit.is_some()) {
        let sep = if right.is_empty() { " " } else { " · " };
        right.push(Seg { text: sep.to_string(), style: base });
        let (text, colour) = match limit {
            Some(l) => {
                let share = app.cost / l;
                let c = if share >= 1.0 {
                    theme.error
                } else if share >= 0.8 {
                    theme.warn
                } else {
                    theme.money
                };
                (format!("{}/{l:.2}", crate::motion::fixed(&format!("${shown:.4}"), &format!("${:.4}", app.cost))), c)
            }
            // no limit: money reads in the money green, never the plain text colour
            None => (crate::motion::fixed(&format!("${shown:.4}"), &format!("${:.4}", app.cost)), theme.money),
        };
        // while it rolls or glows the figure keeps its colour and turns bold
        let rolling = (app.cost - shown).abs() > 0.000_05;
        let style = if rolling || glow > 0.0 { base.fg(colour).add_modifier(Modifier::BOLD) } else { base.fg(colour) };
        right.push(Seg { text, style });
    }
    right.push(Seg { text: " ".to_string(), style: base });
    let w = area.width as usize;
    let right_w: usize = right.iter().map(|s| width(&s.text)).sum();
    let (notes, notes_w) = quiet_notes(app, w, right_w);
    let avail = w.saturating_sub(right_w + notes_w);
    let groups = title_segments(app, avail);
    let mut spans = vec![Span::styled(" nexo", base.add_modifier(Modifier::BOLD))];
    if app.root {
        spans.push(Span::styled(ROOT_MARK, base.fg(theme.error).add_modifier(Modifier::BOLD)));
    }
    if app.update_behind.is_some() {
        // a newer nexo waits: a filled badge that stays until restart, not a line that scrolls away
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            UPDATE_MARK[1..].to_string(),
            Style::default().bg(theme.accent2).fg(theme.bg).add_modifier(Modifier::BOLD),
        ));
    }
    let mut used = brand_width(app);
    let mut hits = Vec::new();
    'outer: for (gi, g) in groups.iter().enumerate() {
        let sep = " │ ";
        if used + width(sep) > avail {
            break;
        }
        spans.push(Span::styled(sep.to_string(), base.fg(theme.border)));
        used += width(sep);
        let gw: usize = g.iter().map(|s| width(&s.text)).sum();
        // the model is the one piece worth cutting short: a long name on a narrow bar ends in `…`
        // instead of leaving the bar without it
        let model_cut = (gi == 0 && used + gw > avail)
            .then(|| {
                let first = g.first()?;
                let room = (avail.saturating_sub(used)).checked_sub(gw - width(&first.text))?;
                (room >= 8 && [&app.model, &format!("{}/{}", app.provider, app.model)].contains(&&first.text))
                    .then(|| truncate(&first.text, room))
            })
            .flatten();
        let gw = if model_cut.is_some() { avail - used } else { gw };
        if used + gw > avail {
            // a piece is shown whole or not at all: drop the separator just pushed
            spans.pop();
            used -= width(sep);
            break 'outer;
        }
        let gauge_group = g.iter().any(is_gauge_pct);
        let mode_label = mode_name(app);
        let model_texts = [app.model.clone(), format!("{}/{}", app.provider, app.model)];
        let mode_group = !gauge_group && g.first().is_some_and(|s| s.text == mode_label);
        // each piece answers for itself, wherever the user's order drew it: the model, its fast
        // marker, its effort, the mode, the goal, the gauge
        let hit_of = |i: usize, s: &Seg| -> Option<StatusHit> {
            if is_goal_seg(s) {
                Some(StatusHit::Goal)
            } else if is_todo_seg(s) {
                Some(StatusHit::Todo)
            } else if is_effort_seg(s) {
                Some(StatusHit::Effort)
            } else if s.text == FAST_MARK {
                Some(StatusHit::Fast)
            } else if model_texts.contains(&s.text) && !mode_group {
                Some(StatusHit::Model)
            } else if mode_group {
                (i == 0).then_some(StatusHit::Mode)
            } else {
                gauge_group.then_some(StatusHit::Gauge)
            }
        };
        let mut run: Option<(StatusHit, usize, usize)> = None;
        for (i, s) in g.iter().enumerate() {
            let shown = match (&model_cut, i) {
                (Some(t), 0) => t.clone(),
                _ => s.text.clone(),
            };
            let sw = width(&shown);
            let h = hit_of(i, s);
            match (run, h) {
                (Some((rh, st, rw)), Some(h)) if rh == h => run = Some((rh, st, rw + sw)),
                _ => {
                    if let Some((rh, st, rw)) = run.take() {
                        hits.push((Rect::new(area.x + st as u16, area.y, rw as u16, 1), rh));
                    }
                    run = h.map(|h| (h, used, sw));
                }
            }
            used += sw;
            let m = h.map(lit).unwrap_or_else(Modifier::empty);
            // the top reasoning level's `◆` pops and breathes: its own span, the same cells
            let mark = if is_effort_seg(s) { app.effort_mark(s.style.fg.unwrap_or(theme.accent2)) } else { None };
            // the line under a hovered piece starts at its first mark, not under the blank before it
            let lead = shown.len() - shown.trim_start().len();
            if let Some(c) = mark {
                let (blank, rest) = shown.split_at(lead);
                let (glyph, rest) = rest.split_at('◆'.len_utf8());
                spans.push(Span::styled(blank.to_string(), s.style));
                spans.push(Span::styled(glyph.to_string(), s.style.fg(c).add_modifier(m)));
                spans.push(Span::styled(rest.to_string(), s.style.add_modifier(m)));
            } else if !m.is_empty() && lead > 0 && lead < shown.len() {
                spans.push(Span::styled(shown[..lead].to_string(), s.style));
                spans.push(Span::styled(shown[lead..].to_string(), s.style.add_modifier(m)));
            } else {
                spans.push(Span::styled(shown, s.style.add_modifier(m)));
            }
        }
        if let Some((rh, st, rw)) = run.take() {
            hits.push((Rect::new(area.x + st as u16, area.y, rw as u16, 1), rh));
        }
    }
    let pad = w.saturating_sub(used + notes_w + right_w);
    spans.push(Span::styled(" ".repeat(pad), base));
    app.hint_hits.extend(note_hits(&notes, used + pad + NOTE_GAP, area));
    spans.extend(note_spans(app, &notes, notes_w));
    // the billed tokens and the cost open the usage (the trailing pad is not part of it)
    // (nor is the turn's clock in front of it)
    let lead = if clock { right.iter().take(2).map(|s| width(&s.text)).sum() } else { 0 };
    let start = used + pad + notes_w + lead;
    let cost_w = right_w.saturating_sub(lead + 1);
    if cost_w > 0 && start < w {
        hits.push((Rect::new(area.x + start as u16, area.y, cost_w.min(w - start) as u16, 1), StatusHit::Cost));
    }
    let cm = lit(StatusHit::Cost);
    let n = right.len();
    let skip = if lead > 0 { 2 } else { 0 };
    for (i, s) in right.into_iter().enumerate() {
        let st = if i >= skip && i + 1 < n { s.style.add_modifier(cm) } else { s.style };
        spans.push(Span::styled(s.text, st));
    }
    app.status_hits = hits;
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
