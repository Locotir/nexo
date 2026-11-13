use super::*;

pub(super) fn draw_palette(f: &mut Frame, app: &mut App, transcript: Rect) {
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
            let a = cut_args(&row.args, args_w.saturating_sub(2));
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

/// An argument hint cut to `max` cells at a `|` boundary, never mid-word: `[ask|accept-edits…`.
/// When not even the first choice fits, the hint is left out.
pub(super) fn cut_args(args: &str, max: usize) -> String {
    if width(args) <= max {
        return args.to_string();
    }
    let mut end = None;
    for (i, c) in args.char_indices() {
        if c == '|' && width(&args[..i]) < max {
            end = Some(i);
        }
    }
    end.map_or_else(String::new, |i| format!("{}…", &args[..i]))
}

/// A path that does not fit, as `name  folder`: the file name stays whole and the folder is what is
/// cut (from the left, behind a `…`); on a narrow list every folder row takes that form, so the
/// rows line up; `hits` (positions in the path) are moved to match.
pub(super) fn path_tail(path: &str, hits: &[usize], max: usize, name_first: bool) -> (String, Vec<usize>) {
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
