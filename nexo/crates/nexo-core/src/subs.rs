//! The conversation of each sub-agent run, kept so it can be watched and written to from outside.
//!
//! One entry per run (the run's own id, never a provider's call id). An entry keeps the newest
//! [`KEEP_ITEMS`] items and [`KEEP_BYTES`] bytes of text, says how many older ones went, and has an
//! inbox: a message a person sends joins the sub-agent's conversation between its rounds. Every change
//! is also sent on a bounded channel; a receiver that is slow or gone loses events, never blocks the
//! sub-agent, and the registry stays whole. Locks here are never held across an `.await`.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::stream::Usage;

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

/// One thing in a sub-agent's conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum SubItem {
    /// The task it was given.
    Task { text: String },
    /// A reply it wrote, whole.
    Text { text: String },
    /// A tool call, with the one-line summary the main transcript shows for that tool.
    Call { id: String, name: String, summary: String },
    /// How a call ended: not its output.
    Result { id: String, ok: bool, bytes: usize },
    /// A message a person sent it.
    Person { text: String },
}

impl SubItem {
    fn size(&self) -> usize {
        match self {
            SubItem::Task { text } | SubItem::Text { text } | SubItem::Person { text } => text.len(),
            SubItem::Call { id, name, summary } => id.len() + name.len() + summary.len(),
            SubItem::Result { id, .. } => id.len(),
        }
    }

    /// The item with its text within [`KEEP_BYTES`].
    fn bounded(self) -> SubItem {
        match self {
            SubItem::Task { text } => SubItem::Task { text: cut(&text, KEEP_BYTES) },
            SubItem::Text { text } => SubItem::Text { text: cut(&text, KEEP_BYTES) },
            SubItem::Person { text } => SubItem::Person { text: cut(&text, KEEP_BYTES) },
            SubItem::Call { id, name, summary } => SubItem::Call { id, name, summary: cut(&summary, KEEP_BYTES / 4) },
            r @ SubItem::Result { .. } => r,
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
    NotDelivered { id: String, why: String },
}

struct Data {
    state: SubState,
    ended_ms: Option<u64>,
    tokens_in: u64,
    tokens_out: u64,
    cost_usd: f64,
    items: VecDeque<SubItem>,
    bytes: usize,
    dropped: usize,
    inbox: VecDeque<String>,
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
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

struct Inner {
    runs: Mutex<Vec<Arc<Entry>>>,
    /// Counts the times the registry was cleared; a run of an older count sends nothing.
    generation: AtomicU64,
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
        Subs(Arc::new(Inner { runs: Mutex::new(Vec::new()), generation: AtomicU64::new(0), changes }))
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
                state: if background { SubState::Background } else { SubState::Attached },
                ended_ms: None,
                tokens_in: 0,
                tokens_out: 0,
                cost_usd: 0.0,
                items: VecDeque::new(),
                bytes: 0,
                dropped: 0,
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
        Some(SubTranscript { items: d.items.iter().cloned().collect(), dropped: d.dropped })
    }

    /// A person's message for run `id`: it joins that run's conversation between its rounds. When it
    /// cannot (the id is unknown, the run ended, the inbox is full) nothing is kept, an event says so,
    /// and the reason comes back.
    pub fn submit(&self, id: &str, text: &str) -> Result<(), String> {
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
                    d.inbox.push_back(cut(text, KEEP_BYTES));
                    None
                }
            }
        };
        match why {
            None => Ok(()),
            Some(why) => {
                let _ = self.0.changes.send(SubChange::NotDelivered { id: id.to_string(), why: why.clone() });
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
    pub fn clear(&self) {
        let mut runs = self.0.runs.lock().unwrap();
        self.0.generation.fetch_add(1, Ordering::SeqCst);
        runs.clear();
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
        if self.live() {
            let _ = self.subs.0.changes.send(c);
        }
    }

    pub fn id(&self) -> &str {
        &self.entry.id
    }

    /// Adds an item (its text bounded) and tells the watchers.
    pub fn item(&self, item: SubItem) {
        let item = item.bounded();
        let mut d = self.entry.data.lock().unwrap();
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
        let _d = self.entry.data.lock().unwrap();
        self.send(SubChange::Delta { id: self.entry.id.clone(), text: text.to_string() });
    }

    /// What one model call spent.
    pub fn spent(&self, usage: &Usage, cost_usd: f64) {
        let mut d = self.entry.data.lock().unwrap();
        d.tokens_in += usage.total_input();
        d.tokens_out += usage.output;
        d.cost_usd += cost_usd;
    }

    /// The messages people sent that are waiting, taken out of the inbox (oldest first).
    pub fn take_inbox(&self) -> Vec<String> {
        self.entry.data.lock().unwrap().inbox.drain(..).collect()
    }

    /// A message waits in the inbox.
    pub fn has_mail(&self) -> bool {
        !self.entry.data.lock().unwrap().inbox.is_empty()
    }

    /// The run ended as `state`. Messages still in the inbox were never read: each is reported as not
    /// delivered. Ending twice changes nothing.
    pub fn end(&self, state: SubState) {
        let e = &self.entry;
        let mut d = e.data.lock().unwrap();
        if !d.state.running() {
            return;
        }
        d.state = state;
        d.ended_ms = Some(now_ms());
        self.send(SubChange::State(e.info(&d)));
        for _ in d.inbox.drain(..) {
            let why = "that sub-agent ended before it read the message".to_string();
            self.send(SubChange::NotDelivered { id: e.id.clone(), why });
        }
        drop(d);
        if self.live() {
            self.subs.prune(&e.id);
        }
    }
}

impl Drop for SubRun {
    /// A run that stops without saying how (a panic, a dropped task) ends as killed.
    fn drop(&mut self) {
        self.end(SubState::Killed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(rx: &mut broadcast::Receiver<SubChange>) -> Vec<SubChange> {
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(c);
        }
        out
    }

    #[test]
    fn items_events_and_transcript_agree_and_a_run_ends_once() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let run = subs.begin("a#1", "explore", "look at x\nmore", false);
        run.item(SubItem::Text { text: "found it".into() });
        run.end(SubState::Ok);
        run.end(SubState::Failed);
        let evs = kinds(&mut rx);
        assert!(matches!(&evs[0], SubChange::State(i) if i.state == SubState::Attached && i.title == "look at x"));
        let from_events: Vec<SubItem> = evs
            .iter()
            .filter_map(|c| match c {
                SubChange::Item { item, .. } => Some(item.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(subs.transcript("a#1").unwrap().items, from_events);
        assert!(matches!(evs.last(), Some(SubChange::State(i)) if i.state == SubState::Ok));
        assert_eq!(evs.iter().filter(|c| matches!(c, SubChange::State(_))).count(), 2);
    }

    #[test]
    fn the_newest_200_items_stay_and_the_counter_says_how_many_went() {
        let subs = Subs::new();
        let run = subs.begin("a#1", "explore", "t", true);
        for i in 0..299 {
            run.item(SubItem::Text { text: format!("{i}") });
        }
        let t = subs.transcript("a#1").unwrap();
        assert_eq!((t.items.len(), t.dropped), (KEEP_ITEMS, 100));
        assert_eq!(t.items.last(), Some(&SubItem::Text { text: "298".into() }));
    }

    #[test]
    fn a_long_text_is_cut_to_the_byte_bound_on_a_character_boundary() {
        let subs = Subs::new();
        let run = subs.begin("a#1", "explore", "t", true);
        run.item(SubItem::Text { text: "é".repeat(512 * 1024) });
        let t = subs.transcript("a#1").unwrap();
        let Some(SubItem::Text { text }) = t.items.last() else { panic!("no text item") };
        assert!(text.len() <= KEEP_BYTES, "{}", text.len());
        assert!(text.ends_with("bytes cut]") && text.starts_with('é'));
    }

    #[test]
    fn many_texts_stay_within_the_byte_bound() {
        let subs = Subs::new();
        let run = subs.begin("a#1", "explore", "t", true);
        for _ in 0..10 {
            run.item(SubItem::Text { text: "x".repeat(100 * 1024) });
        }
        let t = subs.transcript("a#1").unwrap();
        let bytes: usize = t.items.iter().map(SubItem::size).sum();
        assert!(bytes <= KEEP_BYTES && t.dropped >= 7, "{bytes} {}", t.dropped);
    }

    #[test]
    fn only_the_newest_32_ended_runs_stay_and_running_ones_always() {
        let subs = Subs::new();
        let live = subs.begin("live", "explore", "t", true);
        for i in 0..40 {
            subs.begin(&format!("r{i}"), "explore", "t", true).end(SubState::Ok);
        }
        let ids: Vec<String> = subs.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids.len(), KEEP_RUNS + 1);
        assert!(
            ids.contains(&"live".to_string()) && ids.contains(&"r39".to_string()) && !ids.contains(&"r0".to_string())
        );
        drop(live);
    }

    #[test]
    fn a_long_run_that_ends_last_is_kept_and_the_oldest_ended_one_goes() {
        let subs = Subs::new();
        let long = subs.begin("long", "explore", "t", true);
        for i in 0..KEEP_RUNS {
            subs.begin(&format!("s{i}"), "explore", "t", true).end(SubState::Ok);
        }
        long.item(SubItem::Text { text: "the whole report".into() });
        long.end(SubState::Ok);
        let ids: Vec<String> = subs.list().into_iter().map(|i| i.id).collect();
        assert_eq!(ids.len(), KEEP_RUNS);
        assert!(ids.contains(&"long".to_string()), "{ids:?}");
        assert!(!ids.contains(&"s0".to_string()) && ids.contains(&"s1".to_string()), "{ids:?}");
        assert_eq!(subs.transcript("long").unwrap().items.len(), 2);
    }

    #[test]
    fn a_message_goes_to_a_running_run_only_and_a_dead_or_unknown_one_says_so() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let a = subs.begin("a", "explore", "t", true);
        let b = subs.begin("b", "explore", "t", true);
        assert!(subs.submit("a", "hello").is_ok());
        assert_eq!(b.take_inbox(), Vec::<String>::new());
        assert_eq!(a.take_inbox(), vec!["hello".to_string()]);
        a.end(SubState::Ok);
        assert!(subs.submit("a", "late").is_err());
        assert!(subs.submit("zzz", "who").is_err());
        assert!(!a.has_mail());
        let nd: Vec<String> = kinds(&mut rx)
            .into_iter()
            .filter_map(|c| match c {
                SubChange::NotDelivered { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(nd, vec!["a".to_string(), "zzz".to_string()]);
    }

    #[test]
    fn a_message_not_read_when_the_run_ends_is_reported_not_kept() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let a = subs.begin("a", "explore", "t", true);
        subs.submit("a", "too late").unwrap();
        a.end(SubState::Failed);
        assert!(!a.has_mail());
        let evs = kinds(&mut rx);
        assert!(evs.iter().any(|c| matches!(c, SubChange::NotDelivered { id, .. } if id == "a")));
    }

    #[test]
    fn a_run_dropped_without_an_end_is_killed_and_nobody_listening_changes_nothing() {
        let subs = Subs::new();
        // no receiver at all
        let a = subs.begin("a", "explore", "t", false);
        a.item(SubItem::Text { text: "x".into() });
        drop(a);
        let i = &subs.list()[0];
        assert_eq!(i.state, SubState::Killed);
        assert!(i.ended_ms.is_some());
        assert_eq!(subs.transcript("a").unwrap().items.len(), 2);
    }

    #[test]
    fn a_receiver_that_falls_behind_loses_events_and_the_run_is_whole() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let a = subs.begin("a", "explore", "t", true);
        for i in 0..(CHANGES + 100) {
            a.item(SubItem::Text { text: format!("{i}") });
        }
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Lagged(_))));
        assert_eq!(subs.transcript("a").unwrap().items.len(), KEEP_ITEMS);
    }

    #[test]
    fn spending_adds_up_and_going_to_the_background_is_one_event() {
        let subs = Subs::new();
        let mut rx = subs.changes();
        let a = subs.begin("a", "explore", "t", false);
        a.spent(&Usage { input: 10, output: 5, cache_read: 3, ..Default::default() }, 0.5);
        a.spent(&Usage { input: 1, output: 1, ..Default::default() }, 0.25);
        subs.to_background("a");
        subs.to_background("a");
        let i = &subs.list()[0];
        assert_eq!((i.tokens_in, i.tokens_out, i.cost_usd, i.state), (14, 6, 0.75, SubState::Background));
        let states = kinds(&mut rx).into_iter().filter(|c| matches!(c, SubChange::State(_))).count();
        assert_eq!(states, 2);
    }

    #[test]
    fn clear_forgets_every_run() {
        let subs = Subs::new();
        subs.begin("a", "explore", "t", true).end(SubState::Ok);
        subs.clear();
        assert!(subs.list().is_empty() && subs.transcript("a").is_none());
    }

    #[test]
    fn a_run_that_winds_down_after_a_clear_tells_nobody() {
        let subs = Subs::new();
        let old = subs.begin("old", "explore", "t", true);
        subs.clear();
        let mut rx = subs.changes();
        old.item(SubItem::Text { text: "late".into() });
        old.delta("late");
        old.spent(&Usage::default(), 0.5);
        old.end(SubState::Failed);
        drop(old);
        assert!(kinds(&mut rx).is_empty());
        assert!(subs.list().is_empty());
        // a run of the new session is heard
        let new = subs.begin("new", "explore", "t", true);
        new.end(SubState::Ok);
        assert!(!kinds(&mut rx).is_empty());
        assert_eq!(subs.list().len(), 1);
    }

    #[test]
    fn a_run_can_carry_its_own_title_and_its_meter_outlives_the_list() {
        let subs = Subs::new();
        let run = subs.begin_titled("a", "explore", "flow · Find · find #2\nignored", "the task\nmore", true);
        assert_eq!(subs.list()[0].title, "flow · Find · find #2");
        let meter = run.meter();
        run.spent(&Usage { input: 7, output: 3, ..Default::default() }, 0.0);
        run.end(SubState::Ok);
        // 32 runs ended after it push it out of the list
        for i in 0..KEEP_RUNS + 1 {
            subs.begin(&format!("b{i}"), "explore", "t", true).end(SubState::Ok);
        }
        assert!(subs.list().iter().all(|r| r.id != "a"));
        assert_eq!(meter.tokens(), (7, 3));
    }
}
