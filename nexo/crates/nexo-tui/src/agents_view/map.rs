//! A compact relationship map: each lane names its actual parent, if visible.
use super::*;

/// Below this width there is not enough room for two useful nodes and their edge.
pub(super) const MAP_MIN_W: u16 = 64;

impl AgentsView {
    pub(super) fn draw_map(&mut self, f: &mut Frame, body: Rect, rows: &[AgentRow], theme: &Theme) {
        self.drawn = Layout::List;
        self.cols = 1;
        self.grid = (0..rows.len()).map(|i| vec![i]).collect();
        let height = body.height as usize;
        let max = rows.len().saturating_sub(height);
        self.scroll = self.scroll.min(max);
        if self.follow && height > 0 {
            if self.selected < self.scroll {
                self.scroll = self.selected;
            }
            if self.selected >= self.scroll + height {
                self.scroll = self.selected + 1 - height;
            }
            self.scroll = self.scroll.min(max);
        }
        let (parents, issues) = tree::relations(rows);
        let all = self.tree_rows();
        let (all_parents, _) = tree::relations(&all);
        let left = (body.width as usize / 3).clamp(16, 28);
        for (i, row) in rows.iter().enumerate().skip(self.scroll).take(height) {
            let (glyph, state) = chip(row.state);
            let prefix = if let Some(p) = parents[i] {
                // Row numbers disambiguate even names that become identical after truncation.
                let id = format!("#{}", p + 1);
                let name = truncate(&rows[p].name, left.saturating_sub(width(&id) + 3));
                let node = format!("[{name} {id}]");
                format!(
                    "{node}{} {} ",
                    " ".repeat(left.saturating_sub(width(&node))),
                    if self.ascii { "-->" } else { "──▶" }
                )
            } else {
                " ".repeat(left + 4)
            };
            let issue = match issues[i] {
                tree::Issue::None => "",
                tree::Issue::Orphan => " [orphan]",
                tree::Issue::Cycle => " [cycle]",
            };
            let has_children =
                all.iter().position(|r| r.key == row.key).is_some_and(|p| all_parents.contains(&Some(p)));
            let fold = if has_children {
                if self.collapsed.contains(&row.key) {
                    "+"
                } else {
                    "-"
                }
            } else {
                " "
            };
            let head = format!("{prefix}[#{} {fold} {glyph} {state}{issue} ", i + 1);
            let name = truncate(&row.name, (body.width as usize).saturating_sub(width(&head) + 1));
            let text = format!("{head}{name}]");
            let r = Rect::new(body.x, body.y + (i - self.scroll) as u16, body.width, 1);
            let style = if i == self.selected {
                theme.bold(theme.accent).add_modifier(Modifier::REVERSED)
            } else {
                theme.style(chip_color(theme, row.state))
            };
            f.render_widget(Paragraph::new(truncate(&text, body.width as usize)).style(style), r);
            self.cards.push((i, r));
        }
    }
}
