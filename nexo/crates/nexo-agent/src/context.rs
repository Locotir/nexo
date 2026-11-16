//! What the next model call sees: everything (`Plain`), everything with a
//! summary when it grows past a size (`Compact`), or what the context curator
//! built into nexo keeps (`BuiltIn`).

use async_trait::async_trait;

use nexo_core::message::Message;

#[cfg(test)]
mod tests;

/// A job that encodes a policy's state (`ContextPolicy::save`): the bytes of the file next to the session.
pub type SaveJob = Box<dyn FnOnce() -> Vec<u8> + Send>;

/// What to send.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub messages: Vec<Message>,
    pub tokens: u64,
    /// Something the person should hear (a fallback, a restart).
    pub note: Option<String>,
    /// What the curator holds and what of it the call takes (only the curator).
    pub memory: Option<Memory>,
}

/// The curator's memory: a cell per message of the history, in zones (the
/// anchor and the conversation's topics), as `nexo_curator::Reply::memory` gives it.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Memory {
    pub zones: Vec<nexo_core::ContextZone>,
    pub cells: Vec<MemoryCell>,
    /// What the call carries beyond the conversation and what it holds back (set by the policy, not read
    /// from the curator's memory).
    #[serde(skip)]
    pub curated: Vec<nexo_core::CuratedItem>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct MemoryCell {
    pub zone: u32,
    pub tokens: u64,
    /// `whole`, `cut`, `recalled` or `out`.
    pub state: String,
}

#[async_trait]
pub trait ContextPolicy: Send {
    fn name(&self) -> &str;
    async fn view(&mut self, system_tokens: u64, history: &[Message]) -> anyhow::Result<View>;
    /// The version of the history the next `view` gets (see `History::version`): the same version
    /// as the last view's, with no fewer messages, says only appends happened. Optional; without it
    /// the policy compares the whole history.
    fn history_version(&mut self, _version: u64) {}
    /// The loop replaced the history with a summary of it (compaction).
    fn compacted(&mut self) {
        self.reset()
    }
    /// The history was replaced (compaction, a new session).
    fn reset(&mut self);
    /// Tokens of history above which the loop should compact (the `compact` policy).
    fn compact_at(&self) -> Option<u64> {
        None
    }
    /// Fixed instructions this policy adds at the end of the system prompt
    /// (the same on every request, so they stay in the cache). `tools`: the
    /// names of the tools the model is offered, which decide how it is asked to
    /// keep its notes.
    fn directives(&self, _tools: &[&str]) -> Option<&'static str> {
        None
    }
    /// Whether the model is offered the `recall` tool (a message the view left out, back as it was).
    fn recall(&self) -> bool {
        false
    }
    /// The tools the model is offered (their names), before each view.
    fn tools(&mut self, _names: &[String]) {}
    /// The tokens the model takes in one call, less its reply, before each view (the curator keeps
    /// the view under it).
    fn window(&mut self, _tokens: u64) {}
    /// The model's prices per million input tokens, uncached and read from the cache, before each view
    /// (the curator weighs what a cut saves against what it costs the cache at their ratio).
    fn prices(&mut self, _input: f64, _cache_read: f64) {}
    /// What the provider said of the call made with the last view: the input tokens it read from its
    /// cache, and all of them, from which the curator learns how long the cache keeps a view.
    fn usage(&mut self, _read: u64, _input: u64) {}
    /// The model calls left, the next one included, when the loop knows them (a sub-agent's rounds left),
    /// before each view; `None` when it does not (the curator then estimates them, as it always has).
    fn calls_left(&mut self, _calls: Option<u64>) {}
    /// Whether the view keeps under the tokens `window` gives it, so a window learned smaller needs a
    /// new view rather than a compaction.
    fn fits_window(&self) -> bool {
        false
    }
    /// What the last view changed: messages the curator brought back and ones it took out or cut.
    fn activity(&self) -> Option<(usize, usize)> {
        None
    }
    /// Why this policy sends what it sends, when that is not what was asked for
    /// (`auto` or `curator` with settings the curator cannot run, an unknown policy).
    fn detail(&self) -> Option<String> {
        None
    }
    /// What of the settings asked for this policy does not run (it runs with the rest).
    fn caveat(&self) -> Option<String> {
        None
    }
    async fn shutdown(&mut self) {}
    /// The policy's state, saved next to the session so that a resume starts from it instead of rebuilding it
    /// from the whole history (`restore`): a job that encodes it, to run off the agent's lock, or `None` when
    /// the policy keeps no such state or nothing was added to it since the last time.
    fn save(&mut self) -> Option<SaveJob> {
        None
    }
    /// What `save` wrote for this session, before the first view of a resumed one: that view starts from it
    /// when it still fits the history, and rebuilds the state as before when it does not.
    fn restore(&mut self, _saved: Vec<u8>) {}
}

mod describe;
mod directives;
mod policies;
pub mod saved;

pub use describe::{describe, describe_since, saving};
pub use directives::{CURATOR_DIRECTIVES, CURATOR_DIRECTIVES_SAID};
pub use policies::{from_config, working_set_spec, BuiltIn, Compact, Plain};
