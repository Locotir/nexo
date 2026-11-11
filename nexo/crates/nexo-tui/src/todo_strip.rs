//! The to-do strip above the draft, and the one line a transcript card says about an update.

use nexo_core::tool::{TodoItem, TodoStatus};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// The list a `todo` call carried in its arguments (a resumed session has no other copy). None when
/// it does not read as a list, as the tool would refuse it.
pub fn items_from_args(args: &Value) -> Option<Vec<TodoItem>> {
    let arr = args.get("items")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for it in arr {
        let text = match it {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("text")?.as_str()?,
            _ => return None,
        };
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        // the strip and the window draw it as it is: no joiners, tabs or escapes
        let text = crate::render::wrap::clean(text);
        let status =
            match it.get("status").and_then(Value::as_str).unwrap_or("pending").trim().to_ascii_lowercase().as_str() {
                "pending" | "todo" | "" => TodoStatus::Pending,
                "in_progress" | "in-progress" | "doing" | "active" => TodoStatus::InProgress,
                "done" | "completed" | "complete" => TodoStatus::Done,
                _ => return None,
            };
        out.push(TodoItem { text: text.into_owned(), status });
    }
    Some(out)
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What an update changed, in the card's head: `✓ item` finished, `▸ item` started, `todo: 7 items` when
/// the list was made or replaced.
pub fn change_line(old: &[TodoItem], new: &[TodoItem]) -> String {
    let same = old.len() == new.len() && old.iter().zip(new).all(|(a, b)| a.text == b.text);
    if old.is_empty() || !same {
        return format!("todo: {}", nexo_core::count(new.len(), "item"));
    }
    let mut marks: Vec<String> = Vec::new();
    for (a, b) in old.iter().zip(new) {
        if a.status != b.status {
            let m = match b.status {
                TodoStatus::Done => "✓",
                TodoStatus::InProgress => "▸",
                TodoStatus::Pending => "○",
            };
            marks.push(format!("{m} {}", one_line(&b.text)));
        }
    }
    match marks.len() {
        0 => format!("todo: {}", nexo_core::count(new.len(), "item")),
        1 | 2 => format!("todo {}", marks.join(" · ")),
        n => format!("todo {} · +{} more", marks[..2].join(" · "), n - 2),
    }
}

pub fn done_count(items: &[TodoItem]) -> usize {
    items.iter().filter(|i| i.status == TodoStatus::Done).count()
}

/// How many rows the strip takes in a window `h` rows high: none under 8 rows, one when the window is
/// short (the count and the current item), else up to a third of the window.
pub fn strip_height(items: &[TodoItem], h: u16) -> u16 {
    if items.is_empty() || h < 8 {
        return 0;
    }
    if h <= crate::ui::SHORT_H {
        return 1;
    }
    ((items.len() + 1).min(usize::from(h / 3))) as u16
}

/// The strip's rows at `w` columns and `rows` rows (see `strip_height`).
pub fn strip_lines(items: &[TodoItem], w: usize, rows: usize, theme: &Theme, shown_done: bool) -> Vec<Line<'static>> {
    strip_lines_fx(items, w, rows, theme, shown_done, None)
}

/// [`strip_lines`] with the motion of the moment: the count lit, a checked item popping and wiping, a new
/// row fading in, a started task taking its colour (`fx` is the lane's state and the time it is drawn at).
pub fn strip_lines_fx(
    items: &[TodoItem],
    w: usize,
    rows: usize,
    theme: &Theme,
    shown_done: bool,
    fx: Option<(&crate::fx_goal::GoalFx, std::time::Instant)>,
) -> Vec<Line<'static>> {
    let total = items.len();
    let done = done_count(items);
    let all = done == total;
    let count = if all && shown_done { format!("to-do {done}/{total} ✓") } else { format!("to-do {done}/{total}") };
    let cur = items.iter().position(|i| i.status == TodoStatus::InProgress);
    let cut = |s: &str, room: usize| truncate(&one_line(s), room);
    let head_style = theme.todo_head();
    let lit = |line: &mut Line<'static>| {
        if let Some((g, now)) = fx {
            let fig = format!("{done}/{total}");
            crate::fx_goal::light(theme, line, &fig, g.count_at, now, crate::motion::LIT_MS);
        }
    };
    if rows <= 1 {
        let mut spans = vec![Span::styled(format!(" {count}"), head_style)];
        if let Some(i) = cur {
            let used = width(&count) + 4;
            spans.push(Span::styled(" ▸ ", theme.todo_running()));
            spans.push(Span::styled(cut(&items[i].text, w.saturating_sub(used)), theme.todo_running()));
        }
        let mut line = Line::from(spans);
        lit(&mut line);
        return vec![line];
    }
    let mut lines = vec![Line::from(Span::styled(format!(" {count}"), head_style))];
    lit(&mut lines[0]);
    let room = rows - 1;
    let (start, slots) = if total <= room {
        (0, total)
    } else {
        let first = cur.or_else(|| items.iter().position(|i| i.status == TodoStatus::Pending)).unwrap_or(0);
        let slots = room - 1;
        // keep the window inside the list so the tail never shows a short window
        (first.min(total.saturating_sub(slots)), slots)
    };
    for (k, it) in items.iter().enumerate().skip(start).take(slots) {
        let (mark, style) = match it.status {
            TodoStatus::Done => ("✓", theme.dim()),
            TodoStatus::InProgress => ("▸", theme.todo_running()),
            TodoStatus::Pending => ("○", theme.todo_pending()),
        };
        let text = cut(&it.text, w.saturating_sub(3));
        let mut row = [Line::from(vec![Span::styled(format!(" {mark} "), style), Span::styled(text, style)])];
        if let Some((g, now)) = fx {
            if let Some(rf) = g.row(k, &it.text) {
                crate::fx_goal::row(theme, &mut row, 0, rf, now);
            }
        }
        lines.extend(row);
    }
    let hidden = total - slots.min(total);
    if hidden > 0 {
        lines.push(Line::from(Span::styled(format!("   +{hidden} more"), theme.dim())));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(t: &str, s: TodoStatus) -> TodoItem {
        TodoItem { text: t.into(), status: s }
    }
    fn txt(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn list(n: usize) -> Vec<TodoItem> {
        (0..n)
            .map(|i| {
                let s = if i < 2 {
                    TodoStatus::Done
                } else if i == 2 {
                    TodoStatus::InProgress
                } else {
                    TodoStatus::Pending
                };
                it(&format!("item {i}"), s)
            })
            .collect()
    }

    #[test]
    fn an_update_says_only_what_changed() {
        let a = vec![it("a", TodoStatus::InProgress), it("b", TodoStatus::Pending)];
        let b = vec![it("a", TodoStatus::Done), it("b", TodoStatus::InProgress)];
        assert_eq!(change_line(&a, &b), "todo ✓ a · ▸ b");
        assert_eq!(change_line(&[], &a), "todo: 2 items");
        assert_eq!(change_line(&a, &[it("z", TodoStatus::Pending)]), "todo: 1 item");
    }

    #[test]
    fn a_long_list_shows_the_current_item_the_next_ones_and_the_rest_as_a_count() {
        let t = Theme::dark();
        let l = strip_lines(&list(7), 40, 5, &t, false);
        assert_eq!(l.len(), 5);
        assert_eq!(txt(&l[0]), " to-do 2/7");
        assert!(txt(&l[1]).contains("▸ item 2"));
        assert_eq!(txt(&l[4]).trim(), "+4 more");
    }

    #[test]
    fn a_short_window_collapses_to_one_line_and_text_is_cut_to_the_width() {
        let t = Theme::dark();
        let l = strip_lines(&list(7), 24, 1, &t, false);
        assert_eq!(l.len(), 1);
        assert!(txt(&l[0]).starts_with(" to-do 2/7 ▸ item"));
        assert!(width(&txt(&l[0])) <= 24);
        let wide = vec![it(&"界".repeat(30), TodoStatus::InProgress)];
        for r in [1, 3] {
            assert!(strip_lines(&wide, 20, r, &t, false).iter().all(|l| width(&txt(l)) <= 20));
        }
        assert_eq!(strip_height(&list(7), 24), 8);
        assert_eq!(strip_height(&list(7), 15), 5);
        assert_eq!(strip_height(&list(7), 10), 1);
        assert_eq!(strip_height(&list(7), 7), 0);
    }

    #[test]
    fn the_list_comes_back_from_the_call_arguments() {
        let v = serde_json::json!({"items": [{"text": "a", "status": "done"}, {"text": "b"}]});
        let got = items_from_args(&v).unwrap();
        assert_eq!(got[0].status, TodoStatus::Done);
        assert_eq!(got[1].status, TodoStatus::Pending);
        assert!(items_from_args(&serde_json::json!({"items": [{"text": ""}]})).is_none());
    }
}
