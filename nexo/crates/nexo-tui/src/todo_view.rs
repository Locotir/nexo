//! `/todo`, a click on `todo 3/7`: the list in its own window. A bar of how far it is, then the
//! task being worked on, the ones to come and the finished ones folded away. Enter copies a task,
//! Space opens or folds the finished ones.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::tool::{TodoItem, TodoStatus};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::motion::{hump, Counter};
use crate::render::todo::{glyph, todo_row, RowCtx};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// How long a task that just changed state glows, in milliseconds.
const GLOW_MS: u64 = 700;

/// The longest gap between two clicks on a row that count as a double click, in milliseconds.
const DOUBLE_CLICK_MS: u128 = 400;

#[derive(Debug, Clone)]
pub struct TodoView {
    items: Vec<TodoItem>,
    /// The row under the cursor, in the order shown.
    pub selected: usize,
    pub scroll: usize,
    pub done_open: bool,
    changed_at: Vec<Option<Instant>>,
    bar: Counter,
    moving: bool,
    /// The line each row started at in the last body, for a dragged scrollbar.
    row_starts: Vec<usize>,
    /// The row and the moment of the last click, for the second click that copies.
    last_click: Option<(usize, Instant)>,
}

/// What `lines` hands the window.
pub struct Body {
    pub lines: Vec<Line<'static>>,
    /// The line each row of the order starts at, and how many lines it takes.
    pub starts: Vec<(usize, usize)>,
    /// The line of the header of the finished ones, when there is one.
    pub done_head: Option<usize>,
    /// How many lines the selected row takes.
    pub sel_len: usize,
}

impl TodoView {
    pub fn new(items: Vec<TodoItem>) -> TodoView {
        let n = items.len();
        let mut v = TodoView {
            items,
            selected: 0,
            scroll: 0,
            done_open: false,
            changed_at: vec![None; n],
            bar: Counter::default(),
            moving: false,
            row_starts: Vec::new(),
            last_click: None,
        };
        v.done_open = v.all_done();
        v
    }

    fn all_done(&self) -> bool {
        self.items.iter().all(|i| i.status == TodoStatus::Done)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn done_count(&self) -> usize {
        self.items.iter().filter(|i| i.status == TodoStatus::Done).count()
    }

    /// The tasks in the order shown (their places in the list): in progress, pending, then the
    /// finished ones when they are open.
    pub fn order(&self) -> Vec<usize> {
        let by = |s: TodoStatus| (0..self.items.len()).filter(move |&i| self.items[i].status == s);
        let mut v: Vec<usize> = by(TodoStatus::InProgress).chain(by(TodoStatus::Pending)).collect();
        if self.done_open {
            v.extend(by(TodoStatus::Done));
        }
        v
    }

    /// What the size of the window depends on: how many tasks, how many in each group, whether the
    /// finished ones are open.
    pub fn size_key(&self) -> (usize, usize, usize, bool) {
        let n = |s: TodoStatus| self.items.iter().filter(|i| i.status == s).count();
        (self.items.len(), n(TodoStatus::InProgress), n(TodoStatus::Done), self.done_open)
    }

    pub fn current(&self) -> Option<&TodoItem> {
        self.order().get(self.selected).map(|&i| &self.items[i])
    }

    /// The agent changed the list. The cursor stays on the same task when it can.
    pub fn set_items(&mut self, items: Vec<TodoItem>, now: Instant) {
        let text = self.current().map(|i| i.text.clone());
        let was_all = self.all_done();
        let old = std::mem::take(&mut self.items);
        self.changed_at = items
            .iter()
            .map(|n| match old.iter().find(|o| o.text == n.text) {
                Some(o) if o.status == n.status => None,
                _ => Some(now),
            })
            .collect();
        self.items = items;
        if !was_all && self.all_done() {
            self.done_open = true;
        }
        let order = self.order();
        self.selected = text
            .and_then(|t| order.iter().position(|&i| self.items[i].text == t))
            .unwrap_or(self.selected)
            .min(order.len().saturating_sub(1));
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        let last = self.order().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(5),
            KeyCode::PageDown => self.selected = (self.selected + 5).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Char(' ') => self.toggle_done(),
            KeyCode::Enter | KeyCode::Char('y') => return self.copy(),
            _ => {}
        }
        ModalOut::Nothing
    }

    fn toggle_done(&mut self) {
        let text = self.current().map(|i| i.text.clone());
        self.done_open = !self.done_open;
        let order = self.order();
        self.selected = text
            .and_then(|t| order.iter().position(|&i| self.items[i].text == t))
            .unwrap_or(0)
            .min(order.len().saturating_sub(1));
    }

    fn copy(&self) -> ModalOut {
        match self.current() {
            Some(i) => ModalOut::Copy(i.text.clone()),
            None => ModalOut::Nothing,
        }
    }

    /// A click on the row `i` of the order: it takes the cursor, a second click copies it. The
    /// place just after the last row is the header of the finished ones.
    pub fn click_row(&mut self, i: usize) -> ModalOut {
        self.click_row_at(i, Instant::now())
    }

    /// `click_row` with the moment of the click: a second click on the same row within
    /// [`DOUBLE_CLICK_MS`] copies; a first one only moves the cursor.
    pub fn click_row_at(&mut self, i: usize, now: Instant) -> ModalOut {
        let n = self.order().len();
        let twice = self
            .last_click
            .is_some_and(|(r, t)| r == i && now.saturating_duration_since(t).as_millis() <= DOUBLE_CLICK_MS);
        self.last_click = Some((i, now));
        if i >= n {
            self.last_click = None;
            self.toggle_done();
        } else if twice {
            self.last_click = None;
            self.selected = i;
            return self.copy();
        } else {
            self.selected = i;
        }
        ModalOut::Nothing
    }

    /// A dragged scrollbar wants line `off` first in sight: the cursor goes to the first task from there.
    pub fn scroll_to_line(&mut self, off: usize) {
        let n = self.order().len();
        if n == 0 {
            return;
        }
        let row = self.row_starts.iter().position(|&s| s >= off).unwrap_or(n - 1);
        self.selected = row.min(n - 1);
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let n = self.order().len();
        if n > 0 {
            self.selected = (self.selected as i32 + delta).clamp(0, n as i32 - 1) as usize;
        }
    }

    /// True while something moves: the bar filling, a task that just changed.
    pub fn animating(&self, now: Instant) -> bool {
        self.moving || self.changed_at.iter().any(|c| hump(*c, now, GLOW_MS) > 0.0)
    }

    /// Whether the task at place `i` of the list is the one being worked on.
    pub fn item_running(&self, i: usize) -> bool {
        self.items.get(i).is_some_and(|t| t.status == TodoStatus::InProgress)
    }

    pub fn title(&self) -> String {
        " ≡ To-do ".into()
    }

    /// The keys, in the row pinned under the list; the longest that fits.
    pub fn keys(&self, w: usize, theme: &Theme) -> Line<'static> {
        let full: &[(&str, &str)] = &[("↑↓", "move"), ("Enter", "copy"), ("Space", "fold done"), ("Esc", "close")];
        let short: &[(&str, &str)] = &[("↑↓", ""), ("Enter", "copy"), ("Space", "done"), ("Esc", "")];
        for set in [full, short] {
            let mut spans = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled((*k).to_string(), theme.style(theme.accent)));
                if !what.is_empty() {
                    spans.push(Span::styled(format!(" {what}"), theme.dim()));
                }
            }
            if spans.iter().map(|s| width(&s.content)).sum::<usize>() <= w {
                return Line::from(spans);
            }
        }
        Line::from(Span::styled(truncate("Esc closes", w), theme.dim()))
    }

    /// The body at `w` columns. `spinner` is the frame of the spinner (None: still), `pulse` 0..1.
    pub fn body(&mut self, w: usize, theme: &Theme, spinner: Option<usize>, pulse: f32, now: Instant) -> Body {
        let animate = spinner.is_some();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut starts = Vec::new();
        let mut done_head = None;
        if self.items.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("≡", theme.dim2())).centered());
            lines.push(Line::from(Span::styled("No tasks yet", theme.dim())).centered());
            lines.push(Line::from(Span::styled("The agent adds them as it plans.", theme.dim2())).centered());
            return Body { lines, starts, done_head, sel_len: 1 };
        }
        let (done, total) = (self.done_count(), self.items.len());
        let target = done as f64 / total as f64;
        let (shown, moving) = self.bar.sample(target, now, animate);
        self.moving = moving;
        // the bar, then the share and the counts at the right when they fit
        let mut head = crate::render::todo::todo_summary(theme, shown, done, total, false);
        let running = self.items.iter().filter(|i| i.status == TodoStatus::InProgress).count();
        let waiting = self.items.iter().filter(|i| i.status == TodoStatus::Pending).count();
        let pct = format!("{}%", (target * 100.0).round() as u32);
        let counts: Vec<Span<'static>> = {
            let mut c = Vec::new();
            for (n, status) in
                [(running, TodoStatus::InProgress), (waiting, TodoStatus::Pending), (done, TodoStatus::Done)]
            {
                if !c.is_empty() {
                    c.push(Span::styled(" · ", theme.dim2()));
                }
                let (icon, style) = glyph(&TodoItem { text: String::new(), status }, theme, None);
                c.push(Span::styled(
                    format!("{icon} {n}"),
                    if status == TodoStatus::Done { theme.dim() } else { style },
                ));
            }
            c
        };
        let left = head.iter().map(|s| width(&s.content)).sum::<usize>();
        let cw: usize = counts.iter().map(|s| width(&s.content)).sum();
        if w >= left + pct.len() + cw + 6 {
            head.push(Span::raw(" ".repeat(w - left - pct.len() - cw - 2)));
            head.push(Span::styled(pct, theme.dim()));
            head.push(Span::raw("  "));
            head.extend(counts);
        } else if w >= left + pct.len() + 2 {
            head.push(Span::raw(" ".repeat(w - left - pct.len())));
            head.push(Span::styled(pct, theme.dim()));
        }
        lines.push(Line::from(head));
        lines.push(Line::default());

        let digits = total.to_string().len();
        let order = self.order();
        let section = |lines: &mut Vec<Line<'static>>, text: String| {
            if lines.len() > 2 {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(text, theme.todo_head())));
        };
        let mut last: Option<TodoStatus> = None;
        let mut sel_len = 1;
        for (pos, &i) in order.iter().enumerate() {
            let it = &self.items[i];
            if last != Some(it.status) {
                let n = self.items.iter().filter(|x| x.status == it.status).count();
                let at = lines.len() + usize::from(lines.len() > 2);
                let text = match it.status {
                    TodoStatus::InProgress => "NOW".to_string(),
                    TodoStatus::Pending => format!("NEXT · {n}"),
                    TodoStatus::Done => {
                        done_head = Some(at);
                        format!("▾ DONE · {n}")
                    }
                };
                section(&mut lines, text);
                last = Some(it.status);
            }
            let sel = pos == self.selected;
            let ctx = RowCtx {
                spinner,
                number: Some((i + 1, digits)),
                glow: pulse.max(hump(self.changed_at.get(i).copied().flatten(), now, GLOW_MS)),
                compact: false,
                roomy: false,
            };
            // two columns at the left for the cursor: a bar on its rows, blank on the others
            let mut rows = todo_row(it, w.saturating_sub(2), theme, ctx);
            for l in &mut rows {
                let ground = l.spans.first().and_then(|s| s.style.bg);
                let mark = if sel { "▌ " } else { "  " };
                let mut style = ground.map(|c| Style::default().bg(c)).unwrap_or_default();
                if sel {
                    style = style.fg(theme.accent);
                }
                l.spans.insert(0, Span::styled(mark, style));
            }
            if sel {
                sel_len = rows.len();
                for l in &mut rows {
                    let used: usize = l.spans.iter().map(|s| width(&s.content)).sum();
                    if used < w {
                        l.spans.push(Span::raw(" ".repeat(w - used)));
                    }
                    for s in &mut l.spans {
                        s.style = s.style.patch(theme.selection());
                    }
                }
            }
            starts.push((lines.len(), rows.len()));
            lines.extend(rows);
        }
        if !self.done_open && done > 0 {
            section(&mut lines, String::new());
            let at = lines.len() - 1;
            done_head = Some(at);
            lines[at] = Line::from(vec![
                Span::styled(format!("▸ DONE · {done}"), theme.todo_head()),
                Span::styled("  Space opens", theme.dim2()),
            ]);
        }
        self.row_starts = starts.iter().map(|s| s.0).collect();
        Body { lines, starts, done_head, sel_len }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn it(text: &str, status: TodoStatus) -> TodoItem {
        TodoItem { text: text.into(), status }
    }

    fn view() -> TodoView {
        TodoView::new(vec![
            it("read", TodoStatus::Done),
            it("fix the parser", TodoStatus::InProgress),
            it("test", TodoStatus::Pending),
            it("ship", TodoStatus::Pending),
        ])
    }

    fn plain(b: &Body) -> Vec<String> {
        b.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    fn press(v: &mut TodoView, c: KeyCode) -> ModalOut {
        v.key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    #[test]
    fn groups_now_next_done_and_keeps_original_numbers() {
        let mut v = view();
        let rows = plain(&v.body(60, &Theme::dark(), None, 0.0, Instant::now()));
        let at = |s: &str| rows.iter().position(|r| r.contains(s)).unwrap_or_else(|| panic!("{s}\n{rows:#?}"));
        assert!(at("NOW") < at("NEXT · 2") && at("NEXT · 2") < at("DONE · 1"));
        assert!(rows[at("fix the parser")].contains("2  fix"), "{:?}", rows[at("fix the parser")]);
        assert!(rows[at("test")].contains("3  test"));
        assert!(!rows.iter().any(|r| r.contains("read")), "the finished ones are folded");
    }

    #[test]
    fn long_text_wraps_with_a_hanging_indent_inside_the_width() {
        let mut v = TodoView::new(vec![it(
            "wire the new window into the app and keep the scroll between updates of the list",
            TodoStatus::Pending,
        )]);
        let rows = plain(&v.body(40, &Theme::dark(), None, 0.0, Instant::now()));
        assert!(rows.iter().all(|r| width(r) <= 40), "{rows:#?}");
        let n = rows.iter().position(|r| r.trim_start_matches(['▌', ' ']).starts_with("○ 1")).unwrap();
        assert!(rows[n + 1].trim_start_matches('▌').starts_with("      "), "{rows:#?}");
    }

    #[test]
    fn done_is_folded_until_space_unless_everything_is_done() {
        let mut v = view();
        assert!(!v.done_open);
        press(&mut v, KeyCode::Char(' '));
        assert!(v.done_open);
        press(&mut v, KeyCode::Char(' '));
        assert!(!v.done_open);
        assert!(TodoView::new(vec![it("a", TodoStatus::Done)]).done_open);
    }

    #[test]
    fn enter_copies_the_selected_text_and_esc_closes() {
        let mut v = view();
        assert_eq!(press(&mut v, KeyCode::Enter), ModalOut::Copy("fix the parser".into()));
        press(&mut v, KeyCode::Down);
        assert_eq!(press(&mut v, KeyCode::Enter), ModalOut::Copy("test".into()));
        assert_eq!(press(&mut v, KeyCode::Esc), ModalOut::Close);
    }

    #[test]
    fn the_cursor_survives_a_refresh_by_text() {
        let mut v = view();
        press(&mut v, KeyCode::Down);
        press(&mut v, KeyCode::Down);
        assert_eq!(v.current().unwrap().text, "ship");
        v.set_items(
            vec![it("new", TodoStatus::Pending), it("ship", TodoStatus::Pending), it("test", TodoStatus::Done)],
            Instant::now(),
        );
        assert_eq!(v.current().unwrap().text, "ship");
    }

    #[test]
    fn a_dragged_scrollbar_moves_the_cursor_to_the_task_at_that_line() {
        let mut v = view();
        v.body(60, &Theme::dark(), None, 0.0, Instant::now());
        v.scroll_to_line(0);
        assert_eq!(v.selected, 0);
        v.scroll_to_line(1000);
        assert_eq!(v.current().unwrap().text, "ship");
    }

    #[test]
    fn an_empty_list_says_no_tasks_yet() {
        let mut v = TodoView::new(vec![]);
        let rows = plain(&v.body(40, &Theme::dark(), None, 0.0, Instant::now()));
        assert!(rows.iter().any(|r| r.contains("No tasks yet")));
        assert_eq!(press(&mut v, KeyCode::Enter), ModalOut::Nothing);
    }

    #[test]
    fn the_first_click_on_the_row_with_the_cursor_only_selects() {
        let mut v = view();
        let t = Instant::now();
        assert_eq!(v.selected, 0);
        assert_eq!(v.click_row_at(0, t), ModalOut::Nothing);
        assert_eq!(v.click_row_at(0, t + std::time::Duration::from_millis(900)), ModalOut::Nothing, "too slow");
        assert_eq!(
            v.click_row_at(0, t + std::time::Duration::from_millis(1100)),
            ModalOut::Copy("fix the parser".into())
        );
        assert_eq!(v.click_row_at(2, t + std::time::Duration::from_millis(1200)), ModalOut::Nothing);
        assert_eq!(v.click_row_at(1, t + std::time::Duration::from_millis(1300)), ModalOut::Nothing, "another row");
    }

    #[test]
    fn a_click_selects_then_copies_and_the_fold_header_toggles() {
        let mut v = view();
        assert_eq!(v.click_row(1), ModalOut::Nothing);
        assert_eq!(v.click_row(1), ModalOut::Copy("test".into()));
        let n = v.order().len();
        v.click_row(n);
        assert!(v.done_open);
    }
}
