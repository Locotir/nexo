//! Sub-agents attached to a run: why they go to the background and the signals for it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

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
