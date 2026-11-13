//! The short tasks: each one a cargo mini-project with no dependencies and one trap.

use std::path::Path;

use super::{always, files, manifest, Task};
use crate::check::{read, unchanged, Copy, Outcome, Part};

/// The first six tasks, the default set of the first live runs.
pub fn core() -> Vec<Task> {
    vec![rules_first(), noisy_log(), early_detail(), plan_change(), misleading_error(), cross_session()]
}

/// Tasks added after the first live runs.
pub fn more() -> Vec<Task> {
    vec![three_sessions(), selective_revert(), stale_doc()]
}

/// Writes `text` at `rel` under `wd`, creating folders.
fn put(wd: &Path, rel: &str, text: &str) {
    let p = wd.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("a folder in the working directory");
    }
    std::fs::write(p, text).expect("a file in the working directory");
}

/// A copy of `wd` to check, or the outcome that says why there is none.
fn copy(wd: &Path) -> Result<Copy, Outcome> {
    if !wd.is_dir() {
        return Err(Outcome::fail("the working directory is gone"));
    }
    Copy::of(wd).map_err(|e| Outcome::fail(format!("cannot copy the working directory: {e}")))
}

mod rules_given_first;
use rules_given_first::*;
mod noisy_output;
use noisy_output::*;
mod early_detail_late;
use early_detail_late::*;
mod plan_change_midway;
use plan_change_midway::*;
mod misleading_error_loop;
use misleading_error_loop::*;
mod value_from_earlier_session;
use value_from_earlier_session::*;
mod three_sessions;
use three_sessions::*;

#[cfg(test)]
mod tests;
