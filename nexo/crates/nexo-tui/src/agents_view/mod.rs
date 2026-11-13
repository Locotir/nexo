//! `/panel`: one card per agent session (this one, sub-agents, and other tools' sessions read from their
//! logs), each with its name, source, a coloured state chip, the last step, how long it has been quiet and
//! the supervisor's last action. It supervises and messages: a message flies to its card as a letter.

mod draw;
mod effects;
mod keys;
mod pulse;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
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

use crate::anim::{ease_out, progress};
use crate::modal::{contains, ModalOut};
use crate::motion::{fade, within, Glide};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

const CARD_MIN_W: u16 = 34;
const CARD_MAX_W: u16 = 60;
const CARD_H: u16 = 5;
/// With this much room the cards take a sixth row for the last events.
const RIBBON_FROM: u16 = 20;
/// With this much room the fleet pulse gets a line of its own under the header.
const PULSE_FROM: u16 = 22;
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
/// A letter takes this long from the header to the card it is for; the card then pulses.
const FLIGHT_MS: u64 = 1000;
/// Cells of streak behind the letter.
const TRAIL: u16 = 4;
/// Rows the window spends besides the sessions: borders, header and rule, detail, footer.
const CHROME: u16 = 2 + 2 + 1 + 1;
/// From this window height a blank row sets the sessions apart from their details.
const SPARE_FROM: u16 = 16;

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

/// Which sessions the view lists: `s` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Every session, nexo first.
    #[default]
    All,
    /// Only this program's sessions.
    Nexo,
    /// Only the sessions of other programs.
    External,
}

impl Filter {
    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Nexo,
            Filter::Nexo => Filter::External,
            Filter::External => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Nexo => "nexo only",
            Filter::External => "external only",
        }
    }

    fn keeps(self, r: &AgentRow) -> bool {
        match self {
            Filter::All => true,
            Filter::Nexo => r.source == SourceKind::Nexo,
            Filter::External => r.source == SourceKind::External,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Filter::All => 0,
            Filter::Nexo => 1,
            Filter::External => 2,
        }
    }

    fn from_u8(n: u8) -> Filter {
        match n {
            1 => Filter::Nexo,
            2 => Filter::External,
            _ => Filter::All,
        }
    }
}

/// The sessions of one program, under one header.
#[derive(Debug, Clone)]
struct Group {
    label: String,
    source: SourceKind,
    /// Indices into the shown rows; empty for a placeholder that says there is nothing yet.
    idx: Vec<usize>,
}

/// One horizontal strip of the body: a header, a row of cards (or one line), or a group's empty line.
#[derive(Debug, Clone)]
enum Band {
    Header(usize),
    Row(Vec<usize>),
    Empty(usize),
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
    draw::legible(theme, chip_hue(theme, state))
}

fn chip_hue(theme: &Theme, state: State) -> ratatui::style::Color {
    match state {
        State::WaitingPermission => theme.warn2,
        State::WaitingAnswer => theme.accent2,
        State::Stalled => theme.warn,
        State::Looping => theme.error,
        State::Working | State::Done => theme.success,
        State::Idle { .. } => theme.dim,
    }
}

/// `42s`, `7m`, `2h`, `3d`: one unit, rounded down, never more than four columns.
fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// What a card spells out with no session to show: what `/panel` is and how sessions get here.
const EMPTY_STATE: [&str; 7] = [
    "Live board of your agent sessions.",
    "They appear here on their own:",
    "▣ this session, after your message",
    "▢ other agents whose logs nexo reads",
    "",
    "One card each: its state, last step",
    "and how long it has been quiet.",
];

/// A row whose text is safe to lay out: another program's names, paths and steps may hold line breaks, escape
/// sequences or joiners that the terminal draws wider than the layout counted, and break a border.
fn tidy(mut r: AgentRow) -> AgentRow {
    let one = |s: &mut String| *s = crate::render::wrap::one_line(s);
    one(&mut r.name);
    one(&mut r.program);
    r.last_step.iter_mut().for_each(one);
    r.cwd.iter_mut().for_each(one);
    r.model.iter_mut().for_each(one);
    r.permission_mode.iter_mut().for_each(one);
    r.last_action.iter_mut().for_each(|(what, _)| one(what));
    r
}

/// The sessions balanced over their rows: 4 cards on 3 columns make two rows of 2, not 3 and an orphan.
fn balanced(n: usize, cols: usize) -> usize {
    let cols = cols.max(1);
    if n <= cols {
        return n.max(1);
    }
    n.div_ceil(n.div_ceil(cols))
}

/// `n` cells cut into `k` runs that differ by at most one, the longer ones first.
fn split_evenly(n: u16, k: usize) -> Vec<u16> {
    let k = k.max(1) as u16;
    (0..k).map(|i| n / k + u16::from(i < n % k)).collect()
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
    /// The card that pulses when a letter lands, and from when.
    pulse_at: Option<(String, Instant)>,
    /// A message is on its way to this session: the letter starts at the next draw.
    fly_key: Option<String>,
    /// The letter in flight: whose card it is going to, and when it left.
    flight: Option<(String, Instant)>,
    /// Ids of the mails whose letter was already sent; none until the first look (old mail does not fly).
    mails_seen: Option<std::collections::HashSet<String>>,
    pub zoom: Zoom,
    /// The ASCII forms of the glyphs the panel draws itself.
    ascii: bool,
    /// What the effects remember between two draws.
    live: effects::Fleet,
    /// The layout of the last draw (where a letter flies).
    drawn: Layout,
    /// Which sessions are listed.
    pub filter: Filter,
    /// Where the filter is kept for the rest of the session.
    memory: Option<Arc<AtomicU8>>,
    /// The rows of cards (or lines) of the last draw, as indices into the shown sessions, for the arrows.
    grid: Vec<Vec<usize>>,
    /// No motion (`[ui] animations = false`, or a mono theme).
    still: bool,
    /// The nudge being written for the selected session.
    edit: Option<String>,
    /// The session the nudge being written is for: (key, name). It stays that one if the selection moves.
    edit_for: Option<(String, String)>,
    /// Nudges put aside with Esc, by session, so `n` picks them up again.
    drafts: HashMap<String, String>,
    /// The column the arrows hold while the selection goes up and down over rows of different lengths.
    want_col: Option<usize>,
    /// The view keeps the selection in sight; the wheel lets go of it until the selection moves again.
    follow: bool,
    /// One line of news from the last action.
    note: Option<String>,
    /// The selected session's details are open.
    open: bool,
    /// The policy page is showing instead of the sessions.
    policy_page: bool,
    policy_scroll: usize,
    /// The size the window took when it opened.
    size: crate::dialog_size::Slot,
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
            fly_key: None,
            flight: None,
            mails_seen: None,
            pulse_at: None,
            zoom: Zoom::Cards,
            ascii: crate::ascii::wanted(),
            live: effects::Fleet::default(),
            drawn: Layout::List,
            filter: Filter::All,
            memory: None,
            grid: Vec::new(),
            still,
            edit: None,
            edit_for: None,
            drafts: HashMap::new(),
            want_col: None,
            follow: true,
            note: None,
            open: false,
            policy_page: false,
            policy_scroll: 0,
            size: Default::default(),
        };
        v.refresh();
        v
    }

    /// Starts from the filter kept in `memory` and writes every change back to it, so the choice outlives the
    /// window for the rest of the session.
    pub fn remembering(mut self, memory: Arc<AtomicU8>) -> AgentsView {
        self.filter = Filter::from_u8(memory.load(Ordering::Relaxed));
        self.memory = Some(memory);
        self
    }

    /// The sessions the filter keeps, grouped by program: nexo first, then each other program by name. The
    /// order is the one they are drawn in, so an index into it is what the selection holds.
    fn shown(&self) -> Vec<AgentRow> {
        let mut v: Vec<AgentRow> = self.rows.iter().filter(|r| self.filter.keeps(r)).cloned().map(tidy).collect();
        v.sort_by_key(|r| (r.source, r.program.to_lowercase()));
        v
    }

    fn selected_row(&self) -> Option<AgentRow> {
        self.shown().into_iter().nth(self.selected)
    }

    /// The groups of the shown sessions. A program with no sessions that the filter allows still gets its
    /// header, with a line that says there is nothing.
    fn groups(&self, shown: &[AgentRow]) -> Vec<Group> {
        let mut out: Vec<Group> = Vec::new();
        for (i, r) in shown.iter().enumerate() {
            let label = if r.source == SourceKind::Nexo { "nexo".to_string() } else { r.program.clone() };
            match out.last_mut() {
                Some(g) if g.label == label && g.source == r.source => g.idx.push(i),
                _ => out.push(Group { label, source: r.source, idx: vec![i] }),
            }
        }
        if self.filter != Filter::External && !out.iter().any(|g| g.source == SourceKind::Nexo) {
            out.insert(0, Group { label: "nexo".into(), source: SourceKind::Nexo, idx: Vec::new() });
        }
        if self.filter != Filter::Nexo && !out.iter().any(|g| g.source == SourceKind::External) {
            out.push(Group { label: "external".into(), source: SourceKind::External, idx: Vec::new() });
        }
        out
    }

    /// The strips of the body for `cols` cards across.
    fn bands(groups: &[Group], cols: usize, balance: bool) -> Vec<Band> {
        let mut out = Vec::new();
        for (g, grp) in groups.iter().enumerate() {
            out.push(Band::Header(g));
            if grp.idx.is_empty() {
                out.push(Band::Empty(g));
            }
            let per = if balance { balanced(grp.idx.len(), cols) } else { cols.max(1) };
            for chunk in grp.idx.chunks(per) {
                out.push(Band::Row(chunk.to_vec()));
            }
        }
        out
    }

    /// Cards across, the height of one, and whether the strips sit tight (lines) or breathe.
    fn shape(l: Layout, card_h: u16) -> (usize, u16, bool) {
        match l {
            Layout::Cards { cols, .. } => (cols as usize, card_h, false),
            Layout::Blocks { cols } => (cols as usize, BLOCK_H, false),
            Layout::List => (1, 1, true),
        }
    }

    fn band_h(b: &Band, item_h: u16) -> u16 {
        match b {
            Band::Row(_) => item_h,
            _ => 1,
        }
    }

    /// The blank rows between two strips: none in the lines, none under a header, one elsewhere.
    fn band_gap(prev: &Band, tight: bool) -> u16 {
        u16::from(!tight && !matches!(prev, Band::Header(_)))
    }

    /// The height of strips `from..to` laid one under the other.
    fn span(bands: &[Band], from: usize, to: usize, item_h: u16, tight: bool) -> u16 {
        let mut h = 0;
        for i in from..to.min(bands.len()) {
            if i > from {
                h += Self::band_gap(&bands[i - 1], tight);
            }
            h += Self::band_h(&bands[i], item_h);
        }
        h
    }

    /// The height of all the strips of `groups` laid out as `l`.
    fn content_h(groups: &[Group], l: Layout, card_h: u16) -> u16 {
        let (cols, item_h, tight) = Self::shape(l, card_h);
        let bands = Self::bands(groups, cols, matches!(l, Layout::Cards { .. }));
        Self::span(&bands, 0, bands.len(), item_h, tight)
    }

    /// Cards that would hide sessions behind a scroll give way to the lines when those show every session.
    fn fit_cards(groups: &[Group], l: Layout, body_h: u16, card_h: u16) -> Layout {
        match l {
            Layout::Cards { .. }
                if Self::content_h(groups, l, card_h) > body_h
                    && Self::content_h(groups, Layout::List, card_h) <= body_h =>
            {
                Layout::List
            }
            other => other,
        }
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
        let keep = self.selected_row().map(|r| r.key);
        self.rows = rows;
        self.retarget(keep);
    }

    /// After the sessions changed: the selection goes with its session (not its index) when that is still
    /// shown, else it stays near where it was.
    fn retarget(&mut self, keep: Option<String>) {
        let shown = self.shown();
        self.selected = match keep.and_then(|k| shown.iter().position(|r| r.key == k)) {
            Some(i) => i,
            None => self.selected.min(shown.len().saturating_sub(1)),
        };
    }

    pub fn rows(&self) -> &[AgentRow] {
        &self.rows
    }

    /// The key of the selected session.
    pub fn selected_key(&self) -> Option<String> {
        self.selected_row().map(|r| r.key)
    }

    /// Where each session was drawn last (the pointer hits).
    pub fn card_rects(&self) -> Vec<(usize, Rect)> {
        self.cards.clone()
    }

    /// The selected session's details are open.
    pub fn details_open(&self) -> bool {
        self.open
    }

    /// How far the policy page is scrolled.
    pub fn policy_scroll(&self) -> usize {
        self.policy_scroll
    }

    /// Draws the glyphs the panel makes itself in their ASCII forms (or not).
    pub fn set_ascii(&mut self, on: bool) {
        self.ascii = on;
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
        let feed = panel.mail_feed(now - 3600);
        self.ingest_mail(feed, &rows, now);
        self.seen = v;
        let keep = self.selected_row().map(|r| r.key);
        self.rows = rows;
        self.clock = clock;
        self.retarget(keep);
        changed
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

    /// What keeps the window the same window: the zoom, the filter, the page and whether any session has arrived yet (the first poll can land after the first draw), not the cursor.
    pub fn size_key(&self) -> (u8, u8, bool, bool) {
        (self.zoom as u8, self.filter as u8, self.policy_page, self.rows.is_empty())
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
        let shown = self.shown();
        let groups = self.groups(&shown);
        // the cards take a sixth row for the last events, and the fleet a line of figures under the header,
        // when the terminal has the room for every session that way; else the cards keep five rows
        let pulse_ok = !self.policy_page && !self.rows.is_empty() && area.height >= PULSE_FROM;
        let layout_with = |h: u16, card_h: u16, pulse: u16| match zoom {
            Zoom::Blocks => blocks_layout(inner_w),
            Zoom::List => Layout::List,
            Zoom::Cards => {
                Self::fit_cards(&groups, cards_layout(inner_w, h), h.saturating_sub(CHROME + 1 + pulse), card_h)
            }
        };
        let (card_h, pulse) = [(CARD_H + 1, 1), (CARD_H, 1), (CARD_H, 0)]
            .into_iter()
            .filter(|(ch, pl)| (*ch == CARD_H || area.height >= RIBBON_FROM) && (*pl == 0 || pulse_ok))
            .find(|&(ch, pl)| zoom != Zoom::Cards || matches!(layout_with(area.height, ch, pl), Layout::Cards { .. }))
            .unwrap_or((CARD_H, u16::from(pulse_ok)));
        let layout_for = |h: u16| layout_with(h, card_h, pulse);
        // a blank row between the sessions and their details, when the window is tall enough to spare it
        let spare = u16::from(area.height >= SPARE_FROM);
        let want = if self.policy_page {
            area.height
        } else if self.rows.is_empty() && self.filter == Filter::All {
            EMPTY_STATE.len() as u16 + CHROME + spare + pulse
        } else {
            Self::content_h(&groups, layout_for(area.height), card_h) + CHROME + spare + pulse
        };
        let h = want.clamp(8.min(area.height), area.height);
        let w = inner_w + 4;
        // the size it took when it opened: sessions coming and going never resize it
        let key = {
            use std::hash::{Hash, Hasher};
            let mut k = std::collections::hash_map::DefaultHasher::new();
            self.size_key().hash(&mut k);
            k.finish()
        };
        let (w, h) = self.size.keep(key, f.area(), area, || (w, h));
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
        // the pulse row only where the window kept room for it
        let pulse = pulse.min(u16::from(r.height >= CHROME + 5));
        let rule_y = pad.y + 1 + pulse;
        // header: how many sessions are in which state, and what this view is
        f.render_widget(
            Paragraph::new(Line::from(self.header(theme, pad.width as usize, pulse == 0))),
            Rect { height: 1, ..pad },
        );
        if pulse > 0 {
            let line = self.pulse_line(theme, pad.width as usize, now, fx);
            f.render_widget(Paragraph::new(Line::from(line)), Rect { y: pad.y + 1, height: 1, ..pad });
        }
        // four rows or fewer: the rule gives way so one session line shows
        let slim = pad.height < 5 + pulse;
        if !slim {
            f.render_widget(
                Paragraph::new(Span::styled("─".repeat(pad.width as usize), theme.dim2())),
                Rect { y: rule_y, height: 1, ..pad },
            );
        }
        let foot = Rect::new(pad.x, pad.y + pad.height.saturating_sub(1), pad.width, 1);
        let detail = Rect::new(pad.x, foot.y.saturating_sub(1), pad.width, 1);
        let body_top = pad.y + if slim { 1 } else { 2 } + pulse;
        let spare = u16::from(r.height >= SPARE_FROM);
        let body = Rect::new(pad.x, body_top, pad.width, detail.y.saturating_sub(body_top + spare));
        self.cards.clear();
        if self.policy_page {
            self.draw_policy(f, body, theme);
        } else {
            self.glide.note(Some(self.selected), now, fx);
            self.note(now, fx);
            let (layout, card_h) = match self.zoom {
                Zoom::Blocks if body.height >= BLOCK_H => (blocks_layout(body.width), card_h),
                Zoom::Blocks | Zoom::List => (Layout::List, card_h),
                Zoom::Cards => {
                    // a window that kept an older size may have lost the room for the sixth row
                    let at = |ch: u16| Self::fit_cards(&groups, cards_layout(body.width, body.height), body.height, ch);
                    match at(card_h) {
                        Layout::List if card_h > CARD_H => (at(CARD_H), CARD_H),
                        other => (other, card_h),
                    }
                }
            };
            self.draw_bands(f, body, layout, card_h, &shown, &groups, theme, now, fx);
            let lane = Rect { y: rule_y, height: 1, ..pad };
            if !slim {
                self.draw_flights(f, lane, body, &shown, theme, now);
                self.draw_letter(f, lane, body, &shown, theme, now);
            }
            if let Some(row) = shown.get(self.selected) {
                f.render_widget(Paragraph::new(Line::from(self.detail(row, theme, detail.width as usize))), detail);
            }
        }
        let more = self.more_marks();
        let more = more.as_str();
        let line = if self.policy_page {
            Line::from(Span::styled(truncate("↑↓ scroll · o/Esc back", foot.width as usize), theme.dim2()))
        } else if let Some(t) = &self.edit {
            // the end of the text is what is being typed: it is the part kept when the line is long
            let gone = self.edit_for.as_ref().is_some_and(|(k, _)| !self.rows.iter().any(|r| &r.key == k));
            let tails: [&str; 3] = if gone {
                ["  session gone · Esc keep", "  gone · Esc", ""]
            } else {
                ["  Enter send · Esc cancel", "  Enter · Esc", ""]
            };
            let w = foot.width as usize;
            // the longest tail that still leaves 12 columns for the message
            let tail = tails.into_iter().find(|s| w >= width(s) + 12).unwrap_or("");
            let shown =
                tail_of(&format!("message › {t}▏"), w.saturating_sub(width(tail)), "message › ".chars().count());
            Line::from(vec![
                Span::styled(shown, theme.style(theme.accent)),
                Span::styled(tail, if gone { theme.style(theme.warn) } else { theme.dim2() }),
            ])
        } else if let Some(n) = &self.note {
            Line::from(Span::styled(truncate(n, foot.width as usize), theme.style(theme.accent2)))
        } else {
            // "more below" is the part that must survive a narrow window
            let room = (foot.width as usize).saturating_sub(width(more));
            let hints: &[&str] = if self.panel.is_some() {
                &[
                    if self.open {
                        "←↑↓→ move · s source · Enter close · n message · o policy · p/P pause · +/- zoom · Esc"
                    } else {
                        "←↑↓→ move · s source · Enter open · n message · o policy · p/P pause · +/- zoom · Esc"
                    },
                    "←↑↓→ move · s source · n message · o policy · p pause · +/- zoom · Esc close",
                    "s source · n message · p pause · Esc close",
                    "s source · Esc close",
                    "Esc close",
                ]
            } else {
                &[
                    "←↑↓→ move · Enter open · s source · +/- zoom · Esc close",
                    "←↑↓→ move · s source · +/- zoom · Esc close",
                    "s source · Esc close",
                    "Esc close",
                ]
            };
            let hint = hints.iter().find(|h| width(h) <= room).copied().unwrap_or("Esc close");
            Line::from(vec![
                Span::styled(truncate(hint, room), theme.dim2()),
                Span::styled(more, theme.style(theme.accent2)),
            ])
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    /// `↑ 2 ↓ 3` when sessions sit out of sight above and below, so a scrolled list never looks complete.
    fn more_marks(&self) -> String {
        let total = self.shown().len();
        let Some((first, last)) = self
            .cards
            .iter()
            .map(|(i, _)| *i)
            .fold(None, |m: Option<(usize, usize)>, i| Some(m.map_or((i, i), |(a, b)| (a.min(i), b.max(i)))))
        else {
            return String::new();
        };
        let below = total.saturating_sub(last + 1);
        match (first, below) {
            (0, 0) => String::new(),
            (0, n) => format!("  ↓ {n} more"),
            (n, 0) => format!("  ↑ {n} above"),
            (a, b) => format!("  ↑ {a} ↓ {b}"),
        }
    }

    fn header(&self, theme: &Theme, w: usize, fold: bool) -> Vec<Span<'static>> {
        let order = [
            State::WaitingPermission,
            State::WaitingAnswer,
            State::Stalled,
            State::Looping,
            State::Working,
            State::Idle { min: 0 },
            State::Done,
        ];
        let shown = self.shown();
        let counts: Vec<(State, usize)> = order
            .into_iter()
            .map(|s| {
                (s, shown.iter().filter(|r| std::mem::discriminant(&r.state) == std::mem::discriminant(&s)).count())
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
        // which sessions this is, ahead of what the supervisor does; "+N" counts those the filter hides
        let hidden = self.rows.len() - shown.len();
        let which =
            if hidden > 0 { format!("{} +{hidden}", self.filter.label()) } else { self.filter.label().to_string() };
        // with no pulse row of its own, the fleet's figures ride at the front of the right side
        let fold = if fold { self.pulse_fold(&shown) } else { None };
        let mut candidates: Vec<String> = Vec::new();
        if let Some(p) = &fold {
            candidates.push(format!("{p} · {which} · {}", rights[0]));
            candidates.push(format!("{p} · {which} · {}", rights[1]));
            candidates.push(format!("{p} · {which}"));
        }
        candidates.extend([
            format!("{which} · {}", rights[0]),
            format!("{which} · {}", rights[1]),
            which.clone(),
            which.split(' ').next().unwrap_or("").to_string(),
        ]);
        for right in &candidates {
            let rw = width(right);
            if used + rw + 2 <= w {
                left.push(Span::raw(" ".repeat(w - used - rw)));
                left.push(Span::styled(right.clone(), theme.dim2()));
                break;
            }
        }
        left
    }
}

/// The last `room` columns of `text`, with an ellipsis in front when something was cut; the first `keep`
/// characters (a label) are always kept.
fn tail_of(text: &str, room: usize, keep: usize) -> String {
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
