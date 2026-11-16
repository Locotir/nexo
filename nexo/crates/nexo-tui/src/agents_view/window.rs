//! The window of `/panel`: the whole area above the input line. A header with the fleet's figures, then the
//! tree of rows on the left and the pane on the right (from `WIDE_FROM` columns; under it the tree alone, and
//! Enter opens the pane at full width), then a footer with the keys of the selected row.

use ratatui::widgets::{Block as Box_, BorderType, Borders, Clear};

use super::row::clock_of;
use super::tree::Item;
use super::*;

/// The tree's share of a wide window, and its bounds in columns.
const TREE_SHARE: f32 = 0.44;
const TREE_MIN: u16 = 44;
const TREE_MAX: u16 = 76;
/// How far a new row starts to the right of its place before it slides in.
const SLIDE_CELLS: f32 = 8.0;

impl AgentsView {
    /// Whether the last draw had the room for the pane beside the tree.
    pub(super) fn wide(&self) -> bool {
        // before the first draw the view has no width yet: it acts as the wide one, so Enter acts at once
        self.window.width == 0 || self.window.width >= WIDE_FROM
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, theme: &Theme, now: Instant, fx: bool) {
        let fx = fx && !self.still;
        if area.width < 30 || area.height < 6 {
            let msg = if area.width < 30 { "make the window wider" } else { "make the window taller" };
            let msg = truncate(msg, area.width as usize);
            let y = area.y + area.height / 2;
            let line =
                Line::from(Span::styled(msg, theme.style(theme.warn))).alignment(ratatui::layout::Alignment::Center);
            f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
            self.cards.clear();
            self.lines.clear();
            // no window is drawn: what the last one left (its rects) must not take clicks or carry motion
            self.window = Rect::default();
            self.close = Rect::default();
            self.tree_area = Rect::default();
            self.pane_area = Rect::default();
            return;
        }
        let r = area;
        f.render_widget(Clear, r);
        let title = if self.policy_page {
            " panel · policy "
        } else if self.observing.is_some() {
            " panel · observation "
        } else {
            " panel "
        };
        let block = Box_::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.style(theme.border))
            .title(Span::styled(title, theme.bold(theme.accent)))
            .style(Style::default().bg(theme.modal_bg));
        let inner = block.inner(r);
        f.render_widget(block, r);
        self.window = r;
        self.close = Rect::new(r.x + r.width.saturating_sub(6), r.y, 3, 1);
        let close_style =
            if self.hover { theme.bold(theme.error).add_modifier(Modifier::REVERSED) } else { theme.dim() };
        f.render_widget(Paragraph::new(Span::styled(if self.ascii { "[x]" } else { "[✗]" }, close_style)), self.close);
        // the clock the rows are told against, on the top border
        let clock = format!(" {} ", clock_of(self.clock));
        let cw = width(&clock) as u16;
        if r.width > cw + 24 {
            f.render_widget(
                Paragraph::new(Span::styled(clock, theme.dim())),
                Rect::new(self.close.x.saturating_sub(cw + 1), r.y, cw, 1),
            );
        }

        let pad = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), inner.height);
        let wide = r.width >= WIDE_FROM;
        let page = self.policy_page || self.observing.is_some();
        // the split between the tree and the pane: where the divider runs
        let split = (wide && !page).then(|| {
            let tw = ((inner.width as f32 * TREE_SHARE) as u16).clamp(TREE_MIN, TREE_MAX);
            inner.x + tw.min(inner.width.saturating_sub(20))
        });
        // a short window (a split pane) gives the header line and its rule to the rows: who is there matters more
        // than the fleet's figures
        let short = inner.height < 7;
        let top_rule = inner.y + 1;
        let foot_rule = inner.y + inner.height.saturating_sub(2);
        let foot = Rect::new(pad.x, inner.y + inner.height.saturating_sub(1), pad.width, 1);
        let body_y = if short { inner.y } else { top_rule + 1 };
        let body_h = foot_rule.saturating_sub(body_y);
        if !short {
            self.rule(f, r, top_rule, split, '┬', theme);
        } else if let Some(sx) = split {
            f.render_widget(Paragraph::new(Span::styled("┬", theme.style(theme.border))), Rect::new(sx, r.y, 1, 1));
        }
        self.rule(f, r, foot_rule, split, '┴', theme);
        if let Some(sx) = split {
            let st = if self.focus { theme.style(theme.accent) } else { theme.style(theme.border) };
            for y in body_y..foot_rule {
                f.render_widget(Paragraph::new(Span::styled("│", st)), Rect::new(sx, y, 1, 1));
            }
        }
        // the header: the fleet's figures, then the filters and what the supervisor does
        self.note(now, fx);
        let right = self.header_right(pad.width as usize);
        let rw = width(&right);
        let mut head = self.pulse_line(theme, (pad.width as usize).saturating_sub(rw + 2), now, fx);
        let used: usize = head.iter().map(|s| width(&s.content)).sum();
        if rw > 0 {
            head.push(Span::raw(" ".repeat((pad.width as usize).saturating_sub(used + rw))));
            head.push(Span::styled(right, theme.dim2()));
        }
        if !short {
            f.render_widget(Paragraph::new(Line::from(head)), Rect { height: 1, ..pad });
        }

        self.cards.clear();
        self.lines.clear();
        if self.policy_page {
            self.draw_policy(f, Rect::new(pad.x, body_y, pad.width, body_h), theme);
        } else if self.observing.is_some() {
            self.draw_observation(f, Rect::new(pad.x, body_y, pad.width, body_h), theme);
        } else {
            let (tree, pane) = match split {
                Some(sx) => (
                    Rect::new(pad.x, body_y, sx.saturating_sub(pad.x + 1), body_h),
                    Some(Rect::new(sx + 2, body_y, (pad.x + pad.width).saturating_sub(sx + 2), body_h)),
                ),
                None if self.open || self.form.is_some() => {
                    (Rect::default(), Some(Rect::new(pad.x, body_y, pad.width, body_h)))
                }
                None => (Rect::new(pad.x, body_y, pad.width, body_h), None),
            };
            self.tree_area = tree;
            self.pane_area = pane.unwrap_or_default();
            if tree.width > 0 {
                self.draw_tree(f, tree, theme, now, fx);
                let rule = Rect::new(pad.x, foot_rule, pad.width, 1);
                if fx {
                    self.draw_flights(f, rule, tree, theme, now);
                    self.draw_letter(f, rule, tree, theme, now);
                }
            }
            if let Some(p) = pane {
                self.draw_pane(f, p, theme, now, fx);
            }
        }
        let line = self.footer(theme, foot.width as usize);
        f.render_widget(Paragraph::new(line), foot);
    }

    /// A rule across the window at `y` that joins its borders, with `join` where the divider meets it.
    fn rule(&self, f: &mut Frame, r: Rect, y: u16, split: Option<u16>, join: char, theme: &Theme) {
        let w = r.width as usize;
        let mut s: Vec<char> = std::iter::once('├').chain(std::iter::repeat_n('─', w.saturating_sub(2))).collect();
        s.push('┤');
        if let Some(sx) = split {
            if let Some(c) = s.get_mut((sx - r.x) as usize) {
                *c = join;
            }
        }
        let text: String = s.into_iter().collect();
        f.render_widget(Paragraph::new(Span::styled(text, theme.style(theme.border))), Rect::new(r.x, y, r.width, 1));
    }

    /// The tree: one line per group header and row, the selection kept in sight; each row's motion on top.
    fn draw_tree(&mut self, f: &mut Frame, tree: Rect, theme: &Theme, now: Instant, fx: bool) {
        let items = self.items();
        if items.is_empty() {
            // rows hidden by the filters are not "nothing running": say which keys bring them back
            let filtered = ["no row matches the filters:", "/ edits the text, f and F cycle back to all"];
            let say: &[&str] = if self.rows.is_empty() { &EMPTY_STATE } else { &filtered };
            let lines: Vec<Line> =
                say.iter().map(|s| Line::from(Span::styled(truncate(s, tree.width as usize), theme.dim()))).collect();
            f.render_widget(Paragraph::new(Text::from(lines)), tree);
            return;
        }
        let h = tree.height as usize;
        if self.follow {
            if self.selected < self.scroll {
                self.scroll = self.selected;
            } else if self.selected >= self.scroll + h {
                self.scroll = self.selected + 1 - h;
            }
        }
        self.scroll = self.scroll.min(items.len().saturating_sub(h));
        let name_w = Self::name_col(&items);
        let shown = self.shown();
        let w = tree.width as usize;
        // with the motion off the swarm stands still, read from the rows as they are
        let still = (!fx).then(|| self.members());
        for (k, item) in items.iter().enumerate().skip(self.scroll).take(h) {
            let y = tree.y + (k - self.scroll) as u16;
            let at = Rect::new(tree.x, y, tree.width, 1);
            self.lines.push((k, at));
            let (line, gap) = self.tree_line(item, name_w, w, theme, now, fx, k == self.selected);
            let Item::Row { row, group, prefix, .. } = item else {
                f.render_widget(Paragraph::new(line), at);
                // the lanes that are on go round their header
                if matches!(item, Item::Head { group: Group::Lanes, .. }) {
                    self.draw_swarm(f, swarm::LANES, at, gap, theme, now, still.as_deref());
                }
                continue;
            };
            // a new row slides in from the right
            let off = if fx { ((1.0 - self.entered(&row.key, now)) * SLIDE_CELLS).round() as u16 } else { 0 };
            let off = off.min(tree.width.saturating_sub(1));
            let slid = Rect { x: at.x + off, width: at.width - off, ..at };
            f.render_widget(Paragraph::new(line), slid);
            let lead = if prefix.is_empty() { 0 } else { width(prefix) as u16 + 1 };
            let glyph = (at.x + off + lead, y);
            if *group != Group::Needs {
                if let Some(i) = shown.iter().position(|r| r.key == row.key) {
                    self.cards.push((i, Rect::new(glyph.0, y, (at.x + at.width).saturating_sub(glyph.0), 1)));
                }
            }
            // its sub-agents go round it, on the cells its words left blank; only on its own line, not on its copy
            // under NEEDS YOU, whose other width would pull the one orbit two ways
            if *group != Group::Needs {
                self.draw_swarm(f, &row.key, slid, gap, theme, now, still.as_deref());
            }
            if !fx {
                continue;
            }
            if let Some((c, s)) = self.row_tint(&row.key, theme, now) {
                let buf = f.buffer_mut();
                for x in at.x..at.x + at.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        let base = if cell.bg == ratatui::style::Color::Reset { theme.modal_bg } else { cell.bg };
                        cell.set_bg(fade(theme, base, c, 0.45 * s));
                    }
                }
            }
            let line = Rect::new(glyph.0, glyph.1, (at.x + at.width).saturating_sub(glyph.0), 1);
            self.draw_row_fx(f, line, tree, &row.key, theme, now);
        }
    }

    /// What the right side of the header says: the filters in force and what the supervisor does, as much as
    /// fits in a third of the line.
    fn header_right(&self, w: usize) -> String {
        let mut which: Vec<String> = Vec::new();
        if self.filter != Filter::All {
            which.push(self.filter.label().to_string());
        }
        if self.show != Show::All {
            which.push(self.show.label().to_string());
        }
        if let Some(t) = self.find.as_deref().filter(|t| !t.is_empty()) {
            which.push(format!("“{t}”"));
        }
        let hidden = self.rows.len().saturating_sub(self.listed().len());
        if hidden > 0 && !which.is_empty() {
            which.push(format!("+{hidden} hidden"));
        }
        let paused_all = self.panel.as_ref().is_some_and(|p| p.paused_all());
        let sup = match (&self.panel, paused_all) {
            (None, _) => "read-only",
            (Some(_), true) => "supervisor paused",
            (Some(_), false) => "supervising",
        };
        let room = w / 3;
        let full = which.iter().map(String::as_str).chain([sup]).collect::<Vec<_>>().join(" · ");
        if width(&full) <= room {
            return full;
        }
        let which = which.join(" · ");
        if !which.is_empty() && width(&which) <= room {
            return which;
        }
        if width(sup) <= room {
            return sup.to_string();
        }
        String::new()
    }

    /// The keys that act on the selected line, most used first. None act on two things on one row.
    pub fn footer_keys(&self) -> Vec<&'static str> {
        let sup = self.panel.is_some();
        let mut k: Vec<&'static str> = Vec::new();
        let item = self.selected_item();
        if let Some(Item::Head { group, .. }) = &item {
            if *group == Group::Lanes {
                k.extend(["a add lane", "p pause lanes"]);
            }
            k.push("←→ fold");
        }
        if let Some(Item::Row { kind, row, kids, .. }) = &item {
            let kind = *kind;
            if !self.wide() && !self.open {
                k.push("⏎ details");
            }
            match kind {
                Kind::Lane => {
                    k.extend(["s start", "S round now", "x stop", "o open here", "H hold", "p pause lanes"]);
                    if row.key.starts_with("nexo:") {
                        k.push("n message");
                    }
                    k.push("a add lane");
                }
                Kind::This => {
                    k.push("n message");
                    k.push("⏎ observe");
                }
                Kind::Sub => {
                    if self.local_job().is_some_and(|j| self.runs.iter().any(|(id, _)| *id == j)) {
                        k.push("n message");
                    }
                    if self.local_job().is_some() && row.state != State::Done {
                        k.push("k stop");
                    }
                    k.push("⏎ output");
                }
                Kind::Job => {
                    k.push("⏎ output");
                }
                Kind::Peer | Kind::Other => {
                    k.extend(["n message", "⏎ observe"]);
                }
            }
            // under 110 columns Enter first opens the pane; what Enter does on the row is listed once it is open
            if !self.wide() && !self.open {
                k.retain(|s| !s.starts_with('⏎') || *s == "⏎ details");
            }
            if *kids > 0 {
                k.push("←→ fold");
            }
            if sup && !matches!(kind, Kind::Sub | Kind::Job) && !(kind == Kind::Lane && row.key.starts_with("lane:")) {
                k.extend(["z pause watch", "Z pause all"]);
            }
        }
        if self.wide() || self.open {
            k.push("Tab pane");
        }
        k.extend(["/ filter", "f show", "F source"]);
        if sup {
            k.push("P policy");
        }
        k
    }

    /// The footer: the message being written, a question, the last action's news, or the keys of the selection.
    fn footer(&self, theme: &Theme, w: usize) -> Line<'static> {
        if let Some(id) = self.kill_ask {
            let ask = format!("stop job {id}? y stops it · any other key keeps it");
            return Line::from(Span::styled(truncate(&ask, w), theme.bold(theme.warn2)));
        }
        if let Some(t) = &self.edit {
            // the end of the text is what is being typed: it is the part kept when the line is long
            let gone = self.edit_for.as_ref().is_some_and(|(k, _)| !self.rows.iter().any(|r| &r.key == k));
            let tails: [&str; 3] = if gone {
                ["  session gone · Esc keep", "  gone · Esc", ""]
            } else {
                ["  Enter send · Esc cancel", "  Enter · Esc", ""]
            };
            // the longest tail that still leaves 12 columns for the message
            let tail = tails.into_iter().find(|s| w >= width(s) + 12).unwrap_or("");
            let shown =
                tail_of(&format!("message › {t}▏"), w.saturating_sub(width(tail)), "message › ".chars().count());
            return Line::from(vec![
                Span::styled(shown, theme.style(theme.accent)),
                Span::styled(tail, if gone { theme.style(theme.warn) } else { theme.dim2() }),
            ]);
        }
        if self.finding {
            let t = self.find.clone().unwrap_or_default();
            let tail = "  Enter keep · Esc clear";
            let shown = tail_of(&format!("filter › {t}▏"), w.saturating_sub(width(tail)), "filter › ".chars().count());
            return Line::from(vec![Span::styled(shown, theme.style(theme.accent)), Span::styled(tail, theme.dim2())]);
        }
        if self.form.is_some() {
            return Line::from(Span::styled(truncate("Enter answer · Esc give up", w), theme.dim2()));
        }
        if let Some(n) = &self.note {
            return Line::from(Span::styled(truncate(n, w), theme.style(theme.accent2)));
        }
        if self.policy_page {
            return Line::from(Span::styled(truncate("↑↓ scroll · P/Esc back", w), theme.dim2()));
        }
        if self.observing.is_some() {
            return Line::from(Span::styled(truncate("↑↓ scroll · n message · Esc back", w), theme.dim2()));
        }
        // the marks of lines out of sight are what must survive a narrow window, then "Esc close"
        let more = self.more_marks();
        let close = if self.open || self.focus { "Esc back" } else { "Esc close" };
        let room = w.saturating_sub(width(&more) + width(close) + 3);
        let mut keys = self.footer_keys();
        while !keys.is_empty() && width(&keys.join(" · ")) > room {
            keys.pop();
        }
        let mut text = keys.join(" · ");
        if !text.is_empty() {
            text.push_str(" · ");
        }
        text.push_str(close);
        let mut spans = vec![Span::styled(text, theme.dim2())];
        if !more.is_empty() {
            let used: usize = spans.iter().map(|s| width(&s.content)).sum();
            spans.push(Span::raw(" ".repeat(w.saturating_sub(used + width(&more)))));
            spans.push(Span::styled(more, theme.style(theme.accent2)));
        }
        Line::from(spans)
    }

    /// `↑ 2 ↓ 3` when tree lines sit out of sight above and below, so a scrolled tree never looks complete.
    fn more_marks(&self) -> String {
        let total = self.items().len();
        let Some((first, last)) = self.lines.first().zip(self.lines.last()).map(|(a, b)| (a.0, b.0)) else {
            return String::new();
        };
        let below = total.saturating_sub(last + 1);
        match (first, below) {
            (0, 0) => String::new(),
            (0, n) => format!("↓ {n} more"),
            (n, 0) => format!("↑ {n} above"),
            (a, b) => format!("↑ {a} ↓ {b}"),
        }
    }
}

/// The last `room` columns of `text`, with an ellipsis in front when something was cut; the first `keep`
/// characters (a label) are always kept.
pub(super) fn tail_of(text: &str, room: usize, keep: usize) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    let head: String = text.chars().take(keep).collect();
    let rest: Vec<char> = text.chars().skip(keep).collect();
    let mut budget = room.saturating_sub(width(&head) + 1);
    let mut tail: Vec<char> = Vec::new();
    for c in rest.iter().rev() {
        let w = width(&c.to_string());
        if w > budget {
            break;
        }
        budget -= w;
        tail.push(*c);
    }
    tail.reverse();
    format!("{head}…{}", tail.into_iter().collect::<String>())
}
