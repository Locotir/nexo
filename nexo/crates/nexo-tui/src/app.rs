//! The state of the interface and what the agent's events do to it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::{
    AgentCommand, AgentEvent, Attachment, CallMark, CommandTx, ContextCell, ContextZone, GoalState, Level, TurnEnd,
};
use nexo_core::message::{result_text, Message, Part, Role};
use nexo_core::permission::{PermissionDecision, PermissionMode, PermissionRequest};
use nexo_core::pricing::Pricing;
use nexo_core::stream::Usage;
use nexo_core::tool::{TodoItem, ToolDisplay, ToolOutput};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::anim::{self, Tween};
use crate::app_fx::Fx;
use crate::commands::{self, Command, GoalArg};
use crate::input::Input;
use crate::modal::{Modal, ModalOut, PickItem, PickKind, Picker, Setup};
use crate::palette::{self, Palette, PaletteKind};
use crate::render::blocks::{fmt_ms, Block, RenderOpts, ToolStatus};
use crate::render::diff;
use crate::scrollbar::{self, Bar, Hit};
use crate::select::{Pos, Selection};
use crate::theme::Theme;
use crate::TuiOptions;

pub const WHEEL_LINES: usize = 3;
const MAX_PROGRESS: usize = 500;
/// Columns of the changed-files panel, and the narrowest terminal that gets one.
pub const FILES_PANEL_W: u16 = 30;
pub const FILES_PANEL_MIN_W: u16 = 100;
/// How long a cell of the context map that changed stays lit.
pub const MAP_FLASH_MS: u64 = 900;
/// The window the gauge measures against when the model never said its own.
pub const GUESS_WINDOW: u64 = 128_000;

/// A file this session changed, with the lines added and removed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    pub view_tokens: u64,
    pub history_tokens: u64,
    /// Bytes of the whole history (the memory kept) and of what the next call sends.
    pub history_bytes: u64,
    pub view_bytes: u64,
    pub messages_in_view: usize,
    pub messages_total: usize,
    pub policy: String,
    pub note: Option<String>,
    /// What the policy is doing ("curator −38%", "curator idle") and why.
    pub label: String,
    pub detail: Option<String>,
}

/// What is known of the model in use (from the agent's `ModelLimits`).
/// The goal in force as the agent last told it.
#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub text: String,
    /// The continuation round in progress (0: none) and the most there are.
    pub round: u32,
    pub max: u32,
    pub state: GoalState,
}

impl Goal {
    /// The goal in one line: what the agent restated the person wants, or their own words.
    pub fn headline(&self) -> &str {
        let first = self.text.lines().next().unwrap_or_default();
        first.strip_prefix(nexo_core::agent::GOAL_BRIEF_HEAD).unwrap_or(first)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub pricing: Option<Pricing>,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Search {
    pub query: String,
    /// The bar is open and takes the keys.
    pub focused: bool,
    /// Absolute line indices of the matches.
    pub matches: Vec<usize>,
    pub current: usize,
    /// Group blocks whose hit stands for calls the fold leaves out (one hit per group).
    pub groups: Vec<usize>,
}

/// A block's render: the width, the line count (kept), and the lines (dropped when far from the view).
type Rendered = (usize, usize, Option<Vec<Line<'static>>>);

pub struct App {
    pub opts: TuiOptions,
    pub theme: Theme,
    pub commands: CommandTx,
    pub blocks: Vec<Block>,
    cache: Vec<Option<Rendered>>,
    /// Beside each block: its fold when it has none of its own, and its animations.
    looks: Vec<Look>,
    pub offsets: Vec<usize>,
    pub total_lines: usize,
    seen_lines: usize,
    pub scroll: usize,
    pub follow: bool,
    pub selected: Option<usize>,
    /// The side diff panel is open (only its key opens it).
    /// A block being kept in sight while it opens.
    reveal: Option<usize>,
    pub search: Option<Search>,
    pub input: Input,
    pub palette: Option<Palette>,
    /// The draft set aside while the commands window opened by its key has the box.
    pub kept_draft: Option<crate::input::Draft>,
    pub modal: Option<Modal>,
    /// `/diff`: the full-screen change viewer, when open.
    pub diffview: Option<crate::diffview::DiffView>,
    pending_permissions: VecDeque<PermissionRequest>,
    /// Questions from the model waiting behind the open modal.
    pending_questions: VecDeque<nexo_core::permission::Question>,
    pub busy: bool,
    pub turn_started: Option<Instant>,
    /// When the interface opened (the banner's mark lights up for a moment).
    pub opened: Instant,
    /// The gauge's fill as drawn, easing towards the real share (0..1).
    pub gauge_shown: f64,
    /// The cost as the bar shows it: it rolls up to `cost` like a meter.
    pub cost_shown: f64,
    /// Billed tokens as the bar shows them: they roll like the cost.
    pub tok_shown: u64,
    /// Counters, the turn-end glow and the toasts (see `motion` and `toast`).
    pub fx: Fx,
    pub toasts: crate::toast::Toasts,
    pub streamed_chars: usize,
    pub queued: usize,
    /// The text of each queued request, oldest first (the agent keeps the same order); may hold
    /// fewer than `queued` when the count came from the agent alone.
    pub queued_msgs: Vec<String>,
    /// The queued rows of the last frame, with the index of the request each one shows.
    pub queue_rows: Vec<(Rect, usize)>,
    /// The pieces of the keys row a click opens something from.
    pub hint_hits: Vec<(Rect, HintHit)>,
    /// The background jobs, as the agent last listed them.
    pub jobs: Vec<nexo_core::jobs::JobInfo>,
    /// The sessions that can be written to, as the mailbox last listed them (for `/msg`).
    pub peers: Vec<nexo_core::mail::PeerInfo>,
    mail_polled: Option<Instant>,
    /// What this session does, told to the `/panel` supervisor.
    pub nexo_src: nexo_agent::supervise::NexoSource,
    /// The `/panel` poller, started the first time the panel opens.
    pub panel: Option<std::sync::Arc<nexo_agent::supervise::Panel>>,
    /// Which sessions `/panel` lists (all, nexo only, external only); kept while this session runs.
    pub panel_filter: std::sync::Arc<std::sync::atomic::AtomicU8>,
    pub spinner: usize,
    pub session_usage: Usage,
    pub turn_usage: Usage,
    pub cost: f64,
    /// Input of each model call this session, for the usage window's sparkline.
    pub call_inputs: Vec<u64>,
    pub context: Option<ContextInfo>,
    pub provider: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub should_quit: bool,
    pub dirty: bool,
    pub force_clear: bool,
    /// Something glides, fades or pulses: the loop draws at ~60 fps while true.
    pub moving: bool,
    last_ctrl_c: Option<Instant>,
    last_esc: Option<Instant>,
    pub status: Option<(String, Instant)>,
    /// How serious the flash message is: it picks the colour and how long it stays.
    pub status_level: Level,
    /// The flash was clipped by the bar and its whole text already went to a toast.
    pub status_toasted: bool,
    pub transcript_area: Rect,
    pub show_reasoning: bool,
    /// A dim `HH:MM` on each message and reply (`/timestamps`).
    pub timestamps: bool,
    /// Code and files shown for what they mean (Markdown rendered, JSON indented, tables).
    pub interpreted: bool,
    pub fold_after: usize,
    pub keymap: crate::keymap::Keymap,
    files: Option<Vec<String>>,
    pub last_width: usize,
    /// Inline mode: the blocks before this index are already in the terminal's scrollback and take no room here.
    pub inline_flushed: usize,
    /// Picker rows of the last draw, for clicks: (rect, index in the ranked list).
    pub picker_rows: Vec<(Rect, usize)>,
    /// Palette rows of the last draw, for clicks.
    pub palette_rows: Vec<(Rect, usize)>,
    pub session_title: String,
    /// Sessions deleted from here, the last at the end: `/undelete` brings them back.
    pub trashed: Vec<String>,
    /// Memory of other sessions waiting to go with the next message: `(id, title, tokens)`.
    pub shared: Vec<(String, String, usize)>,
    /// The person's messages so far, `(index in the history, text)`, for the rewind picker.
    pub user_messages: Vec<(usize, String)>,
    pub selection: Option<Selection>,
    /// Where the left button went down (screen column, row), until it comes up.
    press: Option<(u16, u16)>,
    /// The last mouse position while dragging, so wheel scrolling can extend the selection.
    drag_pos: Option<(u16, u16)>,
    /// Files changed this session, newest first.
    pub changed: Vec<ChangedFile>,
    pub show_files: bool,
    /// `/context-live`: the map of the context in the side panel.
    pub show_live: bool,
    /// Each message of the history and how much of it the last call carried.
    pub map: Vec<ContextCell>,
    /// The curator's zones the cells are in (empty without the curator).
    pub map_zones: Vec<ContextZone>,
    /// Which cells changed with the last map, and when it came.
    pub map_changed: Vec<bool>,
    /// Where the output of each tool call stands for the last model call, by call id: the word at the
    /// right of its card.
    pub marks: std::collections::HashMap<String, CallMark>,
    /// The tokens of each tool call's output, by call id, to price the card's word.
    pub call_tokens: std::collections::HashMap<String, u64>,
    pub map_at: Option<Instant>,
    pub files_area: Rect,
    /// The "panel hidden" note was shown for the current shrink.
    pub panel_hint_shown: bool,
    /// The text cells of the editor at the last draw, and whether a drag started inside it.
    pub editor_area: Rect,
    pub editor_drag: bool,
    /// The first row of the draft shown in the box.
    pub editor_top: usize,
    /// The last click in the editor: when, where and how many in a row (a double click selects a word).
    editor_click: Option<(Instant, (usize, usize), u8)>,
    /// The screen row of the status bar at the last draw (a click there opens the context window).
    pub status_row: u16,
    /// The terminal's width at the last draw.
    pub term_width: u16,
    pub limits: Option<Limits>,
    /// The reasoning level of the model in use and the levels it takes.
    pub effort: Option<String>,
    pub effort_levels: Vec<String>,
    /// The spend limits and what was spent: (this session, today, session limit, day limit).
    pub budget: Option<(f64, f64, Option<f64>, Option<f64>)>,
    /// Input tokens of the last model call (the gauge's numerator).
    pub last_call_input: u64,
    /// Of that input, what the provider served from its cache.
    pub last_call_cached: u64,
    /// Model calls and tool calls by kind of the running turn, for the summary line.
    pub turn_tally: crate::render::summary::TurnTally,
    /// The session's cost when the turn started, so the usage line can show the turn's share.
    turn_cost_start: f64,
    pub branch: Option<String>,
    /// nexo runs as root: the title bar says so.
    pub root: bool,
    /// Commits `main` is ahead of this build, once the check found a newer nexo.
    pub update_behind: Option<u32>,
    branch_read: Option<Instant>,
    /// The system prompt as last shown (`/prompt`), the seed of the prompt file.
    pub system_prompt: Option<String>,
    /// `/prompt edit` asked before any prompt was seen: edit once it arrives.
    edit_after_prompt: bool,
    /// The run loop opens `$EDITOR` on this file when set.
    pub edit_request: Option<PathBuf>,
    /// Ctrl+G: the draft goes to `$EDITOR` and comes back.
    pub draft_edit_request: bool,
    /// Commands of one's own (`.nexo/commands/*.md`).
    pub custom_commands: Vec<crate::custom::CustomCommand>,
    /// A notification for the terminal (the bell), when a long turn ended.
    pub bell_request: Option<String>,
    pub mouse_on: bool,
    /// The run loop turns mouse capture on or off when set.
    pub mouse_request: Option<bool>,
    /// The theme before the picker started previewing (restored on Esc).
    theme_before_preview: Option<Theme>,
    /// Texts sent from this interface whose echo has not arrived yet (for the timestamps).
    pending_stamps: VecDeque<String>,
    /// The task list the model keeps (the latest `todo` output).
    pub todo: Vec<TodoItem>,
    /// The goal in force: its text, rounds and state.
    pub goal: Option<Goal>,
    /// The scrollbars of the last frame: the transcript's, and the one of the dialog on top.
    pub transcript_bar: Option<Bar>,
    pub modal_bar: Option<Bar>,
    /// The body line a dragged list (picker, setup) starts at, for [`Modal::set_scroll`].
    pub modal_list_first: usize,
    /// A scrollbar's thumb held by the mouse: dragging it scrolls, never selects.
    pub bar_drag: Option<scrollbar::Drag>,
    /// What is under the pointer (the last move), lit while it is there.
    pub hover: Hover,
    /// The first transcript line shown glides to `scroll` over a few frames.
    pub scroll_fx: Tween,
    /// When lines last arrived out of sight: the thumb and the pill beat a moment.
    pub unseen_at: Option<Instant>,
    unseen_before: usize,
    /// The last scroll came from the reader (wheel, keys, the bar), so it glides even
    /// when it is long.
    wished: bool,
    /// The "↓ new" pill of the last frame (a click on it goes to the end).
    pub pill: Rect,
    /// The pieces of the status bar a click opens something from.
    pub status_hits: Vec<(Rect, StatusHit)>,
    /// `[ui] status_command`: the script that draws the bar, its last good line, when it runs.
    pub status_cmd: Option<crate::status_cmd::StatusCommand>,
    /// The sessions read for the usage window's insights.
    pub insights: crate::insights::Scanner,
    /// Turns ended this run: the bar's script runs again after each.
    pub turns_ended: u64,
    /// When the palette and the dialog on top opened: they pop in.
    pub palette_at: Option<Instant>,
    pub modal_at: Option<Instant>,
    /// Whether the last frame had a palette, a dialog: what is new this frame pops.
    pub palette_was: bool,
    pub modal_was: bool,
    /// The size each dialog took when it opened, and the rectangle it was drawn at.
    pub dialog_size: crate::dialog_size::DialogSize,
}

/// A piece of the status bar that answers a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusHit {
    /// The model: `/model`.
    Model,
    /// The context gauge: `/context`.
    Gauge,
    /// The tokens billed and the cost: `/usage`.
    Cost,
}

/// A piece of the keys row that answers a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintHit {
    /// `todo d/t`: `/todo`.
    Todo,
    /// `⚙ N jobs`: `/jobs`.
    Jobs,
    /// `⊕ mem`: `/shared-memory list`.
    Mem,
    /// `◷ N queued`: `/queue`.
    Queued,
    /// The session id or title: `/resume`.
    Session,
}

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hover {
    #[default]
    Nothing,
    /// A scrollbar (its thumb grows and lights up).
    Bar(scrollbar::Target),
    /// The "↓ new" pill.
    Pill,
    Status(StatusHit),
    /// A piece of the keys row (todo, jobs, memory, queue, session).
    Hint(HintHit),
    /// A row of the palette, of a picker or of a question.
    Palette(usize),
    Picker(usize),
    /// The first line of a block that folds (a click there folds or opens it).
    Head(usize),
}

impl App {
    pub fn new(opts: TuiOptions, commands: CommandTx) -> App {
        let theme = Theme::pick(&opts.theme);
        let mut app = App {
            theme,
            commands,
            blocks: Vec::new(),
            cache: Vec::new(),
            looks: Vec::new(),
            offsets: Vec::new(),
            total_lines: 0,
            seen_lines: 0,
            scroll: 0,
            follow: true,
            selected: None,
            reveal: None,
            search: None,
            input: {
                let mut i = Input::new("");
                i.vim = opts.vim.then_some(crate::input::VimMode::Insert);
                i
            },
            palette: None,
            modal: None,
            diffview: None,
            pending_permissions: VecDeque::new(),
            pending_questions: VecDeque::new(),
            busy: false,
            turn_started: None,
            opened: Instant::now(),
            gauge_shown: 0.0,
            cost_shown: 0.0,
            tok_shown: 0,
            fx: Fx::default(),
            toasts: Default::default(),
            streamed_chars: 0,
            queued: 0,
            queued_msgs: Vec::new(),
            queue_rows: Vec::new(),
            hint_hits: Vec::new(),
            jobs: Vec::new(),
            peers: Vec::new(),
            mail_polled: None,
            nexo_src: nexo_agent::supervise::NexoSource::default(),
            panel: None,
            panel_filter: Default::default(),
            spinner: 0,
            session_usage: Usage::default(),
            turn_usage: Usage::default(),
            cost: 0.0,
            call_inputs: Vec::new(),
            context: None,
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            permission_mode: PermissionMode::Ask,
            session_id: opts.session_id.clone(),
            cwd: opts.cwd.clone(),
            should_quit: false,
            dirty: true,
            force_clear: false,
            moving: false,
            last_ctrl_c: None,
            last_esc: None,
            status: None,
            status_level: Level::Info,
            status_toasted: false,
            transcript_area: Rect::default(),
            show_reasoning: opts.show_reasoning,
            timestamps: opts.timestamps,
            interpreted: false,
            fold_after: opts.fold_after.max(1),
            keymap: crate::keymap::Keymap::from_config(&opts.keys).0,
            files: None,
            last_width: 80,
            inline_flushed: 0,
            picker_rows: Vec::new(),
            palette_rows: Vec::new(),
            session_title: String::new(),
            trashed: Vec::new(),
            shared: Vec::new(),
            user_messages: Vec::new(),
            selection: None,
            press: None,
            drag_pos: None,
            changed: Vec::new(),
            show_files: false,
            show_live: false,
            map: Vec::new(),
            map_zones: Vec::new(),
            map_changed: Vec::new(),
            marks: std::collections::HashMap::new(),
            call_tokens: std::collections::HashMap::new(),
            map_at: None,
            files_area: Rect::default(),
            panel_hint_shown: false,
            editor_area: Rect::default(),
            editor_drag: false,
            editor_top: 0,
            editor_click: None,
            status_row: 0,
            term_width: 80,
            limits: None,
            effort: None,
            effort_levels: Vec::new(),
            budget: None,
            last_call_input: 0,
            last_call_cached: 0,
            turn_tally: Default::default(),
            turn_cost_start: 0.0,
            branch: None,
            root: nexo_core::paths::is_root(),
            update_behind: None,
            branch_read: None,
            system_prompt: None,
            edit_after_prompt: false,
            edit_request: None,
            draft_edit_request: false,
            custom_commands: crate::custom::load(&opts.cwd),
            bell_request: None,
            mouse_on: opts.mouse,
            mouse_request: None,
            theme_before_preview: None,
            pending_stamps: VecDeque::new(),
            todo: Vec::new(),
            goal: None,
            transcript_bar: None,
            modal_bar: None,
            modal_list_first: 0,
            bar_drag: None,
            hover: Hover::Nothing,
            scroll_fx: Tween::new(0.0, anim::SCROLL_MS),
            unseen_at: None,
            unseen_before: 0,
            wished: false,
            pill: Rect::default(),
            status_hits: Vec::new(),
            status_cmd: crate::status_cmd::StatusCommand::new(&opts.status_command, &opts.cwd),
            insights: crate::insights::Scanner::default(),
            turns_ended: 0,
            palette_at: None,
            modal_at: None,
            palette_was: false,
            dialog_size: Default::default(),
            kept_draft: None,
            modal_was: false,
            opts,
        };
        app.input.set_placeholder_style(app.theme.dim());
        app.read_branch();
        app.push_welcome();
        for w in crate::keymap::Keymap::from_config(&app.opts.keys.clone()).1 {
            app.flash(&w);
            app.notice(Level::Warn, w);
        }
        app
    }

    fn welcome_block(&self) -> Block {
        Block::Welcome {
            provider: self.provider.clone(),
            model: self.model.clone(),
            cwd: crate::ui::short_path(&self.cwd.to_string_lossy()),
            branch: self.branch.clone(),
            permission_mode: self.permission_mode.as_str().to_string(),
        }
    }

    /// The banner of an empty transcript.
    fn push_welcome(&mut self) {
        if self.blocks.is_empty() {
            let b = self.welcome_block();
            self.blocks.push(b);
            self.cache.push(None);
            self.looks.push(Look::default());
            self.dirty = true;
        }
    }

    /// Keeps the banner's facts current while it is shown.
    fn refresh_welcome(&mut self) {
        if matches!(self.blocks.first(), Some(Block::Welcome { .. })) {
            self.blocks[0] = self.welcome_block();
            self.invalidate(0);
        }
    }

    pub fn has_welcome(&self) -> bool {
        matches!(self.blocks.first(), Some(Block::Welcome { .. }))
    }

    /// Takes the banner off the top (tests that measure a transcript from its first message).
    #[doc(hidden)]
    pub fn drop_banner(&mut self) {
        if self.has_welcome() {
            self.blocks.remove(0);
            self.cache.remove(0);
            self.looks.remove(0);
            self.selected = self.selected.and_then(|i| i.checked_sub(1));
        }
    }

    /// Nothing said yet: only the banner, and maybe notices under it. The banner itself
    /// stays at the top of the transcript and scrolls away with it.
    pub fn fresh(&self) -> bool {
        self.blocks.iter().all(|b| matches!(b, Block::Welcome { .. } | Block::Notice { .. }))
    }

    /// The banner is still coming in (its intro is drawn on the frame clock).
    pub fn intro_running(&self) -> bool {
        self.opts.animations
            && self.has_welcome()
            && self.now().saturating_duration_since(self.opened).as_millis() as u64
                <= crate::render::blocks::INTRO_MS + 100
    }

    /// `.git/HEAD` of the nearest repository, re-read every five seconds.
    fn read_branch(&mut self) {
        self.branch_read = Some(self.now());
        let b = git_branch(&self.cwd);
        if b != self.branch {
            self.branch = b;
            self.refresh_welcome();
            self.dirty = true;
        }
    }

    /// The context gauge: (tokens the last call sent, the window), when known.
    /// The last call's real input against the model's window; None when no
    /// window is known (a share of the history is not a limit and would alarm
    /// for nothing: the bar then shows `ctx_tokens` instead).
    pub fn gauge(&self) -> Option<(u64, u64)> {
        let den = self.limits.as_ref().and_then(|l| l.context_window).map(u64::from).filter(|d| *d > 0)?;
        let num = if self.last_call_input > 0 { self.last_call_input } else { self.context.as_ref()?.view_tokens };
        Some((num.min(den), den))
    }

    /// What the title bar's gauge draws: tokens in use, the window, and whether the
    /// window is a guess (the model never said it; most take at least this much).
    pub fn gauge_view(&self) -> (u64, u64, bool) {
        match self.gauge() {
            Some((n, d)) => (n, d, false),
            None => (self.ctx_tokens().unwrap_or(0).min(GUESS_WINDOW), GUESS_WINDOW, true),
        }
    }

    /// The last call by where its tokens stood (cached prefix, sent fresh) and what the view leaves
    /// out; None until a call reported its input.
    pub fn gauge_split(&self) -> Option<crate::render::gauge::Split> {
        if self.last_call_input == 0 {
            return None;
        }
        let cached = self.last_call_cached.min(self.last_call_input);
        let out = self.context.as_ref().map_or(0, |c| c.history_tokens.saturating_sub(c.view_tokens));
        Some(crate::render::gauge::Split { cached, fresh: self.last_call_input - cached, out })
    }

    /// Tokens the next call sends, for the bar when no window is known.
    pub fn ctx_tokens(&self) -> Option<u64> {
        if self.last_call_input > 0 {
            return Some(self.last_call_input);
        }
        self.context.as_ref().map(|c| c.view_tokens)
    }

    /// Forgets the transcript (a resume, a rewind, a new session).
    pub fn clear_transcript(&mut self) {
        self.blocks.clear();
        self.cache.clear();
        self.looks.clear();
        self.last_call_cached = 0;
        self.last_call_input = 0;
        self.call_inputs.clear();
        self.turn_cost_start = 0.0;
        self.pending_stamps.clear();
        self.offsets.clear();
        self.total_lines = 0;
        self.seen_lines = 0;
        self.scroll = 0;
        self.follow = true;
        self.selected = None;
        self.search = None;
        self.selection = None;
        self.press = None;
        self.drag_pos = None;
        self.user_messages.clear();
        self.changed.clear();
        self.todo.clear();
        self.session_usage = Usage::default();
        self.turn_usage = Usage::default();
        self.cost = 0.0;
        self.context = None;
        self.map.clear();
        self.map_zones.clear();
        self.map_changed.clear();
        self.marks.clear();
        self.call_tokens.clear();
        self.queued = 0;
        self.queued_msgs.clear();
        self.dirty = true;
        self.push_welcome();
    }

    // ------------------------------------------------------------ blocks

    pub fn push(&mut self, b: Block) -> usize {
        let is_call = self.opts.group_tools && matches!(b, Block::Tool { .. });
        self.blocks.push(b);
        self.cache.push(None);
        let born = self.opts.animations.then(|| self.now());
        self.looks.push(Look { born, ..Look::default() });
        self.dirty = true;
        let at = self.blocks.len() - 1;
        if is_call {
            return self.join_group(at);
        }
        at
    }

    /// The call just pushed at `at` joins the run of calls before it (thinking in between is part of
    /// the run): the second call of a run puts a group block in front of the first. Returns where the
    /// call is now.
    fn join_group(&mut self, at: usize) -> usize {
        let mut first = at;
        while first > 0 && matches!(self.blocks[first - 1], Block::Reasoning { .. }) {
            first -= 1;
        }
        let Some(prev) = first.checked_sub(1).filter(|p| matches!(self.blocks[*p], Block::Tool { .. })) else {
            return at;
        };
        if let Some(back) = self.looks[prev].group_back {
            let m = prev - back as usize;
            if let Block::Group { len, .. } = &mut self.blocks[m] {
                *len = at - m;
            }
            for k in first..=at {
                self.looks[k].group_back = Some((k - m) as u32);
            }
            self.cache[m] = None;
            return at;
        }
        // what the inline mode already wrote cannot take a block in front of it
        if prev < self.inline_flushed {
            return at;
        }
        let born = self.looks[prev].born;
        self.blocks.insert(prev, Block::Group { folded: true, len: at + 1 - prev });
        self.cache.insert(prev, None);
        self.looks.insert(prev, Look { born, ..Look::default() });
        for k in prev + 1..=at + 1 {
            self.looks[k].group_back = Some((k - prev) as u32);
        }
        // the indices at and after the new block moved up by one
        let shift = |i: &mut usize| {
            if *i >= prev {
                *i += 1;
            }
        };
        self.selected.iter_mut().for_each(shift);
        self.reveal.iter_mut().for_each(shift);
        if matches!(self.hover, Hover::Head(_)) {
            self.hover = Hover::Nothing;
        }
        at + 1
    }

    /// The group block that block `i` belongs to.
    fn group_of(&self, i: usize) -> Option<usize> {
        let back = self.looks.get(i)?.group_back?;
        i.checked_sub(back as usize).filter(|m| matches!(self.blocks.get(*m), Some(Block::Group { .. })))
    }

    /// Whether block `i` is left out of the transcript: a finished call of a folded group, which
    /// its group's line stands for. A call still running or waiting stays in sight.
    pub fn hidden(&self, i: usize) -> bool {
        match self.group_of(i) {
            Some(m) => {
                let kept = match &self.blocks[i] {
                    // the agreed plan and thinking the user opened stay in sight
                    Block::Tool { name, .. } => name == "exit_plan",
                    Block::Reasoning { folded, text, .. } => !*folded && !text.trim().is_empty(),
                    _ => false,
                };
                matches!(self.blocks[m], Block::Group { folded: true, .. }) && !self.blocks[i].is_live() && !kept
            }
            None => false,
        }
    }

    pub fn invalidate(&mut self, i: usize) {
        if let Some(c) = self.cache.get_mut(i) {
            *c = None;
        }
        // a group line is made from its calls
        if let Some(m) = self.group_of(i) {
            self.cache[m] = None;
        }
        self.dirty = true;
    }

    /// The price of the output of tool call `id` in the last request: its tokens at the cached or the
    /// full rate, from `Pricing::cost`. None unless the model's prices are all given (input, output,
    /// cache read), the mark says the output was sent whole, and its tokens are known.
    fn call_cost(&self, id: &str, m: CallMark) -> Option<f64> {
        let p = self.limits.as_ref()?.pricing.filter(|p| p.input > 0.0 && p.output > 0.0 && p.cache_read > 0.0)?;
        let tokens = *self.call_tokens.get(id)?;
        let usage = match m {
            CallMark::Cached => nexo_core::stream::Usage { cache_read: tokens, ..Default::default() },
            CallMark::Sent => nexo_core::stream::Usage { input: tokens, ..Default::default() },
            _ => return None,
        };
        Some(p.cost(&usage))
    }

    pub fn invalidate_all(&mut self) {
        self.cache.fill(None);
        self.dirty = true;
    }

    pub fn render_opts(&self) -> RenderOpts {
        let since = self.now().saturating_duration_since(self.opened).as_millis() as u64;
        RenderOpts {
            show_reasoning: self.show_reasoning,
            fold_after: self.fold_after,
            spinner: self.spinner,
            intro_ms: (self.opts.animations && since < crate::render::blocks::INTRO_MS).then_some(since),
            interpreted: self.interpreted,
            timestamps: self.timestamps,
        }
    }

    /// How many lines block `i` has at `width`; kept when its lines are evicted.
    pub fn height(&mut self, i: usize, width: usize) -> usize {
        match &self.cache[i] {
            Some((w, h, _)) if *w == width => *h,
            _ => self.lines(i, width).len(),
        }
    }

    /// Drops the lines of blocks more than three screens from the view, so a long session keeps
    /// only what it can show soon; the heights stay, and a block comes back when scrolled to.
    pub fn evict_far(&mut self, top: usize, height: usize) {
        let reach = 3 * height.max(1);
        let lo = top.min(self.scroll).saturating_sub(reach);
        let hi = top.max(self.scroll) + height + reach;
        let now = self.now();
        for i in 0..self.cache.len().min(self.offsets.len()) {
            let Some((_, h, lines @ Some(_))) = self.cache[i].as_mut() else { continue };
            let (start, end) = (self.offsets[i], self.offsets[i] + *h);
            let busy = self.blocks[i].is_live()
                || self.looks.get(i).is_some_and(|l| {
                    l.born.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= FADE_MS as u128 + 100)
                        || l.opened.is_some_and(|(t, _)| t.elapsed().as_millis() <= OPEN_MS as u128 + 100)
                });
            if (end < lo || start > hi) && !busy {
                *lines = None;
            }
        }
    }

    /// The lines of block `i` at `width`, rendered once per width.
    pub fn lines(&mut self, i: usize, width: usize) -> &[Line<'static>] {
        let fresh = match &self.cache[i] {
            Some((w, _, l)) => *w != width || l.is_none(),
            None => true,
        };
        if fresh {
            let indent = indent_of(&self.blocks[i], width);
            let mut lines = match &self.blocks[i] {
                // a group's line is made from the calls it stands for
                Block::Group { folded, len } => {
                    let end = (i + 1 + len).min(self.blocks.len());
                    let head = crate::render::group::header(
                        &self.blocks[i + 1..end],
                        *folded,
                        self.spinner,
                        (width - indent).max(8),
                        &self.theme,
                    );
                    vec![head]
                }
                b => b.render(width - indent, &self.theme, self.render_opts()),
            };
            crate::render::wrap::clean_lines(&mut lines);
            if let Block::Tool { id, output: Some(_), .. } = &self.blocks[i] {
                if let (Some(m), Some(head)) = (self.marks.get(id), lines.first_mut()) {
                    let cost = self.call_cost(id, *m);
                    mark_head(head, *m, cost, width - indent, &self.theme);
                }
            }
            if self.opts.animations && matches!(self.blocks[i], Block::Tool { .. } | Block::Group { .. }) {
                let now = self.now();
                let card = if self.blocks[i].is_live() {
                    Some(crate::motion::Card::Running {
                        now_ms: now.saturating_duration_since(self.opened).as_millis() as u64,
                    })
                } else {
                    self.looks.get(i).and_then(|l| l.done_at).map(|t| crate::motion::Card::Done {
                        since_ms: now.saturating_duration_since(t).as_millis() as u64,
                    })
                };
                if let Some(card) = card {
                    crate::motion::card_fx(&mut lines, card, &self.theme);
                }
            }
            if self.looks.get(i).is_some_and(|l| l.folded) && folds_alone(&self.blocks[i]) && lines.len() > 1 {
                let hidden = lines.len() - 1;
                lines.truncate(1);
                lines.push(Line::from(Span::styled(format!("  ▸ {hidden} more lines"), self.theme.dim())));
            }
            if indent > 0 {
                let margin = " ".repeat(indent);
                for l in &mut lines {
                    l.spans.insert(0, Span::raw(margin.clone()));
                }
            }
            self.cache[i] = Some((width, lines.len(), Some(lines)));
        }
        self.cache[i].as_ref().and_then(|(_, _, l)| l.as_deref()).unwrap_or(&[])
    }

    /// Recomputes offsets at `width`. A blank line follows every non-empty block, two come
    /// before a message of the person (a new turn), and none sits between the one-line steps
    /// of a turn, which read as a list.
    pub fn layout(&mut self, width: usize) {
        // a new width re-wraps every line: keep the block and the line inside it at the top of the
        // view, so the text the person was reading stays under their eyes
        let anchor = (width != self.last_width && !self.follow && !self.offsets.is_empty()).then(|| {
            let bi = match self.offsets.binary_search(&self.scroll) {
                Ok(i) => i,
                Err(i) => i.saturating_sub(1),
            };
            (bi, self.scroll.saturating_sub(self.offsets[bi]))
        });
        self.last_width = width;
        // a call that finished inside a folded group leaves the view: its group takes the selection
        if let Some(m) = self.selected.filter(|s| self.hidden(*s)).and_then(|s| self.group_of(s)) {
            self.selected = Some(m);
        }
        let mut off = 0;
        self.offsets.clear();
        for i in 0..self.blocks.len() {
            self.offsets.push(off);
            if i < self.inline_flushed {
                continue;
            }
            let n = self.shown(i, width);
            off += n + self.gap_after(i, n);
        }
        self.total_lines = off;
        if let Some((bi, j)) = anchor.filter(|(bi, _)| *bi < self.offsets.len()) {
            let end = self.offsets.get(bi + 1).copied().unwrap_or(self.total_lines);
            let top = self.offsets[bi];
            self.scroll = (top + j).min(end.saturating_sub(1).max(top));
            // the view lands at once: no glide across a re-wrapped text
            self.scroll_fx.snap(self.scroll as f64);
        }
    }

    /// Blank lines after block `i`, which shows `n` lines.
    pub(crate) fn gap_after(&self, i: usize, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // the banner is its own frame: one blank line under it is enough
        if matches!(self.blocks[i], Block::Welcome { .. }) {
            return 1;
        }
        // the calls a folded group leaves out are not there to be next to
        let mut at = i + 1;
        while self.hidden(at) {
            at += 1;
        }
        match self.blocks.get(at) {
            Some(Block::User { .. }) => 2,
            Some(next) if n == 1 && is_step(&self.blocks[i]) && is_step(next) => 0,
            _ => 1,
        }
    }

    pub fn max_scroll(&self, height: usize) -> usize {
        self.total_lines.saturating_sub(height)
    }

    /// Settles the scroll after a layout: follows the end, or counts what arrived.
    pub fn settle(&mut self, height: usize) {
        if self.follow {
            self.scroll = self.max_scroll(height);
            self.seen_lines = self.total_lines;
        } else {
            self.scroll = self.scroll.min(self.max_scroll(height));
        }
    }

    pub fn unseen(&self) -> usize {
        if self.follow {
            0
        } else {
            self.total_lines.saturating_sub(self.seen_lines)
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let h = self.transcript_area.height as usize;
        let max = self.max_scroll(h);
        let next = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
        self.scroll = next;
        self.follow = next >= max;
        if self.follow {
            self.seen_lines = self.total_lines;
        }
        self.wished = true;
        self.dirty = true;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.follow = self.total_lines <= self.transcript_area.height as usize;
        self.wished = true;
        self.dirty = true;
    }

    pub fn scroll_to_end(&mut self) {
        self.follow = true;
        self.seen_lines = self.total_lines;
        self.wished = true;
        self.dirty = true;
    }

    /// The block that holds absolute line `line`.
    pub fn block_at_line(&self, line: usize) -> Option<usize> {
        if self.offsets.is_empty() || line >= self.total_lines {
            return None;
        }
        // blocks that show nothing share their offset with the next one: the last of them is the one
        // that holds the line
        let i = self.offsets.partition_point(|o| *o <= line).checked_sub(1)?;
        if self.hidden(i) {
            return None;
        }
        // the blank separator after a block belongs to nobody
        let n = self.cache.get(i).and_then(|c| c.as_ref()).map(|(_, h, _)| *h).unwrap_or(0);
        if line < self.offsets[i] + n {
            Some(i)
        } else {
            None
        }
    }

    /// How many of block `i`'s lines are in sight: all, except while it is opening.
    pub fn shown(&mut self, i: usize, width: usize) -> usize {
        if self.hidden(i) {
            return 0;
        }
        let n = self.height(i, width);
        match self.looks.get(i).and_then(|l| l.opened) {
            Some((at, from)) if from < n => {
                let t = (at.elapsed().as_millis() as f32 / OPEN_MS as f32).min(1.0);
                let eased = 1.0 - (1.0 - t).powi(3);
                from + ((n - from) as f32 * eased).ceil() as usize
            }
            _ => n,
        }
    }

    /// How far block `i` has faded in (1 once it is settled).
    pub fn fade(&self, i: usize) -> f32 {
        match self.looks.get(i).and_then(|l| l.born) {
            Some(at) => (self.now().saturating_duration_since(at).as_millis() as f32 / FADE_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    /// Whether block `i` folds: cards and thinking always, anything else of more than one line.
    pub fn foldable(&mut self, i: usize) -> bool {
        let Some(b) = self.blocks.get(i) else { return false };
        if b.is_foldable() {
            return true;
        }
        if !folds_alone(b) {
            return false;
        }
        self.looks[i].folded || self.lines(i, self.last_width).len() > 1
    }

    pub fn toggle_block(&mut self, i: usize) {
        if !self.foldable(i) {
            self.selected = Some(i);
            return;
        }
        if matches!(self.blocks[i], Block::Group { .. }) {
            return self.toggle_group(i);
        }
        let before = self.lines(i, self.last_width).len();
        if !self.blocks[i].toggle_fold() {
            self.looks[i].folded = !self.looks[i].folded;
        }
        self.looks[i].opened = self.opts.animations.then(|| (self.now(), before));
        self.invalidate(i);
        self.selected = Some(i);
    }

    /// A group of calls opens, or folds back to its line. Opening shows the calls it had left out,
    /// growing in; folding keeps the view inside the transcript and the selection on the group.
    fn toggle_group(&mut self, m: usize) {
        let Block::Group { folded, len } = self.blocks[m] else { return };
        let end = (m + len + 1).min(self.blocks.len());
        let top = self.offsets.get(m).copied();
        let old_end = self.offsets.get(end).copied().unwrap_or(self.total_lines);
        let was_hidden: Vec<usize> = (m + 1..end).filter(|k| self.hidden(*k)).collect();
        self.blocks[m].toggle_fold();
        self.invalidate(m);
        if folded {
            if self.opts.animations {
                let now = self.now();
                for k in was_hidden {
                    self.looks[k].opened = Some((now, 0));
                }
            }
        } else if !self.follow {
            // the lines under the view may be gone: the view lands on the group instead of past it
            if let Some(top) = top.filter(|t| self.scroll > *t && self.scroll < old_end) {
                self.scroll = top;
                self.scroll_fx.snap(top as f64);
            }
        }
        self.selected = Some(m);
        self.dirty = true;
    }

    pub fn fold_all(&mut self) {
        // groups, tool cards and thinking fold together: one key for every body
        let any_open = self.blocks.iter().any(|b| {
            matches!(
                b,
                Block::Tool { folded: false, .. }
                    | Block::Reasoning { folded: false, .. }
                    | Block::Group { folded: false, .. }
            )
        });
        for (i, b) in self.blocks.iter_mut().enumerate() {
            if matches!(b, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. }) {
                b.set_fold(any_open);
                self.cache[i] = None;
            }
        }
        self.dirty = true;
        self.flash(if any_open {
            "tool groups, tool and thinking bodies folded"
        } else {
            "tool groups, tool and thinking bodies unfolded"
        });
    }

    pub fn toggle_reasoning(&mut self) {
        self.show_reasoning = !self.show_reasoning;
        self.invalidate_all();
        self.flash(if self.show_reasoning { "reasoning shown" } else { "reasoning hidden" });
    }

    pub fn toggle_interpreted(&mut self) {
        self.interpreted = !self.interpreted;
        self.invalidate_all();
        self.flash(if self.interpreted {
            "code shown for what it means: markdown rendered, json indented, tables aligned"
        } else {
            "code shown as source"
        });
    }

    /// Shift+Tab with no card selected: ask → accept-edits → plan → ask.
    pub fn cycle_mode(&mut self) {
        let next = match self.permission_mode {
            PermissionMode::Ask => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            _ => PermissionMode::Ask,
        };
        self.send(AgentCommand::SetPermissionMode(next));
    }

    /// Tab / Shift+Tab: the next or previous block that folds, round the transcript; with
    /// none selected, the last one.
    pub fn select_next(&mut self, forward: bool) {
        let n = self.blocks.len();
        if n == 0 {
            return;
        }
        let Some(mut i) = self.selected else {
            self.select_last();
            return;
        };
        for _ in 0..n {
            i = if forward { (i + 1) % n } else { (i + n - 1) % n };
            if self.navigable(i) {
                self.select(i);
                return;
            }
        }
    }

    /// A block the keys stop at: one that folds and shows something.
    pub fn navigable(&mut self, i: usize) -> bool {
        !self.hidden(i) && self.foldable(i) && !self.lines(i, self.last_width).is_empty()
    }

    fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.scroll_into_view(i);
        self.dirty = true;
    }

    /// Shift+↑ (or Tab) on an empty draft: the last block, where the eye already is.
    pub fn select_last(&mut self) {
        match (0..self.blocks.len()).rev().find(|&i| self.navigable(i)) {
            Some(i) => self.select(i),
            None => self.flash_level(Level::Warn, "nothing to select yet"),
        }
    }

    /// ↑ / ↓ with a block selected: the previous or next one. ↑ stops at the first; ↓ past
    /// the last leaves the blocks for the draft and follows the end again.
    pub fn select_step(&mut self, down: bool) {
        let Some(mut i) = self.selected else {
            if !down {
                self.select_last();
            }
            return;
        };
        loop {
            if down {
                if i + 1 >= self.blocks.len() {
                    self.selected = None;
                    self.scroll_to_end();
                    return;
                }
                i += 1;
            } else if i == 0 {
                self.dirty = true;
                return;
            } else {
                i -= 1;
            }
            if self.navigable(i) {
                return self.select(i);
            }
        }
    }

    /// → / ← with a block selected: open it or fold it (nothing when it already is).
    pub fn open_selected(&mut self, open: bool) {
        let Some(i) = self.selected else { return };
        if i < self.blocks.len() && self.foldable(i) && self.is_folded(i) == open {
            self.toggle_block(i);
        }
        self.scroll_into_view(i);
    }

    /// Whether block `i` shows only its head.
    pub fn is_folded(&self, i: usize) -> bool {
        match self.blocks.get(i) {
            Some(Block::Tool { folded, .. } | Block::Reasoning { folded, .. } | Block::Group { folded, .. }) => *folded,
            Some(_) => self.looks.get(i).is_some_and(|l| l.folded),
            None => false,
        }
    }

    /// Keeps block `i` in sight: its whole height when it fits, its head first when not.
    /// The draw keeps at it while the block grows open.
    fn scroll_into_view(&mut self, i: usize) {
        self.reveal = Some(i);
        let w = self.last_width;
        self.layout(w);
        self.reveal_step();
    }

    /// One step of [`scroll_into_view`]: done once the block stopped growing.
    pub fn reveal_step(&mut self) {
        let Some(i) = self.reveal else { return };
        let h = self.transcript_area.height as usize;
        let w = self.last_width;
        let (Some(&off), true) = (self.offsets.get(i), i < self.blocks.len()) else {
            self.reveal = None;
            return;
        };
        let shown = self.shown(i, w);
        let full = self.lines(i, w).len();
        if h > 0 {
            let max = self.max_scroll(h);
            let mut s = self.scroll;
            if off + shown.max(1) > s + h {
                s = off + shown.max(1) - h;
            }
            s = s.min(off).min(max);
            if s != self.scroll || (self.follow && s < max) {
                self.scroll = s;
                self.follow = s >= max;
                self.dirty = true;
            }
        }
        if shown >= full {
            self.reveal = None;
        }
    }

    // ------------------------------------------------------------ search

    pub fn open_search(&mut self) {
        let q = self.search.as_ref().map(|s| s.query.clone()).unwrap_or_default();
        self.search = Some(Search { query: q, focused: true, matches: Vec::new(), current: 0, groups: Vec::new() });
        self.rematch();
        self.dirty = true;
    }

    pub fn close_search(&mut self) {
        self.search = None;
        self.dirty = true;
    }

    pub fn rematch(&mut self) {
        let width = self.last_width;
        let Some(q) = self.search.as_ref().map(|s| s.query.to_lowercase()) else { return };
        let mut matches = Vec::new();
        let mut groups = Vec::new();
        if !q.is_empty() {
            for i in 0..self.blocks.len() {
                let off = self.offsets.get(i).copied().unwrap_or(0);
                let hidden = self.hidden(i);
                let hit_lines: Vec<usize> = self
                    .lines(i, width)
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| crate::render::markdown::text_of(l).to_lowercase().contains(&q))
                    .map(|(j, _)| j)
                    .collect();
                if hidden {
                    // a call the fold leaves out is found at its group's line, once per group
                    if let (false, Some(m)) = (hit_lines.is_empty(), self.group_of(i)) {
                        if !groups.contains(&m) {
                            groups.push(m);
                            matches.push(self.offsets.get(m).copied().unwrap_or(0));
                        }
                    }
                } else {
                    matches.extend(hit_lines.into_iter().map(|j| off + j));
                }
            }
            matches.sort_unstable();
            matches.dedup();
        }
        if let Some(s) = &mut self.search {
            s.matches = matches;
            s.groups = groups;
            s.current = s.current.min(s.matches.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    pub fn search_jump(&mut self, forward: bool) {
        let h = self.transcript_area.height as usize;
        let Some(s) = &mut self.search else { return };
        if s.matches.is_empty() {
            return;
        }
        let n = s.matches.len();
        s.current = if forward { (s.current + 1) % n } else { (s.current + n - 1) % n };
        let line = s.matches[s.current];
        let line = self.open_hit(line);
        self.scroll = line.saturating_sub(h / 3).min(self.max_scroll(h));
        self.follow = false;
        self.dirty = true;
    }

    /// A hit that stands for the calls of a folded group opens the group; the line to go to is the
    /// first one of those calls that matches.
    fn open_hit(&mut self, line: usize) -> usize {
        let Some(m) = self
            .search
            .as_ref()
            .and_then(|s| s.groups.iter().copied().find(|m| self.offsets.get(*m) == Some(&line)))
            .filter(|m| matches!(self.blocks[*m], Block::Group { folded: true, .. }))
        else {
            return line;
        };
        self.toggle_group(m);
        self.layout(self.last_width);
        self.rematch();
        let Some(s) = &mut self.search else { return line };
        // the first hit at or after the old line (the header itself may match too)
        s.current = s.matches.iter().position(|l| *l >= line).unwrap_or(s.matches.len().saturating_sub(1));
        s.matches.get(s.current).copied().unwrap_or(line)
    }

    /// Jumps to the first match without moving the counter (on typing).
    pub fn search_first(&mut self) {
        let h = self.transcript_area.height as usize;
        if let Some(line) = self.search.as_ref().and_then(|s| s.matches.get(s.current).copied()) {
            let line = self.open_hit(line);
            {
                self.scroll = line.saturating_sub(h / 3).min(self.max_scroll(h));
                self.follow = false;
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ status
}

/// What `/init` asks the agent.
pub const INIT_PROMPT: &str = "Study this project and write (or update) AGENTS.md at its root: a short guide for an \
agent working here. Cover what the project is, how it is laid out, how to build, test, lint and run it (the exact \
commands, which you should check by running them), the conventions the code follows, and anything surprising. \
Keep it under 150 lines; prefer facts you verified to guesses.";

/// How long a flash message stays; it fades out over its last `STATUS_FADE_MS`.
pub const STATUS_MS: u64 = 4000;
pub const STATUS_FADE_MS: u64 = 900;
/// An error stays longer: it is not a confirmation to miss.
pub const STATUS_ERR_MS: u64 = 8000;

/// How long a flash of this level stays.
pub fn status_life(level: Level) -> u64 {
    if level == Level::Error {
        STATUS_ERR_MS
    } else {
        STATUS_MS
    }
}

impl App {
    pub fn flash(&mut self, text: &str) {
        self.flash_level(Level::Info, text);
    }

    /// A flash message that says how serious it is (warn and error are coloured; error stays longer).
    pub fn flash_level(&mut self, level: Level, text: &str) {
        self.status = Some((text.to_string(), self.now()));
        self.status_level = level;
        self.status_toasted = false;
        self.dirty = true;
    }

    /// A newer nexo is on `main`: a card in the conversation, a toast, and a badge that stays
    /// in the title bar until nexo restarts.
    pub fn update_ready(&mut self, behind: u32) {
        if behind == 0 || self.update_behind.is_some() {
            return;
        }
        self.update_behind = Some(behind);
        self.push(Block::Update { behind });
        self.toast(crate::toast::Kind::Info, "nexo update ready: run `nexo update`");
    }

    pub fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.push(Block::Notice { level, text: text.into() });
    }

    /// Reads the sessions for the usage window's insights when its tab is on show, and takes the result.
    pub fn poll_insights(&mut self) {
        if matches!(&self.modal, Some(Modal::Info(i)) if i.tab == 1) {
            self.insights.ensure();
        }
        if self.insights.poll() {
            self.dirty = true;
        }
    }

    /// Runs the bar's script (`[ui] status_command`) when a turn ended or the model changed,
    /// at most once a second, and takes its line when it comes.
    pub fn poll_status_cmd(&mut self) {
        let Some(mut sc) = self.status_cmd.take() else { return };
        let key = format!(
            "{}/{}|{}|{}",
            self.provider,
            self.model,
            self.turns_ended,
            self.session_id.as_deref().unwrap_or("")
        );
        if sc.poll(&key, || self.status_json()) {
            self.dirty = true;
        }
        self.status_cmd = Some(sc);
    }

    /// What the bar's script gets on stdin: one JSON line with what nexo knows of the session.
    pub fn status_json(&self) -> String {
        let (used, window) = match self.gauge() {
            Some((n, d)) => (n, Some(d)),
            None => (self.ctx_tokens().unwrap_or(0), None),
        };
        let percent = window.map(|d| (used as f64 * 100.0 / d as f64).round() as u64);
        let u = &self.session_usage;
        let v = serde_json::json!({
            "session_id": self.session_id,
            "cwd": self.cwd,
            "branch": self.branch,
            "model": { "id": self.model, "provider": self.provider, "effort": self.effort },
            "permission_mode": self.permission_mode.as_str(),
            "context": { "used_tokens": used, "window": window, "percent": percent },
            "tokens": {
                "input": u.input,
                "output": u.output,
                "cache_read": u.cache_read,
                "cache_write": u.cache_write,
                "total": u.total_input() + u.output,
            },
            "cost": { "usd": self.cost, "limit_usd": self.budget.and_then(|(_, _, l, _)| l) },
            "busy": self.busy,
            "turns": self.turns_ended,
            "version": nexo_core::version::get(),
        });
        format!("{v}\n")
    }

    /// The poller behind `/panel`, started on first use.
    fn panel_handle(&mut self) -> std::sync::Arc<nexo_agent::supervise::Panel> {
        if let Some(p) = &self.panel {
            return p.clone();
        }
        use nexo_agent::supervise::{run::Setup, Policy};
        let user = nexo_core::paths::config_dir().join("supervisor.toml");
        let project = std::path::Path::new(".nexo/supervisor.toml");
        let loaded = Policy::load(Some(&user), Some(project));
        let mut notes: Vec<String> = loaded.errors.iter().map(|e| e.to_string()).collect();
        let policy = loaded.policy;
        let model = policy.model.clone();
        let setup = Setup::detect(&policy);
        let started = nexo_agent::supervise::Panel::start_with(
            &setup,
            policy.clone(),
            self.nexo_src.clone(),
            &crate::supervise_options(),
        )
        .unwrap_or_else(|_| nexo_agent::supervise::Panel::start(&setup, policy, self.nexo_src.clone()));
        if let Some(m) = &model {
            match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    let client = nexo_agent::supervise::model::default_client();
                    match nexo_agent::supervise::model::judge_for(m, &self.opts.providers, client, rt) {
                        Ok(j) => started.set_judge(j),
                        Err(e) => notes.push(e),
                    }
                }
                Err(_) => notes.push("the model needs the async runtime, which is not running".into()),
            }
        }
        // the other sessions' mail (the ledgers and receipts on disk), only where this process has a mailbox
        if crate::mailbox().is_some() {
            started.add_source(Box::new(nexo_agent::supervise::source_mail::MailSource::new(
                nexo_core::mailbox::mail_root(),
            )));
        }
        started.set_notes(notes);
        let p = std::sync::Arc::new(started);
        self.panel = Some(p.clone());
        p
    }

    pub fn tick(&mut self) {
        self.tick_frame(true)
    }

    /// What the supervisor wants said to this session goes in as a message of its own, once no turn is running.
    fn deliver_supervisor_nudges(&mut self) {
        if self.busy || !self.nexo_src.has_nudges() {
            return;
        }
        for (_, text) in self.nexo_src.take_nudges() {
            self.send(AgentCommand::Submit { text, attachments: vec![] });
            self.flash("supervisor: message sent to this session");
        }
    }

    /// One tick of the clock; `beat` steps the spinner (the loop ticks faster while things glide).
    /// About once a second: the mailbox beats, takes in what arrived, and hands the agent the mails
    /// to read (only when it said it is idle); what it saw is shown like any other agent event.
    fn poll_mailbox(&mut self) {
        let Some(m) = crate::mailbox() else { return };
        let now = self.now();
        if self.mail_polled.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_millis(900)) {
            return;
        }
        self.mail_polled = Some(now);
        let t = m.tick();
        for ev in t.events {
            self.handle_agent(ev);
        }
        if !t.deliver.is_empty() {
            self.send(AgentCommand::Deliver { mails: t.deliver });
        }
    }

    pub fn tick_frame(&mut self, beat: bool) {
        if beat {
            self.spinner = self.spinner.wrapping_add(1);
        }
        self.deliver_supervisor_nudges();
        self.poll_status_cmd();
        self.poll_mailbox();
        self.poll_diffview();
        self.poll_insights();
        if !self.branch_read.is_some_and(|t| t.elapsed().as_secs() < 5) {
            self.read_branch();
        }
        self.sync_theme_preview();
        let mut any = false;
        let mut live = false;
        for i in 0..self.blocks.len() {
            if self.blocks[i].is_live() {
                self.cache[i] = None;
                live = true;
            }
        }
        // a block fades in as it arrives, and opens line by line
        let settle = FADE_MS.max(OPEN_MS) as u128 + 50;
        let now = self.now();
        if self.looks.iter().any(|l| {
            l.born.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= settle)
                || l.opened.is_some_and(|(t, _)| t.elapsed().as_millis() <= settle)
        }) {
            any = true;
        }
        // the banner's mark lights up during the first moments
        if self.intro_running() {
            self.cache[0] = None;
            any = true;
        }
        // cells of the context map that changed blink a moment
        if self.opts.animations
            && self.show_live
            && self.map_at.is_some_and(|t| t.elapsed().as_millis() <= MAP_FLASH_MS as u128 + 100)
        {
            any = true;
        }
        // the gauge eases towards the real share
        {
            let (num, den, _) = self.gauge_view();
            let target = (num as f64 / den.max(1) as f64).min(1.0);
            let d = target - self.gauge_shown;
            if d.abs() > 0.002 && self.opts.animations {
                self.gauge_shown += d * 0.25;
                any = true;
            } else {
                self.gauge_shown = target;
            }
        }
        // the cost rolls up to what was spent, like a meter
        // (and the tokens, the toasts and the turn-end glow)
        if self.tick_fx() {
            any = true;
        }
        // tool cards: a finished one pops and eases its rail
        {
            let now = self.now();
            let on = self.opts.animations;
            let settle = crate::motion::RAIL_MS.max(crate::motion::POP_MS) + 30;
            // a group is live while one of its calls is: its line shows the spinner and the clock
            let mut live_groups: Vec<usize> = Vec::new();
            for i in 0..self.blocks.len() {
                if self.blocks[i].is_live() {
                    if let Some(m) = self.group_of(i) {
                        if live_groups.last() != Some(&m) {
                            live_groups.push(m);
                        }
                        self.cache[m] = None;
                    }
                }
            }
            for i in 0..self.blocks.len() {
                if !matches!(self.blocks[i], Block::Tool { .. } | Block::Group { .. }) {
                    continue;
                }
                let live = match self.blocks[i] {
                    Block::Group { .. } => live_groups.contains(&i),
                    _ => self.blocks[i].is_live(),
                };
                let look = &mut self.looks[i];
                if look.was_live && !live && on {
                    look.done_at = Some(now);
                    self.cache[i] = None;
                }
                look.was_live = live;
                if live && on {
                    any = true;
                } else if crate::motion::within(look.done_at, now, settle) {
                    self.cache[i] = None;
                    any = true;
                } else if look.done_at.take().is_some() {
                    self.cache[i] = None;
                }
            }
        }
        // the panel: new rows from the poller, and breathing chips
        let at = self.now();
        if let Some(Modal::Agents(v)) = &mut self.modal {
            if v.refresh() || v.animating(at) {
                any = true;
                self.dirty = true;
            }
        }
        // the todo window: the bar fills, a task that changed glows, the one being worked on spins
        if let Some(Modal::Todo(v)) = &self.modal {
            let live = self.opts.animations && v.order().iter().any(|&i| v.item_running(i));
            if v.animating(at) || live {
                any = true;
                self.dirty = true;
            }
        }
        // the jobs window asks for fresh output now and then
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            if let Some(cmd) = v.refresh() {
                self.commands.send(cmd).ok();
            }
        }
        // the usage window grows open and its figures fill in
        if matches!(&self.modal, Some(Modal::Info(i)) if i.animating()) {
            any = true;
        }
        if self.insights.running() && matches!(&self.modal, Some(Modal::Info(_))) {
            any = true;
        }
        // the transcript glides, the thumb beats, a palette or a dialog pops open
        let now = self.now();
        if self.scroll_fx.moving(now)
            || self.unseen_at.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= anim::PULSE_MS as u128)
            || [self.palette_at, self.modal_at]
                .iter()
                .any(|t| t.is_some_and(|t| now.saturating_duration_since(t).as_millis() <= anim::POP_MS as u128 + 50))
        {
            any = true;
        }
        // the frame where a motion ends is drawn too, so nothing rests half lit
        if self.moving && !any {
            self.dirty = true;
        }
        self.moving = any;
        if any || live || self.busy {
            self.dirty = true;
        }
        if let Some((_, t)) = &self.status {
            if self.now().saturating_duration_since(*t).as_millis() >= status_life(self.status_level) as u128 {
                self.status = None;
                self.dirty = true;
            }
            // a message fades out: every frame of it is drawn (with animations off, only its end)
            if self.opts.animations {
                self.dirty = true;
            }
        }
    }

    /// What the screen asks of the clock now.
    pub fn load(&self) -> crate::frame::Load {
        use crate::frame::Load;
        // the banner's intro moves from the first frame, before any tick has set `moving`
        if self.moving || self.intro_running() {
            Load::Motion
        } else if self.busy
            || self.status.is_some()
            // the answer of the /diff collection is taken on a tick: an idle clock would hold it for seconds
            || self.diffview.as_ref().is_some_and(|v| v.loading)
            || self.tail_is_live()
        {
            Load::Beat
        } else {
            Load::Idle
        }
    }

    /// Whether a block of the current turn still moves by itself (PF3: only the
    /// tail after the last user message is scanned, never the whole session).
    fn tail_is_live(&self) -> bool {
        self.blocks.iter().rev().take_while(|b| !matches!(b, Block::User { .. })).any(|b| b.is_live())
    }

    /// What the agent is doing now, as the activity line words it.
    pub fn activity(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        if matches!(self.modal, Some(Modal::Permission(_))) {
            return Some(crate::render::phase::Phase::Waiting.label().into());
        }
        let live = self.blocks.iter().rev().find(|b| b.is_live());
        use crate::render::phase::{phase_of_tool, Phase};
        Some(
            match live {
                Some(Block::Tool { name, .. }) => phase_of_tool(name),
                Some(Block::Assistant { .. }) => Phase::Writing,
                _ => Phase::Thinking,
            }
            .label()
            .into(),
        )
    }

    pub fn send(&self, cmd: AgentCommand) {
        let _ = self.commands.send(cmd);
    }

    // ------------------------------------------------------------ agent events

    pub fn handle_agent(&mut self, ev: AgentEvent) {
        self.dirty = true;
        self.nexo_src.observe(&ev, nexo_agent::supervise::run::now_secs());
        match ev {
            AgentEvent::Ready { session_id, provider, model, cwd, permission_mode } => {
                self.session_id = Some(session_id);
                self.provider = provider;
                self.model = model;
                self.cwd = cwd;
                self.permission_mode = permission_mode;
                self.read_branch();
                self.refresh_welcome();
            }
            AgentEvent::History { index, message } => self.history(index, message),
            AgentEvent::TurnStart { .. } => {
                self.busy = true;
                self.turn_started = Some(self.now());
                self.streamed_chars = 0;
                self.turn_tally = Default::default();
                self.turn_usage = Usage::default();
                self.turn_cost_start = self.cost;
                self.queued = self.queued.saturating_sub(1);
                self.trim_queue();
            }
            AgentEvent::AssistantStart => {}
            AgentEvent::TextDelta { text } => {
                self.streamed_chars += text.len();
                self.close_reasoning();
                match self.blocks.last_mut() {
                    Some(Block::Assistant { text: t, streaming: true, .. }) => {
                        t.push_str(&text);
                        let i = self.blocks.len() - 1;
                        self.invalidate(i);
                    }
                    _ => {
                        let (at, started) = (Some(self.stamp_now()), Some(self.now()));
                        self.push(Block::Assistant { text, streaming: true, at, started, took_ms: None });
                    }
                }
            }
            AgentEvent::ReasoningDelta { text } => {
                self.streamed_chars += text.len();
                match self.blocks.last_mut() {
                    Some(Block::Reasoning { text: t, streaming: true, .. }) => {
                        t.push_str(&text);
                        let i = self.blocks.len() - 1;
                        self.invalidate(i);
                    }
                    _ => {
                        self.push(Block::Reasoning {
                            text,
                            streaming: true,
                            started: Some(self.now()),
                            elapsed_ms: None,
                            folded: true,
                        });
                    }
                }
            }
            AgentEvent::ToolCall { id, name, args, summary } => {
                self.close_streaming();
                // a replayed call already has its card from the message: give it the tool's summary
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { summary: s, .. } = &mut self.blocks[i] {
                        *s = summary;
                    }
                    self.invalidate(i);
                    return;
                }
                self.turn_tally.count_call(&name);
                // a plan stays open in the transcript: it is what was agreed
                let folded = name != "exit_plan";
                self.push(Block::Tool {
                    id,
                    name,
                    summary,
                    args,
                    status: ToolStatus::Pending,
                    progress: Vec::new(),
                    output: None,
                    started: None,
                    elapsed_ms: None,
                    folded,
                });
            }
            AgentEvent::ToolStart { id } => {
                if let Some(i) = self.tool_index(&id) {
                    let now = self.now();
                    if let Block::Tool { status, started, .. } = &mut self.blocks[i] {
                        *status = ToolStatus::Running;
                        *started = Some(now);
                    }
                    self.invalidate(i);
                }
            }
            AgentEvent::ToolProgress { id, line } => {
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { progress, .. } = &mut self.blocks[i] {
                        progress.push(line);
                        if progress.len() > MAX_PROGRESS {
                            progress.remove(0);
                        }
                    }
                    self.invalidate(i);
                }
            }
            AgentEvent::ToolDone { id, output, elapsed_ms } => {
                if let ToolDisplay::Todo { items } = &output.display {
                    self.todo = items.clone();
                    let at = self.now();
                    if let Some(Modal::Todo(v)) = &mut self.modal {
                        v.set_items(items.clone(), at);
                        self.dirty = true;
                    }
                }
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { status, output: o, elapsed_ms: e, .. } = &mut self.blocks[i] {
                        *status = status_of(&output);
                        *o = Some(output);
                        *e = (elapsed_ms > 0).then_some(elapsed_ms); // a replay has no timing
                    }
                    self.note_change(i);
                    self.invalidate(i);
                }
            }
            AgentEvent::AssistantDone { message } => {
                let streamed_text = matches!(self.blocks.last(), Some(Block::Assistant { .. }))
                    || self
                        .blocks
                        .iter()
                        .rev()
                        .take_while(|b| !matches!(b, Block::User { .. }))
                        .any(|b| matches!(b, Block::Assistant { .. }));
                self.close_streaming();
                if !streamed_text {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        let at = Some(self.stamp_now());
                        self.push(Block::Assistant { text, streaming: false, at, started: None, took_ms: None });
                    }
                }
            }
            AgentEvent::PermissionAsk { request } => {
                if self.modal.is_none() {
                    self.modal = Some(Modal::permission(request));
                } else {
                    self.pending_permissions.push_back(request);
                }
            }
            AgentEvent::Question { question } => {
                if self.modal.is_none() {
                    self.modal = Some(Modal::question(question));
                } else {
                    self.pending_questions.push_back(question);
                }
            }
            AgentEvent::QuestionResolved { id, .. } => {
                self.pending_questions.retain(|q| q.id != id);
                if matches!(&self.modal, Some(Modal::Question(a)) if a.q.id == id) {
                    self.modal = None;
                    self.next_permission();
                }
            }
            AgentEvent::PermissionResolved { id, decision } => {
                self.pending_permissions.retain(|r| r.id != id);
                if self.modal.as_ref().and_then(Modal::id) == Some(id.as_str()) {
                    self.modal = None;
                    self.next_permission();
                }
                if let Some(i) = self.tool_index(&id) {
                    if matches!(decision, PermissionDecision::Deny { .. }) {
                        if let Block::Tool { status, .. } = &mut self.blocks[i] {
                            *status = ToolStatus::Denied;
                        }
                        self.invalidate(i);
                    }
                }
            }
            AgentEvent::Usage { turn, call, session, cost_usd } => {
                // one event per model call: its input is what that call sent (the attempt that
                // answered), while the turn counts all it spent
                if call.total_input() > 0 {
                    self.last_call_input = call.total_input();
                    self.last_call_cached = call.cache_read;
                    self.call_inputs.push(call.total_input());
                }
                self.turn_tally.steps += 1;
                self.turn_usage += turn;
                self.session_usage = session;
                self.cost = cost_usd;
            }
            AgentEvent::Context {
                view_tokens,
                history_tokens,
                history_bytes,
                view_bytes,
                messages_in_view,
                messages_total,
                policy,
                note,
                label,
                detail,
            } => {
                // the view shrank while the history did not: the curator let something go
                if let Some(prev) = &self.context {
                    let dropped = prev.view_tokens.saturating_sub(view_tokens);
                    if dropped >= crate::motion::CUT_LEAST && history_tokens >= prev.history_tokens {
                        self.fx.cut = Some((dropped, self.now()));
                    }
                }
                self.context = Some(ContextInfo {
                    view_tokens,
                    history_tokens,
                    history_bytes,
                    view_bytes,
                    messages_in_view,
                    messages_total,
                    policy: crate::render::wrap::one_line(&policy),
                    note: note.map(|n| crate::render::wrap::one_line(&n)),
                    label: crate::render::wrap::one_line(&label),
                    detail,
                });
            }
            AgentEvent::ContextMap { cells, zones, calls, call_tokens } => {
                let marks: std::collections::HashMap<String, CallMark> = calls.into_iter().collect();
                let call_tokens: std::collections::HashMap<String, u64> = call_tokens.into_iter().collect();
                // a card whose word changed is drawn again
                for i in 0..self.blocks.len() {
                    if let Block::Tool { id, .. } = &self.blocks[i] {
                        if marks.get(id) != self.marks.get(id) || call_tokens.get(id) != self.call_tokens.get(id) {
                            self.cache[i] = None;
                            self.dirty = true;
                        }
                    }
                }
                self.marks = marks;
                self.call_tokens = call_tokens;
                self.map_changed = cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| self.map.get(i).is_none_or(|old| old.state != c.state))
                    .collect();
                self.map = cells;
                self.map_zones = zones;
                self.map_at = Some(self.now());
            }
            AgentEvent::Notice { level, text } => self.notice(level, text),
            AgentEvent::UpdateAvailable { behind } => self.update_ready(behind),
            AgentEvent::ModelChanged { provider, model } => {
                self.flash(&format!("model: {provider}/{model}"));
                self.provider = provider;
                self.model = model;
                self.limits = None;
                self.refresh_welcome();
            }
            AgentEvent::PermissionModeChanged { mode } => {
                if mode != self.permission_mode && self.opts.animations {
                    self.fx.mode_from = Some(self.permission_mode);
                    self.fx.mode_at = Some(self.now());
                }
                self.permission_mode = mode;
                match mode {
                    PermissionMode::Plan => {
                        self.flash("plan mode: reads only, then a plan to approve · Shift+Tab leaves")
                    }
                    _ => self.flash(&format!("permissions: {}", mode.as_str())),
                }
                self.refresh_welcome();
            }
            AgentEvent::ShellOutput { command, output, exit_code } => {
                self.push(Block::Shell { command, output, exit_code });
            }
            AgentEvent::ModelLimits { provider, model, pricing, context_window, max_output, source } => {
                self.provider = provider.clone();
                self.model = model.clone();
                self.limits = Some(Limits { pricing, context_window, max_output, source: source.clone() });
                self.invalidate_all();
                self.refresh_welcome();
                // the card is worth a line after a change or a question, not at every start
                if !self.fresh() || source == "error" {
                    self.push(Block::Model { provider, model, pricing, context_window, max_output, source });
                }
            }
            AgentEvent::Budget { session_cost, day_cost, session_limit, day_limit } => {
                self.budget = Some((session_cost, day_cost, session_limit, day_limit));
            }
            AgentEvent::LocalServers { found } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.found_local(found);
                }
            }
            AgentEvent::LoginPrompt { url, code } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.login_prompt(url, code);
                }
            }
            AgentEvent::LoggedIn { result, .. } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    if let crate::modal::setup::SetupOut::Send(cmd) = s.logged_in(result) {
                        self.send(cmd);
                    }
                }
            }
            AgentEvent::Probed { result, .. } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.probed(result);
                }
            }
            AgentEvent::ModelChecked { check, .. } => {
                if let Some(Modal::Setup(s)) = &mut self.modal {
                    s.checked(check);
                }
            }
            AgentEvent::Effort { current, levels, .. } => {
                if self.effort != current && current.is_some() && self.effort_levels == levels {
                    self.flash(&format!("effort: {}", current.as_deref().unwrap_or("default")));
                }
                self.effort = current;
                self.effort_levels = levels;
            }
            AgentEvent::Goal { text, round, max, state } => {
                match (&text, &self.goal) {
                    (None, Some(_)) => self.flash("goal cleared"),
                    (Some(t), None) => self.flash(&format!("goal: {}", crate::render::wrap::truncate(t, 40))),
                    (Some(_), Some(g)) if g.state != state => self.flash(match &state {
                        GoalState::Active => "goal resumed",
                        GoalState::Waiting => "goal: waiting for your answer",
                        GoalState::Paused(_) => "goal paused · /goal resume",
                        GoalState::Done => "goal met ✓",
                    }),
                    (Some(_), Some(g)) if round > g.round => {
                        self.flash(&format!("goal: continuing, round {round}/{max}"))
                    }
                    _ => {}
                }
                self.goal = text.map(|text| Goal { text, round, max, state });
            }
            AgentEvent::SystemPrompt { text } => {
                self.system_prompt = Some(text.clone());
                if self.edit_after_prompt {
                    self.edit_after_prompt = false;
                    self.prompt_edit();
                } else {
                    self.push(Block::Shell { command: "system prompt".to_string(), output: text, exit_code: None });
                }
            }
            AgentEvent::TurnEnd { reason } => {
                self.close_streaming();
                self.busy = false;
                if matches!(reason, TurnEnd::Done) {
                    self.turn_glow();
                }
                self.turns_ended += 1;
                let elapsed_ms = self.turn_started.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
                // a long turn rings when it ends: the person may be in another window
                if self.opts.notify_after > 0 && elapsed_ms >= self.opts.notify_after * 1000 {
                    let what = match reason {
                        TurnEnd::Done => "done",
                        TurnEnd::Interrupted => "interrupted",
                        TurnEnd::Error => "stopped on an error",
                        TurnEnd::MaxTurns => "stopped at the turn limit",
                    };
                    self.bell_request = Some(format!("nexo: {what} after {}", fmt_ms(elapsed_ms)));
                }
                if !self.turn_usage.is_empty() {
                    let usage = self.turn_usage;
                    let cost_usd = (self.cost - self.turn_cost_start).max(0.0);
                    let tally = std::mem::take(&mut self.turn_tally);
                    self.push(Block::Usage { usage, cost_usd, elapsed_ms, tally });
                }
                match reason {
                    TurnEnd::Done => {}
                    TurnEnd::Interrupted => self.notice(Level::Warn, "interrupted"),
                    TurnEnd::Error => {}
                    TurnEnd::MaxTurns => {
                        self.notice(Level::Warn, "stopped: the turn limit was reached; send a message to go on")
                    }
                }
                for b in self.blocks.iter_mut() {
                    if let Block::Tool { status, .. } = b {
                        if matches!(status, ToolStatus::Pending | ToolStatus::Running) {
                            *status = ToolStatus::Failed;
                        }
                    }
                }
                self.invalidate_all();
            }
            AgentEvent::HistoryCleared => self.clear_transcript(),
            AgentEvent::SessionChanged { session_id, title } => {
                self.flash(&format!("session: {}", if title.is_empty() { session_id.clone() } else { title.clone() }));
                self.session_id = Some(session_id);
                self.session_title = title;
            }
            AgentEvent::SessionDeleted { id, title } => {
                self.opts.sessions.retain(|(i, _)| *i != id);
                let name = if title.is_empty() { id.clone() } else { title };
                self.flash(&format!("deleted {name} · /undelete brings it back"));
                self.trashed.push(id);
            }
            AgentEvent::SessionRestored { id, title } => {
                self.trashed.retain(|i| *i != id);
                let name = if title.is_empty() { id.clone() } else { title.clone() };
                if !self.opts.sessions.iter().any(|(i, _)| *i == id) {
                    self.opts.sessions.insert(0, (id, title));
                }
                self.flash(&format!("restored {name}"));
            }
            AgentEvent::SessionTree { entries } => {
                if entries.len() < 2 {
                    self.notice(Level::Info, "no forks yet: /fork starts one");
                } else {
                    let here = self.session_id.clone().unwrap_or_default();
                    let n = entries.len();
                    let items = entries
                        .into_iter()
                        .map(|(depth, id, title)| {
                            let mark = if id == here { " ◀ here" } else { "" };
                            let branch =
                                if depth == 0 { String::new() } else { format!("{}└ ", "  ".repeat(depth - 1)) };
                            PickItem { label: format!("{branch}{id}{mark}"), value: id, hint: title }
                        })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(
                        PickKind::Tree,
                        &format!("Sessions · {n} · Enter jumps"),
                        items,
                        None,
                    )));
                }
            }
            AgentEvent::Trash { sessions } => {
                if sessions.is_empty() {
                    self.notice(Level::Info, "the trash is empty");
                } else {
                    let n = sessions.len();
                    let items = sessions
                        .into_iter()
                        .map(|(id, title)| PickItem { label: id.clone(), value: id, hint: title })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(
                        PickKind::Trash,
                        &format!("Trash · {n} · Enter brings back"),
                        items,
                        None,
                    )));
                }
            }
            AgentEvent::MemoryShared { pending } => {
                if let Some((_, title, tok)) = pending.iter().find(|p| !self.shared.iter().any(|s| s.0 == p.0)) {
                    self.flash(&format!(
                        "⊕ {} · {} tok join your next message",
                        if title.is_empty() { "memory" } else { title },
                        crate::render::blocks::fmt_k(*tok as u64)
                    ));
                }
                self.shared = pending;
            }
            AgentEvent::Queued { pending } => {
                self.queued = pending;
                self.trim_queue();
            }
            AgentEvent::Jobs { jobs } => {
                for j in &jobs {
                    let was_running = self.jobs.iter().any(|o| o.id == j.id && o.status.running());
                    if was_running && !j.status.running() {
                        let first: String = j.label.lines().next().unwrap_or("").chars().take(40).collect();
                        self.flash(&format!("job {} {}: {first}", j.id, j.status.plain()));
                    }
                }
                if let Some(Modal::Jobs(v)) = &mut self.modal {
                    v.set_jobs(jobs.clone());
                    self.commands.send(AgentCommand::JobTails).ok();
                }
                self.jobs = jobs;
                self.dirty = true;
            }
            AgentEvent::JobTails { tails } => {
                if let Some(Modal::Jobs(v)) = &mut self.modal {
                    v.set_tails(tails);
                }
            }
            AgentEvent::Idle { .. } => {}
            AgentEvent::MailSent { id, to_name, to, summary, .. } => {
                let peer = if to_name.is_empty() { to } else { to_name };
                self.push(Block::Mail { id, outgoing: true, peer, text: summary, stage: nexo_core::mail::Stage::Sent });
            }
            AgentEvent::MailFailed { to, reason } => {
                self.notice(Level::Warn, format!("not sent to {to}: {reason}"));
                self.flash_level(Level::Warn, &format!("message not sent: {reason}"));
            }
            AgentEvent::MailArrived { id, from, from_name, summary, held } => {
                let peer = if from_name.is_empty() { from } else { from_name };
                let stage = if held { nexo_core::mail::Stage::Held } else { nexo_core::mail::Stage::Sent };
                self.push(Block::Mail { id, outgoing: false, peer: peer.clone(), text: summary, stage });
                self.flash(&format!("message from {peer}"));
            }
            AgentEvent::MailDelivered { ids } => {
                for id in ids {
                    self.mail_stage(&id, nexo_core::mail::Stage::Delivered);
                }
            }
            AgentEvent::MailRead { ids } => {
                for id in ids {
                    self.mail_stage(&id, nexo_core::mail::Stage::Read);
                }
            }
            AgentEvent::MailStage { id, stage } => self.mail_stage(&id, stage),
            AgentEvent::Peers { peers } => {
                self.peers = peers;
                self.dirty = true;
            }
            AgentEvent::Compacted { .. } | AgentEvent::Retrying { .. } | AgentEvent::TodoChanged { .. } => {}

            AgentEvent::Shutdown => self.should_quit = true,
        }
    }

    /// A message of the transcript moved on the receipt ladder (never back).
    fn mail_stage(&mut self, id: &str, to: nexo_core::mail::Stage) {
        for i in 0..self.blocks.len() {
            if let Block::Mail { id: bid, stage, .. } = &mut self.blocks[i] {
                if bid == id && stage.advances_to(to) {
                    *stage = to;
                    self.invalidate(i);
                }
            }
        }
    }

    fn history(&mut self, index: usize, m: Message) {
        match m.role {
            Role::User => {
                if m.is_tool_results() {
                    for p in &m.content {
                        if let Part::ToolResult { call_id, content, is_error, .. } = p {
                            if let Some(i) = self.tool_index(call_id) {
                                if let Block::Tool { output, status, .. } = &mut self.blocks[i] {
                                    if output.is_none() {
                                        let text = result_text(content);
                                        *status = if *is_error { ToolStatus::Failed } else { ToolStatus::Done };
                                        *output = Some(ToolOutput {
                                            content: text,
                                            is_error: *is_error,
                                            display: ToolDisplay::Plain,
                                        });
                                    }
                                }
                                self.note_change(i);
                                self.invalidate(i);
                            }
                        }
                    }
                } else {
                    let text = m.text();
                    self.user_messages.retain(|(i, _)| *i != index);
                    // the loop's own goal requests are not the person's: nothing to rewind to or repeat
                    if !text.starts_with(nexo_core::agent::GOAL_SET_MARK)
                        && text.trim() != nexo_core::agent::GOAL_CONTINUE_MARK
                    {
                        self.user_messages.push((index, text.clone()));
                    }
                    let at = match self.pending_stamps.iter().position(|t| *t == text) {
                        Some(i) => {
                            self.pending_stamps.remove(i);
                            Some(self.stamp_now())
                        }
                        None => None,
                    };
                    let pieces = crate::replay::split_user(&text);
                    if pieces.len() == 1 && matches!(&pieces[0], crate::replay::Piece::Text(t) if *t == text) {
                        self.push(Block::User { text, at });
                    } else {
                        let mut at = at;
                        for p in pieces {
                            use crate::replay::Piece;
                            match p {
                                Piece::JobNote(n) => {
                                    self.push(Block::User { text: n, at: None });
                                }
                                Piece::Memory { title, tokens } => {
                                    let title = if title.is_empty() { "memory" } else { title.as_str() };
                                    let tok = crate::render::blocks::fmt_k(tokens);
                                    self.push(Block::Notice {
                                        level: Level::Info,
                                        text: format!("⊕ memory from {title} · {tok} tok"),
                                    });
                                }
                                Piece::Summary(n) => {
                                    self.push(Block::Notice {
                                        level: Level::Info,
                                        text: format!("earlier conversation summarised · {n} lines"),
                                    });
                                }
                                Piece::Goal(words) => {
                                    let text = match words {
                                        Some(w) => format!("◎ goal set: {w}"),
                                        None => "◎ goal · keeps going".to_string(),
                                    };
                                    self.push(Block::Notice { level: Level::Info, text });
                                }
                                Piece::Text(t) => {
                                    self.push(Block::User { text: t, at: at.take() });
                                }
                            }
                        }
                    }
                }
            }
            Role::Assistant => {
                for p in m.content {
                    match p {
                        Part::Text { text } => {
                            if !text.trim().is_empty() {
                                self.push(Block::Assistant {
                                    text,
                                    streaming: false,
                                    at: None,
                                    started: None,
                                    took_ms: None,
                                });
                            }
                        }
                        Part::Reasoning { text, .. } => {
                            if !text.trim().is_empty() {
                                self.push(Block::Reasoning {
                                    text,
                                    streaming: false,
                                    started: None,
                                    elapsed_ms: None,
                                    folded: true,
                                });
                            }
                        }
                        Part::ToolCall { id, name, args } => {
                            let summary = summarise(&name, &args);
                            self.push(Block::Tool {
                                id,
                                name,
                                summary,
                                args,
                                status: ToolStatus::Pending,
                                progress: Vec::new(),
                                output: None,
                                started: None,
                                elapsed_ms: None,
                                folded: true,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Records what tool block `i` changed, for the changed-files panel: a diff
    /// display gives exact counts; a replayed result only its text (`edited
    /// path: +n −m lines`).
    fn note_change(&mut self, i: usize) {
        let Some(Block::Tool { name, args, output: Some(out), .. }) = self.blocks.get(i) else { return };
        match &out.display {
            ToolDisplay::Diff { path, old, new, .. } => {
                let (a, d) = diff::stats(old, new);
                let path = path.clone();
                self.upsert_change(path, a, d);
            }
            _ if !out.is_error && matches!(name.as_str(), "edit" | "write") => {
                let Some(path) =
                    args.get("path").or_else(|| args.get("file_path")).and_then(|v| v.as_str()).map(String::from)
                else {
                    return;
                };
                let (a, d) = parse_counts(&out.content).unwrap_or((0, 0));
                self.upsert_change(path, a, d);
            }
            _ => {}
        }
    }

    fn upsert_change(&mut self, path: String, added: usize, removed: usize) {
        let (mut a, mut d) = (added, removed);
        if let Some(pos) = self.changed.iter().position(|c| c.path == path) {
            let c = self.changed.remove(pos);
            a += c.added;
            d += c.removed;
        }
        self.changed.insert(0, ChangedFile { path, added: a, removed: d });
        self.dirty = true;
    }

    /// `/context-live`: the context map in the side panel.
    pub fn toggle_live(&mut self, term_width: u16) {
        if !self.show_live && term_width < FILES_PANEL_MIN_W {
            self.flash_level(Level::Warn, &format!("the context map needs {FILES_PANEL_MIN_W} columns"));
            return;
        }
        self.show_live = !self.show_live;
        if self.show_live && self.map.is_empty() {
            self.flash("the map fills in with the next model call");
        }
        self.dirty = true;
    }

    /// Ctrl+B: the changed-files panel (needs a wide enough terminal).
    pub fn toggle_files(&mut self, term_width: u16) {
        if !self.show_files && term_width < FILES_PANEL_MIN_W {
            self.flash_level(Level::Warn, &format!("the files panel needs {FILES_PANEL_MIN_W} columns"));
            return;
        }
        self.show_files = !self.show_files;
        self.dirty = true;
    }

    // ------------------------------------------------------------ selection

    /// The transcript position under screen `(x, y)`; rows above or below the
    /// transcript map to the lines just outside the view.
    pub fn pos_at(&self, x: u16, y: u16) -> Pos {
        let a = self.transcript_area;
        let top = self.view_top();
        let line = if y < a.y {
            top.saturating_sub(1)
        } else if y >= a.y + a.height {
            top + a.height as usize
        } else {
            top + (y - a.y) as usize
        };
        let line = line.min(self.total_lines.saturating_sub(1));
        let col = x.saturating_sub(a.x) as usize;
        Pos { line, col }
    }

    /// The left button went down at `(x, y)`.
    pub fn press_at(&mut self, x: u16, y: u16) {
        self.press = Some((x, y));
        if self.selection.as_ref().is_some_and(|s| !s.dragging) {
            self.selection = None;
            self.dirty = true;
        }
    }

    /// The pointer moved with the button down.
    pub fn drag_to(&mut self, x: u16, y: u16) {
        let Some((px, py)) = self.press else { return };
        if self.selection.is_none() {
            if !crate::modal::contains(self.transcript_area, px, py) {
                return;
            }
            let anchor = self.pos_at(px, py);
            self.selection = Some(Selection::new(anchor));
        }
        let a = self.transcript_area;
        if y < a.y {
            self.scroll_by(-1);
        } else if y >= a.y + a.height {
            self.scroll_by(1);
        }
        self.drag_pos = Some((x, y));
        let head = self.pos_at(x, y);
        if let Some(s) = &mut self.selection {
            s.head = head;
            s.dragging = true;
        }
        self.dirty = true;
    }

    /// After a wheel scroll while dragging, the head follows the pointer.
    pub fn extend_after_scroll(&mut self) {
        if let (Some((x, y)), true) = (self.drag_pos, self.selection.as_ref().is_some_and(|s| s.dragging)) {
            let head = self.pos_at(x, y);
            if let Some(s) = &mut self.selection {
                s.head = head;
            }
        }
    }

    /// A click on line `j` of block `i`: the first click selects it; a click on the head
    /// (or the `▸ more lines` under a folded one) of the selected block folds or opens it.
    pub fn click_block(&mut self, i: usize, j: usize) {
        let n = self.lines(i, self.last_width).len();
        let on_head = j == 0 || (self.is_folded(i) && j + 1 == n);
        if self.selected == Some(i) && on_head {
            self.toggle_block(i);
        } else {
            self.selected = Some(i);
        }
        self.dirty = true;
    }

    /// The button came up: a drag copies its selection, a click selects or toggles a block.
    pub fn release_at(&mut self, x: u16, y: u16) {
        let press = self.press.take();
        self.drag_pos = None;
        let dragged = self.selection.as_ref().is_some_and(|s| s.dragging && !s.is_empty());
        if let Some(s) = &mut self.selection {
            s.dragging = false;
        }
        if dragged {
            self.copy_selection();
            return;
        }
        self.selection = None;
        let Some((px, py)) = press else { return };
        let area = self.transcript_area;
        if crate::modal::contains(area, px, py) && crate::modal::contains(area, x, y) {
            let line = self.view_top() + (py - area.y) as usize;
            if let Some(i) = self.block_at_line(line) {
                self.click_block(i, line - self.offsets[i]);
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ pointer

    /// Notes a palette or a dialog that came up since the last frame: it pops in.
    pub fn note_popups(&mut self) {
        let now = self.now();
        let (p, m) = (self.palette.is_some(), self.modal.is_some());
        // a dialog that just came up starts from its own size, not the last one's
        if p && !self.palette_was {
            self.dialog_size.palette.forget();
        }
        if m && !self.modal_was {
            self.dialog_size.modal.forget();
        }
        if self.opts.animations {
            if p && !self.palette_was {
                self.palette_at = Some(now);
            }
            if m && !self.modal_was {
                self.modal_at = Some(now);
            }
        }
        if self.opts.animations && self.fx.ghost.is_some() {
            if (self.palette_was && !p) || (self.modal_was && !m) {
                self.fx.ghost_at = Some(now);
            } else if p || m {
                self.fx.ghost_at = None;
            }
        }
        if !p {
            self.fx.pal_sel.clear();
            self.dialog_size.palette.forget();
        }
        if !m {
            self.dialog_size.modal.forget();
        }
        if !m {
            self.fx.pick_sel.clear();
        }
        if !m {
            self.modal_bar = None;
            if self.bar_drag.is_some_and(|d| d.target == scrollbar::Target::Modal) {
                self.bar_drag = None;
            }
        }
        (self.palette_was, self.modal_was) = (p, m);
    }

    /// 0..1: how far the dialog on top has popped open.
    pub fn modal_pop(&self) -> f64 {
        anim::ease_out(anim::progress(self.modal_at, self.now(), anim::POP_MS))
    }

    /// 0..1: how far the palette has risen.
    pub fn palette_pop(&self) -> f64 {
        anim::ease_out(anim::progress(self.palette_at, self.now(), anim::POP_MS))
    }

    /// The first transcript line in sight: where the glide is now, else `scroll`.
    pub fn view_top(&self) -> usize {
        let now = self.now();
        let v = if self.scroll_fx.is_moving(now) {
            self.scroll_fx.value_at(now).round().max(0.0) as usize
        } else {
            self.scroll
        };
        v.min(self.max_scroll(self.transcript_area.height as usize))
    }

    /// One frame of the glide towards `scroll` (the draw calls it once the scroll is
    /// settled): the first line to show. A jump while following the end that is
    /// longer than the view (a session loaded, a resize) lands at once, and so does
    /// everything with the animations off.
    pub fn glide(&mut self, height: usize) -> usize {
        let now = self.now();
        let target = self.scroll as f64;
        let far = (self.scroll_fx.value_at(now) - target).abs() > height as f64;
        let wished = std::mem::take(&mut self.wished);
        if !self.opts.animations || (far && !wished) {
            self.scroll_fx.snap(target);
        } else {
            self.scroll_fx.retarget(target, now);
        }
        let unseen = self.unseen();
        if unseen > self.unseen_before && self.opts.animations {
            self.unseen_at = Some(now);
        }
        self.unseen_before = unseen;
        (self.scroll_fx.value_at(now).round().max(0.0) as usize).min(self.max_scroll(height))
    }

    /// Puts transcript line `off` at the top: gliding there, or at once (a dragged thumb
    /// follows the pointer one to one).
    pub fn scroll_to_line(&mut self, off: usize, glide: bool) {
        let max = self.max_scroll(self.transcript_area.height as usize);
        let off = off.min(max);
        self.scroll = off;
        self.follow = off >= max;
        if self.follow {
            self.seen_lines = self.total_lines;
        }
        if glide {
            self.wished = true;
        } else {
            self.scroll_fx.snap(off as f64);
        }
        self.dirty = true;
    }

    /// The bar a press or a drag goes to: the dialog's while one is open.
    fn live_bar(&self, target: Option<scrollbar::Target>) -> Option<Bar> {
        let bar = match target {
            Some(scrollbar::Target::Modal) => self.modal_bar,
            Some(scrollbar::Target::Transcript) => self.transcript_bar,
            None if self.modal.is_some() => self.modal_bar,
            None => self.transcript_bar,
        };
        bar.filter(|b| b.scrollable())
    }

    /// The left button went down at `(x, y)`: on a scrollbar, its thumb is held (a
    /// press on the track jumps there first). True when a bar took the press.
    pub fn bar_press(&mut self, x: u16, y: u16) -> bool {
        let Some(bar) = self.live_bar(None) else { return false };
        let Some((off, grab)) = bar.press(x, y) else { return false };
        let on_thumb = matches!(bar.hit(x, y), Some(Hit::Thumb { .. }));
        self.bar_drag = Some(scrollbar::Drag { target: bar.target, grab });
        self.press = None;
        if bar.target == scrollbar::Target::Transcript {
            self.selection = None;
        }
        self.set_bar_offset(bar, off, !on_thumb);
        self.dirty = true;
        true
    }

    /// The pointer moved with a thumb held: the scroll follows it. True when a thumb is held.
    pub fn bar_drag_to(&mut self, y: u16) -> bool {
        let Some(d) = self.bar_drag else { return false };
        if let Some(bar) = self.live_bar(Some(d.target)) {
            let off = bar.drag(y, d.grab);
            self.set_bar_offset(bar, off, false);
        }
        true
    }

    /// The button came up: a held thumb is let go. True when one was held.
    pub fn bar_release(&mut self) -> bool {
        if self.bar_drag.take().is_some() {
            self.dirty = true;
            return true;
        }
        false
    }

    fn set_bar_offset(&mut self, bar: Bar, off: usize, glide: bool) {
        match bar.target {
            scrollbar::Target::Transcript => self.scroll_to_line(off, glide),
            scrollbar::Target::Modal => {
                let first = self.modal_list_first;
                if let Some(m) = &mut self.modal {
                    m.set_scroll(off, bar.viewport, first);
                }
                self.sync_theme_preview();
                self.dirty = true;
            }
        }
    }

    /// Whether the scrollbar of `target` is lit: under the pointer or held.
    pub fn bar_hot(&self, target: scrollbar::Target) -> bool {
        self.hover == Hover::Bar(target) || self.bar_drag.is_some_and(|d| d.target == target)
    }

    /// What is under `(x, y)`, topmost first.
    pub fn hover_at(&mut self, x: u16, y: u16) -> Hover {
        use crate::modal::contains;
        if self.modal.is_some() {
            if self.modal_bar.is_some_and(|b| b.scrollable() && b.contains(x, y)) {
                return Hover::Bar(scrollbar::Target::Modal);
            }
            if let Some((_, i)) = self.picker_rows.iter().find(|(r, _)| contains(*r, x, y)) {
                return Hover::Picker(*i);
            }
            return Hover::Nothing;
        }
        if self.palette.is_some() {
            if let Some((_, i)) = self.palette_rows.iter().find(|(r, _)| contains(*r, x, y)) {
                return Hover::Palette(*i);
            }
        }
        if self.transcript_bar.is_some_and(|b| b.scrollable() && b.contains(x, y)) {
            return Hover::Bar(scrollbar::Target::Transcript);
        }
        if self.unseen() > 0 && contains(self.pill, x, y) {
            return Hover::Pill;
        }
        if let Some((_, h)) = self.status_hits.iter().find(|(r, _)| contains(*r, x, y)) {
            return Hover::Status(*h);
        }
        if let Some((_, h)) = self.hint_hits.iter().find(|(r, _)| contains(*r, x, y)) {
            return Hover::Hint(*h);
        }
        let a = self.transcript_area;
        if contains(a, x, y) {
            let line = self.view_top() + (y - a.y) as usize;
            if let Some(i) = self.block_at_line(line) {
                if line == self.offsets[i] && self.foldable(i) {
                    return Hover::Head(i);
                }
            }
        }
        Hover::Nothing
    }

    /// The pointer moved to `(x, y)` with no button down: what it is over lights up.
    pub fn pointer_moved(&mut self, x: u16, y: u16) {
        let h = self.hover_at(x, y);
        if h != self.hover {
            self.hover = h;
            self.dirty = true;
        }
    }

    /// The status bar's piece at column `x`, when it answers a click.
    pub fn status_hit(&self, x: u16) -> Option<StatusHit> {
        let y = self.status_row;
        self.status_hits.iter().find(|(r, _)| crate::modal::contains(*r, x, y)).map(|(_, h)| *h)
    }

    /// The plain text of absolute line `line` of the laid-out transcript.
    pub fn line_text(&mut self, line: usize) -> Option<String> {
        let i = self.block_at_line(line)?;
        let off = self.offsets[i];
        let width = self.last_width;
        self.lines(i, width).get(line - off).map(crate::render::markdown::text_of)
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.selection.clone() else { return };
        let (s, e) = sel.range();
        let mut texts = Vec::new();
        for line in s.line..=e.line {
            texts.push(self.line_text(line));
        }
        let text = sel.extract(|l| texts.get(l - s.line).cloned().flatten());
        let n = sel.lines();
        match crate::clipboard::copy(&text) {
            Ok(how) => self.flash_toast(&format!("copied {n} line{} ({how})", if n == 1 { "" } else { "s" })),
            Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
        }
    }

    /// A press in the editor's text: the cursor goes there and a drag selects.
    pub fn editor_press(&mut self, x: u16, y: u16) {
        let a = self.editor_area;
        let pos = crate::input::cell_at(
            &self.input,
            a.width as usize,
            self.editor_top + (y - a.y) as usize,
            (x - a.x) as usize,
        );
        let now = self.now();
        let n = match self.editor_click {
            Some((t, p, n))
                if now.duration_since(t) < Duration::from_millis(400) && p.0 == pos.0 && p.1.abs_diff(pos.1) <= 1 =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.editor_click = Some((now, pos, n));
        match n {
            2 => self.input.select_word(pos),
            3 => self.input.select_line(pos.0),
            _ => self.input.press_at(pos),
        }
        self.editor_drag = true;
        self.dirty = true;
    }

    pub fn editor_drag_to(&mut self, x: u16, y: u16) {
        let a = self.editor_area;
        let (x, y) = (x.clamp(a.x, a.x + a.width.saturating_sub(1)), y.clamp(a.y, a.y + a.height.saturating_sub(1)));
        let pos = crate::input::cell_at(
            &self.input,
            a.width as usize,
            self.editor_top + (y - a.y) as usize,
            (x - a.x) as usize,
        );
        self.input.drag_to(pos);
        self.dirty = true;
    }

    /// The button comes up after a drag in the editor: what it selected is copied.
    pub fn editor_release(&mut self) {
        self.editor_drag = false;
        if let Some(text) = self.input.release_selection() {
            let n = text.lines().count().max(1);
            match crate::clipboard::copy(&text) {
                Ok(how) => self.flash_toast(&format!("copied {n} line{} ({how})", if n == 1 { "" } else { "s" })),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            }
        }
        self.dirty = true;
    }

    /// Esc Esc or `/rewind`: pick an earlier message to go back to.
    pub fn open_rewind_picker(&mut self) {
        if self.busy {
            self.notice(Level::Info, "rewind when the turn has ended");
            return;
        }
        if self.user_messages.is_empty() {
            self.notice(Level::Info, "nothing to rewind to yet");
            return;
        }
        let items = self
            .user_messages
            .iter()
            .rev()
            .map(|(i, text)| PickItem {
                label: text.lines().next().unwrap_or("").to_string(),
                value: i.to_string(),
                hint: format!("#{i}"),
            })
            .collect();
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Rewind, "Rewind to before…", items, None)));
        self.dirty = true;
    }

    /// `/fork`: pick where the new session starts (an earlier message, or the whole conversation).
    pub fn open_fork_picker(&mut self) {
        if self.busy {
            self.notice(Level::Info, "fork when the turn has ended");
            return;
        }
        let mut items =
            vec![PickItem { label: "the whole conversation".into(), value: "all".into(), hint: String::new() }];
        items.extend(self.user_messages.iter().rev().map(|(i, text)| PickItem {
            label: text.lines().next().unwrap_or("").to_string(),
            value: i.to_string(),
            hint: format!("before #{i}"),
        }));
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Fork, "Fork from…", items, None)));
        self.dirty = true;
    }

    fn tool_index(&self, id: &str) -> Option<usize> {
        self.blocks.iter().rposition(|b| matches!(b, Block::Tool { id: i, .. } if i == id))
    }

    fn close_reasoning(&mut self) {
        for i in (0..self.blocks.len()).rev() {
            if let Block::Reasoning { streaming, started, elapsed_ms, .. } = &mut self.blocks[i] {
                if *streaming {
                    *streaming = false;
                    *elapsed_ms = started.map(|s| s.elapsed().as_millis() as u64);
                    self.cache[i] = None;
                }
                break;
            }
        }
    }

    fn close_streaming(&mut self) {
        self.close_reasoning();
        let now = self.now();
        for i in 0..self.blocks.len() {
            if let Block::Assistant { streaming, started, took_ms, .. } = &mut self.blocks[i] {
                if *streaming {
                    *streaming = false;
                    *took_ms = started.map(|s| now.saturating_duration_since(s).as_millis() as u64);
                    self.cache[i] = None;
                }
            }
        }
    }

    fn next_permission(&mut self) {
        if self.modal.is_none() {
            if let Some(r) = self.pending_permissions.pop_front() {
                self.modal = Some(Modal::permission(r));
            } else if let Some(q) = self.pending_questions.pop_front() {
                self.modal = Some(Modal::question(q));
            }
        }
    }

    // ------------------------------------------------------------ the person's actions

    /// Whether a paste goes somewhere now: the draft, or the model setup's field.
    pub fn takes_paste(&self) -> bool {
        match &self.modal {
            None => !self.search.as_ref().is_some_and(|s| s.focused),
            Some(m) => matches!(m, Modal::Setup(_)),
        }
    }

    /// Enter on the draft.
    pub fn submit(&mut self) {
        let text = self.input.full_text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            // Enter again with a request waiting: stop the turn so the queue runs now
            if self.busy && self.queued > 0 {
                self.send(AgentCommand::Interrupt);
                self.flash("interrupting: the queued request runs now");
                return;
            }
            if let Some(i) = self.selected {
                self.toggle_block(i);
            }
            return;
        }
        self.palette = None;
        // a command of one's own sends its text as the request
        if let Some(rest) = trimmed.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let name = name.to_ascii_lowercase();
            if let Some(c) = self.custom_commands.iter().find(|c| c.name == name).cloned() {
                self.input.push_history(trimmed);
                self.input.set_text(&c.expand(args));
                return self.submit();
            }
        }
        if trimmed.starts_with('/') {
            if let Some(cmd) = commands::parse(trimmed) {
                self.input.push_history(trimmed);
                self.input.clear();
                self.run_command(cmd);
                return;
            }
        }
        if let Some(cmd) = trimmed.strip_prefix('!') {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                self.input.push_history(trimmed);
                self.input.clear();
                self.send(AgentCommand::Shell { command: cmd.to_string() });
            }
            return;
        }
        self.input.push_history(trimmed);
        self.input.clear();
        let attachments = self.attachments_in(trimmed);
        if self.busy {
            self.queued += 1;
            self.queued_msgs.push(trimmed.to_string());
            self.flash("queued: joins the turn at its next step · Enter again to send it now");
        }
        self.pending_stamps.push_back(trimmed.to_string());
        if self.pending_stamps.len() > 20 {
            self.pending_stamps.pop_front();
        }
        self.send(AgentCommand::Submit { text: trimmed.to_string(), attachments });
        self.scroll_to_end();
    }

    /// The texts kept for the queue never outnumber the requests the agent says wait: what started
    /// (the oldest) leaves.
    fn trim_queue(&mut self) {
        let extra = self.queued_msgs.len().saturating_sub(self.queued);
        if extra > 0 {
            self.queued_msgs.drain(..extra);
        }
    }

    /// Take queued request `index` back out of the queue and into the draft, to edit it and send it
    /// again (the agent drops it from its own queue). Only into an empty draft.
    pub fn unqueue(&mut self, index: usize) {
        if index >= self.queued_msgs.len() {
            return;
        }
        if !self.input.is_empty() {
            self.flash("send or clear the draft first, then take the queued message back");
            return;
        }
        let text = self.queued_msgs.remove(index);
        self.queued = self.queued.saturating_sub(1);
        if let Some(i) = self.pending_stamps.iter().position(|t| *t == text) {
            self.pending_stamps.remove(i);
        }
        self.send(AgentCommand::Unqueue { index, text: text.clone() });
        self.input.set_text(&text);
        self.refresh_palette();
        self.dirty = true;
        self.flash("queued message back in the draft: edit it, Enter sends it again");
    }

    /// `/todo`: the task list in its own window, grouped by state.
    pub fn open_todo(&mut self) {
        self.modal = Some(Modal::Todo(Box::new(crate::todo_view::TodoView::new(self.todo.clone()))));
    }

    /// `/jobs`, a click on `⚙ N jobs`: the window with every background job and its output.
    pub fn open_jobs(&mut self) {
        self.modal = Some(Modal::Jobs(Box::new(crate::jobs_view::JobsView::new(self.jobs.clone()))));
        self.send(AgentCommand::JobTails);
    }

    /// `/queue`: the requests waiting for the turn to end; Enter takes one back into the draft.
    pub fn open_queue(&mut self) {
        if self.queued_msgs.is_empty() {
            return self.notice(Level::Info, "nothing is queued");
        }
        let items = self
            .queued_msgs
            .iter()
            .enumerate()
            .map(|(i, t)| PickItem {
                label: t.lines().next().unwrap_or("").to_string(),
                value: i.to_string(),
                hint: String::new(),
            })
            .collect();
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Queue, "Queued · Enter edits", items, None)));
    }

    /// `/shared-memory list`: the memory of other sessions that goes with the next message.
    pub fn open_shared(&mut self) {
        if self.shared.is_empty() {
            return self.notice(Level::Info, "no shared memory waits for the next message");
        }
        let items = self
            .shared
            .iter()
            .map(|(id, title, tok)| PickItem {
                label: if title.is_empty() { id.clone() } else { title.clone() },
                value: id.clone(),
                hint: format!("{id} · {} tok", crate::render::blocks::fmt_k(*tok as u64)),
            })
            .collect();
        self.modal =
            Some(Modal::Picker(Picker::new(PickKind::Shared, "Shared memory · joins your next message", items, None)));
    }

    /// A click on a piece of the keys row.
    pub fn open_hint(&mut self, hit: HintHit) {
        match hit {
            HintHit::Todo => self.open_todo(),
            HintHit::Jobs => self.run_command(Command::Jobs(None)),
            HintHit::Mem => self.open_shared(),
            HintHit::Queued => self.open_queue(),
            HintHit::Session => self.run_command(Command::Sessions),
        }
    }

    fn attachments_in(&self, text: &str) -> Vec<Attachment> {
        let mut out = Vec::new();
        for tok in text.split_whitespace() {
            if let Some(p) = tok.strip_prefix('@') {
                let p = p.trim_end_matches([',', '.', ';', ':', ')', '"', '\'']);
                if p.is_empty() {
                    continue;
                }
                let path = self.cwd.join(p);
                if path.is_file() && !out.iter().any(|a: &Attachment| a.path == path) {
                    out.push(Attachment { path });
                }
            }
        }
        out
    }

    pub fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::Help | Command::Keys => self.modal = Some(Modal::Help { scroll: 0 }),
            Command::Model(Some(name)) => {
                let (provider, model) = match name.split_once('/') {
                    Some((p, m)) => (Some(p.to_string()), m.to_string()),
                    None => (None, name),
                };
                self.send(AgentCommand::SetModel { provider, model });
            }
            Command::Model(None) | Command::Provider => self.open_setup(),
            Command::Effort(Some(level)) => {
                let level = level.trim().to_ascii_lowercase();
                let effort = (level != "default" && level != "reset").then_some(level);
                self.send(AgentCommand::SetEffort { effort });
            }
            Command::Effort(None) => self.open_effort_picker(),
            Command::Permissions(Some(mode)) => self.send(AgentCommand::SetPermissionMode(mode)),
            Command::Yolo => self.send(AgentCommand::SetPermissionMode(PermissionMode::Auto)),
            Command::Plan(arg) => match arg.as_deref().map(str::trim) {
                Some("off") | Some("exit") | Some("done") => {
                    self.send(AgentCommand::SetPermissionMode(PermissionMode::Ask));
                }
                Some(text) if !text.is_empty() => {
                    if self.permission_mode != PermissionMode::Plan {
                        self.send(AgentCommand::SetPermissionMode(PermissionMode::Plan));
                    }
                    self.input.set_text(text);
                    self.submit();
                }
                _ if self.permission_mode == PermissionMode::Plan => {
                    self.notice(Level::Info, "already in plan mode · /plan off or Shift+Tab leaves it")
                }
                _ => self.send(AgentCommand::SetPermissionMode(PermissionMode::Plan)),
            },
            Command::Prompt => self.send(AgentCommand::ShowPrompt),
            Command::PromptEdit => self.prompt_edit(),
            Command::Pricing(None) => self.send(AgentCommand::ShowModel),
            Command::Pricing(Some(args)) => match parse_pricing(&args) {
                Ok(pricing) => {
                    self.send(AgentCommand::SetPricing { pricing });
                    self.flash_toast("prices saved");
                }
                Err(e) => self.notice(Level::Warn, format!("/pricing: {e}")),
            },
            Command::Mouse => self.toggle_mouse(),
            Command::Timestamps => {
                self.timestamps = !self.timestamps;
                self.invalidate_all();
                self.flash(if self.timestamps { "timestamps on" } else { "timestamps off" });
            }
            Command::ContextLive => self.toggle_live(self.term_width),
            Command::Goal(GoalArg::Set(text)) => self.send(AgentCommand::SetGoal { text: Some(text) }),
            Command::Goal(GoalArg::Clear) => self.send(AgentCommand::SetGoal { text: None }),
            Command::Goal(GoalArg::Pause) => self.send(AgentCommand::PauseGoal),
            Command::Goal(GoalArg::Resume) => self.send(AgentCommand::ResumeGoal),
            Command::Goal(GoalArg::Show) => match &self.goal {
                Some(g) => {
                    let t = &g.text;
                    let text = match &g.state {
                        GoalState::Active if g.round > 0 => {
                            format!(
                                "goal: {t} (round {}/{}) · /goal pause holds it, /goal off clears it",
                                g.round, g.max
                            )
                        }
                        GoalState::Active => format!("goal: {t} · /goal pause holds it, /goal off clears it"),
                        GoalState::Waiting => {
                            format!("goal: {t} · waiting for your answer: your next message resumes it")
                        }
                        GoalState::Paused(why) => format!("goal: {t} · paused ({why}) · /goal resume goes on"),
                        GoalState::Done => format!("goal: {t} · met · /goal resume reopens it, /goal off clears it"),
                    };
                    self.notice(Level::Info, text)
                }
                None => self.notice(Level::Info, "no goal set: /goal <text> keeps the agent on it across turns"),
            },
            Command::Permissions(None) => {
                let items = [
                    PermissionMode::Ask,
                    PermissionMode::AcceptEdits,
                    PermissionMode::Plan,
                    PermissionMode::Auto,
                    PermissionMode::ReadOnly,
                ]
                .iter()
                .map(|m| PickItem {
                    label: m.as_str().to_string(),
                    value: m.as_str().to_string(),
                    hint: mode_hint(*m).to_string(),
                })
                .collect();
                let cur = self.permission_mode.as_str().to_string();
                self.modal =
                    Some(Modal::Picker(Picker::new(PickKind::PermissionMode, "Permission mode", items, Some(&cur))));
            }
            Command::Compact => {
                self.send(AgentCommand::Compact);
                self.flash("compacting…");
            }
            Command::Clear => {
                self.clear_transcript();
                self.send(AgentCommand::Clear);
            }
            Command::Rewind => self.open_rewind_picker(),
            Command::Fork => self.open_fork_picker(),
            Command::Tree => self.send(AgentCommand::ShowTree),
            Command::Undo => self.undo(),
            Command::Init => {
                self.input.set_text(INIT_PROMPT);
                self.submit();
            }
            Command::Status => self.send(AgentCommand::ShowStatus),
            Command::Tools => self.send(AgentCommand::ShowTools),
            Command::Jobs(None) => self.open_jobs(),
            Command::Jobs(Some(id)) => self.send(AgentCommand::KillJob { id }),
            Command::Budget(None) => self.send(AgentCommand::ShowBudget),
            Command::Budget(Some(arg)) => {
                let a = arg.trim().trim_start_matches('$').to_ascii_lowercase();
                match a.as_str() {
                    "off" | "none" | "0" => self.send(AgentCommand::SetBudget { limit: None }),
                    _ => match a.replace(',', ".").parse::<f64>() {
                        Ok(v) if v > 0.0 && v.is_finite() => self.send(AgentCommand::SetBudget { limit: Some(v) }),
                        _ => self.notice(
                            Level::Warn,
                            format!("/budget: '{arg}' is not an amount of dollars (e.g. /budget 1.50, /budget off)"),
                        ),
                    },
                }
            }
            Command::Vim => {
                let on = self.input.vim.is_none();
                self.input.vim = on.then_some(crate::input::VimMode::Insert);
                let _ = crate::config_edit::set_raw(
                    &nexo_core::paths::user_config_file(),
                    "ui",
                    "vim",
                    if on { "true" } else { "false" },
                );
                self.flash(if on {
                    "vim keys on (saved): Esc for normal mode, i/a/o to type"
                } else {
                    "vim keys off (saved)"
                });
            }
            Command::Export(path) => {
                self.send(AgentCommand::Export { path: path.map(PathBuf::from) });
                self.flash("exporting…");
            }
            Command::Msg(None) => {
                self.send(AgentCommand::ListPeers);
                self.input.set_text("/msg ");
                self.refresh_palette();
            }
            Command::Msg(Some(arg)) => match arg.trim().split_once(char::is_whitespace) {
                Some((to, text)) if !text.trim().is_empty() => {
                    self.send(AgentCommand::SendMail { to: to.to_string(), text: text.trim().to_string() })
                }
                _ => self.notice(Level::Info, "write to a session: /msg <session> <text>"),
            },
            Command::Rename(None) => self.notice(Level::Info, "give the session a name: /rename <name>"),
            Command::Rename(Some(title)) => self.send(AgentCommand::Rename { title }),
            Command::Diff => self.open_diffview(),
            Command::Resume(what) => {
                let w = what.trim();
                let found = self.opts.sessions.iter().find(|(id, _)| id == w).or_else(|| {
                    let mut by_title = self.opts.sessions.iter().filter(|(_, t)| t.eq_ignore_ascii_case(w));
                    by_title.next().filter(|_| by_title.next().is_none())
                });
                match found {
                    Some((id, _)) => {
                        let id = id.clone();
                        self.send(AgentCommand::Resume { id });
                    }
                    None => self.notice(Level::Warn, format!("/resume: no session '{w}' here · /resume lists them")),
                }
            }
            Command::Sessions => {
                if self.opts.sessions.is_empty() {
                    self.notice(Level::Info, "no earlier sessions here");
                } else {
                    let items = self
                        .opts
                        .sessions
                        .iter()
                        .map(|(id, title)| PickItem { label: id.clone(), value: id.clone(), hint: title.clone() })
                        .collect();
                    self.modal = Some(Modal::Picker(Picker::new(PickKind::Session, "Sessions", items, None)));
                }
            }
            Command::Mem(None) => {
                let here = self.session_id.clone().unwrap_or_default();
                let items: Vec<PickItem> = self
                    .opts
                    .sessions
                    .iter()
                    .filter(|(id, _)| *id != here)
                    .map(|(id, title)| PickItem { label: id.clone(), value: id.clone(), hint: title.clone() })
                    .collect();
                if items.is_empty() {
                    self.notice(Level::Info, "no other session to share from");
                } else {
                    self.modal = Some(Modal::Picker(Picker::new(PickKind::Memory, "Share memory from", items, None)));
                }
            }
            Command::Mem(Some(arg)) if arg == "list" => self.open_shared(),
            Command::Todo => self.open_todo(),
            Command::Queue => self.open_queue(),
            Command::Mem(Some(arg)) => {
                let (id, focus) = match arg.split_once(char::is_whitespace) {
                    Some((i, f)) => (i.to_string(), Some(f.trim().to_string()).filter(|f| !f.is_empty())),
                    None => (arg.clone(), None),
                };
                if id == "off" || id == "clear" {
                    self.send(AgentCommand::ShareMemory { id: None, focus: None });
                } else {
                    self.send(AgentCommand::ShareMemory { id: Some(id), focus });
                }
            }
            Command::Undelete(id) => match id.or_else(|| self.trashed.last().cloned()) {
                Some(id) => self.send(AgentCommand::RestoreSession { id }),
                // nothing deleted in this run: show what the trash holds
                None => self.send(AgentCommand::ListTrash),
            },
            Command::Panel => {
                let panel = self.panel_handle();
                let view = crate::agents_view::AgentsView::new(Some(panel), !self.fx_on())
                    .remembering(self.panel_filter.clone());
                self.modal = Some(Modal::Agents(Box::new(view)))
            }
            Command::Cost => {
                self.insights.invalidate();
                self.modal = Some(Modal::Info(Box::new(crate::overlay::Info::new(1).still(!self.opts.animations))))
            }
            Command::Context => {
                self.insights.invalidate();
                self.modal = Some(Modal::Info(Box::new(crate::overlay::Info::new(0).still(!self.opts.animations))))
            }
            Command::Theme(Some(name)) => self.set_theme(&name, true),
            Command::Theme(None) => {
                let items = crate::theme::NAMES
                    .iter()
                    .map(|n| PickItem {
                        label: n.to_string(),
                        value: n.to_string(),
                        hint: Theme::by_name(n).map(|t| t.description.to_string()).unwrap_or_default(),
                    })
                    .collect();
                let cur = self.theme.name.to_string();
                self.theme_before_preview = Some(self.theme.clone());
                self.modal = Some(Modal::Picker(Picker::new(PickKind::Theme, "Theme", items, Some(&cur))));
            }
            Command::Copy(what) => self.copy_last(what.as_deref() == Some("code")),
            Command::Quit => self.quit(),
            Command::Unknown(name) => self.notice(Level::Warn, format!("unknown command /{name} (try /help)")),
            Command::BadArgument(cmd, arg) => self.notice(Level::Warn, format!("/{cmd}: cannot read '{arg}'")),
        }
        self.dirty = true;
    }

    /// `/model`: the configured models to switch to, and the guided setup of a new one.
    fn open_setup(&mut self) {
        let cur = format!("{}/{}", self.provider, self.model);
        let setup = Setup::new(self.opts.providers.clone(), self.opts.models.clone(), &cur);
        self.modal = Some(Modal::Setup(Box::new(setup)));
        self.send(AgentCommand::ScanLocal);
    }

    /// `/effort`: the levels the model takes, the default first.
    fn open_effort_picker(&mut self) {
        let levels = if self.effort_levels.is_empty() {
            vec!["off".to_string(), "low".into(), "medium".into(), "high".into()]
        } else {
            self.effort_levels.clone()
        };
        let mut items = vec![PickItem {
            label: "default".into(),
            value: "default".into(),
            hint: "what the model does without being told".into(),
        }];
        let known = !self.effort_levels.is_empty();
        items.extend(levels.into_iter().map(|l| PickItem {
            hint: if known { String::new() } else { "not checked for this model (/model checks it)".into() },
            label: l.clone(),
            value: l,
        }));
        let cur = self.effort.clone().unwrap_or_else(|| "default".into());
        let title = format!("Effort · {}", self.model);
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Effort, &title, items, Some(&cur))));
    }

    /// Switches the colours; `persist` remembers the choice in the user's configuration.
    pub fn set_theme(&mut self, name: &str, persist: bool) {
        match Theme::by_name(name) {
            Some(t) => {
                let t = if self.theme.degraded || !crate::theme::truecolor_terminal() { t.downsampled() } else { t };
                let degraded = t.degraded;
                self.apply_theme(t);
                if degraded {
                    self.notice(
                        Level::Info,
                        "this terminal reports no 24-bit colour: the theme is mapped to the 256-colour cube",
                    );
                }
                let name = self.theme.name;
                if persist {
                    match crate::config_edit::set_string(&nexo_core::paths::user_config_file(), "ui", "theme", name) {
                        Ok(()) => self.flash_toast(&format!("theme: {name} (saved)")),
                        Err(e) => {
                            self.flash_level(Level::Error, &format!("theme: {name} (not saved: {e})"));
                            self.toast(crate::toast::Kind::Error, &format!("theme not saved: {e}"));
                        }
                    }
                } else {
                    self.flash(&format!("theme: {name}"));
                }
            }
            None => self.notice(Level::Warn, format!("no theme '{name}' ({})", crate::theme::NAMES.join(", "))),
        }
    }

    fn apply_theme(&mut self, t: Theme) {
        if self.theme == t {
            return;
        }
        self.theme = t;
        self.input.set_placeholder_style(self.theme.dim());
        self.invalidate_all();
    }

    /// While the theme picker is open, the transcript shows the hovered theme.
    pub fn sync_theme_preview(&mut self) {
        let Some(Modal::Picker(p)) = &self.modal else { return };
        if p.kind != PickKind::Theme {
            return;
        }
        if let Some(t) = p.current().and_then(|it| Theme::by_name(&it.value)) {
            if self.theme_before_preview.is_none() {
                self.theme_before_preview = Some(self.theme.clone());
            }
            let t = if self.theme.degraded { t.downsampled() } else { t };
            self.apply_theme(t);
        }
    }

    /// Esc on the theme picker: back to the theme before the preview.
    fn end_theme_preview(&mut self, keep: bool) {
        if let Some(t) = self.theme_before_preview.take() {
            if !keep {
                self.apply_theme(t);
            }
        }
    }

    /// `/prompt edit`: the prompt file opens in the editor (the run loop does the opening).
    /// Ctrl+R: the requests sent before, newest first, to search and take one back.
    pub fn open_history_search(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let items: Vec<PickItem> = self
            .input
            .history()
            .iter()
            .rev()
            .filter(|h| seen.insert(h.as_str()))
            .map(|h| PickItem {
                label: h.lines().next().unwrap_or("").chars().take(120).collect(),
                value: h.clone(),
                hint: if h.lines().count() > 1 { format!("+{} lines", h.lines().count() - 1) } else { String::new() },
            })
            .collect();
        if items.is_empty() {
            self.flash_level(Level::Warn, "no requests sent yet");
            return;
        }
        self.modal = Some(Modal::Picker(Picker::new(PickKind::History, "Earlier requests", items, None)));
    }

    /// Ctrl+V: an image in the clipboard is kept as a PNG and attached (`@path` in the draft);
    /// text is pasted as text.
    pub fn paste_clipboard(&mut self) {
        match crate::clipboard::paste() {
            Ok(crate::clipboard::Pasted::Image(png)) => self.attach_png(&png),
            Ok(crate::clipboard::Pasted::Text(t)) => {
                self.input.insert_paste(&t);
                self.refresh_palette();
            }
            Err(e) => self.flash_level(Level::Error, &format!("nothing to paste: {e}")),
        }
    }

    /// Keeps `png` and puts `@path` in the draft, so it goes as an image with the next request.
    pub fn attach_png(&mut self, png: &[u8]) {
        match crate::clipboard::save_pasted(png) {
            Ok(path) => {
                let sep = if self.input.text().is_empty() || self.input.text().ends_with(' ') { "" } else { " " };
                self.input.insert_paste(&format!("{sep}@{} ", path.display()));
                self.flash(&format!("image pasted ({} KB): it goes with the next request", png.len() / 1024));
            }
            Err(e) => self.notice(Level::Warn, format!("the image could not be kept: {e}")),
        }
    }

    /// `/undo`: back to before the last request, its files put back, its text in the editor.
    pub fn undo(&mut self) {
        if self.busy {
            self.notice(Level::Warn, "/undo waits for the turn to end (Esc interrupts it)");
            return;
        }
        match self.user_messages.last().cloned() {
            Some((index, text)) => {
                self.send(AgentCommand::Rewind { index });
                self.input.set_text(&text);
                self.flash("undone: the request is back in the editor");
            }
            None => self.flash_level(Level::Warn, "nothing to undo"),
        }
    }

    pub fn prompt_edit(&mut self) {
        let path = nexo_core::paths::prompt_file();
        if !path.is_file() {
            let Some(text) = self.system_prompt.clone() else {
                self.edit_after_prompt = true;
                self.send(AgentCommand::ShowPrompt);
                return;
            };
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            // only the base text: nexo appends the directory, date and tools itself
            let text = nexo_core::paths::prompt_base(&text);
            if let Err(e) = std::fs::write(&path, text.to_string() + "\n") {
                self.notice(Level::Error, format!("cannot write {}: {e}", path.display()));
                return;
            }
        }
        self.edit_request = Some(path);
    }

    /// `/mouse`: capture on or off (off lets the terminal select text by itself).
    pub fn toggle_mouse(&mut self) {
        self.mouse_on = !self.mouse_on;
        self.mouse_request = Some(self.mouse_on);
        self.flash(if self.mouse_on {
            "mouse on: wheel scrolls, drag copies"
        } else {
            "mouse off: the terminal's own selection works; /mouse to enable"
        });
    }

    /// Copies the last reply, or with `code` the last code block of the replies.
    pub fn copy_last(&mut self, code: bool) {
        let text = self.blocks.iter().rev().find_map(|b| match b {
            Block::Assistant { text, .. } if code => last_code_block(text),
            Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        });
        match text {
            Some(t) => match crate::clipboard::copy(&t) {
                Ok(how) => self.flash_toast(&format!("copied ({how})")),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            },
            None if code => self.flash("no code block to copy yet"),
            None => self.flash_level(Level::Warn, "nothing to copy yet"),
        }
    }

    pub fn quit(&mut self) {
        self.keep_draft();
        self.send(AgentCommand::Shutdown);
        self.should_quit = true;
    }

    /// Puts an unsent draft in the history, so ↑ finds it after a clear or the next run.
    fn keep_draft(&mut self) {
        let text = self.input.full_text();
        if !text.trim().is_empty() {
            self.input.push_history(&text);
        }
    }

    /// The terminal tab's title: the session's name, with a spinner frame while a turn runs.
    pub fn terminal_title(&self) -> String {
        let name = match self.session_title.trim() {
            "" => "nexo".to_string(),
            t => format!("nexo · {t}"),
        };
        if self.busy {
            let frames = crate::render::blocks::SPINNER;
            format!("{} {name}", frames[(self.spinner / 2) % frames.len()])
        } else {
            name
        }
    }

    /// Ctrl+C: interrupt a turn, else clear the draft; twice within two seconds: quit.
    pub fn ctrl_c(&mut self) {
        let now = self.now();
        if self.last_ctrl_c.is_some_and(|t| now.duration_since(t).as_secs_f32() < 2.0) {
            self.quit();
            return;
        }
        self.last_ctrl_c = Some(now);
        if self.busy {
            self.send(AgentCommand::Interrupt);
            self.flash("interrupting… (Ctrl+C again to quit)");
        } else if !self.input.is_empty() {
            self.keep_draft();
            self.input.clear();
            self.palette = None;
            self.dirty = true;
            self.flash("draft cleared, ↑ brings it back (Ctrl+C again to quit)");
        } else {
            self.flash("Ctrl+C again to quit");
        }
    }

    /// Esc: close what is open, interrupt a turn, or clear the draft on a second press.
    pub fn esc(&mut self) {
        if self.selection.take().is_some() {
            self.press = None;
            self.drag_pos = None;
            self.dirty = true;
            return;
        }
        if self.palette.take().is_some() {
            self.dirty = true;
            return;
        }
        if self.search.as_ref().is_some_and(|s| s.focused) {
            self.close_search();
            return;
        }
        // a selected block is let go before anything else (the turn keeps running)
        if self.selected.take().is_some() {
            self.dirty = true;
            return;
        }
        let now = self.now();
        let again = self.last_esc.is_some_and(|t| now.duration_since(t).as_secs_f32() < 1.5);
        self.last_esc = Some(now);
        if self.busy && !again {
            self.send(AgentCommand::Interrupt);
            self.flash("interrupting… (Esc again clears the draft)");
            return;
        }
        if self.search.is_some() {
            self.close_search();
            return;
        }
        if !self.input.is_empty() {
            if again {
                self.input.clear();
                self.flash("draft cleared");
            } else {
                self.flash("Esc again to clear the draft");
            }
        } else if again && !self.busy && !self.user_messages.is_empty() {
            self.open_rewind_picker();
        } else {
            self.selected = None;
            if !self.busy && !self.user_messages.is_empty() {
                self.flash("Esc again to rewind to an earlier message");
            }
        }
        self.dirty = true;
    }

    pub fn modal_out(&mut self, out: ModalOut) {
        match out {
            ModalOut::Nothing => self.sync_theme_preview(),
            ModalOut::Close => {
                self.end_theme_preview(false);
                self.modal = None;
                self.next_permission();
            }
            ModalOut::Permission { id, decision } => {
                if let Some(i) = self.tool_index(&id) {
                    if matches!(decision, PermissionDecision::Deny { .. }) {
                        if let Block::Tool { status, .. } = &mut self.blocks[i] {
                            *status = ToolStatus::Denied;
                        }
                        self.invalidate(i);
                    }
                }
                self.send(AgentCommand::Permission { id, decision });
                self.modal = None;
                self.next_permission();
            }
            ModalOut::Send(cmd) => self.send(cmd),
            ModalOut::Copy(text) => match crate::clipboard::copy(&text) {
                Ok(how) => self.flash_toast(&format!("copied ({how})")),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            },
            ModalOut::SendAndClose(cmd) => {
                self.modal = None;
                if let AgentCommand::SaveModel { name, model, .. } = &cmd {
                    // the switcher offers it from now on
                    let pair = (name.clone(), model.clone());
                    if !self.opts.models.contains(&pair) {
                        self.opts.models.push(pair);
                    }
                    self.flash(&format!("{name}/{model}: saving and switching…"));
                }
                self.send(cmd);
                self.next_permission();
            }
            ModalOut::Pick { kind, value } => {
                self.modal = None;
                match kind {
                    PickKind::Model => {
                        let (provider, model) = match value.split_once('/') {
                            Some((p, m)) => (Some(p.to_string()), m.to_string()),
                            None => (None, value),
                        };
                        self.send(AgentCommand::SetModel { provider, model });
                    }
                    PickKind::Session => {
                        self.send(AgentCommand::Resume { id: value.clone() });
                        self.flash(&format!("resuming {value}…"));
                    }
                    PickKind::Rewind => {
                        if let Ok(index) = value.parse::<usize>() {
                            let text = self
                                .user_messages
                                .iter()
                                .find(|(i, _)| *i == index)
                                .map(|(_, t)| t.clone())
                                .unwrap_or_default();
                            self.send(AgentCommand::Rewind { index });
                            self.input.set_text(&text);
                            self.flash("rewound: edit the message and send it again");
                        }
                    }
                    PickKind::Fork => {
                        let index = value.parse::<usize>().ok();
                        self.send(AgentCommand::Fork { index });
                        self.flash("forking…");
                    }
                    PickKind::Tree => {
                        if self.session_id.as_deref() != Some(value.as_str()) {
                            self.send(AgentCommand::Resume { id: value.clone() });
                            self.flash(&format!("jumping to {value}…"));
                        }
                    }
                    PickKind::Trash => self.send(AgentCommand::RestoreSession { id: value.clone() }),
                    PickKind::Memory => {
                        self.send(AgentCommand::ShareMemory { id: Some(value.clone()), focus: None });
                        self.flash(&format!("reading {value}…"));
                    }
                    PickKind::Theme => {
                        self.end_theme_preview(true);
                        self.set_theme(&value, true);
                    }
                    PickKind::Shared => {}
                    PickKind::Queue => {
                        if let Ok(i) = value.parse::<usize>() {
                            self.unqueue(i);
                        }
                    }
                    PickKind::History => {
                        self.input.set_text(&value);
                        self.refresh_palette();
                    }
                    PickKind::Effort => {
                        let effort = (value != "default").then_some(value);
                        self.send(AgentCommand::SetEffort { effort });
                    }
                    PickKind::PermissionMode => {
                        if let Some(m) = PermissionMode::parse(&value) {
                            self.send(AgentCommand::SetPermissionMode(m));
                        }
                    }
                }
                self.next_permission();
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ palette

    /// After the draft changed: open, update or close the palette.
    pub fn refresh_palette(&mut self) {
        let draft = self.input.text();
        let cursor = self.input.cursor_offset();
        match palette::wanted(&draft, cursor) {
            Some((kind, start, query)) => {
                let same = self.palette.as_ref().is_some_and(|p| p.kind == kind && p.start == start);
                if same {
                    if let Some(p) = &mut self.palette {
                        p.set_query(&query);
                    }
                } else {
                    self.palette = Some(match kind {
                        PaletteKind::Commands => Palette::commands_full(
                            &query,
                            start,
                            self.custom_commands.iter().map(|c| (c.name.clone(), c.description.clone())).collect(),
                            palette::recent_commands(self.input.history()),
                        ),
                        PaletteKind::Files => {
                            let files = self.files().to_vec();
                            Palette::files(&query, start, files)
                        }
                        PaletteKind::Args(cmd) => {
                            let mut values =
                                palette::arg_values(&cmd, &self.opts.models, &self.model, &self.effort_levels);
                            values.extend(palette::live_values(
                                &cmd,
                                &self.jobs,
                                &self.trashed,
                                &self.opts.sessions,
                                &self.peer_values(),
                            ));
                            values.splice(0..0, self.current_arg(&cmd));
                            Palette::args(&cmd, &query, start, values)
                        }
                    });
                }
            }
            None => self.palette = None,
        }
        // an argument list with nothing to offer gets out of the way of free text
        if self.palette.as_ref().is_some_and(|p| p.command().is_some() && p.is_empty()) {
            self.palette = None;
        }
        self.dirty = true;
    }

    /// The sessions `/msg` offers: a one-word name when it is unique, else the start of the id.
    fn peer_values(&self) -> Vec<(String, String)> {
        let ok = |p: &&nexo_core::mail::PeerInfo| p.sendable && !p.me && !p.external;
        self.peers
            .iter()
            .filter(ok)
            .map(|p| {
                let unique = self.peers.iter().filter(|q| q.name == p.name).count() == 1;
                let value = if !p.name.is_empty() && !p.name.contains(char::is_whitespace) && unique {
                    p.name.clone()
                } else {
                    p.id.chars().take(8).collect()
                };
                let tail = p.cwd.rsplit('/').next().unwrap_or("");
                let hint = if p.bypass { format!("{tail} · every tool allowed") } else { tail.to_string() };
                (value, hint)
            })
            .collect()
    }

    /// What `/goal` and `/rename` hold now, first in their list: Tab puts it on the line to edit.
    /// `/export` offers a file here named after the session.
    fn current_arg(&self, cmd: &str) -> Option<(String, String)> {
        if cmd == "export" {
            let slug = self
                .session_title
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            let slug: String = slug.chars().take(48).collect();
            let slug = slug.trim_end_matches('-');
            return (!slug.is_empty())
                .then(|| (format!("{slug}.md"), "a file here named after the session".to_string()));
        }
        let (now, what) = match cmd {
            "goal" => (self.goal.as_ref()?.headline().trim(), "the goal now · Tab to edit it"),
            "rename" => (self.session_title.trim(), "the name now · Tab to edit it"),
            _ => return None,
        };
        (!now.is_empty()).then(|| (now.to_string(), what.to_string()))
    }

    fn files(&mut self) -> &[String] {
        if self.files.is_none() {
            self.files = Some(palette::project_files(&self.cwd, 20_000));
        }
        self.files.as_deref().unwrap_or(&[])
    }

    /// Tab or Enter on the palette: complete the selection into the draft.
    pub fn palette_accept(&mut self, run: bool) {
        let Some(p) = self.palette.take() else { return };
        // nothing matches: Tab takes the likely command, Enter sends the line as typed (and hears why not)
        let guess = if run { None } else { p.suggestion().map(String::from) };
        let Some(choice) = p.current().map(String::from).or(guess) else {
            if run {
                self.submit();
            }
            self.dirty = true;
            return;
        };
        match p.kind {
            PaletteKind::Commands => {
                self.input.replace_range(p.start, &choice);
                // a command that cannot run without its argument waits for it instead of failing
                let needs_arg = commands::lookup(&choice).is_some_and(|s| s.hint.starts_with('<'));
                if run && !needs_arg {
                    self.submit();
                } else {
                    self.input.insert_paste(" ");
                    // a command with values to choose from lists them right away
                    self.refresh_palette();
                }
            }
            PaletteKind::Files => {
                self.input.replace_range(p.start, &format!("{choice} "));
            }
            PaletteKind::Args(_) if run && !p.runs_selection() => self.submit(),
            PaletteKind::Args(_) => {
                self.input.replace_range(p.start, &choice);
                if run {
                    self.submit();
                }
            }
        }
        self.dirty = true;
    }
}

/// `+n −m` (or `+n -m`) in a tool's text, as an edit tool reports it.
pub fn parse_counts(text: &str) -> Option<(usize, usize)> {
    let i = text.find('+')?;
    let rest = &text[i + 1..];
    let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = rest[n.len()..].trim_start();
    let after = after.strip_prefix('−').or_else(|| after.strip_prefix('-'))?;
    let m: String = after.chars().take_while(char::is_ascii_digit).collect();
    if n.is_empty() || m.is_empty() {
        return None;
    }
    Some((n.parse().ok()?, m.parse().ok()?))
}

fn status_of(o: &ToolOutput) -> ToolStatus {
    if !o.is_error {
        ToolStatus::Done
    } else if o.content.to_lowercase().starts_with("permission denied")
        || o.content.to_lowercase().starts_with("denied")
    {
        ToolStatus::Denied
    } else {
        ToolStatus::Failed
    }
}

fn mode_hint(m: PermissionMode) -> &'static str {
    match m {
        PermissionMode::Ask => "ask before writes and commands",
        PermissionMode::AcceptEdits => "writes go through; ask before commands",
        PermissionMode::Auto => "never ask",
        PermissionMode::ReadOnly => "only reads and searches",
        PermissionMode::Plan => "reads, then a plan to approve before any write",
    }
}

/// `HH:MM` now, local time when the offset is known.
pub(crate) fn clock() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!("{:02}:{:02}", now.hour(), now.minute())
}

/// The branch of the repository that holds `cwd` (a short hash when detached).
pub fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let head = d.join(".git").join("HEAD");
        if let Ok(text) = std::fs::read_to_string(&head) {
            let text = text.trim();
            return Some(match text.strip_prefix("ref: ") {
                Some(r) => r.rsplit('/').next().unwrap_or(r).to_string(),
                None => text.chars().take(7).collect(),
            });
        }
        if d.join(".git").is_file() {
            // a worktree: `.git` names the real directory
            if let Ok(text) = std::fs::read_to_string(d.join(".git")) {
                if let Some(p) = text.trim().strip_prefix("gitdir: ") {
                    if let Ok(h) = std::fs::read_to_string(std::path::Path::new(p).join("HEAD")) {
                        let h = h.trim();
                        return Some(match h.strip_prefix("ref: ") {
                            Some(r) => r.rsplit('/').next().unwrap_or(r).to_string(),
                            None => h.chars().take(7).collect(),
                        });
                    }
                }
            }
        }
        dir = d.parent();
    }
    None
}

/// `in=0.28 out=0.42 cache=0.028 write=0`: prices per million tokens.
pub fn parse_pricing(args: &str) -> Result<Pricing, String> {
    let mut p = Pricing::default();
    let mut any = false;
    for tok in args.split([' ', ',']).filter(|t| !t.is_empty()) {
        let (k, v) = tok.split_once('=').ok_or_else(|| format!("'{tok}' is not key=value"))?;
        let v: f64 = v.trim_start_matches('$').parse().map_err(|_| format!("'{v}' is not a number"))?;
        if v < 0.0 {
            return Err(format!("{k} cannot be negative"));
        }
        match k.to_ascii_lowercase().as_str() {
            "in" | "input" | "prompt" => p.input = v,
            "out" | "output" | "completion" => p.output = v,
            "cache" | "cached" | "cache_read" | "read" => p.cache_read = v,
            "write" | "cache_write" => p.cache_write = v,
            other => return Err(format!("unknown key '{other}' (in, out, cache, write)")),
        }
        any = true;
    }
    if !any {
        return Err("give at least one of in=, out=, cache=, write=".into());
    }
    Ok(p)
}

/// `key value` pairs of an object of plain values (`action output, id 3`); None for anything else.
fn pairs(args: &serde_json::Value) -> Option<String> {
    let o = args.as_object().filter(|o| !o.is_empty())?;
    let mut out = Vec::new();
    for (k, v) in o {
        let v = match v {
            serde_json::Value::String(s) => s.trim().lines().next().unwrap_or("").to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => continue,
            _ => v.to_string(),
        };
        out.push(format!("{k} {v}"));
    }
    Some(out.join(", "))
}

/// One line for a call replayed from the history (the agent's summary is not recorded).
pub fn summarise(name: &str, args: &serde_json::Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::trim);
    match name {
        "web_search" => {
            if let Some(q) = s("query") {
                return format!("web_search: {}", q.lines().next().unwrap_or(""));
            }
        }
        "web_fetch" => {
            if let Some(u) = s("url") {
                // host and path: the scheme and the query say little on a card
                let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
                let shown = rest.split(['?', '#']).next().unwrap_or(rest).trim_end_matches('/');
                return format!("web_fetch: {shown}");
            }
        }
        _ => {}
    }
    for k in ["path", "file_path", "command", "pattern", "query", "url"] {
        if let Some(v) = args.get(k).and_then(|v| v.as_str()) {
            return v.lines().next().unwrap_or("").to_string();
        }
    }
    let _ = name;
    let s = pairs(args).unwrap_or_else(|| args.to_string());
    if s.len() > 80 {
        // cut on a character boundary: the JSON keeps non-ASCII text as is
        let cut = (0..=79).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
        format!("{}…", &s[..cut])
    } else {
        s
    }
}

/// The body of the last fenced code block in markdown text.
pub fn last_code_block(text: &str) -> Option<String> {
    let mut found = None;
    let mut open: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        let t = line.trim_start();
        let fence: String = t.chars().take_while(|c| *c == '`' || *c == '~').collect();
        match open.as_mut() {
            Some((f, body)) => {
                if fence.len() >= f.len() && fence.starts_with(&f[..1]) && t.trim_end() == fence {
                    found = Some(body.join("\n"));
                    open = None;
                } else {
                    body.push(line);
                }
            }
            None if fence.len() >= 3 && fence.chars().all(|c| c == fence.chars().next().unwrap()) => {
                open = Some((fence, Vec::new()));
            }
            None => {}
        }
    }
    found
}

/// How long a block takes to fade in, and to open.
const FADE_MS: u64 = 220;
const OPEN_MS: u64 = 160;

/// What the interface keeps about a block besides the block itself.
#[derive(Debug, Clone, Copy, Default)]
struct Look {
    /// Folded to its first line (blocks with no fold of their own).
    folded: bool,
    /// When it arrived, for the fade.
    born: Option<Instant>,
    /// When it last opened, and how many lines it had before.
    opened: Option<(Instant, usize)>,
    /// When a tool card stopped running, and whether it was running at the last tick.
    done_at: Option<Instant>,
    was_live: bool,
    /// How far back the group block of a call is (None: not in a group).
    group_back: Option<u32>,
}

/// The steps of a turn: a tool call with its result, the model's thinking, a group of calls.
fn is_step(b: &Block) -> bool {
    matches!(b, Block::Tool { .. } | Block::Reasoning { .. } | Block::Group { .. })
}

/// Columns before a block's lines: a margin of one for every block (where the selection's
/// bar is drawn), and two more for the steps of a turn, so a call and its result sit under
/// the reply's text. Narrow terminals keep every column for the text.
/// The word at the right of a tool card's head: where its output stands for the last model call (in the
/// cached part of the request, paid in full, trimmed, set aside, recalled). A glyph and a word, coloured like the bar,
/// and only when it fits beside the head.
fn mark_head(head: &mut Line<'static>, m: CallMark, cost: Option<f64>, w: usize, theme: &Theme) {
    use crate::render::wrap::width;
    let (word, style) = match m {
        CallMark::Cached => ("● cached", theme.style(theme.success)),
        CallMark::Sent => ("● full price", theme.style(theme.accent)),
        CallMark::Cut => ("◐ trimmed", theme.style(theme.warn)),
        CallMark::Out => ("○ set aside", theme.dim2()),
        CallMark::Back => ("↺ recalled", theme.style(theme.accent2)),
    };
    // the mark wins over the end of a long head: the command is cut with an ellipsis, never below this
    const HEAD_KEEP: usize = 16;
    let used: usize = head.spans.iter().map(|s| width(&s.content)).sum();
    // the price of this output in the last request goes after the word when it fits, else the word alone
    let priced = cost.map(|c| format!("{word} {c:.5} $"));
    let Some((text, limit)) = priced.into_iter().chain([word.to_string()]).find_map(|t| {
        let limit = w.saturating_sub(width(&t) + 3);
        (used <= limit || limit >= HEAD_KEEP).then_some((t, limit))
    }) else {
        return;
    };
    if used > limit {
        cut_line(head, limit);
    }
    let used: usize = head.spans.iter().map(|s| width(&s.content)).sum();
    head.spans.push(Span::raw(" ".repeat(w.saturating_sub(used + width(&text)))));
    head.spans.push(Span::styled(text, style));
}

/// Cuts `line` to at most `w` cells, ending in an ellipsis; the spans keep their styles.
fn cut_line(line: &mut Line<'static>, w: usize) {
    use crate::render::wrap::{truncate, width};
    let mut acc = 0;
    for i in 0..line.spans.len() {
        let sw = width(&line.spans[i].content);
        let rem = w.saturating_sub(acc);
        if sw < rem {
            acc += sw;
            continue;
        }
        // this span reaches the edge: it ends in the ellipsis and the rest goes
        let c = line.spans[i].content.to_string();
        line.spans[i].content = if sw > rem {
            truncate(&c, rem)
        } else if rem > 1 {
            truncate(&c, rem - 1)
        } else {
            "…".to_string()
        }
        .into();
        line.spans.truncate(i + 1);
        return;
    }
}

fn indent_of(b: &Block, width: usize) -> usize {
    if width < 24 || matches!(b, Block::Welcome { .. }) {
        0
    } else if is_step(b) {
        3
    } else {
        1
    }
}

/// Blocks with no fold of their own that still fold to their first line.
fn folds_alone(b: &Block) -> bool {
    matches!(
        b,
        Block::User { .. } | Block::Assistant { streaming: false, .. } | Block::Shell { .. } | Block::Notice { .. }
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_last_code_block_is_found() {
        let text = "run this:\n```bash\ncargo test\n```\nthen\n````rust\nfn a() {}\n```\nx\n````\nend";
        assert_eq!(last_code_block(text).as_deref(), Some("fn a() {}\n```\nx"));
        assert_eq!(last_code_block("```\ncut off"), None);
        assert_eq!(last_code_block("no code"), None);
    }

    #[test]
    fn ctrl_c_clears_a_draft_first_and_the_title_follows_the_turn() {
        let (mut a, _rx) = app();
        a.input.set_text("half a thought");
        a.ctrl_c();
        assert!(a.input.is_empty());
        assert_eq!(a.input.history().last().map(String::as_str), Some("half a thought"));
        assert!(!a.should_quit);
        a.ctrl_c();
        assert!(a.should_quit);

        let (mut a, _rx) = app();
        assert_eq!(a.terminal_title(), "nexo");
        a.session_title = "fix the parser".into();
        assert_eq!(a.terminal_title(), "nexo · fix the parser");
        a.busy = true;
        assert!(a.terminal_title().ends_with(" nexo · fix the parser"));
        assert_ne!(a.terminal_title(), "nexo · fix the parser");
    }

    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;

    #[test]
    fn a_long_command_is_cut_so_the_cache_mark_and_price_still_show() {
        use crate::render::wrap::width;
        let theme = Theme::dark();
        let text = |l: &Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
        let head = || Line::from(vec![Span::raw("▸ bash  "), Span::raw("cargo test -p nexo-bench ".repeat(6))]);
        // too long for the line: the command ends in … and the mark and price close the line
        let mut l = head();
        mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
        let t = text(&l);
        assert!(t.ends_with("● cached 0.00012 $") && t.contains('…'), "{t}");
        assert_eq!(width(&t), 60);
        // no price known: the word alone
        let mut l = head();
        mark_head(&mut l, CallMark::Sent, None, 50, &theme);
        assert!(text(&l).ends_with("● full price") && width(&text(&l)) == 50, "{}", text(&l));
        // a short command is left whole
        let mut l = Line::from(vec![Span::raw("▸ bash  "), Span::raw("ls")]);
        mark_head(&mut l, CallMark::Cached, Some(0.00012), 60, &theme);
        assert!(text(&l).starts_with("▸ bash  ls ") && !text(&l).contains('…'));
        // a window too narrow for any of it keeps the head as it was
        let mut l = head();
        mark_head(&mut l, CallMark::Cached, Some(0.00012), 20, &theme);
        assert_eq!(text(&l), text(&head()));
    }

    fn app() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let opts = TuiOptions {
            cwd: PathBuf::from("."),
            provider: "mock".into(),
            model: "m".into(),
            models: vec![("mock".into(), "m".into()), ("mock".into(), "n".into())],
            providers: vec![(
                "ollama".into(),
                nexo_core::ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None),
            )],
            theme: "dark".into(),
            mouse: true,
            show_reasoning: true,
            timestamps: false,
            fold_after: 3,
            group_tools: true,
            keys: Default::default(),
            notify_after: 15,
            animations: true,
            status_line: vec![],
            status_command: String::new(),
            vim: false,
            session_id: None,
            sessions: vec![],
        };
        let mut a = App::new(opts, tx);
        a.root = false;
        (a, rx)
    }

    /// An app whose transcript starts empty: tests that count blocks from the first.
    fn bare() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
        let (mut a, rx) = app();
        a.blocks.clear();
        a.cache.clear();
        a.looks.clear();
        (a, rx)
    }

    // ------------------------------------------------------------ groups of calls

    fn running(id: &str) -> Block {
        let mut b = card(id, 1);
        if let Block::Tool { status, output, started, .. } = &mut b {
            *status = ToolStatus::Running;
            *output = None;
            *started = Some(Instant::now());
        }
        b
    }

    fn thinking() -> Block {
        Block::Reasoning { text: "hm".into(), streaming: false, started: None, elapsed_ms: Some(5), folded: true }
    }

    fn head(a: &mut App, i: usize) -> String {
        a.lines(i, 80).first().map(crate::render::markdown::text_of).unwrap_or_default()
    }

    fn groups(a: &App) -> usize {
        a.blocks.iter().filter(|b| matches!(b, Block::Group { .. })).count()
    }

    #[test]
    fn two_tool_calls_form_one_group_marker_and_a_single_call_does_not() {
        let (mut a, _rx) = bare();
        a.push(Block::User { text: "go".into(), at: None });
        let one = a.push(card("a", 2));
        assert_eq!(groups(&a), 0);
        assert_eq!(one, 1);
        let two = a.push(card("b", 2));
        assert_eq!(groups(&a), 1);
        assert!(matches!(a.blocks[1], Block::Group { folded: true, len: 2 }));
        assert_eq!(two, 3, "the call is returned where it now is");
        a.push(card("c", 2));
        assert!(matches!(a.blocks[1], Block::Group { len: 3, .. }), "a third call joins the same group");
        assert_eq!(groups(&a), 1);
        assert_eq!(a.blocks.len(), 5);
        assert!(!a.hidden(1) && a.hidden(2) && a.hidden(3) && a.hidden(4));
    }

    #[test]
    fn reasoning_between_calls_joins_the_group_but_trailing_reasoning_does_not() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(thinking());
        a.push(card("b", 1));
        assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "{:?}", a.blocks.len());
        a.push(thinking());
        assert!(matches!(a.blocks[0], Block::Group { len: 3, .. }), "thinking after the last call is outside");
        assert!(!a.hidden(5));
        a.push(Block::Assistant { text: "x".into(), streaming: false, at: None, started: None, took_ms: None });
        a.push(card("c", 1));
        assert_eq!(groups(&a), 1, "an answer breaks the run");
    }

    #[test]
    fn the_agreed_plan_and_opened_thinking_stay_visible_in_a_folded_group() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        let mut open = thinking();
        if let Block::Reasoning { folded, .. } = &mut open {
            *folded = false;
        }
        a.push(open);
        let mut plan = card("p", 1);
        if let Block::Tool { name, .. } = &mut plan {
            *name = "exit_plan".into();
        }
        a.push(plan);
        a.push(card("b", 1));
        assert!(a.hidden(1) && !a.hidden(2), "thinking the user opened is not hidden");
        assert!(!a.hidden(3), "the plan is not hidden");
        assert!(a.hidden(4));
    }

    #[test]
    fn group_tools_off_creates_no_marker() {
        let (mut a, _rx) = bare();
        a.opts.group_tools = false;
        a.push(card("a", 1));
        a.push(card("b", 1));
        assert_eq!(groups(&a), 0);
        assert_eq!(a.blocks.len(), 2);
    }

    #[test]
    fn a_running_member_stays_visible_under_a_folded_header() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(running("b"));
        a.layout(80);
        assert!(a.hidden(1) && !a.hidden(2));
        let h = head(&mut a, 0);
        assert!(h.contains("Running"), "{h}");
        assert_eq!(a.offsets[1], a.offsets[2], "the finished call takes no room");
    }

    #[test]
    fn group_header_changes_tense_when_the_last_call_finishes() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(running("b"));
        a.layout(80);
        assert!(head(&mut a, 0).contains("Running"), "{}", head(&mut a, 0));
        if let Block::Tool { status, output, .. } = &mut a.blocks[2] {
            *status = ToolStatus::Done;
            *output = Some(ToolOutput::ok("ok"));
        }
        a.invalidate(2);
        a.layout(80);
        let h = head(&mut a, 0);
        assert!(h.contains("Ran 2 shell commands"), "{h}");
        assert!(a.hidden(2), "folded again once everything finished");
    }

    #[test]
    fn enter_on_a_group_toggles_it_and_inner_cards_keep_their_own_fold() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(card("a", 3));
        a.push(card("b", 3));
        a.layout(80);
        a.selected = Some(0);
        press(&mut a, KeyCode::Enter);
        assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
        a.layout(80);
        assert!(!a.hidden(1) && !a.hidden(2));
        assert!(matches!(a.blocks[1], Block::Tool { folded: true, .. }), "the cards stay folded inside");
        assert!(a.offsets[2] > a.offsets[1]);
        press(&mut a, KeyCode::Enter);
        a.layout(80);
        assert!(a.hidden(1) && a.hidden(2));
        assert_eq!(a.selected, Some(0));
    }

    #[test]
    fn click_on_the_header_selects_then_toggles() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(card("b", 1));
        a.layout(80);
        a.click_block(0, 0);
        assert_eq!(a.selected, Some(0));
        assert!(a.is_folded(0));
        a.click_block(0, 0);
        assert!(!a.is_folded(0));
    }

    #[test]
    fn arrow_navigation_skips_hidden_members() {
        let (mut a, _rx) = bare();
        a.push(Block::User { text: "a long\nmessage\nof lines".into(), at: None });
        a.push(card("a", 3));
        a.push(card("b", 3));
        a.push(Block::Assistant {
            text: "done\n\nmore\n\nlines".into(),
            streaming: false,
            at: None,
            started: None,
            took_ms: None,
        });
        a.layout(80);
        a.selected = None;
        for _ in 0..4 {
            a.select_next(true);
            if let Some(s) = a.selected {
                assert!(!a.hidden(s), "{s} is hidden");
            }
        }
    }

    #[test]
    fn collapse_keeps_scroll_inside_the_transcript() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(Block::User { text: "go".into(), at: None });
        for id in ["a", "b", "c", "d"] {
            a.push(card(id, 20));
        }
        a.transcript_area = Rect::new(0, 1, 80, 6);
        a.layout(80);
        a.toggle_block(1);
        for k in 2..6 {
            a.blocks[k].set_fold(false);
            a.invalidate(k);
        }
        a.layout(80);
        a.follow = false;
        a.scroll = a.offsets[5] + 2;
        a.toggle_block(1);
        a.layout(80);
        a.settle(6);
        assert!(a.scroll <= a.max_scroll(6), "{} > {}", a.scroll, a.max_scroll(6));
        assert!(a.scroll <= a.offsets[1] + 1);
    }

    #[test]
    fn ctrl_o_flips_every_group_and_card() {
        let (mut a, _rx) = bare();
        a.push(card("a", 2));
        a.push(card("b", 2));
        a.fold_all();
        assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }), "everything was folded: it opens");
        assert!(matches!(a.blocks[1], Block::Tool { folded: false, .. }));
        a.fold_all();
        assert!(matches!(a.blocks[0], Block::Group { folded: true, .. }));
        assert!(matches!(a.blocks[2], Block::Tool { folded: true, .. }));
    }

    #[test]
    fn search_hit_in_a_folded_group_counts_once_and_unfolds_on_jump() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(card("a", 2));
        a.push(card("b", 2));
        a.transcript_area = Rect::new(0, 1, 80, 10);
        a.layout(80);
        a.open_search();
        a.search.as_mut().unwrap().query = "line".into();
        a.rematch();
        assert_eq!(a.search.as_ref().unwrap().matches.len(), 1, "one hit for the whole folded group");
        a.search_first();
        assert!(matches!(a.blocks[0], Block::Group { folded: false, .. }));
        a.layout(80);
        assert!(!a.search.as_ref().unwrap().matches.is_empty());
    }

    #[test]
    fn a_folded_group_is_one_copyable_line() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(card("b", 1));
        a.layout(80);
        let t = a.line_text(0).unwrap();
        assert!(t.contains("Ran 2 shell commands"), "{t}");
    }

    #[test]
    fn replayed_session_groups_like_a_live_one() {
        let (mut a, _rx) = bare();
        a.push(card("a", 1));
        a.push(thinking());
        a.push(card("b", 1));
        a.push(card("c", 1));
        assert!(matches!(a.blocks[0], Block::Group { len: 4, .. }));
        assert_eq!(a.blocks.len(), 5);
    }

    #[test]
    fn animations_off_opens_instantly() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        a.push(card("a", 4));
        a.push(card("b", 4));
        a.layout(80);
        a.toggle_block(0);
        a.layout(80);
        let full = a.height(1, 80);
        assert_eq!(a.shown(1, 80), full);
    }

    #[test]
    fn a_long_finished_group_costs_nothing_per_frame() {
        let (mut a, _rx) = bare();
        for n in 0..200 {
            a.push(card(&format!("c{n}"), 3));
        }
        a.layout(80);
        a.layout(80);
        let rendered = a.cache.iter().filter(|c| matches!(c, Some((_, _, Some(_))))).count();
        assert!(rendered <= 2, "hidden calls are not rendered: {rendered}");
        assert!(a.total_lines <= 2, "one line and its gap: {}", a.total_lines);
    }

    #[test]
    fn one_key_opens_and_folds_thinking_with_the_tool_cards() {
        let (mut a, _rx) = app();
        let i = a.push(Block::Reasoning {
            text: "a\nb\nc".into(),
            streaming: false,
            started: None,
            elapsed_ms: Some(5),
            folded: true,
        });
        a.fold_all();
        assert!(!a.is_folded(i), "everything folded, so the key opens it");
        a.fold_all();
        assert!(a.is_folded(i));
    }

    #[test]
    fn any_long_block_folds_to_its_first_line_and_opens_again() {
        let (mut a, _rx) = app();
        let text = (1..=6).map(|i| format!("paragraph {i}")).collect::<Vec<_>>().join("\n\n");
        let i = a.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
        a.layout(60);
        let open = a.lines(i, 60).len();
        assert!(open > 2);
        assert!(a.foldable(i));
        a.toggle_block(i);
        let folded: Vec<String> = a.lines(i, 60).iter().map(crate::render::markdown::text_of).collect();
        assert_eq!(folded.len(), 2, "{folded:?}");
        assert!(folded[0].contains("paragraph 1"));
        assert!(folded[1].contains(&format!("▸ {} more lines", open - 1)), "{folded:?}");
        a.toggle_block(i);
        assert_eq!(a.lines(i, 60).len(), open);
        let one = a.push(Block::Notice { level: Level::Info, text: "short".into() });
        assert!(!a.foldable(one), "a one-line block has nothing to fold");
    }

    #[test]
    fn a_new_block_fades_in_and_an_opened_one_grows() {
        let (mut a, _rx) = app();
        let text = (1..=8).map(|i| format!("p{i}")).collect::<Vec<_>>().join("\n\n");
        let i = a.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
        assert!(a.fade(i) < 1.0, "a fresh block starts faint");
        a.layout(60);
        a.toggle_block(i);
        a.toggle_block(i);
        let full = a.lines(i, 60).len();
        assert!(a.shown(i, 60) < full, "an opening block grows over a few frames");
        a.looks[i].opened = None;
        a.looks[i].born = None;
        assert_eq!(a.shown(i, 60), full);
        assert_eq!(a.fade(i), 1.0);
    }

    #[test]
    fn streams_into_blocks_and_closes_them() {
        let (mut a, _rx) = bare();
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::ReasoningDelta { text: "hm".into() });
        a.handle_agent(AgentEvent::TextDelta { text: "Hel".into() });
        a.handle_agent(AgentEvent::TextDelta { text: "lo".into() });
        a.handle_agent(AgentEvent::ToolCall {
            id: "c1".into(),
            name: "read".into(),
            args: serde_json::json!({"path":"a"}),
            summary: "a".into(),
        });
        a.handle_agent(AgentEvent::ToolStart { id: "c1".into() });
        a.handle_agent(AgentEvent::ToolDone { id: "c1".into(), output: ToolOutput::ok("x"), elapsed_ms: 5 });
        a.handle_agent(AgentEvent::AssistantDone { message: Message::assistant("Hello") });
        a.handle_agent(AgentEvent::Usage {
            turn: Usage { input: 1, output: 1, ..Default::default() },
            call: Usage { input: 1, output: 1, ..Default::default() },
            session: Usage::default(),
            cost_usd: 0.0,
        });
        a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
        assert!(matches!(a.blocks[0], Block::Reasoning { streaming: false, .. }));
        assert!(matches!(&a.blocks[1], Block::Assistant { text, streaming: false, .. } if text == "Hello"));
        assert!(matches!(a.blocks[2], Block::Tool { status: ToolStatus::Done, .. }));
        assert!(matches!(a.blocks[3], Block::Usage { .. }));
        assert!(!a.busy);
    }

    #[test]
    fn web_cards_are_summarised_on_replay() {
        assert_eq!(summarise("web_search", &serde_json::json!({"query": "tokio select"})), "web_search: tokio select");
        assert_eq!(
            summarise("web_fetch", &serde_json::json!({"url": "https://docs.rs/tokio/latest/?x=1"})),
            "web_fetch: docs.rs/tokio/latest"
        );
    }

    #[test]
    fn a_replayed_call_keeps_one_card() {
        let (mut a, _rx) = app();
        let m = Message::new(
            Role::Assistant,
            vec![Part::tool_call("t1", "todo", serde_json::json!({"items": [{"text": "a"}]}))],
        );
        a.handle_agent(AgentEvent::History { index: 0, message: m });
        a.handle_agent(AgentEvent::ToolCall {
            id: "t1".into(),
            name: "todo".into(),
            args: serde_json::json!({"items": [{"text": "a"}]}),
            summary: "todo: 0/1 done".into(),
        });
        a.handle_agent(AgentEvent::ToolDone { id: "t1".into(), output: ToolOutput::ok("[ ] a"), elapsed_ms: 0 });
        let cards: Vec<&Block> = a.blocks.iter().filter(|b| matches!(b, Block::Tool { .. })).collect();
        assert_eq!(cards.len(), 1);
        assert!(
            matches!(cards[0], Block::Tool { summary, status: ToolStatus::Done, .. } if summary == "todo: 0/1 done")
        );
    }

    #[test]
    fn history_replays_and_attaches_results() {
        let (mut a, _rx) = bare();
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("do it") });
        let m = Message::new(
            Role::Assistant,
            vec![Part::text("ok"), Part::tool_call("c9", "bash", serde_json::json!({"command": "ls"}))],
        );
        a.handle_agent(AgentEvent::History { index: 1, message: m });
        a.handle_agent(AgentEvent::History {
            index: 2,
            message: Message::tool_results(vec![Part::tool_result("c9", "a.rs\nb.rs", false)]),
        });
        assert_eq!(a.blocks.len(), 3);
        match &a.blocks[2] {
            Block::Tool { summary, output: Some(o), status, .. } => {
                assert_eq!(summary, "ls");
                assert_eq!(o.content, "a.rs\nb.rs");
                assert_eq!(*status, ToolStatus::Done);
            }
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn a_new_width_keeps_the_block_at_the_top_of_the_view() {
        let (mut a, _rx) = app();
        for i in 0..30 {
            a.push(Block::User { text: format!("message {i} {}", "word ".repeat(30)), at: None });
        }
        a.transcript_area = Rect::new(0, 0, 80, 10);
        a.layout(79);
        a.follow = false;
        a.scroll = a.offsets[12];
        a.layout(40);
        assert_eq!(a.scroll, a.offsets[12], "block 12 stays at the top after a re-wrap");
        a.layout(120);
        assert_eq!(a.scroll, a.offsets[12]);
    }

    #[test]
    fn layout_scroll_and_search() {
        let (mut a, _rx) = bare();
        for i in 0..50 {
            a.push(Block::User { text: format!("message number {i}"), at: None });
        }
        a.transcript_area = Rect::new(0, 0, 80, 10);
        a.layout(80);
        // a message of one's own starts a turn: two blank lines before it
        assert_eq!(a.total_lines, 49 * 3 + 2);
        a.settle(10);
        assert_eq!(a.scroll, 139);
        a.scroll_by(-3);
        assert!(!a.follow);
        assert_eq!(a.scroll, 136);
        a.push(Block::User { text: "late".into(), at: None });
        a.layout(80);
        a.settle(10);
        assert_eq!(a.unseen(), 3);
        a.scroll_to_end();
        assert_eq!(a.unseen(), 0);
        assert_eq!(a.block_at_line(0), Some(0));
        assert_eq!(a.block_at_line(1), None, "the separator belongs to nobody");
        assert_eq!(a.block_at_line(2), None);
        assert_eq!(a.block_at_line(3), Some(1));
        a.open_search();
        a.search.as_mut().unwrap().query = "number 4".into();
        a.rematch();
        assert_eq!(a.search.as_ref().unwrap().matches.len(), 11);
        a.search_jump(true);
        assert!(!a.follow);
    }

    #[test]
    fn commands_and_submissions_reach_the_agent() {
        let (mut a, mut rx) = app();
        a.input.set_text("hello there");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "hello there"));
        a.input.set_text("/permissions auto");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPermissionMode(PermissionMode::Auto))));
        a.input.set_text("!ls -la");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Shell { command }) if command == "ls -la"));
        a.input.set_text("/model");
        a.submit();
        assert!(matches!(a.modal, Some(Modal::Setup(_))));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ScanLocal)), "the setup looks for local servers");
        a.handle_agent(AgentEvent::LocalServers { found: vec![("ollama".into(), "Ollama 0.12.6".into())] });
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |a: &mut App, code: KeyCode| {
            let out = a.modal.as_mut().unwrap().key(KeyEvent::new(code, KeyModifiers::NONE));
            a.modal_out(out);
        };
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Enter);
        assert!(a.modal.is_none());
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetModel { model, .. }) if model == "n"));
        // the setup of a new one: the running server is asked at once, its answer lands in the modal
        a.input.set_text("/model");
        a.submit();
        let _ = rx.try_recv();
        a.handle_agent(AgentEvent::LocalServers { found: vec![("ollama".into(), "Ollama 0.12.6".into())] });
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Down);
        key(&mut a, KeyCode::Enter);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Probe { name, .. }) if name == "ollama"));
        a.handle_agent(AgentEvent::Probed {
            name: "ollama".into(),
            result: Ok(nexo_core::ServerProbe {
                kind: "ollama".into(),
                protocol: "openai-chat".into(),
                base_url: "http://localhost:11434/v1".into(),
                models: vec![nexo_core::ModelInfo { id: "qwen3:8b".into(), ..Default::default() }],
                ..Default::default()
            }),
        });
        let Some(Modal::Setup(st)) = &a.modal else { panic!("the setup stays open") };
        assert_eq!(st.step, crate::modal::setup::Step::Model);
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        key(&mut a, KeyCode::Esc);
        assert!(a.modal.is_none(), "Esc walks back and out");
        // /effort: the levels the model takes
        a.handle_agent(AgentEvent::Effort {
            model: "m".into(),
            current: None,
            levels: vec!["low".into(), "high".into()],
        });
        a.input.set_text("/effort");
        a.submit();
        let Some(Modal::Picker(p)) = &a.modal else { panic!("an effort picker") };
        assert_eq!(p.items.iter().map(|i| i.value.as_str()).collect::<Vec<_>>(), ["default", "low", "high"]);
        a.modal_out(ModalOut::Pick { kind: PickKind::Effort, value: "high".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetEffort { effort: Some(e) }) if e == "high"));
        a.input.set_text("/effort default");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetEffort { effort: None })));
        a.input.set_text("/nope");
        a.submit();
        assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
        a.input.set_text("/mo");
        a.refresh_palette();
        assert!(a.palette.is_some());
        a.palette_accept(false);
        assert_eq!(a.input.text(), "/model ");
    }

    #[test]
    fn enter_on_an_empty_draft_with_a_queue_interrupts_the_turn() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.input.set_text("second thought");
        a.submit();
        assert_eq!(a.queued, 1);
        while rx.try_recv().is_ok() {}
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Interrupt)));
        // nothing waiting: an empty Enter does not stop the turn
        a.queued = 0;
        a.submit();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn history_clears_and_the_session_and_queue_follow_the_agent() {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first") });
        a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
        a.handle_agent(AgentEvent::Usage {
            turn: Usage { input: 5, ..Default::default() },
            call: Usage { input: 5, ..Default::default() },
            session: Usage { input: 5, ..Default::default() },
            cost_usd: 0.01,
        });
        a.transcript_area = Rect::new(0, 0, 80, 10);
        a.layout(80);
        a.selection = Some(Selection::new(Pos { line: 0, col: 0 }));
        a.handle_agent(AgentEvent::Queued { pending: 2 });
        assert_eq!(a.queued, 2);
        a.handle_agent(AgentEvent::HistoryCleared);
        assert!(a.has_welcome() && a.blocks.len() == 1, "only the banner is left");
        assert!(a.user_messages.is_empty() && a.selection.is_none());
        assert_eq!(a.total_lines, 0);
        assert_eq!(a.cost, 0.0);
        assert_eq!(a.queued, 0);
        a.handle_agent(AgentEvent::SessionChanged { session_id: "s2".into(), title: "the other one".into() });
        assert_eq!(a.session_id.as_deref(), Some("s2"));
        assert_eq!(a.session_title, "the other one");
        a.handle_agent(AgentEvent::Queued { pending: 0 });
        assert_eq!(a.queued, 0);
    }

    #[test]
    fn msg_writes_to_a_session_and_the_transcript_follows_the_receipts() {
        use nexo_core::mail::{PeerInfo, Stage};
        let (mut a, mut rx) = app();
        assert_eq!(
            commands::parse("/msg api look at parser.rs"),
            Some(Command::Msg(Some("api look at parser.rs".into())))
        );
        a.run_command(Command::Msg(Some("api look at parser.rs".into())));
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::SendMail { to, text }) if to == "api" && text == "look at parser.rs")
        );
        // a session and no text: nothing goes out
        a.run_command(Command::Msg(Some("api".into())));
        assert!(rx.try_recv().is_err() && matches!(a.blocks.last(), Some(Block::Notice { .. })));
        // no arguments: the sessions are asked for and the palette opens on them
        let peer = |id: &str, name: &str, me: bool| PeerInfo {
            id: id.into(),
            name: name.into(),
            cwd: "/home/dev/api".into(),
            pid: 1,
            external: false,
            bypass: false,
            sendable: true,
            me,
        };
        a.handle_agent(AgentEvent::Peers {
            peers: vec![
                peer("aaaa1111", "me", true),
                peer("bbbb2222", "api", false),
                peer("cccc3333", "two words", false),
            ],
        });
        a.run_command(Command::Msg(None));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ListPeers)));
        assert_eq!(a.input.text(), "/msg ");
        let v: Vec<String> = a.peer_values().into_iter().map(|(v, _)| v).collect();
        assert_eq!(v, ["api", "cccc3333"], "not itself; a name with spaces is offered by its id");

        // the receipts move the line along and never back
        let stage = |a: &App| {
            a.blocks.iter().rev().find_map(|b| if let Block::Mail { stage, .. } = b { Some(*stage) } else { None })
        };
        a.handle_agent(AgentEvent::MailSent {
            id: "m1".into(),
            to: "bbbb2222".into(),
            to_name: "api".into(),
            summary: "look at parser.rs".into(),
            bytes: 17,
        });
        assert_eq!(stage(&a), Some(Stage::Sent));
        a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Delivered });
        assert_eq!(stage(&a), Some(Stage::Delivered));
        a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Read });
        a.handle_agent(AgentEvent::MailStage { id: "m1".into(), stage: Stage::Delivered });
        assert_eq!(stage(&a), Some(Stage::Read));
        let lines = a.blocks.iter().rev().find(|b| matches!(b, Block::Mail { .. })).unwrap();
        let text: String = lines
            .render(80, &a.theme, a.render_opts())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("✉ -> api: look at parser.rs") && text.contains("✓✓ read"), "{text}");

        // a mail that comes in is marked, and a refusal warns
        a.handle_agent(AgentEvent::MailArrived {
            id: "m2".into(),
            from: "bbbb2222".into(),
            from_name: "api".into(),
            summary: "done".into(),
            held: false,
        });
        assert!(matches!(a.blocks.last(), Some(Block::Mail { outgoing: false, .. })));
        a.handle_agent(AgentEvent::MailDelivered { ids: vec!["m2".into()] });
        assert_eq!(stage(&a), Some(Stage::Delivered));
        a.handle_agent(AgentEvent::MailFailed { to: "web".into(), reason: "no such session".into() });
        assert!(
            matches!(a.blocks.last(), Some(Block::Notice { level: Level::Warn, text }) if text.contains("no such session"))
        );
    }

    #[test]
    fn fork_picks_a_cut_point_and_tree_jumps_between_sessions() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first ask") });
        a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
        a.run_command(Command::Fork);
        let Some(Modal::Picker(p)) = &a.modal else { panic!("no fork picker") };
        assert_eq!(p.kind, PickKind::Fork);
        a.modal_out(ModalOut::Pick { kind: PickKind::Fork, value: "0".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Fork { index: Some(0) })));
        a.modal_out(ModalOut::Pick { kind: PickKind::Fork, value: "all".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Fork { index: None })));
        a.run_command(Command::Tree);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowTree)));
        a.handle_agent(AgentEvent::SessionTree { entries: vec![(0, "s1".into(), "root".into())] });
        assert!(a.modal.is_none(), "a lone session has no tree to show");
        a.handle_agent(AgentEvent::SessionTree {
            entries: vec![(0, "s1".into(), "root".into()), (1, "s2".into(), "root (fork)".into())],
        });
        assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Tree));
        a.modal_out(ModalOut::Pick { kind: PickKind::Tree, value: "s2".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "s2"));
    }

    #[test]
    fn the_rewind_picker_lists_the_persons_messages_newest_first_and_rewinds() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("first ask\nwith detail") });
        a.handle_agent(AgentEvent::History { index: 1, message: Message::assistant("ok") });
        a.handle_agent(AgentEvent::History { index: 2, message: Message::user("second ask") });
        a.esc();
        assert!(a.modal.is_none(), "the first Esc only hints");
        a.esc();
        let Some(Modal::Picker(p)) = &a.modal else { panic!("no picker") };
        assert_eq!(p.kind, PickKind::Rewind);
        assert_eq!(p.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), vec!["second ask", "first ask"]);
        assert_eq!(p.items[1].value, "0");
        a.modal_out(ModalOut::Pick { kind: PickKind::Rewind, value: "0".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rewind { index: 0 })));
        assert_eq!(a.input.text(), "first ask\nwith detail");
        a.input.clear();
        a.input.set_text("/rewind");
        a.submit();
        assert!(matches!(a.modal, Some(Modal::Picker(_))));
        a.modal = None;
        a.busy = true;
        a.run_command(Command::Rewind);
        assert!(a.modal.is_none(), "no rewind while a turn runs");
    }

    #[test]
    fn sessions_resume_and_export_and_diff_reach_the_agent() {
        let (mut a, mut rx) = app();
        a.opts.sessions = vec![("20261027-1".into(), "one".into())];
        a.run_command(Command::Sessions);
        a.modal_out(ModalOut::Pick { kind: PickKind::Session, value: "20261027-1".into() });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261027-1"));
        a.input.set_text("/export notes.md");
        a.submit();
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::Export { path: Some(p) }) if p == std::path::Path::new("notes.md"))
        );
        a.input.set_text("/diff");
        a.submit();
        assert!(a.diffview.is_some());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_clock_keeps_beating_while_the_diff_view_reads_the_changes() {
        use crate::frame::Load;
        let (mut a, _rx) = app();
        // the banner comes in from the very first frame, before any tick
        assert_eq!(a.load(), Load::Motion, "the intro asks for frames at once");
        a.fx.clock = Some(a.opened + std::time::Duration::from_millis(crate::render::blocks::INTRO_MS + 200));
        assert_eq!(a.load(), Load::Idle, "then the clock rests");
        a.opts.animations = false;
        a.fx.clock = Some(a.opened);
        assert_eq!(a.load(), Load::Idle, "no intro without animations");
        let mut v = crate::diffview::DiffView::from_collected(Default::default());
        v.loading = true;
        a.diffview = Some(v);
        assert_eq!(a.load(), Load::Beat, "the answer of the collection is taken on a tick");
        a.diffview = Some(crate::diffview::DiffView::from_collected(Default::default()));
        assert_eq!(a.load(), Load::Idle, "once it answered, nothing asks for the clock");
    }

    #[test]
    fn enter_on_a_command_that_needs_its_argument_waits_for_it() {
        let (mut a, mut rx) = app();
        a.session_title = "fix the parser".into();
        a.input.set_text("/ren");
        a.refresh_palette();
        a.palette_accept(true);
        assert_eq!(a.input.text(), "/rename ");
        assert!(rx.try_recv().is_err() && !a.blocks.iter().any(|b| matches!(b, Block::Notice { .. })));
        assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("fix the parser"), "its list opens");
    }

    #[test]
    fn a_restated_goal_reads_as_what_the_person_wants() {
        let g = |text: &str| Goal { text: text.into(), round: 0, max: 5, state: GoalState::Active };
        assert_eq!(g("Want: green tests\nWhy: release\nHow: x\nDone when: y").headline(), "green tests");
        assert_eq!(g("ship it").headline(), "ship it");
        let mut a = app().0;
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("[goal set] tests pls") });
        a.handle_agent(AgentEvent::History { index: 2, message: Message::user("[goal: continue]") });
        let notices: Vec<&str> = a
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Notice { text, .. } => Some(text.as_str()),
                Block::User { .. } => panic!("a goal request of the loop shows as a user message"),
                _ => None,
            })
            .collect();
        assert_eq!(notices, ["◎ goal set: tests pls", "◎ goal · keeps going"]);
        assert!(a.user_messages.is_empty(), "nothing of the loop to rewind to");
    }

    #[test]
    fn goal_and_rename_offer_what_they_hold_now_to_edit() {
        let (mut a, _rx) = app();
        a.input.set_text("/rename ");
        a.refresh_palette();
        assert!(a.palette.is_none(), "no name yet: nothing to offer");
        a.session_title = "fix the parser".into();
        a.goal = Some(Goal { text: "pass the tests".into(), round: 0, max: 5, state: GoalState::Active });
        a.input.set_text("/goal ");
        a.refresh_palette();
        let p = a.palette.as_ref().unwrap();
        assert_eq!((p.len(), p.current()), (4, Some("pass the tests")), "the goal now, then pause, resume and off");
        a.palette_accept(false);
        assert_eq!(a.input.text(), "/goal pass the tests");
        assert!(a.palette.is_none(), "free text from here on");
        a.input.set_text("/rename ");
        a.refresh_palette();
        a.palette_accept(false);
        assert_eq!(a.input.text(), "/rename fix the parser");
        a.session_title = "Fix: the parser, again!".into();
        a.input.set_text("/export ");
        a.refresh_palette();
        a.palette_accept(false);
        assert_eq!(a.input.text(), "/export fix-the-parser-again.md");
        // Enter on the bare command still shows the goal, it does not resend it
        a.input.set_text("/goal ");
        a.refresh_palette();
        a.palette_accept(true);
        assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
    }

    #[test]
    fn the_argument_list_offers_running_jobs_and_deleted_sessions() {
        use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::Jobs {
            jobs: vec![JobInfo {
                id: 7,
                kind: JobKind::Shell,
                label: "cargo build".into(),
                status: JobStatus::Running,
                elapsed_ms: 0,
                lines: 0,
            }],
        });
        a.input.set_text("/jobs k");
        a.refresh_palette();
        let p = a.palette.as_ref().expect("the values of /jobs");
        assert_eq!(p.len(), 2, "kill (all) and kill 7");
        // down to the running job, Tab writes it, Enter stops that one only
        a.palette.as_mut().unwrap().selected = 1;
        a.palette_accept(false);
        assert_eq!(a.input.text(), "/jobs kill 7");
        a.refresh_palette();
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::KillJob { id: Some(7) })));
        // nothing deleted in this run: no list, the line is free text
        a.input.set_text("/undelete ");
        a.refresh_palette();
        assert!(a.palette.is_none());
        a.handle_agent(AgentEvent::SessionDeleted { id: "s9".into(), title: "old".into() });
        a.input.set_text("/undelete ");
        a.refresh_palette();
        assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("s9"));
        // /resume: a word of the title finds the session, Enter opens it
        a.opts.sessions = vec![("20261101-1".into(), "parser fix".into()), ("20261102-1".into(), "docs".into())];
        a.input.set_text("/resume pars");
        a.refresh_palette();
        assert_eq!(a.palette.as_ref().and_then(|p| p.current()), Some("20261101-1"));
        a.palette_accept(true);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261101-1"));
        // typed whole, by title, without the list
        a.run_command(Command::Resume("Docs".into()));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Resume { id }) if id == "20261102-1"));
        a.run_command(Command::Resume("nope".into()));
        assert!(rx.try_recv().is_err(), "nothing sent for an unknown session");
        assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
    }

    #[test]
    fn a_session_is_deleted_with_two_presses_and_comes_back_with_undelete() {
        let (mut a, mut rx) = app();
        a.opts.sessions = vec![("s1".into(), "first".into()), ("s2".into(), "second".into())];
        a.run_command(Command::Sessions);
        press(&mut a, KeyCode::Delete);
        assert!(rx.try_recv().is_err(), "one press only marks the row");
        let Some(Modal::Picker(p)) = &a.modal else { panic!("the picker stays") };
        assert_eq!(p.armed.as_deref(), Some("s1"));
        let rows = frame(&mut a, 70, 16).join("\n");
        assert!(rows.contains("Del again deletes") && rows.contains("· 2 · Del deletes"), "{rows}");
        press(&mut a, KeyCode::Esc);
        assert!(a.modal.is_some(), "Esc first unmarks, the picker stays");
        press(&mut a, KeyCode::Delete);
        press(&mut a, KeyCode::Delete);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::DeleteSession { id }) if id == "s1"));
        let Some(Modal::Picker(p)) = &a.modal else { panic!("the picker stays open for more") };
        assert_eq!(p.current().map(|i| i.value.as_str()), Some("s2"), "the next row takes its place");
        a.handle_agent(AgentEvent::SessionDeleted { id: "s1".into(), title: "first".into() });
        assert_eq!(a.opts.sessions.len(), 1);
        assert_eq!(a.trashed, ["s1"]);
        a.modal = None;
        a.run_command(Command::Undelete(None));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::RestoreSession { id }) if id == "s1"));
        a.handle_agent(AgentEvent::SessionRestored { id: "s1".into(), title: "first".into() });
        assert_eq!(a.opts.sessions[0].0, "s1");
        assert!(a.trashed.is_empty());
        a.run_command(Command::Undelete(None));
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::ListTrash)),
            "nothing from this run: ask what the trash holds"
        );
        a.handle_agent(AgentEvent::Trash { sessions: vec![] });
        assert!(a.modal.is_none());
        a.handle_agent(AgentEvent::Trash { sessions: vec![("old".into(), "an old one".into())] });
        let rows = frame(&mut a, 70, 16).join("\n");
        assert!(rows.contains("Trash · 1") && rows.contains("an old one"), "{rows}");
        press(&mut a, KeyCode::Enter);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::RestoreSession { id }) if id == "old"));
        // other pickers never delete
        a.modal = Some(Modal::Picker(Picker::new(
            PickKind::Model,
            "Model",
            vec![PickItem { label: "m".into(), value: "m".into(), hint: String::new() }],
            None,
        )));
        press(&mut a, KeyCode::Delete);
        press(&mut a, KeyCode::Delete);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mem_shares_another_sessions_memory_and_shows_it_waiting() {
        let (mut a, mut rx) = app();
        a.session_id = Some("here".into());
        a.opts.sessions = vec![("here".into(), "this one".into()), ("s1".into(), "the parser".into())];
        a.input.set_text("/shared-memory");
        a.submit();
        let Some(Modal::Picker(p)) = &a.modal else { panic!("a picker") };
        assert_eq!(p.items.len(), 1, "this session is not offered");
        press(&mut a, KeyCode::Enter);
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: Some(i), focus: None }) if i == "s1"));
        a.handle_agent(AgentEvent::MemoryShared { pending: vec![("s1".into(), "the parser".into(), 2100)] });
        assert!(crate::ui::hint_right_text(&a).contains("⊕ mem 1 · 2.1k"), "{}", crate::ui::hint_right_text(&a));
        let rows = frame(&mut a, 120, 20).join("\n");
        assert!(rows.contains("what «the parser» knows goes with your next message"), "{rows}");
        a.input.set_text("/shared-memory s1 the tokenizer tabs");
        a.submit();
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: Some(i), focus: Some(f) }) if i == "s1" && f == "the tokenizer tabs")
        );
        a.input.set_text("/shared-memory off");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShareMemory { id: None, .. })));
        a.handle_agent(AgentEvent::MemoryShared { pending: vec![] });
        assert!(!crate::ui::hint_right_text(&a).contains("mem"));
    }

    #[test]
    fn changed_files_add_up_newest_first() {
        let (mut a, _rx) = app();
        let edit = |id: &str, path: &str, old: &str, new: &str| {
            (
                AgentEvent::ToolCall {
                    id: id.into(),
                    name: "edit".into(),
                    args: serde_json::json!({"path": path}),
                    summary: path.into(),
                },
                AgentEvent::ToolDone {
                    id: id.into(),
                    output: ToolOutput::ok("edited").with_display(ToolDisplay::Diff {
                        path: path.into(),
                        old: old.into(),
                        new: new.into(),
                        start_line: 1,
                    }),
                    elapsed_ms: 1,
                },
            )
        };
        for (c, d) in [
            edit("e1", "src/a.rs", "x\n", "y\nz\n"),
            edit("e2", "src/b.rs", "1\n2\n", "1\n"),
            edit("e3", "src/a.rs", "y\n", "w\n"),
        ] {
            a.handle_agent(c);
            a.handle_agent(d);
        }
        assert_eq!(
            a.changed,
            vec![
                ChangedFile { path: "src/a.rs".into(), added: 3, removed: 2 },
                ChangedFile { path: "src/b.rs".into(), added: 0, removed: 1 }
            ]
        );
        // replayed history carries only the text of the result
        let m =
            Message::new(Role::Assistant, vec![Part::tool_call("h1", "edit", serde_json::json!({"path": "src/c.rs"}))]);
        a.handle_agent(AgentEvent::History { index: 5, message: m });
        a.handle_agent(AgentEvent::History {
            index: 6,
            message: Message::tool_results(vec![Part::tool_result("h1", "edited src/c.rs: +4 −1 lines", false)]),
        });
        assert_eq!(a.changed[0], ChangedFile { path: "src/c.rs".into(), added: 4, removed: 1 });
        assert_eq!(parse_counts("edited a: +12 -3 lines"), Some((12, 3)));
        assert_eq!(parse_counts("no counts here"), None);
        a.toggle_files(80);
        assert!(!a.show_files, "too narrow");
        a.toggle_files(120);
        assert!(a.show_files);
    }

    #[test]
    fn a_drag_selects_and_a_click_toggles() {
        let (mut a, _rx) = bare();
        for i in 0..5 {
            a.push(Block::User { text: format!("message {i}"), at: None });
        }
        let out = ToolOutput::ok((1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"));
        a.push(Block::Tool {
            id: "t".into(),
            name: "bash".into(),
            summary: "ls".into(),
            args: serde_json::Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: None,
            folded: true,
        });
        a.transcript_area = Rect::new(0, 1, 80, 20);
        a.layout(80);
        a.settle(20);
        // a click on the tool card selects it; a second click on its head opens it
        let card_line = a.offsets[5];
        let row = (card_line - a.scroll) as u16 + 1;
        a.press_at(3, row);
        a.release_at(3, row);
        assert_eq!(a.selected, Some(5));
        assert!(matches!(a.blocks[5], Block::Tool { folded: true, .. }), "one click only selects");
        a.press_at(3, row);
        a.release_at(3, row);
        assert!(matches!(a.blocks[5], Block::Tool { folded: false, .. }));
        a.layout(80);
        a.settle(20);
        // a click on its body keeps it selected and open; one on another block selects that
        let body = (a.offsets[5] + 3 - a.scroll) as u16 + 1;
        a.press_at(6, body);
        a.release_at(6, body);
        assert!(matches!(a.blocks[5], Block::Tool { folded: false, .. }));
        let first = (a.offsets[4] - a.scroll) as u16 + 1;
        a.press_at(4, first);
        a.release_at(4, first);
        assert_eq!(a.selected, Some(4));
        a.esc();
        assert_eq!(a.selected, None, "Esc lets the block go");
        // a drag from the first message to the second selects both lines
        a.press_at(2, 1);
        a.drag_to(5, 4);
        let sel = a.selection.clone().unwrap();
        assert_eq!(sel.range(), (Pos { line: 0, col: 2 }, Pos { line: 3, col: 5 }));
        assert_eq!(a.line_text(0).as_deref(), Some(" ▌ message 0"));
        a.release_at(5, 4);
        assert!(a.selection.as_ref().is_some_and(|s| !s.dragging), "the selection stays until Esc");
        a.esc();
        assert!(a.selection.is_none());
    }

    fn card(id: &str, lines: usize) -> Block {
        let out = ToolOutput::ok((1..=lines).map(|i| format!("{id} line {i}")).collect::<Vec<_>>().join("\n"));
        Block::Tool {
            id: id.into(),
            name: "bash".into(),
            summary: format!("bash: run {id}"),
            args: serde_json::Value::Null,
            status: ToolStatus::Done,
            progress: vec![],
            output: Some(out),
            started: None,
            elapsed_ms: None,
            folded: true,
        }
    }

    #[test]
    fn the_arrows_walk_the_blocks_and_open_and_fold_them() {
        let (mut a, _rx) = bare();
        a.opts.group_tools = false;
        a.input.push_history("an earlier request");
        a.push(Block::User { text: "run them".into(), at: None });
        for id in ["a", "b", "c"] {
            a.push(card(id, 30));
        }
        a.push(Block::User { text: "thanks".into(), at: None });
        a.transcript_area = Rect::new(0, 1, 80, 4);
        a.layout(80);
        a.settle(4);
        // ↑ on an empty draft is still the history
        press(&mut a, KeyCode::Up);
        assert_eq!(a.input.full_text(), "an earlier request");
        assert_eq!(a.selected, None);
        a.input.clear();
        // Shift+↑ steps into the blocks at the last one that folds (the one-line message does not)
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(a.selected, Some(3));
        press(&mut a, KeyCode::Up);
        assert_eq!(a.selected, Some(2));
        // → opens it, and the view keeps it in sight
        press(&mut a, KeyCode::Right);
        assert!(matches!(a.blocks[2], Block::Tool { folded: false, .. }));
        a.layout(80);
        a.reveal_step();
        assert_eq!(a.scroll, a.offsets[2], "taller than the view: its head at the top");
        assert!(!a.follow);
        press(&mut a, KeyCode::Right);
        assert!(matches!(a.blocks[2], Block::Tool { folded: false, .. }), "→ on an open block keeps it open");
        // ← folds it
        press(&mut a, KeyCode::Left);
        assert!(matches!(a.blocks[2], Block::Tool { folded: true, .. }));
        // ↑ stops at the first; ↓ walks down and past the last leaves for the draft
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Up);
        assert_eq!(a.selected, Some(1));
        a.scroll_by(-1000);
        assert!(!a.follow);
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        assert_eq!(a.selected, Some(3));
        press(&mut a, KeyCode::Down);
        assert_eq!(a.selected, None);
        assert!(a.follow, "back at the draft, the view follows the end again");
        // Enter toggles the selected one; typing lets it go and goes to the draft
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        press(&mut a, KeyCode::Enter);
        assert!(matches!(a.blocks[3], Block::Tool { folded: false, .. }));
        press(&mut a, KeyCode::Char('x'));
        assert_eq!(a.selected, None);
        assert_eq!(a.input.full_text(), "x");
        // with a draft, the arrows are the editor's
        press(&mut a, KeyCode::Left);
        assert!(matches!(a.blocks[3], Block::Tool { folded: false, .. }));
    }

    #[test]
    fn the_steps_of_a_turn_sit_under_the_reply_and_one_line_steps_read_as_a_list() {
        let (mut a, _rx) = bare();
        a.opts.group_tools = false;
        a.push(Block::User { text: "go".into(), at: None });
        a.push(Block::Assistant { text: "On it.".into(), streaming: false, at: None, started: None, took_ms: None });
        a.push(card("a", 2));
        a.push(card("b", 2));
        if let Block::Tool { folded, .. } = &mut a.blocks[3] {
            *folded = false;
        }
        a.push(Block::User { text: "next".into(), at: None });
        a.layout(80);
        let text = |a: &mut App, i: usize| -> Vec<String> {
            a.lines(i, 80).iter().map(crate::render::markdown::text_of).collect()
        };
        assert_eq!(text(&mut a, 0), vec![" ▌ go"], "a margin of one column");
        assert_eq!(text(&mut a, 1), vec![" ◆ On it."]);
        let sh = nexo_core::shell::shell().label();
        assert!(text(&mut a, 2)[0].starts_with(&format!("   ✓ {sh} run a")), "{:?}", text(&mut a, 2));
        let open = text(&mut a, 3);
        assert!(open[0].starts_with(&format!("   ✓ {sh} run b")), "{open:?}");
        assert_eq!(open[1], "   │ b line 1", "the result under its call, behind a guide");
        assert_eq!(open[2], "   └ b line 2", "and the guide ends in a corner");
        // no blank line between a one-line step and the next; one after the open card; two
        // before the next message of the person
        assert_eq!(a.offsets[3], a.offsets[2] + 1);
        assert_eq!(a.offsets[4], a.offsets[3] + open.len() + 2);
        // a narrow terminal keeps every column for the text
        let narrow: Vec<String> = a.lines(3, 20).iter().map(crate::render::markdown::text_of).collect();
        assert!(narrow[0].starts_with(&format!("✓ {sh}")), "{narrow:?}");
    }

    #[test]
    fn the_banner_stays_on_top_of_the_transcript_and_facts_follow_the_agent() {
        let (mut a, _rx) = app();
        assert!(a.has_welcome());
        a.handle_agent(AgentEvent::Notice { level: Level::Info, text: "hello".into() });
        assert!(a.has_welcome(), "a notice sits under the banner");
        a.handle_agent(AgentEvent::PermissionModeChanged { mode: PermissionMode::Auto });
        assert!(matches!(&a.blocks[0], Block::Welcome { permission_mode, .. } if permission_mode == "auto"));
        let bar = crate::ui::title_segments(&a, 200);
        let yolo = bar.iter().flatten().find(|s| s.text == "▲ YOLO").expect("the mode that never asks reads YOLO");
        assert_eq!(yolo.style.fg, Some(a.theme.error), "and is red");
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("go") });
        assert!(a.has_welcome(), "the banner stays and scrolls away with the transcript");
        assert!(matches!(&a.blocks[1], Block::Notice { .. }) && !a.fresh());
        a.handle_agent(AgentEvent::HistoryCleared);
        assert!(a.has_welcome());
    }

    #[test]
    fn a_todo_result_fills_the_list_and_the_goal_follows_the_agent() {
        use nexo_core::tool::TodoStatus;
        let (mut a, mut rx) = app();
        let items = vec![
            TodoItem { text: "read".into(), status: TodoStatus::Done },
            TodoItem { text: "fix".into(), status: TodoStatus::InProgress },
        ];
        a.handle_agent(AgentEvent::ToolCall {
            id: "t1".into(),
            name: "todo".into(),
            args: serde_json::json!({}),
            summary: "2 tasks".into(),
        });
        a.handle_agent(AgentEvent::ToolDone {
            id: "t1".into(),
            output: ToolOutput::ok("ok").with_display(ToolDisplay::Todo { items: items.clone() }),
            elapsed_ms: 1,
        });
        assert_eq!(a.todo, items);
        assert_eq!(crate::render::blocks::todo_counts(&a.todo), (1, 2));
        a.run_command(Command::Goal(GoalArg::Set("pass the tests".into())));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetGoal { text: Some(t) }) if t == "pass the tests"));
        a.handle_agent(AgentEvent::Goal {
            text: Some("pass the tests".into()),
            round: 0,
            max: 3,
            state: Default::default(),
        });
        assert_eq!(a.goal.as_ref().map(|g| g.text.as_str()), Some("pass the tests"));
        a.run_command(Command::Goal(GoalArg::Show));
        assert!(matches!(a.blocks.last(), Some(Block::Notice { text, .. }) if text.contains("pass the tests")));
        a.handle_agent(AgentEvent::Goal {
            text: Some("pass the tests".into()),
            round: 2,
            max: 3,
            state: Default::default(),
        });
        assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal: continuing, round 2/3".into()));
        // the agent stops for an answer, then pauses: each change is said, and /goal shows why
        let goal = |state: GoalState| AgentEvent::Goal { text: Some("pass the tests".into()), round: 0, max: 3, state };
        a.handle_agent(goal(GoalState::Waiting));
        assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal: waiting for your answer".into()));
        a.handle_agent(goal(GoalState::Paused("interrupted".into())));
        assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal paused · /goal resume".into()));
        a.run_command(Command::Goal(GoalArg::Show));
        assert!(
            matches!(a.blocks.last(), Some(Block::Notice { text, .. }) if text.contains("paused (interrupted) · /goal resume"))
        );
        a.run_command(Command::Goal(GoalArg::Resume));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ResumeGoal)));
        a.run_command(Command::Goal(GoalArg::Pause));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::PauseGoal)));
        a.handle_agent(goal(GoalState::Done));
        assert_eq!(a.status.as_ref().map(|s| s.0.clone()), Some("goal met ✓".into()));
        a.run_command(Command::Goal(GoalArg::Clear));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetGoal { text: None })));
        a.handle_agent(AgentEvent::Goal { text: None, round: 0, max: 3, state: Default::default() });
        assert!(a.goal.is_none());
        a.handle_agent(AgentEvent::HistoryCleared);
        assert!(a.todo.is_empty(), "a new transcript starts without a list");
    }

    #[test]
    fn a_cut_of_the_view_shows_by_the_gauge_and_goes_away() {
        let (mut a, _rx) = app();
        let t0 = a.opened;
        a.fx.clock = Some(t0);
        let ctx = |view: u64, history: u64| AgentEvent::Context {
            view_tokens: view,
            history_tokens: history,
            history_bytes: history * 4,
            view_bytes: view * 4,
            messages_in_view: 1,
            messages_total: 1,
            policy: "curator".into(),
            note: None,
            label: "curator".into(),
            detail: None,
        };
        let bar =
            |a: &App| crate::ui::title_segments(a, 200).iter().flatten().map(|s| s.text.clone()).collect::<String>();
        a.handle_agent(ctx(5000, 5000));
        a.handle_agent(ctx(5200, 5400));
        assert!(a.fx.cut.is_none(), "a view that grew is no cut");
        a.handle_agent(ctx(2000, 5600));
        assert_eq!(a.fx.cut.map(|c| c.0), Some(3200));
        assert!(bar(&a).contains("−3.2k tok"), "{}", bar(&a));
        assert!(a.tick_fx(), "the figure keeps the frames coming until it is gone");
        a.fx.clock = Some(t0 + std::time::Duration::from_millis(crate::motion::CUT_MS + 200));
        assert!(!bar(&a).contains("tok "), "{}", bar(&a));
        assert!(!bar(&a).contains("−3.2k"), "{}", bar(&a));
        // a rewind takes the history down too: not the curator
        a.fx.cut = None;
        a.handle_agent(ctx(500, 1000));
        assert!(a.fx.cut.is_none());
    }

    #[test]
    fn the_gauge_uses_the_last_call_and_the_window() {
        let (mut a, _rx) = app();
        assert_eq!(a.gauge(), None);
        a.handle_agent(AgentEvent::Context {
            view_tokens: 400,
            history_tokens: 1000,
            history_bytes: 4000,
            view_bytes: 1600,
            messages_in_view: 1,
            messages_total: 1,
            policy: "plain".into(),
            note: None,
            label: "plain".into(),
            detail: None,
        });
        assert_eq!(a.gauge(), None, "no window known: no share");
        assert_eq!(a.ctx_tokens(), Some(400));
        a.handle_agent(AgentEvent::ModelLimits {
            provider: "p".into(),
            model: "m".into(),
            pricing: None,
            context_window: Some(1000),
            max_output: None,
            source: "config".into(),
        });
        assert_eq!(a.gauge(), Some((400, 1000)), "no usage yet: the view over the window");
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        let u = |i: u64| AgentEvent::Usage {
            turn: Usage { input: i, ..Default::default() },
            call: Usage { input: i, ..Default::default() },
            session: Usage { input: i, ..Default::default() },
            cost_usd: 0.0,
        };
        a.handle_agent(u(1000));
        a.handle_agent(u(2500));
        assert_eq!(a.last_call_input, 2500, "each event is one call: the last one is what was sent");
        assert_eq!(a.turn_usage.input, 3500, "the turn adds its calls up");
        a.handle_agent(AgentEvent::History { index: 0, message: Message::user("go") });
        a.handle_agent(AgentEvent::ModelLimits {
            provider: "p".into(),
            model: "m".into(),
            pricing: None,
            context_window: Some(128_000),
            max_output: None,
            source: "preset".into(),
        });
        assert_eq!(a.gauge(), Some((2500, 128_000)));
        assert!(matches!(a.blocks.last(), Some(Block::Model { .. })));
        a.handle_agent(AgentEvent::TurnStart { turn: 2 });
        a.handle_agent(u(3000));
        assert_eq!(a.last_call_input, 3000);
        // a call the client retried: the turn counts both attempts, the gauge the one that answered
        a.handle_agent(AgentEvent::Usage {
            turn: Usage { input: 5000, ..Default::default() },
            call: Usage { input: 2000, ..Default::default() },
            session: Usage::default(),
            cost_usd: 0.0,
        });
        assert_eq!((a.last_call_input, a.call_inputs.last().copied()), (2000, Some(2000)));
        assert_eq!(a.turn_usage.input, 3000 + 5000, "the turn counts what was spent");
    }

    #[test]
    fn pricing_parses_and_reaches_the_agent() {
        assert_eq!(
            parse_pricing("in=0.28 out=0.42 cache=0.028").unwrap(),
            Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 }
        );
        assert_eq!(parse_pricing("input=$1, output=2").unwrap().output, 2.0);
        assert!(parse_pricing("in=x").is_err());
        assert!(parse_pricing("").is_err());
        assert!(parse_pricing("foo=1").is_err());
        let (mut a, mut rx) = app();
        a.input.set_text("/pricing in=1 out=2");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPricing { pricing }) if pricing.output == 2.0));
        a.input.set_text("/pricing");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowModel)));
        a.input.set_text("/mouse");
        a.submit();
        assert_eq!(a.mouse_request, Some(false));
        assert!(!a.mouse_on);
    }

    #[test]
    fn the_theme_picker_previews_and_restores() {
        let (mut a, _rx) = app();
        assert_eq!(a.theme.name, "dark");
        a.run_command(Command::Theme(None));
        if let Some(Modal::Picker(p)) = &mut a.modal {
            p.selected = crate::theme::NAMES.iter().position(|n| *n == "nord").unwrap();
        }
        a.modal_out(ModalOut::Nothing);
        assert_eq!(a.theme.name, "nord", "the hovered theme shows");
        a.modal_out(ModalOut::Close);
        assert_eq!(a.theme.name, "dark", "Esc restores");
        a.set_theme("gruvbox-dark", false);
        assert_eq!(a.theme.name, "gruvbox");
        a.set_theme("nope", false);
        assert!(matches!(a.blocks.last(), Some(Block::Notice { .. })));
    }

    #[test]
    fn undo_init_status_tools_and_the_editor_key() {
        let (mut a, mut rx) = app();
        a.input.set_text("/undo");
        a.submit();
        assert!(rx.try_recv().is_err(), "nothing to undo yet");
        a.handle_agent(AgentEvent::History { index: 0, message: nexo_core::message::Message::user("make it blue") });
        a.input.set_text("/undo");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Rewind { index: 0 })));
        assert_eq!(a.input.text(), "make it blue", "the request comes back to the editor");
        a.input.set_text("/init");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text.contains("AGENTS.md")));
        a.input.set_text("/status");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowStatus)));
        a.input.set_text("/tools");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowTools)));
        crate::event::key(
            &mut a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('g'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        );
        assert!(a.draft_edit_request);
    }

    #[test]
    fn a_command_of_ones_own_is_offered_and_sends_its_text() {
        let (mut a, mut rx) = app();
        a.custom_commands = vec![crate::custom::CustomCommand {
            name: "review".into(),
            description: "review against our rules".into(),
            body: "Review $ARGUMENTS against CONTRIBUTING.md.".into(),
            path: PathBuf::from("review.md"),
        }];
        a.input.set_text("/rev");
        a.refresh_palette();
        let p = a.palette.as_ref().expect("the palette");
        let (rows, _) = p.rows(20);
        let row = rows.iter().find(|r| r.label == "review").expect("the command is offered");
        assert!(row.hint.contains("review against our rules") && row.hint.contains("yours"));
        a.input.set_text("/review src/lib.rs");
        a.submit();
        assert!(
            matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "Review src/lib.rs against CONTRIBUTING.md.")
        );
    }

    #[test]
    fn ctrl_r_finds_an_earlier_request_and_the_history_outlives_the_run() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("prompt-history.jsonl");
        let (mut a, _rx) = app();
        a.input.persist_history(file.clone());
        for t in ["fix the parser", "add a test\nfor the parser", "run the suite"] {
            a.input.set_text(t);
            a.submit();
        }
        crate::event::key(
            &mut a,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('r'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
        );
        let Some(Modal::Picker(p)) = &a.modal else { panic!("a history picker") };
        assert_eq!(p.items[0].label, "run the suite", "newest first");
        assert_eq!(p.items[1].hint, "+1 lines");
        a.modal_out(ModalOut::Pick { kind: PickKind::History, value: "fix the parser".into() });
        assert_eq!(a.input.text(), "fix the parser");
        // a new run starts from the file
        let (mut b, _rx2) = app();
        b.input.persist_history(file);
        assert_eq!(b.input.history().len(), 3);
        assert_eq!(b.input.history()[1], "add a test\nfor the parser");
    }

    #[test]
    fn the_cost_rolls_up_like_a_meter_and_settles_on_the_spend() {
        let (mut a, _rx) = app();
        a.opts.animations = true;
        let t0 = Instant::now();
        a.fx.clock = Some(t0);
        a.tick();
        a.cost = 0.0123;
        let first = frame(&mut a, 120, 10).last().unwrap().clone();
        assert!(first.contains("$0.0000"), "starts where it was: {first}");
        let mut seen = vec![];
        for ms in (0..=500u64).step_by(25) {
            a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
            a.tick();
            seen.push(a.cost_shown);
        }
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "never runs backwards");
        assert!(seen.iter().any(|c| *c > 0.0 && *c < 0.0123), "passes through the figures between");
        let last = frame(&mut a, 120, 10).last().unwrap().clone();
        assert!(last.contains("$0.0123"), "{last}");
        // without animations the figure is the spend at once
        a.opts.animations = false;
        a.cost = 0.5;
        assert!(frame(&mut a, 120, 10).last().unwrap().contains("$0.5000"));
    }

    #[test]
    fn budget_commands_and_the_spend_in_the_title() {
        let (mut a, mut rx) = app();
        for (typed, want) in [
            ("/budget", None),
            ("/budget 1.5", Some(Some(1.5))),
            ("/budget $2", Some(Some(2.0))),
            ("/budget off", Some(None)),
        ] {
            a.input.set_text(typed);
            a.submit();
            match (rx.try_recv(), want) {
                (Ok(AgentCommand::ShowBudget), None) => {}
                (Ok(AgentCommand::SetBudget { limit }), Some(l)) => assert_eq!(limit, l, "{typed}"),
                (other, _) => panic!("{typed}: {other:?}"),
            }
        }
        a.input.set_text("/budget lots");
        a.submit();
        assert!(rx.try_recv().is_err(), "not an amount: nothing sent");
        a.cost = 0.9;
        a.cost_shown = 0.9;
        a.handle_agent(AgentEvent::Budget {
            session_cost: 0.9,
            day_cost: 1.2,
            session_limit: Some(1.0),
            day_limit: None,
        });
        let backend = ratatui::backend::TestBackend::new(120, 10);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let top = crate::ui::rows_of(term.backend().buffer()).last().unwrap().clone();
        assert!(top.contains("$0.9000/1.00"), "{top}");
    }

    #[test]
    fn a_pasted_image_is_kept_and_goes_with_the_request() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_CACHE_DIR", d.path());
        // a 2×1 picture: one red pixel, one transparent
        let png = crate::clipboard::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let back = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!((back.width(), back.height(), back.get_pixel(0, 0).0), (2, 1, [255, 0, 0, 255]));
        assert!(crate::clipboard::encode_png(3, 3, &[0; 4]).is_err(), "pixels that do not fill the size");
        let (mut a, mut rx) = app();
        a.input.set_text("what is wrong here");
        a.attach_png(&png);
        std::env::remove_var("NEXO_CACHE_DIR");
        let draft = a.input.text();
        let path = draft.split_whitespace().find_map(|w| w.strip_prefix('@')).unwrap().to_string();
        assert!(path.starts_with(&d.path().join("pastes").display().to_string()) && path.ends_with(".png"), "{draft}");
        assert_eq!(std::fs::read(&path).unwrap(), png);
        a.submit();
        match rx.try_recv() {
            Ok(AgentCommand::Submit { attachments, .. }) => assert_eq!(attachments[0].path, PathBuf::from(&path)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_paste_that_comes_as_keys_is_one_draft_and_a_long_one_is_sent_whole() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let key = |c: KeyCode, kind| Event::Key(KeyEvent::new_with_kind(c, KeyModifiers::NONE, kind));
        // the Windows console: each character a press and a release, Enter between the lines
        let text: String = (1..=10).map(|n| format!("row {n}\n")).collect();
        let mut batch = Vec::new();
        for c in text.chars() {
            let code = if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) };
            batch.push(key(code, KeyEventKind::Press));
            batch.push(key(code, KeyEventKind::Release));
        }
        let (mut a, mut rx) = app();
        assert!(crate::event::may_start_paste(&batch[0]) && a.takes_paste());
        let pasted = crate::event::paste_burst(&batch, false).expect("a burst of keys is a paste");
        assert_eq!(pasted, text);
        crate::event::handle(&mut a, Event::Paste(pasted));
        assert!(rx.try_recv().is_err(), "its Enters did not send anything");
        assert_eq!(a.input.text(), "[Pasted text #1 +10 lines]");
        a.input.insert_paste(" what does it print?");
        a.submit();
        match rx.try_recv() {
            Ok(AgentCommand::Submit { text: sent, .. }) => {
                assert!(sent.starts_with("row 1\nrow 2\n") && sent.ends_with("row 10\n what does it print?"), "{sent}")
            }
            other => panic!("{other:?}"),
        }
        // typing is not a paste: a lone key, or keys with Ctrl among them
        assert!(crate::event::paste_burst(&[key(KeyCode::Char('a'), KeyEventKind::Press)], false).is_none());
        let ctrl = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(
            crate::event::paste_burst(&[batch[0].clone(), batch[2].clone(), batch[4].clone(), ctrl], false).is_none()
        );
        // AltGr (Ctrl+Alt) characters are typed text
        let at = Event::Key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        assert_eq!(crate::event::paste_burst(&[at.clone(), at.clone(), at], false).as_deref(), Some("@@@"));
        // one line and its Enter at once is a paste too: a big paste on Windows comes in
        // pieces, and a piece that is one line must not send it
        let piece: Vec<Event> = "fn main() {\n"
            .chars()
            .map(|c| if c == '\n' { KeyCode::Enter } else { KeyCode::Char(c) })
            .map(|c| key(c, KeyEventKind::Press))
            .collect();
        assert_eq!(crate::event::paste_burst(&piece, false).as_deref(), Some("fn main() {\n"));
        // the tail of a paste is the paste, however short: a lone Enter included
        assert!(crate::event::paste_burst(&[key(KeyCode::Enter, KeyEventKind::Press)], false).is_none());
        assert_eq!(crate::event::paste_burst(&[key(KeyCode::Enter, KeyEventKind::Press)], true).as_deref(), Some("\n"));
        // the mouse moving through it does not break it
        let moved = Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        let mut with_mouse = piece.clone();
        with_mouse.insert(2, moved);
        assert_eq!(crate::event::paste_burst(&with_mouse, false).as_deref(), Some("fn main() {\n"));
        // bracketed paste's markers that come as keys (Windows Terminal) are dropped
        let marked: Vec<Event> = "\x1b[200~a\nb\x1b[201~"
            .chars()
            .map(|c| match c {
                '\n' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
                c => KeyCode::Char(c),
            })
            .map(|c| key(c, KeyEventKind::Press))
            .collect();
        assert!(crate::event::may_start_paste(&marked[0]));
        let got = crate::event::paste_burst(&marked, false).expect("a marked paste");
        assert_eq!(crate::event::unmark(&got), "a\nb");
        // a lone Esc is a key, not a paste
        assert!(crate::event::paste_burst(&[key(KeyCode::Esc, KeyEventKind::Press)], true).is_none());
    }

    fn question(id: &str, multi: bool, plan: Option<&str>) -> nexo_core::permission::Question {
        let opt =
            |l: &str| nexo_core::permission::QuestionOption { label: l.into(), description: format!("about {l}") };
        nexo_core::permission::Question {
            id: id.into(),
            header: "Pick".into(),
            question: "Which one?".into(),
            options: vec![opt("red"), opt("green"), opt("blue")],
            multi,
            plan: plan.map(str::to_string),
        }
    }

    fn press(a: &mut App, code: KeyCode) {
        crate::event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn answer_of(rx: &mut mpsc::UnboundedReceiver<AgentCommand>) -> Option<(String, Option<String>)> {
        std::iter::from_fn(|| rx.try_recv().ok()).find_map(|c| match c {
            AgentCommand::Answer { id, answer } => Some((id, answer)),
            _ => None,
        })
    }

    #[test]
    fn a_question_is_answered_with_the_arrows_a_digit_or_words_of_ones_own() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::Question { question: question("q1", false, None) });
        assert!(matches!(&a.modal, Some(Modal::Question(_))));
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Enter);
        assert_eq!(answer_of(&mut rx), Some(("q1".into(), Some("green".into()))));
        assert!(a.modal.is_none());

        a.handle_agent(AgentEvent::Question { question: question("q2", false, None) });
        press(&mut a, KeyCode::Char('3'));
        assert_eq!(answer_of(&mut rx), Some(("q2".into(), Some("blue".into()))));

        // the last row takes an answer of one's own
        a.handle_agent(AgentEvent::Question { question: question("q3", false, None) });
        press(&mut a, KeyCode::Char('4'));
        for c in "teal".chars() {
            press(&mut a, KeyCode::Char(c));
        }
        press(&mut a, KeyCode::Enter);
        assert_eq!(answer_of(&mut rx), Some(("q3".into(), Some("teal".into()))));

        // Esc closes it without an answer
        a.handle_agent(AgentEvent::Question { question: question("q4", false, None) });
        press(&mut a, KeyCode::Esc);
        assert_eq!(answer_of(&mut rx), Some(("q4".into(), None)));
    }

    #[test]
    fn several_options_are_ticked_and_sent_together() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::Question { question: question("m", true, None) });
        press(&mut a, KeyCode::Char(' '));
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Char(' '));
        assert!(a.modal.is_some(), "a tick does not send");
        press(&mut a, KeyCode::Enter);
        assert_eq!(answer_of(&mut rx), Some(("m".into(), Some("red, blue".into()))));
    }

    #[test]
    fn questions_wait_behind_an_open_modal_and_go_when_resolved_elsewhere() {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::Question { question: question("a", false, None) });
        a.handle_agent(AgentEvent::Question { question: question("b", false, None) });
        a.handle_agent(AgentEvent::QuestionResolved { id: "a".into(), answer: None });
        assert!(matches!(&a.modal, Some(Modal::Question(q)) if q.q.id == "b"));
        a.handle_agent(AgentEvent::QuestionResolved { id: "b".into(), answer: None });
        assert!(a.modal.is_none());
    }

    #[test]
    fn a_plan_shows_as_markdown_above_its_options() {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::Question {
            question: question("p", false, Some("# Plan\n\n1. **read** the parser")),
        });
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let screen = crate::ui::rows_of(term.backend().buffer()).join("\n");
        assert!(screen.contains(" plan "), "{screen}");
        assert!(screen.contains("read the parser") && !screen.contains("**read**"), "{screen}");
        assert!(screen.contains("❯ 1. red") && screen.contains("Tell the model what to change"), "{screen}");
        assert_eq!(a.picker_rows.len(), 4, "three options and the row of one's own");
    }

    #[test]
    fn a_question_and_its_options_read_as_markdown() {
        let (mut a, _rx) = app();
        let mut q = question("m", false, None);
        q.question = "## Which **fast** parser?".into();
        q.options[0].label = "**Fast** `nom`".into();
        q.options[0].description = "uses *less* memory".into();
        a.handle_agent(AgentEvent::Question { question: q });
        screen(&mut a, 100, 44); // the dialog pops in on this frame
        a.fx.clock = Some(a.now() + std::time::Duration::from_secs(2));
        let screen = screen(&mut a, 100, 44).join("\n");
        assert!(!screen.contains("##") && !screen.contains("**") && !screen.contains('`'), "{screen}");
        assert!(screen.contains("Which fast parser?"), "{screen}");
        assert!(screen.contains("1. Fast nom") && screen.contains("uses less memory"), "{screen}");
    }

    #[test]
    fn a_slash_word_is_known_unknown_or_still_being_typed() {
        use crate::ui::{slash_word, SlashWord};
        assert_eq!(slash_word("/plan add a cache", &[]), Some(("/plan", SlashWord::Known)));
        assert_eq!(slash_word("/pla", &[]), Some(("/pla", SlashWord::Typing)));
        assert_eq!(slash_word("/nosuch x", &[]), Some(("/nosuch", SlashWord::Unknown)));
        assert_eq!(slash_word("/etc/hosts is odd", &[]), None);
        assert_eq!(slash_word("hello /plan", &[]), None);
    }

    #[test]
    fn plan_mode_shows_on_the_rule_and_a_known_command_is_coloured() {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::PermissionModeChanged { mode: PermissionMode::Plan });
        // past the wipe: the rule has its new colour and name
        a.fx.clock = Some(a.now() + std::time::Duration::from_secs(2));
        a.input.set_text("/plan x");
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = crate::ui::rows_of(&buf);
        assert!(rows.iter().any(|r| r.contains("◇ plan")), "{}", rows.join("\n"));
        let e = a.editor_area;
        let cell = &buf[(e.x, e.y)];
        assert_eq!(cell.symbol(), "/");
        assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD), "the command word is lit");
    }

    #[test]
    fn shift_tab_walks_the_modes_through_plan_and_slash_plan_sends_the_text() {
        let (mut a, mut rx) = app();
        let mut modes = Vec::new();
        for m in [PermissionMode::Ask, PermissionMode::AcceptEdits, PermissionMode::Plan] {
            a.permission_mode = m;
            crate::event::key(&mut a, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
            if let Ok(AgentCommand::SetPermissionMode(n)) = rx.try_recv() {
                modes.push(n);
            }
        }
        assert_eq!(modes, [PermissionMode::AcceptEdits, PermissionMode::Plan, PermissionMode::Ask]);
        a.permission_mode = PermissionMode::Ask;
        a.input.set_text("/plan add a cache");
        a.submit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::SetPermissionMode(PermissionMode::Plan))));
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Submit { text, .. }) if text == "add a cache"));
    }

    #[test]
    fn a_long_turn_rings_when_it_ends_and_a_short_one_does_not() {
        let (mut a, _rx) = app();
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
        assert!(a.bell_request.is_none());
        a.handle_agent(AgentEvent::TurnStart { turn: 2 });
        a.turn_started = Some(Instant::now() - std::time::Duration::from_secs(20));
        a.handle_agent(AgentEvent::TurnEnd { reason: TurnEnd::Done });
        assert!(a.bell_request.as_deref().is_some_and(|t| t.starts_with("nexo: done after")), "{:?}", a.bell_request);
    }

    #[test]
    fn prompt_edit_waits_for_the_prompt_when_the_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut a, mut rx) = app();
        let path = dir.path().join("prompt.md");
        // the file is missing and no prompt was seen: the agent is asked first
        a.system_prompt = None;
        assert!(a.edit_request.is_none());
        std::env::set_var("NEXO_CONFIG_DIR", dir.path());
        a.prompt_edit();
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::ShowPrompt)));
        a.handle_agent(AgentEvent::SystemPrompt { text: "You are nexo.".into() });
        std::env::remove_var("NEXO_CONFIG_DIR");
        assert_eq!(a.edit_request.as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "You are nexo.\n");
        assert!(!matches!(a.blocks.last(), Some(Block::Shell { .. })), "no card when the prompt was fetched to edit");
    }

    #[test]
    fn permissions_queue_behind_the_open_modal() {
        let (mut a, mut rx) = app();
        let req = |id: &str| PermissionRequest {
            id: id.into(),
            tool: "bash".into(),
            kind: nexo_core::tool::ToolKind::Exec,
            summary: "x".into(),
            detail: ToolDisplay::Plain,
            suggested_rules: vec![],
        };
        a.handle_agent(AgentEvent::PermissionAsk { request: req("p1") });
        a.handle_agent(AgentEvent::PermissionAsk { request: req("p2") });
        assert_eq!(a.modal.as_ref().and_then(Modal::id), Some("p1"));
        a.modal_out(ModalOut::Permission { id: "p1".into(), decision: PermissionDecision::Allow });
        assert!(matches!(rx.try_recv(), Ok(AgentCommand::Permission { id, .. }) if id == "p1"));
        assert_eq!(a.modal.as_ref().and_then(Modal::id), Some("p2"));
    }

    #[test]
    fn title_pieces_leave_in_order_and_the_session_name_is_bounded() {
        use crate::ui::{title_segments, TITLE_COLS};
        let (mut a, _rx) = app();
        a.cwd = std::path::PathBuf::from("/srv/project-alpha");
        a.branch = Some("main".into());
        a.goal = Some(Goal { text: "ship it".into(), round: 0, max: 5, state: GoalState::Active });
        a.handle_agent(AgentEvent::ModelLimits {
            provider: "p".into(),
            model: "m".into(),
            pricing: None,
            context_window: Some(1000),
            max_output: None,
            source: "config".into(),
        });
        a.handle_agent(AgentEvent::Context {
            view_tokens: 400,
            history_tokens: 1000,
            history_bytes: 4000,
            view_bytes: 1600,
            messages_in_view: 1,
            messages_total: 1,
            policy: "curator".into(),
            note: None,
            label: "curator −60%".into(),
            detail: None,
        });
        let texts = |w: usize| -> Vec<String> {
            title_segments(&a, w).iter().map(|g| g.iter().map(|s| s.text.clone()).collect::<String>()).collect()
        };
        let full = texts(400);
        assert!(full.iter().any(|t| t == "mem 1.0k"), "{full:?}");
        assert!(full.iter().any(|t| t.contains("main")), "{full:?}");
        assert!(full.iter().any(|t| t.contains("◎ goal")), "{full:?}");
        let mut last = full.len();
        for w in (20..400).rev() {
            let n = texts(w).len();
            assert!(n <= last, "a narrower bar never has more pieces ({w})");
            last = n;
        }
        // the order: directory, then branch, then memory, then the counts, then the model shortened
        let order: Vec<Vec<String>> = (20..400).rev().map(texts).collect();
        let first_without = |needle: &str| order.iter().position(|t| !t.iter().any(|x| x.contains(needle)));
        let dir = first_without("project-alpha").unwrap();
        let branch = first_without("main").unwrap();
        let mem = first_without("mem ").unwrap();
        let counts = first_without("/1.0k").unwrap();
        assert!(dir <= branch && branch <= mem && mem <= counts, "{dir} {branch} {mem} {counts}");
        a.session_title = "a very long session name that would eat the key hints".into();
        a.session_id = Some("20261027-000000-abcd".into());
        let bar = crate::ui::hint_right_text(&a);
        assert!(bar.contains('…'));
        assert!(crate::render::wrap::width(&bar) < TITLE_COLS + 30, "{bar}");
    }

    fn pointer(a: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) {
        let m = crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        crate::event::mouse(a, m);
    }

    fn frame(a: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, a)).unwrap();
        crate::ui::rows_of(term.backend().buffer())
    }

    #[test]
    fn the_scrollback_text_holds_every_block_wrapped_with_the_screen_gaps() {
        let (mut a, _rx) = app();
        a.push(Block::Assistant {
            text: "first answer".into(),
            streaming: false,
            at: None,
            started: None,
            took_ms: None,
        });
        a.push(Block::User { text: "a question that is long enough to wrap around".into(), at: None });
        long_answer(&mut a, 0);
        let text = crate::inline::scrollback_text(&mut a, 30);
        assert!(text.contains("first answer") && text.contains("line 59"), "{text}");
        assert!(text.lines().all(|l| l.chars().count() < 30), "wrapped to the width");
        assert!(text.lines().count() > 60 && !text.ends_with('\n'));
    }

    fn long_answer(a: &mut App, from: usize) {
        let text: Vec<String> = (from..from + 60).map(|i| format!("line {i}")).collect();
        a.push(Block::Assistant { text: text.join("\n\n"), streaming: false, at: None, started: None, took_ms: None });
    }

    #[test]
    fn the_transcript_thumb_drags_one_to_one_and_selects_no_text() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, _rx) = app();
        long_answer(&mut a, 0);
        frame(&mut a, 60, 20);
        let bar = a.transcript_bar.expect("a long transcript has a bar");
        assert!(a.follow && bar.offset == bar.max_offset(), "at the end");
        let (start, size) = bar.thumb();
        assert!(size >= 1 && size < bar.len);
        // held by its top row, from the column left of the bar (the wider grip)
        pointer(&mut a, K::Down(MouseButton::Left), bar.x - 1, bar.y + start);
        assert!(a.bar_drag.is_some());
        pointer(&mut a, K::Drag(MouseButton::Left), bar.x - 1, bar.y);
        assert_eq!(a.scroll, 0);
        assert!(!a.follow);
        assert_eq!(a.view_top(), 0, "a drag does not glide");
        assert!(a.selection.is_none() && a.press.is_none(), "no text selected on the way");
        // halfway down the track: the offset follows the thumb's top
        pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y + (bar.len - size) / 2);
        let mid = a.scroll;
        assert!(mid > 0 && mid < bar.max_offset(), "{mid}");
        pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + 3);
        assert!(a.bar_drag.is_none());
        // the drag is over: the pointer elsewhere moves nothing
        pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y);
        assert_eq!(a.scroll, mid);
    }

    #[test]
    fn a_click_on_the_track_glides_there_and_the_wheel_glides_too() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, _rx) = app();
        long_answer(&mut a, 0);
        frame(&mut a, 60, 20);
        a.scroll_to_line(0, false);
        frame(&mut a, 60, 20);
        let bar = a.transcript_bar.unwrap();
        pointer(&mut a, K::Down(MouseButton::Left), bar.x, bar.y + bar.len - 1);
        pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + bar.len - 1);
        assert!(a.scroll > bar.len as usize, "jumped down the transcript: {}", a.scroll);
        frame(&mut a, 60, 20);
        assert!(a.scroll_fx.is_moving(Instant::now()), "on its way there");
        assert!(a.view_top() < a.scroll);
        a.dirty = false;
        a.tick();
        assert!(a.dirty, "a glide keeps the frames coming");

        // with the animations off, everything lands at once
        let (mut a, _rx) = app();
        a.opts.animations = false;
        long_answer(&mut a, 0);
        frame(&mut a, 60, 20);
        pointer(&mut a, K::ScrollUp, 10, 5);
        frame(&mut a, 60, 20);
        assert_eq!(a.view_top(), a.scroll);
        assert!(!a.scroll_fx.is_moving(Instant::now()));
    }

    #[test]
    fn the_thumb_lights_up_under_the_pointer() {
        use crossterm::event::MouseEventKind as K;
        let (mut a, _rx) = app();
        long_answer(&mut a, 0);
        let before = frame(&mut a, 60, 20);
        assert!(!before.iter().any(|r| r.contains('▐')));
        let bar = a.transcript_bar.unwrap();
        a.dirty = false;
        pointer(&mut a, K::Moved, bar.x, bar.y + 1);
        assert_eq!(a.hover, Hover::Bar(scrollbar::Target::Transcript));
        assert!(a.dirty, "a new hover draws a frame");
        let lit = frame(&mut a, 60, 20);
        let (start, _) = bar.thumb();
        assert!(lit[(bar.y + start) as usize].ends_with("▐█"), "{:?}", lit[(bar.y + start) as usize]);
        // moving within it changes nothing
        a.dirty = false;
        pointer(&mut a, K::Moved, bar.x - 1, bar.y + 2);
        assert!(!a.dirty);
        pointer(&mut a, K::Moved, 3, bar.y + 2);
        assert_ne!(a.hover, Hover::Bar(scrollbar::Target::Transcript));
    }

    #[test]
    fn the_new_pill_beats_and_a_click_on_it_goes_to_the_end() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, _rx) = app();
        long_answer(&mut a, 0);
        frame(&mut a, 60, 20);
        a.scroll_to_line(0, false);
        frame(&mut a, 60, 20);
        long_answer(&mut a, 100);
        let rows = frame(&mut a, 60, 20);
        assert!(a.unseen() > 0);
        assert!(rows.iter().any(|r| r.contains("new")), "{rows:?}");
        assert!(a.unseen_at.is_some(), "the thumb beats");
        assert!(anim::pulse(a.unseen_at, Instant::now() + std::time::Duration::from_millis(200)) > 0.0);
        let pill = a.pill;
        assert!(pill.width > 0);
        pointer(&mut a, K::Moved, pill.x + 1, pill.y);
        assert_eq!(a.hover, Hover::Pill);
        pointer(&mut a, K::Down(MouseButton::Left), pill.x + 1, pill.y);
        assert!(a.follow);
        frame(&mut a, 60, 20);
        assert_eq!(a.unseen(), 0);
    }

    #[test]
    fn the_status_bar_opens_the_model_the_context_and_the_usage() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, _rx) = app();
        a.cost = 0.25;
        frame(&mut a, 120, 12);
        let at = |a: &App, h: StatusHit| a.status_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r);
        let model = at(&a, StatusHit::Model).expect("the model is on the bar");
        let cost = at(&a, StatusHit::Cost).expect("so is the cost");
        assert!(model.x + model.width <= cost.x);
        pointer(&mut a, K::Moved, model.x, model.y);
        assert_eq!(a.hover, Hover::Status(StatusHit::Model));
        pointer(&mut a, K::Down(MouseButton::Left), model.x, model.y);
        assert!(matches!(a.modal, Some(Modal::Setup(_))), "the model opens its picker");
        a.modal = None;
        frame(&mut a, 120, 12);
        pointer(&mut a, K::Down(MouseButton::Left), cost.x + 1, cost.y);
        assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 1), "the cost opens the usage");
        a.modal = None;
        frame(&mut a, 120, 12);
        // anywhere else on it, the context window as before
        let row = a.status_row;
        pointer(&mut a, K::Down(MouseButton::Left), 1, row);
        assert!(matches!(&a.modal, Some(Modal::Info(i)) if i.tab == 0));
    }

    fn drain(rx: &mut mpsc::UnboundedReceiver<AgentCommand>) -> Vec<AgentCommand> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn queue_two(a: &mut App, rx: &mut mpsc::UnboundedReceiver<AgentCommand>) {
        a.busy = true;
        for t in ["first request", "second request"] {
            a.input.set_text(t);
            a.submit();
        }
        a.status = None;
        drain(rx);
    }

    #[test]
    fn up_on_an_empty_draft_takes_the_last_queued_message_back_to_edit_it() {
        let (mut a, mut rx) = app();
        queue_two(&mut a, &mut rx);
        assert_eq!((a.queued, a.queued_msgs.len()), (2, 2));
        let rows = frame(&mut a, 100, 20);
        assert!(rows.iter().any(|r| r.contains("◷ first request")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("◷ second request")), "{rows:?}");
        press(&mut a, KeyCode::Up);
        assert_eq!(a.input.full_text(), "second request");
        assert_eq!(a.queued_msgs, vec!["first request".to_string()]);
        assert_eq!(a.queued, 1);
        assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 1, text: "second request".into() }]);
        let rows = frame(&mut a, 100, 20);
        assert!(!rows.iter().any(|r| r.contains("◷ second request")), "the row went with it");
        // Enter sends the edited text and queues it again
        a.input.set_text("second request, edited");
        a.submit();
        assert!(matches!(&drain(&mut rx)[..], [AgentCommand::Submit { text, .. }] if text == "second request, edited"));
        assert_eq!(a.queued_msgs, vec!["first request".to_string(), "second request, edited".to_string()]);
        // the agent starts the first one: the count and the texts follow
        a.handle_agent(AgentEvent::Queued { pending: 1 });
        assert_eq!(a.queued_msgs, vec!["second request, edited".to_string()]);
        a.handle_agent(AgentEvent::TurnStart { turn: 2 });
        assert_eq!((a.queued, a.queued_msgs.len()), (0, 0));
    }

    #[test]
    fn a_click_on_a_queued_row_takes_that_one_back_and_a_full_draft_is_kept() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, mut rx) = app();
        queue_two(&mut a, &mut rx);
        frame(&mut a, 100, 20);
        let (row, i) = a.queue_rows[0];
        assert_eq!(i, 0);
        a.input.set_text("half typed");
        pointer(&mut a, K::Down(MouseButton::Left), row.x + 2, row.y);
        assert_eq!(a.input.full_text(), "half typed", "a draft in progress is never overwritten");
        assert_eq!(a.queued_msgs.len(), 2);
        a.input.clear();
        pointer(&mut a, K::Down(MouseButton::Left), row.x + 2, row.y);
        assert_eq!(a.input.full_text(), "first request");
        assert_eq!(a.queued_msgs, vec!["second request".to_string()]);
        assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 0, text: "first request".into() }]);
    }

    fn lit_cells(a: &mut App, r: Rect) -> bool {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 20)).unwrap();
        term.draw(|f| crate::ui::draw(f, a)).unwrap();
        let buf = term.backend().buffer();
        (r.x + 1..r.x + r.width).all(|x| buf[(x, r.y)].modifier.contains(ratatui::style::Modifier::UNDERLINED))
    }

    #[test]
    fn the_pieces_of_the_keys_row_light_up_and_open_their_views() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        use nexo_core::jobs::{JobInfo, JobKind, JobStatus};
        use nexo_core::tool::TodoStatus;
        let (mut a, mut rx) = app();
        queue_two(&mut a, &mut rx);
        a.todo = vec![
            TodoItem { text: "read".into(), status: TodoStatus::Done },
            TodoItem { text: "fix".into(), status: TodoStatus::InProgress },
            TodoItem { text: "ship".into(), status: TodoStatus::Pending },
        ];
        a.handle_agent(AgentEvent::Jobs {
            jobs: vec![JobInfo {
                id: 7,
                kind: JobKind::Shell,
                label: "cargo build".into(),
                status: JobStatus::Running,
                elapsed_ms: 0,
                lines: 0,
            }],
        });
        a.shared = vec![("s1".into(), "the parser".into(), 2100)];
        a.session_id = Some("20261104-1".into());
        a.opts.sessions = vec![("20261103-1".into(), "docs".into())];
        a.status = None;
        a.dirty = true;
        frame(&mut a, 200, 20);
        let hits: Vec<_> = a.hint_hits.clone();
        let find = |h: HintHit| hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap_or_else(|| panic!("{h:?}"));
        for h in [HintHit::Todo, HintHit::Jobs, HintHit::Mem, HintHit::Queued, HintHit::Session] {
            let r = find(h);
            assert!(!lit_cells(&mut a, r), "{h:?} is plain until the pointer comes");
            pointer(&mut a, K::Moved, r.x + 1, r.y);
            assert_eq!(a.hover, Hover::Hint(h));
            assert!(lit_cells(&mut a, r), "{h:?} is underlined under the pointer");
            pointer(&mut a, K::Moved, 0, 0);
            assert_eq!(a.hover, Hover::Nothing);
        }
        let click = |a: &mut App, h: HintHit| {
            frame(a, 200, 20);
            let r = a.hint_hits.iter().find(|(_, x)| *x == h).map(|(r, _)| *r).unwrap();
            pointer(a, K::Down(MouseButton::Left), r.x + 1, r.y);
        };
        click(&mut a, HintHit::Todo);
        match &a.modal {
            Some(Modal::Todo(v)) => {
                assert_eq!(v.len(), 3);
                assert_eq!(
                    v.current().map(|t| t.text.as_str()),
                    Some("fix"),
                    "the cursor starts on the task being worked on"
                );
            }
            other => panic!("{other:?}"),
        }
        press(&mut a, KeyCode::Esc);
        assert!(a.modal.is_none(), "Esc closes it");
        click(&mut a, HintHit::Jobs);
        assert!(matches!(&a.modal, Some(Modal::Jobs(v)) if v.len() == 1));
        assert!(drain(&mut rx).contains(&AgentCommand::JobTails));
        press(&mut a, KeyCode::Esc);
        assert!(a.modal.is_none());
        click(&mut a, HintHit::Mem);
        assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Shared && p.items.len() == 1));
        press(&mut a, KeyCode::Esc);
        click(&mut a, HintHit::Session);
        assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Session));
        press(&mut a, KeyCode::Esc);
        // the queue: picking a message edits it
        click(&mut a, HintHit::Queued);
        assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Queue && p.items.len() == 2));
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Enter);
        assert!(a.modal.is_none());
        assert_eq!(a.input.full_text(), "second request");
        assert_eq!(drain(&mut rx), vec![AgentCommand::Unqueue { index: 1, text: "second request".into() }]);
    }

    #[test]
    fn todo_queue_and_memory_lists_answer_to_slash_commands() {
        let (mut a, mut rx) = app();
        assert_eq!(commands::parse("/todo"), Some(Command::Todo));
        assert_eq!(commands::parse("/queue"), Some(Command::Queue));
        assert_eq!(commands::parse("/shared-memory list"), Some(Command::Mem(Some("list".into()))));
        // no tasks: the window says so, and closes
        a.input.set_text("/todo");
        a.submit();
        assert!(matches!(&a.modal, Some(Modal::Todo(v)) if v.is_empty()));
        a.modal = None;
        // nothing to list: a notice, no window
        for c in ["/queue", "/shared-memory list"] {
            a.input.set_text(c);
            a.submit();
            assert!(a.modal.is_none(), "{c}");
        }
        assert!(drain(&mut rx).is_empty(), "listing asks nothing of the agent");
        a.shared = vec![("s1".into(), String::new(), 900)];
        a.input.set_text("/shared-memory list");
        a.submit();
        assert!(matches!(&a.modal, Some(Modal::Picker(p)) if p.kind == PickKind::Shared && p.items[0].label == "s1"));
    }

    #[test]
    fn a_dialogs_scrollbar_drags_its_text() {
        use crossterm::event::{MouseButton, MouseEventKind as K};
        let (mut a, _rx) = app();
        a.run_command(Command::Help);
        frame(&mut a, 70, 16);
        let bar = a.modal_bar.expect("the help is longer than the dialog");
        assert_eq!(bar.target, scrollbar::Target::Modal);
        pointer(&mut a, K::Moved, bar.x, bar.y);
        assert_eq!(a.hover, Hover::Bar(scrollbar::Target::Modal));
        pointer(&mut a, K::Down(MouseButton::Left), bar.x, bar.y + bar.len - 1);
        pointer(&mut a, K::Drag(MouseButton::Left), bar.x, bar.y + bar.len - 1);
        pointer(&mut a, K::Up(MouseButton::Left), bar.x, bar.y + bar.len - 1);
        assert!(
            matches!(a.modal, Some(Modal::Help { scroll }) if scroll == bar.max_offset()),
            "{:?}",
            a.modal.is_some()
        );
        frame(&mut a, 70, 16);
        assert!(a.modal_bar.unwrap().offset > 0);
        a.modal = None;
        frame(&mut a, 70, 16);
        assert!(a.modal_bar.is_none(), "no stale bar once the dialog is gone");
    }

    #[test]
    fn dialogs_and_the_palette_pop_in_unless_the_animations_are_off() {
        let (mut a, _rx) = app();
        a.run_command(Command::Help);
        frame(&mut a, 70, 16);
        assert!(a.modal_at.is_some() && a.modal_pop() < 1.0, "just opened");
        let r = crate::ui::pop_rect(Rect::new(10, 10, 40, 20), 0.0);
        assert!(r.width < 40 && r.height < 20 && r.x > 10 && r.y > 10);
        assert_eq!(crate::ui::pop_rect(Rect::new(10, 10, 40, 20), 1.0), Rect::new(10, 10, 40, 20));

        let (mut a, _rx) = app();
        a.opts.animations = false;
        a.run_command(Command::Help);
        frame(&mut a, 70, 16);
        assert!(a.modal_at.is_none());
        assert_eq!(a.modal_pop(), 1.0);
        a.run_command(Command::Cost);
        assert!(matches!(&a.modal, Some(Modal::Info(i)) if !i.animating() && i.grow() == 1.0));
    }

    fn fx_at(a: &mut App, t0: Instant, ms: u64) {
        a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
    }

    fn buf_of(a: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::ui::draw(f, a)).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn a_closing_dialog_melts_away_instead_of_vanishing() {
        let (mut a, _rx) = app();
        let t0 = Instant::now();
        fx_at(&mut a, t0, 0);
        a.run_command(Command::Help);
        frame(&mut a, 70, 20);
        fx_at(&mut a, t0, 1000);
        let boxes = |rows: &[String]| rows.iter().filter(|r| r.contains("╭ help")).count();
        let open = frame(&mut a, 70, 20);
        let base = 0;
        assert!(boxes(&open) > base, "the dialog is up");
        a.modal = None;
        fx_at(&mut a, t0, 1020);
        let closing = frame(&mut a, 70, 20);
        assert!(boxes(&closing) > base, "its first frames of leaving still show it");
        assert!(a.tick_fx(), "frames keep coming while it leaves");
        fx_at(&mut a, t0, 1400);
        let gone = frame(&mut a, 70, 20);
        assert!(boxes(&gone) <= base);
        assert!(!a.tick_fx());
        // with the motion off it closes at once
        let (mut b, _rx) = app();
        b.opts.animations = false;
        b.run_command(Command::Help);
        let up = frame(&mut b, 70, 20);
        assert!(up.iter().any(|r| r.contains("╭ help")));
        b.modal = None;
        assert!(!frame(&mut b, 70, 20).iter().any(|r| r.contains("╭ help")));
    }

    #[test]
    fn the_palette_highlight_glides_to_the_next_row() {
        let (mut a, _rx) = app();
        let t0 = Instant::now();
        fx_at(&mut a, t0, 0);
        a.input.set_text("/");
        a.refresh_palette();
        frame(&mut a, 90, 24);
        fx_at(&mut a, t0, 1000);
        frame(&mut a, 90, 24);
        let rows = a.palette_rows.clone();
        assert!(rows.len() > 2, "palette rows: {}", rows.len());
        let bg = |buf: &ratatui::buffer::Buffer, i: usize| buf[(rows[i].0.x + 3, rows[i].0.y)].bg;
        let settled = buf_of(&mut a, 90, 24);
        let sel_bg = bg(&settled, 0);
        a.palette.as_mut().unwrap().selected = 1;
        let first = buf_of(&mut a, 90, 24);
        assert_ne!(bg(&first, 1), sel_bg, "the new row is not lit all at once");
        assert_eq!(bg(&first, 0), sel_bg, "the old row has not gone dark yet");
        assert!(a.tick_fx());
        fx_at(&mut a, t0, 1300);
        let done = buf_of(&mut a, 90, 24);
        assert_eq!(bg(&done, 1), sel_bg);
        assert_ne!(bg(&done, 0), sel_bg);
    }

    #[test]
    fn a_queued_request_fades_in_and_the_keys_row_crossfades() {
        let (mut a, _rx) = app();
        let t0 = Instant::now();
        fx_at(&mut a, t0, 0);
        frame(&mut a, 100, 20);
        fx_at(&mut a, t0, 1000);
        frame(&mut a, 100, 20);
        a.queued_msgs.push("then do the other thing".into());
        a.queued = 1;
        let find = |buf: &ratatui::buffer::Buffer| {
            let y = (0..buf.area.height).find(|y| (0..buf.area.width).any(|x| buf[(x, *y)].symbol() == "◷")).unwrap();
            let x = (0..buf.area.width).find(|x| buf[(*x, y)].symbol() == "t").unwrap();
            buf[(x, y)].fg
        };
        let first = find(&buf_of(&mut a, 100, 20));
        fx_at(&mut a, t0, 1600);
        let later = find(&buf_of(&mut a, 100, 20));
        assert_ne!(first, later, "the row's text comes up");
        // the keys change with the state: a busy turn fades its row in
        a.busy = true;
        let k0 = buf_of(&mut a, 100, 20);
        fx_at(&mut a, t0, 2200);
        let k1 = buf_of(&mut a, 100, 20);
        let y = k0.area.height - 2;
        assert_ne!(k0[(2, y)].fg, k1[(2, y)].fg);
    }

    fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        crate::draw_frame(&mut t, a).unwrap();
        crate::ui::rows_of(t.backend().buffer())
    }

    fn cached(a: &App) -> usize {
        a.cache.iter().filter(|c| matches!(c, Some((_, _, Some(_))))).count()
    }

    #[test]
    fn far_blocks_lose_their_lines_but_keep_their_height() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        for i in 0..3000 {
            a.push(Block::User { text: format!("message number {i}"), at: None });
        }
        a.show_reasoning = true;
        screen(&mut a, 80, 24);
        let total = a.total_lines;
        assert!(a.follow, "the view is at the end");
        let at_end = screen(&mut a, 80, 24);
        let kept = cached(&a);
        assert!(kept < 24 * 8, "{kept} blocks keep their lines");
        assert_eq!(a.total_lines, total, "the heights are kept");
        assert_eq!(a.block_at_line(3), Some(1), "a hit-test needs no lines");
        a.scroll_to_top();
        a.follow = false;
        let top = screen(&mut a, 80, 24);
        assert!(top.iter().any(|r| r.contains("message number 0")), "{top:?}");
        assert!(cached(&a) < 24 * 8);
        a.scroll_to_end();
        assert_eq!(screen(&mut a, 80, 24), at_end, "the end draws as before");
    }

    #[test]
    fn without_animations_nothing_is_born_or_opening() {
        let (mut a, _rx) = bare();
        a.opts.animations = false;
        for i in 0..3 {
            a.push(Block::User { text: format!("m{i}"), at: None });
        }
        screen(&mut a, 80, 24);
        assert!(a.looks.iter().all(|l| l.born.is_none() && l.opened.is_none()));
        assert_eq!(a.fade(0), 1.0);
    }

    #[test]
    fn an_unknown_tool_shows_key_value_pairs_not_json() {
        let args = serde_json::json!({ "action": "output", "id": 3, "wait": 5 });
        assert_eq!(summarise("jobs", &args), "action output, id 3, wait 5");
    }

    #[test]
    fn a_replayed_message_with_a_note_memory_and_text_becomes_cards_and_a_bubble() {
        let (mut a, _rx) = app();
        let base = a.blocks.len();
        let note = "[background jobs ended]\n- job 3 `make` done (exit 0) after 4s, 2 lines of output\nRead the output with the `jobs` tool (action \"output\", the job's id).";
        let mem = "<memory from=\"s-1\" title=\"parser\">\nShared.\n</memory>";
        a.history(0, Message::user(format!("{mem}\n\n{note}\n\nfix it")));
        a.history(1, Message::user("[summary] The conversation before this point was replaced\n[files]"));
        assert!(matches!(&a.blocks[base], Block::Notice { text, .. } if text.starts_with("⊕ memory from parser")));
        assert!(matches!(&a.blocks[base + 1], Block::User { text, .. } if crate::render::blocks::is_job_note(text)));
        assert!(matches!(&a.blocks[base + 2], Block::User { text, .. } if text == "fix it"));
        assert!(
            matches!(&a.blocks[base + 3], Block::Notice { text, .. } if text.starts_with("earlier conversation summarised"))
        );
    }

    #[test]
    fn a_long_call_summary_cuts_non_ascii_json_on_a_character_boundary() {
        // byte 79 falls inside a two-byte "ñ"
        let args = serde_json::json!({ "x": format!("{}{}", "a".repeat(72), "ñ".repeat(10)) });
        let s = summarise("other", &args);
        assert!(s.ends_with('…'), "{s}");
        assert!(crate::render::wrap::width(&s) <= 80, "{s}");
    }

    #[test]
    fn ctrl_c_with_the_palette_open_clears_the_draft_and_the_palette() {
        let (mut a, _rx) = app();
        for c in "/m".chars() {
            crate::event::key(&mut a, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(a.palette.is_some());
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.input.is_empty());
        assert!(a.palette.is_none(), "no list left over an empty prompt");
    }

    #[test]
    fn ctrl_c_in_a_permission_dialog_denies_it_and_stops_the_turn() {
        let (mut a, mut rx) = app();
        a.handle_agent(AgentEvent::TurnStart { turn: 1 });
        a.handle_agent(AgentEvent::PermissionAsk {
            request: PermissionRequest {
                id: "p1".into(),
                tool: "bash".into(),
                kind: nexo_core::tool::ToolKind::Exec,
                summary: "x".into(),
                detail: ToolDisplay::Plain,
                suggested_rules: vec![],
            },
        });
        while rx.try_recv().is_ok() {}
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.modal.is_none());
        let sent: Vec<AgentCommand> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            matches!(&sent[..], [AgentCommand::Permission { id, decision: PermissionDecision::Deny { .. } }, AgentCommand::Interrupt] if id == "p1"),
            "{sent:?}"
        );
        // a second press quits, as at the prompt
        crate::event::key(&mut a, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit);
    }

    #[test]
    fn a_list_at_rest_shows_the_row_the_arrow_reached_without_another_key() {
        let (mut a, _rx) = app();
        a.opts.animations = true;
        a.opts.sessions = (0..5).map(|i| (format!("s{i}"), format!("chat {i}"))).collect();
        a.run_command(Command::Sessions);
        let t0 = Instant::now();
        fx_at(&mut a, t0, 0);
        frame(&mut a, 70, 16);
        fx_at(&mut a, t0, 20_000);
        a.tick_frame(false);
        assert_eq!(a.load(), crate::frame::Load::Idle, "the open list rests");
        press(&mut a, KeyCode::Down);
        frame(&mut a, 70, 16);
        assert_eq!(a.load(), crate::frame::Load::Motion, "the glide begun in the frame asks for the next ones");
        fx_at(&mut a, t0, 20_000 + crate::motion::GLIDE_MS + 50);
        a.tick_frame(false);
        assert!(a.dirty, "a frame comes after the glide ends");
        frame(&mut a, 70, 16);
        let Some(Modal::Picker(p)) = &a.modal else { panic!() };
        assert_eq!(a.fx.pick_sel.level(p.selected, a.now()), 1.0, "the row reached is fully lit");
    }
}
