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
    let mut out: Vec<LiveSession> = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let sid = e.file_name().to_string_lossy().to_string();
            let Some(f) = read_live(root, &sid, now) else { continue };
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
    out.sort_by(|a, b| (a.name.as_str(), a.id.as_str()).cmp(&(b.name.as_str(), b.id.as_str())));
    out
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
