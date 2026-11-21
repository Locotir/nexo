use super::*;

/// What starts a sub-agent: the arguments of a `task` call, or a step of a workflow.
pub(crate) struct Launch<'a> {
    /// The kind of sub-agent (a name of [`SubAgent::profiles`]).
    pub agent: &'a str,
    pub prompt: &'a str,
    /// Another provider or model, as the `task` tool takes them.
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    /// Explicit reasoning level for this run, ahead of the model default.
    pub effort: Option<&'a str>,
    /// What the job and the claims call it; the first line of the prompt when none.
    pub description: Option<&'a str>,
    /// What the registry calls it; the first line of the task when none.
    pub title: Option<&'a str>,
    /// Starts in the background: no call waits for it.
    pub background: bool,
    /// The call that waits for it may let go of it when the person writes or presses the key, and sees its
    /// progress lines. A workflow keeps its sub-agents until they end: false.
    pub detachable: bool,
    /// Omit inherited conversation and shared-memory access.
    pub start_clean: bool,
}

/// A sub-agent at work, as whoever started it sees it.
pub(crate) struct Started {
    /// Its job.
    pub id: u32,
    pub label: String,
    /// Its own id in the registry.
    pub run_id: String,
    pub rx: tokio::sync::oneshot::Receiver<Ended>,
    pub attached: Arc<AtomicBool>,
    /// Its report is also the job's report (what the ended note carries).
    pub inline: bool,
    /// What it has spent so far; stays readable when the registry lets go of the run.
    pub meter: nexo_core::subs::SubMeter,
}

/// How a sub-agent's run ended, for the call that waits for it.
pub(crate) enum Ended {
    Report(String),
    /// It failed: the error and the worktree note.
    Failed(String, String),
    /// It was stopped: what it had said, and the worktree note.
    Stopped(String, String),
}

/// Seconds an interrupted call waits for the sub-agent to finish its cleanup.
pub(super) const CLEANUP: Duration = Duration::from_secs(5);
