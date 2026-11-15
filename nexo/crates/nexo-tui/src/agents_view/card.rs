//! One session as a card: its name, step, source, last events and the chip on the right.

use std::time::Instant;

use nexo_agent::supervise::{AgentRow, EvDetail, EvKind, SourceKind, State};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::draw::{fit_step, legible, tone, CARD_LIT_MAX};
use super::{age, attention, chip, AgentsView, BREATH_MS, CHIP_MS};
use crate::anim::pulse;
use crate::motion::{fade, hump, lit_style};
use crate::panel_fx as px;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

impl AgentsView {
    /// The chip as spans: glyph and label, breathing when it wants a person.
    pub(super) fn chip_spans(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Vec<Span<'static>> {
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

    pub(super) fn name_of(row: &AgentRow) -> String {
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

    pub(super) fn tag_color(theme: &Theme, row: &AgentRow) -> Color {
        match row.source {
            SourceKind::Nexo => legible(theme, theme.accent),
            SourceKind::External => legible(theme, theme.accent2),
        }
    }

    /// How lit a card's ground is: selected, gliding, a soft hump while its state changes, a glow as a letter
    /// lands on it.
    pub(super) fn lit(&self, i: usize, row: &AgentRow, now: Instant) -> f32 {
        let sel = if self.selected == i && self.still { 1.0 } else { self.glide.level(i, now) };
        let changed = self.fx.get(&row.key).and_then(|x| x.changed);
        sel.max(0.5 * hump(changed, now, CHIP_MS)).max(self.land_glow(&row.key, now))
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
    pub(super) fn card(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
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
        let step = if let Some(s) = &row.last_conclusion {
            Span::styled(truncate(&format!("last result: {s}"), step_room), theme.dim())
        } else {
            match &row.last_step {
                Some(s) => Span::styled(format!("▸ {}", fit_step(s, step_room.saturating_sub(2))), theme.dim()),
                None => Span::styled(truncate("▸ no step seen yet", step_room), theme.dim2()),
            }
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
}
