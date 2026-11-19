use super::*;

pub(super) fn draw_input(f: &mut Frame, app: &mut App, area: Rect) {
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
        let top = app.editor_top;
        paint_chips(f, app, editor, top);
    }
    paint_complete(f, app, editor);
    // on the rule above the box, at the right: the reasoning level and, for a long or
    // multiline draft, its size; never over the text
    if let Some((x, label)) = rule_tag(app, area.width).filter(|_| area.y > 0) {
        let w = crate::render::wrap::width(&label) as u16;
        let r = Rect { x: area.x + x, y: area.y - 1, width: w, height: 1 };
        // at the top level the letters wear a gradient (see `fx_effort`); otherwise the tag is dim
        let tag = match app.effort_tag_spans(&label, theme.dim2()) {
            Some(spans) => Paragraph::new(Line::from(spans)),
            None => Paragraph::new(Span::styled(label, theme.dim2())),
        };
        f.render_widget(tag, r);
    }
    // an empty box says what it takes, or shows the suggested next message (Tab or → takes it whole)
    if app.input.is_untouched() && app.palette.is_none() && editor.width > 4 {
        let shared;
        let pend;
        let queued;
        let tip;
        let mut tip_shown = None;
        // the hint must fit whole: a cut tip or a cut count of queued messages says less than nothing
        let width_room = (editor.width as usize).saturating_sub(2);
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
        } else if let Some(s) = queued_hint(app, width_room) {
            queued = s;
            queued.as_str()
        } else if app.busy {
            if app.quiet() {
                "add a message"
            } else {
                "type a message: it joins the turn at its next step"
            }
        } else if let Some((id, title, _)) = app.shared.last() {
            let name = if title.is_empty() { id } else { title };
            shared = format!("⊕ what «{name}» knows goes with your next message · /shared-memory off drops it");
            shared.as_str()
        } else if let Some((t, s)) = tip_for_box(app, width_room) {
            tip_shown = Some(t);
            tip = s;
            tip.as_str()
        } else {
            "ask anything"
        };
        // one cell right of the caret, so the caret never covers its first letter
        let ph = Rect { x: editor.x + 2, width: editor.width - 2, height: 1, ..editor };
        f.render_widget(Paragraph::new(Span::styled(truncate(text, ph.width as usize), theme.dim2())), ph);
        if let Some(t) = tip_shown {
            let turn = app.turns_ended;
            app.hint_state.note_shown(t, turn);
        }
    }
}

/// Requests wait and the box is empty: say how to take one back (the short form when the long does not fit). Enter
/// sends them on only while a turn runs; idle (an Esc kept them) they join the next message.
fn queued_hint(app: &App, room: usize) -> Option<String> {
    if !app.opts.hints || app.queued_msgs.is_empty() {
        return None;
    }
    let long = if app.busy {
        "↑ edit queued messages · Enter sends now"
    } else {
        "↑ edit queued · joins your next message"
    };
    [long, "↑ edit queued"]
        .into_iter()
        .find(|s| width(s) <= room)
        .map(String::from)
}

/// A key worth knowing, in the quiet moments only: idle, not in vim's normal mode, no window, list or search open,
/// and room for the whole line.
fn tip_for_box(app: &App, room: usize) -> Option<(crate::hints::Tip, String)> {
    if !app.opts.hints
        || app.quiet()
        || app.busy
        || app.modal.is_some()
        || app.search.is_some()
        || app.input.vim == Some(crate::input::VimMode::Normal)
    {
        return None;
    }
    app.hint_state.tip(app.turns_ended, &app.keymap).filter(|(_, s)| width(s) <= room)
}

/// The newest queued requests as dim rows (`◷ first line`): a click takes one back into the draft.
pub(super) fn draw_todo_strip(f: &mut Frame, app: &mut App, area: Rect) {
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

/// How long a row taken out of the queue stays drawn: a beat of `›`, then it sweeps out.
pub const TAKEN_MS: u64 = 350;
const TAKEN_LIGHT_MS: u64 = 150;
/// The soft edge of the sweep, in cells.
const TAKEN_EDGE: f32 = 6.0;

/// The taken rows still on screen.
pub(crate) fn taken_live(app: &App) -> usize {
    if !app.fx_on() {
        return 0;
    }
    let now = app.now();
    app.fx.queue_gone.iter().filter(|(_, at)| crate::motion::within(Some(*at), now, TAKEN_MS)).count()
}

/// A queued row `ms` after the agent took it, as a pure function of the time: the mark turns from the
/// clock into `›` in the accent while the text lights towards the full colour, then the row is swept out
/// from the left to the background. Nothing moves; only colour changes.
pub(crate) fn taken_row(theme: &Theme, text: &str, ms: u64, width: usize) -> Line<'static> {
    use crate::motion::fade;
    let lit = (ms as f32 / TAKEN_LIGHT_MS as f32).clamp(0.0, 1.0);
    let sweep = (ms.saturating_sub(TAKEN_LIGHT_MS) as f32 / (TAKEN_MS - TAKEN_LIGHT_MS) as f32).clamp(0.0, 1.0);
    let body = truncate(&format!(" ◷ {text}"), width);
    let cells: Vec<char> = body.chars().collect();
    // the front of the sweep, from before the first cell to past the last plus its soft edge
    let front = sweep * (cells.len() as f32 + TAKEN_EDGE);
    let spans = cells
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let gone = ((front - i as f32) / TAKEN_EDGE).clamp(0.0, 1.0);
            let (c, base, bold) = if i == 1 {
                ('›', fade(theme, theme.dim, theme.accent, 1.0), true)
            } else {
                (c, fade(theme, theme.dim, theme.fg, lit), false)
            };
            let mut st = Style::default().fg(fade(theme, base, theme.bg, gone));
            if bold || theme.mono {
                st = st.add_modifier(Modifier::BOLD);
            }
            // without gradients (16 colours) the sweep is a cut: a cell is either there or not
            Span::styled(if gone >= 1.0 { " ".to_string() } else { c.to_string() }, st)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

pub(super) fn draw_queue(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let rows = area.height as usize;
    let (now, on) = (app.now(), app.fx_on());
    app.fx.queue_gone.retain(|(_, at)| crate::motion::within(Some(*at), now, TAKEN_MS));
    // the taken rows take the places at the top, where the oldest message waited
    let gone: Vec<(String, u64)> = app
        .fx
        .queue_gone
        .iter()
        .filter(|_| on)
        .map(|(t, at)| (t.clone(), now.saturating_duration_since(*at).as_millis() as u64))
        .collect();
    let skip_gone = (gone.len() + app.queued_msgs.len()).saturating_sub(rows);
    let mut y = 0usize;
    for (text, ms) in gone.iter().skip(skip_gone) {
        let r = Rect { y: area.y + y as u16, height: 1, ..area };
        let line = text.lines().next().unwrap_or("");
        f.render_widget(Paragraph::new(taken_row(&app.theme, line, *ms, r.width as usize)), r);
        y += 1;
    }
    let first = app.queued_msgs.len().saturating_sub(rows.saturating_sub(y));
    let mut hits = Vec::new();
    // a request that just joined the queue fades in
    let queued = &app.queued_msgs;
    app.fx.queue_seen.retain(|(t, _)| queued.contains(t));
    for text in queued {
        if !app.fx.queue_seen.iter().any(|(t, _)| t == text) {
            app.fx.queue_seen.push((text.clone(), now));
        }
    }
    for (k, text) in app.queued_msgs.iter().enumerate().skip(first).take(rows.saturating_sub(y)) {
        let r = Rect { y: area.y + y as u16, height: 1, ..area };
        y += 1;
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

/// The first visible row of the box: where it was, moved only as far as keeps the cursor row in
/// sight, so a click or an arrow inside the window does not slide the text.
fn scroll_top(was: usize, crow: usize, h: usize, rows: usize) -> usize {
    let top = if crow < was {
        crow
    } else if crow >= was + h {
        crow + 1 - h
    } else {
        was
    };
    top.min(rows.saturating_sub(h))
}

fn draw_draft(f: &mut Frame, app: &mut App, editor: Rect) {
    app.editor_area = editor;
    app.input.wrap_width = editor.width as usize;
    if app.input.is_untouched() {
        app.editor_top = 0;
        // the placeholder (or a suggested message) takes the box; the caret sits on its first cell
        let cell = f.buffer_mut().cell_mut((editor.x, editor.y));
        if let Some(c) = cell {
            c.set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        return;
    }
    let (rows, (crow, ccol)) = crate::input::wrapped_rows(&app.input, editor.width as usize);
    let h = editor.height as usize;
    let top = scroll_top(app.editor_top, crow, h, rows.len());
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
fn paint_chips(f: &mut Frame, app: &mut App, editor: Rect, top: usize) {
    if !app.fx_on() {
        return;
    }
    let w_box = editor.width as usize;
    let (rows, _) = crate::input::wrapped_rows(&app.input, w_box);
    let origins = crate::input::row_origins(&app.input, w_box);
    let lines: Vec<String> = app.input.text().split('\n').map(String::from).collect();
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
        for (r, x0, x1) in chip_spans(&lines, &origins, &rows, label) {
            let Some(i) = r.checked_sub(top).filter(|i| *i < editor.height as usize) else { continue };
            for x in x0..x1.min(w_box) {
                if let Some(c) = f.buffer_mut().cell_mut((editor.x + x as u16, editor.y + i as u16)) {
                    c.set_style(Style::default().fg(colour));
                }
            }
        }
    }
}

/// The cells `label` covers in the laid-out rows: `(row, first cell, end cell)`, one per row it
/// touches, so a chip that wraps over two rows is lit on both. `origins` is the `(line, first
/// character)` each row starts at (see `row_origins`).
fn chip_spans(
    lines: &[String],
    origins: &[(usize, usize)],
    rows: &[String],
    label: &str,
) -> Vec<(usize, usize, usize)> {
    let n = label.chars().count();
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        for (b, _) in line.match_indices(label) {
            let s = line[..b].chars().count();
            for (r, (&(l, start), row)) in origins.iter().zip(rows).enumerate() {
                let (from, to) = (s.max(start), (s + n).min(start + row.chars().count()));
                if l == li && from < to {
                    let cells = |k: usize| width(&row.chars().take(k - start).collect::<String>());
                    out.push((r, cells(from), cells(to)));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_that_holds_the_cursor_row_does_not_scroll() {
        // 20 rows in an 8-row box, the window on rows 12..20
        assert_eq!(scroll_top(12, 18, 8, 20), 12, "Up inside the window leaves the text where it is");
        assert_eq!(scroll_top(12, 12, 8, 20), 12, "the first visible row is inside too");
        assert_eq!(scroll_top(12, 11, 8, 20), 11, "above the window it scrolls just enough");
        assert_eq!(scroll_top(0, 8, 8, 20), 1, "below it, the cursor row lands on the last row");
        assert_eq!(scroll_top(15, 3, 8, 6), 0, "a draft that shrank to fit goes back to the top");
    }

    #[test]
    fn a_chip_split_over_two_rows_is_found_on_both() {
        let label = "[Pasted text #1 +20 lines]";
        let line = "x [Pasted text #1 +20 lines]".to_string();
        let rows = vec!["x [Pasted text #1 ".to_string(), "+20 lines]".to_string()];
        let spans = chip_spans(&[line], &[(0, 0), (0, 18)], &rows, label);
        assert_eq!(spans, vec![(0, 2, 18), (1, 0, 10)]);
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
