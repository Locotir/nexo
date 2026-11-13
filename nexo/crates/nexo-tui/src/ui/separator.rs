use super::*;

pub(super) fn draw_separator(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    // what opened or started since the last frame, to ease it in
    let (now, on) = (app.now(), app.fx_on());
    let open = app.search.is_some();
    if app.fx.search_on != open {
        app.fx.search_on = open;
        app.fx.search_at = on.then_some(now);
    }
    let working = app.activity().is_some();
    if app.fx.working != working {
        app.fx.working = working;
        app.fx.turn_start = (working && on).then_some(now);
    }
    let theme = &app.theme;
    let w = area.width as usize;
    let line = match &app.search {
        Some(s) => {
            let n = s.matches.len();
            let pos = if n == 0 { "no match".to_string() } else { format!("{}/{n}", s.current + 1) };
            let label = if s.focused {
                format!(" search: {}▏ {pos} ", s.query)
            } else {
                format!(" search: {} {pos} · n/N · Esc ", s.query)
            };
            let rest = w.saturating_sub(width(&label) + 2);
            // the label comes up out of the rule
            let hl = theme.highlight();
            let label_style = if on {
                let k = crate::anim::progress(app.fx.search_at, now, crate::motion::MODE_MS) as f32;
                let k = crate::anim::ease_out(k as f64) as f32;
                let from = |c: Option<ratatui::style::Color>, to: ratatui::style::Color| {
                    crate::motion::fade(theme, theme.border, c.unwrap_or(to), k)
                };
                hl.fg(from(hl.fg, theme.fg)).bg(from(hl.bg, theme.bg))
            } else {
                hl
            };
            Line::from(vec![
                Span::styled("─ ", theme.style(theme.border)),
                Span::styled(label, label_style),
                Span::styled(" ".to_string() + &"─".repeat(rest), theme.style(theme.border)),
            ])
        }
        // a sub-agent's conversation names itself where the mode is named
        None if app.in_sub() => sub_rule(app, w),
        None => match app.activity() {
            Some(verb) => {
                let line = activity_line(app, &verb, w);
                // a turn starting: the line comes up out of the plain rule
                if on && crate::motion::within(app.fx.turn_start, now, crate::motion::MODE_MS) {
                    let k = crate::anim::progress(app.fx.turn_start, now, crate::motion::MODE_MS);
                    rise_from_rule(line, theme, crate::anim::ease_out(k) as f32)
                } else {
                    line
                }
            }
            None if mode_tint(theme, app.permission_mode).is_some()
                || crate::motion::within(app.fx.mode_at, app.now(), crate::motion::MODE_MS) =>
            {
                mode_rule(app, w)
            }
            None => match app.fx.turn_done.filter(|t| {
                app.opts.animations && !theme.mono && crate::motion::within(Some(*t), app.now(), crate::motion::GLOW_MS)
            }) {
                Some(t) => Line::from(crate::motion::sweep(theme, w, t, app.now())),
                // search closed: the rule glows back from the left, as at the end of a turn
                None if on && crate::motion::within(app.fx.search_at, now, crate::motion::GLOW_MS) => {
                    Line::from(crate::motion::sweep(theme, w, app.fx.search_at.unwrap_or(now), now))
                }
                None => mode_rule(app, w),
            },
        },
    };
    f.render_widget(Paragraph::new(line), area);
    // the hardest reasoning level: a comet runs the rule into the tag once when it is set, a band drifts
    // along it while the model works (colour only, the comet's head heavy)
    let edge = (!(app.locked && app.input.is_empty())).then(|| rule_tag(app, area.width)).flatten().map(|t| t.0);
    app.paint_crest(f.buffer_mut(), area, edge);
    // a rejected draft flashes the rule once (colour only)
    let k = if on { crate::motion::hump(app.fx.reject_at, now, crate::fx_chrome::REJECT_MS) } else { 0.0 };
    if k > 0.0 {
        let theme = &app.theme;
        for x in area.x..area.right() {
            if let Some(c) = f.buffer_mut().cell_mut((x, area.y)) {
                if c.symbol() == "─" {
                    c.fg = crate::motion::fade(theme, c.fg, theme.error, k);
                }
            }
        }
    }
}

/// The colour and the name a permission mode wears on the rule over the draft; none for the
/// default (ask), which keeps the plain rule.
pub fn mode_tint(theme: &Theme, mode: PermissionMode) -> Option<(ratatui::style::Color, &'static str)> {
    match mode {
        PermissionMode::Ask => None,
        PermissionMode::AcceptEdits => Some((theme.warn, "✎ accept edits")),
        PermissionMode::Auto => Some((theme.error, "▲ YOLO · never asks")),
        PermissionMode::ReadOnly => Some((theme.info, "◉ read-only")),
        PermissionMode::Plan => Some((theme.accent2, "◇ plan · reads, then a plan to approve")),
    }
}

/// The colour of the rule's line in `mode`: a tint of the mode's colour, the border otherwise.
fn rule_colour(theme: &Theme, mode: PermissionMode) -> ratatui::style::Color {
    match mode_tint(theme, mode) {
        Some((c, _)) => crate::motion::fade(theme, theme.border, c, 0.55),
        None => theme.border,
    }
}

/// The rule over the draft in a sub-agent's conversation: `─ sub · title ────`, the title cut to fit, `· ended`
/// when the run is over.
fn sub_rule(app: &App, w: usize) -> Line<'static> {
    let theme = &app.theme;
    let (title, ended) = app.sub_label().unwrap_or_default();
    let (head, tail) = ("sub · ", if ended { " · ended" } else { "" });
    // the rule keeps a dash and a space on each side
    let room = w.saturating_sub(4);
    let budget = room.saturating_sub(width(head) + width(tail));
    let label = if budget >= 2 || title.is_empty() {
        format!("{head}{}{tail}", truncate(&title, budget))
    } else {
        truncate(&format!("{head}{title}{tail}"), room)
    };
    let line = theme.style(rule_colour(theme, app.permission_mode));
    if room == 0 {
        return Line::from(Span::styled("─".repeat(w), line));
    }
    let dashes = w.saturating_sub(3 + width(&label));
    Line::from(vec![
        Span::styled("─ ", line),
        Span::styled(label, theme.bold(theme.accent)),
        Span::styled(format!(" {}", "─".repeat(dashes)), line),
    ])
}

/// The rule over the draft in `mode`, one styled cell per column: `─ ◇ plan ─────`.
fn mode_cells(theme: &Theme, mode: PermissionMode, w: usize) -> Vec<(char, Style)> {
    let line = theme.style(rule_colour(theme, mode));
    let mut out = Vec::with_capacity(w);
    // ask has no tint, but the rule still names it (dim), so a quiet mode is told from a hidden one
    let named = mode_tint(theme, mode).or(Some((theme.dim, "ask")));
    if let Some((c, label)) = named.filter(|(_, l)| w > width(l) + 8) {
        out.push(('─', line));
        out.push((' ', line));
        out.extend(label.chars().map(|ch| (ch, theme.bold(c))));
        out.push((' ', line));
    }
    while out.len() < w {
        out.push(('─', line));
    }
    out.truncate(w);
    out
}

/// The rule over the draft at rest, or, just after a change of mode, wiping from the old mode to
/// the new one, a bright edge leading.
fn mode_rule(app: &App, w: usize) -> Line<'static> {
    let theme = &app.theme;
    let now = app.now();
    let new = mode_cells(theme, app.permission_mode, w);
    let moving =
        app.opts.animations && !theme.mono && crate::motion::within(app.fx.mode_at, now, crate::motion::MODE_MS);
    let cells: Vec<(char, Style)> = match (moving, app.fx.mode_from) {
        (true, Some(from)) => {
            let old = mode_cells(theme, from, w);
            let p = crate::anim::ease_out(crate::anim::progress(app.fx.mode_at, now, crate::motion::MODE_MS));
            let head = (p * (w as f64 + 6.0)) as usize;
            let edge = mode_tint(theme, app.permission_mode).map_or(theme.accent, |t| t.0);
            new.into_iter()
                .zip(old)
                .enumerate()
                .map(|(x, (n, o))| {
                    if x + 6 < head {
                        n
                    } else if x < head {
                        // the edge: the new cell lit up, brightest at the front
                        let glow = 1.0 - (head - x) as f32 / 6.0;
                        let c = crate::motion::fade(theme, edge, theme.fg, glow * 0.7);
                        (n.0, theme.bold(c))
                    } else {
                        o
                    }
                })
                .collect()
        }
        _ => new,
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (ch, st) in cells {
        match spans.last_mut() {
            Some(last) if last.style == st => last.content.to_mut().push(ch),
            _ => spans.push(Span::styled(ch.to_string(), st)),
        }
    }
    Line::from(spans)
}

/// `line` with every colour eased from the rule's own (`k` 0..1).
fn rise_from_rule(line: Line<'static>, theme: &Theme, k: f32) -> Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| {
            let fg = s.style.fg.unwrap_or(theme.fg);
            let style = s.style.fg(crate::motion::fade(theme, theme.border, fg, k));
            Span::styled(s.content, style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style)
}

/// `─ ⠴ Thinking… 3.7s · ↓ 120 tok · Esc interrupt ───`: what the agent is doing, with a
/// light sweeping across the verb.
pub fn activity_line(app: &App, verb: &str, w: usize) -> Line<'static> {
    let theme = &app.theme;
    let frame = app.spinner;
    // the mode keeps its colour while the agent works
    let border = theme.style(rule_colour(theme, app.permission_mode));
    let mut spans = vec![
        Span::styled("─ ", border),
        Span::styled(SPINNER[frame % SPINNER.len()].to_string(), theme.bold(theme.accent)),
        Span::raw(" "),
    ];
    let word = format!("{verb}…");
    let n = word.chars().count() as f32;
    // the light runs left to right, rests a moment, and starts again
    let pos = (frame as f32 * 0.42) % (n + 10.0) - 3.0;
    for (i, c) in word.chars().enumerate() {
        let near = (1.0 - ((i as f32 - pos).abs() / 3.0)).max(0.0);
        let colour = mix(theme.accent, theme.fg, near);
        spans.push(Span::styled(c.to_string(), Style::default().fg(colour).add_modifier(Modifier::BOLD)));
    }
    let secs = app.turn_started.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
    // segments in order of worth: a narrow line drops whole ones from the end of this list
    // (the token count first, then the hint) instead of cutting a word or leaving a lone `·`
    let time = crate::render::blocks::fmt_secs(secs / 1000);
    let toks = (app.streamed_chars > 0).then(|| format!("↓ {} tok", fmt_k((app.streamed_chars / 4) as u64)));
    let mut parts: Vec<String> = vec![time, "Esc interrupt".to_string()];
    parts.extend(toks);
    let head: usize = spans.iter().map(|s| width(&s.content)).sum();
    // the tag at the right of the rule (level, size of the draft) is drawn over it: the segments stop short of it
    let room = rule_tag(app, w.min(u16::MAX as usize) as u16).map_or(w, |(x, _)| (x as usize).saturating_sub(1));
    let fits = |p: &[String]| head + 2 + p.iter().map(|s| width(s)).sum::<usize>() + 3 * (p.len() - 1) <= room;
    let mut keep = parts.len();
    while keep > 0 && !fits(&parts[..keep]) {
        keep -= 1;
    }
    parts.truncate(keep);
    // on screen the count sits before the hint, as it always has
    if keep == 3 {
        parts.swap(1, 2);
    }
    if !parts.is_empty() {
        let rest = format!(" {} ", parts.join(" · "));
        spans.push(Span::styled(rest, theme.dim()));
    }
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    let tail = w.saturating_sub(used);
    if app.opts.animations && !theme.mono {
        spans.extend(comet(theme, tail, frame));
    } else {
        spans.push(Span::styled("─".repeat(tail), border));
    }
    Line::from(spans)
}

/// A rule of `n` cells with a glow running along it, frame by frame: the line under a
/// working agent. Cells of the same colour share a span.
pub fn comet(theme: &Theme, n: usize, frame: usize) -> Vec<Span<'static>> {
    const TAIL: f32 = 8.0;
    let head = (frame as f32 * 1.0) % (n as f32 + 2.0 * TAIL) - TAIL;
    let mut out: Vec<Span<'static>> = Vec::new();
    for i in 0..n {
        let d = head - i as f32;
        // bright at the head, fading behind it, nothing ahead of it
        let glow = if d < 0.0 { (1.0 + d).max(0.0) } else { (1.0 - d / TAIL).max(0.0) };
        let style = theme.style(mix(theme.border, theme.accent, glow));
        match out.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push('─'),
            _ => out.push(Span::styled("─".to_string(), style)),
        }
    }
    out
}

/// The tag on the rule above the box, at the right: the reasoning level and, for a long or multiline draft,
/// its size. Where it starts, counted from the left of a row `width` wide, and its text (blanks at both
/// ends). A narrow row drops the level first, then the size.
pub(super) fn rule_tag(app: &App, width: u16) -> Option<(u16, String)> {
    let mut tags: Vec<String> = Vec::new();
    if let Some(e) = app.effort.as_deref().filter(|e| !e.is_empty()) {
        tags.push(format!("◆ {e}"));
    }
    if let Some(c) = crate::input::counter(&app.input) {
        tags.push(c);
    }
    while !tags.is_empty() {
        let label = format!(" {} ", tags.join(" · "));
        let w = crate::render::wrap::width(&label) as u16;
        if width > w + 6 {
            return Some((width - w - 1, label));
        }
        tags.remove(0);
    }
    None
}
