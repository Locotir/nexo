//! The registry of jobs, a job's handle and what one read of its output gives.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Notify};
use tokio_util::sync::CancellationToken;

use super::*;

struct Out {
    lines: VecDeque<String>,
    chars: usize,
    /// Lines dropped from the front to keep within `KEEP_CHARS`.
    dropped: usize,
    /// The absolute line the model has read up to.
    read: usize,
}

struct Entry {
    id: u32,
    kind: JobKind,
    label: String,
    /// Left running by a foreground command (`Jobs::start_held`): no review judges it by its silence.
    held: bool,
    started: Instant,
    ended: Mutex<Option<Instant>>,
    out: Mutex<Out>,
    status: Mutex<JobStatus>,
    /// The model was told it ended.
    told: Mutex<bool>,
    /// What a sub-agent reported when it ended (the end note carries it).
    report: Mutex<Option<String>>,
    cancel: CancellationToken,
    done: Notify,
}

impl Entry {
    pub(super) fn info(&self) -> JobInfo {
        let end = self.ended.lock().unwrap().unwrap_or_else(Instant::now);
        let out = self.out.lock().unwrap();
        JobInfo {
            id: self.id,
            kind: self.kind,
            label: self.label.clone(),
            status: self.status.lock().unwrap().clone(),
            elapsed_ms: end.duration_since(self.started).as_millis() as u64,
            lines: out.dropped + out.lines.len(),
            held: self.held,
        }
    }
}

struct Inner {
    next: AtomicU32,
    jobs: Mutex<BTreeMap<u32, Arc<Entry>>>,
    root: CancellationToken,
    changes: broadcast::Sender<JobChange>,
    subs: crate::subs::Subs,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.root.cancel();
    }
}

/// The jobs of a session. Cloning shares them.
#[derive(Clone)]
pub struct Jobs(Arc<Inner>);

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.list()).finish()
    }
}

/// The side a job's runner writes to.
pub struct JobHandle {
    entry: Arc<Entry>,
    changes: broadcast::Sender<JobChange>,
}

impl JobHandle {
    pub fn id(&self) -> u32 {
        self.entry.id
    }

    /// Cancelled when the job is killed or the session ends.
    pub fn cancel(&self) -> CancellationToken {
        self.entry.cancel.clone()
    }

    pub fn line(&self, line: &str) {
        let n = line.chars().count();
        let line = if n > LINE_CHARS {
            let head: String = line.chars().take(LINE_CHARS).collect();
            format!("{head} … [{} characters of this line cut]", n - LINE_CHARS)
        } else {
            line.to_string()
        };
        let mut o = self.entry.out.lock().unwrap();
        o.chars += line.chars().count() + 1;
        o.lines.push_back(line);
        while o.chars > KEEP_CHARS && o.lines.len() > 1 {
            let l = o.lines.pop_front().unwrap();
            o.chars -= l.chars().count() + 1;
            o.dropped += 1;
        }
    }

    /// Several lines at once (a report).
    pub fn text(&self, text: &str) {
        for l in text.lines() {
            self.line(l);
        }
    }

    /// What a sub-agent reported: the ended note carries it, so the model needs no `jobs output` call.
    pub fn set_report(&self, text: &str) {
        *self.entry.report.lock().unwrap() = Some(text.to_string());
    }

    /// The job ended by itself. A job killed or stopped stays so.
    pub fn finish(self, ok: bool, how: impl Into<String>) {
        // the check and the write share one lock, so a kill cannot land between them
        {
            let mut status = self.entry.status.lock().unwrap();
            if status.running() {
                *status = JobStatus::Done { ok, how: how.into() };
            }
        }
        self.end();
    }

    pub(super) fn end(&self) {
        let mut e = self.entry.ended.lock().unwrap();
        if e.is_none() {
            *e = Some(Instant::now());
            drop(e);
            self.entry.done.notify_waiters();
            let _ = self.changes.send(JobChange(self.entry.info()));
        }
    }
}

impl Drop for JobHandle {
    /// A runner that stops without saying how (a panic, a dropped task) ends the job as killed.
    fn drop(&mut self) {
        {
            let mut status = self.entry.status.lock().unwrap();
            if status.running() {
                *status = JobStatus::Killed;
            }
        }
        self.end();
    }
}

/// What a read of a job gives.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRead {
    pub info: JobInfo,
    /// The output not read before: whole lines up to `READ_CHARS`, and when more remain, a note of how many.
    pub new: String,
}

impl Jobs {
    pub fn new() -> Jobs {
        let (changes, _) = broadcast::channel(64);
        Jobs(Arc::new(Inner {
            next: AtomicU32::new(1),
            jobs: Mutex::new(BTreeMap::new()),
            root: CancellationToken::new(),
            changes,
            subs: crate::subs::Subs::new(),
        }))
    }

    /// A new job; the caller runs it (in a spawned task) and writes to the handle.
    pub fn start(&self, kind: JobKind, label: impl Into<String>) -> JobHandle {
        self.start_as(kind, label.into(), false)
    }

    /// A new job for what a foreground command left running (a server, a watcher): it is silent by nature, so
    /// the periodic review, which judges a job by its output, does not look at it (`held` tells it so).
    pub fn start_held(&self, kind: JobKind, label: impl Into<String>) -> JobHandle {
        self.start_as(kind, label.into(), true)
    }

    /// Whether job `id` was made by `start_held`.
    pub fn held(&self, id: u32) -> bool {
        self.entry(id).is_some_and(|e| e.held)
    }

    fn start_as(&self, kind: JobKind, label: String, held: bool) -> JobHandle {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let entry = Arc::new(Entry {
            id,
            kind,
            label,
            held,
            started: Instant::now(),
            ended: Mutex::new(None),
            out: Mutex::new(Out { lines: VecDeque::new(), chars: 0, dropped: 0, read: 0 }),
            status: Mutex::new(JobStatus::Running),
            told: Mutex::new(false),
            report: Mutex::new(None),
            cancel: self.0.root.child_token(),
            done: Notify::new(),
        });
        self.0.jobs.lock().unwrap().insert(id, entry.clone());
        let _ = self.0.changes.send(JobChange(entry.info()));
        JobHandle { entry, changes: self.0.changes.clone() }
    }

    pub fn list(&self) -> Vec<JobInfo> {
        self.0.jobs.lock().unwrap().values().map(|e| e.info()).collect()
    }

    pub fn get(&self, id: u32) -> Option<JobInfo> {
        self.entry(id).map(|e| e.info())
    }

    pub fn running(&self) -> usize {
        self.list().iter().filter(|j| j.status.running()).count()
    }

    /// The running jobs whose end is awaited: all but those a foreground command left running (`start_held`).
    pub fn awaited(&self) -> usize {
        self.list().iter().filter(|j| j.status.running() && !j.held).count()
    }

    fn entry(&self, id: u32) -> Option<Arc<Entry>> {
        self.0.jobs.lock().unwrap().get(&id).cloned()
    }

    /// The output of job `id` the model has not read yet, after waiting up to
    /// `wait` for it to end (returns at once when it already has, or when
    /// `cancel` fires). Reading it marks it read; an ended job read whole is
    /// one the model needs no notice of.
    pub async fn read(&self, id: u32, wait: Duration, cancel: &CancellationToken) -> Option<JobRead> {
        let e = self.entry(id)?;
        if !wait.is_zero() {
            let done = e.done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if e.ended.lock().unwrap().is_none() {
                tokio::select! {
                    _ = done => {}
                    _ = tokio::time::sleep(wait) => {}
                    _ = cancel.cancelled() => {}
                }
            }
        }
        let info = e.info();
        let (new, unread) = {
            let mut o = e.out.lock().unwrap();
            let from = o.read.max(o.dropped);
            let skipped = from - o.read;
            let start = from - o.dropped;
            let mut text = String::new();
            if skipped > 0 {
                text.push_str(&format!("[{skipped} earlier lines no longer kept]\n"));
            }
            // whole lines while they fit one read (always at least one: a line is at most `LINE_CHARS`), so
            // nothing is skipped: the next read goes on where this one stopped
            let mut used = text.chars().count();
            let mut taken = 0;
            for l in o.lines.iter().skip(start) {
                let n = l.chars().count() + 1;
                if taken > 0 && used + n > READ_CHARS {
                    break;
                }
                text.push_str(l);
                text.push('\n');
                used += n;
                taken += 1;
            }
            let left = o.lines.len() - start - taken;
            if left > 0 {
                let chars: usize = o.lines.iter().skip(start + taken).map(|l| l.chars().count() + 1).sum();
                let last = o
                    .lines
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| {
                        let l = l.trim();
                        match l.char_indices().nth(300) {
                            Some((i, _)) => format!("{} …", &l[..i]),
                            None => l.to_string(),
                        }
                    })
                    .unwrap_or_default();
                text.push_str(&format!(
                    "[{left} more lines ({chars} chars) unread, the last of them: {last}; `jobs` with action \
                     `output` and this id shows the next {READ_CHARS} chars]"
                ));
            }
            o.read = from + taken;
            (text, left > 0)
        };
        // told only when the model has all of the output: a long job read in part stays to be noted
        if !info.status.running() && !unread {
            *e.told.lock().unwrap() = true;
        }
        Some(JobRead { info, new })
    }

    /// The newest non-empty line of job `id`'s output, without marking anything read.
    pub fn last_line(&self, id: u32) -> Option<String> {
        let e = self.entry(id)?;
        let o = e.out.lock().unwrap();
        o.lines.iter().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string())
    }

    /// The end of the output of every job, without marking anything read.
    pub fn tails(&self) -> Vec<JobTail> {
        let jobs = self.0.jobs.lock().unwrap();
        jobs.values()
            .map(|e| {
                let ended_ago_ms = e.ended.lock().unwrap().map(|t| t.elapsed().as_millis() as u64);
                let o = e.out.lock().unwrap();
                let from = o.lines.len().saturating_sub(TAIL_LINES);
                let text = o.lines.iter().skip(from).map(String::as_str).collect::<Vec<_>>().join("\n");
                JobTail { id: e.id, ended_ago_ms, text }
            })
            .collect()
    }

    /// Stops job `id`. False when there is no such job or it had ended.
    pub fn kill(&self, id: u32) -> bool {
        let Some(e) = self.entry(id) else { return false };
        let mut s = e.status.lock().unwrap();
        if !s.running() {
            return false;
        }
        *s = JobStatus::Killed;
        drop(s);
        // the model asked for it: no notice
        *e.told.lock().unwrap() = true;
        e.cancel.cancel();
        true
    }

    /// Ends job `id` for a reason the model is told when it learns the job ended (unlike `kill`, which
    /// the model asked for). False when there is no such job or it had ended.
    pub fn stop(&self, id: u32, why: &str) -> bool {
        let Some(e) = self.entry(id) else { return false };
        let mut s = e.status.lock().unwrap();
        if !s.running() {
            return false;
        }
        *s = JobStatus::Done { ok: false, how: format!("stopped: {why}") };
        drop(s);
        e.cancel.cancel();
        true
    }

    /// Stops every job still running.
    pub fn kill_all(&self) -> usize {
        let ids: Vec<u32> = self.list().iter().filter(|j| j.status.running()).map(|j| j.id).collect();
        ids.into_iter().filter(|id| self.kill(*id)).count()
    }

    /// Jobs that ended and the model has not been told of (they stay untold).
    pub fn untold(&self) -> Vec<JobInfo> {
        let jobs = self.0.jobs.lock().unwrap();
        jobs.values()
            .filter(|e| e.ended.lock().unwrap().is_some() && !*e.told.lock().unwrap())
            .map(|e| e.info())
            .collect()
    }

    /// Jobs that ended since the model was last told, now marked told.
    pub fn take_ended(&self) -> Vec<JobInfo> {
        let jobs = self.0.jobs.lock().unwrap();
        jobs.values()
            .filter(|e| e.ended.lock().unwrap().is_some())
            .filter(|e| !std::mem::replace(&mut *e.told.lock().unwrap(), true))
            .map(|e| e.info())
            .collect()
    }

    /// The model got the outcome of job `id` some other way (a tool result): no ended note for it.
    pub fn tell(&self, id: u32) {
        if let Some(e) = self.entry(id) {
            *e.told.lock().unwrap() = true;
        }
    }

    /// What the sub-agent of job `id` reported, if it did.
    pub fn report_of(&self, id: u32) -> Option<String> {
        self.entry(id).and_then(|e| e.report.lock().unwrap().clone())
    }

    /// The note for jobs that ended (see [`ended_note`]), with the report of each sub-agent inline.
    pub fn note_ended(&self, ended: &[JobInfo]) -> String {
        self.note_ended_within(ended, None)
    }

    /// [`Jobs::note_ended`] with each report held to its part of `room`, the room one result may take of the model's window.
    pub fn note_ended_within(&self, ended: &[JobInfo], room: Option<crate::tool::ResultRoom>) -> String {
        let reports: Vec<Option<String>> =
            ended.iter().map(|j| if j.kind == JobKind::Agent { self.report_of(j.id) } else { None }).collect();
        note(ended, &reports, room)
    }

    /// Every start and end, as they happen.
    pub fn changes(&self) -> broadcast::Receiver<JobChange> {
        self.0.changes.subscribe()
    }

    /// The conversations of the session's sub-agent runs.
    pub fn subs(&self) -> crate::subs::Subs {
        self.0.subs.clone()
    }
}
