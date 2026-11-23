use super::*;

/// The side panel at the right: the goal, the todo list and the files
/// changed this session (`path  +n −m`, newest first).
pub(super) fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
    if area.width < 6 || area.height == 0 {
        return;
    }
    let theme = &app.theme;
    let mut lines = Vec::with_capacity(area.height as usize);
    let inner_w = area.width.saturating_sub(2) as usize;
    let rail = || Span::styled("│ ", theme.style(theme.border));
    let header = |text: String| -> Line<'static> {
        Line::from(vec![rail(), Span::styled(truncate(&text, inner_w), Style::default().add_modifier(Modifier::BOLD))])
    };
    if app.show_live {
        let body = match app.live_rows.and_then(|k| app.curated.get(k).map(|items| (k, items))) {
            Some((k, items)) => {
                let title = if k == 0 { "curator rows".to_string() } else { format!("curator rows · {k} back") };
                let mut v = crate::curated::lines(theme, items, inner_w, &title);
                v.push(Line::default());
                v.push(Line::from(Span::styled("F3 next call · then the map", theme.dim2())));
                v
            }
            None => {
                let (mut v, spot) = context_map(app, inner_w, area.height as usize);
                // the light of a cut leaves the map from the newest cell that condensed
                if let Some((row, col)) = spot.filter(|s| s.0 < area.height as usize) {
                    app.fx.cut_spot.set(Some((area.x + 2 + col as u16, area.y + row as u16)));
                }
                if !app.curated.is_empty() {
                    v.push(Line::from(Span::styled("F3 curator rows", theme.dim2())));
                }
                v
            }
        };
        lines.extend(body.into_iter().map(|mut l| {
            l.spans.insert(0, rail());
            l
        }));
        lines.push(Line::from(rail()));
        if !app.show_files {
            lines.truncate(area.height as usize);
            while lines.len() < area.height as usize {
                lines.push(Line::from(Span::styled("│", theme.style(theme.border))));
            }
            clear_bg(f, area, theme);
            f.render_widget(Paragraph::new(Text::from(lines)), area);
            return;
        }
    }
    let (gfx, now) = (&app.fx.goal, app.now());
    if let Some(goal) = &app.goal {
        lines.push(header("goal".to_string()));
        let runs = crate::render::markdown::inline_runs(goal.headline(), theme, theme.style(theme.accent));
        let first = [rail(), Span::raw("◎ ")];
        let rest = [rail(), Span::raw("  ")];
        let body = crate::render::wrap::wrap_runs(&runs, inner_w, &first, &rest);
        // four rows of the goal, more as the panel gets taller
        let goal_rows = (area.height as usize / 6).clamp(4, 10);
        for (k, l) in body.into_iter().enumerate() {
            if k < goal_rows {
                lines.push(l);
            }
        }
        let (round, max) = (goal.round, goal.max);
        let state = match &goal.state {
            GoalState::Active if round > 0 => {
                // the figure rolls to the new round; the row keeps its width
                let ms = app.fx_on().then(|| gfx.notch_ms(now)).flatten();
                Some((format!("round {}/{max}", crate::fx_goal::round_shown(round, ms)), theme.dim()))
            }
            GoalState::Active => None,
            GoalState::Waiting => Some(("waiting for your answer".to_string(), theme.style(theme.warn))),
            GoalState::Paused(why) => Some((format!("paused: {why} · /goal resume"), theme.dim())),
            GoalState::Done => Some(("met ✓ · /goal off clears it".to_string(), theme.style(theme.success))),
        };
        if let Some(state) = state {
            let body = crate::render::wrap::wrap_runs(&[state], inner_w, &[rail()], &[rail()]);
            let first = lines.len();
            lines.extend(body.into_iter().take(2));
            // a burst for the goal starts here: at the ✓ of a goal met, else at the start of the state
            if first < area.height as usize {
                let at = if goal.state == GoalState::Done { 6 } else { 4 };
                app.fx.goal_spot.set(Some(((area.x + at).min(area.right() - 1), area.y + first as u16)));
            }
            if let GoalState::Active = goal.state {
                let fig = format!("{round}/{max}");
                crate::fx_goal::light(theme, &mut lines[first], &fig, gfx.round_at, now, crate::motion::LIT_MS);
            }
            if let GoalState::Done = goal.state {
                crate::fx_goal::pop(theme, &mut lines[first], "✓", gfx.met_at, now, crate::fx_goal::MET_MS);
                crate::fx_goal::glow(theme, &mut lines[first], 1, gfx.met_at, now);
            }
        }
        lines.push(Line::from(rail()));
    }
    if !app.todo.is_empty() {
        let (done, total) = todo_counts(&app.todo);
        let mut head = vec![rail(), Span::styled("≡ todo  ", Style::default().add_modifier(Modifier::BOLD))];
        let frac = done as f64 / total as f64;
        head.extend(crate::render::todo::todo_summary(theme, gfx.bar_shown(frac, now), done, total, false));
        let mut head = Line::from(head);
        crate::fx_goal::light(theme, &mut head, &format!("{done}/{total}"), gfx.count_at, now, crate::motion::LIT_MS);
        lines.push(head);
        let ctx = crate::render::todo::RowCtx {
            spinner: app.opts.animations.then_some(app.spinner),
            compact: true,
            // two lines a task only when the changed files still fit under them
            roomy: lines.len() + app.todo.len() * 2 + 3 + app.changed.len().max(1) <= area.height as usize,
            ..Default::default()
        };
        for (k, it) in app.todo.iter().enumerate() {
            let mut rows: Vec<Line<'static>> = crate::render::todo::todo_row(it, inner_w, theme, ctx)
                .into_iter()
                .map(|row| {
                    let mut spans = vec![rail()];
                    spans.extend(row.spans);
                    Line::from(spans)
                })
                .collect();
            if let Some(rf) = gfx.row(k, &it.text) {
                crate::fx_goal::row(theme, &mut rows, 1, rf, now);
            }
            lines.extend(rows);
        }
        lines.push(Line::from(rail()));
    }
    lines.push(header(format!("changed files ({})", app.changed.len())));
    if app.changed.is_empty() {
        lines.push(Line::from(vec![rail(), Span::styled("none yet", theme.dim())]));
    }
    let (fnow, mut fading) = (app.now(), Vec::new());
    for c in &app.changed {
        let look = app.fx.diff.look(&c.path, fnow);
        // a rolling figure keeps the width of the settled one, so the path never moves
        let fig = |shown: Option<u64>, settled: usize| {
            let s = settled.to_string();
            match shown.map(|n| crate::motion::fixed(&n.to_string(), &s)) {
                Some(r) if width(&r) == width(&s) => r,
                _ => s,
            }
        };
        let (fa, fd) = (fig(look.map(|l| l.added), c.added), fig(look.map(|l| l.removed), c.removed));
        let counts = format!("+{} −{}", c.added, c.removed);
        let avail = inner_w.saturating_sub(width(&counts) + 1);
        let path = if width(&c.path) > avail {
            format!("…{}", tail_cols(&c.path, avail.saturating_sub(1)))
        } else {
            c.path.clone()
        };
        let pad = avail.saturating_sub(width(&path));
        let glow = look.map_or(0.0, |l| l.lit);
        let tint = |col: ratatui::style::Color| {
            let s = theme.style(col);
            if glow > 0.0 {
                s.bg(crate::motion::fade(theme, theme.bg, col, 0.4 * glow)).add_modifier(Modifier::BOLD)
            } else {
                s
            }
        };
        let mut line = Line::from(vec![
            rail(),
            Span::raw(path),
            Span::raw(" ".repeat(pad + 1)),
            Span::styled(format!("+{fa}"), tint(theme.diff_add)),
            Span::raw(" "),
            Span::styled(format!("−{fd}"), tint(theme.diff_del)),
        ]);
        if glow > 0.0 {
            line = line.style(crate::motion::lit_style(theme, Style::default(), 0.6 * glow));
        }
        if look.is_some_and(|l| l.born < 1.0) {
            fading.push((lines.len(), look.map_or(1.0, |l| l.born)));
        }
        lines.push(line);
    }
    crate::fx_goal::open_rows(theme, &mut lines, 0, gfx.open_at, now);
    lines.truncate(area.height as usize);
    while lines.len() < area.height as usize {
        lines.push(Line::from(Span::styled("│", theme.style(theme.border))));
    }
    clear_bg(f, area, theme);
    f.render_widget(Paragraph::new(Text::from(lines)), area);
    // a file that just showed up fades in from the ground (its rail stays)
    for (i, k) in fading {
        if i < area.height as usize {
            let row = Rect { x: area.x + 2, y: area.y + i as u16, width: area.width.saturating_sub(2), height: 1 };
            crate::fx_diff::fade_row(f.buffer_mut(), row, theme, k);
        }
    }
}

/// Token counts one cell of the context map can stand for.
const MAP_UNITS: [u64; 12] = [50, 100, 250, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000, 100_000, 200_000];

/// The smallest unit at which `tokens` (one entry per message) fit in `cap`
/// cells, each message taking at least one.
fn map_unit(tokens: &[u64], cap: usize) -> u64 {
    let cells = |u: u64| tokens.iter().map(|t| t.div_ceil(u).max(1) as usize).sum::<usize>();
    MAP_UNITS.iter().copied().find(|&u| cells(u) <= cap).unwrap_or(MAP_UNITS[MAP_UNITS.len() - 1])
}

// Bounds on original sizes, not payload sizes. Unknown is neither in nor out.
fn selection_counts<'a>(cells: impl Iterator<Item = &'a nexo_core::ContextCell>) -> (u64, u64, u64) {
    use nexo_core::CellState;
    cells.fold((0u64, 0u64, 0u64), |(known, unknown, total), c| {
        let total = total.saturating_add(c.tokens);
        match c.state {
            CellState::Dropped => (known, unknown, total),
            CellState::Unknown => (known, unknown.saturating_add(c.tokens), total),
            _ => (known.saturating_add(c.tokens), unknown, total),
        }
    })
}

fn selection_label(known: u64, unknown: u64, total: u64) -> String {
    let fmt = crate::render::blocks::fmt_k;
    if unknown == 0 {
        format!("{}/{}", fmt(known), fmt(total))
    } else {
        format!("{}–{}?/{}", fmt(known), fmt(known.saturating_add(unknown)), fmt(total))
    }
}

/// `/context-live`: the whole history as a grid, a cell per `unit` tokens,
/// coloured by who wrote it and lit by how much of it the last call sent.
/// With the curator, the grid is its memory in zones: the anchor, the topic in
/// progress and the other topics, newest first, each under a line that says
/// what of it the call takes. Also where the newest cell condensing after a cut is (line, column), for the
/// light that leaves the map from it.
fn context_map(app: &App, w: usize, height: usize) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let theme = &app.theme;
    let fmt_k = crate::render::blocks::fmt_k;
    let mut out = vec![Line::from(Span::styled("context live", Style::default().add_modifier(Modifier::BOLD)))];
    if let Some(n) = app.relation_quotes {
        let unit = if n == 1 { "quote" } else { "quotes" };
        out.push(Line::from(Span::styled(format!("relations · {n} selected {unit}"), theme.style(theme.accent))));
    }
    // Keep the scale and its denominator on the same call. A source mark identifies a
    // message, not a substring or a token position within the original message.
    let heat = app.quote_heat.as_ref();
    let mut zone_tokens = std::collections::HashMap::<u32, u64>::new();
    if let Some(h) = heat {
        for (&i, &tokens) in &h.by_message {
            if let Some(c) = app.map.get(i) {
                let total = zone_tokens.entry(c.zone).or_default();
                *total = total.saturating_add(tokens);
            }
        }
        out.push(Line::from(Span::styled("◇ quoted source message", theme.dim())));
        out.push(Line::from(Span::styled(
            if h.input_tokens > 0 { "zone quote / est. input toks" } else { "input unknown · no heat scale" },
            theme.dim(),
        )));
        if h.input_tokens > 0 {
            out.push(Line::from(Span::styled("dim → bright: 0–1% (capped)", theme.dim())));
        }
    }
    let (sent, unknown, mem) = selection_counts(app.map.iter());
    if app.map.is_empty() {
        out.push(Line::from(Span::styled("fills in with the next call", theme.dim())));
        return (out, None);
    }
    let policy = app.context.as_ref().map(|c| c.policy.clone()).unwrap_or_default();
    // Original sizes of selected messages, not the payload: messages may be
    // trimmed and synthetic additions are not in the map.
    out.push(Line::from(vec![
        Span::styled("selected originals ", theme.style(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(selection_label(sent, unknown, mem), theme.dim()),
    ]));
    // the grid gets what the panel has left after the text around it
    let zoned = !app.map_zones.is_empty();
    let rows = height
        .saturating_sub(usize::from(app.relation_quotes.is_some()))
        .saturating_sub(heat.map_or(0, |h| 2 + usize::from(h.input_tokens > 0)))
        .saturating_sub(if zoned { 10 } else { 9 })
        .clamp(2, if zoned { 40 } else { 12 });
    let w = w.max(1);
    // the zones to draw, in order, each with its cells (index into the map)
    let mut groups: Vec<(Option<usize>, Vec<usize>)> = if zoned {
        let mut by: Vec<Vec<usize>> = vec![Vec::new(); app.map_zones.len()];
        for (i, c) in app.map.iter().enumerate() {
            if let Some(g) = by.get_mut(c.zone as usize) {
                g.push(i);
            }
        }
        let mut order: Vec<usize> = (0..by.len()).filter(|&z| !by[z].is_empty()).collect();
        // Keep quoted topics ahead of other inactive ones when the grid is short.
        order.sort_by_key(|&z| {
            (
                z != 0,
                !app.map_zones[z].active,
                !zone_tokens.contains_key(&(z as u32)),
                std::cmp::Reverse(by[z].last().copied()),
            )
        });
        order.into_iter().map(|z| (Some(z), std::mem::take(&mut by[z]))).collect()
    } else {
        vec![(None, (0..app.map.len()).collect())]
    };
    let lines_at = |u: u64, gs: &[(Option<usize>, Vec<usize>)]| -> usize {
        gs.iter()
            .map(|(z, ix)| {
                let n: usize = ix.iter().map(|&i| app.map[i].tokens.div_ceil(u).max(1) as usize).sum();
                n.div_ceil(w) + usize::from(z.is_some())
            })
            .sum()
    };
    let largest = MAP_UNITS[MAP_UNITS.len() - 1];
    let unit = if zoned {
        MAP_UNITS.iter().copied().find(|&u| lines_at(u, &groups) <= rows).unwrap_or(largest)
    } else {
        let tokens: Vec<u64> = app.map.iter().map(|c| c.tokens).collect();
        map_unit(&tokens, rows * w)
    };
    // Still too many lines: omit the lowest-priority topics, counted at the end.
    let mut hidden = 0;
    while zoned && groups.len() > 2 && lines_at(unit, &groups) + 1 > rows {
        groups.pop();
        hidden += 1;
    }
    // with animations off a changed cell does not blink
    let flash = app
        .map_at
        .filter(|_| app.opts.animations)
        .map(|t| app.now().saturating_duration_since(t).as_millis() as u64)
        .filter(|&ms| ms < MAP_FLASH_MS);
    // the curator just cut the view: the cells it let go condense, oldest first (see `fx_condense`)
    let condensing = app.condense_ms().map(|ms| {
        let from = &app.fx.cut_from;
        let ix: Vec<usize> = (0..app.map.len().min(from.len()))
            .filter(|&i| crate::fx_condense::condenses(from[i], app.map[i].state))
            .collect();
        (ms, ix)
    });
    let mut spot = None;
    for (z, ix) in &groups {
        if let Some(z) = *z {
            let zone = &app.map_zones[z];
            let (s, u, m) = selection_counts(ix.iter().map(|&i| &app.map[i]));
            let (mark, style, tag) = if z == 0 {
                ("◆ ", theme.style(theme.accent2), String::new())
            } else if zone.active {
                ("● ", theme.style(theme.accent).add_modifier(Modifier::BOLD), " now".to_string())
            } else if s > 0 {
                ("◐ ", theme.style(theme.info), " back".to_string())
            } else if u > 0 {
                ("? ", theme.style(theme.warn), " unknown".to_string())
            } else if zone.card {
                ("▭ ", theme.style(theme.info), " card".to_string())
            } else {
                ("○ ", theme.dim2(), String::new())
            };
            let nums = format!(" {}{tag}", selection_label(s, u, m));
            let label = truncate(&zone.label, w.saturating_sub(2 + width(&nums)).max(1));
            out.push(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(label, style),
                Span::styled(nums, if zone.active || s > 0 { theme.dim() } else { theme.dim2() }),
            ]));
        }
        let kinds = theme.kind_colours();
        let mut cells: Vec<Span<'static>> = Vec::new();
        let mut newest = None;
        for &i in ix {
            let cond = condensing.as_ref().and_then(|(ms, c)| {
                let rank = c.binary_search(&i).ok()?;
                let start = crate::fx_condense::start_ms(rank, c.len());
                Some((ms.saturating_sub(start), app.fx.cut_from[i]))
            });
            let start = cells.len();
            cells.extend(cell_spans(app, i, unit, flash, &kinds, cond));
            if let Some(h) = heat.filter(|h| h.by_message.contains_key(&i)) {
                let tokens = zone_tokens[&app.map[i].zone];
                cells[start] = Span::styled("◇", quote_style(theme, tokens, h.input_tokens));
            }
            if cond.is_some() && condensing.as_ref().and_then(|c| c.1.last()) == Some(&i) {
                newest = Some(cells.len() - 1);
            }
        }
        // too long even at the largest unit: the newest part
        let cap = if z.is_some() { usize::MAX } else { rows * w };
        if cells.len() > cap {
            let gone = cells.len() - cap;
            cells.drain(..gone);
            newest = newest.and_then(|k: usize| k.checked_sub(gone));
        }
        if let Some(k) = newest {
            spot = Some((out.len() + k / w, k % w));
        }
        while !cells.is_empty() {
            let rest = cells.split_off(cells.len().min(w));
            out.push(Line::from(std::mem::replace(&mut cells, rest)));
        }
    }
    if hidden > 0 {
        out.push(Line::from(Span::styled(format!("… {hidden} hidden topics"), theme.dim2())));
    }
    out.push(Line::from(Span::styled(format!("up to {} tok/cell · {policy}", fmt_k(unit)), theme.dim())));
    out.push(Line::from(Span::styled("1+ cells per message", theme.dim())));
    // the legend wears the colours the cells wear, so the key matches the map
    let kinds = theme.kind_colours();
    out.push(Line::from(vec![
        Span::styled("█", theme.style(kinds[0])),
        Span::styled(" you ", theme.dim()),
        Span::styled("█", theme.style(kinds[1])),
        Span::styled(" reply ", theme.dim()),
        Span::styled("█", theme.style(kinds[2])),
        Span::styled(" call ", theme.dim()),
        Span::styled("█", theme.style(kinds[3])),
        Span::styled(" tool", theme.dim()),
    ]));
    let (recalled, left) = ("░ recalled ", "· out ? unknown");
    out.push(Line::from(vec![
        Span::styled(format!("█ whole ▒ cut {recalled}"), theme.dim()),
        Span::styled(left, theme.dim2()),
    ]));
    (out, spot)
}

/// Linear over the first percentage point of estimated input. A fixed, readable floor
/// lets even a single recovered token remain visible; above 1% the scale saturates.
fn quote_style(theme: &Theme, tokens: u64, input: u64) -> Style {
    if input == 0 {
        return theme.dim();
    }
    let t = (100.0 * tokens as f64 / input as f64).min(1.0) as f32;
    if theme.mono {
        return if t < 0.5 { theme.dim() } else { Style::default().add_modifier(Modifier::BOLD) };
    }
    theme.style(crate::motion::fade(theme, theme.dim2, theme.accent, t))
}

/// The place of a kind of message in [`Theme::kind_colours`], the colours of the map and of the context chart.
pub(crate) fn kind_ix(kind: nexo_core::CellKind) -> usize {
    use nexo_core::CellKind;
    match kind {
        CellKind::Request => 0,
        CellKind::Reply => 1,
        CellKind::Call => 2,
        CellKind::Output => 3,
    }
}

/// How long a changed cell of the map shows reversed before it fades back.
const FLASH_HOLD_MS: u64 = 150;

/// The style of a map cell that changed `ms` ago: reversed and bold at first, then its fill fades from the
/// accent to its own colour until the flash is over. A mono theme has no colour to fade: it ends with the hold.
pub(crate) fn flash_style(theme: &Theme, style: Style, own: Color, ms: u64) -> Style {
    if ms < FLASH_HOLD_MS {
        return style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
    }
    if theme.mono {
        return style;
    }
    let t = (ms - FLASH_HOLD_MS) as f32 / (MAP_FLASH_MS - FLASH_HOLD_MS) as f32;
    style.fg(crate::motion::fade(theme, theme.accent, own, t))
}

/// The cells of message `i` of the map at `unit` tokens a cell. `cond`: the cell condenses after a cut, `ms`
/// into its own steps, from the state it had before.
fn cell_spans(
    app: &App,
    i: usize,
    unit: u64,
    flash: Option<u64>,
    kinds: &[Color; 4],
    cond: Option<(u64, nexo_core::CellState)>,
) -> impl Iterator<Item = Span<'static>> {
    use nexo_core::CellState;
    let theme = &app.theme;
    let c = &app.map[i];
    let colour = kinds[kind_ix(c.kind)];
    let (mut glyph, mut style): (std::borrow::Cow<'static, str>, Style) = match c.state {
        CellState::Whole => ("█".into(), theme.style(colour)),
        CellState::Shrunk => ("▒".into(), theme.style(colour)),
        CellState::Recalled => ("░".into(), theme.style(colour)),
        CellState::Dropped => ("·".into(), theme.dim2()),
        CellState::Unknown => ("?".into(), theme.style(theme.warn)),
    };
    match cond {
        // condensing: a step down the ladder at a time, the colour lifted at first so the eye finds it, then
        // easing into the new state's; the last step is the settled cell itself
        Some((ms, from)) => {
            let level = crate::fx_condense::condense_level(ms, from, c.state);
            if level < 1.0 {
                let to = if c.state == CellState::Dropped { theme.dim2().fg.unwrap_or(theme.border) } else { colour };
                let lifted = crate::motion::fade(theme, colour, theme.fg, 0.45);
                glyph =
                    crate::fx_condense::condense_glyph(ms, from, c.state, crate::ascii::wanted()).to_string().into();
                style = theme.style(crate::motion::fade(theme, lifted, to, level)).add_modifier(Modifier::BOLD);
            }
        }
        // a cell that just changed lights up, then decays to its own colour
        None => {
            if let Some(ms) = flash.filter(|_| app.map_changed.get(i).copied().unwrap_or(false)) {
                style = flash_style(theme, style, colour, ms);
            }
        }
    }
    let n = c.tokens.div_ceil(unit).max(1) as usize;
    std::iter::repeat_n(Span::styled(glyph, style), n)
}

/// The last `w` columns of `s`.
fn tail_cols(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars().rev() {
        let cw = width(&ch.to_string());
        if used + cw > w {
            break;
        }
        out.insert(0, ch);
        used += cw;
    }
    out
}
