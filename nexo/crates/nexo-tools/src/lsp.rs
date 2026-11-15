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
