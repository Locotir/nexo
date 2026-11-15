//! The body of `/panel`: the groups under their headers, and each strip of cards, lines or blocks in them.

use std::time::Instant;

use nexo_agent::supervise::{AgentRow, SourceKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::layout::split_evenly;
use super::{AgentsView, Band, Filter, Group, Layout, BLOCK_W, CARD_MAX_W, EMPTY_STATE, GAP};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

impl AgentsView {
    /// The line under a header with no sessions.
    fn nothing_line(&self, group: &Group) -> &'static str {
        match (group.source, self.filter) {
            (SourceKind::Nexo, _) => "no nexo sessions yet · this one shows up with your first message",
            (SourceKind::External, Filter::External) => "no sessions from other programs found",
            (SourceKind::External, _) => "no sessions from other programs",
        }
    }

    /// The body: each group under its header (or its empty line), the cards flowing in rows, the strips
    /// scrolled so the selected one is in sight.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_bands(
        &mut self,
        f: &mut Frame,
        body: Rect,
        layout: Layout,
        card_h: u16,
        shown: &[AgentRow],
        groups: &[Group],
        theme: &Theme,
        now: Instant,
        fx: bool,
    ) {
        let (cols, item_h, tight) = Self::shape(layout, card_h);
        let card_w = match layout {
            Layout::Cards { card_w, .. } => card_w,
            Layout::Blocks { .. } => BLOCK_W,
            Layout::List => body.width,
        };
        self.cols = cols;
        self.drawn = layout;
        if self.rows.is_empty() && self.filter == Filter::All {
            self.grid.clear();
            self.scroll = 0;
            Self::empty_state(f, body, theme);
            return;
        }
        let mut bands = Self::bands(groups, cols, matches!(layout, Layout::Cards { .. }));
        // one body row: a session beats the header of its group
        if body.height < 2 {
            bands.retain(|b| matches!(b, Band::Row(_)));
        }
        self.grid = bands.iter().filter_map(|b| if let Band::Row(r) = b { Some(r.clone()) } else { None }).collect();
        // the strip that holds the selection, and the first strip that keeps it in sight
        let sel = bands.iter().position(|b| matches!(b, Band::Row(r) if r.contains(&self.selected))).unwrap_or(0);
        // a group's first row brings its header along
        let sel_top = if sel > 0 && matches!(bands[sel - 1], Band::Header(_)) { sel - 1 } else { sel };
        let fits = |from: usize, to: usize| Self::span(&bands, from, to + 1, item_h, tight) <= body.height;
        // after the wheel the view stays where the user put it, even if the selection is out of sight
        if self.follow {
            if sel_top < self.scroll {
                self.scroll = sel_top;
            }
            while self.scroll < sel && !fits(self.scroll, sel) {
                self.scroll += 1;
            }
        }
        // never scroll past the point where the last strip is at the foot
        let mut last_first = bands.len().saturating_sub(1);
        while last_first > 0 && fits(last_first - 1, bands.len() - 1) {
            last_first -= 1;
        }
        self.scroll = self.scroll.min(last_first);

        // cards share the whole width up to their widest; blocks keep their own size
        let used_w = match layout {
            _ if tight => body.width,
            Layout::Cards { .. } => body.width.min(cols as u16 * CARD_MAX_W + GAP * (cols as u16 - 1)),
            _ => card_w * cols as u16 + GAP * (cols as u16 - 1),
        };
        let x0 = body.x + body.width.saturating_sub(used_w) / 2;
        let mut y = body.y;
        for (n, band) in bands.iter().enumerate().skip(self.scroll) {
            if n > self.scroll {
                y += Self::band_gap(&bands[n - 1], tight);
            }
            let h = Self::band_h(band, item_h);
            if y + h > body.y + body.height {
                break;
            }
            // a header never stands alone at the foot: its first strip comes with it or neither shows
            if let (Band::Header(_), Some(next)) = (band, bands.get(n + 1)) {
                if y + h + Self::band_h(next, item_h) > body.y + body.height {
                    break;
                }
            }
            match band {
                Band::Header(g) => self.group_header(f, Rect::new(x0, y, used_w, 1), &groups[*g], theme),
                Band::Empty(g) => {
                    let text = truncate(self.nothing_line(&groups[*g]), used_w.saturating_sub(2) as usize);
                    f.render_widget(
                        Paragraph::new(Span::styled(text, theme.dim2())),
                        Rect::new(x0 + 2, y, used_w - 2, 1),
                    );
                }
                Band::Row(items) => {
                    let widths = Self::row_widths(layout, used_w, card_w, items.len());
                    let mut x = x0;
                    for (c, &i) in items.iter().enumerate() {
                        let row = &shown[i];
                        let r = if tight {
                            Rect::new(body.x, y, body.width, 1)
                        } else {
                            Rect::new(x, y, widths[c], item_h)
                        };
                        x += widths[c] + GAP;
                        let t = self.entered(&row.key, now);
                        if tight {
                            self.list_line(f, r, i, row, theme, now, fx);
                            Self::veil(f, r, theme, t);
                        } else {
                            // a card rises one row as it fades in, when the row below it is free
                            let drawn =
                                if t < 0.4 && y + item_h < body.y + body.height { Rect { y: y + 1, ..r } } else { r };
                            if t > 0.0 {
                                if matches!(layout, Layout::Blocks { .. }) {
                                    self.block(f, drawn, i, row, theme, now, fx);
                                } else {
                                    self.card(f, drawn, i, row, theme, now, fx);
                                }
                                Self::veil(f, drawn, theme, t);
                            }
                        }
                        self.cards.push((i, r));
                    }
                }
            }
            y += h;
        }
    }

    /// No session anywhere: what the panel is for and how sessions get here, instead of two bare headers.
    fn empty_state(f: &mut Frame, body: Rect, theme: &Theme) {
        for (n, text) in EMPTY_STATE.iter().enumerate().take(body.height as usize) {
            let room = body.width.saturating_sub(1) as usize;
            let mut chars = text.chars();
            let line = match (n, chars.next()) {
                (0, _) => Line::from(Span::styled(truncate(text, room), theme.bold(theme.fg))),
                (_, Some(mark @ ('▣' | '▢'))) => {
                    let color = if mark == '▣' { theme.accent } else { theme.accent2 };
                    let rest = chars.as_str();
                    Line::from(vec![
                        Span::styled(mark.to_string(), theme.bold(color)),
                        Span::styled(truncate(rest, room.saturating_sub(1)), theme.dim()),
                    ])
                }
                _ => Line::from(Span::styled(truncate(text, room), theme.dim())),
            };
            f.render_widget(
                Paragraph::new(line),
                Rect::new(body.x + 1, body.y + n as u16, body.width.saturating_sub(1), 1),
            );
        }
    }

    /// The widths of the `n` cards of one strip: cards stretch over the strip so no strip ends ragged, blocks keep
    /// their size.
    pub(super) fn row_widths(layout: Layout, full_w: u16, card_w: u16, n: usize) -> Vec<u16> {
        match layout {
            Layout::Cards { .. } => {
                let gaps = GAP * (n as u16).saturating_sub(1);
                let room = full_w.min(n as u16 * CARD_MAX_W + gaps);
                split_evenly(room.saturating_sub(gaps), n)
            }
            _ => vec![card_w; n],
        }
    }

    /// `◆ nexo · 2 ─────`: the program's name, how many sessions it has, a rule to the edge.
    fn group_header(&self, f: &mut Frame, r: Rect, group: &Group, theme: &Theme) {
        let mark = if group.source == SourceKind::Nexo { "▣" } else { "▢" };
        let color = match group.source {
            SourceKind::Nexo => theme.accent,
            SourceKind::External => theme.accent2,
        };
        let name = truncate(&format!("{mark} {}", group.label), (r.width as usize).saturating_sub(12).max(4));
        let count = if group.idx.is_empty() { String::new() } else { format!(" · {}", group.idx.len()) };
        let used = width(&name) + width(&count) + 1;
        let rule = "─".repeat((r.width as usize).saturating_sub(used));
        let line = Line::from(vec![
            Span::styled(name, theme.bold(color)),
            Span::styled(count, theme.dim()),
            Span::raw(" "),
            Span::styled(rule, theme.dim2()),
        ]);
        f.render_widget(Paragraph::new(line), r);
    }
}
