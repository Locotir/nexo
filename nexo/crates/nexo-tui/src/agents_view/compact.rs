//! The compact forms of a session: one line, and one block of the far zoom.

use std::time::Instant;

use nexo_agent::supervise::{AgentRow, SourceKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{age, chip, chip_color, AgentsView};
use crate::motion::{fade, fixed, lit_style};
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
    /// The far zoom: a tinted block with the state's colour down its left edge, the name, the chip and, on
    /// the foot, the last events as coloured half blocks.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn block(
        &self,
        f: &mut Frame,
        r: Rect,
        i: usize,
        row: &AgentRow,
        theme: &Theme,
        now: Instant,
        fx: bool,
    ) {
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

    /// One session as a line, in `r`: chip | name | mail badge | sub-agents | last events | quiet | step | todo
    /// | tokens, dropped from the right as the width shrinks.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn list_line(
        &self,
        f: &mut Frame,
        r: Rect,
        i: usize,
        row: &AgentRow,
        theme: &Theme,
        now: Instant,
        fx: bool,
    ) {
        let w = r.width as usize;
        let level = self.lit(i, row, now);
        let bg = lit_style(theme, Style::default().bg(theme.modal_bg), level);
        let chip = self.chip_spans(row, theme, now, fx);
        let shown_w: usize = chip.iter().map(|s| width(&s.content)).sum();
        // A paused chip can be wider than the normal column; account for every cell.
        let chip_w = 13usize.max(shown_w);
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
}
