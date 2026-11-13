use super::*;

/// One coloured piece of the status bar (the bottom line).
pub struct Seg {
    pub text: String,
    pub style: Style,
}

/// The segments of the title bar, most important first within the drop
/// order: `(model, mode[, goal], gauge, mem, branch, directory)`; when the
/// bar does not fit, the directory goes first, then the branch, then the
/// memory, then the gauge's counts, then the model is shortened.
pub fn title_segments(app: &App, avail: usize) -> Vec<Vec<Seg>> {
    let mut groups = title_core(app, avail);
    // quiet: `? help` ends the bar, the first piece to go when it is tight
    let help = "? help";
    if app.quiet() && segs_width(app, &groups) + 3 + width(help) <= avail {
        groups.push(vec![Seg { text: help.to_string(), style: app.theme.bar().fg(app.theme.dim) }]);
    }
    groups
}

fn title_core(app: &App, avail: usize) -> Vec<Vec<Seg>> {
    let theme = &app.theme;
    let lean = app.quiet() && app.opts.status_line.is_empty();
    let base = theme.bar();
    // a piece that just changed is green for a moment, then its own colour (the curator label's highlight)
    let lit = |i: usize, own: Color| {
        let now = app.now();
        match app.fx.chg_at[i].filter(|t| crate::motion::within(Some(*t), now, crate::motion::CUT_MS)) {
            Some(at) => {
                let t = now.saturating_duration_since(at).as_millis() as f32 / crate::motion::CUT_MS as f32;
                crate::motion::fade(theme, theme.success, own, t)
            }
            None => own,
        }
    };
    let model = |short: bool| Seg {
        text: if short { app.model.clone() } else { format!("{}/{}", app.provider, app.model) },
        style: base.fg(lit(0, theme.accent)).add_modifier(Modifier::BOLD),
    };
    // the same glyph, words and colour as the rule over the draft (mode_tint), minus its explanation
    let mode_color = mode_tint(theme, app.permission_mode).map_or(theme.bar_fg, |(c, _)| c);
    let mode_name = mode_name(app);
    let mode = match app.permission_mode {
        // never asking is said plainly, and loud
        PermissionMode::Auto => {
            Seg { text: mode_name, style: base.fg(lit(2, mode_color)).add_modifier(Modifier::BOLD) }
        }
        _ => Seg { text: mode_name, style: base.fg(lit(2, mode_color)) },
    };
    // the goal and, when it does not run, why: waiting for an answer, paused, met
    let goal = app.goal.as_ref().map(|g| match &g.state {
        GoalState::Active => Seg { text: "◎ goal".to_string(), style: base.fg(theme.accent2) },
        GoalState::Waiting => Seg { text: "◎ goal: your turn".to_string(), style: base.fg(theme.warn) },
        GoalState::Paused(_) => Seg { text: "◎ goal paused".to_string(), style: base.fg(theme.dim) },
        GoalState::Done => Seg { text: "✓ goal".to_string(), style: base.fg(theme.success) },
    });
    // the to-do list as a count (the strip above the draft is opt-in): all done turns it green until the turn ends
    let todo_chip = app.todo_chip_on().then(|| {
        let (done, total) = todo_counts(&app.todo);
        let colour = if done == total { theme.success } else { theme.accent2 };
        Seg { text: format!("{TODO_MARK} {done}/{total}"), style: base.fg(colour) }
    });
    let gauge = {
        let (num, den, guessed) = app.gauge_view();
        let pct = share(num, den);
        // drawn at the eased share and colour: a new value slides in rather than jumps
        let now = app.now();
        let shown = app.fx.gauge.frac_at(now, (num as f64 / den.max(1) as f64).min(1.0));
        let colour = app.fx.gauge.colour_at(theme, now, theme.level_color(pct));
        let mut cells = if let Some(split) = app.gauge_split().filter(|_| !guessed) {
            let [cached, fresh, out] = app.fx.gauge.parts_at(now, [split.cached, split.fresh, split.out]);
            crate::render::gauge::stacked(
                theme,
                crate::render::gauge::Split { cached, fresh, out },
                den,
                GAUGE_CELLS,
                colour,
            )
        } else {
            gauge_segs(theme, shown, GAUGE_CELLS, colour, pct >= 90 && (app.spinner / 3).is_multiple_of(2))
        };
        cells.push(Seg {
            text: if guessed { format!(" ~{pct}%") } else { format!(" {pct}%") },
            style: base.fg(colour).add_modifier(Modifier::BOLD),
        });
        let counts = if guessed { fmt_k(num) } else { format!("{}/{}", fmt_k(num), fmt_k(den)) };
        (cells, Seg { text: counts, style: base.fg(theme.dim) })
    };
    // what the policy does ("curator", "curator -12k tok", "curator idle"), not only its name
    let policy = app
        .context
        .as_ref()
        .map(|c| if c.label.is_empty() { c.policy.clone() } else { c.label.clone() })
        .filter(|p| !p.is_empty());
    // every token of the session so far (what each call sends is the gauge); "tok" so nobody reads it as RAM
    let mem = app
        .context
        .as_ref()
        .filter(|c| c.history_tokens > 0)
        .map(|c| Seg { text: format!("session {} tok", fmt_k(c.history_tokens)), style: base.fg(theme.warn2) })
        // a loaded conversation the curator is still counting
        .or_else(|| {
            (app.context.is_none() && app.context_wait)
                .then(|| Seg { text: mem_waiting(app.spinner), style: base.fg(theme.dim) })
        });
    // a glyph every common font has, not an icon font's
    // a new branch or folder is lit like a new model; at rest the branch keeps the bar's own colour
    let lit_if = |i: usize, own: Color, st: Style| {
        let c = lit(i, own);
        if c == own {
            st
        } else {
            st.fg(c)
        }
    };
    let branch = app.branch.as_ref().map(|b| Seg { text: format!("⎇ {b}"), style: lit_if(3, theme.bar_fg, base) });
    let dir = Seg { text: short_path(&app.cwd.to_string_lossy()), style: lit_if(4, theme.dim, base.fg(theme.dim)) };
    // what the curator just let go is in its label: green for a moment, then its own colour
    let policy_seg = policy.filter(|_| !lean).map(|p| {
        let now = app.now();
        match app.fx.cut.filter(|c| crate::motion::within(Some(c.1), now, crate::motion::CUT_MS)) {
            Some((_, at)) => {
                let t = now.saturating_duration_since(at).as_millis() as f32 / crate::motion::CUT_MS as f32;
                let fg = crate::motion::fade(theme, theme.success, theme.accent2, t);
                Seg { text: p, style: base.fg(fg).add_modifier(Modifier::BOLD) }
            }
            None => Seg { text: p, style: base.fg(theme.accent2) },
        }
    });
    // the share of input the cache served (billed at a fraction) and the model calls so far
    let extra = {
        let u = &app.session_usage;
        let mut g = Vec::new();
        if u.total_input() > 0 {
            let pct = share(u.cache_read, u.total_input());
            let colour = if pct >= 50 {
                theme.success
            } else if pct >= 20 {
                theme.warn
            } else {
                theme.dim
            };
            g.push(Seg { text: format!("{pct:>3}% cached"), style: base.fg(colour) });
        }
        let calls = app.call_inputs.len();
        if calls > 0 {
            let sep = if g.is_empty() { "" } else { " · " };
            let word = if calls == 1 { "call" } else { "calls" };
            g.push(Seg { text: format!("{sep}{calls} {word}"), style: base.fg(theme.dim) });
        }
        g
    };

    // what each candidate keeps, from the whole bar down to the model alone
    #[derive(Clone, Copy, PartialEq)]
    struct Keep {
        short_model: bool,
        dir: bool,
        branch: bool,
        mem: bool,
        extra: bool,
        counts: bool,
        // the curator's label: it outlives the counts
        label: bool,
        goal: bool,
        gauge: bool,
        mode: bool,
        // the `◆ default` hint (no level set): the first piece to go
        hint: bool,
        // the to-do count: goes with the session figure, before the context counts and the model name
        todo: bool,
    }
    let order = status_pieces(&app.opts.status_line);
    let build = |k: &Keep| {
        let mut out: Vec<Vec<Seg>> = Vec::new();
        // the model and its reasoning level share a group when both are on, in the listed order
        let mut model_done = false;
        for piece in &order {
            match *piece {
                "model" | "effort" => {
                    if model_done {
                        continue;
                    }
                    model_done = true;
                    let mut g = Vec::new();
                    // no model yet (the first screen, before the setup): no `/` for a name
                    if order.contains(&"model") && !app.model.is_empty() {
                        g.push(model(k.short_model));
                        if app.fast {
                            g.push(Seg { text: FAST_MARK.to_string(), style: base.fg(theme.warn) });
                        }
                    }
                    if order.contains(&"effort") {
                        if let Some(e) = app.effort.as_deref().filter(|e| !e.is_empty()) {
                            g.push(Seg { text: format!(" ◆ {e}"), style: base.fg(lit(1, theme.accent2)) });
                        } else if k.hint && !app.effort_levels.is_empty() {
                            // no level set, on a model that takes one: the word the picker's first entry uses
                            g.push(Seg { text: " ◆ default".to_string(), style: base.fg(theme.dim) });
                        }
                    }
                    if !g.is_empty() {
                        out.push(g);
                    }
                }
                "dir" if k.dir => out.push(vec![Seg { text: dir.text.clone(), style: dir.style }]),
                "branch" if k.branch => {
                    if let Some(b) = &branch {
                        out.push(vec![Seg { text: b.text.clone(), style: b.style }]);
                    }
                }
                "mode" if k.mode => {
                    let mut m = vec![Seg { text: mode.text.clone(), style: mode.style }];
                    if k.goal {
                        if let Some(g) = &goal {
                            m.push(Seg { text: " │ ".to_string(), style: base.fg(theme.border) });
                            m.push(Seg { text: g.text.clone(), style: g.style });
                        }
                    }
                    // the to-do count sits beside the goal: a click opens the list
                    if k.todo {
                        if let Some(t) = &todo_chip {
                            m.push(Seg { text: " │ ".to_string(), style: base.fg(theme.border) });
                            m.push(Seg { text: t.text.clone(), style: t.style });
                        }
                    }
                    out.push(m);
                }
                "context" if k.gauge => {
                    let (cells, counts) = &gauge;
                    let mut g: Vec<Seg> = cells.iter().map(|c| Seg { text: c.text.clone(), style: c.style }).collect();
                    if k.counts {
                        g.push(Seg { text: format!(" {}", counts.text), style: counts.style });
                    }
                    // the curator policy rides with the gauge when it is listed right after
                    let at = order.iter().position(|p| *p == "context");
                    if k.label && at.is_some_and(|i| order.get(i + 1) == Some(&"curator")) {
                        if let Some(p) = &policy_seg {
                            g.push(Seg { text: format!(" {}", p.text), style: p.style });
                        }
                    }
                    out.push(g);
                }
                "curator" if k.label => {
                    let at = order.iter().position(|p| *p == "context");
                    let joined = k.gauge && at.is_some_and(|i| order.get(i + 1) == Some(&"curator"));
                    if let (false, Some(p)) = (joined, &policy_seg) {
                        out.push(vec![Seg { text: p.text.clone(), style: p.style }]);
                    }
                }
                "mem" if k.mem => {
                    if let Some(m) = &mem {
                        out.push(vec![Seg { text: m.text.clone(), style: m.style }]);
                    }
                }
                "cache" if k.extra && !extra.is_empty() => {
                    let g: Vec<Seg> = extra.iter().map(|e| Seg { text: e.text.clone(), style: e.style }).collect();
                    if !g.is_empty() {
                        out.push(g);
                    }
                }
                _ => {}
            }
        }
        out
    };
    let all = Keep {
        short_model: false,
        dir: true,
        branch: true,
        mem: true,
        extra: true,
        counts: true,
        label: true,
        goal: true,
        gauge: true,
        mode: true,
        hint: true,
        todo: true,
    };
    // the steps up to this one keep the whole model name, the gauge and the mode
    const LAST_WHOLE_STEP: usize = 6;
    let steps = [
        Keep { ..all },
        Keep { dir: false, hint: false, ..all },
        Keep { dir: false, hint: false, branch: false, ..all },
        Keep { dir: false, hint: false, branch: false, extra: false, ..all },
        Keep { dir: false, hint: false, branch: false, mem: false, todo: false, extra: false, ..all },
        Keep { dir: false, hint: false, branch: false, mem: false, todo: false, extra: false, counts: false, ..all },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            extra: false,
            counts: false,
            label: false,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            extra: false,
            counts: false,
            label: false,
            short_model: true,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            extra: false,
            counts: false,
            label: false,
            short_model: true,
            goal: false,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            extra: false,
            counts: false,
            label: false,
            short_model: true,
            goal: false,
            gauge: false,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            extra: false,
            counts: false,
            label: false,
            short_model: true,
            goal: false,
            gauge: false,
            mode: false,
        },
    ];
    for (i, k) in steps.iter().enumerate() {
        let c = build(k);
        if segs_width(app, &c) <= avail {
            // the steps drop pieces together, so the last one may have left room: while the model is
            // whole and the gauge and mode stay, a piece it took out comes back where it fits, the
            // one that matters most first (the order the steps drop them in, reversed)
            if i > LAST_WHOLE_STEP {
                return c;
            }
            let mut keep = *k;
            let back: [fn(&mut Keep); 8] = [
                |k| k.label = true,
                |k| k.counts = true,
                |k| k.todo = true,
                |k| k.mem = true,
                |k| k.extra = true,
                |k| k.branch = true,
                |k| k.dir = true,
                |k| k.hint = true,
            ];
            let mut out = c;
            for add in back {
                let mut more = keep;
                add(&mut more);
                if more == keep {
                    continue;
                }
                let c = build(&more);
                if segs_width(app, &c) <= avail {
                    keep = more;
                    out = c;
                }
            }
            return out;
        }
    }
    build(&steps[steps.len() - 1])
}

/// The status bar's pieces in the order they are drawn: the configured list (unknown names and
/// repeats dropped), or all of them when it is empty. `tokens` and `cost` are drawn on the right.
pub fn status_pieces(cfg: &[String]) -> Vec<&'static str> {
    const ALL: [&str; 12] =
        ["model", "effort", "dir", "branch", "mode", "context", "curator", "mem", "cache", "plan", "tokens", "cost"];
    let mut out: Vec<&'static str> = Vec::new();
    for n in cfg {
        if let Some(p) = ALL.iter().find(|p| p.eq_ignore_ascii_case(n.trim())) {
            if !out.contains(p) {
                out.push(p);
            }
        }
    }
    if out.is_empty() && cfg.is_empty() {
        out = ALL.to_vec();
    }
    out
}

/// " nexo", and " # root" after it when nexo runs as root, so that is never out of sight.
const ROOT_MARK: &str = " # root";
/// Shown after the brand while a newer nexo waits on `main`.
const UPDATE_MARK: &str = "  ⬆ update ";

fn brand_width(app: &App) -> usize {
    5 + if app.root { width(ROOT_MARK) } else { 0 } + if app.update_behind.is_some() { width(UPDATE_MARK) } else { 0 }
}

fn segs_width(app: &App, segs: &[Vec<Seg>]) -> usize {
    // the brand + " │ " between every group
    brand_width(app) + segs.iter().map(|g| 3 + g.iter().map(|s| width(&s.text)).sum::<usize>()).sum::<usize>()
}

pub(super) fn share(num: u64, den: u64) -> u64 {
    ((num as f64 / den.max(1) as f64) * 100.0).round().clamp(0.0, 999.0) as u64
}
/// Columns kept clear between the bar's pieces and the notes at its right.
const NOTE_GAP: usize = 2;

/// Quiet: what the keys row carried at its right (a message, queued requests, running jobs, shared notes) rides
/// in the free space of the status bar instead, so the row can go. Returns the pieces that fit `w / 2` of what
/// the clock and the cost leave, and the columns they take with the gap. The session name stays out.
fn quiet_notes(app: &mut App, w: usize, right_w: usize) -> (Vec<Piece>, usize) {
    let room = w.saturating_sub(right_w) / 2;
    if !app.quiet() {
        return (Vec::new(), 0);
    }
    // a warning or an error clipped to the room (or with no room at all) is not lost: the whole of it goes to
    // a toast, once
    if let Some((msg, _)) = app.status.clone() {
        if width(&msg) > room.saturating_sub(2) && !app.status_toasted && app.status_level != Level::Info {
            app.status_toasted = true;
            let kind = match app.status_level {
                Level::Error => crate::toast::Kind::Error,
                _ => crate::toast::Kind::Info,
            };
            app.toast(kind, &msg);
        }
    }
    if room < 8 {
        return (Vec::new(), 0);
    }
    let mut pieces = hint_pieces(app, room);
    pieces.retain(|p| !matches!(p.1, Some(HintHit::Session | HintHit::Todo)));
    let span_w = |ps: &[Piece]| -> usize { join_pieces(app, ps).iter().map(|s| width(&s.content)).sum() };
    while !pieces.is_empty() && span_w(&pieces) > room {
        let low = pieces.iter().enumerate().min_by_key(|(_, p)| p.0).map(|(i, _)| i).unwrap_or(0);
        pieces.remove(low);
    }
    let total = if pieces.is_empty() { 0 } else { span_w(&pieces) + NOTE_GAP };
    (pieces, total)
}

/// The spans of the notes, after the gap that sets them apart from the bar's pieces.
fn note_spans(app: &App, notes: &[Piece], notes_w: usize) -> Vec<Span<'static>> {
    if notes_w == 0 {
        return Vec::new();
    }
    let mut out = vec![Span::styled(" ".repeat(NOTE_GAP), app.theme.bar())];
    out.extend(join_pieces(app, notes));
    out
}

/// Where each clickable note sits when they start at column `x`.
fn note_hits(notes: &[Piece], x: usize, area: Rect) -> Vec<(Rect, HintHit)> {
    let mut at = x;
    let mut hits = Vec::new();
    for (i, (_, hit, parts)) in notes.iter().enumerate() {
        if i > 0 {
            at += width(" ·");
        }
        let w: usize = parts.iter().map(|s| width(&s.content)).sum();
        if let Some(h) = hit {
            hits.push((Rect::new(area.x + at as u16, area.y, w as u16, 1), *h));
        }
        at += w;
    }
    hits
}

pub(super) fn draw_title(f: &mut Frame, app: &mut App, area: Rect) {
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

/// The fast-mode marker after the model.
const FAST_MARK: &str = " ⚡";

/// The reasoning level's piece of the bar: ` ◆ high`.
fn is_effort_seg(s: &Seg) -> bool {
    s.text.starts_with(" ◆ ")
}

/// The words the bar shows for the permission mode.
fn mode_name(app: &App) -> String {
    match mode_tint(&app.theme, app.permission_mode) {
        Some((_, label)) => label.split(" · ").next().unwrap_or(label).to_string(),
        None => app.permission_mode.as_str().to_string(),
    }
}

/// The goal's piece of the bar: `◎ goal`, `◎ goal: your turn`, `◎ goal paused`, `✓ goal`.
fn is_goal_seg(s: &Seg) -> bool {
    s.text.starts_with("◎ goal") || s.text.starts_with("✓ goal")
}

/// The mark of the to-do count in the bar (the terminal's ASCII fallback folds it like the goal's check).
const TODO_MARK: &str = "✓";

/// The to-do count's piece of the bar: `✓ 3/7`.
fn is_todo_seg(s: &Seg) -> bool {
    s.text.strip_prefix(TODO_MARK).is_some_and(|r| r.trim_start().starts_with(|c: char| c.is_ascii_digit()))
}

/// The gauge's percentage (" 42%", " ~42%"), not another piece that ends in one.
fn is_gauge_pct(s: &Seg) -> bool {
    let t = s.text.trim_start().trim_start_matches('~');
    t.ends_with('%') && t.starts_with(|c: char| c.is_ascii_digit())
}

/// Cells of the status bar's context gauge.
pub const GAUGE_CELLS: usize = 8;

/// The filled part of a gauge of `cells` cells at `share` (0..=1), in eighths of
/// a cell, and how many empty cells follow it: `(fill, empty)`.
pub fn gauge_cells(share: f64, cells: usize) -> (String, usize) {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (share.clamp(0.0, 1.0) * (cells * 8) as f64).round() as usize;
    let full = eighths / 8;
    let mut fill = "█".repeat(full);
    fill.push_str(PART[eighths % 8]);
    let used = full + usize::from(!eighths.is_multiple_of(8));
    (fill, cells - used)
}

/// A thin line gauge of `cells` cells at `share` (0..=1), in half cells:
/// `(fill, track)`, the fill heavy (`━`, `╸` for a half) and the track the same
/// line in a dim colour, so the bar sits on the text's baseline rather than
/// filling the whole row.
pub fn line_gauge(share: f64, cells: usize) -> (String, String) {
    let halves = (share.clamp(0.0, 1.0) * (cells * 2) as f64).round() as usize;
    let mut fill = "━".repeat(halves / 2);
    if halves % 2 == 1 {
        fill.push('╸');
    }
    let used = halves.div_ceil(2);
    (fill, "━".repeat(cells - used))
}

/// The gauge as coloured pieces: the fill in `colour` on a dim track, brighter while `pulse`.
fn gauge_segs(theme: &Theme, share: f64, cells: usize, colour: ratatui::style::Color, pulse: bool) -> Vec<Seg> {
    let track = mix(theme.bar_bg, theme.border, 0.7);
    let fill_colour = if pulse { mix(colour, theme.fg, 0.35) } else { colour };
    let (fill, rest) = line_gauge(share, cells);
    let mut out = vec![Seg { text: fill, style: Style::default().fg(fill_colour) }];
    if !rest.is_empty() {
        out.push(Seg { text: rest, style: Style::default().fg(track) });
    }
    out
}

pub fn short_path(p: &str) -> String {
    let p = nexo_core::paths::shown(std::path::Path::new(p));
    if width(&p) > 40 {
        let parts: Vec<&str> = p.rsplit('/').take(2).collect();
        format!("…/{}", parts.into_iter().rev().collect::<Vec<_>>().join("/"))
    } else {
        p
    }
}
