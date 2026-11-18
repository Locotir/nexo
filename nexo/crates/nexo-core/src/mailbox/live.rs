//! The live sessions as their heartbeats tell them, for `nexo sessions live` and dashboards. Only metadata
//! leaves a session this way: who it is, whether a turn runs, and the jobs running. Never a message.

use super::facts::LiveFacts;
use super::peers::{clean, read_live};
use super::*;
use crate::jobs::{JobInfo, JobKind};

/// Characters of a job's title kept in the heartbeat (a command line or a sub-agent's description).
const TITLE_MAX: usize = 80;
/// Jobs listed in a heartbeat at most.
const JOBS_MAX: usize = 16;

/// A job that is running, as the heartbeat lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveJob {
    pub id: u32,
    pub kind: JobKind,
    pub state: String,
    pub title: String,
}

/// A session with a fresh heartbeat.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveSession {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub pid: u32,
    /// When the session started and when it last beat (ms since the epoch).
    pub started_ms: i64,
    pub beat_ms: i64,
    /// Whether a turn is running; none when the heartbeat does not say (an older nexo wrote it).
    pub busy: Option<bool>,
    pub jobs: Vec<LiveJob>,
    /// Model, mode, step, turn start, cost, context use, goal and lane, as far as the session said them.
    pub facts: LiveFacts,
}

/// The sessions whose heartbeat under `root` is fresh at `now` (ms) and whose process exists, by name and id.
/// This is the same test the mailbox applies to its peers.
pub fn live_sessions(root: &Path, now: i64) -> Vec<LiveSession> {
    LiveScan::default().scan(root, now)
}

/// The size and modification time of a heartbeat file: while both are the same, so is its text.
type Stamp = (u64, Option<std::time::SystemTime>);

fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

/// Whether a file with this stamp is old enough that a rewrite cannot share its stamp (timestamps have a
/// granularity, so a file written a moment ago might be written again without the stamp changing).
fn settled(stamp: &Stamp) -> bool {
    stamp.1.is_some_and(|t| t.elapsed().is_ok_and(|d| d > std::time::Duration::from_secs(1)))
}

/// A repeated [`live_sessions`] that does not read again a heartbeat it has already found dead. A session that
/// ended without closing leaves its file behind, and a list asked for every second would otherwise open, read
/// and parse every such file each time. A dead heartbeat cannot come back without being written again, which
/// changes its stamp: its beat only gets older and a process id that is gone stays gone.
#[derive(Default)]
pub struct LiveScan {
    dead: HashMap<String, Stamp>,
}

impl LiveScan {
    /// As [`live_sessions`].
    pub fn scan(&mut self, root: &Path, now: i64) -> Vec<LiveSession> {
        let mut out: Vec<LiveSession> = Vec::new();
        let mut present: HashSet<String> = HashSet::new();
        if let Ok(rd) = fs::read_dir(root) {
            for e in rd.flatten() {
                let sid = e.file_name().to_string_lossy().to_string();
                let stamp = stamp_of(&root.join(&sid).join("session.json"));
                if stamp.is_some() {
                    present.insert(sid.clone());
                }
                if stamp.is_some() && self.dead.get(&sid) == stamp.as_ref() {
                    continue;
                }
                let Some(f) = read_live(root, &sid, now) else {
                    if let Some(st) = stamp.filter(settled) {
                        self.dead.insert(sid, st);
                    }
                    continue;
                };
                self.dead.remove(&sid);
                out.push(LiveSession {
                    id: f.id,
                    name: f.name,
                    cwd: f.cwd,
                    pid: f.pid,
                    started_ms: f.started,
                    beat_ms: f.beat,
                    busy: f.busy,
                    jobs: f.jobs,
                    facts: f.facts,
                });
            }
        }
        // a heartbeat that is gone needs no memory
        self.dead.retain(|sid, _| present.contains(sid));
        out.sort_by(|a, b| (a.name.as_str(), a.id.as_str()).cmp(&(b.name.as_str(), b.id.as_str())));
        out
    }

    /// How many heartbeats are known dead and skipped while they stay unchanged.
    #[cfg(test)]
    pub(super) fn skipped(&self) -> usize {
        self.dead.len()
    }
}

impl Mailbox {
    /// The host says which jobs run now (only the running ones are kept); a change shows in the session file at once.
    pub fn set_jobs(&self, jobs: &[JobInfo]) {
        let jobs: Vec<LiveJob> = jobs
            .iter()
            .filter(|j| j.status.running())
            .take(JOBS_MAX)
            .map(|j| LiveJob {
                id: j.id,
                kind: j.kind,
                state: j.status.plain().to_string(),
                title: clean(&j.label, TITLE_MAX),
            })
            .collect();
        let changed = {
            let mut s = self.lock();
            let changed = s.jobs != jobs;
            s.jobs = jobs;
            changed
        };
        if changed {
            self.write_session(self.now());
        }
    }
}
