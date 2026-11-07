//! `nexo --worktree [name]` and the `nexo worktree` subcommand: the session runs in a git worktree of
//! its own (see `nexo_core::worktree`). The commands: `list`, `remove <name>`, `prune`.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use nexo_core::worktree as wt;

#[derive(Subcommand, Debug)]
pub enum WorktreeCmd {
    /// List the worktrees nexo made in this repository and whether they hold changes.
    List,
    /// Remove a worktree and its branch, changes included.
    Remove { name: String },
    /// Remove the worktrees with no changes, and the leftovers of ones deleted by hand.
    Prune,
}

/// The session's working directory: the worktree when `--worktree` was given. `Ok(None)`: not asked for.
pub fn enter(args: &crate::cli::Args, cwd: &Path) -> anyhow::Result<Option<wt::Worktree>> {
    let Some(name) = &args.worktree else { return Ok(None) };
    // the subcommands (providers, config, ...) have no session to isolate
    if args.cmd.as_ref().is_some_and(|c| !matches!(c, crate::cli::Cmd::Review { .. })) {
        return Ok(None);
    }
    let setup = nexo_core::Config::load(cwd)?.agent.worktree_setup;
    let name = Some(name.as_str()).filter(|n| !n.is_empty());
    let made = wt::create(cwd, name, &setup).map_err(|e| anyhow::anyhow!("--worktree: {e}"))?;
    eprintln!("nexo: working in worktree {} (branch {})", made.path.display(), made.branch);
    Ok(Some(made))
}

/// Ends the session's worktree: removed when nothing changed, kept (with how to merge it) otherwise.
pub fn leave(made: &wt::Worktree) {
    match wt::finish(made) {
        wt::Finished::Removed => eprintln!("nexo: worktree {} had no changes and was removed", made.name),
        wt::Finished::Kept(c) => eprintln!("nexo: {}", wt::kept_message(made, &c)),
    }
}

pub fn run(cwd: &Path, what: &WorktreeCmd) -> anyhow::Result<i32> {
    let root: PathBuf = match wt::repo_root(cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("nexo worktree: {e}");
            return Ok(2);
        }
    };
    match what {
        WorktreeCmd::List => {
            let all = wt::list(&root);
            if all.is_empty() {
                println!("no nexo worktrees in {}", root.display());
            }
            for e in all {
                let state = if e.changes.is_empty() {
                    "clean".to_string()
                } else {
                    format!("{} commit(s), {} changed", e.changes.commits, e.changes.dirty.len())
                };
                println!("{}  {}  {}  {}", e.worktree.name, e.worktree.branch, state, e.worktree.path.display());
            }
            Ok(0)
        }
        WorktreeCmd::Remove { name } => match wt::remove(&root, name) {
            Ok(()) => {
                println!("removed worktree {name} and branch nexo/{name}");
                Ok(0)
            }
            Err(e) => {
                eprintln!("nexo worktree: {e}");
                Ok(1)
            }
        },
        WorktreeCmd::Prune => {
            let gone = wt::prune(&root);
            println!(
                "{}",
                if gone.is_empty() { "nothing to prune".to_string() } else { format!("removed: {}", gone.join(", ")) }
            );
            Ok(0)
        }
    }
}
