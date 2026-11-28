//! The card page of `/panel`: one card per filtered row, newest step bright, earlier steps dim. `c` opens it.

use std::collections::VecDeque;

use nexo_supervise::{AgentRow, EvDetail, State};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::chips::age;
use super::AgentsView;
use crate::render::wrap::{one_line, truncate, width};
use crate::theme::Theme;

const STEP_KEEP: usize = 4;
const STEP_SHOWN: usize = 3;

impl AgentsView {
    /// Remember a new latest step, and seed a row from its ribbon the first time it is seen.
    pub(super) fn note_steps(&mut self) {
        self.steps.retain(|k, _| self.rows.iter().any(|r| r.key == *k));
        for r in &self.rows {
            let q = self.steps.entry(r.key.clone()).or_insert_with(|| {
                let mut q = VecDeque::new();
                for ev in r.ribbon.iter().rev().take(STEP_SHOWN) {
                    if let EvDetail::Text(t) = &ev.detail {
                        let t = one_line(t);
                        if !t.is_empty() {
                            q.push_front(t);
                        }
                    }
                }
                q
            });
            if let Some(step) = r.last_step.as_deref().map(one_line).filter(|s| !s.is_empty()) {
                if q.back().is_none_or(|last| last != &step) {
                    q.push_back(step);
                    while q.len() > STEP_KEEP {
                        q.pop_front();
                    }
                }
            }
        }
    }

    /// `c` opens the cards and closes them. A page that already covers the body stays as it is.
    pub(super) fn cards_toggle(&mut self) {
        if self.policy_page || self.observing.is_some() {
            return;
        }
        self.cards_page = !self.cards_page;
        self.cards_scroll = 0;
    }

    /// The keys of the card page. Esc and `c` return to the tree; the wheel and arrows move the list.
    pub(super) fn cards_key(&mut self, key: crossterm::event::KeyEvent) -> crate::modal::ModalOut {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('q') => {
                self.cards_page = false;
                self.cards_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.cards_scroll = self.cards_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cards_scroll = self.cards_scroll.saturating_add(1),
            KeyCode::PageUp => self.cards_scroll = self.cards_scroll.saturating_sub(4),
            KeyCode::PageDown => self.cards_scroll = self.cards_scroll.saturating_add(4),
            _ => {}
        }
        crate::modal::ModalOut::Nothing
    }

    pub(super) fn draw_cards(&mut self, f: &mut Frame, body: Rect, theme: &Theme) {
        let cols = if body.width >= 100 { 2 } else { 1 };
        let rows = self.listed();
        let card_h = 7u16;
        let gap = 1u16;
        // the last row of the body is the hint, so a card never covers it
        let list_h = body.height.saturating_sub(1);
        let per_col = ((list_h.saturating_add(gap)) / card_h.saturating_add(gap)).max(1) as usize;
        let page = per_col.saturating_mul(cols);
        let max = rows.len().saturating_sub(page);
        self.cards_scroll = self.cards_scroll.min(max);
        let start = self.cards_scroll;
        let shown: Vec<&AgentRow> = rows.iter().skip(start).take(page).collect();
        let col_w = body.width.saturating_sub(gap.saturating_mul(cols as u16 - 1)) / cols as u16;
        for (i, row) in shown.iter().enumerate() {
            let col = (i % cols) as u16;
            let line = (i / cols) as u16;
            let x = body.x + col * col_w.saturating_add(gap);
            let y = body.y + line * card_h.saturating_add(gap);
            let height = card_h.min(list_h.saturating_sub(line * card_h.saturating_add(gap)));
            let area = Rect { x, y, width: col_w, height };
            if area.height < 3 || area.width < 12 {
                continue;
            }
            self.draw_card(f, area, row, theme);
        }
        let hint = if rows.is_empty() {
            "no sessions"
        } else if max > 0 {
            "j/k move   c or Esc back"
        } else {
            "c or Esc back to the tree"
        };
        let foot = Rect { x: body.x, y: body.bottom().saturating_sub(1), width: body.width, height: 1 };
        f.render_widget(Paragraph::new(Line::from(Span::styled(hint, theme.dim()))), foot);
    }

    fn draw_card(&self, f: &mut Frame, area: Rect, row: &AgentRow, theme: &Theme) {
        let done = row.state == State::Done;
        let title = truncate(&format!(" {} ", row.name), area.width.saturating_sub(4) as usize);
        let border = if done { theme.dim() } else { theme.style(theme.accent) };
        let block = Block::default().borders(Borders::ALL).border_style(border).title(Span::styled(title, border));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height == 0 || inner.width < 4 {
            return;
        }
        let dim = if done { theme.dim2() } else { theme.dim() };
        let bright = if done { theme.dim() } else { theme.style(theme.fg).add_modifier(Modifier::BOLD) };
        let mut lines = vec![Line::from(self.card_meta(row, inner.width as usize, theme, done))];
        let steps = self.steps.get(&row.key);
        let latest = steps.and_then(|q| q.back().map(String::as_str)).or(row.last_step.as_deref());
        let older: Vec<&str> = steps
            .map(|q| {
                let n = q.len();
                q.iter()
                    .skip(n.saturating_sub(STEP_SHOWN + 1))
                    .take(n.saturating_sub(1).min(STEP_SHOWN))
                    .map(String::as_str)
                    .collect()
            })
            .unwrap_or_default();
        for step in older {
            lines.push(Line::from(Span::styled(truncate(step, inner.width as usize), dim)));
        }
        lines.push(Line::from(Span::styled(truncate(latest.unwrap_or("no step yet"), inner.width as usize), bright)));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn card_meta(&self, row: &AgentRow, w: usize, theme: &Theme, done: bool) -> Vec<Span<'static>> {
        let dim = if done { theme.dim2() } else { theme.dim() };
        let state = theme.style(super::chips::chip_color(theme, row.state));
        let model = row.model.as_deref().unwrap_or("-");
        let elapsed = age((self.clock - row.since).max(0) as u64);
        let mut parts = vec![
            Span::styled(row.state.label().to_string(), if done { dim } else { state }),
            Span::styled(format!("  {model}"), dim),
            Span::styled(format!("  {elapsed}"), dim),
        ];
        let used: usize = parts.iter().map(|s| width(&s.content)).sum();
        if used > w {
            parts = vec![Span::styled(
                truncate(&format!("{}  {elapsed}", row.state.label()), w),
                if done { dim } else { state },
            )];
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_supervise::{Ev, EvKind, SourceKind};

    const NOW: i64 = 1_800_000_000;

    fn row(key: &str, step: &str) -> AgentRow {
        AgentRow {
            key: key.into(),
            name: key.into(),
            source: SourceKind::Nexo,
            program: "nexo".into(),
            state: State::Working,
            since: NOW - 90,
            last_step: Some(step.into()),
            last_conclusion: None,
            idle_secs: Some(1),
            cost_usd: None,
            model: Some("grok".into()),
            cwd: None,
            permission_mode: None,
            parent: None,
            last_action: None,
            paused: false,
            ribbon: vec![Ev {
                at: NOW - 10,
                kind: EvKind::Tool,
                n: 1,
                detail: EvDetail::Text("seeded from the ribbon".into()),
            }],
            spark: vec![],
            todo: None,
            tests: None,
            subs: vec![],
            unread: 0,
            held: 0,
            retry_secs: None,
            ctx_pct: None,
            goal: None,
            lane: None,
        }
    }

    #[test]
    fn a_row_keeps_four_steps_and_drops_the_oldest() {
        let mut rows = vec![row("k", "step 0")];
        let mut v = AgentsView::with_rows(rows.clone(), NOW, true);
        let seeded = v.steps.get("k").unwrap();
        assert_eq!(seeded.front().map(String::as_str), Some("seeded from the ribbon"));
        assert_eq!(seeded.back().map(String::as_str), Some("step 0"));
        for i in 1..8 {
            rows[0].last_step = Some(format!("step {i}"));
            v.set_rows(rows.clone());
        }
        let q = v.steps.get("k").unwrap();
        assert_eq!(q.len(), 4, "{q:?}");
        assert_eq!(q.iter().map(String::as_str).collect::<Vec<_>>(), ["step 4", "step 5", "step 6", "step 7"]);
        rows[0].last_step = Some("step 7".into());
        v.set_rows(rows);
        assert_eq!(v.steps.get("k").unwrap().len(), 4, "the same step is not stored twice");
    }
}
