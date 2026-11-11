//! Work that goes on while the agent does something else: a shell command
//! started with `bash` and `background: true`, or a sub-agent sent off with
//! `task` and `background: true`. Each is a job of the session, with a number,
//! its output (bounded) and how it ended. The model reads it with the `jobs`
//! tool; when one ends, the agent tells the model at its next call, or wakes
//! it when it was waiting for the person. Dropping the last handle of the
//! registry (the session ends) stops every job still running.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Notify};
use tokio_util::sync::CancellationToken;

/// Characters of output a job keeps; older lines go first.
pub const KEEP_CHARS: usize = 200_000;
/// Characters of new output one read gives (head and tail).
pub const READ_CHARS: usize = 30_000;
/// Characters kept of one line; the rest of a longer one is dropped.
pub const LINE_CHARS: usize = 16_384;
/// Lines of output the interface gets of each job.
pub const TAIL_LINES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Shell,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    /// It ended by itself: `ok`, and how (`exit 0`, `exit 2`, `report`).
    Done {
        ok: bool,
        how: String,
    },
    /// Stopped by the model, the person, or the end of the session.
    Killed,
}

impl JobStatus {
    pub fn running(&self) -> bool {
        matches!(self, JobStatus::Running)
    }

    /// How the person reads the state: no exit codes, no internals.
    pub fn plain(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done { ok: true, .. } => "finished",
            JobStatus::Done { .. } => "failed",
            JobStatus::Killed => "stopped",
        }
    }

    pub fn word(&self) -> String {
        match self {
            JobStatus::Running => "running".into(),
            JobStatus::Done { how, .. } => format!("done ({how})"),
            JobStatus::Killed => "killed".into(),
        }
    }
}

/// What the model, the agent and the interface see of a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: u32,
    pub kind: JobKind,
    /// The command, or the sub-agent's description.
    pub label: String,
    pub status: JobStatus,
    pub elapsed_ms: u64,
    /// Lines of output so far (including those no longer kept).
    pub lines: usize,
}

/// The end of a job's output, for the interface to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobTail {
    pub id: u32,
    /// How long ago it ended; None while it runs.
    pub ended_ago_ms: Option<u64>,
    /// The last lines kept, at most `TAIL_LINES`.
    pub text: String,
}

/// A job began or ended.
#[derive(Debug, Clone)]
pub struct JobChange(pub JobInfo);

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
    fn info(&self) -> JobInfo {
        let end = self.ended.lock().unwrap().unwrap_or_else(Instant::now);
        let out = self.out.lock().unwrap();
        JobInfo {
            id: self.id,
            kind: self.kind,
            label: self.label.clone(),
            status: self.status.lock().unwrap().clone(),
            elapsed_ms: end.duration_since(self.started).as_millis() as u64,
            lines: out.dropped + out.lines.len(),
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
        let killed = { !self.entry.status.lock().unwrap().running() };
        if !killed {
            *self.entry.status.lock().unwrap() = JobStatus::Done { ok, how: how.into() };
        }
        self.end();
    }

    fn end(&self) {
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
        if self.entry.status.lock().unwrap().running() {
            *self.entry.status.lock().unwrap() = JobStatus::Killed;
        }
        self.end();
    }
}

/// What a read of a job gives.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRead {
    pub info: JobInfo,
    /// The output not read before (head and tail when long).
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
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let entry = Arc::new(Entry {
            id,
            kind,
            label: label.into(),
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
        let new = {
            let mut o = e.out.lock().unwrap();
            let from = o.read.max(o.dropped);
            let skipped = from - o.read;
            let lines: Vec<&String> = o.lines.iter().skip(from - o.dropped).collect();
            let mut text = String::new();
            if skipped > 0 {
                text.push_str(&format!("[{skipped} earlier lines no longer kept]\n"));
            }
            for l in lines {
                text.push_str(l);
                text.push('\n');
            }
            o.read = o.dropped + o.lines.len();
            cap(&text, READ_CHARS)
        };
        if !info.status.running() {
            *e.told.lock().unwrap() = true;
        }
        Some(JobRead { info, new })
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
        let reports: Vec<Option<String>> =
            ended.iter().map(|j| if j.kind == JobKind::Agent { self.report_of(j.id) } else { None }).collect();
        note(ended, &reports)
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

/// Why attached sub-agents go to the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachWhy {
    /// The person wrote while they ran.
    Message,
    /// The person pressed the key that sends them to the background.
    Key,
}

impl DetachWhy {
    /// What the `task` call answers when it lets go of the sub-agent.
    pub fn text(self, id: u32) -> String {
        let why = match self {
            DetachWhy::Message => "the person wrote while it ran",
            DetachWhy::Key => "the person sent it to the background",
        };
        format!("Moved to the background as job {id}: {why}. You will be told when it ends, with its report.")
    }
}

#[derive(Default)]
struct DetachInner {
    fired: Mutex<Option<DetachWhy>>,
    notify: Notify,
    /// The sub-agent runs that are alive, by their own id: true once a run is in the background, false
    /// while a call waits for it.
    runs: Mutex<HashMap<String, bool>>,
}

/// The signal that sends the sub-agents a round waits for to the background. It stays set until the round
/// ends, so a sub-agent that starts after the person wrote lets go at once. Cloning shares it.
#[derive(Clone, Default)]
pub struct Detach(Arc<DetachInner>);

impl std::fmt::Debug for Detach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Detach({:?})", self.why())
    }
}

impl Detach {
    pub fn fire(&self, why: DetachWhy) {
        {
            let mut f = self.0.fired.lock().unwrap();
            // the key says more than a message
            if f.is_none() || why == DetachWhy::Key {
                *f = Some(why);
            }
        }
        self.0.notify.notify_waiters();
    }

    /// The round ended: sub-agents that start from now on stay attached.
    pub fn clear(&self) {
        *self.0.fired.lock().unwrap() = None;
    }

    pub fn why(&self) -> Option<DetachWhy> {
        *self.0.fired.lock().unwrap()
    }

    /// Returns when the signal is (or already was) set.
    pub async fn wait(&self) -> DetachWhy {
        loop {
            let n = self.0.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if let Some(w) = self.why() {
                return w;
            }
            n.await;
        }
    }

    /// The sub-agent run `run` (an id of its own, never a provider's call id: those can repeat) starts,
    /// in the background (`background`) or waited for by its call.
    pub fn begin(&self, run: &str, background: bool) {
        self.0.runs.lock().unwrap().insert(run.to_string(), background);
    }

    /// The call lets go of run `run`. False when the run already ended: its call then takes the report.
    pub fn to_background(&self, run: &str) -> bool {
        match self.0.runs.lock().unwrap().get_mut(run) {
            Some(bg) => {
                *bg = true;
                true
            }
            None => false,
        }
    }

    /// The run ended; nothing of it stays behind.
    pub fn end(&self, run: &str) {
        self.0.runs.lock().unwrap().remove(run);
    }

    /// How many runs a call waits for now.
    pub fn attached(&self) -> usize {
        self.0.runs.lock().unwrap().values().filter(|bg| !**bg).count()
    }

    /// Whether `ask_id` (`<run>.<call>`) is a question of a sub-agent in the background.
    pub fn is_gone(&self, ask_id: &str) -> bool {
        self.0
            .runs
            .lock()
            .unwrap()
            .iter()
            .any(|(run, bg)| *bg && ask_id.strip_prefix(run.as_str()).is_some_and(|r| r.starts_with('.')))
    }
}

/// `text` within `max` characters: the head and the tail, the middle said to be cut.
fn cap(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text.chars().skip(n - half).collect();
    format!("{head}\n… [{} characters omitted] …\n{tail}", n - 2 * half)
}

/// The note the model gets when jobs have ended.
pub fn ended_note(jobs: &[JobInfo]) -> String {
    note(jobs, &vec![None; jobs.len()])
}

/// Characters of a sub-agent's report the ended note carries (as many as one `jobs` read gives).
pub const REPORT_NOTE_CHARS: usize = READ_CHARS;

fn note(jobs: &[JobInfo], reports: &[Option<String>]) -> String {
    let lines: Vec<String> = jobs
        .iter()
        .map(|j| {
            let what = match j.kind {
                JobKind::Shell => format!("`{}`", one_line(&j.label)),
                JobKind::Agent => format!("sub-agent \"{}\"", one_line(&j.label)),
            };
            format!(
                "- job {} {what} {} after {}, {} lines of output",
                j.id,
                j.status.word(),
                duration(j.elapsed_ms),
                j.lines
            )
        })
        .collect();
    let mut out = format!("[background jobs ended]\n{}", lines.join("\n"));
    for (j, r) in jobs.iter().zip(reports) {
        if let Some(r) = r {
            out.push_str(&format!("\n\n--- report of job {} ---\n{}", j.id, cap(r, REPORT_NOTE_CHARS)));
        }
    }
    // a job whose report is not here is read with the tool
    if reports.iter().any(Option::is_none) {
        out.push_str("\nRead the output with the `jobs` tool (action \"output\", the job's id).");
    }
    out
}

fn one_line(s: &str) -> String {
    let first = s.lines().next().unwrap_or("");
    let short: String = first.chars().take(80).collect();
    if short.len() < s.len() {
        format!("{short}…")
    } else {
        short
    }
}

/// `850 ms`, `12 s`, `3 min 5 s`.
pub fn duration(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{} s", ms / 1000),
        _ => format!("{} min {} s", ms / 60_000, ms % 60_000 / 1000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tails_show_the_end_of_each_output_without_marking_it_read() {
        let jobs = Jobs::new();
        let h = jobs.start(JobKind::Shell, "make");
        for i in 0..(TAIL_LINES + 5) {
            h.line(&format!("l{i}"));
        }
        let t = jobs.tails();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].ended_ago_ms, None);
        assert_eq!(t[0].text.lines().count(), TAIL_LINES);
        assert!(t[0].text.ends_with(&format!("l{}", TAIL_LINES + 4)));
        h.finish(true, "exit 0");
        assert!(jobs.tails()[0].ended_ago_ms.is_some());
        assert_eq!(jobs.untold().len(), 1, "a look is not a read");
    }

    #[tokio::test]
    async fn a_job_is_read_once_waited_for_and_told_once() {
        let jobs = Jobs::new();
        let mut changes = jobs.changes();
        let h = jobs.start(JobKind::Shell, "make");
        assert_eq!(changes.recv().await.unwrap().0.status, JobStatus::Running);
        h.line("one");
        h.line("two");
        let c = CancellationToken::new();
        let r = jobs.read(1, Duration::ZERO, &c).await.unwrap();
        assert_eq!(r.new, "one\ntwo\n");
        assert!(r.info.status.running());
        assert_eq!(jobs.read(1, Duration::ZERO, &c).await.unwrap().new, "", "read once");
        assert!(jobs.take_ended().is_empty());
        let t = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            h.line("three");
            h.finish(true, "exit 0");
        });
        let t0 = Instant::now();
        let r = jobs.read(1, Duration::from_secs(10), &c).await.unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5), "returns when the job ends");
        assert_eq!(r.new, "three\n");
        assert_eq!(r.info.status, JobStatus::Done { ok: true, how: "exit 0".into() });
        t.await.unwrap();
        assert!(jobs.take_ended().is_empty(), "the read told the model");
        let h2 = jobs.start(JobKind::Agent, "find the parser");
        h2.finish(false, "error");
        let ended = jobs.take_ended();
        assert_eq!(ended.len(), 1);
        assert!(ended_note(&ended).contains("job 2 sub-agent \"find the parser\" done (error)"));
        assert!(jobs.take_ended().is_empty(), "told once");
        assert!(jobs.read(9, Duration::ZERO, &c).await.is_none());
    }

    #[tokio::test]
    async fn killing_cancels_and_the_end_of_the_session_stops_everything() {
        let jobs = Jobs::new();
        let h = jobs.start(JobKind::Shell, "sleep");
        let cancel = h.cancel();
        assert!(jobs.kill(1));
        assert!(cancel.is_cancelled());
        h.finish(true, "exit 0");
        assert_eq!(jobs.get(1).unwrap().status, JobStatus::Killed, "a killed job stays killed");
        assert!(!jobs.kill(1));
        assert!(jobs.take_ended().is_empty(), "the model killed it: no notice");

        let h = jobs.start(JobKind::Shell, "server");
        let cancel = h.cancel();
        drop(jobs);
        assert!(cancel.is_cancelled(), "the session ended");
        drop(h);
    }

    #[test]
    fn long_output_keeps_the_end() {
        let jobs = Jobs::new();
        let h = jobs.start(JobKind::Shell, "yes");
        for i in 0..50_000 {
            h.line(&format!("line {i}"));
        }
        let info = jobs.get(1).unwrap();
        assert_eq!(info.lines, 50_000);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(jobs.read(1, Duration::ZERO, &CancellationToken::new())).unwrap();
        assert!(r.new.starts_with('['), "{}", &r.new[..60]);
        assert!(r.new.trim_end().ends_with("line 49999"));
        assert!(r.new.chars().count() < READ_CHARS + 200);
        drop(h);
        assert_eq!(jobs.get(1).unwrap().status, JobStatus::Killed, "a dropped runner ends the job");
        assert_eq!(duration(65_000), "1 min 5 s");
    }

    #[test]
    fn one_huge_line_is_kept_bounded() {
        let jobs = Jobs::new();
        let h = jobs.start(JobKind::Shell, "yes | tr -d '\\n'");
        h.line(&"y".repeat(LINE_CHARS * 10));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = rt.block_on(jobs.read(1, Duration::ZERO, &CancellationToken::new())).unwrap();
        assert!(r.new.starts_with(&"y".repeat(100)));
        assert!(r.new.trim_end().ends_with(&format!("[{} characters of this line cut]", LINE_CHARS * 9)));
        assert!(r.new.chars().count() < LINE_CHARS + 100);
        drop(h);
    }

    #[test]
    fn a_run_that_ended_before_the_call_let_go_leaves_nothing_marked() {
        let d = Detach::default();
        d.begin("t1#1", false);
        assert_eq!(d.attached(), 1);
        // it ends in the same moment the person writes: the call must take the report, not the "moved" text
        d.end("t1#1");
        assert!(!d.to_background("t1#1"), "an ended run cannot be let go of");
        assert!(!d.is_gone("t1#1.s1"));
        assert_eq!(d.attached(), 0);
    }

    #[test]
    fn a_question_is_in_the_background_only_for_its_own_run() {
        let d = Detach::default();
        // two runs of calls that share a provider id
        d.begin("call_0#1", true);
        d.begin("call_0#2", false);
        assert!(d.is_gone("call_0#1.s1"));
        assert!(!d.is_gone("call_0#2.s1"), "the run a call still waits for is not in the background");
        assert!(!d.is_gone("call_0#12.s1"), "a longer id is another run");
        assert_eq!(d.attached(), 1);
        assert!(d.to_background("call_0#2"));
        assert!(d.is_gone("call_0#2.s1"));
        assert_eq!(d.attached(), 0);
        d.end("call_0#1");
        assert!(!d.is_gone("call_0#1.s1"));
        assert!(d.is_gone("call_0#2.s1"));
    }
}
