//! `diagnostics`: what a language server says of a file (errors, warnings), so
//! the agent can check an edit the way an editor would. One server per
//! language and project root, started on the first call and kept for the
//! session: rust-analyzer, pyright (or pylsp), typescript-language-server,
//! gopls, clangd, whichever is installed.

use std::time::Duration;

/// How long the first answer of a server may take (it indexes the project first).
const FIRST: Duration = Duration::from_secs(90);
/// How long `initialize` may take: a live server answers it in a moment (indexing comes after).
const INIT: Duration = Duration::from_secs(20);
/// After diagnostics arrive, how long to wait for more (servers publish in waves).
const SETTLE: Duration = Duration::from_millis(1500);
/// The largest message a server may announce; past it the connection ends, as at its EOF.
const MAX_MESSAGE: usize = 64 << 20;
/// The longest header line a server may send.
const MAX_HEADER: u64 = 8 << 10;
/// How many answers to the server's own requests may wait to be written; past it they are dropped.
const REPLY_QUEUE: usize = 256;

mod check;
mod quick;
mod running;
mod server;
mod table;
#[cfg(test)]
mod tests;

pub use check::{format, Diagnostics};
pub use quick::{quick, QUICK};
pub use running::warm;
pub use table::{available, servers_for};
