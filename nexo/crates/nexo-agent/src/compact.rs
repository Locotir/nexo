//! Making the history shorter: a summary of the older part replaces it.
//!
//! What replaces it, in one message from the person: a header, every message the person wrote in the
//! summarised part word for word (`[request]` parts, carried over from an earlier summary too), the
//! files the tools changed and read (`[files]`), and the model's summary of the work. The task and
//! the files are kept by the harness, not left to the summary, so a compaction never loses them.

/// The system prompt of the summary request (a recorder can tell that request apart by it).
pub const SUMMARY_SYSTEM: &str =
    "You write the summary of a coding session so that the work can continue without the transcript. \
Keep: the goal; what was done (files changed and how, commands run and their results); what was learned (errors seen, \
decisions and why); what remains. Keep exact names: paths, functions, identifiers, values, commands. Short sections, \
plain text, no preamble.";

/// Characters of a tool result kept in the transcript given to the summariser.
const RESULT_CHARS: usize = 2_000;
/// Characters of the whole transcript given to the summariser, at most (fewer when the model's window is
/// smaller, see [`transcript_chars`]).
const TRANSCRIPT_CHARS: usize = 160_000;
/// Characters a token is taken to be when the transcript is fitted to the window: fewer than the
/// usual 4, since code, paths and numbers take more tokens.
const WINDOW_CHARS_PER_TOKEN: u64 = 3;
/// Characters of one message of the person kept word for word (longer ones keep their head and tail);
/// the first one, the task statement, keeps up to [`FIRST_REQUEST_CHARS`].
const REQUEST_CHARS: usize = 8_000;
/// Characters of the person's first message (the task statement) kept word for word.
const FIRST_REQUEST_CHARS: usize = 48_000;
/// Tokens all the kept messages of the person take together, at most (fewer with a small window): past
/// it the oldest are left out, never the first.
const REQUESTS_TOKENS: u64 = 24_000;

const HEADER: &str = "[summary] The conversation before this point was replaced by a summary. Below: the person's \
messages in it word for word ([request]), the files touched ([files]), then what was done.";
/// The head of a kept message of the person in a summary.
pub(crate) const REQUEST: &str = "[request] ";
const FILES: &str = "[files]";
const WORK: &str = "[summary] What was done:";

mod kept;
mod summariser;
mod transcript;

pub use kept::{fresh_tokens, keep_recent, kept, summary_parts, Kept};
pub use summariser::{compact, Compaction, CompactionFailure, ImagesInCompactionPrefix, Summariser};
pub use transcript::{transcript, transcript_chars};

#[cfg(test)]
mod tests;
