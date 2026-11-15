//! One line of the tree: a group's header, or a row with its animated state glyph, its name, and on the right
//! what fits of its fields, most useful first. A lane shows its round or its next one with a pie that fills
//! towards it, and its spend against its cap; a session its step, model, quiet time, cost and context; a
//! sub-agent or job its elapsed time and sparkline.

use nexo_agent::supervise::{EvKind, LaneInfo};
use ratatui::style::Color;

use super::draw::fit_step;
use super::tree::Item;
use super::*;
use crate::panel_fx as px;

/// The widest a name column gets before the fields.
const NAME_MAX: usize = 18;
/// Cells of a spend bar on a row.
const BAR_CELLS: usize = 7;

/// `▰▰▰▱▱▱▱`: `frac` of `cells`, at least one lit for any spend above nothing.
pub(super) fn bar(frac: f64, cells: usize, ascii: bool) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let mut on = (frac * cells as f64).round() as usize;
    if frac > 0.0 && on == 0 {
        on = 1;
    }
    let (full, empty) = if ascii { ("#", "-") } else { ("▰", "▱") };
    format!("{}{}", full.repeat(on.min(cells)), empty.repeat(cells - on.min(cells)))
}

/// A pie that fills towards the next round: `○ ◔ ◑ ◕ ●`.
pub(super) fn pie(frac: f64, ascii: bool) -> &'static str {
    const PIE: [&str; 5] = ["○", "◔", "◑", "◕", "●"];
    const ASCII: [&str; 5] = [".", ":", "-", "=", "#"];
    let i = (frac.clamp(0.0, 1.0) * 4.0).floor() as usize;
    if ascii {
        ASCII[i.min(4)]
    } else {
        PIE[i.min(4)]
    }
}

/// `HH:MM` local time of `secs`.
pub(super) fn clock_of(secs: Secs) -> String {
    nexo_core::team::clock(secs.saturating_mul(1000), nexo_agent::budget::local_offset())
}

/// The model's own name, without its provider: `xai/grok-4.7` is `grok-4.7`.
pub(super) fn short_model(m: &str) -> &str {
    m.rsplit_once('/').map_or(m, |(_, n)| n)
}

/// How far a lane is towards its next round, 0..1.
pub(super) fn round_frac(l: &LaneInfo, clock: Secs) -> Option<f64> {
    let next = l.next_round?;
    let every = l.every_secs? as i64;
    if every <= 0 {
        return None;
    }
    let left = (next - clock).clamp(0, every);
    Some(1.0 - left as f64 / every as f64)
}

/// A field on the right of a row: its text and colour.
type Field = (String, Color);

impl AgentsView {
    /// The glyph of a row and its colour, with the motion on it: a working row breathes, one that needs the
    /// person pulses amber, a lane whose round just ended pops its ✓ or ✗.
    pub(super) fn glyph(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> (&'static str, Color, bool) {
        let a = self.ascii;
        let phase = self.since_open(now);
        let breathe = |c: Color, depth: f32, period: u64| {
            if fx {
                fade(theme, c, theme.dim, depth * px::breath(phase, period))
            } else {
                c
            }
        };
        if let Some((at, ok)) = self.fx.get(&row.key).and_then(|x| x.round) {
            if fx && within(Some(at), now, POP_MS) {
                let g = match (ok, a) {
                    (true, false) => "✓",
                    (true, true) => "+",
                    (false, false) => "✗",
                    (false, true) => "x",
                };
                return (g, if ok { theme.success } else { theme.error }, true);
            }
        }
        if let Some(l) = &row.lane {
            return match l.state.as_str() {
                "running" => (if a { "*" } else { "●" }, breathe(theme.success, 0.5, BREATH_MS as u64), false),
                "failed" => (if a { "x" } else { "✗" }, theme.error, true),
                "held" => (if a { "h" } else { "◌" }, theme.warn, false),
                "paused" => (if a { "=" } else { "‖" }, theme.warn, false),
                "stopped" => (if a { "-" } else { "■" }, theme.dim2, false),
                _ => (if a { "o" } else { "○" }, theme.dim, false),
            };
        }
        if row.paused {
            return (if a { "=" } else { "‖" }, theme.dim, false);
        }
        let failed = row.state == State::Done && row.ribbon.last().is_some_and(|e| e.kind == EvKind::Error);
        if failed {
            return (if a { "x" } else { "✗" }, theme.error, true);
        }
        let (state, color) = self.chip_at(row, theme, now);
        let g = if a {
            match state {
                State::WaitingPermission | State::WaitingAnswer => "!",
                State::Stalled => "~",
                State::Looping => "@",
                State::Working => "*",
                State::Idle { .. } => "o",
                State::Done => "+",
            }
        } else {
            chip(state).0
        };
        // a row waiting for the person breathes towards the dim colour, never past what reads; a done one is softened
        let color = match state {
            State::Working => breathe(color, 0.5, BREATH_MS as u64),
            s => {
                let wave = if fx && attention(s) { px::breath(phase, px::WAIT_BREATH_MS) } else { 0.0 };
                super::draw::tone(theme, s, color, wave)
            }
        };
        (g, color, attention(state))
    }

    /// The fields of a row, most useful first (the last ones go first when the line is short), and its step,
    /// which takes the room left in the middle.
    fn fields(&self, row: &AgentRow, kind: Kind, theme: &Theme, now: Instant, fx: bool) -> (Vec<Field>, Option<String>) {
        let a = self.ascii;
        let mut out: Vec<Field> = Vec::new();
        let quiet = |r: &AgentRow| age((self.clock - r.since).max(0) as u64);
        if let Some(l) = &row.lane {
            match l.state.as_str() {
                "running" => out.push((format!("round {} {}", l.rounds + 1, quiet(row)), theme.success)),
                _ => match (l.next_round, round_frac(l, self.clock)) {
                    (Some(next), frac) if l.state == "idle" => {
                        let arrow = if a { "->" } else { "→" };
                        let p = frac.map(|f| format!(" {}", pie(f, a))).unwrap_or_default();
                        out.push((format!("{arrow} {}{p}", clock_of(next)), theme.accent2));
                    }
                    _ => out.push((l.state.clone(), if l.state == "failed" { theme.error } else { theme.dim })),
                },
            }
            if let Some(cap) = l.cap_day.filter(|c| *c > 0.0) {
                let spend = self.rolled(&row.key, l.spend_today, fx);
                let frac = spend / cap;
                out.push((bar(frac, BAR_CELLS, a), theme.level_color((frac * 100.0) as u64)));
            } else if l.spend_today > 0.0 {
                out.push((format!("${:.2}", self.rolled(&row.key, l.spend_today, fx)), theme.dim));
            }
            let step = (l.state == "running").then(|| row.last_step.clone()).flatten();
            return (out, step);
        }
        match kind {
            Kind::Sub | Kind::Job => {
                out.push((quiet(row), theme.dim));
                let spark = px::spark(&row.spark, 4);
                if !spark.is_empty() {
                    let spark =
                        if a { spark.chars().map(|c| crate::ascii::fold_char(c).unwrap_or(c)).collect() } else { spark };
                    out.push((spark, theme.accent2));
                }
                if let Some(c) = row.cost_usd.filter(|c| *c >= 0.005) {
                    out.push((format!("${:.2}", self.rolled(&row.key, c, fx)), theme.dim));
                }
                (out, row.last_step.clone())
            }
            _ => {
                if let Some(m) = &row.model {
                    out.push((truncate(short_model(m), 12), theme.dim2));
                }
                out.push((quiet(row), theme.dim));
                if let Some(c) = row.cost_usd.filter(|c| *c >= 0.005) {
                    out.push((format!("${:.2}", self.rolled(&row.key, c, fx)), theme.money));
                }
                if let Some(p) = row.ctx_pct {
                    out.push((bar(p as f64 / 100.0, 4, a), theme.level_color(p as u64)));
                }
                if let Some(b) = self.badge(row, theme, now, fx) {
                    out.insert(0, (b.content.to_string(), b.style.fg.unwrap_or(theme.accent2)));
                }
                let step = row.last_step.clone().or_else(|| {
                    (kind == Kind::Peer || kind == Kind::Other).then(|| row.cwd.clone().map(|c| tilde(&c))).flatten()
                });
                (out, step)
            }
        }
    }

    /// The widest name of the rows in sight, capped, so the fields of a group line up.
    pub(super) fn name_col(items: &[Item]) -> usize {
        items
            .iter()
            .filter_map(|it| match it {
                Item::Row { row, prefix, .. } => Some(width(&lead_of(prefix)) + width(&row.name)),
                Item::Head { .. } => None,
            })
            .max()
            .unwrap_or(0)
            .min(NAME_MAX)
    }

    /// One tree line in `w` columns.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn tree_line(
        &self,
        item: &Item,
        name_w: usize,
        w: usize,
        theme: &Theme,
        now: Instant,
        fx: bool,
        selected: bool,
    ) -> Line<'static> {
        let a = self.ascii;
        let mut spans: Vec<Span<'static>> = Vec::new();
        match item {
            Item::Head { group, count, folded } => {
                let arrow = match (folded, a) {
                    (true, false) => "▸",
                    (false, false) => "▾",
                    (true, true) => ">",
                    (false, true) => "v",
                };
                let needs = *group == Group::Needs;
                let blink = fx && needs && within(self.need_blink, now, NEED_MS);
                let color = if needs { theme.warn2 } else { theme.accent };
                let mut st = theme.bold(color);
                if blink && px::hump_ms(self.need_blink.map_or(0, |t| now.duration_since(t).as_millis() as u64), NEED_MS) > 0.5
                {
                    st = st.add_modifier(Modifier::REVERSED);
                }
                let mark = if needs { "!" } else { arrow };
                spans.push(Span::styled(format!("{mark} {}", group.title()), st));
                let extra = match group {
                    Group::Lanes => self.lanes_summary(),
                    Group::Needs => format!("{count}"),
                    _ if *folded => format!("· {count}"),
                    _ => String::new(),
                };
                // a folded group reads as one phrase (`CLAUDE · 2`); a summary stands apart from the title
                let gap = if extra.starts_with('·') { " " } else { "  " };
                if !extra.is_empty() {
                    spans.push(Span::styled(format!("{gap}{extra}"), theme.dim()));
                }
            }
            Item::Row { row, kind, prefix, kids, folded, group } => {
                let (g, gc, bold) = self.glyph(row, theme, now, fx);
                let lead = lead_of(prefix);
                let fold = if *kids > 0 && *folded { format!(" +{kids}") } else { String::new() };
                let name_room = name_w.saturating_sub(width(&lead)).max(4);
                let name = truncate(&row.name, name_room);
                let pad = name_room.saturating_sub(width(&name));
                spans.push(Span::styled(lead, theme.dim2()));
                spans.push(Span::styled(g.to_string(), if bold { theme.bold(gc) } else { theme.style(gc) }));
                spans.push(Span::raw(" "));
                let name_style = if *group == Group::Needs || matches!(kind, Kind::This | Kind::Lane) {
                    theme.bold(theme.fg)
                } else {
                    theme.style(theme.fg)
                };
                spans.push(Span::styled(name, name_style));
                spans.push(Span::styled(fold, theme.dim2()));
                spans.push(Span::raw(" ".repeat(pad)));
                let used: usize = spans.iter().map(|s| width(&s.content)).sum();
                let (mut fields, step) = self.fields(row, *kind, theme, now, fx);
                let fw = |f: &[Field]| f.iter().map(|(t, _)| width(t) + 2).sum::<usize>();
                while !fields.is_empty() && used + fw(&fields) > w {
                    fields.pop();
                }
                let room = w.saturating_sub(used + fw(&fields));
                let step = step.filter(|_| room >= 8).map(|s| fit_step(&s, room - 2)).unwrap_or_default();
                if !step.is_empty() {
                    spans.push(Span::styled(format!("  {step}"), theme.dim()));
                }
                let used = used + if step.is_empty() { 0 } else { width(&step) + 2 };
                spans.push(Span::raw(" ".repeat(w.saturating_sub(used + fw(&fields)))));
                for (t, c) in fields {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(t, theme.style(c)));
                }
            }
        }
        let mut line = Line::from(spans);
        if selected {
            line = line.style(theme.selection());
        }
        line
    }

    /// `2 on · $7.97 of $9`: lanes with a live runner, and today's spend against the summed caps.
    pub(super) fn lanes_summary(&self) -> String {
        let lanes: Vec<&LaneInfo> = self.rows.iter().filter_map(|r| r.lane.as_ref()).collect();
        if lanes.is_empty() {
            return if self.lanes_first { "no lanes yet: a adds one".into() } else { String::new() };
        }
        let on = lanes.iter().filter(|l| l.runner).count();
        let spend: f64 = lanes.iter().map(|l| l.spend_today).sum();
        let cap: f64 = lanes.iter().filter_map(|l| l.cap_day).sum();
        let paused = if self.lanes_paused { " · paused" } else { "" };
        if cap > 0.0 {
            format!("{on} on · ${spend:.2} of ${}{paused}", money(cap))
        } else {
            format!("{on} on · ${spend:.2}{paused}")
        }
    }
}

/// `$9` for whole dollars, `$9.50` otherwise.
pub(super) fn money(v: f64) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

/// A path under the home folder as `~/…`.
pub(super) fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// The tree lines before a nested row's glyph with one space after them (`╰ `, `+- `), so the name column is
/// counted the way it is drawn.
fn lead_of(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{} ", prefix.trim_end())
    }
}
