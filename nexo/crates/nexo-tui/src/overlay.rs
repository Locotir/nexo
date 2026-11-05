//! The window of `/context`, `/usage` and the session: a small window over
//! the transcript, with tabs, a close button and figures that fill in.

use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::render::blocks::fmt_k;
use crate::render::wrap::truncate;
use crate::theme::{mix, Theme};

pub const TABS: [&str; 3] = ["Context", "Usage", "Session"];
/// The window grows open in this long.
pub const OPEN_MS: u64 = 220;
/// The figures of a tab fill in in this long.
pub const FILL_MS: u64 = 650;
/// Cells of the context grid.
pub const GRID_CELLS: usize = 100;

/// The open window: which tab, where it was drawn (for the mouse).
#[derive(Debug, Clone)]
pub struct Info {
    pub tab: usize,
    pub scroll: usize,
    pub opened: Instant,
    /// When the tab on show came up: its figures fill in from here.
    pub shown: Instant,
    pub window: Rect,
    pub close: Rect,
    pub tabs: Vec<Rect>,
    /// The pointer is on the close button.
    pub hover: bool,
    /// Drawn whole at once, with no opening or filling in (`[ui] animations = false`).
    pub still: bool,
}

impl Info {
    pub fn new(tab: usize) -> Info {
        let now = Instant::now();
        Info {
            tab: tab.min(TABS.len() - 1),
            scroll: 0,
            opened: now,
            shown: now,
            window: Rect::default(),
            close: Rect::default(),
            tabs: Vec::new(),
            hover: false,
            still: false,
        }
    }

    /// This window, with no motion when `still`.
    pub fn still(mut self, still: bool) -> Info {
        self.still = still;
        self
    }

    pub fn set_tab(&mut self, tab: usize) {
        if tab != self.tab {
            self.tab = tab % TABS.len();
            self.scroll = 0;
            self.shown = Instant::now();
        }
    }

    pub fn next(&mut self, step: isize) {
        let n = TABS.len() as isize;
        self.set_tab(((self.tab as isize + step).rem_euclid(n)) as usize);
    }

    /// 0..1: how far the window has grown open.
    pub fn grow(&self) -> f64 {
        if self.still {
            return 1.0;
        }
        ease(self.opened.elapsed().as_millis() as f64 / OPEN_MS as f64)
    }

    /// 0..1: how far the tab's figures have filled in.
    pub fn fill(&self) -> f64 {
        if self.still {
            return 1.0;
        }
        ease(self.shown.elapsed().as_millis() as f64 / FILL_MS as f64)
    }

    /// Still moving: every frame is drawn.
    pub fn animating(&self) -> bool {
        !self.still
            && (self.opened.elapsed().as_millis() as u64 <= OPEN_MS + 50
                || self.shown.elapsed().as_millis() as u64 <= FILL_MS + 50)
    }
}

/// Ease-out cubic on a clamped 0..1.
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The window's rectangle in `area` once fully open, `body` lines tall.
pub fn window_rect(area: Rect, body: usize) -> Rect {
    let w = ((area.width as u32 * 65 / 100) as u16).clamp(44, 100).min(area.width.saturating_sub(2));
    // border, tabs, rule, body, footer, border
    // a short area is used whole: the window needs its six rows
    let margin = if area.height < 10 { 0 } else { 2 };
    let h = (body as u16 + 5).clamp(10, 30).min(area.height.saturating_sub(margin));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// A bar of `cells` cells at `share`, in eighths: (filled, track).
pub fn bar(share: f64, cells: usize) -> (String, String) {
    let (fill, rest) = crate::ui::gauge_cells(share, cells);
    (fill, "░".repeat(rest))
}

/// The lines of tab `tab` at `w` columns; `t` (0..1) is how far its figures have filled in.
pub fn lines(app: &App, tab: usize, w: usize, t: f64) -> Vec<Line<'static>> {
    match tab {
        0 => context_lines(app, w, t),
        1 => usage_lines(app, w, t),
        _ => session_lines(app, w),
    }
}

/// The parts of what the next call sends, as (glyph, label, tokens, colour).
pub fn context_parts(app: &App, theme: &Theme) -> Vec<(&'static str, &'static str, u64, Color)> {
    let (num, _, _) = app.gauge_view();
    let mut by_kind = [0u64; 4];
    for c in app.map.iter().filter(|c| c.state != nexo_core::agent::CellState::Dropped) {
        by_kind[crate::ui::kind_ix(c.kind)] += c.tokens;
    }
    let total: u64 = by_kind.iter().sum();
    let mut parts = Vec::new();
    let mut given = 0;
    if total > 0 && num > 0 {
        // the colours of the map: four kinds, four colours far apart
        let k = theme.kind_colours();
        let names = [
            ("◆", "your messages", k[0]),
            ("◆", "replies", k[1]),
            ("◈", "tool calls", k[2]),
            ("◈", "tool output", k[3]),
        ];
        for (i, (g, l, c)) in names.into_iter().enumerate() {
            let n = (by_kind[i] as f64 / total as f64 * num as f64).round() as u64;
            if n > 0 {
                given += n;
                parts.push((g, l, n, c));
            }
        }
    }
    if num > given {
        parts.insert(0, ("◆", if parts.is_empty() { "in use" } else { "system & tools" }, num - given, theme.dim));
    }
    parts
}

fn context_lines(app: &App, w: usize, t: f64) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let (num, den, guessed) = app.gauge_view();
    let share = num as f64 / den.max(1) as f64;
    let parts = context_parts(app, theme);
    let mut out = Vec::new();
    // the grid: one cell per hundredth of the window, glyphs apart
    let cols = if w >= 41 { 20 } else { 10 };
    let mut cells: Vec<(&str, Color)> = Vec::with_capacity(GRID_CELLS);
    for (g, _, n, c) in &parts {
        let k = ((*n as f64 / den.max(1) as f64) * GRID_CELLS as f64).round() as usize;
        let k = if *n > 0 { k.max(1) } else { 0 };
        for _ in 0..k {
            if cells.len() < GRID_CELLS {
                cells.push((g, *c));
            }
        }
    }
    let lit = (t * GRID_CELLS as f64).ceil() as usize;
    let mut grid = Vec::new();
    for row in 0..GRID_CELLS / cols {
        let mut spans = Vec::new();
        for col in 0..cols {
            let i = row * cols + col;
            let (g, style) = match cells.get(i) {
                // a cell comes in bright and settles to its colour
                Some((g, c)) if i < lit => {
                    let age = (lit - i) as f32 / 12.0;
                    (*g, Style::default().fg(mix(theme.fg, *c, age)))
                }
                _ => ("◇", theme.dim2()),
            };
            spans.push(Span::styled(g.to_string(), style));
            if col + 1 < cols {
                spans.push(Span::raw(" "));
            }
        }
        grid.push(Line::from(spans));
    }
    let pct = (share * 100.0 * t).round() as u64;
    let shown = (num as f64 * t) as u64;
    let level = theme.level_color((share * 100.0) as u64);
    let mut side = vec![
        Line::from(vec![
            Span::styled(format!("{}{pct}%", if guessed { "~" } else { "" }), theme.bold(level)),
            Span::styled(format!("  {} / {} tokens", fmt_k(shown), fmt_k(den)), theme.dim()),
        ]),
        Line::default(),
    ];
    let label_w = parts.iter().map(|p| p.1.len()).max().unwrap_or(0).max(4);
    for (g, l, n, c) in &parts {
        side.push(Line::from(vec![
            Span::styled(format!("{g} "), Style::default().fg(*c)),
            Span::raw(format!("{l:<label_w$}  ")),
            Span::styled(format!("{:>6}", fmt_k(*n)), theme.dim()),
        ]));
    }
    side.push(Line::from(vec![
        Span::styled("◇ ", theme.dim2()),
        Span::raw(format!("{:<label_w$}  ", "free")),
        Span::styled(format!("{:>6}", fmt_k(den.saturating_sub(num))), theme.dim()),
    ]));
    if guessed {
        side.push(Line::from(Span::styled("window not known yet", theme.dim2())));
    }
    let grid_w = cols * 2 - 1;
    let side_w = side.iter().map(Line::width).max().unwrap_or(0);
    if w >= grid_w + 4 + side_w {
        // wide: the legend beside the grid
        let n = grid.len().max(side.len());
        for i in 0..n {
            let mut l = grid.get(i).cloned().unwrap_or_else(|| Line::from(" ".repeat(grid_w)));
            if let Some(sl) = side.get(i) {
                l.spans.push(Span::raw("    "));
                l.spans.extend(sl.spans.iter().cloned());
            }
            out.push(l);
        }
    } else {
        out.extend(grid);
        out.push(Line::default());
        out.extend(side);
    }
    if let Some(c) = &app.context {
        out.push(Line::default());
        out.push(Line::from(vec![
            Span::styled("memory  ", theme.dim()),
            Span::raw(format!("{} tok · {} messages", fmt_k(c.history_tokens), c.messages_total)),
        ]));
        out.push(Line::from(vec![
            Span::styled("sent    ", theme.dim()),
            Span::raw(format!("{} tok · {} messages", fmt_k(c.view_tokens), c.messages_in_view)),
        ]));
        let name = if c.label.is_empty() { c.policy.clone() } else { c.label.clone() };
        let mut policy =
            vec![Span::styled("policy  ", theme.dim()), Span::styled(name.clone(), theme.style(theme.accent))];
        if let Some(n) = &c.note {
            policy.push(Span::styled(format!(" · {}", truncate(n, w.saturating_sub(name.len() + 12))), theme.dim()));
        }
        out.push(Line::from(policy));
        // why the policy does what it does: idle, off, or how much it saves
        if let Some(d) = &c.detail {
            out.extend(crate::render::wrap::wrap_plain(d, w.max(20), theme.dim(), &[Span::raw("        ")]));
        }
    }
    out
}

/// Eight levels of a sparkline.
const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A sparkline of `values`, the last `w` of them, grown to `t` of their height.
pub fn sparkline(values: &[u64], w: usize, t: f64) -> String {
    let tail = &values[values.len().saturating_sub(w)..];
    let max = tail.iter().copied().max().unwrap_or(0).max(1) as f64;
    tail.iter()
        .map(|&v| {
            let h = (v as f64 / max * t * 7.0).round() as usize;
            SPARK[h.min(7)]
        })
        .collect()
}

fn usage_lines(app: &App, w: usize, t: f64) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let u = app.session_usage;
    let mut out = Vec::new();
    out.push(Line::from(vec![
        Span::styled(format!("${:.4}", app.cost * t), theme.bold(theme.success)),
        Span::styled("  this session".to_string(), theme.dim()),
    ]));
    out.push(Line::default());
    let input = u.total_input();
    let rows = [
        ("sent", input, theme.accent),
        ("  from cache", u.cache_read, theme.info),
        ("written", u.output, theme.accent2),
        ("  reasoning", u.reasoning, theme.dim),
    ];
    let max = rows.iter().map(|r| r.1).max().unwrap_or(0).max(1) as f64;
    let cells = w.saturating_sub(24).clamp(8, 30);
    for (label, n, c) in rows {
        let (fill, rest) = bar(n as f64 / max * t, cells);
        out.push(Line::from(vec![
            Span::raw(format!("{label:<13}")),
            Span::styled(format!("{:>7}  ", fmt_k(n)), theme.dim()),
            Span::styled(fill, Style::default().fg(c)),
            Span::styled(rest, theme.dim2()),
        ]));
    }
    if let Some(hit) = (u.cache_read * 100).checked_div(input) {
        out.push(Line::default());
        out.push(Line::from(vec![
            Span::styled("cache hit  ", theme.dim()),
            Span::styled(format!("{hit}%"), theme.bold(theme.level_color(100 - hit.min(100)))),
        ]));
    }
    if !app.call_inputs.is_empty() {
        out.push(Line::default());
        let spark_w = w.saturating_sub(2).min(60);
        out.push(Line::from(Span::styled(
            format!(
                "sent per call · {} call{}",
                app.call_inputs.len(),
                if app.call_inputs.len() == 1 { "" } else { "s" }
            ),
            theme.dim(),
        )));
        out.push(Line::from(Span::styled(sparkline(&app.call_inputs, spark_w, t), theme.style(theme.accent))));
    }
    if let Some((s, d, sl, dl)) = app.budget {
        out.push(Line::default());
        for (label, spent, limit) in [("session", s, sl), ("today", d, dl)] {
            let mut spans = vec![
                Span::raw(format!("{label:<9}")),
                Span::styled(format!("${spent:.4}"), theme.style(theme.success)),
            ];
            if let Some(l) = limit.filter(|l| *l > 0.0) {
                let share = (spent / l).min(1.0);
                let (fill, rest) = bar(share * t, 12);
                let c = theme.level_color((share * 100.0) as u64);
                spans.push(Span::styled(format!(" / ${l:.2}  "), theme.dim()));
                spans.push(Span::styled(fill, Style::default().fg(c)));
                spans.push(Span::styled(rest, theme.dim2()));
                spans.push(Span::styled(format!(" {}%", (share * 100.0).round()), theme.dim()));
            }
            out.push(Line::from(spans));
        }
    }
    out
}

fn session_lines(app: &App, w: usize) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut rows: Vec<(&str, String)> = vec![
        ("model", app.model.clone()),
        ("provider", app.provider.clone()),
        ("effort", app.effort.clone().unwrap_or_else(|| "default".into())),
        ("session", app.session_id.clone().unwrap_or_else(|| "new".into())),
    ];
    if !app.session_title.is_empty() {
        rows.push(("title", app.session_title.clone()));
    }
    rows.push(("folder", app.cwd.display().to_string()));
    if let Some(b) = &app.branch {
        rows.push(("branch", b.clone()));
    }
    if let Some(l) = &app.limits {
        if let Some(cw) = l.context_window {
            rows.push(("window", format!("{} tokens", fmt_k(cw as u64))));
        }
        if let Some(mo) = l.max_output {
            rows.push(("max output", format!("{} tokens", fmt_k(mo as u64))));
        }
        if let Some(p) = &l.pricing {
            rows.push(("price / 1M", format!("${} in · ${} out · ${} cached", p.input, p.output, p.cache_read)));
        }
        if !l.source.is_empty() {
            rows.push(("known from", l.source.clone()));
        }
    }
    rows.into_iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<11}"), theme.dim()),
                Span::raw(truncate(&v, w.saturating_sub(12))),
            ])
        })
        .collect()
}

/// The tab row: (spans, the columns each tab takes from the row's start).
pub fn tab_row(tab: usize, theme: &Theme) -> (Vec<Span<'static>>, Vec<(u16, u16)>) {
    let mut spans = Vec::new();
    let mut at = Vec::new();
    let mut x = 0u16;
    for (i, name) in TABS.iter().enumerate() {
        let label = format!(" {} {name} ", i + 1);
        let wl = label.chars().count() as u16;
        let style = if i == tab { theme.bold(theme.accent).add_modifier(Modifier::REVERSED) } else { theme.dim() };
        at.push((x, wl));
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
        x += wl + 1;
    }
    (spans, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_scales_to_the_largest() {
        assert_eq!(sparkline(&[0, 50, 100], 10, 1.0), "▁▅█");
        assert_eq!(sparkline(&[1, 2, 3, 4], 2, 1.0), "▆█", "only the last w values");
        assert_eq!(sparkline(&[100], 5, 0.0), "▁", "grows from the floor");
    }

    #[test]
    fn the_window_keeps_to_its_bounds() {
        let r = window_rect(Rect::new(0, 0, 200, 60), 100);
        assert_eq!((r.width, r.height), (100, 30));
        let r = window_rect(Rect::new(0, 0, 50, 20), 3);
        assert_eq!((r.width, r.height), (44, 10));
        assert_eq!(r.x, 3);
    }

    #[test]
    fn tabs_wrap_around() {
        let mut i = Info::new(0);
        i.next(-1);
        assert_eq!(i.tab, 2);
        i.next(1);
        assert_eq!(i.tab, 0);
        assert_eq!(Info::new(9).tab, 2);
    }
}
