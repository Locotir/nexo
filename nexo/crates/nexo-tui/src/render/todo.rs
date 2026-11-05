//! One row of the todo list, the same in the dialog, the side panel and the permission dialog: a
//! glyph for the state, the number the model sees, then the whole text wrapped under itself.

use nexo_core::tool::{TodoItem, TodoStatus};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::render::blocks::{todo_style, SPINNER};
use crate::render::markdown::md_lines;
use crate::render::wrap::width;
use crate::theme::Theme;

/// How a row is drawn.
#[derive(Debug, Clone, Copy, Default)]
pub struct RowCtx {
    /// The frame of the spinner the task being worked on shows instead of `◐` (None: a still glyph).
    pub spinner: Option<usize>,
    /// The position in the list and the width of the biggest one (None: no number).
    pub number: Option<(usize, usize)>,
    /// 0..1 swell of the ground of the task being worked on.
    pub glow: f32,
    /// One line per task, cut with `…`; else the whole text.
    pub compact: bool,
    /// With `compact`: two lines when the text needs them (the panel has height to spare).
    pub roomy: bool,
}

/// The glyph of a task and its style.
pub fn glyph(item: &TodoItem, theme: &Theme, spinner: Option<usize>) -> (String, Style) {
    let (icon, style) = todo_style(item.status, theme);
    match (item.status, spinner) {
        (TodoStatus::InProgress, Some(n)) => (SPINNER[n % SPINNER.len()].to_string(), style),
        _ => (icon.to_string(), style),
    }
}

/// The lines of one task at `w` columns. The task being worked on gets a tinted ground over the
/// whole width; every other line is as wide as its text.
pub fn todo_row(item: &TodoItem, w: usize, theme: &Theme, ctx: RowCtx) -> Vec<Line<'static>> {
    let (icon, style) = glyph(item, theme, ctx.spinner);
    let lead = match ctx.number {
        Some((n, digits)) => format!("{icon} {n:>digits$}  "),
        None => format!("{icon} "),
    };
    let indent = " ".repeat(width(&lead));
    let blank = [Span::raw(indent.clone())];
    let mut lines = md_lines(&item.text, w.max(width(&lead) + 4), theme, style, false, &blank);
    if lines.is_empty() {
        lines.push(Line::from(Span::raw(indent)));
    }
    if ctx.compact {
        let keep = if ctx.roomy { 2 } else { 1 };
        let more = lines.len() > keep;
        lines.truncate(keep);
        if let Some(last) = lines.last_mut() {
            let used: usize = last.spans.iter().map(|s| width(&s.content)).sum();
            if more || used > w {
                clip(last, w.saturating_sub(1));
                last.spans.push(Span::styled("…", theme.dim()));
            }
        }
    }
    // the first line takes the glyph where the indent is
    if let Some(first) = lines.first_mut() {
        first.spans[0] = Span::styled(lead, style);
        if let (Some((_, _)), TodoStatus::Done) = (ctx.number, item.status) {
            // the number of a finished task is dim like the rest
            first.spans[0].style = theme.dim();
        }
    }
    if item.status == TodoStatus::InProgress && !theme.mono {
        let ground = theme.todo_now_bg(ctx.glow);
        for l in &mut lines {
            let used: usize = l.spans.iter().map(|s| width(&s.content)).sum();
            if used < w {
                l.spans.push(Span::raw(" ".repeat(w - used)));
            }
            for s in &mut l.spans {
                s.style = s.style.patch(ground);
            }
        }
    }
    lines
}

/// Cuts a line to at most `cols` columns, trailing blanks dropped.
fn clip(line: &mut Line<'static>, cols: usize) {
    let mut left = cols;
    let mut out: Vec<Span<'static>> = Vec::new();
    for s in line.spans.drain(..) {
        if left == 0 {
            break;
        }
        if width(&s.content) <= left {
            left -= width(&s.content);
            out.push(s);
            continue;
        }
        let mut cut = String::new();
        for g in s.content.graphemes(true) {
            if width(g) > left {
                break;
            }
            left -= width(g);
            cut.push_str(g);
        }
        out.push(Span::styled(cut, s.style));
        break;
    }
    while out.last().is_some_and(|s| s.content.trim_end().is_empty() && out.len() > 1) {
        out.pop();
    }
    if let Some(s) = out.last_mut() {
        s.content = s.content.trim_end().to_string().into();
    }
    line.spans = out;
}

/// `▕███▌░░▏ 3/7` for the status bar and the side panel header, a fixed width.
pub fn todo_summary(theme: &Theme, shown: f64, done: usize, total: usize, ascii: bool) -> Vec<Span<'static>> {
    crate::panel_fx::todo_spans(theme, shown, done as u16, total as u16, ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, status: TodoStatus) -> TodoItem {
        TodoItem { text: text.into(), status }
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_whole_text_wraps_under_the_text_not_under_the_glyph() {
        let t = Theme::dark();
        let it = item("wire the new dialog into the app and make the click on the hint open it", TodoStatus::Pending);
        let ctx = RowCtx { number: Some((5, 1)), ..Default::default() };
        let lines = todo_row(&it, 30, &t, ctx);
        assert!(lines.len() >= 3);
        assert!(text(&lines[0]).starts_with("○ 5  wire"));
        for l in &lines[1..] {
            assert!(text(l).starts_with("     "), "{:?}", text(l));
        }
        assert!(lines.iter().all(|l| width(&text(l)) <= 30));
    }

    #[test]
    fn the_task_being_worked_on_has_a_ground_across_the_width() {
        let t = Theme::dark();
        let it = item("fix", TodoStatus::InProgress);
        let lines = todo_row(&it, 20, &t, RowCtx::default());
        assert_eq!(width(&text(&lines[0])), 20);
        assert!(lines[0].spans.iter().all(|s| s.style.bg.is_some()));
        let plain = todo_row(&item("fix", TodoStatus::Pending), 20, &t, RowCtx::default());
        assert!(plain[0].spans.iter().all(|s| s.style.bg.is_none()));
    }

    #[test]
    fn a_compact_row_is_one_line_cut_with_an_ellipsis_unless_there_is_room() {
        let t = Theme::dark();
        let it = item(&"word ".repeat(40), TodoStatus::Pending);
        let one = todo_row(&it, 20, &t, RowCtx { compact: true, ..Default::default() });
        assert_eq!(one.len(), 1);
        assert!(text(&one[0]).ends_with('…') && width(&text(&one[0])) <= 20, "{:?}", text(&one[0]));
        let two = todo_row(&it, 20, &t, RowCtx { compact: true, roomy: true, ..Default::default() });
        assert_eq!(two.len(), 2);
        assert!(text(&two[1]).ends_with('…'));
    }

    #[test]
    fn the_spinner_replaces_the_half_moon_only_when_asked() {
        let t = Theme::dark();
        let it = item("x", TodoStatus::InProgress);
        assert!(text(&todo_row(&it, 10, &t, RowCtx::default())[0]).starts_with('◐'));
        let s = RowCtx { spinner: Some(2), ..Default::default() };
        assert!(text(&todo_row(&it, 10, &t, s)[0]).starts_with(SPINNER[2]));
    }
}
