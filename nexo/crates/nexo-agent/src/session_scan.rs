//! What the usage is made of: the saved sessions read once, boiled down per day, and ranked.
//!
//! The reader lives next to the writer (`session.rs`) so the line format stays in one place. Each
//! file is read whole, in one pass, into a [`Digest`] (per local day: the counts, the tool output
//! that stayed in the context, the cost per model); a digest is kept while the file's modification
//! time and size stay the same, so a second scan reads only what changed. [`rank`] then answers for
//! a [`Period`] without touching the disk. Nothing here keeps text of a session: only counts, tool
//! names, the program (and verb) of a command, and folders. Reading takes no lock.
//!
//! Approximate by construction. A `usage` line of an older file has no time, model or owner: it is
//! put in the day of the message before it and under the model in force. A file rewritten by a
//! compaction or a rewind holds one `usage` line with everything spent so far; it counts in the
//! totals (on the day the session began) but is not one call.

mod insights;
mod model;
mod parse;
mod scan;

pub use insights::*;
pub use model::*;
pub use parse::{day_of, digest_reader};
pub use scan::*;

#[cfg(test)]
mod tests;
