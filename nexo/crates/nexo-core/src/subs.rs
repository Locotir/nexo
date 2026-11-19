//! The conversation of each sub-agent run, kept so it can be watched and written to from outside.
//!
//! One entry per run (the run's own id, never a provider's call id). An entry keeps the newest
//! [`KEEP_ITEMS`] items and [`KEEP_BYTES`] bytes of text, says how many older ones went, and has an
//! inbox: a message the person or the agent that started it sends joins the sub-agent's conversation between
//! its rounds. Every change
//! is also sent on a bounded channel; a receiver that is slow or gone loses events, never blocks the
//! sub-agent, and the registry stays whole. Locks here are never held across an `.await`.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::stream::Usage;

mod flow;
pub use flow::{FlowInfo, FlowPhase, FlowRun, FlowState, FlowStep, StepState};

/// Items one sub-agent keeps; older ones go first.
pub const KEEP_ITEMS: usize = 200;
/// Bytes of text one sub-agent keeps (and the most one item may hold).
pub const KEEP_BYTES: usize = 256 * 1024;
/// Ended runs kept; the oldest go first.
pub const KEEP_RUNS: usize = 32;
/// Messages waiting in one sub-agent's inbox.
const INBOX_MAX: usize = 32;
/// Characters of the title.
const TITLE_CHARS: usize = 80;
/// Events the channel holds for a slow receiver.
const CHANGES: usize = 4096;
/// Maximum bytes retained for each tool argument or output payload.
pub const KEEP_TOOL_BYTES: usize = 16 * 1024;

/// One thing in a sub-agent's conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum SubItem {
    /// The task it was given.
    Task { text: String },
    /// A reply it wrote, whole.
    Text { text: String },
    /// Tool arguments are JSON text; oversized values are explicitly truncated.
    Call { id: String, name: String, summary: String, args: String },
    /// Text output only (images are not retained). `bytes` is the original size.
    Result { id: String, ok: bool, bytes: usize, output: String },
    /// A message a person sent it.
    Person { text: String },
    /// A message the agent that started it sent it (`send_message` to its job).
    Parent { text: String },
}

impl SubItem {
    fn size(&self) -> usize {
        match self {
            SubItem::Task { text } | SubItem::Text { text } | SubItem::Person { text } | SubItem::Parent { text } => {
                text.len()
            }
            SubItem::Call { id, name, summary, args } => id.len() + name.len() + summary.len() + args.len(),
            SubItem::Result { id, output, .. } => id.len() + output.len(),
        }
    }

    /// The item with its text within [`KEEP_BYTES`].
    fn bounded(self) -> SubItem {
        match self {
            SubItem::Task { text } => SubItem::Task { text: cut(&text, KEEP_BYTES) },
            SubItem::Text { text } => SubItem::Text { text: cut(&text, KEEP_BYTES) },
            SubItem::Person { text } => SubItem::Person { text: cut(&text, KEEP_BYTES) },
            SubItem::Parent { text } => SubItem::Parent { text: cut(&text, KEEP_BYTES) },
            SubItem::Call { id, name, summary, args } => SubItem::Call {
                id: cut(&id, 1024),
                name: cut(&name, 1024),
                summary: cut(&summary, KEEP_TOOL_BYTES),
                args: cut(&args, KEEP_TOOL_BYTES),
            },
            SubItem::Result { id, ok, bytes, output } => {
                SubItem::Result { id: cut(&id, 1024), ok, bytes, output: cut(&output, KEEP_TOOL_BYTES) }
            }
        }
    }
}

/// `text` within `max` bytes, cut on a character boundary; a cut text says how much went.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut keep = max.saturating_sub(40);
    while !text.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}… [{} bytes cut]", &text[..keep], text.len() - keep)
}

/// Where a sub-agent run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubState {
    /// Running, and the call that started it waits for it.
    Attached,
    /// Running in the background.
    Background,
    /// Ended with a report.
    Ok,
    /// Ended without one.
    Failed,
    /// Stopped by the person or the end of the session.
    Killed,
}

impl SubState {
    pub fn running(self) -> bool {
        matches!(self, SubState::Attached | SubState::Background)
    }
}

/// What a view lists of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubInfo {
    pub id: String,
    pub profile: String,
    /// Effective target, absent until resolved by the runner.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Effective request effort; None means no level was specified (also before target resolution).
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// The first line of the task.
    pub title: String,
    pub state: SubState,
    /// Unix milliseconds.
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Input tokens (cached ones included) and output tokens, as this run spent them.
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// What this run cost (already part of the session total).
    pub cost_usd: f64,
}

/// What a view that opens late reads: the newest items, and how many older ones went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubTranscript {
    pub items: Vec<SubItem>,
    pub dropped: usize,
    /// Bounded text of the current model attempt, if any.
    #[serde(default)]
    pub partial: String,
}

/// A change of the registry, in the order it was made.
#[derive(Debug, Clone, PartialEq)]
pub enum SubChange {
    /// A run began, or its state changed.
    State(SubInfo),
    /// An item was added to the transcript.
    Item { id: String, item: SubItem },
    /// Part of a reply being written (the whole reply follows as a `Text` item).
    Delta { id: String, text: String },
    /// A message to a run could not be delivered.
    NotDelivered { id: String, why: String, text: String },
    /// A workflow run began, or something in it changed.
    Flow(FlowInfo),
}

struct Data {
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    state: SubState,
    ended_ms: Option<u64>,
    tokens_in: u64,
    tokens_out: u64,
    cost_usd: f64,
    items: VecDeque<SubItem>,
    bytes: usize,
    dropped: usize,
    partial: String,
    partial_cut: bool,
    /// `Person` and `Parent` items, oldest first.
    inbox: VecDeque<SubItem>,
}

struct Entry {
    id: String,
    profile: String,
    title: String,
    started_ms: u64,
    data: Mutex<Data>,
}

impl Entry {
    fn info(&self, d: &Data) -> SubInfo {
        SubInfo {
            id: self.id.clone(),
            profile: self.profile.clone(),
            provider: d.provider.clone(),
            model: d.model.clone(),
            reasoning_effort: d.reasoning_effort.clone(),
            title: self.title.clone(),
            state: d.state,
            started_ms: self.started_ms,
            ended_ms: d.ended_ms,
            tokens_in: d.tokens_in,
            tokens_out: d.tokens_out,
            cost_usd: d.cost_usd,
        }
    }
}

fn now_ms() -> u64 {
    crate::mailbox::now_ms().max(0) as u64
}

struct Inner {
    runs: Mutex<Vec<Arc<Entry>>>,
    /// The workflow runs ([`flow`]), and how many began in this process (the next one is `wf#` of that plus one).
    flows: Mutex<Vec<Arc<flow::Entry>>>,
    flow_seq: AtomicU64,
    /// Counts the times the registry was cleared; a run of an older count sends nothing. Only [`Subs::clear`]
    /// changes it, with `flows` locked, so a run that reads it under that lock gets the count of the session it
    /// is entered in.
    generation: AtomicU64,
    /// Held while a run checks the count and sends, and while a clear changes it: a clear and what a run still
    /// sent before it are in order, so the one who drains the channel after the clear leaves nothing of the old.
    gate: Mutex<()>,
    changes: broadcast::Sender<SubChange>,
}

/// The sub-agent runs of a session. Cloning shares them.
#[derive(Clone)]
pub struct Subs(Arc<Inner>);

impl Default for Subs {
    fn default() -> Subs {
        Subs::new()
    }
}

impl std::fmt::Debug for Subs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.list()).finish()
    }
}

impl Subs {
    pub fn new() -> Subs {
        let (changes, _) = broadcast::channel(CHANGES);
        Subs(Arc::new(Inner {
            runs: Mutex::new(Vec::new()),
            flows: Mutex::new(Vec::new()),
            flow_seq: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            gate: Mutex::new(()),
            changes,
        }))
    }

    /// Every change from now on. A receiver that falls behind loses the oldest ones (`Lagged`): the
    /// registry has them all.
    pub fn changes(&self) -> broadcast::Receiver<SubChange> {
        self.0.changes.subscribe()
    }

    fn entry(&self, id: &str) -> Option<Arc<Entry>> {
        self.0.runs.lock().unwrap().iter().find(|e| e.id == id).cloned()
    }

    /// A run begins with `task`. `background`: it was started in the background, else a call waits for it.
    pub fn begin(&self, id: &str, profile: &str, task: &str, background: bool) -> SubRun {
        self.begin_titled(id, profile, task.lines().next().unwrap_or(""), task, background)
    }

    /// Like [`Subs::begin`], with a title of its own instead of the first line of the task (a step of a
    /// workflow is listed by its place in the workflow).
    pub fn begin_titled(&self, id: &str, profile: &str, title: &str, task: &str, background: bool) -> SubRun {
        let title: String = title.lines().next().unwrap_or("").chars().take(TITLE_CHARS).collect();
        let entry = Arc::new(Entry {
            id: id.to_string(),
            profile: profile.to_string(),
            title,
            started_ms: now_ms(),
            data: Mutex::new(Data {
                provider: None,
                model: None,
                reasoning_effort: None,
                state: if background { SubState::Background } else { SubState::Attached },
                ended_ms: None,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                items: VecDeque::new(),
                bytes: 0,
                dropped: 0,
                partial: String::new(),
                partial_cut: false,
                inbox: VecDeque::new(),
            }),
        });
        {
            let mut runs = self.0.runs.lock().unwrap();
            runs.retain(|e| e.id != id);
            runs.push(entry.clone());
        }
        let run = SubRun { entry, subs: self.clone(), generation: self.0.generation.load(Ordering::SeqCst) };
        {
            let d = run.entry.data.lock().unwrap();
            let _ = self.0.changes.send(SubChange::State(run.entry.info(&d)));
        }
        run.item(SubItem::Task { text: task.to_string() });
        run
    }

    /// The runs, oldest first.
    pub fn list(&self) -> Vec<SubInfo> {
        let runs = self.0.runs.lock().unwrap().clone();
        runs.iter().map(|e| e.info(&e.data.lock().unwrap())).collect()
    }

    /// The kept conversation of run `id`.
    pub fn transcript(&self, id: &str) -> Option<SubTranscript> {
        let e = self.entry(id)?;
        let d = e.data.lock().unwrap();
        Some(SubTranscript { items: d.items.iter().cloned().collect(), dropped: d.dropped, partial: d.partial.clone() })
    }

    /// A person's message for run `id`: it joins that run's conversation between its rounds. When it
    /// cannot (the id is unknown, the run ended, the inbox is full) nothing is kept, an event says so,
    /// and the reason comes back.
    pub fn submit(&self, id: &str, text: &str) -> Result<(), String> {
        self.put(id, text, false)
    }

    /// A message for run `id` from the agent that started it, kept like [`Subs::submit`]'s. A refusal is
    /// only the reason that comes back: it is the model's tool result, not news for the person's screen.
    pub fn submit_from_parent(&self, id: &str, text: &str) -> Result<(), String> {
        self.put(id, text, true)
    }

    fn put(&self, id: &str, text: &str, parent: bool) -> Result<(), String> {
        let why = match self.entry(id) {
            None => Some("there is no sub-agent with that id".to_string()),
            Some(_) if text.trim().is_empty() => Some("the message is empty".to_string()),
            Some(e) => {
                let mut d = e.data.lock().unwrap();
                if !d.state.running() {
                    Some("that sub-agent has already ended".to_string())
                } else if d.inbox.len() >= INBOX_MAX {
                    Some("that sub-agent has too many messages waiting".to_string())
                } else {
                    let text = cut(text, KEEP_BYTES);
                    d.inbox.push_back(if parent { SubItem::Parent { text } } else { SubItem::Person { text } });
                    None
                }
            }
        };
        match why {
            None => Ok(()),
            Some(why) => {
                if !parent {
                    let _ = self.0.changes.send(SubChange::NotDelivered {
                        id: id.to_string(),
                        why: why.clone(),
                        text: text.to_string(),
                    });
                }
                Err(why)
            }
        }
    }

    /// The call that waited for run `id` let go of it.
    pub fn to_background(&self, id: &str) {
        let Some(e) = self.entry(id) else { return };
        let mut d = e.data.lock().unwrap();
        if d.state == SubState::Attached {
            d.state = SubState::Background;
            let _ = self.0.changes.send(SubChange::State(e.info(&d)));
        }
    }

    /// The session starts over: no run is kept.
    ///
    /// The count of workflow runs goes on: `wf#N` is never given twice in one process, so a late command that
    /// names an old run cannot reach a new one.
    pub fn clear(&self) {
        let mut runs = self.0.runs.lock().unwrap();
        // the count changes with `flows` locked, so a run that begins meanwhile either is in the list this clears
        // (and has the old count) or gets the new count and stays
        let mut flows = self.0.flows.lock().unwrap();
        {
            let _gate = self.0.gate.lock().unwrap();
            self.0.generation.fetch_add(1, Ordering::SeqCst);
        }
        runs.clear();
        flows.clear();
    }

    /// Sends `c` for a run that began at count `generation`, unless the registry was cleared since. Checking and
    /// sending are one step against [`Subs::clear`].
    fn send_if_live(&self, generation: u64, c: SubChange) {
        let _gate = self.0.gate.lock().unwrap();
        if self.0.generation.load(Ordering::SeqCst) == generation {
            let _ = self.0.changes.send(c);
        }
    }

    /// Drops the ended runs beyond [`KEEP_RUNS`], the one that ended longest ago first. Run `keep` (the one
    /// that just ended) stays.
    fn prune(&self, keep: &str) {
        let mut runs = self.0.runs.lock().unwrap();
        let mut ended: Vec<(u64, String)> = runs
            .iter()
            .filter_map(|e| {
                let d = e.data.lock().unwrap();
                (!d.state.running() && e.id != keep).then(|| (d.ended_ms.unwrap_or(0), e.id.clone()))
            })
            .collect();
        // the one just ended counts toward the limit
        let over = (ended.len() + 1).saturating_sub(KEEP_RUNS);
        if over == 0 {
            return;
        }
        ended.sort_by_key(|(at, _)| *at);
        let gone: HashSet<String> = ended.into_iter().take(over).map(|(_, id)| id).collect();
        runs.retain(|e| !gone.contains(&e.id));
    }
}

/// The side a sub-agent's loop writes to. Dropping it before `end` ends the run as killed.
pub struct SubRun {
    entry: Arc<Entry>,
    subs: Subs,
    /// The registry's count of clears when this run began.
    generation: u64,
}

/// Reads what one run has spent. It outlives the registry's entry: a run pruned from the list can still be read.
#[derive(Clone)]
pub struct SubMeter(Arc<Entry>);

impl SubMeter {
    /// Input tokens (cached ones included) and output tokens, as the run spent them so far.
    pub fn tokens(&self) -> (u64, u64) {
        let d = self.0.data.lock().unwrap();
        (d.tokens_in, d.tokens_out)
    }
}

impl SubRun {
    /// A reader of what this run spends.
    pub fn meter(&self) -> SubMeter {
        SubMeter(self.entry.clone())
    }

    /// The registry was not cleared since this run began. After a clear the run still winds down, but it
    /// tells nobody: its id belongs to a session that is gone.
    fn live(&self) -> bool {
        self.subs.0.generation.load(Ordering::SeqCst) == self.generation
    }

    fn send(&self, c: SubChange) {
        self.subs.send_if_live(self.generation, c);
    }

    pub fn id(&self) -> &str {
        &self.entry.id
    }

    /// Record the target actually selected by the runner and notify late and live readers.
    pub fn target(&self, provider: &str, model: &str, reasoning_effort: Option<&str>) {
        let mut d = self.entry.data.lock().unwrap();
        if !d.state.running() {
            return;
        }
        d.provider = Some(cut(provider, 1024));
        d.model = Some(cut(model, 1024));
        d.reasoning_effort = reasoning_effort.map(|e| cut(e, 1024));
        self.send(SubChange::State(self.entry.info(&d)));
    }

    /// Adds an item (its text bounded) and tells the watchers.
    pub fn item(&self, item: SubItem) {
        let item = item.bounded();
        let mut d = self.entry.data.lock().unwrap();
        if matches!(item, SubItem::Text { .. }) {
            d.partial.clear();
            d.partial_cut = false;
        }
        d.bytes += item.size();
        d.items.push_back(item.clone());
        while (d.items.len() > KEEP_ITEMS || d.bytes > KEEP_BYTES) && d.items.len() > 1 {
            if let Some(old) = d.items.pop_front() {
                d.bytes -= old.size();
                d.dropped += 1;
            }
        }
        self.send(SubChange::Item { id: self.entry.id.clone(), item });
    }

    /// Part of the reply being written.
    pub fn delta(&self, text: &str) {
        let mut d = self.entry.data.lock().unwrap();
        if !d.partial_cut {
            let combined = format!("{}{}", d.partial, text);
            d.partial_cut = combined.len() > KEEP_BYTES;
            d.partial = cut(&combined, KEEP_BYTES);
        }
        self.send(SubChange::Delta { id: self.entry.id.clone(), text: text.to_string() });
    }

    /// Discard an unsuccessful attempt before another is started.
    pub fn reset_partial(&self) {
        let mut d = self.entry.data.lock().unwrap();
        if !d.partial.is_empty() {
            d.partial.clear();
            d.partial_cut = false;
            self.send(SubChange::Delta { id: self.entry.id.clone(), text: String::new() });
        }
    }

    /// What one model call spent.
    pub fn spent(&self, usage: &Usage, cost_usd: f64) {
        let mut d = self.entry.data.lock().unwrap();
        d.tokens_in += usage.total_input();
        d.tokens_out += usage.output;
        d.cost_usd += cost_usd;
    }

    /// The messages waiting (`Person` and `Parent` items), taken out of the inbox (oldest first).
    pub fn take_inbox(&self) -> Vec<SubItem> {
        self.entry.data.lock().unwrap().inbox.drain(..).collect()
    }

    /// A message waits in the inbox.
    pub fn has_mail(&self) -> bool {
        !self.entry.data.lock().unwrap().inbox.is_empty()
    }

    /// The run ended as `state`. The person's messages still in the inbox were never read: each is
    /// reported as not delivered (the parent's go unreported on the screen, like its other sends); how many
    /// of the parent's were left unread comes back, for the report its ending brings. Ending twice changes
    /// nothing.
    pub fn end(&self, state: SubState) -> usize {
        let e = &self.entry;
        let mut d = e.data.lock().unwrap();
        if !d.state.running() {
            return 0;
        }
        d.state = state;
        d.ended_ms = Some(now_ms());
        self.send(SubChange::State(e.info(&d)));
        let mut unread = 0;
        for m in d.inbox.drain(..) {
            match m {
                SubItem::Person { text } => {
                    let why = "that sub-agent ended before it read the message".to_string();
                    self.send(SubChange::NotDelivered { id: e.id.clone(), why, text });
                }
                SubItem::Parent { .. } => unread += 1,
                _ => {}
            }
        }
        drop(d);
        if self.live() {
            self.subs.prune(&e.id);
        }
        unread
    }
}

impl Drop for SubRun {
    /// A run that stops without saying how (a panic, a dropped task) ends as killed.
    fn drop(&mut self) {
        self.end(SubState::Killed);
    }
}

#[cfg(test)]
mod tests;
