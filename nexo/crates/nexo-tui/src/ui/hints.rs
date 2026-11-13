use super::*;

/// The session name as the hint bar shows it: never longer than this, so the
/// keys on the left always stay visible.
pub const TITLE_COLS: usize = 24;

/// The pieces of the right part of the hint bar, each already bounded, with the priority that
/// keeps it (the lowest goes first when the bar is narrow): the state (a flash message, or the
/// session), a todo count, running jobs, shared memory, queued messages.
/// One piece of the right part of the keys row: its priority, what a click on it opens, its spans.
pub(super) type Piece = (u8, Option<HintHit>, Vec<Span<'static>>);

pub(super) fn hint_pieces(app: &App, status_room: usize) -> Vec<Piece> {
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
pub(super) fn join_pieces(app: &App, pieces: &[Piece]) -> Vec<Span<'static>> {
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
pub(super) fn quiet_hints_on(app: &App) -> bool {
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
pub(super) fn mem_waiting(spinner: usize) -> String {
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

pub(super) fn draw_hints(f: &mut Frame, app: &mut App, area: Rect) {
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
pub(super) fn fit_hints(prefix: &str, base: &str, suffix: &str, keep_last: bool, w: usize) -> String {
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
