//! `/panel`: one card per agent session (this one, sub-agents, and other tools' sessions read from their
//! logs), each with its name, source, a coloured state chip, the last step, how long it has been quiet and
//! the supervisor's last action. Read-only: it shows, it sends nothing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::supervise::run::now_secs;
use nexo_agent::supervise::{AgentRow, Panel, Secs, SourceKind, State};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block as Box_, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::anim::{ease_out, progress, pulse, PULSE_MS};
use crate::modal::{contains, ModalOut};
use crate::motion::{fade, fixed, hump, lit_style, within, Glide};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

const CARD_MIN_W: u16 = 34;
const CARD_MAX_W: u16 = 60;
const CARD_H: u16 = 5;
const GAP: u16 = 2;
const MAX_COLS: u16 = 4;
/// The width under which the cards give way to one line per session.
const LIST_BELOW: u16 = 44;
/// One breath of an attention chip.
const BREATH_MS: u128 = 1600;
/// A card fades (and rises one row) in this long when it first shows; the first cards of a new view start
/// one after the other, `STAGGER_MS` apart, up to `STAGGER_MAX`.
const ENTER_MS: u64 = 280;
const STAGGER_MS: u64 = 45;
const STAGGER_MAX: u64 = 270;
/// A chip changing state cross-fades from the old colour to the new one in this long.
const CHIP_MS: u64 = 400;
/// A far-zoom block: wide, tall, and the most of them that a row can hold.
const BLOCK_W: u16 = 16;
const BLOCK_H: u16 = 3;
const MAX_BLOCK_COLS: u16 = 12;

/// The cards or the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Cards {
        cols: u16,
        card_w: u16,
    },
    List,
    /// The far zoom: a colour block per session.
    Blocks {
        cols: u16,
    },
}

/// How far in the view looks. `Cards` is the normal one (it still falls back to lines when the window is
/// narrow); `-` zooms out to lines and then to blocks, `+` back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Blocks,
    List,
    Cards,
}

/// The blocks of the far zoom in a body `w` wide: as many columns as fit.
pub fn blocks_layout(w: u16) -> Layout {
    Layout::Blocks { cols: ((w + GAP) / (BLOCK_W + GAP)).clamp(1, MAX_BLOCK_COLS) }
}

/// How `n` sessions lay out in a body `w` wide and `h` tall. Pure, so the sizes are testable.
pub fn cards_layout(w: u16, h: u16) -> Layout {
    if w < LIST_BELOW || h < CARD_H {
        return Layout::List;
    }
    let cols = ((w + GAP) / (CARD_MIN_W + GAP)).clamp(1, MAX_COLS);
    let card_w = ((w - GAP * (cols - 1)) / cols).min(CARD_MAX_W);
    Layout::Cards { cols, card_w }
}

/// The glyph, the label and the order of a state in the header.
fn chip(state: State) -> (&'static str, &'static str) {
    match state {
        State::WaitingPermission => ("◈", "permission"),
        State::WaitingAnswer => ("◆", "question"),
        State::Stalled => ("◐", "stalled"),
        State::Looping => ("↻", "looping"),
        State::Working => ("●", "working"),
        State::Idle { .. } => ("○", "idle"),
        State::Done => ("✓", "done"),
    }
}

fn attention(state: State) -> bool {
    matches!(state, State::WaitingPermission | State::WaitingAnswer | State::Stalled)
}

fn chip_color(theme: &Theme, state: State) -> ratatui::style::Color {
    match state {
        State::WaitingPermission => theme.warn2,
        State::WaitingAnswer => theme.accent2,
        State::Stalled => theme.warn,
        State::Looping => theme.error,
        State::Working | State::Done => theme.success,
        State::Idle { .. } => theme.dim,
    }
}

/// `42s`, `7m 05s`, `2h 05m`, `3d 04h`.
fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..=86399 => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {:02}h", secs / 86400, secs % 86400 / 3600),
    }
}

/// Where a session's idle time is rounded to, so the view repaints only when its text changes: per second
/// while a session is fresh, per minute once it is not.
fn bucket(secs: u64) -> u64 {
    if secs < 120 {
        secs
    } else {
        secs / 60 * 60
    }
}

/// The motion of one session: when its card first showed, and its last change of state.
#[derive(Debug, Clone)]
struct RowFx {
    born: Instant,
    delay_ms: u64,
    state: State,
    prev: State,
    changed: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct AgentsView {
    pub selected: usize,
    panel: Option<Arc<Panel>>,
    seen: u64,
    rows: Vec<AgentRow>,
    /// The clock the age of an action is told against (seconds).
    clock: Secs,
    pub window: Rect,
    pub close: Rect,
    pub hover: bool,
    /// Where each card (or line) was drawn last, for the pointer.
    cards: Vec<(usize, Rect)>,
    cols: usize,
    /// The first grid row in sight.
    pub scroll: usize,
    glide: Glide,
    /// When the view was first drawn (the phase of the breathing chips).
    opened: Option<Instant>,
    fx: HashMap<String, RowFx>,
    /// A nudge was sent to this session: its card pulses once at the next draw.
    pulse_key: Option<String>,
    pulse_at: Option<(String, Instant)>,
    pub zoom: Zoom,
    /// No motion (`[ui] animations = false`, or a mono theme).
    still: bool,
    /// The nudge being written for the selected session.
    edit: Option<String>,
    /// One line of news from the last action.
    note: Option<String>,
    /// The selected session's details are open.
    open: bool,
    /// The policy page is showing instead of the sessions.
    policy_page: bool,
    policy_scroll: usize,
}

impl AgentsView {
    pub fn new(panel: Option<Arc<Panel>>, still: bool) -> AgentsView {
        let mut v = AgentsView {
            selected: 0,
            panel,
            seen: u64::MAX,
            rows: Vec::new(),
            clock: now_secs(),
            window: Rect::default(),
            close: Rect::default(),
            hover: false,
            cards: Vec::new(),
            cols: 1,
            scroll: 0,
            glide: Glide::default(),
            opened: None,
            fx: HashMap::new(),
            pulse_key: None,
            pulse_at: None,
            zoom: Zoom::Cards,
            still,
            edit: None,
            note: None,
            open: false,
            policy_page: false,
            policy_scroll: 0,
        };
        v.refresh();
        v
    }

    /// A view of fixed rows, no panel behind it (tests).
    pub fn with_rows(rows: Vec<AgentRow>, clock: Secs, still: bool) -> AgentsView {
        let mut v = AgentsView::new(None, still);
        v.rows = rows;
        v.clock = clock;
        v
    }

    /// Replaces the rows as a poll would (tests drive the motion with it).
    pub fn set_rows(&mut self, rows: Vec<AgentRow>) {
        self.rows = rows;
    }

    pub fn rows(&self) -> &[AgentRow] {
        &self.rows
    }

    /// Takes the panel's newest rows. True when something the view shows changed (the caller repaints).
    pub fn refresh(&mut self) -> bool {
        let Some(panel) = &self.panel else { return false };
        let now = now_secs();
        let v = panel.version();
        let mut rows = panel.rows(now);
        for r in &mut rows {
            r.idle_secs = r.idle_secs.map(bucket);
        }
        let clock = if rows.iter().any(|r| r.idle_secs.is_some_and(|s| s < 120)) { now } else { now / 60 * 60 };
        let changed = rows != self.rows || clock != self.clock;
        self.seen = v;
        self.rows = rows;
        self.clock = clock;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        changed
    }

    /// Whether something moves on its own (a breathing chip, an entering card, a chip changing colour, the
    /// selection gliding, a pulse): the loop keeps drawing. At rest it is false, so nothing repaints.
    pub fn animating(&self, now: Instant) -> bool {
        if self.still {
            return false;
        }
        self.rows.iter().any(|r| attention(r.state))
            || self.glide.moving(now)
            || self
                .fx
                .values()
                .any(|x| within(Some(x.born), now, x.delay_ms + ENTER_MS) || within(x.changed, now, CHIP_MS))
            || self.pulse_key.is_some()
            || self.pulse_at.as_ref().is_some_and(|(_, t)| within(Some(*t), now, PULSE_MS))
    }

    /// Learns what changed since the last draw: new sessions (their card enters), new states (the chip
    /// cross-fades), a nudge just sent (its card pulses). Without motion nothing is kept.
    fn note(&mut self, now: Instant, fx: bool) {
        if self.opened.is_none() {
            self.opened = Some(now);
        }
        if !fx {
            self.fx.clear();
            self.pulse_key = None;
            self.pulse_at = None;
            return;
        }
        let first = self.fx.is_empty();
        self.fx.retain(|k, _| self.rows.iter().any(|r| &r.key == k));
        for (i, r) in self.rows.iter().enumerate() {
            match self.fx.get_mut(&r.key) {
                Some(x) => {
                    if chip(x.state).1 != chip(r.state).1 {
                        x.prev = x.state;
                        x.changed = Some(now);
                    }
                    x.state = r.state;
                }
                None => {
                    let delay_ms = if first { (i as u64 * STAGGER_MS).min(STAGGER_MAX) } else { 0 };
                    self.fx.insert(
                        r.key.clone(),
                        RowFx { born: now, delay_ms, state: r.state, prev: r.state, changed: None },
                    );
                }
            }
        }
        if let Some(k) = self.pulse_key.take() {
            self.pulse_at = Some((k, now));
        }
    }

    /// 0..1: how far a session's card has entered (1 with no motion).
    fn entered(&self, key: &str, now: Instant) -> f32 {
        match self.fx.get(key) {
            Some(x) => {
                let start = x.born + std::time::Duration::from_millis(x.delay_ms);
                if now < start {
                    0.0
                } else {
                    ease_out(progress(Some(start), now, ENTER_MS)) as f32
                }
            }
            None => 1.0,
        }
    }

    /// The state to draw for a session and the colour of its chip: while the state is changing, the old
    /// one's word fades into the new one's.
    fn chip_at(&self, row: &AgentRow, theme: &Theme, now: Instant) -> (State, ratatui::style::Color) {
        let new = chip_color(theme, row.state);
        match self.fx.get(&row.key) {
            Some(x) if within(x.changed, now, CHIP_MS) => {
                let t = ease_out(progress(x.changed, now, CHIP_MS)) as f32;
                let shown = if t < 0.5 { x.prev } else { row.state };
                (shown, fade(theme, chip_color(theme, x.prev), new, t))
            }
            _ => (row.state, new),
        }
    }

    /// Dims everything drawn in `r` towards the window ground by `1 - t` (a card coming in).
    fn veil(f: &mut Frame, r: Rect, theme: &Theme, t: f32) {
        if t >= 1.0 || theme.mono {
            return;
        }
        let bg = theme.modal_bg;
        let buf = f.buffer_mut();
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                let Some(c) = buf.cell_mut((x, y)) else { continue };
                let fg = if c.fg == ratatui::style::Color::Reset { theme.fg } else { c.fg };
                c.set_fg(fade(theme, bg, fg, t));
                if c.bg != ratatui::style::Color::Reset {
                    c.set_bg(fade(theme, bg, c.bg, t));
                }
            }
        }
    }

    // ---------------------------------------------------------------- input

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        if self.edit.is_some() {
            return self.edit_key(key);
        }
        self.note = None;
        if self.policy_page {
            return self.policy_key(key);
        }
        let n = self.rows.len();
        let cols = self.cols.max(1);
        match key.code {
            KeyCode::Esc if self.open => self.open = false,
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Enter => self.open = !self.open && !self.rows.is_empty(),
            KeyCode::Char('o') if self.panel.is_some() => {
                self.policy_page = true;
                self.policy_scroll = 0;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.zoom = match self.zoom {
                    Zoom::Blocks => Zoom::List,
                    _ => Zoom::Cards,
                }
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.zoom = match self.zoom {
                    Zoom::Cards => Zoom::List,
                    _ => Zoom::Blocks,
                }
            }
            KeyCode::Char('n') => self.start_nudge(),
            KeyCode::Char('p') => self.toggle_pause(false),
            KeyCode::Char('P') => self.toggle_pause(true),
            KeyCode::Left | KeyCode::Char('h') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                self.selected = (self.selected + 1).min(n.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(cols),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + cols).min(n.saturating_sub(1)),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = n.saturating_sub(1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(cols * 3),
            KeyCode::PageDown => self.selected = (self.selected + cols * 3).min(n.saturating_sub(1)),
            _ => {}
        }
        ModalOut::Nothing
    }

    fn policy_key(&mut self, key: KeyEvent) -> ModalOut {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('o') => self.policy_page = false,
            KeyCode::Up | KeyCode::Char('k') => self.policy_scroll = self.policy_scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.policy_scroll += 1,
            KeyCode::PageUp => self.policy_scroll = self.policy_scroll.saturating_sub(10),
            KeyCode::PageDown => self.policy_scroll += 10,
            KeyCode::Home => self.policy_scroll = 0,
            _ => {}
        }
        ModalOut::Nothing
    }

    /// The policy page: what is in force, the model's spend of this hour, and why anything was refused.
    fn draw_policy(&mut self, f: &mut Frame, body: Rect, theme: &Theme) {
        let lines = self.panel.as_ref().map(|p| p.policy_lines()).unwrap_or_default();
        let max = lines.len().saturating_sub(body.height as usize);
        self.policy_scroll = self.policy_scroll.min(max);
        let shown: Vec<Line> = lines
            .iter()
            .skip(self.policy_scroll)
            .take(body.height as usize)
            .map(|l| {
                let text = truncate(l, body.width as usize);
                let style = if l.starts_with('!') {
                    theme.style(theme.warn)
                } else if l.starts_with('[') {
                    theme.bold(theme.accent)
                } else if l.starts_with("model") {
                    theme.style(theme.accent2)
                } else {
                    theme.dim()
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        f.render_widget(Paragraph::new(Text::from(shown)), body);
    }

    fn start_nudge(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        let Some(panel) = &self.panel else { return };
        if row.parent.is_some() {
            self.note = Some("a sub-agent takes no message; write to its session".into());
            return;
        }
        self.edit = Some(panel.template(&row.key));
    }

    fn edit_key(&mut self, key: KeyEvent) -> ModalOut {
        let Some(text) = self.edit.as_mut() else { return ModalOut::Nothing };
        match key.code {
            KeyCode::Esc => self.edit = None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
            KeyCode::Enter => {
                let text = self.edit.take().unwrap_or_default();
                if let (Some(row), Some(panel)) = (self.rows.get(self.selected), &self.panel) {
                    if !text.trim().is_empty() {
                        panel.nudge(&row.key, &text);
                        self.pulse_key = Some(row.key.clone());
                        self.note = Some(if row.source == SourceKind::Nexo {
                            format!("message sent to {}", row.name)
                        } else {
                            format!("nudge queued for {}; it goes out when the session stops", row.name)
                        });
                    }
                }
                self.refresh();
            }
            _ => {}
        }
        ModalOut::Nothing
    }

    fn toggle_pause(&mut self, all: bool) {
        let Some(panel) = self.panel.clone() else { return };
        if all {
            let on = !panel.paused_all();
            panel.pause(None, on);
            self.note = Some(if on { "all sessions paused" } else { "all sessions resumed" }.into());
        } else if let Some(row) = self.rows.get(self.selected) {
            panel.pause(Some(&row.key), !row.paused);
            self.note = Some(format!("{} {}", row.name, if row.paused { "resumed" } else { "paused" }));
        }
        self.refresh();
    }

    pub fn click(&mut self, x: u16, y: u16) -> ModalOut {
        if contains(self.close, x, y) || !contains(self.window, x, y) {
            return ModalOut::Close;
        }
        if let Some((i, _)) = self.cards.iter().find(|(_, r)| contains(*r, x, y)) {
            self.selected = *i;
        }
        ModalOut::Nothing
    }

    pub fn on_hover(&mut self, x: u16, y: u16) -> bool {
        let on = contains(self.close, x, y);
        std::mem::replace(&mut self.hover, on) != on
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }

    // ---------------------------------------------------------------- drawing

    pub fn draw(&mut self, f: &mut Frame, area: Rect, theme: &Theme, now: Instant, fx: bool) {
        if area.width < 30 || area.height < 6 {
            let msg = if area.width < 30 { "make the window wider" } else { "make the window taller" };
            let msg = truncate(msg, area.width as usize);
            let y = area.y + area.height / 2;
            let line =
                Line::from(Span::styled(msg, theme.style(theme.warn))).alignment(ratatui::layout::Alignment::Center);
            f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
            self.cards.clear();
            return;
        }
        let widest = if self.zoom == Zoom::Blocks {
            MAX_BLOCK_COLS * BLOCK_W + (MAX_BLOCK_COLS - 1) * GAP
        } else {
            MAX_COLS * CARD_MAX_W + (MAX_COLS - 1) * GAP
        };
        let inner_w = area.width.min(widest + 4).saturating_sub(4);
        let zoom = self.zoom;
        let layout_for = |h: u16| match zoom {
            Zoom::Blocks => blocks_layout(inner_w),
            Zoom::List => Layout::List,
            Zoom::Cards => cards_layout(inner_w, h),
        };
        let line_per_row = |l: Layout| match l {
            Layout::Cards { cols, .. } => {
                (self.rows.len() as u16).div_ceil(cols) * CARD_H
                    + (self.rows.len() as u16).div_ceil(cols).saturating_sub(1)
            }
            Layout::List => self.rows.len() as u16,
            Layout::Blocks { cols } => {
                let n = (self.rows.len() as u16).div_ceil(cols);
                n * BLOCK_H + n.saturating_sub(1)
            }
        };
        let chrome = 2 + 2 + 1 + 1; // borders, header and rule, detail, footer
        let want = if self.policy_page { area.height } else { line_per_row(layout_for(area.height)) + chrome };
        let h = want.clamp(8.min(area.height), area.height);
        let w = inner_w + 4;
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        f.render_widget(Clear, r);
        let block = Box_::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.style(theme.border))
            .title(Span::styled(
                if self.policy_page { " panel · policy " } else { " panel " },
                theme.bold(theme.accent),
            ))
            .style(Style::default().bg(theme.modal_bg));
        let inner = block.inner(r);
        f.render_widget(block, r);
        self.window = r;
        self.close = Rect::new(r.x + r.width.saturating_sub(6), r.y, 3, 1);
        let close_style =
            if self.hover { theme.bold(theme.error).add_modifier(Modifier::REVERSED) } else { theme.dim() };
        f.render_widget(Paragraph::new(Span::styled("[✗]", close_style)), self.close);

        let pad = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), inner.height);
        // header: how many sessions are in which state, and what this view is
        f.render_widget(Paragraph::new(Line::from(self.header(theme, pad.width as usize))), Rect { height: 1, ..pad });
        if pad.height > 1 {
            f.render_widget(
                Paragraph::new(Span::styled("─".repeat(pad.width as usize), theme.dim2())),
                Rect { y: pad.y + 1, height: 1, ..pad },
            );
        }
        let foot = Rect::new(pad.x, pad.y + pad.height.saturating_sub(1), pad.width, 1);
        let detail = Rect::new(pad.x, foot.y.saturating_sub(1), pad.width, 1);
        let body_top = pad.y + 2;
        let body = Rect::new(pad.x, body_top, pad.width, detail.y.saturating_sub(body_top));
        self.cards.clear();
        if self.policy_page {
            self.draw_policy(f, body, theme);
        } else if self.rows.is_empty() {
            self.empty(f, body, theme);
        } else {
            self.glide.note(Some(self.selected), now, fx);
            self.note(now, fx);
            let layout = match self.zoom {
                Zoom::Blocks if body.height >= BLOCK_H => blocks_layout(body.width),
                Zoom::Blocks | Zoom::List => Layout::List,
                Zoom::Cards => cards_layout(body.width, body.height),
            };
            match layout {
                Layout::Cards { cols, card_w } => self.draw_cards(f, body, cols, card_w, CARD_H, theme, now, fx),
                Layout::Blocks { cols } => self.draw_cards(f, body, cols, BLOCK_W, BLOCK_H, theme, now, fx),
                Layout::List => self.draw_list(f, body, theme, now, fx),
            }
            if let Some(row) = self.rows.get(self.selected) {
                f.render_widget(Paragraph::new(Line::from(self.detail(row, theme, detail.width as usize))), detail);
            }
        }
        let more = self.more_below(body);
        let line = if self.policy_page {
            Line::from(Span::styled(
                truncate("↑↓ scroll · o or Esc back to the sessions", foot.width as usize),
                theme.dim2(),
            ))
        } else if let Some(t) = &self.edit {
            let shown = truncate(&format!("nudge › {t}▏"), foot.width.saturating_sub(24) as usize);
            Line::from(vec![
                Span::styled(shown, theme.style(theme.accent)),
                Span::styled("  Enter send · Esc cancel", theme.dim2()),
            ])
        } else if let Some(n) = &self.note {
            Line::from(Span::styled(truncate(n, foot.width as usize), theme.style(theme.accent2)))
        } else {
            let hint = if self.panel.is_some() {
                "←↑↓→ move · Enter details · n nudge · o policy · p pause · P pause all · +/- zoom · Esc close"
                    .to_string()
            } else {
                "←↑↓→ move · +/- zoom · Esc close".to_string()
            };
            // "more below" is the part that must survive a narrow window
            let room = (foot.width as usize).saturating_sub(width(more));
            Line::from(vec![
                Span::styled(truncate(&hint, room), theme.dim2()),
                Span::styled(more, theme.style(theme.accent2)),
            ])
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    fn more_below(&self, body: Rect) -> &'static str {
        let last = self.cards.iter().map(|(i, _)| *i).max().map_or(0, |i| i + 1);
        let _ = body;
        if last < self.rows.len() {
            "  ↓ more"
        } else {
            ""
        }
    }

    fn header(&self, theme: &Theme, w: usize) -> Vec<Span<'static>> {
        let order = [
            State::WaitingPermission,
            State::WaitingAnswer,
            State::Stalled,
            State::Looping,
            State::Working,
            State::Idle { min: 0 },
            State::Done,
        ];
        let counts: Vec<(State, usize)> = order
            .into_iter()
            .map(|s| {
                (s, self.rows.iter().filter(|r| std::mem::discriminant(&r.state) == std::mem::discriminant(&s)).count())
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        // the words when they fit, else the glyph and the number alone: a count is never left out
        let build = |full: bool| -> (Vec<Span<'static>>, usize) {
            let mut out: Vec<Span<'static>> = Vec::new();
            let mut used = 0;
            for (s, n) in &counts {
                let (g, label) = chip(*s);
                let text = if full { format!("{g} {n} {label}") } else { format!("{g}{n}") };
                if used > 0 {
                    out.push(Span::raw(if full { "  " } else { " " }));
                    used += if full { 2 } else { 1 };
                }
                used += width(&text);
                out.push(Span::styled(text, theme.style(chip_color(theme, *s))));
            }
            if out.is_empty() {
                out.push(Span::styled("no sessions", theme.dim()));
                used = 11;
            }
            (out, used)
        };
        let (mut left, mut used) = build(true);
        if used > w {
            (left, used) = build(false);
        }
        let paused_all = self.panel.as_ref().is_some_and(|p| p.paused_all());
        let rights: [&str; 2] = match (&self.panel, paused_all) {
            (None, _) => ["watching · read-only", "read-only"],
            (Some(_), true) => ["supervisor paused", "paused"],
            (Some(_), false) => ["supervising · rules", "supervising"],
        };
        for right in rights {
            let rw = width(right);
            if used + rw + 2 <= w {
                left.push(Span::raw(" ".repeat(w - used - rw)));
                left.push(Span::styled(right, theme.dim2()));
                break;
            }
        }
        left
    }

    fn empty(&self, f: &mut Frame, body: Rect, theme: &Theme) {
        let lines = vec![
            Line::from(Span::styled("no sessions yet", theme.dim())).alignment(ratatui::layout::Alignment::Center),
            Line::from(Span::styled("this one shows up with your first message", theme.dim2()))
                .alignment(ratatui::layout::Alignment::Center),
        ];
        let h = (lines.len() as u16).min(body.height);
        f.render_widget(
            Paragraph::new(Text::from(lines)),
            Rect::new(body.x, body.y + body.height.saturating_sub(h) / 2, body.width, h),
        );
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
            SourceKind::Nexo => "◆",
            SourceKind::External => "◈",
        };
        if row.parent.is_some() {
            format!("└ {mark} {}", row.name)
        } else {
            format!("{mark} {}", row.name)
        }
    }

    fn since_action(&self, at: Secs) -> String {
        let d = (self.clock - at).max(0) as u64;
        format!("{} ago", age(d))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cards(
        &mut self,
        f: &mut Frame,
        body: Rect,
        cols: u16,
        card_w: u16,
        item_h: u16,
        theme: &Theme,
        now: Instant,
        fx: bool,
    ) {
        self.cols = cols as usize;
        let per_screen = ((body.height + 1) / (item_h + 1)).max(1) as usize;
        let sel_row = self.selected / cols as usize;
        if sel_row < self.scroll {
            self.scroll = sel_row;
        } else if sel_row >= self.scroll + per_screen {
            self.scroll = sel_row + 1 - per_screen;
        }
        let total_rows = self.rows.len().div_ceil(cols as usize);
        self.scroll = self.scroll.min(total_rows.saturating_sub(per_screen));
        let used_w = card_w * cols + GAP * (cols - 1);
        let x0 = body.x + body.width.saturating_sub(used_w) / 2;
        for (i, row) in self.rows.clone().iter().enumerate() {
            let (gr, gc) = (i / cols as usize, i % cols as usize);
            if gr < self.scroll || gr >= self.scroll + per_screen {
                continue;
            }
            let y = body.y + (gr - self.scroll) as u16 * (item_h + 1);
            if y + item_h > body.y + body.height {
                continue;
            }
            let r = Rect::new(x0 + gc as u16 * (card_w + GAP), y, card_w, item_h);
            let t = self.entered(&row.key, now);
            // a card rises one row as it fades in, when the row below it is free
            let drawn = if t < 0.4 && y + item_h < body.y + body.height { Rect { y: y + 1, ..r } } else { r };
            if t > 0.0 {
                if item_h == BLOCK_H {
                    self.block(f, drawn, i, row, theme, now);
                } else {
                    self.card(f, drawn, i, row, theme, now, fx);
                }
                Self::veil(f, drawn, theme, t);
            }
            self.cards.push((i, r));
        }
    }

    /// How lit a card's ground is: selected, gliding, and a soft hump while its state changes.
    fn lit(&self, i: usize, row: &AgentRow, now: Instant) -> f32 {
        let sel = if self.selected == i && self.still { 1.0 } else { self.glide.level(i, now) };
        let changed = self.fx.get(&row.key).and_then(|x| x.changed);
        sel.max(0.5 * hump(changed, now, CHIP_MS))
    }

    /// The far zoom: a tinted block with the state's colour down its left edge, the name, and the chip.
    fn block(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant) {
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
        let name_line = Span::styled(truncate(&row.name, w), name);
        let quiet = row.idle_secs.map_or_else(|| "-".to_string(), age);
        let sub = format!("{g} {}", truncate(&quiet, w.saturating_sub(2)));
        let at = |dy| Rect::new(r.x + 2, r.y + dy, w as u16, 1);
        f.render_widget(Paragraph::new(Line::from(name_line)).style(bg), at(0));
        f.render_widget(Paragraph::new(Line::from(Span::styled(sub, theme.style(color)))).style(bg), at(1));
        if r.height > 2 {
            let step = row.last_step.clone().unwrap_or_default();
            f.render_widget(Paragraph::new(Span::styled(truncate(&step, w), theme.dim2())).style(bg), at(2));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn card(&self, f: &mut Frame, r: Rect, i: usize, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
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

        // line 1: the name, the chip on the right
        let chip = self.chip_spans(row, theme, now, fx);
        let chip_w: usize = chip.iter().map(|s| width(&s.content)).sum();
        let name = truncate(&Self::name_of(row), cw.saturating_sub(chip_w + 1));
        let gap = cw.saturating_sub(width(&name) + chip_w);
        let mut l1 = vec![Span::styled(name, theme.bold(theme.fg))];
        l1.push(Span::raw(" ".repeat(gap)));
        l1.extend(chip);
        f.render_widget(Paragraph::new(Line::from(l1)).style(bg), at(0));

        // line 2: the last step
        let l2 = match &row.last_step {
            Some(s) => Span::styled(format!("▸ {}", truncate(s, cw.saturating_sub(2))), theme.dim()),
            None => Span::styled("▸ no step seen yet", theme.dim2()),
        };
        f.render_widget(Paragraph::new(Line::from(l2)).style(bg), at(1));

        // line 3: quiet for, source, cost; the supervisor's last action when there is room
        let quiet = row.idle_secs.map_or_else(|| "-".to_string(), age);
        let mut left = row.source.label().to_string();
        if let Some(c) = row.cost_usd.filter(|c| *c > 0.0) {
            left.push_str(&format!(" · ${c:.2}"));
        }
        left.push_str(&format!(" · quiet {quiet}"));
        let action = row.last_action.as_ref().map(|(what, t)| format!("{what} · {}", self.since_action(*t)));
        let mut l3 = vec![Span::styled(truncate(&left, cw), theme.dim())];
        if let Some(a) = action {
            let used = width(&left);
            if used + 2 + width(&a) <= cw {
                l3.push(Span::raw(" ".repeat(cw - used - width(&a))));
                l3.push(Span::styled(a, theme.style(theme.accent2)));
            }
        }
        f.render_widget(Paragraph::new(Line::from(l3)).style(bg), at(2));
    }

    fn draw_list(&mut self, f: &mut Frame, body: Rect, theme: &Theme, now: Instant, fx: bool) {
        self.cols = 1;
        let rows_in = body.height.max(1) as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows_in {
            self.scroll = self.selected + 1 - rows_in;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(rows_in));
        let w = body.width as usize;
        for (i, row) in self.rows.clone().iter().enumerate().skip(self.scroll).take(rows_in) {
            let k = i - self.scroll;
            let r = Rect::new(body.x, body.y + k as u16, body.width, 1);
            let level = self.lit(i, row, now);
            let bg = lit_style(theme, Style::default().bg(theme.modal_bg), level);
            // columns, dropped from the right as the width shrinks: chip | name | quiet | step
            let chip = self.chip_spans(row, theme, now, fx);
            let chip_w = 13usize;
            let shown_w: usize = chip.iter().map(|s| width(&s.content)).sum();
            let quiet = row.idle_secs.map(age).unwrap_or_else(|| "-".to_string());
            let quiet_w = 7usize;
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled(if self.selected == i { "▌" } else { " " }, theme.style(theme.accent)));
            let mut used = 1;
            let push_chip = |spans: &mut Vec<Span<'static>>| {
                spans.extend(chip.clone());
                spans.push(Span::raw(" ".repeat(chip_w.saturating_sub(shown_w))));
            };
            if w >= chip_w + 12 {
                push_chip(&mut spans);
                used += chip_w;
            } else {
                // too narrow for the label: the glyph alone
                let g = super::agents_view::chip(row.state).0;
                spans.push(Span::styled(format!("{g} "), theme.style(chip_color(theme, row.state))));
                used += 2;
            }
            let show_quiet = w >= 40;
            let show_step = w >= 64;
            let step_w = if show_step { (w - used).saturating_sub(quiet_w + 2) * 45 / 100 } else { 0 };
            let name_room = w
                .saturating_sub(used)
                .saturating_sub(if show_quiet { quiet_w + 1 } else { 0 })
                .saturating_sub(if show_step { step_w + 1 } else { 0 });
            let name = truncate(&Self::name_of(row), name_room.max(1));
            let pad_name = name_room.saturating_sub(width(&name));
            spans.push(Span::styled(name, theme.bold(theme.fg)));
            spans.push(Span::raw(" ".repeat(pad_name)));
            if show_quiet {
                spans.push(Span::styled(format!(" {}", fixed(&quiet, &"8".repeat(quiet_w))), theme.dim()));
            }
            if show_step {
                let step = row.last_step.clone().unwrap_or_default();
                spans.push(Span::styled(format!("  {}", truncate(&step, step_w.saturating_sub(1))), theme.dim2()));
            }
            f.render_widget(Paragraph::new(Line::from(spans)).style(bg), r);
            Self::veil(f, r, theme, self.entered(&row.key, now));
            self.cards.push((i, r));
        }
    }

    fn detail(&self, row: &AgentRow, theme: &Theme, w: usize) -> Vec<Span<'static>> {
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
