//! Drawing the diff viewer: header, file list, the diff of the selected file, keys line.

use std::collections::HashSet;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Box_, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use super::collect::{Area, FileDiff, Kind, Status};
use super::{DiffView, Focus, SIDE_MIN_W};
use crate::render::code::{highlight, lang_of_path};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// Files with more lines than this are shown without syntax colour.
const HIGHLIGHT_MAX_LINES: usize = 4000;
/// The list is a side column from this width, a strip on top below it.
const LIST_SIDE_MIN_W: u16 = 100;
const KEYS: &str = "↑↓ file  Tab pane  n/p hunk  z fold  f all  s split  y copy  PgUp/PgDn scroll  Esc close";

/// The key hints in `w` columns: whole items from the left, and the last one (the way out) always kept.
fn fit_keys(keys: &str, w: usize) -> String {
    if width(keys) <= w {
        return keys.to_string();
    }
    let mut items: Vec<&str> = keys.split("  ").collect();
    let last = items.pop().unwrap_or_default();
    let mut out = String::new();
    for it in items {
        let next = if out.is_empty() { it.to_string() } else { format!("{out}  {it}") };
        if width(&next) + 2 + width(last) > w {
            break;
        }
        out = next;
    }
    let s = if out.is_empty() { last.to_string() } else { format!("{out}  {last}") };
    truncate(&s, w)
}

/// What one screen row of the body shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Hunk(usize),
    Line(usize, usize),
    /// Side by side: the old line and the new line (either may be missing).
    Pair(usize, Option<usize>, Option<usize>),
    Note,
}

/// The rows of a file and the row each hunk starts at. A folded hunk keeps its header only.
pub fn build_rows(f: &FileDiff, fi: usize, folded: &HashSet<(usize, usize)>, side: bool) -> (Vec<Row>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut starts = Vec::new();
    if f.hunks.is_empty() {
        rows.push(Row::Note);
    }
    for (h, hunk) in f.hunks.iter().enumerate() {
        starts.push(rows.len());
        rows.push(Row::Hunk(h));
        if folded.contains(&(fi, h)) {
            continue;
        }
        if !side {
            rows.extend((0..hunk.lines.len()).map(|l| Row::Line(h, l)));
            continue;
        }
        let lines = &hunk.lines;
        let mut i = 0;
        while i < lines.len() {
            if lines[i].kind == Kind::Context {
                rows.push(Row::Pair(h, Some(i), Some(i)));
                i += 1;
                continue;
            }
            let (mut dels, mut adds) = (Vec::new(), Vec::new());
            while i < lines.len() && lines[i].kind != Kind::Context {
                if lines[i].kind == Kind::Del {
                    dels.push(i);
                } else {
                    adds.push(i);
                }
                i += 1;
            }
            for k in 0..dels.len().max(adds.len()) {
                rows.push(Row::Pair(h, dels.get(k).copied(), adds.get(k).copied()));
            }
        }
    }
    (rows, starts)
}

fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// The part of `spans` from column `skip` for `w` columns, on `base`, padded with `base` to `w`.
fn slice(spans: &[Span<'static>], skip: usize, w: usize, base: Style) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let (mut col, mut used) = (0usize, 0usize);
    for sp in spans {
        let mut text = String::new();
        for c in sp.content.chars() {
            let cw = c.width().unwrap_or(0);
            if col >= skip && used + cw <= w {
                text.push(c);
                used += cw;
            }
            col += cw;
        }
        if !text.is_empty() {
            out.push(Span::styled(text, base.patch(sp.style)));
        }
    }
    if used < w {
        out.push(Span::styled(" ".repeat(w - used), base));
    }
    out
}

fn tail_trunc(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    let mut keep: Vec<char> = Vec::new();
    let mut used = 1;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            break;
        }
        keep.push(c);
        used += cw;
    }
    keep.reverse();
    format!("…{}", keep.into_iter().collect::<String>())
}

fn status_style(s: Status, theme: &Theme) -> Style {
    match s {
        Status::Added | Status::Untracked => theme.style(theme.diff_add),
        Status::Deleted => theme.style(theme.diff_del),
        Status::Renamed => theme.style(theme.accent2),
        Status::Modified => theme.style(theme.accent),
    }
}

/// The two-column tag of git status: the letter in the first column when staged, in the second otherwise.
fn tag(f: &FileDiff) -> String {
    let l = f.status.letter();
    match f.area {
        Area::Staged | Area::Session => format!("{l} "),
        Area::Unstaged => format!(" {l}"),
        Area::Untracked => "??".to_string(),
    }
}

fn ensure_highlight(dv: &mut DiffView, theme: &Theme) {
    if dv.hl.contains_key(&dv.sel) {
        return;
    }
    let Some(f) = dv.files.get(dv.sel) else { return };
    let plain = f.line_count() > HIGHLIGHT_MAX_LINES;
    let lang = lang_of_path(&f.path).to_string();
    let hl = f
        .hunks
        .iter()
        .map(|h| {
            let code = h.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
            let lines = if plain || theme.syntax.is_none() {
                code.split('\n').map(|l| Line::from(crate::render::code::detab(l))).collect()
            } else {
                highlight(&code, &lang, theme)
            };
            lines.into_iter().map(|l: Line<'static>| l.spans).collect()
        })
        .collect();
    dv.hl.insert(dv.sel, hl);
}

pub fn draw(f: &mut Frame, dv: &mut DiffView, area: Rect, theme: &Theme) {
    f.render_widget(Box_::default().style(theme.base()), area);
    if area.width < 30 || area.height < 6 {
        let p = Paragraph::new("Too small | Esc close").style(theme.dim());
        f.render_widget(p, area);
        return;
    }
    dv.eff_side = dv.side && area.width >= SIDE_MIN_W;
    let note_h = u16::from(dv.note.is_some());
    let [head, note, main, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(note_h),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, dv, head, theme);
    if let Some(n) = &dv.note {
        f.render_widget(Paragraph::new(format!(" {}", truncate(n, area.width as usize - 2))).style(theme.dim()), note);
    }
    let w = area.width as usize - 2;
    let keys = dv.hint.as_deref().map_or_else(|| fit_keys(KEYS, w), |h| truncate(h, w));
    f.render_widget(Paragraph::new(format!(" {keys}")).style(theme.dim()), foot);
    if dv.files.is_empty() {
        dv.list_area = Rect::default();
        dv.body_area = Rect::default();
        let text = if dv.loading {
            "Reading changes…".to_string()
        } else if let Some(e) = &dv.error {
            truncate(e, area.width as usize - 4)
        } else {
            "No changes".to_string()
        };
        let mid = Rect { y: main.y + main.height / 2, height: 1, ..main };
        f.render_widget(Paragraph::new(text).alignment(Alignment::Center).style(theme.dim()), mid);
        return;
    }
    let (list, body) = if area.width >= LIST_SIDE_MIN_W {
        let w = (area.width / 4).clamp(24, 40);
        let [l, m, r] =
            Layout::horizontal([Constraint::Length(w), Constraint::Length(1), Constraint::Min(10)]).areas(main);
        draw_sep(f, m, theme);
        (l, r)
    } else {
        let h = (dv.files.len() as u16).clamp(1, 4).min(main.height.saturating_sub(4));
        let [l, r] = Layout::vertical([Constraint::Length(h.max(1)), Constraint::Min(3)]).areas(main);
        (l, r)
    };
    dv.list_area = list;
    draw_list(f, dv, list, theme);
    draw_body(f, dv, body, theme);
}

fn draw_header(f: &mut Frame, dv: &DiffView, area: Rect, theme: &Theme) {
    let adds: usize = dv.files.iter().map(|x| x.added).sum();
    let dels: usize = dv.files.iter().map(|x| x.removed).sum();
    let n = |a: Area| dv.files.iter().filter(|x| x.area == a).count();
    let mut spans = vec![
        Span::styled(" Diff ", theme.highlight()),
        Span::styled(format!("  {} files  ", dv.files.len()), Style::default()),
        Span::styled(format!("+{adds}"), theme.style(theme.diff_add)),
        Span::raw(" "),
        Span::styled(format!("-{dels}"), theme.style(theme.diff_del)),
    ];
    let parts: Vec<String> = [(Area::Staged, "staged"), (Area::Unstaged, "unstaged"), (Area::Untracked, "untracked")]
        .iter()
        .filter(|(a, _)| n(*a) > 0)
        .map(|(a, name)| format!("{name} {}", n(*a)))
        .collect();
    if !parts.is_empty() {
        spans.push(Span::styled(format!("  |  {}", parts.join(" | ")), theme.dim()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_list(f: &mut Frame, dv: &mut DiffView, area: Rect, theme: &Theme) {
    let h = area.height as usize;
    if dv.sel < dv.list_top {
        dv.list_top = dv.sel;
    } else if dv.sel >= dv.list_top + h {
        dv.list_top = dv.sel + 1 - h;
    }
    dv.list_top = dv.list_top.min(dv.files.len().saturating_sub(h));
    let w = area.width as usize;
    let max_total = dv.files.iter().map(|x| x.added + x.removed).max().unwrap_or(0).max(1);
    let max_add = dv.files.iter().map(|x| x.added).max().unwrap_or(0);
    let max_del = dv.files.iter().map(|x| x.removed).max().unwrap_or(0);
    let cw = digits(max_add) + digits(max_del) + 3;
    let bar_w = if w >= 34 { 6 } else { 0 };
    let right_w = if w >= 24 { bar_w + usize::from(bar_w > 0) + cw } else { 0 };
    let mut lines = Vec::new();
    for (i, file) in dv.files.iter().enumerate().skip(dv.list_top).take(h) {
        let left_w = w.saturating_sub(right_w + 6);
        let mut spans = vec![Span::raw(" "), Span::styled(tag(file), status_style(file.status, theme)), Span::raw(" ")];
        let path = tail_trunc(&file.path, left_w);
        let pad = left_w.saturating_sub(width(&path));
        spans.push(Span::raw(format!("{path}{}", " ".repeat(pad))));
        if right_w > 0 {
            spans.push(Span::raw(" "));
            if bar_w > 0 {
                let total = file.added + file.removed;
                let filled = if total == 0 { 0 } else { (total * bar_w).div_ceil(max_total).clamp(1, bar_w) };
                let add_cells = if total == 0 { 0 } else { (filled * file.added).div_ceil(total).min(filled) };
                let add_cells =
                    if file.removed > 0 && add_cells == filled && filled > 1 { filled - 1 } else { add_cells };
                let (a, d) = if theme.mono { ('+', '-') } else { ('█', '█') };
                spans.push(Span::styled(a.to_string().repeat(add_cells), theme.style(theme.diff_add)));
                spans.push(Span::styled(d.to_string().repeat(filled - add_cells), theme.style(theme.diff_del)));
                spans.push(Span::styled("░".repeat(bar_w - filled), theme.dim()));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(format!("+{:<w$}", file.added, w = digits(max_add)), theme.style(theme.diff_add)));
            spans.push(Span::styled(
                format!(" -{:<w$}", file.removed, w = digits(max_del)),
                theme.style(theme.diff_del),
            ));
        }
        let mut line = Line::from(spans);
        if i == dv.sel {
            line = line.style(if dv.focus == Focus::List { theme.highlight() } else { theme.selection() });
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_body(f: &mut Frame, dv: &mut DiffView, area: Rect, theme: &Theme) {
    ensure_highlight(dv, theme);
    let [title, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    dv.body_area = body;
    let (rows, starts) = dv.rows();
    let h = body.height as usize;
    dv.scroll = dv.scroll.min(rows.len().saturating_sub(h));
    let Some(file) = dv.files.get(dv.sel) else { return };
    // title: path, rename source, position
    let cur = dv.hunk.min(starts.len().saturating_sub(1)) + 1;
    let pos = if starts.is_empty() { String::new() } else { format!("hunk {cur}/{} ", starts.len()) };
    let mut name = file.path.clone();
    if let Some(o) = &file.old_path {
        name = format!("{name} (was {o})");
    }
    let tw = (area.width as usize).saturating_sub(pos.len() + 2);
    let mut tspans =
        vec![Span::raw(" "), Span::styled(tail_trunc(&name, tw), Style::default().add_modifier(Modifier::BOLD))];
    let used = 1 + width(&tail_trunc(&name, tw));
    tspans.push(Span::raw(" ".repeat((area.width as usize).saturating_sub(used + pos.len()))));
    tspans.push(Span::styled(pos, theme.dim()));
    let mut tl = Line::from(tspans);
    if dv.focus == Focus::Body {
        tl = tl.style(theme.selection());
    }
    f.render_widget(Paragraph::new(tl), title);

    let w = body.width as usize;
    let d =
        digits(file.hunks.iter().flat_map(|x| &x.lines).filter_map(|l| l.old.max(l.new)).max().map_or(1, |n| n + 1));
    let hl = dv.hl.get(&dv.sel);
    let spans_of = |hi: usize, li: usize| -> Vec<Span<'static>> {
        hl.and_then(|x| x.get(hi))
            .and_then(|x| x.get(li))
            .cloned()
            .unwrap_or_else(|| vec![Span::raw(file.hunks[hi].lines[li].text.clone())])
    };
    let kind_style = |k: Kind| match k {
        Kind::Add => theme.diff_add_line(),
        Kind::Del => theme.diff_del_line(),
        Kind::Context => Style::default(),
    };
    let sign = |k: Kind| match k {
        Kind::Add => '+',
        Kind::Del => '-',
        Kind::Context => ' ',
    };
    let skip = dv.hscroll;
    let mut out: Vec<Line<'static>> = Vec::new();
    for row in rows.iter().skip(dv.scroll).take(h) {
        match *row {
            Row::Note => {
                let t = file.note.clone().unwrap_or_else(|| "no textual changes".to_string());
                out.push(Line::from(Span::styled(format!("  {t}"), theme.dim())));
            }
            Row::Hunk(hi) => {
                let hunk = &file.hunks[hi];
                let closed = dv.folded.contains(&(dv.sel, hi));
                let text = if closed {
                    format!("▸ {}  ({} lines)", hunk.header, hunk.lines.len())
                } else {
                    format!("▾ {}", hunk.header)
                };
                out.push(Line::from(slice(&[Span::raw(text)], 0, w, theme.style(theme.hunk))));
            }
            Row::Line(hi, li) => {
                let l = &file.hunks[hi].lines[li];
                let num = |n: Option<usize>| n.map_or(" ".repeat(d), |n| format!("{:>d$}", n + 1));
                let mut spans = vec![Span::styled(format!("{} {} ", num(l.old), num(l.new)), theme.dim())];
                let gutter = 2 * d + 2;
                let base = kind_style(l.kind);
                spans.push(Span::styled(format!("{} ", sign(l.kind)), base));
                spans.extend(slice(&spans_of(hi, li), skip, w.saturating_sub(gutter + 2), base));
                out.push(Line::from(spans));
            }
            Row::Pair(hi, lo, ln) => {
                let col = w.saturating_sub(3) / 2;
                let cell = |li: Option<usize>, old: bool| -> Vec<Span<'static>> {
                    let Some(li) = li else { return vec![Span::raw(" ".repeat(col))] };
                    let l = &file.hunks[hi].lines[li];
                    let n = if old { l.old } else { l.new };
                    let base = kind_style(l.kind);
                    let mut s = vec![
                        Span::styled(format!("{:>d$} ", n.map_or(0, |n| n + 1)), theme.dim()),
                        Span::styled(format!("{} ", sign(l.kind)), base),
                    ];
                    s.extend(slice(&spans_of(hi, li), skip, col.saturating_sub(d + 3), base));
                    s
                };
                let mut spans = cell(lo, true);
                spans.push(Span::styled(" │ ", theme.dim()));
                spans.extend(cell(ln, false));
                out.push(Line::from(spans));
            }
        }
    }
    f.render_widget(Paragraph::new(out), body);
}

fn draw_sep(f: &mut Frame, area: Rect, theme: &Theme) {
    let lines: Vec<Line<'static>> = (0..area.height).map(|_| Line::from(Span::styled("│", theme.dim()))).collect();
    f.render_widget(Paragraph::new(lines), area);
}
