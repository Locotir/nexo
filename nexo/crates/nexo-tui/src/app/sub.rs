//! A sub-agent's conversation in the transcript area.
//!
//! The view is the main transcript's own machinery used a second time: while it is open, the fields that
//! hold the transcript (blocks, layout, scroll, folds, search, selection) are swapped with a parked set, so
//! every key, click and renderer works on the run's blocks unchanged. The main chat's set waits in the
//! view; the events of the main agent are handled with it swapped back in (see [`App::on_main`]), so it
//! goes on receiving them and is as it was on the way back.
//!
//! Which events are new. The agent numbers nothing, so the app counts: for each run it counts every item
//! event since the run began (`seen`). A transcript answer holds `n` items (those dropped and those kept),
//! all made before the answer was; every event the app handled before the answer is among them, and so are
//! the first `n - seen` that arrive after it (they were queued behind the answer). So the item events with a
//! count up to `n` are already in the view and are skipped; the ones after it are new. If events may have
//! been lost (a lagging channel: the agent then sends the state of every run again) the count cannot be
//! trusted for that run, and the view asks for the transcript again after each change instead.

use super::*;
use crate::toast::Kind;
use nexo_core::subs::{SubInfo, SubItem, SubTranscript, KEEP_ITEMS, KEEP_RUNS};

/// Runs the app remembers; the ended ones that ended longest ago go first.
const MAX_RUNS: usize = KEEP_RUNS * 2;

/// One run as the app knows it.
#[derive(Debug, Clone)]
pub struct RunRec {
    pub info: SubInfo,
    /// Item events handled since the run began.
    seen: u64,
    /// No event of this run can have been lost.
    exact: bool,
}

/// The fields that make up a transcript, parked while the other one is shown.
struct Parked {
    blocks: Vec<Block>,
    cache: Vec<Option<Rendered>>,
    looks: Vec<Look>,
    offsets: Vec<usize>,
    total_lines: usize,
    seen_lines: usize,
    scroll: usize,
    follow: bool,
    selected: Option<usize>,
    reveal: Option<usize>,
    search: Option<Search>,
    selection: Option<Selection>,
    scroll_fx: anim::Tween,
    unseen_at: Option<Instant>,
    unseen_before: usize,
    wished: bool,
    transcript_bar: Option<scrollbar::Bar>,
    inline_flushed: usize,
    hover: Hover,
    tclick: Option<(Instant, Pos, u8)>,
    press: Option<(u16, u16)>,
    drag_pos: Option<(u16, u16)>,
}

impl Parked {
    fn empty() -> Parked {
        Parked {
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
            selection: None,
            scroll_fx: anim::Tween::new(0.0, anim::SCROLL_MS),
            unseen_at: None,
            unseen_before: 0,
            wished: false,
            transcript_bar: None,
            inline_flushed: 0,
            hover: Hover::Nothing,
            tclick: None,
            press: None,
            drag_pos: None,
        }
    }
}

/// The open run's conversation.
pub struct SubView {
    pub id: String,
    /// The transcript that is not on show (the main chat's while the view is up).
    parked: Parked,
    /// The main chat's fields are in place (the view is parked): only while `on_main` runs.
    main_in: bool,
    /// What the person had typed in the main chat's box.
    draft: Option<crate::input::Draft>,
    /// The transcript answer came.
    answered: bool,
    /// Items the answer held: item events up to this count are in the view already.
    base: u64,
    /// Items added since the view was last built.
    grown: usize,
    /// A transcript was asked for and has not come.
    asking: bool,
    /// Something changed while it was awaited.
    again: bool,
}

impl App {
    /// The sub-agent's conversation is on show.
    pub fn in_sub(&self) -> bool {
        self.sub_view.as_ref().is_some_and(|v| !v.main_in)
    }

    /// The open run's title and whether it ended, for the rule over the box.
    pub fn sub_label(&self) -> Option<(String, bool)> {
        let v = self.sub_view.as_ref().filter(|v| !v.main_in)?;
        Some(match self.sub_runs.iter().find(|r| r.info.id == v.id) {
            Some(r) => (r.info.title.clone(), !r.info.state.running()),
            None => (String::new(), true),
        })
    }

    fn swap_fields(&mut self) {
        let Some(v) = self.sub_view.as_mut() else { return };
        let p = &mut v.parked;
        macro_rules! swap {
            ($($f:ident),*) => { $(std::mem::swap(&mut self.$f, &mut p.$f);)* };
        }
        swap!(
            blocks,
            cache,
            looks,
            offsets,
            total_lines,
            seen_lines,
            scroll,
            follow,
            selected,
            reveal,
            search,
            selection,
            scroll_fx,
            unseen_at,
            unseen_before,
            wished,
            transcript_bar,
            inline_flushed,
            hover,
            tclick,
            press,
            drag_pos
        );
        v.main_in = !v.main_in;
    }

    /// Runs `f` with the main chat in place, as if no view were open; the view comes back after.
    pub(super) fn on_main<R>(&mut self, f: impl FnOnce(&mut App) -> R) -> R {
        if !self.in_sub() {
            return f(self);
        }
        self.swap_fields();
        let r = f(self);
        self.swap_fields();
        r
    }

    /// Opens the conversation of run `id`.
    pub fn open_sub(&mut self, id: &str) {
        if !self.sub_runs.iter().any(|r| r.info.id == id) {
            return;
        }
        if self.sub_view.as_ref().is_some_and(|v| v.id == id) {
            return;
        }
        self.close_sub();
        self.palette = None;
        self.suggestion = None;
        let draft = self.input.take_draft();
        self.sub_view = Some(SubView {
            id: id.to_string(),
            parked: Parked::empty(),
            main_in: true,
            draft: Some(draft),
            answered: false,
            base: 0,
            grown: 0,
            asking: false,
            again: false,
        });
        self.swap_fields();
        self.push(Block::Notice { level: Level::Info, text: "…".into() });
        self.sub_ask();
        self.dirty = true;
    }

    /// Back to the main chat, as it was.
    pub fn close_sub(&mut self) {
        if self.sub_view.is_none() {
            return;
        }
        if self.in_sub() {
            self.swap_fields();
        }
        if let Some(v) = self.sub_view.take() {
            if let Some(d) = v.draft {
                self.input.put_draft(d);
            }
        }
        self.palette = None;
        self.dirty = true;
    }

    /// Asks for the open run's conversation (one request at a time).
    fn sub_ask(&mut self) {
        let Some(v) = self.sub_view.as_mut().filter(|v| !v.main_in) else { return };
        if v.asking {
            v.again = true;
            return;
        }
        v.asking = true;
        let id = v.id.clone();
        self.send(AgentCommand::SubTranscript { id });
    }

    /// The runs the jobs window can open: (job number, run id).
    pub fn sub_job_runs(&self) -> Vec<(u32, String)> {
        self.sub_runs.iter().filter_map(|r| Some((job_of_run(&r.info.id)?, r.info.id.clone()))).collect()
    }

    /// The run a click on the jobs chip opens: the only job running is a sub-agent.
    pub fn sub_chip_target(&self) -> Option<String> {
        let mut running = self.jobs.iter().filter(|j| j.status.running());
        let (only, none_else) = (running.next()?, running.next().is_none());
        if !none_else || only.kind != nexo_core::jobs::JobKind::Agent {
            return None;
        }
        self.sub_runs
            .iter()
            .find(|r| r.info.state.running() && job_of_run(&r.info.id) == Some(only.id))
            .map(|r| r.info.id.clone())
    }

    // ------------------------------------------------------------ events

    /// One of the four `Sub` events or the answer to a request. Nothing here draws unless the run is open.
    pub(super) fn handle_sub(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::SubState { info } => self.sub_state(info),
            AgentEvent::Sub { id, item } => {
                let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) else { return };
                r.seen += 1;
                let (count, exact) = (r.seen, r.exact);
                let Some(v) = self.sub_view.as_mut().filter(|v| v.id == id && !v.main_in) else { return };
                if !exact {
                    return self.sub_ask();
                }
                // before the answer, and up to the count it holds, the item is in the view already
                if !v.answered || count <= v.base {
                    return;
                }
                v.grown += 1;
                let grown = v.grown;
                self.sub_apply(item);
                // the view holds what the agent keeps and no more
                if grown >= KEEP_ITEMS {
                    self.sub_ask();
                }
            }
            AgentEvent::SubDelta { id, text } => {
                let Some(r) = self.sub_runs.iter().find(|r| r.info.id == id) else { return };
                let count = r.seen;
                let Some(v) = self.sub_view.as_ref().filter(|v| v.id == id && !v.main_in) else { return };
                // replies the answer holds whole are not written again
                if !v.answered || count < v.base {
                    return;
                }
                self.sub_delta(&text);
            }
            AgentEvent::SubNotDelivered { why, .. } => self.toast(Kind::Error, &format!("not delivered: {why}")),
            AgentEvent::SubTranscript { id, transcript }
                if self.sub_view.as_ref().is_some_and(|v| v.id == id && !v.main_in) =>
            {
                self.sub_answer(&id, transcript)
            }
            _ => {}
        }
    }

    fn sub_state(&mut self, info: SubInfo) {
        let id = info.id.clone();
        // the state of a run the app knows, unchanged: the agent sends that after a lag
        let mut resync = false;
        match self.sub_runs.iter_mut().find(|r| r.info.id == id) {
            Some(r) => {
                resync = r.info.state == info.state;
                if resync {
                    r.exact = false;
                }
                r.info = info;
            }
            None => {
                let exact = info.state.running() && info.tokens_in == 0 && info.tokens_out == 0;
                self.sub_runs.push(RunRec { info, seen: 0, exact });
            }
        }
        if self.sub_runs.len() > MAX_RUNS {
            let open = self.sub_view.as_ref().map(|v| v.id.clone());
            let gone = self
                .sub_runs
                .iter()
                .filter(|r| !r.info.state.running() && Some(&r.info.id) != open.as_ref())
                .min_by_key(|r| r.info.ended_ms)
                .map(|r| r.info.id.clone());
            if let Some(g) = gone {
                self.sub_runs.retain(|r| r.info.id != g);
            }
        }
        let runs = self.sub_job_runs();
        if let Some(Modal::Jobs(v)) = &mut self.modal {
            v.set_runs(runs);
            self.dirty = true;
        }
        if self.sub_view.as_ref().is_some_and(|v| v.id == id && !v.main_in) {
            self.dirty = true;
            if resync {
                self.sub_ask();
            }
        }
    }

    /// The answer: the view is built from it.
    fn sub_answer(&mut self, id: &str, t: Option<SubTranscript>) {
        let n = t.as_ref().map_or(0, |t| (t.dropped + t.items.len()) as u64);
        let again = {
            let Some(v) = self.sub_view.as_mut() else { return };
            v.asking = false;
            std::mem::take(&mut v.again)
        };
        // the old view goes; what was read of it stays read
        self.blocks.clear();
        self.cache.clear();
        self.looks.clear();
        self.offsets.clear();
        self.total_lines = 0;
        self.selected = None;
        self.reveal = None;
        self.selection = None;
        self.search = None;
        match t {
            Some(t) => {
                for item in t.items {
                    self.sub_apply(item);
                }
            }
            None => {
                self.push(Block::Notice { level: Level::Info, text: "this conversation is no longer kept".into() });
            }
        }
        // history is not an arrival: nothing fades in
        for l in &mut self.looks {
            l.born = None;
        }
        if let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) {
            if !r.exact || r.seen > n {
                r.exact = false;
                r.seen = n;
            }
        }
        if let Some(v) = self.sub_view.as_mut() {
            v.answered = true;
            v.base = n;
            v.grown = 0;
        }
        self.dirty = true;
        if again {
            self.sub_ask();
        }
    }

    /// One item of the run's conversation, drawn by the blocks the main chat uses.
    fn sub_apply(&mut self, item: SubItem) {
        match item {
            SubItem::Task { text } => {
                self.close_streaming();
                self.push(Block::User { text, at: None });
            }
            SubItem::Person { text } => {
                self.close_streaming();
                self.push(Block::User { text, at: None });
                self.scroll_to_end();
            }
            SubItem::Text { text } => {
                if let Some(Block::Assistant { text: t, streaming: true, .. }) = self.blocks.last_mut() {
                    *t = text;
                    self.close_streaming();
                    self.invalidate(self.blocks.len() - 1);
                } else {
                    self.close_streaming();
                    self.push(Block::Assistant { text, streaming: false, at: None, started: None, took_ms: None });
                }
            }
            SubItem::Call { id, name, summary } => {
                self.close_streaming();
                let started = Some(self.now());
                self.push(Block::Tool {
                    id,
                    name,
                    summary,
                    args: serde_json::Value::Null,
                    status: ToolStatus::Running,
                    progress: Vec::new(),
                    output: None,
                    started,
                    elapsed_ms: None,
                    folded: true,
                });
            }
            SubItem::Result { id, ok, bytes } => {
                let now = self.now();
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { status, output, started, elapsed_ms, .. } = &mut self.blocks[i] {
                        *status = if ok { ToolStatus::Done } else { ToolStatus::Failed };
                        *output = Some(if ok {
                            ToolOutput::ok(format!("{bytes} bytes"))
                        } else {
                            ToolOutput::error(format!("failed, {bytes} bytes"))
                        });
                        let ms = started.map_or(0, |s| now.saturating_duration_since(s).as_millis() as u64);
                        *elapsed_ms = (ms > 0).then_some(ms);
                    }
                    self.invalidate(i);
                }
            }
        }
    }

    /// Part of the reply being written, as the main chat shows its own.
    fn sub_delta(&mut self, text: &str) {
        match self.blocks.last_mut() {
            Some(Block::Assistant { text: t, streaming: true, .. }) => {
                t.push_str(text);
                self.invalidate(self.blocks.len() - 1);
            }
            _ => {
                let started = Some(self.now());
                self.push(Block::Assistant {
                    text: text.to_string(),
                    streaming: true,
                    at: None,
                    started,
                    took_ms: None,
                });
            }
        }
    }

    // ------------------------------------------------------------ the person's actions

    /// Enter in the view: the text goes to this run and to no one else.
    pub(super) fn submit_sub(&mut self) {
        let text = self.input.full_text();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            if let Some(i) = self.selected {
                self.toggle_block(i);
            }
            return;
        }
        if trimmed.starts_with('/') && !starts_with_path(trimmed) {
            self.fx.reject_at = self.fx_on().then(|| self.now());
            self.toast(Kind::Info, "commands run in the main chat · Esc back");
            return;
        }
        let Some(id) = self.sub_view.as_ref().map(|v| v.id.clone()) else { return };
        let trimmed = trimmed.to_string();
        self.input.push_history(&trimmed);
        self.input.clear();
        self.send(AgentCommand::SubSubmit { id, text: trimmed });
        self.scroll_to_end();
        self.dirty = true;
    }

    /// Esc in the view: the text goes first, then the view. Nothing is interrupted.
    pub(super) fn esc_sub(&mut self) {
        if self.input.is_empty() {
            self.close_sub();
        } else {
            self.input.clear();
        }
        self.dirty = true;
    }
}

/// The job a run belongs to: run ids end in `#` and the job's number.
pub fn job_of_run(id: &str) -> Option<u32> {
    id.rsplit_once('#')?.1.parse().ok()
}
