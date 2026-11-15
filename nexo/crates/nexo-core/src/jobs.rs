//! Work that goes on while the agent does something else: a shell command
//! started with `bash` and `background: true`, or a sub-agent sent off with
//! `task` and `background: true`. Each is a job of the session, with a number,
//! its output (bounded) and how it ended. The model reads it with the `jobs`
//! tool; when one ends, the agent tells the model at its next call, or wakes
//! it when it was waiting for the person. Dropping the last handle of the
//! registry (the session ends) stops every job still running.

#[cfg(test)]
use std::time::{Duration, Instant};
#[cfg(test)]
use tokio_util::sync::CancellationToken;

/// Characters of output a job keeps; older lines go first.
pub const KEEP_CHARS: usize = 200_000;
/// Characters of new output one read gives (head and tail).
pub const READ_CHARS: usize = 30_000;
/// Characters kept of one line; the rest of a longer one is dropped.
pub const LINE_CHARS: usize = 16_384;
/// Lines of output the interface gets of each job.
pub const TAIL_LINES: usize = 200;

mod detach;
mod registry;
mod report;
mod types;

pub use detach::*;
pub use registry::*;
pub use report::*;
pub use types::*;

#[cfg(test)]
mod tests;
