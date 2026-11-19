//! What a job is and how it is described: its kind, status and the two views of it.

use serde::{Deserialize, Serialize};

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

    /// How the ended note words the state: `done (how)` for a job that went well, `failed (how)` for one that
    /// did not, so a reader of the note (the interface draws its mark from it) need not guess from `how`.
    pub fn note_word(&self) -> String {
        match self {
            JobStatus::Done { ok: false, how } if how == "failed" => "failed".into(),
            JobStatus::Done { ok: false, how } => format!("failed ({how})"),
            other => other.word(),
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
    /// Left running by a foreground command (a server, a watcher; `Jobs::start_held`): silent by nature, and it
    /// may never end, so neither the review nor a run that waits for its jobs waits on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
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
