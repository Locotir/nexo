//! Drawing one frame from the app's state.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::anim;
use crate::app::{
    status_life, App, HintHit, Hover, StatusHit, FILES_PANEL_MIN_W, FILES_PANEL_W, MAP_FLASH_MS, STATUS_FADE_MS,
};
use crate::modal::{Modal, PickKind};
use crate::overlay;
use crate::palette::{PaletteKind, MAX_ROWS};
use crate::render::blocks::{fmt_k, todo_counts, SPINNER};
use crate::render::markdown::{inline_md, text_of, tint_plain};
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::scrollbar::{self, Bar, Target};
use crate::select::style_cols;
use crate::theme::{mix, Theme};
use nexo_core::agent::{GoalState, Level};
use nexo_core::permission::PermissionMode;

pub const MAX_INPUT_LINES: u16 = 8;
/// Queued requests shown above the draft; the rest are one click away on the bar.
pub const MAX_QUEUE_ROWS: usize = 3;

/// Button labels of a permission dialog with the key each one stands for.
type Buttons = (Vec<(String, char)>, usize);

pub fn draw(f: &mut Frame, app: &mut App) {
    draw_all(f, app);
    // a highlight noted while drawing (a list row, a block) starts its glide in this frame, after
    // the tick: the loop must know, or at rest the next frame waits for the next key and the
    // list shows the row before the one chosen
    let now = app.now();
    let fx = &app.fx;
    if fx.pick_sel.moving(now)
        || fx.pal_sel.moving(now)
        || fx.block_sel.moving(now)
        || fx.diff.moving(now)
        || app.diffview.as_ref().is_some_and(|d| d.fx.moving(now))
    {
        app.moving = true;
    }
}

fn draw_all(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let (now0, on0) = (app.now(), app.fx_on());
    app.term_width = area.width;
    app.queue_rows.clear();
    app.todo_area = Rect::default();
    app.todo_shown = false;
    app.hint_hits.clear();
    app.note_popups();
    // the theme's ground under everything: no cell is left to the terminal's own colours
    f.render_widget(Box_::default().style(app.theme.base()), area);
    if let Some(dv) = &mut app.diffview {
        dv.fx.now = Some(now0);
        dv.fx.on = on0;
        app.fx.diff.note(false, &[], now0, on0); // the panel is not shown: reseed when it is back
        crate::diffview::render::draw(f, dv, area, &app.theme);
        // DF7: a dialog (permission, question, info) is always on top, the viewer never hides it
        if app.modal.is_some() {
            draw_modal(f, app, area);
        }
        scrub(f.buffer_mut());
        app.dirty = false;
        return;
    }
    if area.width < MIN_W || area.height < MIN_H {
        app.fx.diff.note(false, &[], now0, on0);
        draw_too_small(f, app, area);
        app.files_area = Rect::default();
        app.dirty = false;
        return;
    }
    let input_h = (crate::input::wrapped_rows(&app.input, area.width.saturating_sub(2) as usize).0.len() as u16)
        .clamp(1, MAX_INPUT_LINES);
    // a short window gives its keys line to the text, unless a window or the commands list is open
    // the keys row keeps its line while a draft is typed (blank then), so the text above never jumps
    // quiet: the row exists only while a list, a window or a mode needs its keys said
    let hints_h = if area.height <= SHORT_H && app.modal.is_none() && app.palette.is_none() {
        0
    } else if app.quiet() {
        u16::from(quiet_hints_on(app))
    } else {
        1
    };
    // the requests waiting behind the turn, a dim row each, right above the draft
    let queue_h = if app.queued_msgs.is_empty() || area.height <= SHORT_H {
        0
    } else {
        app.queued_msgs.len().min(MAX_QUEUE_ROWS) as u16
    };
    // the strip stays out where the list is on screen already: its own window, or the side panel
    let listed = matches!(app.modal, Some(Modal::Todo(_))) || (app.show_files && area.width >= FILES_PANEL_MIN_W);
    let todo_h =
        if app.todo_rows_on() && !listed { crate::todo_strip::strip_height(&app.todo, area.height) } else { 0 };
    // the keys right under the draft, the state (model, context, spend) on the last row
    let [middle, todo_area, queue, sep, input, hints, title] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(todo_h),
        Constraint::Length(queue_h),
        Constraint::Length(1),
        Constraint::Length(input_h),
        Constraint::Length(hints_h),
        Constraint::Length(1),
    ])
    .areas(area);
    let files_w = if (app.show_files || app.show_live) && area.width >= FILES_PANEL_MIN_W {
        files_panel_width(area.width)
    } else {
        0
    };
    let [text_area, files] = Layout::horizontal([Constraint::Min(0), Constraint::Length(files_w)]).areas(middle);
    app.fx.diff.note(files_w > 0, &app.changed, now0, on0);
    let transcript = text_area;
    app.files_area = if files_w > 0 { files } else { Rect::default() };
    if files_w == 0 && (app.show_files || app.show_live) {
        if !app.panel_hint_shown {
            app.panel_hint_shown = true;
            app.flash(&format!("panel hidden under {FILES_PANEL_MIN_W} columns"));
        }
    } else {
        app.panel_hint_shown = false;
    }
    app.status_row = title.y;
    draw_title(f, app, title);
    if app.locked {
        app.transcript_area = transcript;
        // the card comes in once per start
        let now = app.now();
        app.note_welcome(now);
        draw_lock(f, app, transcript, 0.0);
    } else {
        draw_transcript(f, app, transcript);
        // the card slides away over the text for a moment after a model works
        if let Some(at) = app.fx.unlock_at {
            let t = anim::progress(Some(at), app.now(), UNLOCK_MS);
            if t < 1.0 {
                draw_lock(f, app, transcript, anim::ease_out(t));
            } else {
                app.fx.unlock_at = None;
            }
        }
    }
    if files_w > 0 {
        draw_panel(f, app, files);
        app.fx.diff.draw_open(f.buffer_mut(), files, &app.theme, now0);
    }
    // the count in the bar is the list shown too: the keys row does not say it again
    let chip = app.status_hits.iter().any(|(_, h)| *h == StatusHit::Todo);
    app.todo_shown = todo_area.height > 0 || (files_w > 0 && !app.todo.is_empty()) || chip;
    draw_todo_strip(f, app, todo_area);
    draw_queue(f, app, queue);
    draw_separator(f, app, sep);
    if app.locked && app.input.is_empty() {
        draw_lock_input(f, app, input);
    } else {
        draw_input(f, app, input);
    }
    // the first screen has no key hints, but a message (Ctrl+C again to quit) still needs its row
    if (!app.locked || app.status.is_some()) && !matches!(app.modal, Some(Modal::Permission(_))) {
        draw_hints(f, app, hints);
    }
    // toasts: one row each, top right of the transcript, under every dialog and off the scrollbar column
    let toast_area = Rect { width: transcript.width.saturating_sub(1), ..transcript };
    crate::toast::draw(f, &app.toasts, &app.theme, toast_area, app.now(), app.opts.animations);
    if app.palette.is_some() {
        draw_palette(f, app, transcript);
    }
    if app.modal.is_some() {
        // over the transcript only: the draft and the state line stay in sight
        // the /context window wants 8 rows: on a short screen it also takes the rule's row under the text
        let over = Rect { height: middle.height + sep.height, ..middle };
        let area_for = match app.modal {
            Some(Modal::Info(_) | Modal::Agents(_) | Modal::Question(_)) if middle.height < 8 && over.height >= 6 => {
                over
            }
            _ if middle.height >= 5 => middle,
            _ => area,
        };
        // a permission dialog over the welcome card (the first screen) clears the rows of the card. The to-do and jobs windows get it, so no piece of the welcome card shows beside them; the
        // others keep the text in sight (a theme preview needs it) and clear only the cells beside the box
        if matches!(
            app.modal,
            Some(Modal::Todo(_) | Modal::Jobs(_) | Modal::Mcp(_) | Modal::Goal(_) | Modal::Sent(_) | Modal::Trust(_))
        ) {
            clear_bg(f, area_for, &app.theme);
        } else if app.has_welcome() && matches!(app.modal, Some(Modal::Permission(_))) {
            // only the rows the card holds, so the text around a dialog stays in sight
            let card_end = app.offsets.get(1).copied().unwrap_or(app.total_lines);
            let rows = card_end.saturating_sub(app.view_top()).min(usize::from(area_for.height)) as u16;
            clear_bg(f, Rect { height: rows, ..area_for }, &app.theme);
        }
        draw_modal(f, app, area_for);
        if matches!(app.modal, Some(Modal::Permission(_))) {
            draw_hints(f, app, hints);
        }
    }
    // a dialog or the palette leaving is drawn as the reverse of its pop
    if app.palette.is_some() || app.modal.is_some() {
        app.ghost_save(f.buffer_mut(), middle);
    } else {
        app.ghost_draw(f.buffer_mut());
    }
    scrub(f.buffer_mut());
    app.dirty = false;
}

/// The last line of defence: a cell whose text would move the cursor, ring, recolour or draw
/// differently from the layout (a tab, an escape, a line break, a joiner, a direction override)
/// shows a space instead. Every text is cleaned where it arrives; this catches the one that was not.
pub fn scrub(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        if cell.symbol().chars().any(crate::render::wrap::unsafe_char) {
            cell.set_symbol(" ");
        }
    }
}

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
            (app.context.is_none() && app.context_wait && !lean)
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

fn share(num: u64, den: u64) -> u64 {
    ((num as f64 / den.max(1) as f64) * 100.0).round().clamp(0.0, 999.0) as u64
}
/// The draft row while no model works: it cannot be typed into, it says what to do.
fn draw_lock_input(f: &mut Frame, app: &App, area: Rect) {
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
fn draw_lock(f: &mut Frame, app: &App, area: Rect, away: f64) {
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

fn draw_title(f: &mut Frame, app: &mut App, area: Rect) {
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
    // what is left of the subscription plan: `5h 37% ↻2h · wk 12% ↻4d`, warm from 80%, red when spent
    if let (true, true, Some(plan)) = (pieces.contains(&"plan"), area.width >= 60, &app.plan_usage) {
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

/// Clears a rectangle and paints the theme's ground on it (a `Clear` alone
/// would leave the terminal's own background there).
fn clear_bg(f: &mut Frame, r: Rect, theme: &Theme) {
    f.render_widget(Clear, r);
    f.render_widget(Box_::default().style(theme.base()), r);
}

/// One cell of clear ground on each side of a dialog, so the transcript's markers do not peek out beside it.
fn clear_margin(f: &mut Frame, r: Rect, area: Rect, theme: &Theme) {
    let x = r.x.saturating_sub(1).max(area.x);
    let end = (r.x + r.width + 1).min(area.x + area.width);
    clear_bg(f, Rect::new(x, r.y, end - x, r.height), theme);
}

/// Where the welcome card is on the screen (an empty rectangle when it is not in sight).
fn card_rect(app: &App) -> Rect {
    if !app.has_welcome() {
        return Rect::default();
    }
    let t = app.transcript_area;
    let card_end = app.offsets.get(1).copied().unwrap_or(app.total_lines);
    let rows = card_end.saturating_sub(app.view_top()).min(usize::from(t.height)) as u16;
    Rect { height: rows, ..t }
}

/// A window over the welcome card clears the card's rows it spans, across the card: no piece of
/// the card's frame (its edge, its corners) is left beside the window. Rows of the card the window
/// does not reach, and every other kind of text, stay in sight.
fn clear_card_beside(f: &mut Frame, card: Rect, window: Rect, theme: &Theme) {
    let (top, bottom) = (window.y.max(card.y), (window.y + window.height).min(card.y + card.height));
    if top < bottom {
        clear_bg(f, Rect::new(card.x, top, card.width, bottom - top), theme);
    }
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

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
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
fn fade_in(line: Line<'static>, theme: &Theme, t: f32) -> Line<'static> {
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
fn highlight(line: Line<'static>, q: &str, style: Style, current: bool) -> Line<'static> {
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

fn draw_separator(f: &mut Frame, app: &mut App, area: Rect) {
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
    // the hardest reasoning level was just set: a crest runs along the rule once (colour only)
    app.paint_crest(f.buffer_mut(), area);
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
    let fits = |p: &[String]| head + 2 + p.iter().map(|s| width(s)).sum::<usize>() + 3 * (p.len() - 1) <= w;
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

fn draw_input(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || area.width < 3 {
        return;
    }
    let theme = app.theme.clone();
    let prefix = Rect { width: 2, ..area };
    // with the vim keys, the mark says the mode: › to type, ▮ in normal mode
    let mark = match app.input.vim {
        // plain text colour: no permission tint (warn, error, info, accent2) or the default accent wears it
        Some(crate::input::VimMode::Normal) => Span::styled("▮ ", theme.bold(theme.fg)),
        // the mark wears the permission mode's colour too, so it is seen where one types
        _ => Span::styled("› ", theme.bold(mode_tint(&theme, app.permission_mode).map_or(theme.accent, |t| t.0))),
    };
    let mut marks = vec![Line::from(mark)];
    for _ in 1..area.height {
        marks.push(Line::from(Span::styled("  ", theme.dim())));
    }
    f.render_widget(Paragraph::new(Text::from(marks)), prefix);
    let editor = Rect { x: area.x + 2, width: area.width - 2, ..area };
    draw_draft(f, app, editor);
    f.render_widget(Box_::default().style(theme.base()), editor);
    if !app.input.is_empty() {
        let (rows, _) = crate::input::wrapped_rows(&app.input, editor.width as usize);
        let top = app.editor_top.min(rows.len());
        paint_chips(f, app, editor, &rows[top..]);
    }
    paint_complete(f, app, editor);
    // on the rule above the box, at the right: the reasoning level and, for a long or
    // multiline draft, its size; never over the text
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
        if area.y > 0 && area.width > w + 6 {
            let r = Rect { x: area.x + area.width - w - 1, y: area.y - 1, width: w, height: 1 };
            f.render_widget(Paragraph::new(Span::styled(label, theme.dim2())), r);
            break;
        }
        tags.remove(0);
    }
    // an empty box says what it takes, or shows the suggested next message (Tab or → takes it whole)
    if app.input.is_empty() && app.palette.is_none() && editor.width > 4 {
        let shared;
        let pend;
        let text = if let Some((_, ended)) = app.sub_label() {
            if ended {
                "ended · Esc back"
            } else {
                let n = app.sub_pending();
                if n == 0 {
                    "message this sub-agent · Esc back"
                } else if n == 1 {
                    "sent · arrives at its next step"
                } else {
                    pend = format!("{n} sent · arrive at its next step");
                    pend.as_str()
                }
            }
        } else if let Some(s) = app.suggestion_shown() {
            s
        } else if app.busy {
            if app.quiet() {
                "add a message"
            } else {
                "type a message: it joins the turn at its next step"
            }
        } else if let Some((id, title, _)) = app.shared.last() {
            let name = if title.is_empty() { id } else { title };
            shared = format!("⊕ what «{name}» knows goes with your next message · /memory share off drops it");
            shared.as_str()
        } else {
            "ask anything"
        };
        // one cell right of the caret, so the caret never covers its first letter
        let ph = Rect { x: editor.x + 2, width: editor.width - 2, height: 1, ..editor };
        f.render_widget(Paragraph::new(Span::styled(truncate(text, ph.width as usize), theme.dim2())), ph);
    }
}

/// The newest queued requests as dim rows (`◷ first line`): a click takes one back into the draft.
fn draw_todo_strip(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let fx = app.fx_on().then(|| (&app.fx.goal, app.now()));
    let lines = crate::todo_strip::strip_lines_fx(
        &app.todo,
        area.width as usize,
        area.height as usize,
        &app.theme,
        app.todo_done_live,
        fx,
    );
    f.render_widget(Paragraph::new(lines), area);
    app.todo_area = area;
}

fn draw_queue(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let first = app.queued_msgs.len().saturating_sub(rows);
    let mut hits = Vec::new();
    // a request that just joined the queue fades in
    let (now, on) = (app.now(), app.fx_on());
    let queued = &app.queued_msgs;
    app.fx.queue_seen.retain(|(t, _)| queued.contains(t));
    for text in queued {
        if !app.fx.queue_seen.iter().any(|(t, _)| t == text) {
            app.fx.queue_seen.push((text.clone(), now));
        }
    }
    for (k, text) in app.queued_msgs.iter().enumerate().skip(first).take(rows) {
        let r = Rect { y: area.y + (k - first) as u16, height: 1, ..area };
        let line = text.lines().next().unwrap_or("");
        let line = truncate(&format!(" ◷ {line}"), r.width as usize);
        let mut row = Line::from(Span::styled(line, app.theme.dim()));
        let seen = app.fx.queue_seen.iter().find(|(t, _)| t == text).map(|(_, at)| *at);
        if on {
            let k = crate::anim::progress(seen, now, crate::motion::LIT_MS) as f32;
            if k < 1.0 {
                row = fade_in(row, &app.theme, k);
            }
        }
        f.render_widget(Paragraph::new(row), r);
        hits.push((r, k));
    }
    app.queue_rows = hits;
}

/// The session name as the hint bar shows it: never longer than this, so the
/// keys on the left always stay visible.
pub const TITLE_COLS: usize = 24;

/// The pieces of the right part of the hint bar, each already bounded, with the priority that
/// keeps it (the lowest goes first when the bar is narrow): the state (a flash message, or the
/// session), a todo count, running jobs, shared memory, queued messages.
/// One piece of the right part of the keys row: its priority, what a click on it opens, its spans.
type Piece = (u8, Option<HintHit>, Vec<Span<'static>>);

fn hint_pieces(app: &App, status_room: usize) -> Vec<Piece> {
    let theme = &app.theme;
    let base = theme.bar();
    // the piece under the pointer is underlined
    let lit = |hit: HintHit| match app.hover {
        Hover::Hint(h) if h == hit => Modifier::UNDERLINED,
        _ => Modifier::empty(),
    };
    let mut out: Vec<Piece> = Vec::new();
    let (done, total) = todo_counts(&app.todo);
    if total > 0 && done < total && !app.todo_shown {
        out.push((
            2,
            Some(HintHit::Todo),
            vec![Span::styled(
                format!(" todo {done}/{total}"),
                base.fg(theme.accent2).add_modifier(lit(HintHit::Todo)),
            )],
        ));
    }
    let running = app.jobs.iter().filter(|j| j.status.running()).count();
    if running > 0 {
        let word = if running == 1 { "job" } else { "jobs" };
        out.push((
            3,
            Some(HintHit::Jobs),
            vec![Span::styled(format!(" ⚙ {running} {word}"), base.fg(theme.accent).add_modifier(lit(HintHit::Jobs)))],
        ));
    }
    if !app.shared.is_empty() {
        let tok: usize = app.shared.iter().map(|s| s.2).sum();
        out.push((
            1,
            Some(HintHit::Mem),
            vec![Span::styled(
                format!(" ⊕ mem {} · {}", app.shared.len(), crate::render::blocks::fmt_k(tok as u64)),
                base.fg(theme.accent2).add_modifier(lit(HintHit::Mem)),
            )],
        ));
    }
    if app.queued > 0 {
        out.push((
            4,
            Some(HintHit::Queued),
            vec![Span::styled(
                if app.quiet() {
                    format!(" ◷ {} queued", app.queued)
                } else {
                    format!(" ◷ {} queued · Enter send now", app.queued)
                },
                base.fg(theme.warn).add_modifier(lit(HintHit::Queued)),
            )],
        ));
    }
    if let Some((msg, at)) = &app.status {
        // the message fades into the bar over its last moments
        let life = status_life(app.status_level);
        let age = app.now().saturating_duration_since(*at).as_millis() as u64;
        let fade = age.saturating_sub(life - STATUS_FADE_MS) as f32 / STATUS_FADE_MS as f32;
        // with animations off it keeps the bar's colour to the end, no shade in between
        // a warning or an error keeps its colour (the level is the point), a confirmation the bar's
        let tint = match app.status_level {
            Level::Info => theme.bar_fg,
            Level::Warn => theme.warn,
            Level::Error => theme.error,
        };
        let colour = if app.opts.animations { mix(tint, theme.bar_bg, fade) } else { tint };
        // the room the bar really has, less the two spaces around it
        let text = truncate(msg, status_room.saturating_sub(2).max(1));
        out.push((5, None, vec![Span::styled(format!(" {} ", text), base.fg(colour).add_modifier(Modifier::BOLD))]));
    } else if let Some(id) = &app.session_id {
        // the whole id or title, or nothing: a piece of a name tells nobody anything
        let span = if app.session_title.is_empty() {
            Span::styled(format!(" {id} "), base.fg(theme.dim).add_modifier(lit(HintHit::Session)))
        } else {
            Span::styled(
                format!(" {} ", truncate(&app.session_title, TITLE_COLS)),
                base.fg(theme.accent2).add_modifier(lit(HintHit::Session)),
            )
        };
        out.push((0, Some(HintHit::Session), vec![span]));
    }
    out
}

/// The pieces joined by a separator between them (never one at an end), the last one followed by a space.
fn join_pieces(app: &App, pieces: &[Piece]) -> Vec<Span<'static>> {
    let theme = &app.theme;
    let base = theme.bar();
    let mut out = Vec::new();
    for (i, (_, _, spans)) in pieces.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(" ·".to_string(), base.fg(theme.border)));
        }
        out.extend(spans.iter().cloned());
    }
    if out.last().is_some_and(|s| !s.content.ends_with(' ')) {
        out.push(Span::styled(" ".to_string(), base));
    }
    out
}

/// Quiet: whether a list, a window or a mode has keys worth a row. The idle and busy lines never do (`? help` is in
/// the status bar), and a dialog that shows its own keys (the permission buttons, a window with its key line) needs none.
fn quiet_hints_on(app: &App) -> bool {
    app.input.vim.is_some()
        || app.palette.is_some()
        || app.search.as_ref().is_some_and(|s| s.focused)
        || (app.selected.is_some() && app.input.is_empty())
        || matches!(
            app.modal,
            Some(Modal::Question(_) | Modal::Trust(_) | Modal::Picker(_) | Modal::Setup(_) | Modal::Help { .. })
        )
}

/// The keys row is for a moment of doubt: while a draft is being typed it stays blank (its line kept,
/// so nothing above moves), unless it carries something that must be seen (a queued message, a note, the vim mode),
/// or a list or window is open whose keys are not obvious.
pub fn hints_shown(app: &App) -> bool {
    let typing = !app.input.is_empty()
        && app.modal.is_none()
        && app.palette.is_none()
        && app.selected.is_none()
        && !app.search.as_ref().is_some_and(|s| s.focused);
    let urgent = app.queued > 0 || app.status.is_some() || app.input.vim.is_some();
    !typing || urgent
}

/// What the memory figure says while the curator is still counting a loaded conversation.
fn mem_waiting(spinner: usize) -> String {
    format!("session {} counting…", SPINNER[spinner % SPINNER.len()])
}

/// The right part of the hint bar with everything on it.
fn hint_right(app: &App) -> Vec<Span<'static>> {
    join_pieces(app, &hint_pieces(app, usize::MAX))
}

/// The right part of the hint bar as plain text (tests).
pub fn hint_right_text(app: &App) -> String {
    hint_right(app).iter().map(|s| s.content.to_string()).collect()
}

/// Columns kept clear between the keys and the right part of the hint bar.
const HINT_GAP: usize = 2;

fn draw_hints(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    if !hints_shown(app) {
        f.render_widget(Box_::default().style(app.theme.base()), area);
        return;
    }
    let theme = app.theme.clone();
    let theme = &theme;
    let base = theme.bar();
    let left = if app.modal.is_some() {
        match app.modal.as_ref() {
            Some(Modal::Permission(p)) if p.rules_shown == 0 => {
                " y/Enter allow · a always · n/Esc deny · ↑↓ scroll".to_string()
            }
            // too narrow for the Enter and Esc spellings: the plain keys (the buttons above say the same)
            Some(Modal::Permission(_)) if area.width < 50 => " y allow · a always · n deny · 1-9 rule".to_string(),
            Some(Modal::Permission(_)) => " y/Enter allow · a always · n/Esc deny · 1-9 rule · ↑↓ scroll".to_string(),
            Some(Modal::Question(a)) if a.typing => {
                " type your answer · Enter send · Esc back to the options".to_string()
            }
            Some(Modal::Question(a)) if a.q.multi => {
                " ↑↓ choose · Space tick · Enter send · PgUp/PgDn scroll · Esc close".to_string()
            }
            Some(Modal::Question(_)) => " ↑↓ choose · 1-9 pick · Enter send · PgUp/PgDn scroll · Esc close".to_string(),
            Some(Modal::Trust(_)) => " ↑↓ choose · Enter pick · y yes · n or Esc no".to_string(),
            Some(Modal::Picker(_)) => " type to filter · ↑↓ choose · Enter pick · Esc close".to_string(),
            Some(Modal::Setup(s)) => s.hints().to_string(),
            Some(Modal::Help { .. }) => " ↑↓ PgUp/PgDn Home/End scroll · Esc close".to_string(),
            _ => " Esc close".to_string(),
        }
    } else if let Some(p) = &app.palette {
        match p.kind {
            PaletteKind::Files => " ↑↓ choose · Tab or Enter insert · PgUp/PgDn page · Esc close".to_string(),
            _ if !p.runs_selection() => " ↑↓ choose · Tab complete · Enter send as typed · Esc close".to_string(),
            _ => " ↑↓ choose · Tab complete · Enter run · PgUp/PgDn page · Esc close".to_string(),
        }
    } else if app.search.as_ref().is_some_and(|s| s.focused) {
        " type to search · Enter next · Shift+Enter previous · Esc close".to_string()
    } else if app.selected.is_some() && app.input.is_empty() {
        let key = app.keymap.label(crate::keymap::Action::Diff).to_uppercase();
        let dk = if app.selected_is_edit() { format!(" {key} diff · ") } else { " ".to_string() };
        format!("{dk}↑↓ move · → open · ← fold · Enter toggle · Esc or typing back to the draft")
    } else if app.quiet() {
        String::new()
    } else if app.busy {
        // the blocks key is said only once there is a block to step into
        let blocks = if app.has_navigable() { " · Shift+↑ blocks" } else { "" };
        format!(" Esc interrupt · Enter add a message · PgUp/PgDn scroll{blocks}")
    } else {
        let blocks = if app.has_navigable() { " · Shift+↑ blocks" } else { "" };
        format!(" Enter send · Ctrl+J newline · / commands · @ files · ! shell{blocks} · ? help")
    };
    let quiet = app.quiet();
    let suffix = if app.mouse_on || quiet { "" } else { " · mouse off · /settings mouse to enable" };
    let mut prefix = match (app.input.vim, app.modal.is_some()) {
        (Some(crate::input::VimMode::Normal), false) => " NORMAL · i insert ·".to_string(),
        (Some(crate::input::VimMode::Insert), false) => " INSERT · Esc normal ·".to_string(),
        _ => String::new(),
    };
    // scrolled up: say first how to get back down
    if !quiet && !app.follow && app.modal.is_none() && app.palette.is_none() && app.input.is_empty() {
        prefix = format!(" End bottom ·{prefix}");
    }
    // quiet, nothing to list: the vim mode alone, without a dot hanging at its end
    if quiet && left.is_empty() {
        prefix = prefix.trim_end_matches('·').trim_end().to_string();
    }
    let full = format!("{prefix}{left}{suffix}");
    let w = area.width as usize;
    // the keys come first, and the right side is whole pieces: the one that matters least goes first
    let keys_min = if app.status.is_some() { width(&full).min(w / 2) } else { width(&full).min(w) };
    // quiet: the message, queued requests and jobs ride in the status bar
    let mut pieces = if quiet { Vec::new() } else { hint_pieces(app, w.saturating_sub(keys_min)) };
    // a warning or an error clipped to the room is not lost: the whole of it goes to a toast, once
    if let Some((msg, _)) = app.status.clone().filter(|_| !quiet) {
        let room = w.saturating_sub(keys_min).saturating_sub(2).max(1);
        if width(&msg) > room && !app.status_toasted && app.status_level != Level::Info {
            app.status_toasted = true;
            let kind = match app.status_level {
                Level::Error => crate::toast::Kind::Error,
                _ => crate::toast::Kind::Info,
            };
            app.toast(kind, &msg);
        }
    }
    let mut right = join_pieces(app, &pieces);
    let mut right_w: usize = right.iter().map(|s| width(&s.content)).sum();
    while right_w > w.saturating_sub(keys_min) && !pieces.is_empty() {
        let low = pieces.iter().enumerate().min_by_key(|(_, p)| p.0).map(|(i, _)| i).unwrap_or(0);
        pieces.remove(low);
        right = join_pieces(app, &pieces);
        right_w = right.iter().map(|s| width(&s.content)).sum();
    }
    // the idle line keeps "? help" at its end; the others drop from the end (their last key is the least)
    let keep_last = left.ends_with("? help");
    // the keys and what stands on the right never touch: a message (Ctrl+C again to quit) is not a continuation of the last key
    let gap = if right_w > 0 { HINT_GAP } else { 0 };
    let left = fit_hints(&prefix, &left, suffix, keep_last, w.saturating_sub(right_w + gap));
    let pad = w.saturating_sub(width(&left) + right_w);
    // a change of state (idle, busy, a block, the palette, a dialog) fades the new keys in
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        left.hash(&mut h);
        h.finish()
    };
    let now = app.now();
    if app.fx.hint_key != key {
        if app.fx.hint_key != 0 && app.fx_on() {
            app.fx.hint_at = Some(now);
        }
        app.fx.hint_key = key;
    }
    let hint_k = if app.fx_on() {
        crate::anim::ease_out(crate::anim::progress(app.fx.hint_at, now, crate::motion::GLIDE_MS)) as f32
    } else {
        1.0
    };
    // the key names in the accent, the words between them dim
    let mut spans = Vec::new();
    for (i, part) in left.split(" · ").enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ".to_string(), base.fg(theme.border)));
        }
        match part.split_once(' ') {
            Some((key, rest)) if !key.is_empty() => {
                spans.push(Span::styled(key.to_string(), base.fg(theme.accent)));
                spans.push(Span::styled(format!(" {rest}"), base));
            }
            _ => spans.push(Span::styled(part.to_string(), base)),
        }
    }
    if hint_k < 1.0 {
        let ground = base.bg.unwrap_or(theme.bg);
        for s in spans.iter_mut() {
            if let Some(fg) = s.style.fg {
                s.style.fg = Some(crate::motion::fade(theme, ground, fg, hint_k));
            }
        }
    }
    // where each clickable piece of the right part sits
    let mut at = area.x as usize + width(&left) + pad;
    let mut hits = Vec::new();
    for (i, (_, hit, parts)) in pieces.iter().enumerate() {
        if i > 0 {
            at += width(" ·");
        }
        let w: usize = parts.iter().map(|s| width(&s.content)).sum();
        if let Some(h) = hit {
            hits.push((Rect::new(at as u16, area.y, w as u16, 1), *h));
        }
        at += w;
    }
    spans.push(Span::styled(" ".repeat(pad), base));
    spans.extend(right);
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    app.hint_hits.extend(hits);
}

/// Whole hints are dropped until the rest fits, so a key is never cut from its word: first the
/// `suffix` (mouse note), then `base` from its end (with `keep_last`, from the one before the last,
/// so the first hint, the main key, and the last, the help, stay); only what is left is truncated.
fn fit_hints(prefix: &str, base: &str, suffix: &str, keep_last: bool, w: usize) -> String {
    let mut parts: Vec<&str> = base.split(" · ").collect();
    let mut suffix = suffix;
    let build = |parts: &[&str], suffix: &str| format!("{prefix}{}{suffix}", parts.join(" · "));
    while width(&build(&parts, suffix)) > w {
        if !suffix.is_empty() {
            suffix = "";
        } else if keep_last && parts.len() > 2 {
            parts.remove(parts.len() - 2);
        } else if parts.len() > 1 {
            parts.pop();
        } else {
            break;
        }
    }
    truncate(&build(&parts, suffix), w)
}

/// The side panel at the right: the goal, the todo list and the files
/// changed this session (`path  +n −m`, newest first).
fn draw_panel(f: &mut Frame, app: &App, area: Rect) {
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
                let mut v = context_map(app, inner_w, area.height as usize);
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
        for (k, l) in body.into_iter().enumerate() {
            if k < 4 {
                lines.push(l);
            }
        }
        let (round, max) = (goal.round, goal.max);
        let state = match &goal.state {
            GoalState::Active if round > 0 => Some((format!("round {round}/{max}"), theme.dim())),
            GoalState::Active => None,
            GoalState::Waiting => Some(("waiting for your answer".to_string(), theme.style(theme.warn))),
            GoalState::Paused(why) => Some((format!("paused: {why} · /goal resume"), theme.dim())),
            GoalState::Done => Some(("met ✓ · /goal off clears it".to_string(), theme.style(theme.success))),
        };
        if let Some(state) = state {
            let body = crate::render::wrap::wrap_runs(&[state], inner_w, &[rail()], &[rail()]);
            let first = lines.len();
            lines.extend(body.into_iter().take(2));
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

/// `/context-live`: the whole history as a grid, a cell per `unit` tokens,
/// coloured by who wrote it and lit by how much of it the last call sent.
/// With the curator, the grid is its memory in zones: the anchor, the topic in
/// progress and the other topics, newest first, each under a line that says
/// what of it the call takes.
fn context_map(app: &App, w: usize, height: usize) -> Vec<Line<'static>> {
    use nexo_core::CellState;
    let theme = &app.theme;
    let fmt_k = crate::render::blocks::fmt_k;
    let mut out = vec![Line::from(Span::styled("context live", Style::default().add_modifier(Modifier::BOLD)))];
    let sent_of = |cells: &mut dyn Iterator<Item = &nexo_core::ContextCell>| {
        cells.fold((0, 0), |(s, m), c| match c.state {
            CellState::Dropped => (s, m + c.tokens),
            _ => (s + c.tokens, m + c.tokens),
        })
    };
    let (sent, mem) = sent_of(&mut app.map.iter());
    if app.map.is_empty() {
        out.push(Line::from(Span::styled("fills in with the next call", theme.dim())));
        return out;
    }
    let policy = app.context.as_ref().map(|c| c.policy.clone()).unwrap_or_default();
    let pct = sent * 100 / mem.max(1);
    out.push(Line::from(vec![
        Span::styled(format!("sent {}", fmt_k(sent)), theme.style(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" of {} · {pct}%", fmt_k(mem)), theme.dim()),
    ]));
    // the grid gets what the panel has left after the text around it
    let zoned = !app.map_zones.is_empty();
    let rows = height.saturating_sub(if zoned { 9 } else { 8 }).clamp(2, if zoned { 40 } else { 12 });
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
        // anchor, then the topic in progress, then the others by their latest message
        order.sort_by_key(|&z| (z != 0, !app.map_zones[z].active, std::cmp::Reverse(by[z].last().copied())));
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
    // still too many lines: the oldest topics go, counted at the end
    let mut hidden = 0;
    while zoned && groups.len() > 2 && lines_at(unit, &groups) + 1 > rows {
        groups.pop();
        hidden += 1;
    }
    // with animations off a changed cell does not blink
    let flash = app
        .map_at
        .filter(|_| app.opts.animations)
        .map(|t| t.elapsed().as_millis() as u64)
        .filter(|&ms| ms < MAP_FLASH_MS);
    for (z, ix) in &groups {
        if let Some(z) = *z {
            let zone = &app.map_zones[z];
            let (s, m) = sent_of(&mut ix.iter().map(|&i| &app.map[i]));
            let (mark, style, tag) = if z == 0 {
                ("◆ ", theme.style(theme.accent2), String::new())
            } else if zone.active {
                ("● ", theme.style(theme.accent).add_modifier(Modifier::BOLD), " now".to_string())
            } else if s > 0 {
                ("◐ ", theme.style(theme.info), " back".to_string())
            } else if zone.card {
                ("▭ ", theme.style(theme.info), " card".to_string())
            } else {
                ("○ ", theme.dim2(), String::new())
            };
            let nums = format!(" {}/{}{tag}", fmt_k(s), fmt_k(m));
            let label = truncate(&zone.label, w.saturating_sub(2 + width(&nums)).max(1));
            out.push(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(label, style),
                Span::styled(nums, if zone.active || s > 0 { theme.dim() } else { theme.dim2() }),
            ]));
        }
        let kinds = theme.kind_colours();
        let mut cells: Vec<Span<'static>> = Vec::new();
        for &i in ix {
            cells.extend(cell_spans(app, i, unit, flash.is_some(), &kinds));
        }
        // too long even at the largest unit: the newest part
        let cap = if z.is_some() { usize::MAX } else { rows * w };
        if cells.len() > cap {
            cells.drain(..cells.len() - cap);
        }
        while !cells.is_empty() {
            let rest = cells.split_off(cells.len().min(w));
            out.push(Line::from(std::mem::replace(&mut cells, rest)));
        }
    }
    if hidden > 0 {
        out.push(Line::from(Span::styled(format!("… {hidden} older topics"), theme.dim2())));
    }
    out.push(Line::from(Span::styled(format!("1 cell ≈ {} tok · {policy}", fmt_k(unit)), theme.dim())));
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
    let (recalled, left) = if zoned { ("░ back ", "· out") } else { ("", "· left out") };
    out.push(Line::from(vec![
        Span::styled(format!("█ sent ▒ cut {recalled}"), theme.dim()),
        Span::styled(left, theme.dim2()),
    ]));
    out
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

/// The cells of message `i` of the map at `unit` tokens a cell.
fn cell_spans(app: &App, i: usize, unit: u64, flash: bool, kinds: &[Color; 4]) -> impl Iterator<Item = Span<'static>> {
    use nexo_core::CellState;
    let theme = &app.theme;
    let c = &app.map[i];
    let colour = kinds[kind_ix(c.kind)];
    let (glyph, mut style) = match c.state {
        CellState::Whole => ("█", theme.style(colour)),
        CellState::Shrunk => ("▒", theme.style(colour)),
        CellState::Recalled => ("░", theme.style(colour)),
        CellState::Dropped => ("·", theme.dim2()),
    };
    // a cell that just changed lights up, then settles
    if flash && app.map_changed.get(i).copied().unwrap_or(false) {
        style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
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

fn draw_palette(f: &mut Frame, app: &mut App, transcript: Rect) {
    app.palette_rows.clear();
    let theme = app.theme.clone();
    let pop = app.palette_pop();
    let hovered = match app.hover {
        Hover::Palette(i) => Some(i),
        _ => None,
    };
    let (now, on) = (app.now(), app.fx_on());
    let sel = app.palette.as_ref().map(|p| p.selected);
    app.fx.pal_sel.note(sel, now, on);
    let glide = app.fx.pal_sel;
    let cwd = app.cwd.clone();
    let Some(p) = &mut app.palette else { return };
    if transcript.height == 0 || transcript.width < 10 {
        return;
    }
    let is_files = p.kind == PaletteKind::Files;
    let tw = transcript.width.min(if is_files { 160 } else { 140 });
    // the file picker shows the head of the selected file beside the list when there is room
    let preview_w = if is_files && tw >= 90 && !p.is_empty() { tw / 2 } else { 0 };
    let list_w = tw - preview_w;
    let cap = MAX_ROWS.min(transcript.height as usize);
    let mut view = p.view(cap);
    let footer = view.above + view.below > 0 && cap >= 3;
    if footer {
        view = p.view(cap - 1);
    }
    let list_h = view.entries.len().max(1) + usize::from(footer);
    let want_h = if preview_w > 0 { cap.max(list_h) } else { list_h } as u16;
    // the height it took for this filter: moving the selection through it never resizes the list
    // (typing does, as it leaves other rows)
    let key = {
        use std::hash::{Hash, Hasher};
        let mut k = std::collections::hash_map::DefaultHasher::new();
        (format!("{:?}", p.kind), &p.query, preview_w > 0).hash(&mut k);
        k.finish()
    };
    let (_, full_h) = app.dialog_size.palette.keep(key, f.area(), transcript, || (tw, want_h));
    let full_h = full_h.max(1);
    // it rises from the input: a few rows more each frame while it opens
    let h = ((full_h as f64 * pop).ceil() as u16).clamp(1, full_h);
    let r = Rect::new(transcript.x, transcript.y + transcript.height - h, tw, h);
    app.dialog_size.palette.drawn = r;
    // on a short screen a sliver of conversation above the list only distracts: clear it
    if transcript.height.saturating_sub(full_h) < 5 {
        clear_bg(f, transcript, &theme);
    }
    clear_bg(f, r, &theme);
    let avail = list_w as usize;
    let ground = theme.code_block();
    let (name_cols, args_cols) = p.columns();
    let slash = matches!(p.kind, PaletteKind::Commands);
    // aligned columns: the name, the argument hint (when wide enough), what it does
    // a file path is all there is on its row: it gets the whole width
    let name_w = if is_files { avail } else { (name_cols + if slash { 4 } else { 3 }).min(avail * 45 / 100).max(8) };
    let args_w = if avail >= 70 && args_cols > 0 { (args_cols + 2).min(avail * 25 / 100) } else { 0 };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(u16, usize)> = Vec::new();
    if view.entries.is_empty() {
        let q = truncate(&crate::render::wrap::clean(p.query.trim()), 24);
        let msg = match &p.kind {
            PaletteKind::Commands => match p.suggestion() {
                Some(s) => format!(" no command “{q}” · did you mean /{s}? Tab accept · /help lists all"),
                None => format!(" no command “{q}” · Enter send as text · /help lists all"),
            },
            PaletteKind::Files => format!(" no file matches “{q}” · {} files here", p.items.len()),
            PaletteKind::Args(c) => format!(" no value of /{c} matches “{q}”"),
        };
        let msg = truncate(&msg, avail);
        let pad = avail.saturating_sub(width(&msg));
        lines.push(Line::from(vec![
            Span::styled(msg, ground.patch(theme.dim())),
            Span::styled(" ".repeat(pad), ground),
        ]));
    }
    for entry in &view.entries {
        let (row, abs) = match entry {
            crate::palette::Entry::Header(title) => {
                // a section title: dim, with a rule to the edge
                let t = truncate(&format!(" {title} "), avail);
                let rule = "─".repeat(avail.saturating_sub(width(&t) + 1));
                lines.push(Line::from(vec![
                    Span::styled(t, ground.patch(theme.dim()).add_modifier(Modifier::BOLD)),
                    Span::styled(rule, ground.patch(theme.dim2())),
                    Span::styled(" ", ground),
                ]));
                continue;
            }
            crate::palette::Entry::Item(row, abs) => (row, *abs),
        };
        // the highlight glides from the old row to the new one
        let lvl = glide.level(abs, now);
        let style = if lvl >= 1.0 {
            theme.selection().add_modifier(Modifier::BOLD)
        } else if lvl > 0.0 {
            crate::motion::lit_style(&theme, ground, lvl)
        } else if hovered == Some(abs) {
            ground.patch(theme.hover())
        } else {
            ground
        };
        let lit = style.fg(theme.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        let mut spans = vec![Span::styled(if slash { " /" } else { " " }, style.fg(theme.accent))];
        let lead = if slash { 2 } else { 1 };
        // a path keeps its end, the file name: `…/render/markdown.rs`
        let (label, shifted) = if is_files {
            path_tail(&row.label, &row.hits, name_w.saturating_sub(lead + 1), avail < 60)
        } else {
            (truncate(&row.label, name_w.saturating_sub(lead + 1)), row.hits.clone())
        };
        let on_label = if row.alias.is_none() { shifted.as_slice() } else { &[] };
        spans.extend(lit_spans(&label, on_label, style.fg(theme.fg), lit));
        let mut used = lead + width(&label);
        if let Some(a) = &row.alias {
            // the alias the query matched, dim, right after the name: `/resume  (sessions)`
            let room = name_w.saturating_sub(used + 5);
            if room > 0 {
                let a = truncate(a, room);
                spans.push(Span::styled("  (", style.patch(theme.dim())));
                spans.extend(lit_spans(&a, &row.hits, style.patch(theme.dim()), lit));
                spans.push(Span::styled(")", style.patch(theme.dim())));
                used += width(&a) + 4;
            }
        }
        let pad = name_w.saturating_sub(used).max(1);
        spans.push(Span::styled(" ".repeat(pad), style));
        used += pad;
        if args_w > 0 {
            let a = truncate(&row.args, args_w.saturating_sub(2));
            let pad = args_w.saturating_sub(width(&a));
            spans.push(Span::styled(a, style.patch(theme.dim2())));
            spans.push(Span::styled(" ".repeat(pad), style));
            used += args_w;
        }
        let hint = truncate(&row.hint, avail.saturating_sub(used + 1));
        let pad = avail.saturating_sub(used + width(&hint));
        spans.push(Span::styled(hint, style.patch(theme.dim())));
        spans.push(Span::styled(" ".repeat(pad), style));
        rows.push((lines.len() as u16, abs));
        lines.push(Line::from(spans));
    }
    if footer {
        // where the list stands: what is hidden above and below, and the place of the selection
        let up = if view.above > 0 { format!(" ↑ {} more", view.above) } else { String::new() };
        let down = if view.below > 0 { format!("{} more ↓ ", view.below) } else { String::new() };
        let mid = format!("{}/{}", p.selected + 1, p.len());
        let gap = avail.saturating_sub(width(&up) + width(&down) + width(&mid));
        let left = gap / 2;
        lines.push(Line::from(vec![
            Span::styled(up, ground.patch(theme.dim())),
            Span::styled(" ".repeat(left), ground),
            Span::styled(mid, ground.patch(theme.dim2())),
            Span::styled(" ".repeat(gap - left), ground),
            Span::styled(down, ground.patch(theme.dim())),
        ]));
    }
    let list = Rect::new(r.x, r.y, list_w, h);
    f.render_widget(Paragraph::new(Text::from(lines)).style(ground), list);
    if preview_w > 0 {
        let pr = Rect::new(r.x + list_w, r.y, preview_w, h);
        let name = p.current().unwrap_or_default().to_string();
        if let Some(pv) = p.preview(&cwd, full_h.saturating_sub(1) as usize) {
            draw_preview(f, &theme, pr, &name, pv);
        }
    }
    app.palette_rows =
        rows.into_iter().filter(|(y, _)| *y < h).map(|(y, abs)| (Rect::new(r.x, r.y + y, list_w, 1), abs)).collect();
}

/// A path that does not fit, as `name  folder`: the file name stays whole and the folder is what is
/// cut (from the left, behind a `…`); on a narrow list every folder row takes that form, so the
/// rows line up; `hits` (positions in the path) are moved to match.
fn path_tail(path: &str, hits: &[usize], max: usize, name_first: bool) -> (String, Vec<usize>) {
    if max < 2 || (width(path) <= max && !(name_first && path.contains('/'))) {
        return (path.to_string(), hits.to_vec());
    }
    let chars: Vec<char> = path.chars().collect();
    let name_start = chars.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
    let name: Vec<char> = chars[name_start..].to_vec();
    let name_w = width(&name.iter().collect::<String>());
    // the file name alone does not fit: its end is cut, as before
    if name_w + 4 > max {
        let keep_from = chars.len().saturating_sub(max.saturating_sub(1));
        let from = keep_from.max(name_start.min(chars.len()));
        let tail: String = chars[from.min(chars.len())..].iter().collect();
        let hits = hits.iter().filter(|&&h| h >= from).map(|h| h - from + 1).collect();
        return (format!("…{tail}"), hits);
    }
    let dir: Vec<char> = chars[..name_start.saturating_sub(1)].to_vec();
    let room = max - name_w - 2;
    let mut out: Vec<char> = name.clone();
    let mut moved: Vec<usize> = hits.iter().filter(|&&h| h >= name_start).map(|h| h - name_start).collect();
    out.extend("  ".chars());
    // the folder: whole when it fits, else its last columns behind a `…`
    let mut keep = 0;
    let mut w = 1;
    if width(&dir.iter().collect::<String>()) <= room {
        out.extend(dir.iter());
        moved.extend(hits.iter().filter(|&&h| h < dir.len()).map(|h| h + name.len() + 2));
    } else {
        for c in dir.iter().rev() {
            let cw = unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            if w + cw > room {
                break;
            }
            w += cw;
            keep += 1;
        }
        let dropped = dir.len() - keep;
        out.push('…');
        out.extend(dir[dropped..].iter());
        moved.extend(hits.iter().filter(|&&h| h >= dropped && h < dir.len()).map(|h| h - dropped + 1 + name.len() + 2));
    }
    (out.iter().collect(), moved)
}

/// `text` with the characters at `hits` (positions in characters) in `lit`, the rest in `base`.
fn lit_spans(text: &str, hits: &[usize], base: Style, lit: Style) -> Vec<Span<'static>> {
    if hits.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    for (i, c) in text.chars().enumerate() {
        let on = hits.contains(&i);
        if on != run_lit && !run.is_empty() {
            out.push(Span::styled(std::mem::take(&mut run), if run_lit { lit } else { base }));
        }
        run_lit = on;
        run.push(c);
    }
    if !run.is_empty() {
        out.push(Span::styled(run, if run_lit { lit } else { base }));
    }
    out
}

/// The head of the file the `@` picker has selected: its name and size on
/// top, then its first lines with their numbers.
fn draw_preview(f: &mut Frame, theme: &Theme, r: Rect, name: &str, pv: &crate::palette::Preview) {
    let ground = theme.alt();
    let w = r.width.saturating_sub(1) as usize;
    let mut lines = Vec::new();
    let note = format!(" · {}", pv.note);
    let title = truncate(&format!(" {}", crate::render::wrap::clean(name)), w.saturating_sub(width(&note)).max(4));
    lines.push(Line::from(vec![
        Span::styled(title, ground.fg(theme.fg).add_modifier(Modifier::BOLD)),
        Span::styled(truncate(&note, w), ground.patch(theme.dim())),
    ]));
    let num_w = pv.lines.len().to_string().len();
    for (i, l) in pv.lines.iter().enumerate() {
        let num = format!(" {:>num_w$} ", i + 1);
        let text = truncate(&l.replace('\t', "    "), w.saturating_sub(num.len()));
        lines.push(Line::from(vec![
            Span::styled(num, ground.patch(theme.dim2())),
            Span::styled(text, ground.fg(theme.code_fg)),
        ]));
    }
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(Text::from(lines)).style(ground), r);
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
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
fn pop_style(theme: &Theme, colour: ratatui::style::Color, t: f64) -> Style {
    if t >= 1.0 || theme.mono {
        return theme.style(colour);
    }
    theme.style(mix(theme.modal_bg, colour, t as f32))
}

/// The scrollbar of a dialog, on its right border beside the `len` rows of its body
/// from `y`, and remembered for the mouse. `first`: the body line a list starts at.
#[allow(clippy::too_many_arguments)]
fn modal_scrollbar(
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
fn draw_question(f: &mut Frame, app: &mut App, area: Rect) {
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

/// The `/context` · `/usage` window: it grows open from the middle, a tab
/// row on top, `[✗]` on its border, the figures of the tab fill in.
fn draw_info(f: &mut Frame, app: &mut App, area: Rect) {
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
fn draw_agents(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let (now, fx) = (app.now(), app.fx_on());
    if let Some(Modal::Agents(v)) = &mut app.modal {
        v.draw(f, area, &theme, now, fx);
        app.dialog_size.modal.drawn = v.window;
    }
}

/// The text of a drawn buffer, row by row (tests).
pub fn rows_of(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buf.area;
    let mut rows = Vec::new();
    for y in 0..area.height {
        let mut s = String::new();
        for x in 0..area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        rows.push(s.trim_end().to_string());
    }
    rows
}

#[allow(dead_code)]
fn _unused(l: &Line) -> String {
    text_of(l)
}

/// The draft in rows that fit the box, the cursor as a lit cell; the rows scroll to keep it in sight.
/// The ghost: what finishes the word before the cursor, in the dim of the placeholder. Drawn after
/// the box takes its base colours, so they do not cover it.
fn paint_complete(f: &mut Frame, app: &App, editor: Rect) {
    if app.input.is_empty() {
        return;
    }
    let Some(s) = app.complete_suffix() else { return };
    let (_, (crow, ccol)) = crate::input::wrapped_rows(&app.input, editor.width as usize);
    let top = app.editor_top;
    if crow < top || crow - top >= editor.height as usize {
        return;
    }
    let x = editor.x + (ccol as u16).min(editor.width.saturating_sub(1));
    let room = (editor.x + editor.width).saturating_sub(x) as usize;
    // too long for the row: cut with an ellipsis; → still takes it all
    let shown = if crate::render::wrap::width(&s) > room {
        let mut t = String::new();
        for c in s.chars() {
            if crate::render::wrap::width(&t) + crate::render::wrap::width(c.encode_utf8(&mut [0; 4])) + 1 > room {
                break;
            }
            t.push(c);
        }
        t.push('…');
        t
    } else {
        s
    };
    f.buffer_mut().set_stringn(
        x,
        editor.y + (crow - top) as u16,
        &shown,
        room,
        app.theme.dim().add_modifier(Modifier::ITALIC),
    );
}

fn draw_draft(f: &mut Frame, app: &mut App, editor: Rect) {
    app.editor_area = editor;
    app.input.wrap_width = editor.width as usize;
    if app.input.is_empty() {
        // the placeholder (or a suggested message) takes the box; the caret sits on its first cell
        let cell = f.buffer_mut().cell_mut((editor.x, editor.y));
        if let Some(c) = cell {
            c.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        return;
    }
    let (rows, (crow, ccol)) = crate::input::wrapped_rows(&app.input, editor.width as usize);
    let h = editor.height as usize;
    let top = (crow + 1).saturating_sub(h);
    app.editor_top = top;
    let sel = app.input.selection_cells();
    let origins = crate::input::row_origins(&app.input, editor.width as usize);
    for (i, row) in rows.iter().skip(top).take(h).enumerate() {
        // the selected cells of this row
        if let (Some(((r0, c0), (r1, c1))), Some(&(line, start))) = (sel, origins.get(top + i)) {
            let n = row.chars().count();
            let (from, to) = match (line.cmp(&r0), line.cmp(&r1)) {
                (std::cmp::Ordering::Less, _) | (_, std::cmp::Ordering::Greater) => (n, n),
                _ => {
                    let f0 = if line == r0 { c0.saturating_sub(start) } else { 0 };
                    let t0 = if line == r1 { c1.saturating_sub(start).min(n) } else { n };
                    (f0.min(n), t0)
                }
            };
            if from < to {
                let cw = |s: &str| crate::render::wrap::width(s);
                let x0: usize = cw(&row.chars().take(from).collect::<String>());
                let x1: usize = cw(&row.chars().take(to).collect::<String>());
                for x in x0..x1.min(editor.width as usize) {
                    if let Some(c) = f.buffer_mut().cell_mut((editor.x + x as u16, editor.y + i as u16)) {
                        c.set_style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                }
            }
        }
        let r = Rect { y: editor.y + i as u16, height: 1, ..editor };
        f.render_widget(Paragraph::new(Span::styled(row.clone(), app.theme.base())), r);
    }
    if top == 0 {
        paint_command(f, app, editor);
    }
    if crow >= top && crow - top < h {
        let x = editor.x + (ccol as u16).min(editor.width.saturating_sub(1));
        if let Some(c) = f.buffer_mut().cell_mut((x, editor.y + (crow - top) as u16)) {
            c.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
    }
}

/// A chip that was just added to the draft (a folded paste, a pasted attachment) is lit once, then rests.
fn paint_chips(f: &mut Frame, app: &mut App, editor: Rect, rows: &[String]) {
    if !app.fx_on() {
        return;
    }
    let now = app.now();
    app.note_chips(now);
    let theme = &app.theme;
    let from = theme.base().fg.unwrap_or(theme.fg);
    for (label, at) in &app.fx.chip_seen {
        // a pasted path is lit only until the next key: the same word typed by hand is not a chip
        if label.starts_with('@') && !app.input.just_pasted() {
            continue;
        }
        let k = crate::fx_chrome::fade_out(Some(*at), now, crate::fx_chrome::CHIP_MS);
        if k <= 0.0 {
            continue;
        }
        let colour = crate::motion::fade(theme, from, theme.accent, k);
        let w = width(label);
        for (i, row) in rows.iter().take(editor.height as usize).enumerate() {
            for (b, _) in row.match_indices(label.as_str()) {
                let x0 = width(&row[..b]);
                for x in x0..(x0 + w).min(editor.width as usize) {
                    if let Some(c) = f.buffer_mut().cell_mut((editor.x + x as u16, editor.y + i as u16)) {
                        c.set_style(Style::default().fg(colour));
                    }
                }
            }
        }
    }
}

/// What the slash word a draft starts with is: a real command, one that does not exist, or not
/// finished being typed (a prefix of a real one).
#[derive(Debug, PartialEq)]
pub enum SlashWord {
    Known,
    Unknown,
    Typing,
}

/// The `/word` at the start of `text`, with what it is; None when the draft is not a command.
pub fn slash_word<'a>(text: &'a str, custom: &[crate::custom::CustomCommand]) -> Option<(&'a str, SlashWord)> {
    let rest = text.strip_prefix('/')?;
    let name = rest.split(char::is_whitespace).next().unwrap_or("");
    if name.is_empty() || name.contains('/') {
        // "/" alone, or a path such as /etc/hosts
        return None;
    }
    let word = &text[..name.len() + 1];
    let known = crate::commands::lookup(name).is_some() || custom.iter().any(|c| c.name == name);
    let finished = rest.len() > name.len();
    let kind = if known {
        SlashWord::Known
    } else if finished {
        SlashWord::Unknown
    } else {
        SlashWord::Typing
    };
    Some((word, kind))
}

/// The command word of the draft in colour: the accent once it names a real command (it lights
/// up once as it does), the error colour when a finished word names none.
fn paint_command(f: &mut Frame, app: &mut App, editor: Rect) {
    let text = app.input.text();
    let Some((word, kind)) = slash_word(&text, &app.custom_commands) else {
        app.fx.cmd_lit = None;
        return;
    };
    let theme = app.theme.clone();
    let now = app.now();
    let colour = match kind {
        SlashWord::Known => {
            if !matches!(&app.fx.cmd_lit, Some((w, _)) if w == word) {
                app.fx.cmd_lit = Some((word.to_string(), now));
            }
            theme.accent
        }
        SlashWord::Unknown => {
            app.fx.cmd_lit = None;
            theme.error
        }
        SlashWord::Typing => {
            app.fx.cmd_lit = None;
            return;
        }
    };
    let n = width(word).min(editor.width as usize);
    let since = app.fx.cmd_lit.as_ref().map(|c| c.1);
    let moving =
        app.opts.animations && kind == SlashWord::Known && crate::motion::within(since, now, crate::motion::LIT_MS);
    let p = crate::anim::progress(since, now, crate::motion::LIT_MS) as f32;
    for x in 0..n {
        // a light runs once along the word, then it rests in the accent
        let c = if moving {
            let d = (p * (n as f32 + 4.0) - x as f32).abs();
            crate::motion::fade(&theme, colour, theme.fg, (1.0 - d / 3.0).max(0.0))
        } else {
            colour
        };
        if let Some(cell) = f.buffer_mut().cell_mut((editor.x + x as u16, editor.y)) {
            cell.set_style(Style::default().fg(c).add_modifier(Modifier::BOLD));
        }
    }
}

/// Below this the screen cannot hold a draft, a status line and a line of text: a notice is drawn instead.
pub const MIN_W: u16 = 24;
pub const MIN_H: u16 = 6;
/// A window this short or shorter gives its keys line to the transcript.
pub const SHORT_H: u16 = 12;

/// The files panel grows with the window: the narrowest that fits, a wider one on a big screen.
pub fn files_panel_width(total: u16) -> u16 {
    match total {
        0..=119 => FILES_PANEL_W,
        120..=159 => FILES_PANEL_W + 6,
        _ => FILES_PANEL_W + 12,
    }
}

/// A dialog is open but its window does not fit: one line says so (the keys still reach the dialog),
/// instead of a screen that looks frozen.
fn draw_too_narrow(f: &mut Frame, theme: &Theme, area: Rect, narrow: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let msg = if narrow { "make the window wider" } else { "make the window taller" };
    let msg: String = truncate(msg, area.width as usize);
    let y = area.y + area.height / 2;
    let line = Line::from(Span::styled(msg, theme.style(theme.warn))).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
}

/// The notice under the minimum size: what is wrong and what the window has, nothing cut mid-word.
fn draw_too_small(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let need = format!("needs {MIN_W}x{MIN_H}");
    let have = format!("has {}x{}", area.width, area.height);
    let lines: Vec<Line<'static>> = ["window too small".to_string(), need, have]
        .into_iter()
        .map(|s| s.chars().take(area.width as usize).collect::<String>())
        .enumerate()
        .map(|(i, s)| {
            let style = if i == 0 { theme.style(theme.warn).add_modifier(Modifier::BOLD) } else { theme.dim() };
            Line::from(Span::styled(s, style)).alignment(ratatui::layout::Alignment::Center)
        })
        .collect();
    let h = lines.len().min(area.height as usize) as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(Paragraph::new(Text::from(lines)), Rect { y: top, height: h, ..area });
}

#[cfg(test)]
mod tests {
    #[test]
    fn search_highlight_survives_case_folds_that_move_character_boundaries() {
        use ratatui::style::Style;
        use ratatui::text::Line;
        // "Ⱥ" (2 bytes) folds to 3, "ẞ" (3 bytes) folds to 2: same length, other boundaries
        let l = super::highlight(Line::from("Ⱥẞ ñ"), "ß", Style::default(), true);
        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Ⱥẞ ñ");
        let l = super::highlight(Line::from("año ÑU"), "ñ", Style::default(), false);
        let parts: Vec<&str> = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(parts, ["a", "ñ", "o ", "Ñ", "U"]);
    }

    #[test]
    fn a_long_path_keeps_its_file_name() {
        use super::{path_tail, width};
        let (t, h) = path_tail("crates/nexo-tui/src/render/markdown.rs", &[27, 28], 16, false);
        assert_eq!(t, "markdown.rs  …er");
        assert_eq!(width(&t), 16);
        assert_eq!(h, [0, 1], "the marks stay on the m and a of markdown");
        // the folder is what is cut, and as much of it as fits stays
        let (t, _) = path_tail("crates/nexo-bench/src/tasks/inventory.rs", &[], 38, false);
        assert_eq!(t, "inventory.rs  …es/nexo-bench/src/tasks");
        assert_eq!(
            path_tail("crates/nexo-bench/src/tasks/inventory.rs", &[], 30, false).0,
            "inventory.rs  …bench/src/tasks"
        );
        // on a narrow list the short paths take the same form, so the rows line up
        assert_eq!(path_tail("src/mod.rs", &[], 30, true).0, "mod.rs  src");
        assert_eq!(path_tail("a.rs", &[0], 16, true), ("a.rs".to_string(), vec![0]));
        assert_eq!(path_tail("a.rs", &[0], 16, false), ("a.rs".to_string(), vec![0]));
    }

    #[test]
    fn hints_that_do_not_fit_are_dropped_whole() {
        let h = " Enter send · Ctrl+J newline · / commands · @ files";
        assert_eq!(super::fit_hints("", h, "", true, 40), " Enter send · Ctrl+J newline · @ files");
        assert_eq!(super::fit_hints("", h, "", true, 100), h);
        assert!(super::fit_hints("", h, "", true, 5).chars().count() <= 5);
    }

    #[test]
    fn status_pieces_follow_the_configured_order() {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(status_pieces(&[]).len(), 12);
        assert_eq!(status_pieces(&v(&["Context", "model", "bogus", "context"])), vec!["context", "model"]);
        assert!(status_pieces(&v(&["bogus"])).is_empty());
    }

    use super::*;
    use crate::theme::Theme;

    #[test]
    fn the_comet_runs_along_the_rule_and_keeps_its_width() {
        let t = Theme::nexo();
        for frame in [0, 7, 30, 200] {
            let spans = comet(&t, 40, frame);
            let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, "─".repeat(40));
        }
        let lit = |frame| comet(&t, 40, frame).iter().any(|s| s.style.fg == Some(t.accent));
        assert!((0..60).any(lit), "the head is drawn in the accent somewhere along the way");
        assert!(comet(&t, 0, 3).is_empty());
    }

    #[test]
    fn the_gauge_colour_follows_the_share() {
        let t = Theme::nexo();
        assert_eq!(t.level_color(0), t.success);
        assert_eq!(t.level_color(49), t.success);
        assert_eq!(t.level_color(50), t.warn);
        assert_eq!(t.level_color(74), t.warn);
        assert_eq!(t.level_color(75), t.warn2);
        assert_eq!(t.level_color(89), t.warn2);
        assert_eq!(t.level_color(90), t.error);
        assert_eq!(t.level_color(130), t.error);
        assert_eq!(share(92, 100), 92);
        assert_eq!(gauge_cells(0.5, 8), ("████".to_string(), 4));
        assert_eq!(gauge_cells(0.11, 8), ("▉".to_string(), 7));
        assert_eq!(gauge_cells(1.0, 8), ("████████".to_string(), 0));
        assert_eq!(gauge_cells(0.0, 8), (String::new(), 8));
        assert_eq!(line_gauge(0.5, 10), ("━━━━━".to_string(), "━━━━━".to_string()));
        assert_eq!(line_gauge(0.25, 10), ("━━╸".to_string(), "━━━━━━━".to_string()));
        assert_eq!(line_gauge(1.0, 4), ("━━━━".to_string(), String::new()));
        assert_eq!(line_gauge(0.0, 4), (String::new(), "━━━━".to_string()));
    }

    #[test]
    fn every_theme_has_an_orange() {
        for n in crate::theme::NAMES {
            let t = Theme::by_name(n).unwrap();
            if !t.mono {
                assert_ne!(t.warn2, t.warn, "{n}");
                assert_ne!(t.warn2, t.error, "{n}");
            }
        }
    }
}
