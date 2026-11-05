//! The body of `/panel`: the groups under their headers, and each session as a card, a line or a block.

use std::time::Instant;

use nexo_agent::supervise::{AgentRow, EvDetail, EvKind, Secs, SourceKind, State};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::{
    age, attention, chip, chip_color, AgentsView, Band, Filter, Group, Layout, BLOCK_W, BREATH_MS, CHIP_MS, GAP,
};
use crate::anim::pulse;
use crate::motion::{fade, fixed, hump, lit_style};
use crate::panel_fx as px;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// The ribbon cells of a line at this width: none under 44 columns, then 4, 8 and 12.
fn list_ribbon(w: usize) -> usize {
    match w {
        0..=43 => 0,
        44..=69 => 4,
        70..=99 => 8,
        _ => 12,
    }
}

/// The braille glyph that counts running sub-agents in a line.
fn dots(n: usize) -> char {
    ['⠁', '⠃', '⠇', '⠧', '⠷', '⠿', '⡿', '⣿'][n.clamp(1, 8) - 1]
}

impl AgentsView {
    /// The line under a header with no sessions.
    fn nothing_line(&self, group: &Group) -> &'static str {
        match (group.source, self.filter) {
            (SourceKind::Nexo, _) => "no nexo sessions yet · this one shows up with your first message",
            (SourceKind::External, Filter::External) => "no sessions from other programs found",
            (SourceKind::External, _) => "no sessions from other programs",
        }
    }

    /// The chip as spans: glyph and label, breathing when it wants a person.
    fn chip_spans(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Vec<Span<'static>> {
        let (shown, base) = self.chip_at(row, theme, now);
        let (g, label) = chip(shown);
        let mut color = base;
        if fx && attention(row.state) {
            let opened = self.opened.unwrap_or(now);
            let ms = now.saturating_duration_since(opened).as_millis() % BREATH_MS;
            let phase = ms as f32 / BREATH_MS as f32 * std::f32::consts::TAU;
            color = fade(theme, base, theme.dim, 0.45 * (0.5 - 0.5 * phase.cos()));
        } else if row.state == State::Done {
            color = fade(theme, base, theme.dim, 0.4);
        }
        let mut style = theme.style(color);
        if attention(row.state) || row.state == State::Looping {
            style = style.add_modifier(Modifier::BOLD);
        }
        vec![Span::styled(if row.paused { format!("{g} {label} ⏸") } else { format!("{g} {label}") }, style)]
    }

    fn name_of(row: &AgentRow) -> String {
        let mark = match row.source {
            SourceKind::Nexo => "▣",
            SourceKind::External => "▢",
        };
        if row.parent.is_some() {
            format!("└ {mark} {}", row.name)
        } else {
            format!("{mark} {}", row.name)
        }
    }

    /// The small source tag every card carries: `[nexo]`, or `[program]` for another tool.
    fn tag_of(row: &AgentRow) -> String {
        let name = if row.source == SourceKind::Nexo { "nexo" } else { row.program.as_str() };
        format!("[{}]", truncate(name, 14))
    }

    fn tag_color(theme: &Theme, row: &AgentRow) -> Color {
        match row.source {
            SourceKind::Nexo => theme.accent,
            SourceKind::External => theme.accent2,
        }
    }

    pub(super) fn since_action(&self, at: Secs) -> String {
        let d = (self.clock - at).max(0) as u64;
        format!("{} ago", age(d))
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
        let bands = Self::bands(groups, cols);
        self.grid = bands.iter().filter_map(|b| if let Band::Row(r) = b { Some(r.clone()) } else { None }).collect();
        // the strip that holds the selection, and the first strip that keeps it in sight
        let sel = bands.iter().position(|b| matches!(b, Band::Row(r) if r.contains(&self.selected))).unwrap_or(0);
        // a group's first row brings its header along
        let sel_top = if sel > 0 && matches!(bands[sel - 1], Band::Header(_)) { sel - 1 } else { sel };
        let fits = |from: usize, to: usize| Self::span(&bands, from, to + 1, item_h, tight) <= body.height;
        if sel_top < self.scroll {
            self.scroll = sel_top;
        }
        while self.scroll < sel && !fits(self.scroll, sel) {
            self.scroll += 1;
        }
        // never scroll past the point where the last strip is at the foot
        let mut last_first = bands.len().saturating_sub(1);
        while last_first > 0 && fits(last_first - 1, bands.len() - 1) {
            last_first -= 1;
        }
        self.scroll = self.scroll.min(last_first);

        let used_w = if tight { body.width } else { card_w * cols as u16 + GAP * (cols as u16 - 1) };
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
                    for (c, &i) in items.iter().enumerate() {
                        let row = &shown[i];
                        let r = if tight {
                            Rect::new(body.x, y, body.width, 1)
                        } else {
                            Rect::new(x0 + c as u16 * (card_w + GAP), y, card_w, item_h)
                        };
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

    /// How lit a card's ground is: selected, gliding, a soft hump while its state changes, a glow as a letter
    /// lands on it.
    fn lit(&self, i: usize, row: &AgentRow, now: Instant) -> f32 {
        let sel = if self.selected == i && self.still { 1.0 } else { self.glide.level(i, now) };
        let changed = self.fx.get(&row.key).and_then(|x| x.changed);
        sel.max(0.5 * hump(changed, now, CHIP_MS)).max(self.land_glow(&row.key, now))
    }

    /// The far zoom: a tinted block with the state's colour down its left edge, the name, the chip and, on
    /// the foot, the last events as coloured half blocks.
    #[allow(clippy::too_many_arguments)]
    fn block(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
        let (shown, color) = self.chip_at(row, theme, now);
        let (g, _) = chip(shown);
        let tint = fade(theme, theme.modal_bg, color, 0.2);
        let bg = lit_style(theme, Style::default().bg(tint), self.lit(i, row, now) * 0.6);
        let selected = self.selected == i;
        f.render_widget(Paragraph::new("").style(bg), r);
        let bar = if selected { "█" } else { "▌" };
        for dy in 0..r.height {
            f.render_widget(
                Paragraph::new(Span::styled(bar, theme.style(color))).style(bg),
                Rect::new(r.x, r.y + dy, 1, 1),
            );
        }
        let w = r.width.saturating_sub(3) as usize;
        let mut name = theme.bold(theme.fg);
        if selected {
            name = name.add_modifier(Modifier::UNDERLINED);
        }
        let badge = self.badge(row, theme, now, fx);
        let badge_w = badge.as_ref().map_or(0, |b| width(&b.content) + 1);
        let name_line = Span::styled(truncate(&row.name, w), name);
        let quiet = row.idle_secs.map_or_else(|| "-".to_string(), age);
        let sub = format!("{g} {}", truncate(&quiet, w.saturating_sub(2)));
        let at = |dy| Rect::new(r.x + 2, r.y + dy, w as u16, 1);
        f.render_widget(Paragraph::new(Line::from(name_line)).style(bg), at(0));
        let mut sub_line = vec![Span::styled(sub.clone(), theme.style(color))];
        let prog = if row.source == SourceKind::Nexo { "nexo" } else { row.program.as_str() };
        let room = w.saturating_sub(width(&sub) + 1);
        // the whole name or none: a cut-off name would read as another program
        if width(prog) <= room {
            sub_line.push(Span::styled(format!(" {prog}"), theme.style(Self::tag_color(theme, row))));
        }
        f.render_widget(Paragraph::new(Line::from(sub_line)).style(bg), at(1));
        if r.height > 2 {
            // the foot: the last events as half blocks (their own glyphs in mono and ASCII), then the badge
            let cells = w.saturating_sub(badge_w).min(12);
            let mut foot: Vec<Span<'static>> = Vec::new();
            if row.ribbon.is_empty() {
                let step = row.last_step.clone().unwrap_or_default();
                foot.push(Span::styled(truncate(&step, w.saturating_sub(badge_w)), theme.dim2()));
            } else {
                let (rc, _) = px::ribbon(&row.ribbon, cells, theme, self.ascii, self.clock, None);
                for c in rc {
                    if self.ascii || theme.mono {
                        foot.push(Span::styled(c.text, c.style));
                    } else if c.kind.is_none() {
                        foot.push(Span::raw(c.text));
                    } else {
                        foot.push(Span::styled("▀".repeat(c.width()), c.style));
                    }
                }
            }
            if let Some(b) = badge {
                let used: usize = foot.iter().map(|s| width(&s.content)).sum();
                foot.push(Span::raw(" ".repeat((w.saturating_sub(used + badge_w)) + 1)));
                foot.push(b);
            }
            f.render_widget(Paragraph::new(Line::from(foot)).style(bg), at(2));
            // running sub-agents: one dot on the corner
            if Self::running_subs(row) > 0 {
                let dot = if self.ascii { "o" } else { "●" };
                f.render_widget(
                    Paragraph::new(Span::styled(dot, theme.style(theme.success))).style(bg),
                    Rect::new(r.x + r.width - 1, r.y + r.height - 1, 1, 1),
                );
            }
        }
    }

    /// The right end of a card's third line: the mail receipt for twenty seconds, else the wait of a retry or
    /// a rate limit, else the supervisor's last action. In priority order, for the caller to take what fits.
    fn right_of_line3(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        if let Some(s) = self.receipt(row, theme) {
            out.push(s);
        }
        if let Some(s) = self.wait_of(row, theme, now, fx) {
            out.push(s);
        }
        if let Some((what, t)) = &row.last_action {
            out.push(Span::styled(format!("{what} · {}", self.since_action(*t)), theme.style(theme.accent2)));
        }
        out
    }

    /// `◐ retry 12s`, or `⧖ ▰▰▱▱▱ 12s` while a rate limit drains.
    fn wait_of(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Option<Span<'static>> {
        let a = self.ascii;
        let phase = if fx { self.since_open(now) } else { 0 };
        if let Some(s) = row.retry_secs {
            return Some(Span::styled(format!("{} retry {s}s", px::retry_frame(phase, a)), theme.style(theme.warn)));
        }
        let e = row.ribbon.last().filter(|e| e.kind == EvKind::RateLimit)?;
        let EvDetail::Wait { secs } = e.detail else { return None };
        let left = (secs as i64 - (self.clock - e.at)).max(0) as u32;
        if left == 0 {
            return None;
        }
        let bar = px::drain(left, secs, 5);
        let bar = if a { bar.replace('▰', "#").replace('▱', ".") } else { bar };
        Some(Span::styled(format!("{} {bar} {left}s", px::glyph(EvKind::RateLimit, a)), theme.style(theme.warn)))
    }

    #[allow(clippy::too_many_arguments)]
    fn card(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
        let a = self.ascii;
        let level = self.lit(i, row, now);
        let selected = self.selected == i;
        let base = Style::default().bg(theme.modal_bg);
        let bg = lit_style(theme, base, level);
        let mut border = if selected { theme.accent } else { theme.border };
        if let Some((k, at)) = &self.pulse_at {
            if *k == row.key {
                border = fade(theme, border, theme.accent2, pulse(Some(*at), now));
            }
        }
        border = self.border_fx(row, border, theme, now, fx);
        let mut bstyle = theme.style(border);
        if selected && theme.mono {
            bstyle = bstyle.add_modifier(Modifier::BOLD);
        }
        let block =
            Box_::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(bstyle).style(bg);
        let inner = block.inner(r);
        f.render_widget(block, r);
        let cw = inner.width.saturating_sub(2) as usize;
        let at = |dy: u16| Rect::new(inner.x + 1, inner.y + dy, cw as u16, 1);
        let squash = if fx { self.squash_of(&row.key, now) } else { 0.0 };

        // line 1: the name, the mail badge, the chip on the right
        let chip = self.chip_spans(row, theme, now, fx);
        let chip_w: usize = chip.iter().map(|s| width(&s.content)).sum();
        let badge = self.badge(row, theme, now, fx);
        let badge_w = badge.as_ref().map_or(0, |b| width(&b.content) + 1);
        let name = truncate(&Self::name_of(row), cw.saturating_sub(chip_w + badge_w + 1));
        let gap = cw.saturating_sub(width(&name) + badge_w + chip_w);
        let mut l1 = vec![Span::styled(name, theme.bold(theme.fg)), Span::raw(" ".repeat(gap))];
        if let Some(b) = badge {
            l1.push(b);
            l1.push(Span::raw(" "));
        }
        l1.extend(chip);
        f.render_widget(Paragraph::new(Line::from(l1)).style(bg), at(0));

        // line 2: the last step; a todo bar takes its tail when there is one
        let todo = row.todo.filter(|&(d, t)| t > 0 && cw >= 20 && cw >= px::todo_width(d, t) + 12);
        let bar_w = todo.map_or(0, |(d, t)| px::todo_width(d, t) + 1);
        let step_room = cw.saturating_sub(bar_w);
        let step = match &row.last_step {
            Some(s) => Span::styled(format!("▸ {}", truncate(s, step_room.saturating_sub(2))), theme.dim()),
            None => Span::styled(truncate("▸ no step seen yet", step_room), theme.dim2()),
        };
        let mut l2 = vec![step];
        if let Some((d, t)) = todo {
            let used = width(&l2[0].content);
            l2.push(Span::raw(" ".repeat(cw.saturating_sub(used + bar_w) + 1)));
            let frac = if fx {
                self.live.todo_shown.get(&row.key).copied().unwrap_or(d as f64 / t as f64)
            } else {
                d as f64 / t as f64
            };
            l2.extend(px::todo_spans(theme, frac, d, t, a));
        }
        f.render_widget(Paragraph::new(Line::from(px::squash(l2, cw, squash))).style(bg), at(1));

        // line 3: source, cost, quiet for; at the right the receipt, a wait, or the supervisor's last action
        let quiet = row.idle_secs.map_or_else(|| "-".to_string(), age);
        let tag = Self::tag_of(row);
        let mut cost = String::new();
        if let Some(c) = row.cost_usd.filter(|c| *c > 0.0) {
            cost = format!(" · ${c:.2}");
        }
        let quiet_part = format!(" · quiet {quiet}");
        let left_of = |with_quiet: bool| {
            let rest = if with_quiet { format!("{cost}{quiet_part}") } else { cost.clone() };
            let tag_room = cw.saturating_sub(width(&rest));
            let tag = truncate(&tag, tag_room.max(3));
            (tag, rest)
        };
        let rights = self.right_of_line3(row, theme, now, fx);
        let mut chosen: Option<(bool, Span<'static>)> = None;
        'pick: for rt in rights {
            for with_quiet in [true, false] {
                let (t, rest) = left_of(with_quiet);
                if width(&t) + width(&rest) + 2 + width(&rt.content) <= cw {
                    chosen = Some((with_quiet, rt));
                    break 'pick;
                }
            }
        }
        let with_quiet = !matches!(chosen, Some((false, _)));
        let (tag, rest) = left_of(with_quiet);
        let used = width(&tag) + width(&rest);
        let rest = truncate(&rest, cw.saturating_sub(used - width(&rest)));
        let mut l3 = vec![Span::styled(tag, theme.style(Self::tag_color(theme, row))), Span::styled(rest, theme.dim())];
        if let Some((_, rt)) = chosen {
            l3.push(Span::raw(" ".repeat(cw.saturating_sub(used + width(&rt.content)))));
            l3.push(rt);
        }
        f.render_widget(Paragraph::new(Line::from(px::squash(l3, cw, squash))).style(bg), at(2));

        // line 4: the last events, newest at the right, then the tokens of the last minutes
        if inner.height >= 4 {
            let cells = if cw >= 20 { 12 } else { 6 };
            let phase = self.live_phase(row, now, fx);
            let (mut rc, marks) = px::ribbon(&row.ribbon, cells, theme, a, self.clock, phase);
            if fx && row.state == State::WaitingPermission {
                // the lock breathes
                let b = px::breath(self.since_open(now), 1200);
                for c in rc.iter_mut().filter(|c| c.kind == Some(EvKind::PermissionWait)) {
                    c.style = theme.bold(fade(theme, theme.warn2, theme.dim2, 0.6 * b));
                }
            }
            let mut l4 = px::ribbon_spans(&rc);
            if cw >= 28 {
                let mut spark = px::spark(&row.spark, 12);
                if a {
                    spark = spark.chars().map(|c| crate::ascii::fold_char(c).unwrap_or(c)).collect();
                }
                if !spark.is_empty() {
                    let sw = width(&spark);
                    l4.push(Span::raw(" ".repeat(cw.saturating_sub(cells + sw))));
                    l4.push(Span::styled(spark, theme.style(theme.accent2)));
                }
            }
            f.render_widget(Paragraph::new(Line::from(l4)).style(bg), at(3));
            if fx {
                self.draw_ribbon_fx(f, r, at(3).x, at(3).y, &marks, row, theme, now);
            }
        }
        self.draw_sats(f, r, row, theme, now, fx);
    }

    /// One session as a line, in `r`: chip | name | mail badge | sub-agents | last events | quiet | step | todo
    /// | tokens, dropped from the right as the width shrinks.
    #[allow(clippy::too_many_arguments)]
    fn list_line(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
        let w = r.width as usize;
        let level = self.lit(i, row, now);
        let bg = lit_style(theme, Style::default().bg(theme.modal_bg), level);
        let chip = self.chip_spans(row, theme, now, fx);
        let chip_w = 13usize;
        let shown_w: usize = chip.iter().map(|s| width(&s.content)).sum();
        let quiet = row.idle_secs.map(age).unwrap_or_else(|| "-".to_string());
        let quiet_w = 7usize;
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled(if self.selected == i { "▌" } else { " " }, theme.style(theme.accent)));
        let mut used = 1;
        if w >= chip_w + 12 {
            spans.extend(chip);
            spans.push(Span::raw(" ".repeat(chip_w.saturating_sub(shown_w))));
            used += chip_w;
        } else {
            // too narrow for the label: the glyph alone
            let g = super::chip(row.state).0;
            spans.push(Span::styled(format!("{g} "), theme.style(chip_color(theme, row.state))));
            used += 2;
        }
        let show_quiet = w >= 40;
        let show_step = w >= 64;
        let wide = w >= 100;
        let badge = self.badge(row, theme, now, fx);
        let subs = Self::running_subs(row);
        let ribbon_n = list_ribbon(w);
        let todo = row.todo.filter(|&(_, t)| t > 0 && wide);
        let mut spark = if wide { px::spark(&row.spark, 8) } else { String::new() };
        if self.ascii {
            spark = spark.chars().map(|c| crate::ascii::fold_char(c).unwrap_or(c)).collect();
        }
        let badge_w = badge.as_ref().map_or(0, |b| width(&b.content) + 1);
        let subs_w = if subs > 0 { 2 } else { 0 };
        let ribbon_w = if ribbon_n > 0 { ribbon_n + 1 } else { 0 };
        let tail_w =
            todo.map_or(0, |(d, t)| px::todo_width(d, t) + 1) + if spark.is_empty() { 0 } else { width(&spark) + 1 };
        let fixed_w = badge_w + subs_w + ribbon_w + tail_w + if show_quiet { quiet_w + 1 } else { 0 };
        let rest = w.saturating_sub(used).saturating_sub(fixed_w);
        let step_w = if show_step { rest * 45 / 100 } else { 0 };
        let name_room = rest.saturating_sub(if show_step { step_w + 1 } else { 0 });
        let name = truncate(&Self::name_of(row), name_room.max(1));
        let pad_name = name_room.saturating_sub(width(&name));
        spans.push(Span::styled(name, theme.bold(theme.fg)));
        spans.push(Span::raw(" ".repeat(pad_name)));
        if let Some(b) = badge {
            spans.push(Span::raw(" "));
            spans.push(b);
        }
        if subs > 0 {
            let g = if self.ascii { subs.min(9).to_string() } else { dots(subs).to_string() };
            spans.push(Span::styled(format!(" {g}"), theme.style(theme.success)));
        }
        if ribbon_n > 0 {
            let phase = self.live_phase(row, now, fx);
            let (rc, _) = px::ribbon(&row.ribbon, ribbon_n, theme, self.ascii, self.clock, phase);
            spans.push(Span::raw(" "));
            spans.extend(px::ribbon_spans(&rc));
        }
        if show_quiet {
            spans.push(Span::styled(format!(" {}", fixed(&quiet, &"8".repeat(quiet_w))), theme.dim()));
        }
        if show_step {
            let step = row.last_step.clone().unwrap_or_default();
            spans.push(Span::styled(format!("  {}", truncate(&step, step_w.saturating_sub(1))), theme.dim2()));
        }
        if let Some((d, t)) = todo {
            let frac = if fx {
                self.live.todo_shown.get(&row.key).copied().unwrap_or(d as f64 / t as f64)
            } else {
                d as f64 / t as f64
            };
            spans.push(Span::raw(" "));
            spans.extend(px::todo_spans(theme, frac, d, t, self.ascii));
        }
        if !spark.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(spark, theme.style(theme.accent2)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(bg), r);
    }

    pub(super) fn detail(&self, row: &AgentRow, theme: &Theme, w: usize) -> Vec<Span<'static>> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &row.cwd {
            parts.push(c.clone());
        }
        if let Some(m) = &row.permission_mode {
            parts.push(m.clone());
        }
        if let Some(m) = &row.model {
            parts.push(m.clone());
        }
        if self.open {
            parts.push(row.key.clone());
            parts.push(format!("{} since {}", row.state.label(), self.since_action(row.since)));
            if row.paused {
                parts.push("paused".into());
            }
        } else {
            parts.push(row.key.chars().take(12).collect());
        }
        let text = parts.join(" · ");
        vec![Span::styled(truncate(&text, w), theme.dim2())]
    }
}
