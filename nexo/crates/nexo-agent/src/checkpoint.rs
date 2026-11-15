//! Checkpoints: what a request does at `agent.max_turns` tool rounds instead of stopping.
//!
//! Code gathers the facts (cost, calls, files written, the todo list, the latest build or test line, errors
//! that repeat) and renders them as one short message, appended to the end of the conversation so the prompt
//! cache stays. The model answers on its first line with `CHECKPOINT CONTINUE: <reason>` or
//! `CHECKPOINT STOP: <reason>`. Whether a request has made progress is decided here, from the facts, never
//! by the model. The functions are pure so they are tested without the loop.

/// Checkpoints one request may take when no spend limit is in force and `max_checkpoints` is not set.
pub const DEFAULT_MAX: u32 = 5;
/// Characters of a message the model gets, at most.
const MAX_CHARS: usize = 1500;
/// Characters of one detail (the request, an error line, a build line).
const DETAIL: usize = 160;

mod marker;
mod report;
mod tracker;

pub use marker::{calls_in, parse_decision, progress, todo_of, Decision, Mark, MarkerFilter, Todo};
pub use report::{elapsed, render, stats, tokens, Report};
pub use tracker::{build_line, Tracker};

#[cfg(test)]
mod tests;
