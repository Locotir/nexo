//! Day-one compatibility with the files a team already keeps for other coding agents:
//! `CLAUDE.md`, `CLAUDE.local.md` and nested `AGENTS.md` instructions with `@path` imports,
//! commands in `.claude/commands` and `.codex/prompts`, and sub-agents in `.claude/agents`.
//!
//! Everything here takes its folders as arguments, so tests use temporary ones. nexo's own files
//! (`NEXO.md`, `.nexo/commands`, `.nexo/agents`) win over the foreign ones on a clash, and a
//! project's foreign commands and agents are left out until the project is trusted, like nexo's.

#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether Claude Code's files are read at all (`[compat] claude_code`): `CLAUDE.md`, `CLAUDE.local.md`,
/// `.claude/` in the project and `~/.claude` in the home. Process wide, set once when the configuration is loaded.
static CLAUDE_CODE: AtomicBool = AtomicBool::new(true);

/// Turns the reading of Claude Code's files on or off for this process (`[compat] claude_code`).
pub fn set_claude_code(on: bool) {
    CLAUDE_CODE.store(on, Ordering::Relaxed);
}

/// Whether Claude Code's files are read (on unless `[compat] claude_code = false`).
pub fn claude_code() -> bool {
    CLAUDE_CODE.load(Ordering::Relaxed)
}

mod commands;
mod hooks;
mod instructions;
mod paths;
mod rules;

pub use commands::*;
pub use hooks::*;
pub use instructions::*;
pub use paths::*;
pub use rules::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod rule_tests;
