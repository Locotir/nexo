//! The segments of the title bar, and which pieces of the status bar are drawn.

use super::*;

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

pub(super) fn title_core(app: &App, avail: usize) -> Vec<Vec<Seg>> {
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
        GoalState::Active => {
            // a round that just advanced notches the mark: a filled ◉ in the accent, then one colour pass
            let ms = app.fx_on().then(|| app.fx.goal.notch_ms(app.now())).flatten();
            let (mark, pass) = crate::fx_goal::notch(ms);
            let mut style = base.fg(crate::motion::fade(theme, theme.accent2, theme.fg, pass));
            if mark == '◉' {
                style = base.fg(theme.accent).add_modifier(Modifier::BOLD);
            }
            Seg { text: format!("{mark} goal"), style }
        }
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
        // the leading cell lights at a new step; an edge runs over the cells at a higher band
        if app.fx_on() {
            cells = meniscus(theme, cells, app.fx.gauge.step_ms(now), app.fx.gauge.cross_ms(now));
        }
        cells.push(Seg {
            text: if guessed { format!(" ~{pct}%") } else { format!(" {pct}%") },
            style: base.fg(colour).add_modifier(Modifier::BOLD),
        });
        let counts = if guessed { fmt_k(num) } else { format!("{}/{}", fmt_k(num), fmt_k(den)) };
        (cells, Seg { text: counts, style: base.fg(theme.dim) })
    };
    let policy = policy_text(app);
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
    // what the curator just let go is in its label: green for a moment, then its own colour. With the motion
    // on, the label waits for the light that carries the cut out of the map (see `fx_condense`), lights up
    // once as it lands and its figure counts up
    let policy_seg = policy.filter(|_| !lean).map(|p| {
        let now = app.now();
        match app.fx.cut.filter(|c| crate::motion::within(Some(c.1), now, crate::motion::CUT_MS)) {
            Some((_, at)) => {
                let ms = now.saturating_duration_since(at).as_millis() as u64;
                let land = if app.opts.animations { crate::fx_condense::LIGHT_TO_MS } else { 0 };
                let bold = base.add_modifier(Modifier::BOLD);
                let Some(after) = ms.checked_sub(land) else {
                    return Seg { text: p, style: bold.fg(theme.accent2) };
                };
                let t = after as f32 / (crate::motion::CUT_MS - land) as f32;
                let style = bold.fg(crate::motion::fade(theme, theme.success, theme.accent2, t));
                // the pop as the light lands: the selection colour behind the label, gone in `POP_MS`
                let pop = if land > 0 { 1.0 - (after as f32 / crate::motion::POP_MS as f32).min(1.0) } else { 0.0 };
                let style = if pop > 0.0 { crate::motion::lit_style(theme, style, pop) } else { style };
                Seg { text: p, style }
            }
            None => Seg { text: p, style: base.fg(theme.accent2) },
        }
    });
    // a chip only when the cache let the last call down (the share itself is in /context)
    let extra = |k: &Keep| {
        let mut g = Vec::new();
        if let Some(fresh) = app.cache_miss().filter(|_| k.chip) {
            g.push(Seg {
                text: format!("cache miss · {} full price", fmt_k(fresh).replace(".0k", "k")),
                style: base.fg(theme.warn),
            });
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
        // the cache miss chip: a warning, so it goes only with the curator label
        chip: bool,
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
                "cache" => {
                    let g: Vec<Seg> = extra(k).iter().map(|e| Seg { text: e.text.clone(), style: e.style }).collect();
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
        chip: true,
        counts: true,
        label: true,
        goal: true,
        gauge: true,
        mode: true,
        hint: true,
        todo: true,
    };
    // the steps up to this one keep the whole model name, the gauge and the mode
    const LAST_WHOLE_STEP: usize = 5;
    let steps = [
        Keep { ..all },
        Keep { dir: false, hint: false, ..all },
        Keep { dir: false, hint: false, branch: false, ..all },
        Keep { dir: false, hint: false, branch: false, mem: false, todo: false, ..all },
        Keep { dir: false, hint: false, branch: false, mem: false, todo: false, counts: false, ..all },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            counts: false,
            label: false,
            chip: false,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            counts: false,
            label: false,
            chip: false,
            short_model: true,
            ..all
        },
        Keep {
            dir: false,
            hint: false,
            branch: false,
            mem: false,
            todo: false,
            counts: false,
            label: false,
            chip: false,
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
            counts: false,
            label: false,
            chip: false,
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
            counts: false,
            label: false,
            chip: false,
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
                |k| k.chip = true,
                |k| k.counts = true,
                |k| k.todo = true,
                |k| k.mem = true,
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

/// Every piece the status bar can draw, in the order it draws them when none is configured.
pub const STATUS_PIECES: [&str; 12] =
    ["model", "effort", "dir", "branch", "mode", "context", "curator", "mem", "cache", "plan", "tokens", "cost"];

/// The status bar's pieces in the order they are drawn: the configured list (unknown names and
/// repeats dropped), or all of them when it is empty. `tokens` and `cost` are drawn on the right.
pub fn status_pieces(cfg: &[String]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for n in cfg {
        if let Some(p) = STATUS_PIECES.iter().find(|p| p.eq_ignore_ascii_case(n.trim())) {
            if !out.contains(p) {
                out.push(p);
            }
        }
    }
    if out.is_empty() && cfg.is_empty() {
        out = STATUS_PIECES.to_vec();
    }
    out
}

/// What the policy does ("curator", "curator -12k tok", "curator idle"), not only its name; while a cut plays,
/// with its figure counting up (see `fx_condense`).
pub(super) fn policy_text(app: &App) -> Option<String> {
    let c = app.context.as_ref()?;
    let p = if c.label.is_empty() { c.policy.clone() } else { c.label.clone() };
    if p.is_empty() {
        return None;
    }
    Some(match app.condense_ms() {
        Some(ms) => crate::fx_condense::roll_label(&p, ms),
        None => p,
    })
}
