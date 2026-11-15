//! `/panel`: one card per agent session (this one, sub-agents, and other tools' sessions read from their
//! logs), each with its name, source, a coloured state chip, the last step, how long it has been quiet and
//! the supervisor's last action. It supervises and messages: a message flies to its card as a letter.

mod bands;
mod card;
mod chips;
mod compact;
mod detail;
mod draw;
mod effects;
mod keys;
mod layout;
mod map;
mod observe;
mod pulse;
mod tree;
mod view;
mod window;

use self::chips::{age, attention, chip, chip_color};
pub use self::layout::{blocks_layout, cards_layout, Layout};
use self::layout::{Band, Group};
use self::view::View;
pub use self::view::{Filter, Zoom};

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
    r.last_conclusion.iter_mut().for_each(one);
    r.cwd.iter_mut().for_each(one);
    r.model.iter_mut().for_each(one);
    r.permission_mode.iter_mut().for_each(one);
    r.last_action.iter_mut().for_each(|(what, _)| one(what));
    r
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

/// A message for a session of another program, asked about before it is typed in.
#[derive(Debug, Clone)]
struct Typing {
    key: String,
    name: String,
    address: String,
    text: String,
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
    view: View,
    collapsed: std::collections::HashSet<String>,
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
    /// Where sessions of other programs can be typed into.
    reach: nexo_core::reach::Reach,
    /// A message waiting for the person's yes before it is typed into another program.
    confirm: Option<Typing>,
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
    /// Stable key of the session being observed, independent of fleet reordering.
    observing: Option<String>,
    observe_scroll: usize,
    observe_max: usize,
    history_rx: Option<HistoryReceiver>,
    history: Option<Result<nexo_agent::session::HistorySnapshot, String>>,
    /// The size the window took when it opened.
    size: crate::dialog_size::Slot,
}

type HistoryReceiver =
    Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Result<nexo_agent::session::HistorySnapshot, String>>>>;

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
            view: View::Cards,
            collapsed: Default::default(),
            ascii: crate::ascii::wanted(),
            live: effects::Fleet::default(),
            drawn: Layout::List,
            filter: Filter::All,
            memory: None,
            grid: Vec::new(),
            still,
            edit: None,
            edit_for: None,
            reach: nexo_core::reach::Reach::default(),
            confirm: None,
            drafts: HashMap::new(),
            want_col: None,
            follow: true,
            note: None,
            open: false,
            policy_page: false,
            policy_scroll: 0,
            observing: None,
            observe_scroll: 0,
            observe_max: 0,
            history_rx: None,
            history: None,
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
        if self.view != View::Cards {
            v = tree::visible(tree::ordered(v), &self.collapsed);
        }
        v
    }

    fn tree_rows(&self) -> Vec<AgentRow> {
        let mut rows: Vec<_> = self.rows.iter().filter(|r| self.filter.keeps(r)).cloned().map(tidy).collect();
        rows.sort_by_key(|r| (r.source, r.program.to_lowercase()));
        tree::ordered(rows)
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

    /// A view of fixed rows, no panel behind it (tests).
    pub fn with_rows(rows: Vec<AgentRow>, clock: Secs, still: bool) -> AgentsView {
        let mut v = AgentsView::new(None, still);
        v.rows = rows;
        v.clock = clock;
        v
    }

    /// Looks for sessions of other programs here instead of the usual places (tests).
    pub fn set_reach(&mut self, reach: nexo_core::reach::Reach) {
        self.reach = reach;
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
        let history_changed = self.poll_history();
        let Some(panel) = &self.panel else { return history_changed };
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
        changed || history_changed
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
        (self.zoom as u8, self.filter as u8, self.policy_page || self.observing.is_some(), self.rows.is_empty())
    }
}
