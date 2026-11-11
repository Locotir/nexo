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
use crate::select::{LineInfo, Pos, RegionRef, Selection};
use crate::theme::Theme;
use crate::TuiOptions;

mod blocks;
mod dispatch;
mod events;
mod keys;
mod palette_ui;
mod pointer;
mod submit;
#[cfg(test)]
mod tests;

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
    /// What the policy is doing ("curator", "curator -12k tok", "curator idle") and why.
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
type Rendered = (usize, usize, Option<Vec<Line<'static>>>, crate::regions::Meta);

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
    /// Every word of the session, for the ghost text of the draft.
    pub words: crate::complete::WordIndex,
    /// The next message the agent suggests, dim in the empty draft; Tab or → takes it, any edit drops it.
    pub suggestion: Option<String>,
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
    /// The MCP servers, as the agent last listed them (their prompts are commands, their resources complete after `@`).
    pub mcp: Vec<nexo_core::agent::McpServerInfo>,
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
    /// A conversation was loaded and the curator has not yet said what it keeps: the footer shows a spinner.
    pub context_wait: bool,
    pub session_usage: Usage,
    pub turn_usage: Usage,
    pub cost: f64,
    /// Input of each model call this session, for the usage window's sparkline.
    pub call_inputs: Vec<u64>,
    pub context: Option<ContextInfo>,
    /// Nothing can be asked until a model works: the home shows a card to set one up (see `ui::draw_lock`).
    pub locked: bool,
    /// What the last scan for local servers found; `None` until one has answered.
    pub scan: Option<Vec<(String, String)>>,
    pub provider: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub session_id: Option<String>,
    pub cwd: PathBuf,
    pub should_quit: bool,
    /// The answer to the trust question, once given (`ask_trust`).
    pub trust_answer: Option<bool>,
    pub dirty: bool,
    pub force_clear: bool,
    /// Something glides, fades or pulses: the loop draws at ~60 fps while true.
    pub moving: bool,
    last_ctrl_c: Option<Instant>,
    last_esc: Option<Instant>,
    /// The last Esc interrupted a turn: a quick second one belongs to it, it is not a rewind.
    esc_interrupted: bool,
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
    /// What the curator added to the last model calls and held back, newest first (empty until a call
    /// comes with the curator).
    pub curated: Vec<Vec<nexo_core::CuratedItem>>,
    /// `/context-live`: the call (0 the latest) whose curator rows the panel lists in place of the map.
    pub live_rows: Option<usize>,
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
    /// The last click in the transcript: when, where, and how many in a row.
    tclick: Option<(Instant, Pos, u8)>,
    /// The button went down with Alt or Ctrl: the drag selects without box limits.
    pub press_free: bool,
    /// The screen row of the status bar at the last draw (a click there opens the context window).
    pub status_row: u16,
    /// The terminal's width at the last draw.
    pub term_width: u16,
    pub limits: Option<Limits>,
    /// The reasoning level of the model in use and the levels it takes.
    pub effort: Option<String>,
    pub effort_levels: Vec<String>,
    /// The model in use runs fast (`/fast`): a mark on the status bar.
    pub fast: bool,
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
    /// What is left of the subscription plan the current provider is signed in with (None: it is on a
    /// key, or nothing has been read yet).
    pub plan_usage: Option<nexo_providers::usage::PlanUsage>,
    branch_read: Option<Instant>,
    /// The system prompt as last shown (`/prompt`), the seed of the prompt file.
    pub system_prompt: Option<String>,
    /// The run loop opens `$EDITOR` on this file when set.
    pub edit_request: Option<PathBuf>,
    /// Ctrl+G: the draft goes to `$EDITOR` and comes back.
    pub draft_edit_request: bool,
    /// Commands of one's own (`.nexo/commands/*.md`).
    pub custom_commands: Vec<crate::custom::CustomCommand>,
    /// The Agent Skills found at start (`SKILL.md` folders); each also answers to `/name` as a command.
    pub skills: Vec<nexo_core::skills::Skill>,
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
    /// A finished list stays in the strip as `✓` until its turn ends.
    pub todo_done_live: bool,
    /// Where the to-do strip was drawn last frame (a click on it opens the list).
    pub todo_area: Rect,
    /// The to-do list is on screen this frame (the strip or the side panel): the status bar does not repeat its count.
    pub todo_shown: bool,
    /// The goal in force: its text, rounds and state.
    pub goal: Option<Goal>,
    /// What the goal has done so far (both wordings, stops, steps, spend), for its window.
    pub goal_log: nexo_core::agent::GoalLog,
    /// The round a goal continuation just began, for the line that marks it in the transcript.
    goal_round_said: Option<(u32, u32)>,
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
    /// The reasoning level: `/effort`.
    Effort,
    /// The fast-mode marker: `/fast`.
    Fast,
    /// The permission mode: its picker (`/permissions`).
    Mode,
    /// The context gauge: `/context`.
    Gauge,
    /// The tokens billed and the cost: `/usage`.
    Cost,
    /// The goal: its window.
    Goal,
    /// The to-do count: `/todo`.
    Todo,
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

/// What a subscription's usage would have cost on the API, worded as included in the plan.
pub fn plan_value(usd: f64) -> String {
    format!("≈${usd:.4} API value · included in plan")
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
            words: crate::complete::WordIndex::new(opts.complete_words),
            suggestion: None,
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
            mcp: Vec::new(),
            peers: Vec::new(),
            mail_polled: None,
            nexo_src: nexo_agent::supervise::NexoSource::default(),
            panel: None,
            panel_filter: Default::default(),
            spinner: 0,
            context_wait: false,
            session_usage: Usage::default(),
            turn_usage: Usage::default(),
            cost: 0.0,
            call_inputs: Vec::new(),
            context: None,
            locked: false,
            scan: None,
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            permission_mode: PermissionMode::Ask,
            session_id: opts.session_id.clone(),
            cwd: opts.cwd.clone(),
            should_quit: false,
            trust_answer: None,
            dirty: true,
            force_clear: false,
            moving: false,
            last_ctrl_c: None,
            last_esc: None,
            esc_interrupted: false,
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
            curated: Vec::new(),
            live_rows: None,
            files_area: Rect::default(),
            panel_hint_shown: false,
            editor_area: Rect::default(),
            editor_drag: false,
            editor_top: 0,
            editor_click: None,
            tclick: None,
            press_free: false,
            status_row: 0,
            term_width: 80,
            limits: None,
            effort: None,
            effort_levels: Vec::new(),
            fast: false,
            budget: None,
            last_call_input: 0,
            last_call_cached: 0,
            turn_tally: Default::default(),
            turn_cost_start: 0.0,
            branch: None,
            root: nexo_core::paths::is_root(),
            update_behind: None,
            plan_usage: None,
            branch_read: None,
            system_prompt: None,
            edit_request: None,
            draft_edit_request: false,
            custom_commands: crate::custom::load(&opts.cwd),
            skills: Vec::new(),
            bell_request: None,
            mouse_on: opts.mouse,
            mouse_request: None,
            theme_before_preview: None,
            pending_stamps: VecDeque::new(),
            todo: Vec::new(),
            todo_done_live: false,
            todo_area: Rect::default(),
            todo_shown: false,
            goal: None,
            goal_round_said: None,
            goal_log: Default::default(),
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
        let found = std::mem::take(&mut app.opts.skills);
        app.set_skills(found);
        app.input.set_placeholder_style(app.theme.dim());
        app.read_branch();
        app.push_welcome();
        // a provider that cannot be used yet (no key, no login, no server found): the home asks for a setup
        // (no provider chosen at all is the same: the name stays empty until one is set up)
        app.locked = app.opts.provider.is_empty()
            || app
                .opts
                .providers
                .iter()
                .find(|(n, _)| *n == app.opts.provider)
                .is_some_and(|(n, c)| !crate::modal::setup::provider_ready(n, c, &[]));
        if app.locked {
            // a local server may answer: that unlocks it
            app.send(AgentCommand::ScanLocal);
        }
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
        self.suggestion = None;
        self.words.clear();
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
        self.todo_done_live = false;
        self.session_usage = Usage::default();
        self.turn_usage = Usage::default();
        self.cost = 0.0;
        self.context = None;
        self.context_wait = false;
        self.map.clear();
        self.map_zones.clear();
        self.map_changed.clear();
        self.curated.clear();
        self.live_rows = None;
        self.marks.clear();
        self.call_tokens.clear();
        self.queued = 0;
        self.queued_msgs.clear();
        self.dirty = true;
        self.push_welcome();
    }
}

/// What `/init` asks the agent.
pub const INIT_PROMPT: &str = "Study this project and write (or update) AGENTS.md at its root: a short guide for an \
agent working here. Cover what the project is, how it is laid out, how to build, test, lint and run it (the exact \
commands, which you should check by running them), the conventions the code follows, and anything surprising. \
Keep it under 150 lines; prefer facts you verified to guesses.";

/// How long a flash message stays; it fades out over its last `STATUS_FADE_MS`.
pub const STATUS_MS: u64 = 2500;
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

/// A line that starts with a path (`/home/u/a.rs has a bug`), not a command: its first word has a second
/// `/`, a `\`, or a `.`.
fn starts_with_path(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|w| w[1..].contains(['/', '\\', '.']))
}

/// The branch of the repository that holds `cwd` (a short hash when detached).
pub fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let head = d.join(".git").join("HEAD");
        if let Ok(text) = std::fs::read_to_string(&head) {
            let text = text.trim();
            return Some(match text.strip_prefix("ref: ") {
                Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_string(),
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
                            Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_string(),
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
        "read" if nexo_core::tool::has_paths(args) => {
            return nexo_core::tool::paths_label(&nexo_core::tool::call_paths(args));
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

/// Cuts `line` to at most `w` cells, ending in an ellipsis (or before a trailing fact that does not fit); the spans keep their styles.
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
        // this span reaches the edge. A fact after the text (` · 40 lines`, `  44ms`) is shown whole
        // or not at all: cut, it would end in `· …` or a figure with its tail missing
        if i > 0 && line.spans[i].content.starts_with(' ') {
            line.spans.truncate(i);
            return;
        }
        // any other span ends in the ellipsis and the rest goes
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

/// Removes from the auth store `file` what is kept for `name` (else `current`) under its `auth` entry;
/// what to tell the person. A key from an environment variable or the configuration is not nexo's to remove.
fn sign_out(
    providers: &[(String, nexo_core::ProviderConfig)],
    current: &str,
    name: Option<&str>,
    file: &std::path::Path,
) -> (Level, String) {
    let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(current);
    let entry = providers.iter().find(|(n, _)| n == name).and_then(|(_, p)| p.auth.as_deref()).unwrap_or(name);
    match nexo_core::auth::remove_in(file, entry) {
        Ok(true) => (Level::Info, format!("{name}: signed out, the stored key is removed; /model signs in again")),
        Ok(false) => (
            Level::Warn,
            format!("{name}: nothing stored to remove (a key from an environment variable or the configuration stays)"),
        ),
        Err(e) => (Level::Error, format!("{name}: cannot change {}: {e}", file.display())),
    }
}

/// Where a command of one's own comes from, as the palette shows it: `~/.claude/commands`, `project .nexo/commands`,
/// `skill`, `plugin <name>`; `custom` when it is not known.
fn custom_origin(origin: &str) -> String {
    match origin.split_once(' ') {
        Some(("user", dir)) => format!("~/{dir}"),
        _ if origin.is_empty() => "custom".into(),
        _ => origin.into(),
    }
}

#[cfg(test)]
mod custom_origin_tests {
    #[test]
    fn a_command_of_ones_own_names_the_folder_it_comes_from() {
        assert_eq!(super::custom_origin("user .claude/commands"), "~/.claude/commands");
        assert_eq!(super::custom_origin("project .nexo/commands"), "project .nexo/commands");
        assert_eq!(super::custom_origin("plugin tidy"), "plugin tidy");
        assert_eq!(super::custom_origin("skill"), "skill");
        assert_eq!(super::custom_origin(""), "custom");
    }
}
