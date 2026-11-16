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
use nexo_core::subs::{SubInfo, KEEP_RUNS};
#[cfg(any(unix, test))]
use nexo_core::subs::{SubItem, SubTranscript};

mod local;
#[cfg(any(unix, test))]
mod remote;
mod remote_items;
#[cfg(any(unix, test))]
use remote_items::remote_items;
use remote_items::{safe_tool_args, safe_tool_text};

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
    seen_blocks: usize,
    live_reply_from: usize,
    live_reasoning: bool,
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
            seen_blocks: 0,
            live_reply_from: 0,
            live_reasoning: false,
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
    #[cfg(any(unix, test))]
    pub(super) remote: Option<RemoteView>,
    /// The transcript that is not on show (the main chat's while the view is up).
    parked: Parked,
    /// The main chat's fields are in place (the view is parked): only while `on_main` runs.
    main_in: bool,
    /// What the person had typed in the main chat's box.
    draft: Option<crate::input::Draft>,
    /// Panel to return to when entered from it.
    pub(super) panel: Option<Box<crate::agents_view::AgentsView>>,
    /// The transcript answer came.
    answered: bool,
    /// Items the answer held: item events up to this count are in the view already.
    base: u64,
    /// Items added since the view was last built.
    grown: usize,
    /// Messages sent from this view that the run has not shown as its own person items yet.
    pending: usize,
    /// A transcript was asked for and has not come.
    pub(super) asking: Option<u64>,
    pub(super) last_ask: Option<Instant>,
    /// Something changed while it was awaited.
    again: bool,
}

#[cfg(any(unix, test))]
pub(super) struct RemoteView {
    rx: std::sync::mpsc::Receiver<Result<RemoteUpdate, String>>,
    snapshot: Option<nexo_core::agent::ObservationSnapshot>,
    resynced: bool,
    error: Option<String>,
    target: Option<nexo_core::agent::ObservationTarget>,
    pub(super) delivery: RemoteDelivery,
    identity: Option<nexo_core::agent::ObservationTarget>,
    invalid: bool,
    close_requested: bool,
    abandon_unknown_confirm: bool,
}

#[cfg(any(unix, test))]
pub(super) enum RemoteUpdate {
    Initial(nexo_core::agent::ObservationSnapshot),
    Delta(nexo_core::agent::ObservationSnapshot),
    Resync(nexo_core::agent::ObservationSnapshot),
}

#[cfg(any(unix, test))]
pub(super) enum RemoteDelivery {
    Ready,
    Sending { text: String, request_id: String, rx: std::sync::mpsc::Receiver<RemoteReceipt> },
    Unknown { text: String, request_id: String },
}

#[cfg(any(unix, test))]
#[derive(Debug)]
pub(super) enum RemoteReceipt {
    Accepted,
    Rejected(String),
    Unknown,
    BeforeSend(String),
}

impl SubView {
    fn is_remote(&self) -> bool {
        #[cfg(any(unix, test))]
        {
            self.remote.is_some()
        }
        #[cfg(not(any(unix, test)))]
        {
            false
        }
    }
}

impl App {
    /// The sub-agent's conversation is on show.
    pub fn in_sub(&self) -> bool {
        self.sub_view.as_ref().is_some_and(|v| !v.main_in)
    }

    /// Messages sent from the open view that have not come back as the run's person items.
    pub fn sub_pending(&self) -> usize {
        self.sub_view.as_ref().filter(|v| !v.main_in).map_or(0, |v| v.pending)
    }

    /// The open run's title and whether it ended, for the rule over the box.
    pub fn sub_label(&self) -> Option<(String, bool)> {
        let v = self.sub_view.as_ref().filter(|v| !v.main_in)?;
        if v.is_remote() {
            return Some((format!("remote · {} · directed", v.id), false));
        }
        Some(match self.sub_runs.iter().find(|r| r.info.id == v.id) {
            Some(r) => (r.info.title.clone(), !r.info.state.running()),
            None => (String::new(), true),
        })
    }

    pub(super) fn swap_fields(&mut self) {
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
            seen_blocks,
            live_reply_from,
            live_reasoning,
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
        if self.sub_view.is_some() {
            return;
        }
        self.palette = None;
        self.suggestion = None;
        let draft = self.input.take_draft();
        self.sub_view = Some(SubView {
            id: id.to_string(),
            #[cfg(any(unix, test))]
            remote: None,
            parked: Parked::empty(),
            main_in: true,
            draft: Some(draft),
            panel: None,
            pending: 0,
            answered: false,
            base: 0,
            grown: 0,
            asking: None,
            last_ask: None,
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
        #[cfg(any(unix, test))]
        if self
            .sub_view
            .as_ref()
            .and_then(|v| v.remote.as_ref())
            .is_some_and(|r| !matches!(r.delivery, RemoteDelivery::Ready))
        {
            if let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) {
                r.close_requested = true;
            }
            self.toast(Kind::Error, "remote delivery unresolved · close deferred");
            return;
        }
        if self.in_sub() {
            self.swap_fields();
        }
        if let Some(v) = self.sub_view.take() {
            if let Some(d) = v.draft {
                self.input.put_draft(d);
            }
            if self.modal.is_none() {
                self.modal = v.panel.map(Modal::Agents);
            }
        }
        self.palette = None;
        self.dirty = true;
    }

    /// Asks for the open run's conversation (one request at a time).
    pub(super) fn sub_ask(&mut self) {
        let now = self.now();
        let Some(v) = self.sub_view.as_mut().filter(|v| !v.main_in) else { return };
        if v.asking.is_some() {
            v.again = true;
            return;
        }
        if v.last_ask.is_some_and(|t| now.saturating_duration_since(t) < std::time::Duration::from_millis(200)) {
            v.again = true;
            return;
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        v.asking = Some(request);
        v.last_ask = Some(now);
        v.again = false;
        let id = v.id.clone();
        self.send(AgentCommand::SubTranscript { id, request });
    }

    /// Flush a coalesced refresh even if no more events arrive.
    pub(super) fn sub_tick(&mut self) {
        #[cfg(any(unix, test))]
        self.remote_tick();
        if self.sub_view.as_ref().is_some_and(|v| !v.main_in && v.again && v.asking.is_none()) {
            self.sub_ask();
        }
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
}

/// The job a run belongs to: run ids end in `#` and the job's number.
pub fn job_of_run(id: &str) -> Option<u32> {
    id.rsplit_once('#')?.1.parse().ok()
}

/// Keep later edits intact; a draft that starts with the sent text already
/// contains the whole rejected message, including when it has been extended.
pub(super) fn recover_rejected(draft: &str, sent: &str) -> String {
    if draft.starts_with(sent) {
        draft.to_owned()
    } else if draft.is_empty() {
        sent.to_owned()
    } else {
        format!("{draft}\n\n{sent}")
    }
}
