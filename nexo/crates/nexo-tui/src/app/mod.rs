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
mod cmd_support;
mod dispatch;
mod events;
mod gauge;
mod keys;
mod lines;
mod palette_ui;
mod pointer;
mod status_and_call_text;
mod sub;
mod submit;
#[cfg(test)]
mod tests;
mod types;
mod welcome;

use cmd_support::{custom_origin, sign_out};
use lines::{folds_alone, indent_of, is_step, mark_head};
pub use status_and_call_text::*;
pub use types::*;

pub const WHEEL_LINES: usize = 3;
const MAX_PROGRESS: usize = 500;
/// Columns of the changed-files panel, and the narrowest terminal that gets one.
pub const FILES_PANEL_W: u16 = 30;
pub const FILES_PANEL_MIN_W: u16 = 100;
/// How long a cell of the context map that changed stays lit.
pub const MAP_FLASH_MS: u64 = 900;
/// The window the gauge measures against when the model never said its own.
pub const GUESS_WINDOW: u64 = 128_000;

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
    seen_blocks: usize,
    /// A fold the person made: the next layout must not count its rows as new.
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
    /// Which key tips were shown and used (the empty draft's hint); kept in the data folder while the interface runs.
    pub hint_state: crate::hints::HintState,
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
    /// The sub-agent runs of the session as the agent last told them, and the one whose conversation is open.
    pub sub_runs: Vec<sub::RunRec>,
    pub sub_view: Option<sub::SubView>,
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
            seen_blocks: 0,
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
            hint_state: Default::default(),
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
            sub_runs: Vec::new(),
            sub_view: None,
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
        self.seen_blocks = 0;
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
    /// When the thinking of a reasoning block closed (its header settles), and when a turn was interrupted
    /// at this block (its rail warms).
    ended: Option<Instant>,
    stopped: Option<Instant>,
    /// How far back the group block of a call is (None: not in a group).
    group_back: Option<u32>,
}
