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
use nexo_core::subs::{SubInfo, SubItem, SubTranscript, KEEP_RUNS};

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

pub(super) enum RemoteUpdate {
    Initial(nexo_core::agent::ObservationSnapshot),
    Delta(nexo_core::agent::ObservationSnapshot),
    Resync(nexo_core::agent::ObservationSnapshot),
}

pub(super) enum RemoteDelivery {
    Ready,
    Sending { text: String, request_id: String, rx: std::sync::mpsc::Receiver<RemoteReceipt> },
    Unknown { text: String, request_id: String },
}

#[derive(Debug)]
pub(super) enum RemoteReceipt {
    Accepted,
    Rejected(String),
    Unknown,
    BeforeSend(String),
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
        if v.remote.is_some() {
            return Some((format!("remote · {} · directed", v.id), false));
        }
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
            seen_blocks,
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

    /// Open a remote session without pretending it is a local sub-agent.
    #[cfg(unix)]
    pub(super) fn open_remote(&mut self, id: &str) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let target = id.to_owned();
        tokio::spawn(async move {
            let mut cursor = None;
            loop {
                use nexo_agent::observe_remote::ObservationUpdate;
                let result = nexo_agent::observe_remote::snapshot_since(
                    &nexo_agent::observe_remote::default_root(),
                    &target,
                    cursor.as_ref(),
                )
                .await
                .map(|update| match update {
                    ObservationUpdate::Initial(s) => RemoteUpdate::Initial(s),
                    ObservationUpdate::Delta(s) => RemoteUpdate::Delta(s),
                    ObservationUpdate::Resync(s) => RemoteUpdate::Resync(s),
                })
                .map_err(|e| e.to_string());
                let next = result.as_ref().ok().map(|u| match u {
                    RemoteUpdate::Initial(s) | RemoteUpdate::Delta(s) | RemoteUpdate::Resync(s) => {
                        let mut c = s.clone();
                        c.events.clear();
                        c
                    }
                });
                // Never advance the cursor over an update that the UI has not received.
                let mut message = result;
                loop {
                    match tx.try_send(message) {
                        Ok(()) => break,
                        Err(std::sync::mpsc::TrySendError::Full(m)) => message = m,
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                if let Some(next) = next {
                    cursor = Some(next);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
        self.open_remote_receiver(id, rx);
    }

    pub(super) fn open_remote_receiver(
        &mut self,
        id: &str,
        rx: std::sync::mpsc::Receiver<Result<RemoteUpdate, String>>,
    ) {
        self.close_sub();
        if self.sub_view.is_some() {
            return;
        }
        self.palette = None;
        self.suggestion = None;
        let draft = self.input.take_draft();
        self.sub_view = Some(SubView {
            id: id.to_owned(),
            remote: Some(RemoteView {
                rx,
                snapshot: None,
                resynced: false,
                error: None,
                target: None,
                delivery: RemoteDelivery::Ready,
                identity: None,
                invalid: false,
                close_requested: false,
                abandon_unknown_confirm: false,
            }),
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
        self.push(Block::Notice { level: Level::Info, text: "Connecting to remote observation…".into() });
        self.dirty = true;
    }

    fn remote_tick(&mut self) {
        self.remote_delivery_tick();
        let result = self.sub_view.as_ref().and_then(|v| v.remote.as_ref()).and_then(|r| match r.rx.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("observation worker stopped".into())),
        });
        let Some(result) = result else { return };
        match result {
            Err(error) => {
                let r = self.sub_view.as_mut().unwrap().remote.as_mut().unwrap();
                r.target = None;
                if r.error.as_ref() != Some(&error) {
                    r.error = Some(error.clone());
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("Remote observation disconnected: {}", safe_tool_text(&error)),
                    });
                    self.dirty = true;
                }
            }
            Ok(update) => {
                let view = self.sub_view.as_mut().unwrap();
                let r = view.remote.as_mut().unwrap();
                let snapshot = match &update {
                    RemoteUpdate::Initial(s) | RemoteUpdate::Delta(s) | RemoteUpdate::Resync(s) => s,
                };
                let target = snapshot.session_id.as_ref().filter(|id| **id == view.id).map(|id| {
                    nexo_core::agent::ObservationTarget {
                        instance_id: snapshot.instance_id.clone(),
                        session_id: id.clone(),
                        generation: snapshot.generation,
                    }
                });
                if target.is_none() || r.identity.as_ref().is_some_and(|old| target.as_ref() != Some(old)) {
                    r.invalid = true;
                }
                if r.invalid {
                    r.target = None;
                    if r.error.as_deref() != Some("identity changed") {
                        r.error = Some("identity changed".into());
                        self.push(Block::Notice {
                            level: Level::Info,
                            text: "Remote identity changed · close and reopen observation before sending.".into(),
                        });
                        self.dirty = true;
                    }
                    return;
                }
                if r.identity.is_none() {
                    r.identity = target.clone();
                }
                // An empty poll still needs validation, but should not rebuild the
                // transcript or append another notice when nothing has changed.
                let unchanged = matches!(&update, RemoteUpdate::Delta(next) if r.snapshot.as_ref().is_some_and(|old|
                    next.events.is_empty() && next.sequence == old.sequence && next.dropped == old.dropped
                    && next.permission_waiting == old.permission_waiting && next.question_waiting == old.question_waiting));
                let recovering = r.error.is_some();
                let result = merge_remote(&mut r.snapshot, update);
                if let Err(error) = result {
                    r.target = None;
                    r.invalid = true;
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("Remote observation invalid: {error} · close and reopen."),
                    });
                    self.dirty = true;
                    return;
                }
                r.resynced |= result.unwrap();
                r.target = target;
                r.error = None;
                if unchanged && !recovering {
                    return;
                }
                let (items, partial) = remote_items(r.snapshot.as_ref().unwrap());
                let resynced = r.resynced;
                let id = view.id.clone();
                self.sub_answer(&id, Some(SubTranscript { items, dropped: 0, partial }));
                self.push(Block::Notice {
                    level: Level::Info,
                    text: if resynced {
                        "Remote observation resynchronized after loss · earlier events may be missing."
                    } else {
                        "Remote live observation is bounded and may omit events."
                    }
                    .into(),
                });
            }
        }
    }

    fn remote_delivery_tick(&mut self) {
        let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) else { return };
        let (text, request_id, result) = match &r.delivery {
            RemoteDelivery::Sending { text, request_id, rx } => match rx.try_recv() {
                Ok(result) => (text.clone(), request_id.clone(), result),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    (text.clone(), request_id.clone(), RemoteReceipt::Unknown)
                }
            },
            _ => return,
        };
        r.delivery = if matches!(result, RemoteReceipt::Unknown) {
            RemoteDelivery::Unknown { text: text.clone(), request_id }
        } else {
            RemoteDelivery::Ready
        };
        if matches!(result, RemoteReceipt::Rejected(_) | RemoteReceipt::BeforeSend(_)) {
            let current = self.input.full_text();
            self.input.clear();
            self.input.set_text(&recover_rejected(&current, &text));
        }
        match result {
            RemoteReceipt::Accepted => {
                self.input.push_history(&text);
                self.toast(Kind::Info, "remote message consumed · response not yet complete");
            }
            RemoteReceipt::Rejected(reason) => {
                self.toast(Kind::Error, &format!("remote rejected: {} · draft kept", safe_tool_text(&reason)))
            }
            RemoteReceipt::BeforeSend(error) => {
                self.toast(Kind::Error, &format!("remote not sent: {} · draft kept", safe_tool_text(&error)))
            }
            RemoteReceipt::Unknown => {
                self.toast(Kind::Error, "remote delivery unknown · do not resend; delivery remains locked")
            }
        }
        self.dirty = true;
        let close = self
            .sub_view
            .as_ref()
            .and_then(|v| v.remote.as_ref())
            .is_some_and(|r| r.close_requested && matches!(r.delivery, RemoteDelivery::Ready));
        if close {
            let draft = self.input.full_text();
            if !draft.is_empty() {
                self.input.push_history(&draft);
            }
            self.close_sub();
            if !draft.is_empty() {
                self.toast(Kind::Info, "remote draft saved in input history");
            }
        }
    }

    fn submit_remote(&mut self, text: String) {
        let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) else { return };
        match &r.delivery {
            RemoteDelivery::Sending { .. } => {
                self.toast(Kind::Info, "remote delivery pending");
                return;
            }
            RemoteDelivery::Unknown { text: uncertain, request_id } => {
                let _ = (uncertain, request_id); // No further send until explicit discard, even after editing.
                self.toast(Kind::Error, "delivery unknown · do not resend; delivery remains locked");
                return;
            }
            RemoteDelivery::Ready => {}
        }
        let Some(target) = r.target.clone() else {
            self.toast(Kind::Error, "remote observation unavailable · draft kept");
            return;
        };
        #[cfg(unix)]
        {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let request_id = uuid::Uuid::new_v4().to_string();
            r.delivery = RemoteDelivery::Sending { text: text.clone(), request_id: request_id.clone(), rx };
            self.input.clear();
            tokio::spawn(async move {
                use nexo_agent::observe_remote::{intervene, InterventionResult};
                let result =
                    match intervene(&nexo_agent::observe_remote::default_root(), target, request_id, text).await {
                        Ok(InterventionResult::Accepted) => RemoteReceipt::Accepted,
                        Ok(InterventionResult::Rejected(reason)) => RemoteReceipt::Rejected(reason),
                        Ok(InterventionResult::Unknown) => RemoteReceipt::Unknown,
                        Err(error) => RemoteReceipt::BeforeSend(error.to_string()),
                    };
                let _ = tx.send(result);
            });
            self.toast(Kind::Info, "sending remote message · awaiting consumption");
            self.dirty = true;
        }
        #[cfg(not(unix))]
        {
            let _ = (target, text);
            self.toast(Kind::Error, "remote intervention unavailable · draft kept");
        }
    }

    /// Back to the main chat, as it was.
    pub fn close_sub(&mut self) {
        if self.sub_view.is_none() {
            return;
        }
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
    fn sub_ask(&mut self) {
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

    // ------------------------------------------------------------ events

    /// One of the four `Sub` events or the answer to a request. Nothing here draws unless the run is open.
    pub(super) fn handle_sub(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::SubState { info } => self.sub_state(info),
            AgentEvent::Sub { id, item } => {
                let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) else { return };
                r.seen += 1;
                let (count, exact) = (r.seen, r.exact);
                let Some(v) = self.sub_view.as_mut().filter(|v| v.remote.is_none() && v.id == id && !v.main_in) else {
                    return;
                };
                if matches!(item, SubItem::Person { .. }) && v.pending > 0 {
                    v.pending -= 1;
                    self.dirty = true;
                }
                if !exact {
                    return self.sub_ask();
                }
                // before the answer, and up to the count it holds, the item is in the view already
                if !v.answered || count <= v.base {
                    if v.asking.is_some() {
                        v.again = true;
                    }
                    return;
                }
                v.grown += 1;
                self.sub_apply(item);
                // Byte eviction can happen after any item, not only after KEEP_ITEMS.
                self.sub_ask();
            }
            AgentEvent::SubDelta { id, .. } => {
                // Deltas and snapshot answers travel on different queues. Re-read the
                // authoritative bounded partial, coalescing events while a read is pending.
                if self.sub_view.as_ref().is_some_and(|v| v.remote.is_none() && v.id == id && !v.main_in) {
                    self.sub_ask();
                }
            }
            AgentEvent::SubNotDelivered { id, why, text } => {
                if let Some(v) = self.sub_view.as_mut().filter(|v| v.remote.is_none() && v.id == id && !v.main_in) {
                    v.pending = v.pending.saturating_sub(1);
                    let draft = self.input.full_text();
                    let recovered = recover_rejected(&draft, &text);
                    self.input.clear();
                    self.input.set_text(&recovered);
                    self.dirty = true;
                    self.toast(Kind::Error, &format!("not delivered: {why} · restored in editor"));
                } else {
                    self.input.push_history(&text);
                    self.toast(Kind::Error, &format!("not delivered: {why} · recover from input history"));
                }
            }
            AgentEvent::SubTranscript { id, request, transcript }
                if self
                    .sub_view
                    .as_ref()
                    .is_some_and(|v| v.remote.is_none() && v.id == id && !v.main_in && v.asking == Some(request)) =>
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
        if self.sub_view.as_ref().is_some_and(|v| v.remote.is_none() && v.id == id && !v.main_in) {
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
            v.asking = None;
            std::mem::take(&mut v.again)
        };
        // Preserve a selection only when the entire selected prefix remains unchanged.
        let old_blocks: Vec<String> = self.blocks.iter().map(|b| format!("{b:?}")).collect();
        let old_offsets = self.offsets.clone();
        let old_total_lines = self.total_lines;
        let old_selection = self.selection.take();
        let old_selected = self.selected;
        let old_reveal = self.reveal;
        self.blocks.clear();
        self.cache.clear();
        self.looks.clear();
        self.offsets.clear();
        self.total_lines = 0;
        self.selected = None;
        self.reveal = None;
        self.selection = None;
        // Keep the search query and focus; matches are recomputed against the new layout.
        self.inline_flushed = 0;
        match t {
            Some(t) => {
                if t.dropped > 0 {
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("{} earlier conversation items no longer kept", t.dropped),
                    });
                }
                for item in t.items {
                    self.sub_apply(item);
                }
                if !t.partial.is_empty() {
                    self.sub_delta(&t.partial);
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
        if self.sub_view.as_ref().is_some_and(|v| v.remote.is_none()) {
            if let Some(r) = self.sub_runs.iter_mut().find(|r| r.info.id == id) {
                if !r.exact || r.seen > n {
                    r.exact = false;
                    r.seen = n;
                }
            }
        }
        if let Some(v) = self.sub_view.as_mut() {
            v.answered = true;
            v.base = n;
            v.grown = 0;
        }
        if self.search.is_some() {
            self.layout(self.last_width);
            self.rematch();
        }
        let unchanged =
            old_blocks.iter().zip(&self.blocks).take_while(|(old, new)| **old == format!("{new:?}")).count();
        self.selected = old_selected.filter(|&i| i < unchanged);
        self.reveal = old_reveal.filter(|&i| i < unchanged);
        self.selection = old_selection.filter(|sel| {
            let last = sel.anchor.line.max(sel.head.line);
            let end = old_offsets.get(unchanged).copied().unwrap_or(old_total_lines);
            last < end
        });
        self.dirty = true;
        if again {
            if let Some(v) = self.sub_view.as_mut() {
                v.again = true;
            }
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
            // what the agent that started it sent (`send_message` to its job): a message it read, not the person's
            SubItem::Parent { text } => {
                self.close_streaming();
                let stage = nexo_core::mail::Stage::Read;
                self.push(Block::Mail { id: String::new(), outgoing: false, peer: "main agent".into(), text, stage });
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
            SubItem::Call { id, name, summary, args } => {
                self.close_streaming();
                let started = Some(self.now());
                self.push(Block::Tool {
                    id,
                    name: safe_tool_text(&name),
                    summary: safe_tool_text(&summary),
                    args: safe_tool_args(&args),
                    status: ToolStatus::Running,
                    progress: Vec::new(),
                    output: None,
                    started,
                    elapsed_ms: None,
                    folded: true,
                });
            }
            SubItem::Result { id, ok, bytes: _, output: content } => {
                let now = self.now();
                if let Some(i) = self.tool_index(&id) {
                    if let Block::Tool { status, output, started, elapsed_ms, .. } = &mut self.blocks[i] {
                        *status = if ok { ToolStatus::Done } else { ToolStatus::Failed };
                        *output = Some(if ok {
                            ToolOutput::ok(safe_tool_text(&content))
                        } else {
                            ToolOutput::error(safe_tool_text(&content))
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
        if self.sub_view.as_ref().is_some_and(|v| v.remote.is_some()) {
            self.submit_remote(trimmed.to_owned());
            return;
        }
        let Some(id) = self.sub_view.as_ref().map(|v| v.id.clone()) else { return };
        let trimmed = trimmed.to_string();
        self.input.push_history(&trimmed);
        self.input.clear();
        self.send(AgentCommand::SubSubmit { id, text: trimmed });
        if let Some(v) = self.sub_view.as_mut() {
            v.pending += 1;
        }
        self.scroll_to_end();
        self.dirty = true;
    }

    /// Esc in the view: the text goes first, then the view. Nothing is interrupted.
    pub(super) fn esc_sub(&mut self) {
        if let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) {
            match &r.delivery {
                RemoteDelivery::Sending { .. } => {
                    self.toast(Kind::Info, "remote delivery pending · please wait");
                    return;
                }
                RemoteDelivery::Unknown { text, .. } => {
                    if !r.abandon_unknown_confirm {
                        r.abandon_unknown_confirm = true;
                        self.toast(
                            Kind::Error,
                            "delivery may have succeeded · Esc again to leave; check before resending",
                        );
                        return;
                    }
                    let uncertain = text.clone();
                    let draft = self.input.full_text();
                    self.input.push_history(&uncertain);
                    if !draft.is_empty() {
                        self.input.push_history(&draft);
                    }
                    r.delivery = RemoteDelivery::Ready;
                    self.close_sub();
                    self.toast(
                        Kind::Error,
                        "delivery still unknown · texts saved in input history; check before resending",
                    );
                    return;
                }
                RemoteDelivery::Ready => {}
            }
        }
        if self.input.is_empty() {
            self.close_sub();
        } else {
            self.input.clear();
        }
        self.dirty = true;
    }
}

// Merge only contiguous, identity-consistent deltas; retain at most the host's bounded suffix.
fn merge_remote(
    slot: &mut Option<nexo_core::agent::ObservationSnapshot>,
    update: RemoteUpdate,
) -> Result<bool, &'static str> {
    let (next, kind) = match update {
        RemoteUpdate::Initial(s) => (s, 0),
        RemoteUpdate::Delta(s) => (s, 1),
        RemoteUpdate::Resync(s) => (s, 2),
    };
    if kind == 1 {
        let old = slot.as_mut().ok_or("delta before initial snapshot")?;
        if old.instance_id != next.instance_id
            || old.session_id != next.session_id
            || old.generation != next.generation
            || next.sequence < old.sequence
            || next.dropped < old.dropped
            || next.dropped > old.sequence
            || next.events.len() as u64 != next.sequence - old.sequence
            || next.events.iter().enumerate().any(|(i, e)| e.sequence != old.sequence + i as u64 + 1)
        {
            return Err("noncontiguous delta");
        }
        old.events.retain(|e| e.sequence > next.dropped);
        old.events.extend(next.events);
        old.sequence = next.sequence;
        old.dropped = next.dropped;
        old.permission_waiting = next.permission_waiting;
        old.question_waiting = next.question_waiting;
        Ok(false)
    } else {
        if (kind == 0) != slot.is_none() {
            return Err("unexpected snapshot");
        }
        if next.dropped > next.sequence
            || next.events.len() as u64 != next.sequence - next.dropped
            || next.events.iter().enumerate().any(|(i, e)| e.sequence != next.dropped + i as u64 + 1)
        {
            return Err("invalid snapshot sequence");
        }
        if let Some(old) = slot.as_ref() {
            if old.instance_id != next.instance_id
                || old.session_id != next.session_id
                || old.generation != next.generation
                || next.sequence < old.sequence
            {
                return Err("invalid resynchronization");
            }
        }
        *slot = Some(next);
        Ok(kind == 2)
    }
}

/// Keep later edits intact; a draft that starts with the sent text already
/// contains the whole rejected message, including when it has been extended.
fn recover_rejected(draft: &str, sent: &str) -> String {
    if draft.starts_with(sent) {
        draft.to_owned()
    } else if draft.is_empty() {
        sent.to_owned()
    } else {
        format!("{draft}\n\n{sent}")
    }
}

/// The job a run belongs to: run ids end in `#` and the job's number.
pub fn job_of_run(id: &str) -> Option<u32> {
    id.rsplit_once('#')?.1.parse().ok()
}

/// Never pass terminal control characters from a tool to the terminal renderer.
fn safe_tool_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

fn safe_tool_args(text: &str) -> serde_json::Value {
    fn clean(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) => *s = safe_tool_text(s),
            serde_json::Value::Array(a) => a.iter_mut().for_each(clean),
            serde_json::Value::Object(o) => {
                let old = std::mem::take(o);
                for (key, mut value) in old {
                    clean(&mut value);
                    o.insert(safe_tool_text(&key), value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_owned()));
    clean(&mut value);
    value
}

/// Rebuild from the retained atomic event suffix. History is indexed, whereas live
/// assistant completion and tool events overlap their streamed representations.
fn remote_items(snapshot: &nexo_core::agent::ObservationSnapshot) -> (Vec<SubItem>, String) {
    use std::collections::HashSet;
    let mut items = Vec::new();
    let mut partial = String::new();
    let mut flushed = String::new();
    let mut history = HashSet::new();
    let mut calls = HashSet::new();
    let mut results = HashSet::new();
    let mut expected = 1u64;
    if snapshot.dropped > 0 {
        items.push(SubItem::Text {
            text: format!("[remote observation: {} earlier events no longer kept]", snapshot.dropped),
        });
        expected = snapshot.dropped.saturating_add(1);
    }
    for event in &snapshot.events {
        if event.sequence != expected {
            items.push(SubItem::Text {
                text: format!("[remote observation sequence gap: expected {expected}, got {}]", event.sequence),
            });
        }
        expected = event.sequence.saturating_add(1);
        if event.truncated {
            items.push(SubItem::Text { text: format!("[remote event {} truncated or parts omitted]", event.sequence) });
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&event.json) else {
            if !event.truncated {
                items.push(SubItem::Text { text: format!("[remote event {} invalid]", event.sequence) });
            }
            continue;
        };
        match event.kind.as_str() {
            "history" | "assistant_done" => {
                if event.kind == "history" && !history.insert(v[0].as_u64()) {
                    continue;
                }
                let msg = if event.kind == "history" { &v[1] } else { &v };
                let role = msg[0].as_str().unwrap_or("").to_ascii_lowercase();
                if event.kind == "assistant_done" {
                    partial.clear();
                }
                if let Some(parts) = msg[1].as_array() {
                    let full_text: String = parts
                        .iter()
                        .filter(|p| p["type"].as_str() == Some("text"))
                        .map(|p| safe_tool_text(p["text"].as_str().unwrap_or("")))
                        .collect();
                    let mut skip = if event.kind == "assistant_done" && full_text.starts_with(&flushed) {
                        flushed.len()
                    } else {
                        0
                    };
                    for part in parts {
                        match part["type"].as_str().unwrap_or("") {
                            "text" if role == "user" => items
                                .push(SubItem::Person { text: safe_tool_text(part["text"].as_str().unwrap_or("")) }),
                            "text" if role == "assistant" => {
                                let text = safe_tool_text(part["text"].as_str().unwrap_or(""));
                                let n = skip.min(text.len());
                                skip -= n;
                                if n < text.len() {
                                    items.push(SubItem::Text { text: text[n..].to_owned() });
                                }
                            }
                            "reasoning" => items.push(SubItem::Text {
                                text: format!(
                                    "[remote reasoning] {}",
                                    safe_tool_text(part["text"].as_str().unwrap_or(""))
                                ),
                            }),
                            "tool_call" => {
                                remote_call(&mut items, &mut calls, &part["id"], &part["name"], &part["args"], "")
                            }
                            "tool_result" => remote_result(
                                &mut items,
                                &mut calls,
                                &mut results,
                                &part["call_id"],
                                part["is_error"].as_bool().unwrap_or(false),
                                &remote_result_text(&part["content"]),
                            ),
                            "omitted" => items.push(SubItem::Text { text: "[remote message part omitted]".into() }),
                            _ => {}
                        }
                    }
                }
                if event.kind == "assistant_done" {
                    flushed.clear();
                }
            }
            "text_delta" => {
                if let Some(t) = v.as_str() {
                    partial.push_str(&safe_tool_text(t));
                }
            }
            "assistant_start" | "turn_end" => {
                if !partial.is_empty() {
                    items.push(SubItem::Text { text: std::mem::take(&mut partial) });
                }
                flushed.clear();
            }
            "tool_call" => {
                if !partial.is_empty() {
                    flushed.push_str(&partial);
                    items.push(SubItem::Text { text: std::mem::take(&mut partial) });
                }
                remote_call(&mut items, &mut calls, &v[0], &v[1], &v[2], v[3].as_str().unwrap_or(""));
            }
            "tool_done" => remote_result(
                &mut items,
                &mut calls,
                &mut results,
                &v[0],
                v[2].as_bool().unwrap_or(false),
                v[1].as_str().unwrap_or(""),
            ),
            "tool_progress" => {
                if let (Some(id), Some(line)) = (v[0].as_str(), v[1].as_str()) {
                    items.push(SubItem::Text {
                        text: format!("[remote tool {} progress] {}", safe_tool_text(id), safe_tool_text(line)),
                    });
                }
            }
            "preparing" | "notice" | "shell_output" => items
                .push(SubItem::Text { text: format!("[remote {}] {}", event.kind, safe_tool_text(&v.to_string())) }),
            _ => {}
        }
    }
    if expected <= snapshot.sequence {
        items.push(SubItem::Text {
            text: format!("[remote observation sequence gap: events {expected}–{} unavailable]", snapshot.sequence),
        });
    }
    if snapshot.permission_waiting {
        items.push(SubItem::Text { text: "[remote session waiting for permission]".into() });
    }
    if snapshot.question_waiting {
        items.push(SubItem::Text { text: "[remote session waiting for an answer]".into() });
    }
    (items, partial)
}

fn remote_call(
    items: &mut Vec<SubItem>,
    calls: &mut std::collections::HashSet<String>,
    id: &serde_json::Value,
    name: &serde_json::Value,
    args: &serde_json::Value,
    summary: &str,
) {
    if let (Some(id), Some(name)) = (id.as_str(), name.as_str()) {
        if calls.insert(id.to_owned()) {
            items.push(SubItem::Call {
                id: id.into(),
                name: safe_tool_text(name),
                args: args.to_string(),
                summary: safe_tool_text(summary),
            });
        }
    }
}

fn remote_result(
    items: &mut Vec<SubItem>,
    calls: &mut std::collections::HashSet<String>,
    results: &mut std::collections::HashSet<String>,
    id: &serde_json::Value,
    error: bool,
    output: &str,
) {
    if let Some(id) = id.as_str() {
        if results.insert(id.to_owned()) {
            if calls.insert(id.to_owned()) {
                items.push(SubItem::Call {
                    id: id.into(),
                    name: "unknown remote tool (call not retained)".into(),
                    args: "null".into(),
                    summary: String::new(),
                });
            }
            items.push(SubItem::Result {
                id: id.into(),
                ok: !error,
                bytes: output.len(),
                output: safe_tool_text(output),
            });
        }
    }
}

fn remote_result_text(parts: &serde_json::Value) -> String {
    parts
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| match p["type"].as_str() {
                    Some("text") | Some("reasoning") => p["text"].as_str().unwrap_or("").to_owned(),
                    _ => "[remote result part omitted]".into(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "[remote result unavailable]".into())
}

#[cfg(test)]
mod remote_tests {
    use super::*;
    use nexo_core::agent::{channels, AgentEvent};
    use nexo_core::message::{Message, Part, Role};
    use nexo_core::tool::ToolOutput;

    #[test]
    fn rejected_drafts_preserve_extensions_and_independent_edits() {
        assert_eq!(recover_rejected("hola mundo", "hola"), "hola mundo");
        assert_eq!(recover_rejected("hola", "hola"), "hola");
        assert_eq!(recover_rejected("", "hola"), "hola");
        assert_eq!(recover_rejected("otra cosa", "hola"), "otra cosa\n\nhola");
    }

    #[test]
    fn remote_rejection_keeps_retyped_extension() {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(crate::TuiOptions::default(), commands);
        let (_tx, rx) = std::sync::mpsc::sync_channel(1);
        app.open_remote_receiver("remote", rx);
        app.input.set_text("hola mundo");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.sub_view.as_mut().unwrap().remote.as_mut().unwrap().delivery =
            RemoteDelivery::Sending { text: "hola".into(), request_id: "id".into(), rx };
        tx.send(RemoteReceipt::Rejected("offline".into())).unwrap();
        app.remote_delivery_tick();
        assert_eq!(app.input.full_text(), "hola mundo");
    }

    #[test]
    fn empty_remote_polls_do_not_rebuild_or_accumulate_notices() {
        let (commands, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(crate::TuiOptions::default(), commands);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.open_remote_receiver("remote", rx);
        let (observer, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        let first = observer.observation.snapshot();
        tx.send(Ok(RemoteUpdate::Initial(first.clone()))).unwrap();
        app.remote_tick();
        let initial = format!("{:?}", app.blocks);
        app.dirty = false;
        for _ in 0..100 {
            tx.send(Ok(RemoteUpdate::Delta(observer.observation.snapshot_since(&first).unwrap()))).unwrap();
            app.remote_tick();
        }
        assert_eq!(format!("{:?}", app.blocks), initial);
        assert!(!app.dirty);
        tx.send(Err("offline".into())).unwrap();
        app.remote_tick();
        assert!(format!("{:?}", app.blocks).contains("offline"));
        tx.send(Ok(RemoteUpdate::Delta(observer.observation.snapshot_since(&first).unwrap()))).unwrap();
        app.remote_tick();
        assert!(!format!("{:?}", app.blocks).contains("offline"));
    }

    #[test]
    fn incremental_merge_rejects_duplicates_and_resyncs_after_eviction() {
        let (handle, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        ends.events.send(AgentEvent::TextDelta { text: "one".into() }).unwrap();
        let first = handle.observation.snapshot();
        let mut state = None;
        assert!(!merge_remote(&mut state, RemoteUpdate::Initial(first.clone())).unwrap());
        ends.events.send(AgentEvent::TextDelta { text: "two".into() }).unwrap();
        let delta = handle.observation.snapshot_since(&first).unwrap();
        assert!(!merge_remote(&mut state, RemoteUpdate::Delta(delta.clone())).unwrap());
        assert_eq!(remote_items(state.as_ref().unwrap()).1, "onetwo");
        assert!(merge_remote(&mut state, RemoteUpdate::Delta(delta)).is_err());
        for _ in 0..520 {
            ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
        }
        let replacement = handle.observation.snapshot_since(&first).unwrap_err();
        assert!(merge_remote(&mut state, RemoteUpdate::Resync(replacement)).unwrap());
        assert!(state.as_ref().unwrap().dropped > 0);
        assert!(format!("{:?}", remote_items(state.as_ref().unwrap()).0).contains("earlier events"));
    }

    #[test]
    fn replay_and_live_tool_events_do_not_duplicate() {
        let (handle, ends) = channels();
        let tx = ends.events;
        tx.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        let msg = Message::new(
            Role::Assistant,
            vec![
                Part::text("hello"),
                Part::ToolCall { id: "call".into(), name: "bash".into(), args: serde_json::json!({"cmd":"ls"}) },
            ],
        );
        tx.send(AgentEvent::History { index: 0, message: msg }).unwrap();
        tx.send(AgentEvent::ToolCall {
            id: "call".into(),
            name: "bash".into(),
            args: serde_json::json!({"cmd":"ls"}),
            summary: "ls".into(),
        })
        .unwrap();
        tx.send(AgentEvent::ToolDone { id: "call".into(), output: ToolOutput::ok("done"), elapsed_ms: 0 }).unwrap();
        tx.send(AgentEvent::TextDelta { text: "live".into() }).unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Call { .. })).count(), 1);
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Result { output, .. } if output == "done")).count(), 1);
        assert_eq!(partial, "live");
        tx.send(AgentEvent::AssistantDone { message: Message::new(Role::Assistant, vec![Part::text("live reply")]) })
            .unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert!(partial.is_empty());
        assert_eq!(items.iter().filter(|i| matches!(i, SubItem::Text { text } if text == "live reply")).count(), 1);
    }

    #[test]
    fn streamed_text_before_tool_is_not_repeated_on_completion() {
        let (handle, ends) = channels();
        let tx = ends.events;
        tx.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        tx.send(AgentEvent::TextDelta { text: "before ".into() }).unwrap();
        tx.send(AgentEvent::ToolCall {
            id: "c".into(),
            name: "bash".into(),
            args: serde_json::json!({}),
            summary: String::new(),
        })
        .unwrap();
        tx.send(AgentEvent::TextDelta { text: "after".into() }).unwrap();
        tx.send(AgentEvent::AssistantDone {
            message: Message::new(
                Role::Assistant,
                vec![
                    Part::text("before "),
                    Part::ToolCall { id: "c".into(), name: "bash".into(), args: serde_json::json!({}) },
                    Part::text("after"),
                ],
            ),
        })
        .unwrap();
        let (items, partial) = remote_items(&handle.observation.snapshot());
        assert!(partial.is_empty());
        assert!(
            matches!(&items[..], [SubItem::Text { text: a }, SubItem::Call { .. }, SubItem::Text { text: b }] if a == "before " && b == "after"),
            "{items:?}"
        );
    }

    #[test]
    fn gaps_truncation_and_orphan_result_are_visible() {
        let (handle, ends) = channels();
        ends.events.send(AgentEvent::SessionChanged { session_id: "remote".into(), title: String::new() }).unwrap();
        ends.events
            .send(AgentEvent::ToolDone { id: "lost".into(), output: ToolOutput::error("bad"), elapsed_ms: 0 })
            .unwrap();
        let mut snap = handle.observation.snapshot();
        snap.dropped = 3;
        snap.events[0].sequence = 5;
        snap.sequence = 6;
        let (items, _) = remote_items(&snap);
        let text = format!("{items:?}");
        assert!(text.contains("earlier events no longer kept"));
        assert!(text.contains("sequence gap"));
        assert!(text.contains("call not retained"));
        assert!(text.contains("bad"));
        snap.events[0].truncated = true;
        assert!(format!("{:?}", remote_items(&snap).0).contains("truncated"));
    }
}
