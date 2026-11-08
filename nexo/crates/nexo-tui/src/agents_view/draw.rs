//! The body of `/panel`: the groups under their headers, and each session as a card, a line or a block.

use std::time::Instant;

use nexo_agent::supervise::{AgentRow, EvDetail, EvKind, Secs, SourceKind, State};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::{
    age, attention, chip, chip_color, split_evenly, AgentsView, Band, Filter, Group, Layout, BLOCK_W, BREATH_MS,
    CARD_MAX_W, CHIP_MS, EMPTY_STATE, GAP,
};
use crate::anim::pulse;
use crate::motion::{fade, fixed, hump, lit_style};
use crate::panel_fx as px;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// The contrast ratio (WCAG) of two RGB colours; 21 for none that is not RGB (a mapped theme keeps its own colours).
fn contrast(a: Color, b: Color) -> f32 {
    let lum = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
        }
        _ => None,
    };
    match (lum(a), lum(b)) {
        (Some(x), Some(y)) => (x.max(y) + 0.05) / (x.min(y) + 0.05),
        _ => 21.0,
    }
}

/// `c` moved towards the text colour until it reads at 4.5:1 (AA) on a card, resting and at its most lit. A hue
/// that already reads is returned as it is; a mono theme has no hues.
pub(super) fn legible(theme: &Theme, c: Color) -> Color {
    if theme.mono {
        return c;
    }
    let sel = theme.selection().bg.unwrap_or(theme.border);
    let grounds = [theme.modal_bg, fade(theme, theme.modal_bg, sel, CARD_LIT_MAX)];
    (0..=10)
        .map(|n| crate::theme::mix(c, theme.fg, n as f32 / 10.0))
        .find(|x| grounds.iter().all(|g| contrast(*x, *g) >= 4.5))
        .unwrap_or(theme.fg)
}

/// How far a card's ground may go towards the selection colour: past this the dim text on it stops reading (4.5:1);
/// the accent border says "selected" on its own.
const CARD_LIT_MAX: f32 = 0.15;

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

/// The colour a chip is drawn in: a finished session is softened, an attention chip breathes towards the dim text
/// colour by `wave` (0..1) of its depth.
fn tone(theme: &Theme, state: State, base: Color, wave: f32) -> Color {
    if attention(state) {
        legible(theme, fade(theme, base, theme.dim, 0.45 * wave))
    } else if state == State::Done {
        fade(theme, base, theme.dim, 0.4)
    } else {
        base
    }
}

/// A step cut to `room` columns. A path at its end keeps its tail (the file is what tells steps apart), the verb in
/// front stays whole; anything else keeps its start.
fn fit_step(step: &str, room: usize) -> String {
    if width(step) <= room {
        return step.to_string();
    }
    if let Some((head, path)) = step.rsplit_once(char::is_whitespace) {
        let left = room.saturating_sub(width(head) + 1);
        if path.contains('/') && left >= 6 {
            let mut tail = String::new();
            for g in unicode_segmentation::UnicodeSegmentation::graphemes(path, true).rev() {
                if width(&tail) + width(g) + 1 > left {
                    break;
                }
                tail.insert_str(0, g);
            }
            return format!("{head} …{tail}");
        }
    }
    truncate(step, room)
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
        let mut wave = 0.0;
        if fx && attention(row.state) {
            let opened = self.opened.unwrap_or(now);
            let ms = now.saturating_duration_since(opened).as_millis() % BREATH_MS;
            let phase = ms as f32 / BREATH_MS as f32 * std::f32::consts::TAU;
            wave = 0.5 - 0.5 * phase.cos();
        }
        let color = tone(theme, row.state, base, wave);
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
            SourceKind::Nexo => legible(theme, theme.accent),
            SourceKind::External => legible(theme, theme.accent2),
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
        if self.rows.is_empty() && self.filter == Filter::All {
            self.grid.clear();
            self.scroll = 0;
            Self::empty_state(f, body, theme);
            return;
        }
        let bands = Self::bands(groups, cols, matches!(layout, Layout::Cards { .. }));
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
    fn row_widths(layout: Layout, full_w: u16, card_w: u16, n: usize) -> Vec<u16> {
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
            out.push(Span::styled(
                format!("{what} · {}", self.since_action(*t)),
                theme.style(legible(theme, theme.accent2)),
            ));
        }
        out
    }

    /// `◐ retry 12s`, or `⧖ ▰▰▱▱▱ 12s` while a rate limit drains.
    fn wait_of(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Option<Span<'static>> {
        let a = self.ascii;
        let phase = if fx { self.since_open(now) } else { 0 };
        if let Some(s) = row.retry_secs {
            return Some(Span::styled(
                format!("{} retry {s}s", px::retry_frame(phase, a)),
                theme.style(legible(theme, theme.warn)),
            ));
        }
        let e = row.ribbon.last().filter(|e| e.kind == EvKind::RateLimit)?;
        let EvDetail::Wait { secs } = e.detail else { return None };
        let left = (secs as i64 - (self.clock - e.at)).max(0) as u32;
        if left == 0 {
            return None;
        }
        let bar = px::drain(left, secs, 5);
        let bar = if a { bar.replace('▰', "#").replace('▱', ".") } else { bar };
        Some(Span::styled(
            format!("{} {bar} {left}s", px::glyph(EvKind::RateLimit, a)),
            theme.style(legible(theme, theme.warn)),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn card(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
        let a = self.ascii;
        let level = self.lit(i, row, now).min(CARD_LIT_MAX);
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
            Some(s) => Span::styled(format!("▸ {}", fit_step(s, step_room.saturating_sub(2))), theme.dim()),
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
        // what the left side keeps as the right one claims room: the cost goes first, the time only after it
        let left_of = |tier: usize| {
            let rest = match tier {
                0 => format!("{cost}{quiet_part}"),
                1 => quiet_part.clone(),
                _ => cost.clone(),
            };
            let tag_room = cw.saturating_sub(width(&rest));
            let tag = truncate(&tag, tag_room.max(3));
            (tag, rest)
        };
        let rights = self.right_of_line3(row, theme, now, fx);
        let mut chosen: Option<(usize, Span<'static>)> = None;
        'pick: for rt in rights {
            for tier in 0..3 {
                let (t, rest) = left_of(tier);
                if width(&t) + width(&rest) + 2 + width(&rt.content) <= cw {
                    chosen = Some((tier, rt));
                    break 'pick;
                }
            }
        }
        let (tag, rest) = left_of(chosen.as_ref().map_or(0, |(tier, _)| *tier));
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
        let quiet_w = 4usize;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_view::{balanced, split_evenly};

    fn lum(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else { return 0.0 };
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
    }

    fn ratio(a: Color, b: Color) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    const STATES: [State; 7] = [
        State::WaitingPermission,
        State::WaitingAnswer,
        State::Stalled,
        State::Looping,
        State::Working,
        State::Idle { min: 0 },
        State::Done,
    ];

    /// The coloured text of a card (chips, breathing and softened, tags, the supervisor's action, waits) reads at
    /// AA on the card's ground, resting and at its most lit, in every theme that has colours; the plain, dim and
    /// dimmer text keeps the contrast its theme promises (dark, nexo and light).
    #[test]
    fn card_text_reads_on_its_ground_in_every_theme() {
        let mut weak: Vec<String> = Vec::new();
        for name in crate::theme::NAMES {
            let t = Theme::by_name(name).unwrap();
            if t.mono {
                continue;
            }
            let selected = t.selection().bg.unwrap_or(t.border);
            let lit = crate::motion::fade(&t, t.modal_bg, selected, CARD_LIT_MAX);
            for ground in [t.modal_bg, lit] {
                let mut hues = vec![
                    ("tag nexo", legible(&t, t.accent)),
                    ("tag other", legible(&t, t.accent2)),
                    ("action", legible(&t, t.accent2)),
                    ("retry", legible(&t, t.warn)),
                ];
                for s in STATES {
                    let base = chip_color(&t, s);
                    for wave in [0.0, 1.0] {
                        hues.push((chip(s).1, tone(&t, s, base, wave)));
                    }
                }
                for (what, c) in hues {
                    // a finished session is softened on purpose, down to 3:1
                    let need = if what == "done" { 3.0 } else { 4.5 };
                    let r = ratio(c, ground);
                    if r < need {
                        weak.push(format!("{name}: {what} is {r:.2}:1 on {ground:?}, wants {need}"));
                    }
                }
            }
            if matches!(*name, "dark" | "nexo" | "light") {
                for (what, c, floor) in [("name", t.fg, 4.5), ("dim", t.dim, 4.0), ("dim2", t.dim2, 3.0)] {
                    for ground in [t.modal_bg, lit] {
                        if ratio(c, ground) < floor {
                            weak.push(format!("{name}: {what} is {:.2}:1 on {ground:?}", ratio(c, ground)));
                        }
                    }
                }
            }
        }
        assert!(weak.is_empty(), "{}", weak.join("\n"));
    }

    #[test]
    fn a_hue_that_reads_is_left_alone_and_one_that_does_not_moves_to_the_text_colour() {
        let t = Theme::dark();
        assert_eq!(legible(&t, t.success), t.success);
        let faint = Color::Rgb(0x30, 0x30, 0x30);
        let fixed = legible(&t, faint);
        assert!(ratio(fixed, t.modal_bg) >= 4.5, "{fixed:?}");
        assert_eq!(legible(&Theme::mono(), faint), faint);
    }

    #[test]
    fn quiet_times_are_one_short_unit() {
        let got: Vec<String> =
            [0, 2, 59, 60, 95, 3599, 3600, 7300, 86399, 86400, 400_000].map(crate::agents_view::age).into();
        assert_eq!(got, ["0s", "2s", "59s", "1m", "1m", "59m", "1h", "2h", "23h", "1d", "4d"]);
    }

    #[test]
    fn every_state_has_its_own_glyph_so_colour_is_never_the_only_cue() {
        let glyphs: std::collections::HashSet<_> = STATES.iter().map(|s| chip(*s).0).collect();
        assert_eq!(glyphs.len(), STATES.len());
        let labels: std::collections::HashSet<_> = STATES.iter().map(|s| chip(*s).1).collect();
        assert_eq!(labels.len(), STATES.len());
    }

    #[test]
    fn a_step_keeps_the_end_of_its_path_and_cuts_other_text_at_the_end() {
        assert_eq!(fit_step("Edit  src/parser.rs", 40), "Edit  src/parser.rs");
        let cut = fit_step("Edit  crates/nexo-tui/src/agents_view/draw.rs", 24);
        assert!(cut.starts_with("Edit  …") && cut.ends_with("view/draw.rs") || cut.ends_with("draw.rs"), "{cut}");
        assert!(width(&cut) <= 24, "{cut}");
        let cut = fit_step("Bash  cargo test -p parser --lib", 16);
        assert!(cut.starts_with("Bash  cargo") && cut.ends_with('…') && width(&cut) <= 16, "{cut}");
        // CJK path: never wider than the room
        let cut = fit_step("Read  /项目/源代码/解析器/语法分析.rs", 20);
        assert!(width(&cut) <= 20, "{cut}");
    }

    #[test]
    fn rows_balance_and_cards_split_a_strip_to_the_cell() {
        assert_eq!(balanced(4, 3), 2);
        assert_eq!(balanced(5, 3), 3);
        assert_eq!(balanced(7, 4), 4);
        assert_eq!(balanced(5, 4), 3);
        assert_eq!(balanced(1, 4), 1);
        assert_eq!(balanced(0, 4), 1);
        assert_eq!(split_evenly(113, 3), vec![38, 38, 37]);
        let l = Layout::Cards { cols: 3, card_w: 37 };
        for n in 1..=4 {
            let w = AgentsView::row_widths(l, 116, 37, n);
            let gaps = GAP * (n as u16 - 1);
            assert_eq!(w.iter().sum::<u16>() + gaps, 116.min(n as u16 * CARD_MAX_W + gaps), "{n}: {w:?}");
        }
    }
}
