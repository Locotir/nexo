//! `/panel`: mission control for everything nexo runs. A tree on the left groups the rows (who needs the
//! person, this session and its sub-agents, the `nexo team` lanes, the other live nexo sessions, then the
//! sessions of other programs), one line each; the pane on the right tells the selected row in full. The keys
//! depend on the kind of row selected, and the footer lists only those. It supervises, messages, starts and
//! stops lanes; a message flies to its row as a letter.

mod chips;
mod detail;
mod draw;
mod effects;
mod keys;
mod lanes;
mod observe;
mod pane;
mod pulse;
mod row;
mod tree;
mod view;
mod window;

use self::chips::{age, attention, chip, chip_color};
pub use self::lanes::Starter;
pub use self::view::{needs_you, FleetCount, Filter, Group, Kind, Show};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_agent::supervise::run::now_secs;
use nexo_agent::supervise::{AgentRow, Panel, Secs, SourceKind, State};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::anim::{ease_out, progress};
use crate::modal::{contains, ModalOut};
use crate::motion::{fade, within};
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// From this width the pane sits beside the tree; under it the tree takes the width and Enter opens the pane.
pub const WIDE_FROM: u16 = 110;
/// One breath of an attention chip.
const BREATH_MS: u128 = 1600;
/// A new row slides in from the right in this long; the first rows of a new view start one after the other,
/// `STAGGER_MS` apart, up to `STAGGER_MAX`.
const ENTER_MS: u64 = 280;
const STAGGER_MS: u64 = 25;
const STAGGER_MAX: u64 = 200;
/// A glyph changing state cross-fades from the old colour to the new one in this long.
const CHIP_MS: u64 = 400;
/// A letter takes this long from the footer to the row it is for; the row then pulses.
const FLIGHT_MS: u64 = 1000;
/// Cells of streak behind the letter.
const TRAIL: u16 = 4;
/// A row that starts needing the person pulses this long, and the NEEDS YOU header blinks once.
const NEED_MS: u64 = 900;
/// A lane's round that ended pops its ✓ or ✗ this long.
const POP_MS: u64 = 700;
/// Two clicks on one row within this long are one double click (Enter).
const DOUBLE_MS: u128 = 400;

/// What the tree says with no row to show: what `/panel` is and how rows get here.
const EMPTY_STATE: [&str; 5] = [
    "Nothing running besides this session yet.",
    "Rows appear here on their own:",
    "this session after your message, its sub-agents,",
    "nexo team lanes, other nexo sessions,",
    "and other agents whose logs nexo reads.",
];

/// What the pane says with nothing selected: the groups the tree will hold, in their order.
const LEGEND: [(&str, &str); 5] = [
    ("NEEDS YOU", "permission waits, errors, failed lanes, held mail"),
    ("THIS SESSION", "this session, its sub-agents and its jobs"),
    ("LANES", "nexo team lanes: s start, x stop, p pause all"),
    ("NEXO SESSIONS", "nexo running in other terminals"),
    ("other programs", "one group per program, folded to its count"),
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
    r.goal.iter_mut().for_each(one);
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

/// The motion of one row: when it first showed, and its last change of state.
#[derive(Debug, Clone)]
struct RowFx {
    born: Instant,
    delay_ms: u64,
    state: State,
    prev: State,
    changed: Option<Instant>,
    /// It started needing the person at this moment (it pulses).
    needs: Option<Instant>,
    /// Its lane's round ended at this moment, well or not (the glyph pops).
    round: Option<(Instant, bool)>,
    /// It needed the person at the last look.
    needing: bool,
    /// When its lane's last round ended, at the last look.
    last_round: Option<Secs>,
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
    /// The selected line of the tree (a group's header or a row).
    pub selected: usize,
    panel: Option<Arc<Panel>>,
    seen: u64,
    rows: Vec<AgentRow>,
    /// The clock the age of an action is told against (seconds).
    clock: Secs,
    pub window: Rect,
    pub close: Rect,
    pub hover: bool,
    /// Where each row was drawn last, as an index into the rows in tree order (the letters fly to it).
    cards: Vec<(usize, Rect)>,
    /// Where each tree line was drawn last, as an index into the tree (the pointer).
    lines: Vec<(usize, Rect)>,
    /// Where the tree and the pane were drawn last (the wheel scrolls the one under the pointer).
    tree_area: Rect,
    pane_area: Rect,
    /// The first tree line in sight.
    pub scroll: usize,
    /// How far the pane's events are scrolled.
    pane_scroll: usize,
    pane_max: usize,
    /// When the view was first drawn (the phase of the breathing glyphs).
    opened: Option<Instant>,
    fx: HashMap<String, RowFx>,
    /// The NEEDS YOU header blinks once from this moment.
    need_blink: Option<Instant>,
    /// The row that pulses when a letter lands, and from when.
    pulse_at: Option<(String, Instant)>,
    /// A message is on its way to this session: the letter starts at the next draw.
    fly_key: Option<String>,
    /// The letter in flight: whose row it is going to, and when it left.
    flight: Option<(String, Instant)>,
    /// Ids of the mails whose letter was already sent; none until the first look (old mail does not fly).
    mails_seen: Option<HashSet<String>>,
    /// Parent rows folded away.
    collapsed: HashSet<String>,
    /// Groups whose fold differs from their first one.
    flipped: HashSet<String>,
    /// The ASCII forms of the glyphs the panel draws itself.
    ascii: bool,
    /// What the effects remember between two draws.
    live: effects::Fleet,
    /// Which sessions are listed by source (`F`).
    pub filter: Filter,
    /// Which rows are listed by what they do (`f`).
    pub show: Show,
    /// The text filter (`/`), and whether it is being typed.
    find: Option<String>,
    finding: bool,
    /// Where the source filter is kept for the rest of the session.
    memory: Option<Arc<AtomicU8>>,
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
    /// A job of this session waiting for the person's yes before it is stopped.
    kill_ask: Option<u32>,
    /// The view keeps the selection in sight; the wheel lets go of it until the selection moves again.
    follow: bool,
    /// One line of news from the last action.
    note: Option<String>,
    /// Narrow window: the pane is open at full width.
    open: bool,
    /// The keys act on the pane (Enter or Tab moved them there): the arrows scroll its events.
    focus: bool,
    /// The policy page is showing instead of the tree.
    policy_page: bool,
    policy_scroll: usize,
    /// Stable key of the session being observed, independent of fleet reordering.
    observing: Option<String>,
    observe_scroll: usize,
    observe_max: usize,
    history_rx: Option<HistoryReceiver>,
    history: Option<Result<nexo_agent::session::HistorySnapshot, String>>,
    /// The runs of this session's sub-agents by job id (a message to a running one goes to its run).
    runs: Vec<(u32, String)>,
    /// The end of the output of this session's jobs, by job id, as `/jobs` reads it.
    tails: HashMap<u32, String>,
    /// The last time the tails were asked for.
    tails_asked: Option<Instant>,
    /// Starts a lane's runner: the detached `nexo team run <name>`; a test puts a stand-in here.
    starter: Starter,
    starter_now: Starter,
    /// The questions of a new lane, while they are being answered.
    form: Option<lanes::Form>,
    /// A lane asked to stop so that this session takes its place.
    wait: Option<lanes::Wait>,
    /// The session of a lane that stopped for this one: the app resumes it.
    resume: Option<String>,
    /// The last click on a tree line: which, and when (two make a double click).
    last_click: Option<(usize, Instant)>,
    /// The pointer was last over the pane (the wheel scrolls it).
    over_pane: bool,
    /// The LANES group shows even with no lane yet (the panel was opened with `/team`).
    lanes_first: bool,
    /// Every lane is paused (`nexo team pause`). Read by `refresh` from the team folder, never while drawing, so a
    /// view built from rows alone (tests) never touches the person's files.
    lanes_paused: bool,
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
            lines: Vec::new(),
            tree_area: Rect::default(),
            pane_area: Rect::default(),
            scroll: 0,
            pane_scroll: 0,
            pane_max: 0,
            opened: None,
            fx: HashMap::new(),
            need_blink: None,
            fly_key: None,
            flight: None,
            mails_seen: None,
            pulse_at: None,
            collapsed: Default::default(),
            flipped: Default::default(),
            ascii: crate::ascii::wanted(),
            live: effects::Fleet::default(),
            filter: Filter::All,
            show: Show::All,
            find: None,
            finding: false,
            memory: None,
            still,
            edit: None,
            edit_for: None,
            reach: nexo_core::reach::Reach::default(),
            confirm: None,
            kill_ask: None,
            drafts: HashMap::new(),
            follow: true,
            note: None,
            open: false,
            focus: false,
            policy_page: false,
            policy_scroll: 0,
            observing: None,
            observe_scroll: 0,
            observe_max: 0,
            history_rx: None,
            history: None,
            runs: Vec::new(),
            tails: HashMap::new(),
            tails_asked: None,
            starter: nexo_core::team::spawn::runner,
            starter_now: nexo_core::team::spawn::runner_now,
            form: None,
            wait: None,
            resume: None,
            last_click: None,
            over_pane: false,
            lanes_first: false,
            lanes_paused: false,
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

    /// Opens on the lanes (`/team`): the LANES group shows even when empty, and the cursor starts on it.
    pub fn on_lanes(mut self) -> AgentsView {
        self.lanes_first = true;
        self.select_lanes();
        self
    }

    /// The cursor on the LANES header.
    pub fn select_lanes(&mut self) {
        self.lanes_first = true;
        if let Some(i) = self.items().iter().position(|it| matches!(it, tree::Item::Head { group: Group::Lanes, .. })) {
            self.selected = i;
            self.follow = true;
        }
    }

    /// The rows in tree order, each once (a row that needs the person is counted in its own group): what the
    /// totals count and the letters fly to.
    fn shown(&self) -> Vec<AgentRow> {
        let items = self.items();
        items.iter().filter(|it| it.group() != &Group::Needs).filter_map(|it| it.row().cloned()).collect()
    }

    fn selected_item(&self) -> Option<tree::Item> {
        self.items().into_iter().nth(self.selected)
    }

    fn selected_row(&self) -> Option<AgentRow> {
        self.selected_item().and_then(|it| it.row().cloned())
    }

    /// The kind of the selected row; None on a group's header.
    pub fn selected_kind(&self) -> Option<Kind> {
        self.selected_item().and_then(|it| it.kind())
    }

    /// A view of fixed rows, no panel behind it (tests).
    pub fn with_rows(rows: Vec<AgentRow>, clock: Secs, still: bool) -> AgentsView {
        let mut v = AgentsView::new(None, still);
        v.rows = rows;
        v.clock = clock;
        v.retarget(None);
        v
    }

    /// Looks for sessions of other programs here instead of the usual places (tests).
    pub fn set_reach(&mut self, reach: nexo_core::reach::Reach) {
        self.reach = reach;
    }

    /// Replaces the rows as a poll would (tests drive the motion with it).
    pub fn set_rows(&mut self, rows: Vec<AgentRow>) {
        let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
        self.rows = rows;
        self.retarget(keep);
    }

    /// The clock rows are told against (tests).
    pub fn set_clock(&mut self, clock: Secs) {
        self.clock = clock;
    }

    /// The runs of this session's sub-agents, by job id: `n` on a running one sends to its run.
    pub fn set_runs(&mut self, runs: Vec<(u32, String)>) {
        self.runs = runs;
    }

    /// The end of the output of this session's jobs (the answer to `JobTails`).
    pub fn set_tails(&mut self, tails: Vec<nexo_core::jobs::JobTail>) {
        self.tails = tails.into_iter().map(|t| (t.id, t.text)).collect();
    }

    /// Whether the pane wants the tails of this session's jobs now: a job of this session is selected and the
    /// last ask is a second old. It notes the ask.
    pub fn wants_tails(&mut self, now: Instant) -> bool {
        if self.selected_kind() != Some(Kind::Sub) || self.local_job().is_none() {
            return false;
        }
        if self.tails_asked.is_some_and(|t| now.duration_since(t).as_millis() < 1000) {
            return false;
        }
        self.tails_asked = Some(now);
        true
    }

    /// The job id of the selected sub-agent of this session (`<session>.<job id>`).
    fn local_job(&self) -> Option<u32> {
        let row = self.selected_row()?;
        if Kind::of(&row) != Kind::Sub {
            return None;
        }
        row.key.rsplit_once('.')?.1.parse().ok()
    }

    /// After the rows changed: the selection goes with its line (not its index) when that is still shown, then
    /// with its row in another group, else it stays near where it was.
    fn retarget(&mut self, keep: Option<(String, Option<String>)>) {
        let items = self.items();
        // with nothing selected yet the cursor starts on the first row, not on a header: the pane then shows
        // someone at once and the row keys act without a first Down
        if keep.is_none() {
            self.selected = items.iter().position(|it| it.row().is_some()).unwrap_or(0);
            return;
        }
        let found = keep.and_then(|(id, key)| {
            items.iter().position(|it| it.id() == id).or_else(|| {
                let key = key?;
                items.iter().position(|it| it.row().is_some_and(|r| r.key == key))
            })
        });
        self.selected = found.unwrap_or_else(|| self.selected.min(items.len().saturating_sub(1)));
    }

    pub fn rows(&self) -> &[AgentRow] {
        &self.rows
    }

    /// The key of the selected row.
    pub fn selected_key(&self) -> Option<String> {
        self.selected_row().map(|r| r.key)
    }

    /// Where each row was drawn last (the pointer hits), as an index into the rows in tree order.
    pub fn card_rects(&self) -> Vec<(usize, Rect)> {
        self.cards.clone()
    }

    /// Where each tree line was drawn last, as an index into the tree.
    pub fn line_rects(&self) -> Vec<(usize, Rect)> {
        self.lines.clone()
    }

    /// The pane shows at full width (a narrow window after Enter).
    pub fn details_open(&self) -> bool {
        self.open
    }

    /// The keys act on the pane.
    pub fn pane_focused(&self) -> bool {
        self.focus
    }

    /// How far the policy page is scrolled.
    pub fn policy_scroll(&self) -> usize {
        self.policy_scroll
    }

    /// What the last action said.
    pub fn said(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Draws the glyphs the panel makes itself in their ASCII forms (or not).
    pub fn set_ascii(&mut self, on: bool) {
        self.ascii = on;
    }

    /// Takes the panel's newest rows. True when something the view shows changed (the caller repaints).
    pub fn refresh(&mut self) -> bool {
        let history_changed = self.poll_history();
        let waited = self.poll_wait(Instant::now());
        let Some(panel) = &self.panel else { return history_changed || waited };
        let now = now_secs();
        let v = panel.version();
        let mut rows = panel.rows(now);
        for r in &mut rows {
            r.idle_secs = r.idle_secs.map(bucket);
        }
        let clock = if rows.iter().any(|r| r.idle_secs.is_some_and(|s| s < 120)) { now } else { now / 60 * 60 };
        let lanes_paused = rows.iter().any(|r| r.lane.is_some()) && nexo_core::team::state::paused();
        let changed = rows != self.rows || clock != self.clock || lanes_paused != self.lanes_paused;
        self.lanes_paused = lanes_paused;
        let feed = panel.mail_feed(now - 3600);
        self.ingest_mail(feed, &rows, now);
        self.seen = v;
        let keep = self.selected_item().map(|it| (it.id(), it.row().map(|r| r.key.clone())));
        self.rows = rows;
        self.clock = clock;
        self.retarget(keep);
        changed || history_changed || waited
    }

    /// 0..1: how far a row has slid in (1 with no motion).
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

    /// The state to draw for a session and the colour of its glyph: while the state is changing, the old
    /// one's colour fades into the new one's.
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

    /// What keeps the window the same window: the filters, the page, and whether any row has arrived yet.
    pub fn size_key(&self) -> (u8, u8, bool, bool) {
        (self.show as u8, self.filter as u8, self.policy_page || self.observing.is_some(), self.rows.is_empty())
    }
}
